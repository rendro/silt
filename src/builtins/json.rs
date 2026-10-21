//! The `json.*` builtin functions and the typed decoding `toml.*` shares.

use std::sync::Arc;

use super::data::{self, Data, Format};
use super::decode::{self, At, Decoder, Document, I64_MAX_PLUS_ONE, Numbers, Rules};
use super::registry::decoding::Family;
use super::typed::{Type, builtins};
use crate::typeinfo::FieldType;
use crate::typeinfo::bv;
use crate::value::Value;
use crate::vm::{Runtime, Step, Vm, VmError};

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
    Value::variant(bv::ERR, vec![inner])
}

/// Classify a `serde_json::Error` into one of the `JsonError` variants.
/// Syntax/Eof errors get `Syntax(msg, column)`. Data and Io errors
/// collapse to `Other` because serde_json does not expose the
/// expected/actual type pair for a category=Data error in a way we can
/// meaningfully surface; the typed decoder ([`decode`]) reports a
/// value of the wrong type.
pub(crate) fn json_error_to_variant(err: &serde_json::Error) -> Value {
    use serde_json::error::Category;
    match err.classify() {
        Category::Syntax | Category::Eof => {
            // `column()` is 1-based and usable as a byte offset into the
            // line (serde's internal tokenizer uses UTF-8 byte positions,
            // not codepoints). Prefer it over `line()` because fully
            // inlined TOML/JSON blobs are common in practice.
            let offset = err.column() as i64;
            Value::variant(
                bv::JSON_SYNTAX,
                vec![Value::String(err.to_string().into()), Value::Int(offset)],
            )
        }
        _ => Value::variant(bv::JSON_OTHER, vec![Value::String(err.to_string().into())]),
    }
}

/// Build a full `Err(JsonError)` from a `serde_json::Error`.
pub(crate) fn json_result_err(err: &serde_json::Error) -> Value {
    json_err_wrap(json_error_to_variant(err))
}

/// Build `Err(Other(msg))` for ad-hoc failures (unknown type
/// descriptor, internal unexpected results, etc.).
pub(crate) fn json_unknown_err<S: Into<Arc<str>>>(msg: S) -> Value {
    json_err_wrap(Value::variant(
        bv::JSON_OTHER,
        vec![Value::String(msg.into())],
    ))
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

/// `json`, which is made of a `json.Value`, with every number that is a
/// whole number in `Int`'s range held as an `i64`, as
/// [`parse_document`] holds one of a text: `json.Float(1.0)` decodes
/// into an `Int` as the text `1.0` does.
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
    Ok(decode::json_value(&json))
}

/// `data` as the JSON text of `caller`, compact or `pretty`: an object
/// keeps its members in the order of the data (a record's fields as
/// its type declares them). `Bytes` in it, which only a postgres
/// parameter holds, has no form.
pub(crate) fn text(caller: &str, data: &Data, pretty: bool) -> Result<String, VmError> {
    let text = match pretty {
        true => serde_json::to_string_pretty(&Json(data)),
        false => serde_json::to_string(&Json(data)),
    };
    text.map_err(|e| VmError::new(format!("{caller}: {e}")))
}

// ── Reading a document ──────────────────────────────────────────────

/// The document the JSON text `text` is, with serde_json's errors. A
/// number whose text is a whole number in `Int`'s range (`1`, `1.0`,
/// `1e3`, `9223372036854775807.0`) is held as an `i64`, decided on its
/// text: serde_json alone reads every number that is no integer
/// literal of 64 bits as an `f64`, which rounds (`9007199254740993.0`)
/// and hides a fraction or a number out of range
/// (`-9223372036854775809`). Every other number keeps serde_json's
/// reading, so a number held as no `i64` is no `Int`.
pub(crate) fn parse_document(text: &[u8]) -> Result<serde_json::Value, serde_json::Error> {
    use serde::de::DeserializeSeed;
    let mut de = serde_json::Deserializer::from_slice(text);
    let read = Node {
        text,
        at: &std::cell::Cell::new(0),
    }
    .deserialize(&mut de)?;
    de.end()?;
    Ok(read)
}

/// The reader of one value of the document `text`. `at` is how far the
/// numbers of the text are read: serde_json gives the values in the
/// order the text has them, so the next number it gives is the next
/// number token after `at`.
#[derive(Clone, Copy)]
struct Node<'t> {
    text: &'t [u8],
    at: &'t std::cell::Cell<usize>,
}

