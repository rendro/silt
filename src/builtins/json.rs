//! The `json.*` builtin functions and the typed decoding `toml.*` shares.

use std::sync::Arc;

use super::data::{self, Data, Format};
use super::decode::{self, Decoder, I64_MAX_PLUS_ONE, Numbers, Rules};
use super::registry::decoding::Family;
use super::typed::{Type, builtins};
use crate::typeinfo::FieldType;
use crate::typeinfo::bv;
use crate::value::Value;
use crate::vm::{Source, Step, Vm, VmError};

// ── JsonError helpers ────────────────────────────────────────────────
//
// Phase 1 of the stdlib error redesign: every fallible json.* call now
// surfaces a typed `JsonError` variant wrapped in `Err(...)` instead of
// a bare `Err(String)`. These helpers mirror `io_error_to_variant` +
// `io_result_err` in `src/builtins/io.rs` so the two modules stay in
// step.
//
// `JsonError` variants:
//   Syntax(message, byte_offset)
//   TypeMismatch(expected, actual)
//   MissingField(name)
//   Other(message)

fn json_err_wrap(inner: Value) -> Value {
    Value::variant(bv::ERR, [inner])
}

/// Classify a `serde_json::Error` of reading `text` into one of the
/// `JsonError` variants. Syntax/Eof errors get `Syntax(msg, offset)`.
/// Data and Io errors collapse to `Other` because serde_json does not
/// expose the expected/actual type pair for a category=Data error in a
/// way we can meaningfully surface; the typed decoder ([`decode`])
/// reports a value of the wrong type.
pub(crate) fn json_error_to_variant(err: &serde_json::Error, text: &[u8]) -> Value {
    use serde_json::error::Category;
    match err.classify() {
        Category::Syntax | Category::Eof => {
            let offset = syntax_offset(err, text) as i64;
            // serde's text ends with where it stopped, as a line and a
            // column; the offset says where, once.
            let full = err.to_string();
            let place = format!(" at line {} column {}", err.line(), err.column());
            let message = full.strip_suffix(&place).unwrap_or(&full);
            Value::variant(
                bv::JSON_SYNTAX,
                [Value::String(message.into()), Value::Int(offset)],
            )
        }
        _ => Value::variant(bv::JSON_OTHER, [Value::String(err.to_string().into())]),
    }
}

/// Build a full `Err(JsonError)` from a `serde_json::Error` of reading
/// `text`.
pub(crate) fn json_result_err(err: &serde_json::Error, text: &[u8]) -> Value {
    json_err_wrap(json_error_to_variant(err, text))
}

/// Where in `text` the syntax error `err` is, as a 0-based byte offset
/// (as `toml.Syntax` has it): the byte serde stopped at, which it gives
/// as a line and a 1-based column of bytes in that line (column 0 is
/// the newline that ends the line before: a raw newline in a string);
/// the end of the text for one that ended too soon.
fn syntax_offset(err: &serde_json::Error, text: &[u8]) -> usize {
    if err.classify() == serde_json::error::Category::Eof {
        return text.len();
    }
    let line_start = match err.line().checked_sub(2) {
        // The line after the (line - 1)th newline.
        Some(newlines) => text
            .iter()
            .enumerate()
            .filter(|(_, byte)| **byte == b'\n')
            .nth(newlines)
            .map_or(text.len(), |(at, _)| at + 1),
        None => 0,
    };
    let at = match err.column() {
        0 => line_start.saturating_sub(1),
        column => line_start + column - 1,
    };
    at.min(text.len())
}

/// Build `Err(Other(msg))` for ad-hoc failures (unknown type
/// descriptor, internal unexpected results, etc.).
pub(crate) fn json_unknown_err<S: Into<Arc<str>>>(msg: S) -> Value {
    json_err_wrap(Value::variant(bv::JSON_OTHER, [Value::String(msg.into())]))
}

/// What `JsonError`'s `message` says of the variant `tag` with `fields`:
/// `None` if they are no variant of it.
pub(crate) fn error_text(tag: &str, fields: &[Value]) -> Option<String> {
    Some(match (tag, fields) {
        ("Syntax", [Value::String(m), Value::Int(offset)]) => {
            format!("json syntax error at byte {offset}: {m}")
        }
        ("TypeMismatch", [Value::String(exp), Value::String(act)]) => {
            format!("json type mismatch: expected {exp}, got {act}")
        }
        ("MissingField", [Value::String(n)]) => {
            format!("json missing field: {n}")
        }
        ("Other", [Value::String(m)]) => m.to_string(),
        _ => return None,
    })
}