impl<'t> Node<'t> {
    /// The text of the next number of the document.
    fn next_number(self) -> &'t [u8] {
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

impl<'de> serde::de::DeserializeSeed<'de> for Node<'_> {
    type Value = serde_json::Value;

    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> serde::de::Visitor<'de> for Node<'_> {
    type Value = serde_json::Value;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E>(self, b: bool) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Bool(b))
    }

    fn visit_i64<E>(self, n: i64) -> Result<Self::Value, E> {
        self.next_number();
        Ok(serde_json::Value::Number(n.into()))
    }

    fn visit_u64<E>(self, n: u64) -> Result<Self::Value, E> {
        self.next_number();
        Ok(serde_json::Value::Number(n.into()))
    }

    fn visit_f64<E>(self, f: f64) -> Result<Self::Value, E> {
        Ok(match exact_int(self.next_number()) {
            Some(n) => serde_json::Value::Number(n.into()),
            None => serde_json::Number::from_f64(f).map_or(serde_json::Value::Null, Into::into),
        })
    }

    fn visit_str<E>(self, s: &str) -> Result<Self::Value, E> {
        Ok(serde_json::Value::String(s.to_owned()))
    }

    fn visit_string<E>(self, s: std::string::String) -> Result<Self::Value, E> {
        Ok(serde_json::Value::String(s))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Null)
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element_seed(self)? {
            items.push(item);
        }
        Ok(serde_json::Value::Array(items))
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        // (Of a key written twice the last value counts.)
        let mut object = serde_json::Map::new();
        while let Some(key) = map.next_key::<std::string::String>()? {
            object.insert(key, map.next_value_seed(self)?);
        }
        Ok(serde_json::Value::Object(object))
    }
}

/// The `Int` the JSON number `text` is, decided on its digits: `None`
/// when it has a fraction or is out of `Int`'s range.
fn exact_int(text: &[u8]) -> Option<i64> {
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
    // The digits, and the power of ten they are multiplied by.
    let digits: Vec<u8> = whole.iter().chain(fraction).map(|b| b - b'0').collect();
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
    for &b in exponent {
        power = (power * 10 + i64::from(b - b'0')).min(1 << 40);
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
/// whole number in range, as [`parse_document`] reads it), a date and a
/// time are strings, and a `json.Value` is decoded.
pub(crate) const RULES: Rules = Rules {
    caller: "json.parse",
    table: "object",
    variant: "String or object",
    dates: ["date string", "time string", "datetime string"],
    numbers: Numbers::Values,
    family: Family::Json,
    errors: [
        bv::JSON_TYPE_MISMATCH,
        bv::JSON_MISSING_FIELD,
        bv::JSON_OTHER,
    ],
};

impl Document for serde_json::Value {
    type Table = serde_json::Map<std::string::String, serde_json::Value>;
    const RULES: &'static Rules = &RULES;

    fn node(&self) -> decode::Node<'_, Self> {
        use decode::Node;
        match self {
            serde_json::Value::Null => Node::Null,
            serde_json::Value::Bool(b) => Node::Bool(*b),
            serde_json::Value::Number(n) => match (n.as_i64(), n.as_f64()) {
                (Some(i), _) => Node::Int(i),
                (None, Some(f)) => Node::Float(f),
                // (A number serde_json holds is an `i64`, a `u64` or an
                // `f64`.)
                (None, None) => Node::Float(f64::NAN),
            },
            serde_json::Value::String(s) => Node::String(s),
            serde_json::Value::Array(items) => Node::Array(items),
            serde_json::Value::Object(members) => Node::Table(members),
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            serde_json::Value::Null => "null",
            serde_json::Value::Bool(_) => "boolean",
            serde_json::Value::Number(_) => "number",
            serde_json::Value::String(_) => "string",
            serde_json::Value::Array(_) => "array",
            serde_json::Value::Object(_) => "object",
        }
    }

    fn get<'d>(table: &'d Self::Table, key: &str) -> Option<&'d Self> {
        table.get(key)
    }

    fn entries(table: &Self::Table) -> impl ExactSizeIterator<Item = (&str, &Self)> {
        table.iter().map(|(key, value)| (key.as_str(), value))
    }
}