/// `json`, which is made of data, with every number that is a whole
/// number in `Int`'s range held as an `i64`, as `json.parse` reads one
/// of a text.
fn whole_numbers(json: &mut serde_json::Value) {
    match json {
        serde_json::Value::Number(n) => {
            if let Some(f) = n.as_f64()
                && n.as_i64().is_none()
                && f.fract() == 0.0
                && (-I64_MAX_PLUS_ONE..I64_MAX_PLUS_ONE).contains(&f)
            {
                *n = (f as i64).into();
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(whole_numbers),
        serde_json::Value::Object(fields) => fields.values_mut().for_each(whole_numbers),
        _ => {}
    }
}

/// `data` as JSON: a date and a time as the text silt shows of them
/// (`2026-10-19`, `12:30:00.250000000`, `2026-10-19T12:30:00`), which
/// their decoders read back; `None` and `()` as `null`.
struct Json<'d>(&'d Data);

impl serde::Serialize for Json<'_> {
    fn serialize<S: serde::Serializer>(&self, to: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{Error, SerializeMap, SerializeSeq};
        match self.0 {
            Data::Null | Data::Unit => to.serialize_unit(),
            Data::Int(n) => to.serialize_i64(*n),
            Data::Float(f) => to.serialize_f64(*f),
            Data::Bool(b) => to.serialize_bool(*b),
            Data::String(s) => to.serialize_str(s),
            Data::Bytes(_) => Err(S::Error::custom(data::unsupported("Bytes"))),
            Data::Date(date) => to.collect_str(date),
            Data::Time(time) => to.collect_str(time),
            Data::DateTime(at) => {
                let (date, time) = &**at;
                to.collect_str(&format_args!("{date}T{time}"))
            }
            Data::Array(items) | Data::Tuple(items) => {
                let mut seq = to.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(&Json(item))?;
                }
                seq.end()
            }
            Data::Table(members) => {
                let mut map = to.serialize_map(Some(members.len()))?;
                for (key, value) in members {
                    map.serialize_entry(key, &Json(value))?;
                }
                map.end()
            }
            Data::Json(content) => Json(content).serialize(to),
            Data::Hole(_) => Err(S::Error::custom(data::UNFILLED)),
        }
    }
}

/// `data` as a `json.Value`: what `json.parse` reads of the text
/// `json.stringify` writes of it (a whole number in `Int`'s range is a
/// `json.Int`, a record an object whose keys are sorted).
pub(crate) fn json_value_of(caller: &str, data: &Data) -> Result<Value, VmError> {
    let mut json =
        serde_json::to_value(Json(data)).map_err(|e| VmError::new(format!("{caller}: {e}")))?;
    whole_numbers(&mut json);
    Ok(json_value(&json))
}

/// The `json.Value` that holds the JSON document `json`: a number an
/// `Int` where it is held as an `i64`, else a `Float`.
fn json_value(json: &serde_json::Value) -> Value {
    use serde_json::Value as J;
    match json {
        J::Null => Value::variant(bv::JSON_NULL, []),
        J::Bool(b) => Value::variant(bv::JSON_BOOL, [Value::Bool(*b)]),
        J::Number(n) => match (n.as_i64(), n.as_f64().and_then(crate::value::Float::new)) {
            (Some(n), _) => Value::variant(bv::JSON_INT, [Value::Int(n)]),
            (None, Some(f)) => Value::variant(bv::JSON_FLOAT, [Value::Float(f)]),
            // (JSON has no number that is no finite float.)
            (None, None) => Value::variant(bv::JSON_NULL, []),
        },
        J::String(s) => Value::variant(bv::JSON_STRING, [Value::String(s.as_str().into())]),
        J::Array(items) => Value::variant(
            bv::JSON_ARRAY,
            [Value::list(items.iter().map(json_value).collect())],
        ),
        J::Object(members) => Value::variant(
            bv::JSON_OBJECT,
            [Value::map(members.iter().map(|(key, member)| {
                (Value::String(key.as_str().into()), json_value(member))
            }))],
        ),
    }
}

/// `data` as the JSON text of `caller`, compact or `pretty`: an object
/// keeps its members in the order of the data (a record's fields as
/// its type declares them). `Bytes` in it, which only a postgres
/// parameter holds, has no form.
/// The text is no longer than a string may be: it is written into a
/// [`Bounded`](super::common::Bounded), which stops it there (the data
/// can hold one long string many times).
pub(crate) fn text(caller: &str, data: &Data, pretty: bool) -> Result<String, VmError> {
    let mut out = super::common::Bounded::default();
    let written = match pretty {
        true => serde_json::to_writer_pretty(&mut out, &Json(data)),
        false => serde_json::to_writer(&mut out, &Json(data)),
    };
    out.text(caller, written.err().map(|e| format!("{caller}: {e}")))
}

// ── Reading a document ──────────────────────────────────────────────

/// The numbers of the JSON text `text`, read again from their digits
/// (`decode::seed`): serde_json reads every number that is no integer
/// literal of 64 bits as an `f64`, which rounds (`9007199254740993.0`)
/// and hides a fraction or a number out of range
/// (`-9223372036854775809`); a number whose text is a whole number in
/// `Int`'s range (`1`, `1.0`, `1e3`, `9223372036854775807.0`) is an
/// `Int`. `at` is how far the numbers of the text are read: serde_json
/// gives the values in the order the text has them, so the next number
/// it gives is the next number token after `at`.
#[derive(Clone, Copy)]
pub(crate) struct Numerals<'t> {
    text: &'t [u8],
    at: &'t std::cell::Cell<usize>,
}

impl<'t> Numerals<'t> {
    /// The numbers of the text `text`, read up to `at`.
    pub(crate) fn new(text: &'t [u8], at: &'t std::cell::Cell<usize>) -> Numerals<'t> {
        Numerals { text, at }
    }

    /// The text of the next number of the document.
    pub(crate) fn next_number(self) -> &'t [u8] {
        let text = self.text;
        let mut i = self.at.get();
        let mut in_string = false;
        while let Some(&b) = text.get(i) {
            match (in_string, b) {
                (true, b'\\') => i += 1,
                (_, b'"') => in_string = !in_string,
                (false, b'-' | b'0'..=b'9') => break,
                _ => {}
            }
            i += 1;
        }
        let start = i;
        while text
            .get(i)
            .is_some_and(|b| matches!(b, b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E'))
        {
            i += 1;
        }
        self.at.set(i);
        &text[start..i]
    }
}

/// The `Int` the JSON number `text` is, decided on its digits: `None`
/// when it has a fraction or is out of `Int`'s range.
pub(crate) fn exact_int(text: &[u8]) -> Option<i64> {
    let (negative, text) = match text.split_first() {
        Some((b'-', rest)) => (true, rest),
        _ => (false, text),
    };
    let (mantissa, exponent) = match text.iter().position(|b| matches!(b, b'e' | b'E')) {
        Some(e) => (&text[..e], &text[e + 1..]),
        None => (text, &b""[..]),
    };
    let (whole, fraction) = match mantissa.iter().position(|&b| b == b'.') {
        Some(dot) => (&mantissa[..dot], &mantissa[dot + 1..]),
        None => (mantissa, &b""[..]),
    };
    // (A text the parser broke off in, as `1e3-` of `[1e3-]`, is no
    // number: the parser's error follows.)
    let digit = |b: &u8| b.checked_sub(b'0').filter(|d| *d < 10);
    // The digits, and the power of ten they are multiplied by.
    let digits: Vec<u8> = whole
        .iter()
        .chain(fraction)
        .map(digit)
        .collect::<Option<_>>()?;
    let first = digits.iter().position(|&d| d != 0);
    let last = digits.iter().rposition(|&d| d != 0);
    let (Some(first), Some(last)) = (first, last) else {
        return Some(0);
    };
    // An exponent past what any digits make up for is out of range one
    // way or a fraction the other: kept within `i64` as such.
    let mut power: i64 = 0;
    let (sign, exponent) = match exponent.split_first() {
        Some((b'-', rest)) => (-1, rest),
        Some((b'+', rest)) => (1, rest),
        _ => (1, exponent),
    };
    for b in exponent {
        power = (power * 10 + i64::from(digit(b)?)).min(1 << 40);
    }
    let power = sign * power - fraction.len() as i64 + (digits.len() - 1 - last) as i64;
    let significant = &digits[first..=last];
    if power < 0 || significant.len() as i64 + power > 19 {
        return None;
    }
    let mut n: i128 = 0;
    for &d in significant {
        n = n * 10 + i128::from(d);
    }
    n *= 10i128.pow(power as u32);
    i64::try_from(if negative { -n } else { n }).ok()
}