/// The value of the field type `expected` that the document `json` is,
/// for a decoder of a format that holds JSON documents (postgres's
/// `json` and `jsonb` columns): `Err` of what `JsonError`'s `message`
/// says of why there is none.
#[cfg(feature = "postgres")]
pub(crate) fn decode_field(
    rt: &Runtime,
    json: &serde_json::Value,
    expected: &FieldType,
) -> Result<Value, std::string::String> {
    decode::value(&decode::Reader::new(rt), json, expected, &At::Top).map_err(|e| {
        let why = e.variant(&RULES);
        let text = match &why {
            Value::Variant(why) => error_text(why.name(), why.fields()),
            _ => None,
        };
        text.unwrap_or_else(|| why.to_string())
    })
}

/// What `caller`, a json decoder, gives for its type argument `ty`:
/// `Ok` of the value of the document `read_doc` gives (as `read` reads
/// it: one value, a list, a map), or `Err` of the `JsonError` that says
/// why there is none, the program's `json.Decode` impls honoured. As
/// every decoder does, it takes the type and refuses one no decoder
/// fills before the document is read; `read_doc` gives the document, or
/// the call's result where there is none. (An impl is the program's
/// code: a step.)
fn decoded(
    vm: &mut Vm,
    caller: &Decoder,
    ty: Type,
    read: crate::vm::decode::Read,
    read_doc: impl FnOnce() -> Result<serde_json::Value, Value>,
) -> Result<Step, VmError> {
    let expected = decode::target(caller, ty)?.element();
    vm.decode_json(expected, read, read_doc)
}

/// `json.decode(value, ty)`, and `T.decode(value)` of a type with no
/// written `json.Decode` impl.
pub(crate) fn decode_value(vm: &mut Vm, value: &Value, ty: Type) -> Result<Step, VmError> {
    // A `json.Value` holds only what JSON has a form for, so the one way
    // to fail here is a value nested past the limit: an `Err`, as
    // `json.parse` gives one for a document nested too deep.
    let document = match data::data("json.decode", Format::Json, value) {
        Ok(data) => {
            let mut json = serde_json::to_value(Json(&data))
                .map_err(|e| VmError::new(format!("json.decode: {e}")))?;
            whole_numbers(&mut json);
            Ok(json)
        }
        Err(e) => Err(json_unknown_err(e.message)),
    };
    decoded(vm, &DECODE, ty, one, || document)
}

/// A document read as one value.
fn one(
    r: &decode::Reader,
    doc: &serde_json::Value,
    expected: &FieldType,
) -> Result<Value, decode::Error> {
    decode::value(r, doc, expected, &At::Top)
}

/// A document read as a list of values.
fn list_of(
    r: &decode::Reader,
    doc: &serde_json::Value,
    element: &FieldType,
) -> Result<Value, decode::Error> {
    decode::list(r, element, doc, &At::Top)
}

/// A document read as a map of values.
fn map_of(
    r: &decode::Reader,
    doc: &serde_json::Value,
    element: &FieldType,
) -> Result<Value, decode::Error> {
    decode::map(r, element, doc, &At::Top)
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
static PARSE_LIST: Decoder = Decoder::new("json.parse_list");
static PARSE_MAP: Decoder = Decoder::new("json.parse_map");

builtins! {
    fn decode(vm, value: &Value, ty: Type) -> Result<Step, VmError> {
        decode_value(vm, value, ty)
    }

    fn parse(vm, s: &str, ty: Type) -> Result<Step, VmError> {
        decoded(vm, &PARSE, ty, one, || {
            parse_document(s.as_bytes()).map_err(|e| json_result_err(&e))
        })
    }

    fn parse_list(vm, s: &str, ty: Type) -> Result<Step, VmError> {
        decoded(vm, &PARSE_LIST, ty, list_of, || {
            parse_document(s.as_bytes()).map_err(|e| json_result_err(&e))
        })
    }

    fn parse_map(vm, s: &str, ty: Type) -> Result<Step, VmError> {
        decoded(vm, &PARSE_MAP, ty, map_of, || {
            parse_document(s.as_bytes()).map_err(|e| json_result_err(&e))
        })
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
            let json: serde_json::Value = serde_json::from_str(text).expect("a document");
            // As json.parse decodes it: the type is refused before the
            // document is read.
            let target = decode::Target::Record(&ty, &fields);
            let decoded = decode::refused(&decode::Reader::new(&vm.runtime), &RULES, &target)
                .unwrap_or_else(|| {
                    decode::result(
                        &RULES,
                        decode::document(&decode::Reader::new(&vm.runtime), &target, &json),
                    )
                });
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
                        Value::variant(bv::OK, vec![Value::list(vec![inner])])
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