// ── Decoding a document ─────────────────────────────────────────────

/// How a JSON document decodes: a number is an `Int` by its value (a
/// whole number in range, read from its digits), a date and a time are
/// strings, and a `json.Value` is decoded.
pub(crate) const RULES: Rules = Rules {
    caller: "json.parse",
    table: "object",
    variant: "String or object",
    numbers: Numbers::Values,
    family: Family::Json,
    errors: [
        bv::JSON_TYPE_MISMATCH,
        bv::JSON_MISSING_FIELD,
        bv::JSON_OTHER,
    ],
    from_impl: decode::at_part,
};

/// The value of the field type `expected` that the JSON text `text` is,
/// for a decoder of a format that holds JSON documents (postgres's
/// `json` and `jsonb` columns), read by `r` (a part of a type with a
/// written `json.Decode` impl is its hole): `Err` of what `JsonError`'s
/// `message` says of why there is none, or `None` where the text is no
/// document.
#[cfg(feature = "postgres")]
pub(crate) fn decode_field(
    r: &decode::Reader,
    text: &[u8],
    expected: &FieldType,
) -> Result<Result<Value, std::string::String>, serde_json::Error> {
    let decoded = decode::seed::json_cell(r, expected, text)?;
    Ok(decoded.map_err(|e| message(&e.variant(&RULES))))
}

/// What `JsonError`'s `message` says of `error`.
#[cfg(feature = "postgres")]
pub(crate) fn message(error: &Value) -> std::string::String {
    let text = match error {
        Value::Variant(why) => error_text(why.name(), why.fields()),
        _ => None,
    };
    text.unwrap_or_else(|| error.to_string())
}

/// `json.decode(value, ty)`, and `T.decode(value)` of a type with no
/// written `json.Decode` impl.
pub(crate) fn decode_value(vm: &mut Vm, value: &Value, ty: Type) -> Result<Step, VmError> {
    // A `json.Value` holds only what JSON has a form for, so the one way
    // to fail here is a value nested past the limit: an `Err`, as
    // `json.parse` gives one for a document nested too deep.
    let expected = decode::target(&DECODE, ty)?.element();
    let read = |r: &decode::Reader, expected: &FieldType, parts: &decode::seed::Parts| {
        decode::seed::json_value(r, expected, value, parts).map_err(json_unknown_err)
    };
    let source = || -> Box<dyn Source> { Box::new(Read(value.clone())) };
    vm.decode_read("json.decode", &RULES, expected, read, source)
}

/// A `json.Value` decoded, read again.
struct Read(Value);

impl Source for Read {
    fn read(
        &self,
        r: &decode::Reader,
        expected: &FieldType,
        parts: &decode::seed::Parts,
    ) -> Result<decode::seed::Outcome, Value> {
        decode::seed::json_value(r, expected, &self.0, parts).map_err(json_unknown_err)
    }
}

/// A JSON text read again.
struct Text(Box<[u8]>);

impl Source for Text {
    fn read(
        &self,
        r: &decode::Reader,
        expected: &FieldType,
        parts: &decode::seed::Parts,
    ) -> Result<decode::seed::Outcome, Value> {
        decode::seed::json_text(r, expected, &self.0, parts)
            .map_err(|e| json_result_err(&e, &self.0))
    }
}

// ── The functions ───────────────────────────────────────────────────

/// The JSON text `caller` writes of `value`, compact or `pretty`.
fn written_text(
    vm: &mut Vm,
    caller: &'static str,
    value: &Value,
    pretty: bool,
) -> Result<Step, VmError> {
    vm.encode(caller, Format::Json, value, move |_, data| {
        let text = text(caller, &data?, pretty)?;
        Ok(Step::Done(Value::String(text.into())))
    })
}

static DECODE: Decoder = Decoder::new("json.decode");
static PARSE: Decoder = Decoder::new("json.parse");

builtins! {
    fn decode(vm, value: &Value, ty: Type) -> Result<Step, VmError> {
        decode_value(vm, value, ty)
    }

    fn parse(vm, s: &str, ty: Type) -> Result<Step, VmError> {
        let text = s.as_bytes();
        let expected = decode::target(&PARSE, ty)?.element();
        let read = |r: &decode::Reader, expected: &FieldType, parts: &decode::seed::Parts| {
            decode::seed::json_text(r, expected, text, parts).map_err(|e| json_result_err(&e, text))
        };
        let source = || -> Box<dyn Source> { Box::new(Text(text.into())) };
        vm.decode_read("json.decode", &RULES, expected, read, source)
    }

    // (A part of a type with a written `json.Encode` impl is written
    // as the impl says, which is the program's code: a step.)
    fn encode(vm, value: &Value) -> Result<Step, VmError> {
        vm.encode_json("json.encode", value)
    }

    fn stringify(vm, value: &Value) -> Result<Step, VmError> {
        written_text(vm, "json.stringify", value, false)
    }

    fn pretty(vm, value: &Value) -> Result<Step, VmError> {
        written_text(vm, "json.pretty", value, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtins::data::MAX_ENCODE_DEPTH;
    use crate::builtins::decode::shown;
    use crate::typeinfo::{FieldType, TypeInfo};

    /// A field whose type holds `Bytes` has no decoder, whatever the
    /// document holds there: an empty list, a `null` for an `Option`,
    /// a list of numbers, or nothing (an `Option` whose key is left
    /// out).
    #[test]
    fn a_field_that_holds_bytes_is_never_decoded() {
        use crate::defs::{DefId, TypeId};
        let vm = crate::vm::Vm::new(crate::vm::HostIo::process());
        for (k, (field, text)) in [
            (FieldType::List(Box::new(FieldType::Bytes)), r#"{"b": []}"#),
            (FieldType::List(Box::new(FieldType::Bytes)), r#"{"b": [1]}"#),
            (
                FieldType::Option(Box::new(FieldType::Bytes)),
                r#"{"b": null}"#,
            ),
            (FieldType::Option(Box::new(FieldType::Bytes)), r#"{}"#),
            (
                FieldType::Tuple(vec![FieldType::Int, FieldType::Bytes]),
                r#"{"b": [1, 2]}"#,
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let shown = shown(&vm.runtime, &field);
            let fields = vec![("b".to_string(), field)];
            // (An id of its own: a runtime decides a type's decoder once.)
            let ty = TypeInfo::new_record(TypeId(DefId(9000 + k as u32)), "B", fields.clone());
            // As json.parse decodes it: the type is refused before the
            // document (`text`) is read.
            let target = decode::Target::Record(&ty);
            let decoded = decode::refused(&decode::Reader::new(&vm.runtime), &RULES, &target)
                .unwrap_or_else(|| panic!("{text} is read"));
            assert_eq!(
                decoded.to_string(),
                format!(
                    "Err(cannot decode field `b` of `B`: a value of type {shown} cannot be decoded)"
                ),
                "{text}"
            );
        }
    }

    /// The deepest value the encoder writes, of the shape that makes
    /// the deepest document (a list in a variant: three levels of the
    /// document for two of the value), is written on a stack of 16 MiB,
    /// the least a thread that runs silt code has
    /// (`scheduler::WORKER_STACK_BYTES` where pointers have 32 bits),
    /// in a debug build too, where it takes some 5 MiB; and a value
    /// one level deeper is refused.
    #[test]
    fn the_deepest_value_is_written_on_the_smallest_stack() {
        let (text, pretty, refused) = std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                let nested = |levels: usize| {
                    (0..levels / 2).fold(Value::Int(1), |inner, _| {
                        Value::variant(bv::OK, [Value::list(vec![inner])])
                    })
                };
                let deepest = nested(MAX_ENCODE_DEPTH);
                let json = data::data("json.stringify", Format::Json, &deepest).expect("data");
                let compact = text("json.stringify", &json, false).expect("written");
                let pretty = text("json.pretty", &json, true).expect("written");
                drop(json);
                let too_deep = nested(MAX_ENCODE_DEPTH + 2);
                let refused = data::data("json.pretty", Format::Json, &too_deep).unwrap_err();
                (compact, pretty, refused.message)
            })
            .expect("a thread")
            .join()
            .expect("the thread ends");
        assert!(text.starts_with(r#"{"fields":[[{"fields":[[{"#));
        assert!(text.contains(r#"{"fields":[[1]],"variant":"Ok"}"#));
        assert_eq!(text.len(), MAX_ENCODE_DEPTH / 2 * 30 + 1);
        assert!(pretty.len() > text.len());
        assert_eq!(
            refused,
            format!("json.pretty: the value is nested more than {MAX_ENCODE_DEPTH} levels deep")
        );
    }
}
