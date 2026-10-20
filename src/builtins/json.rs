//! The `json.*` builtin functions and the typed decoding `toml.*` shares.

use std::sync::Arc;

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};

use super::time::{
    LEAP_SECOND, is_leap_second, leap_datetime_message, make_date, make_datetime, make_time,
};
use super::typed::{Type, builtins};
use crate::defs::TypeId;
use crate::typeinfo::{FieldType, Shape, TypeInfo, bv, ty};
use crate::value::{Float, Value};
use crate::vm::{Runtime, VmError};

// ── Field types for JSON / TOML parsing ──────────────────────────────

/// The message of the error a decoder returns for a field whose declared
/// type has no decoder. `field` is the field of the record type `record`
/// whose type is, or contains, `declared`. Shared by the JSON and TOML
/// decoders.
pub(crate) fn unsupported_field_type_message(
    record: &str,
    field: &str,
    declared: &str,
) -> std::string::String {
    format!(
        "cannot decode field `{field}` of `{record}`: a value of type {declared} cannot be decoded"
    )
}

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
/// meaningfully surface; our hand-written record decoder covers the
/// type-mismatch path with explicit `json_type_mismatch_err` calls.
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

/// Where in a document a decoder is: the path that the error of a value
/// there ends with (`inner.a`, `xs[1]`, `[2]`, `m["a b"]`). A key is
/// written bare when it is made of letters, digits, `_` and `-`, else as
/// a JSON string in brackets; the element of an array by its index.
/// Shared by the JSON and TOML decoders.
#[derive(Clone, Copy)]
pub(crate) enum At<'a> {
    /// The document itself.
    Top,
    /// The value of a key (a field of a record, a key of a map) of an
    /// object.
    Key(&'a At<'a>, &'a str),
    /// An element of an array.
    Index(&'a At<'a>, usize),
}

impl<'a> At<'a> {
    /// The value of `key` here.
    pub(crate) fn key(&'a self, key: &'a str) -> At<'a> {
        At::Key(self, key)
    }

    /// The element `index` here.
    pub(crate) fn index(&'a self, index: usize) -> At<'a> {
        At::Index(self, index)
    }

    /// What a message about the value here ends with: ` at <path>`, or
    /// nothing for the document itself.
    pub(crate) fn suffix(&self) -> std::string::String {
        match self {
            At::Top => std::string::String::new(),
            _ => format!(" at {self}"),
        }
    }
}

impl std::fmt::Display for At<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            At::Top => Ok(()),
            At::Key(up, key) => {
                let bare = !key.is_empty()
                    && key
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
                match (bare, up) {
                    (true, At::Top) => f.write_str(key),
                    (true, _) => write!(f, "{up}.{key}"),
                    (false, _) => write!(f, "{up}[{}]", serde_json::Value::from(*key)),
                }
            }
            At::Index(up, index) => write!(f, "{up}[{index}]"),
        }
    }
}

/// Build `Err(TypeMismatch(expected, actual))` for the value `at` a
/// path: `actual` ends with the path. Used by the hand-written record
/// decoder to report a value of the wrong shape.
pub(crate) fn json_type_mismatch_err(expected: &str, actual: &str, at: &At) -> Value {
    json_err_wrap(Value::variant(
        bv::JSON_TYPE_MISMATCH,
        vec![
            Value::String(expected.into()),
            Value::String(format!("{actual}{}", at.suffix()).into()),
        ],
    ))
}

/// Build `Err(MissingField(path))`: `path` is the missing field's own.
pub(crate) fn json_missing_field_err(path: &str) -> Value {
    json_err_wrap(Value::variant(
        bv::JSON_MISSING_FIELD,
        vec![Value::String(path.into())],
    ))
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

/// `i64::MAX` is not exactly an `f64` (it rounds up to 2^63): the
/// bound of `Int`'s range that an `f64` can hold, exclusive.
const I64_MAX_PLUS_ONE: f64 = 9223372036854775808.0; // exact

/// The `Int` that the number `n` of a document is: as
/// [`parse_document`] reads a document, and as [`whole_numbers`] makes
/// one of a `json.Value`, the number is held as an `i64` when it is
/// one. Else what it is instead, for the type mismatch.
fn int_of(n: &serde_json::Number) -> Result<i64, &'static str> {
    match (n.as_i64(), n.as_f64()) {
        (Some(i), _) => Ok(i),
        (None, Some(f)) if f.abs() < I64_MAX_PLUS_ONE => Err("a number with a fraction"),
        (None, _) => Err("a number out of Int's range"),
    }
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

fn json_type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// The most levels a value may be nested to be written as JSON or as
/// TOML (`json.stringify`, `json.pretty`, `toml.stringify`,
/// `toml.pretty`): the element of a list or of a tuple, the field of a
/// record or of a variant and the value of a map are each one level
/// further in than what holds them (the content of a `Some` is not: it
/// is written in the `Some`'s place). The encoders go down a value on
/// the native stack, and one nested without bound would overflow it:
/// a value at the bound takes some 5 MiB of it in a debug build, a
/// third of the least a thread that runs silt code has
/// (`scheduler::WORKER_STACK_BYTES`). (A parser stops far sooner:
/// `json.parse` at 127 levels of a document, `toml.parse` at 79.)
pub(crate) const MAX_ENCODE_DEPTH: usize = 1000;

/// The error of `caller` for a value nested deeper than
/// [`MAX_ENCODE_DEPTH`]: `level` is how far in the encoder is.
pub(crate) fn ensure_encodable_depth(caller: &str, level: usize) -> Result<(), VmError> {
    match level > MAX_ENCODE_DEPTH {
        true => Err(VmError::new(format!(
            "{caller}: the value is nested more than {MAX_ENCODE_DEPTH} levels deep"
        ))),
        false => Ok(()),
    }
}

/// The error of `caller` for `v`, a value the format it writes has no
/// form for: it names the kind of the value. (To a program every
/// function is an `Fn`.)
pub(crate) fn unsupported_kind(caller: &str, v: &Value) -> VmError {
    let kind = match v.is_fn() {
        true => "Fn",
        false => v.kind(),
    };
    VmError::new(format!("{caller}: unsupported value kind {kind}"))
}

/// A document as the writers write it (`json.stringify`, `toml.stringify`
/// and their `pretty` forms): an object, or a table, keeps its members
/// in the order they are put in, where `serde_json::Map` and
/// `toml::map::Map` would sort them. `S` is a value of the format that
/// has no members.
#[derive(Debug)]
pub(crate) enum Written<S> {
    Scalar(S),
    Array(Vec<Written<S>>),
    Object(Vec<(std::string::String, Written<S>)>),
}

impl<S> Written<S> {
    /// The object of the members of a map, `entries`: sorted by key,
    /// and of a key written twice the last value counts.
    pub(crate) fn sorted(entries: Vec<(std::string::String, Written<S>)>) -> Written<S> {
        let members: std::collections::BTreeMap<_, _> = entries.into_iter().collect();
        Written::Object(members.into_iter().collect())
    }
}

impl<S: serde::Serialize> serde::Serialize for Written<S> {
    fn serialize<Z: serde::Serializer>(&self, to: Z) -> Result<Z::Ok, Z::Error> {
        use serde::ser::{SerializeMap, SerializeSeq};
        match self {
            Written::Scalar(v) => v.serialize(to),
            Written::Array(items) => {
                let mut seq = to.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(item)?;
                }
                seq.end()
            }
            Written::Object(members) => {
                let mut map = to.serialize_map(Some(members.len()))?;
                for (key, value) in members {
                    map.serialize_entry(key, value)?;
                }
                map.end()
            }
        }
    }
}

impl Written<serde_json::Value> {
    /// The document as serde_json holds it, for a decoder to read.
    fn into_value(self) -> serde_json::Value {
        match self {
            Written::Scalar(v) => v,
            Written::Array(items) => {
                serde_json::Value::Array(items.into_iter().map(Written::into_value).collect())
            }
            Written::Object(members) => serde_json::Value::Object(
                members
                    .into_iter()
                    .map(|(key, value)| (key, value.into_value()))
                    .collect(),
            ),
        }
    }
}

/// The compact JSON text.
impl std::fmt::Display for Written<serde_json::Value> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&serde_json::to_string(self).map_err(|_| std::fmt::Error)?)
    }
}

/// `v`, which is `level` levels inside the value `caller` writes, as
/// JSON: the fields of a record in the order its type declares them,
/// the keys of a map sorted. A value JSON has no form for (a set, bytes,
/// a function, the constructor of a variant among them, a channel, a
/// handle) is an error, wherever in the value it is.
pub(crate) fn value_to_json(
    caller: &str,
    v: &Value,
    level: usize,
) -> Result<Written<serde_json::Value>, VmError> {
    use serde_json::Value as J;
    ensure_encodable_depth(caller, level)?;
    let inner = |v: &Value| value_to_json(caller, v, level + 1);
    // `Some(x)` is written as `x` is, and so is a `json.Value` as the
    // JSON it holds (`json.Int(1)` as `1`); neither is a level of its own.
    let mut v = v;
    while let Value::Variant(variant) = v
        && (variant.is(bv::SOME) || variant.of(ty::JSON_VALUE))
        && let [content] = variant.fields()
    {
        v = content;
    }
    Ok(match v {
        Value::Int(n) => Written::Scalar(J::Number((*n).into())),
        Value::Float(f) => {
            Written::Scalar(serde_json::Number::from_f64(f.get()).map_or(J::Null, J::Number))
        }
        Value::Bool(b) => Written::Scalar(J::Bool(*b)),
        Value::String(s) => Written::Scalar(J::String(s.to_string())),
        Value::List(xs) => {
            xs.writable()?;
            let items: Result<Vec<_>, _> = xs.iter().map(|x| inner(&x)).collect();
            Written::Array(items?)
        }
        Value::Map(m) => {
            let entries: Result<Vec<_>, VmError> = m
                .iter()
                .map(|(k, v)| Ok((k.to_string(), inner(v)?)))
                .collect();
            Written::sorted(entries?)
        }
        Value::Tuple(vs) => {
            let items: Result<Vec<_>, _> = vs.iter().map(inner).collect();
            Written::Array(items?)
        }
        // A date, a time of day and a datetime are written as the text
        // silt shows of them (`2026-10-19`, `12:30:00.250000000`,
        // `2026-10-19T12:30:00`), which their decoders read back.
        Value::Record(record)
            if matches!(record.type_id(), ty::DATE | ty::TIME | ty::DATE_TIME) =>
        {
            Written::Scalar(J::String(v.to_string()))
        }
        Value::Record(record) => {
            let members: Result<Vec<_>, VmError> = record
                .named()
                .map(|(k, v)| Ok((k.to_string(), inner(v)?)))
                .collect();
            Written::Object(members?)
        }
        Value::Variant(variant) if variant.is(bv::NONE) || variant.is(bv::JSON_NULL) => {
            Written::Scalar(J::Null)
        }
        // (`fields` before `variant`, as the keys of an object sort.)
        Value::Variant(variant) => {
            let mut members = Vec::with_capacity(2);
            if !variant.fields().is_empty() {
                let items: Result<Vec<_>, _> = variant.fields().iter().map(inner).collect();
                members.push(("fields".into(), Written::Array(items?)));
            }
            let name = Written::Scalar(J::String(variant.name().into()));
            members.push(("variant".into(), name));
            Written::Object(members)
        }
        Value::Unit => Written::Scalar(J::Null),
        _ => return Err(unsupported_kind(caller, v)),
    })
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

// ── JSON helpers ────────────────────────────────────────────────────

/// `s`, the text of a date or a datetime, as chrono reads it: a year of
/// five or more digits, which silt writes without a sign
/// (`12026-01-01`), takes the `+` that chrono's `%Y` wants of one. A
/// year padded with zeros (`02026`) is no text silt writes, and stays
/// refused.
fn signed_year(s: &str) -> std::borrow::Cow<'_, str> {
    let digits = s.bytes().take_while(u8::is_ascii_digit).count();
    match digits > 4 && !s.starts_with('0') && s.as_bytes().get(digits) == Some(&b'-') {
        true => format!("+{s}").into(),
        false => s.into(),
    }
}

/// The date that the text `s` of a document writes: `YYYY-MM-DD`, the
/// year with more digits or a sign where it has them, as silt shows it
/// (`12026-01-01`, `-001-01-01`). Shared by the JSON and TOML decoders.
pub(crate) fn parse_date(s: &str) -> Result<NaiveDate, chrono::ParseError> {
    NaiveDate::parse_from_str(&signed_year(s), "%Y-%m-%d")
}

/// The message of a decoder's error for `s`, the string of a date that
/// is none, `why`.
pub(crate) fn invalid_date_message(s: &str, why: chrono::ParseError) -> std::string::String {
    format!("invalid date '{s}' (expected YYYY-MM-DD): {why}")
}

/// The message of a decoder's error for `s`, the string of a time of
/// day that is none, `why`.
pub(crate) fn invalid_time_message(s: &str, why: &str) -> std::string::String {
    format!("invalid time '{s}' (expected HH:MM:SS): {why}")
}

/// The time of day that the text `s` of a document writes: `HH:MM:SS`,
/// with a fraction of a second up to nanoseconds (`13:14:15.25`), or
/// `HH:MM`. Else why it is none: a second 60 is a leap second, which is
/// refused. Shared by the JSON and TOML decoders.
pub(crate) fn parse_time(s: &str) -> Result<NaiveTime, std::string::String> {
    let t = NaiveTime::parse_from_str(s, "%H:%M:%S%.f")
        .or_else(|_| NaiveTime::parse_from_str(s, "%H:%M"))
        .map_err(|e| e.to_string())?;
    match is_leap_second(t) {
        true => Err(LEAP_SECOND.into()),
        false => Ok(t),
    }
}

/// The date and time that the text `s` of a document writes, in UTC
/// where it has an offset: RFC 3339, the same with a space for the `T`,
/// or without an offset; seconds with a fraction up to nanoseconds, or
/// no seconds (`2024-03-09T07:30`); the year as in [`parse_date`]. Else
/// the message of the decoder's error: a second 60 is a leap second,
/// which is refused. Shared by the JSON and TOML decoders.
pub(crate) fn parse_datetime(s: &str) -> Result<NaiveDateTime, std::string::String> {
    let signed = signed_year(s);
    let read = &*signed;
    let offset = ["%Y-%m-%dT%H:%M:%S%.f%z", "%Y-%m-%d %H:%M:%S%.f%z"].into_iter();
    let naive = [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M",
    ]
    .into_iter();
    let dt = chrono::DateTime::parse_from_rfc3339(read)
        .ok()
        .into_iter()
        .chain(offset.filter_map(|f| chrono::DateTime::parse_from_str(read, f).ok()))
        .map(|dt| dt.naive_utc())
        .chain(naive.filter_map(|f| NaiveDateTime::parse_from_str(read, f).ok()))
        .next();
    match dt {
        Some(dt) if is_leap_second(dt.time()) => Err(leap_datetime_message(s)),
        Some(dt) => Ok(dt),
        None => Err(format!("invalid datetime '{s}'")),
    }
}

/// The record type `ty` a decoder builds, with its fields. `caller` is
/// the builtin on whose behalf the type is looked up (`json.parse`,
/// `toml.parse_list`, ...); it starts the error message. A builtin
/// record type (`Date`) has no decoder.
pub(crate) fn decodable_record(
    caller: &str,
    ty: &Arc<TypeInfo>,
) -> Result<Vec<(std::string::String, FieldType)>, VmError> {
    match &ty.shape {
        Shape::Record(fields) if !ty.is_builtin() => Ok(fields.clone()),
        _ => Err(VmError::new(format!(
            "{caller}: unknown record type '{}'",
            ty.name
        ))),
    }
}

/// `field_type` as silt writes it, a record type by its name.
pub(crate) fn shown(rt: &Runtime, field_type: &FieldType) -> std::string::String {
    field_type.shown(&|id| {
        rt.type_info(id)
            .map_or_else(Default::default, |info| info.name.clone())
    })
}

/// The type a field of `field_type` is decoded as by a decoder of a
/// format with no bytes (JSON, TOML): a type that holds `Bytes` has no
/// decoder, whatever the document holds (an empty list too).
pub(crate) fn without_bytes(rt: &Runtime, field_type: &FieldType) -> Option<FieldType> {
    field_type
        .holds_bytes()
        .then(|| FieldType::Unsupported(shown(rt, field_type)))
}

/// The record type `id` a record field names, from the program's types.
pub(crate) fn field_record_type(
    rt: &Runtime,
    caller: &str,
    id: TypeId,
) -> Result<Arc<TypeInfo>, VmError> {
    rt.type_info(id)
        .ok_or_else(|| VmError::new(format!("{caller}: unknown record type")))
}

/// Inner-decoder error type. Each variant carries the already-
/// constructed `JsonError` variant `Value` so `json_to_record` can
/// forward it into the outer `Err(...)` without a second format step.
/// A separate enum lets us distinguish "decoder recursion failed
/// cleanly" (surfaceable to silt) from "VM infrastructure bug"
/// (surfaced as `VmError`).
enum JsonDecodeErr {
    /// A decoded `JsonError` variant value ready to be wrapped in
    /// `Err(...)` — propagate unchanged.
    Variant(Value),
    /// A value was present for a type no decoder exists for (the type as
    /// written). `json_to_record` turns it into a `Other` that names
    /// the record field whose type this is.
    Unsupported(std::string::String),
    /// VM-internal failure (e.g. record type missing from globals).
    /// Caught at the call site and converted to a silt-visible
    /// `Err(Other(...))`.
    Vm(VmError),
}

impl From<VmError> for JsonDecodeErr {
    fn from(e: VmError) -> Self {
        JsonDecodeErr::Vm(e)
    }
}

/// Adapt a `JsonDecodeErr` into a silt-visible `Err(JsonError)` value.
fn decode_err_to_silt(e: JsonDecodeErr) -> Value {
    match e {
        JsonDecodeErr::Variant(v) => json_err_wrap(v),
        // Only record fields carry unsupported types, and
        // `json_to_record` names the field; this arm is the fallback.
        JsonDecodeErr::Unsupported(declared) => {
            json_unknown_err(format!("a value of type {declared} cannot be decoded"))
        }
        JsonDecodeErr::Vm(err) => json_unknown_err(err.message),
    }
}

/// The record of type `ty` (of the `fields`) that the document `json`,
/// which is `at` a path, is: `Ok` of it, or `Err` of the `JsonError`
/// that says why there is none.
fn json_to_record(
    rt: &Runtime,
    ty: &Arc<TypeInfo>,
    fields: &[(std::string::String, FieldType)],
    json: &serde_json::Value,
    at: &At,
) -> Result<Value, VmError> {
    let serde_json::Value::Object(obj) = json else {
        return Ok(json_type_mismatch_err("object", json_type_name(json), at));
    };
    // (In the order the type declares them: the record's own.)
    let mut record_fields = Vec::with_capacity(fields.len());
    for (field_name, field_type) in fields {
        let unsupported = without_bytes(rt, field_type);
        let field_type = unsupported.as_ref().unwrap_or(field_type);
        match obj.get(field_name) {
            Some(json_val) => {
                match json_to_typed_value(rt, json_val, field_type, &at.key(field_name)) {
                    Ok(val) => record_fields.push(val),
                    Err(JsonDecodeErr::Unsupported(declared)) => {
                        return Ok(json_unknown_err(unsupported_field_type_message(
                            &ty.name, field_name, &declared,
                        )));
                    }
                    Err(e) => return Ok(decode_err_to_silt(e)),
                }
            }
            None => match field_type {
                FieldType::Option(_) => record_fields.push(Value::variant(bv::NONE, Vec::new())),
                _ => {
                    return Ok(json_missing_field_err(&at.key(field_name).to_string()));
                }
            },
        }
    }
    Ok(Value::variant(
        bv::OK,
        vec![Value::record(ty.clone(), record_fields)],
    ))
}

/// The type of the elements `json.parse_list` and of the values
/// `json.parse_map` (`caller`) decode, which their type argument `ty`
/// names: `Int`, `Float`, `String`, `Bool`, `json.Value` or a record
/// type. Any other type is an error.
fn element_type(caller: &str, ty: Type) -> Result<FieldType, VmError> {
    Ok(match ty {
        Type::Primitive(field_type) => field_type,
        Type::Named(ty) if ty.id == ty::JSON_VALUE => FieldType::JsonValue,
        Type::Named(ty) => {
            decodable_record(caller, ty)?;
            FieldType::Record(ty.id)
        }
    })
}

/// The list of values of the field type `element` that the document
/// `json`, an array, holds.
fn json_to_list(rt: &Runtime, element: &FieldType, json: &serde_json::Value) -> Value {
    let serde_json::Value::Array(items) = json else {
        return json_type_mismatch_err("array", json_type_name(json), &At::Top);
    };
    let mut values = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        match json_to_typed_value(rt, item, element, &At::Top.index(index)) {
            Ok(value) => values.push(value),
            Err(e) => return decode_err_to_silt(e),
        }
    }
    Value::variant(bv::OK, vec![Value::list(values)])
}

/// The map of values of the field type `element` that the document
/// `json`, an object, holds.
fn json_to_map(rt: &Runtime, element: &FieldType, json: &serde_json::Value) -> Value {
    let serde_json::Value::Object(obj) = json else {
        return json_type_mismatch_err("object", json_type_name(json), &At::Top);
    };
    let mut map = Vec::with_capacity(obj.len());
    for (key, json_val) in obj {
        match json_to_typed_value(rt, json_val, element, &At::Top.key(key)) {
            Ok(val) => map.push((Value::String(key.clone().into()), val)),
            Err(e) => return decode_err_to_silt(e),
        }
    }
    Value::variant(bv::OK, vec![Value::map(map)])
}

/// The `json.Value` that holds the document `json`. A number is an
/// `Int` when it is held as one ([`int_of`]), as for an `Int` field, and
/// a `Float` otherwise.
pub(crate) fn json_value(json: &serde_json::Value) -> Value {
    match json {
        serde_json::Value::Null => Value::variant(bv::JSON_NULL, Vec::new()),
        serde_json::Value::Bool(b) => Value::variant(bv::JSON_BOOL, vec![Value::Bool(*b)]),
        serde_json::Value::Number(n) => match (n.as_i64(), n.as_f64().and_then(Float::new)) {
            (Some(i), _) => Value::variant(bv::JSON_INT, vec![Value::Int(i)]),
            (None, Some(f)) => Value::variant(bv::JSON_FLOAT, vec![Value::Float(f)]),
            // (A document has no NaN.)
            (None, None) => Value::variant(bv::JSON_NULL, Vec::new()),
        },
        serde_json::Value::String(s) => {
            Value::variant(bv::JSON_STRING, vec![Value::String(s.clone().into())])
        }
        serde_json::Value::Array(items) => Value::variant(
            bv::JSON_ARRAY,
            vec![Value::list(items.iter().map(json_value).collect())],
        ),
        serde_json::Value::Object(obj) => Value::variant(
            bv::JSON_OBJECT,
            vec![Value::map(obj.iter().map(|(key, item)| {
                (Value::String(key.clone().into()), json_value(item))
            }))],
        ),
    }
}

/// Decode a `serde_json::Value` into a silt `Value` of the expected
/// shape, short-circuiting with a typed `JsonError` variant the moment
/// a mismatch is found. `JsonDecodeErr::Variant(...)` always carries a
/// ready-built `JsonError` variant (not wrapped in `Err(..)`); the
/// caller wraps it before returning to silt.
fn json_to_typed_value(
    rt: &Runtime,
    json: &serde_json::Value,
    expected: &FieldType,
    at: &At,
) -> Result<Value, JsonDecodeErr> {
    // The `TypeMismatch` and the `Other` of the value here: each ends
    // with its path.
    let mismatch = |expected: &str, actual: &str| -> JsonDecodeErr {
        JsonDecodeErr::Variant(Value::variant(
            bv::JSON_TYPE_MISMATCH,
            vec![
                Value::String(expected.into()),
                Value::String(format!("{actual}{}", at.suffix()).into()),
            ],
        ))
    };
    let unknown = |msg: String| -> JsonDecodeErr {
        JsonDecodeErr::Variant(Value::variant(
            bv::JSON_OTHER,
            vec![Value::String(format!("{msg}{}", at.suffix()).into())],
        ))
    };
    match expected {
        FieldType::String => match json {
            serde_json::Value::String(s) => Ok(Value::String(s.clone().into())),
            _ => Err(mismatch("String", json_type_name(json))),
        },
        // A number is an `Int` when it is a whole number an `Int` can
        // be (`1`, `1.0`, `1e3`); one with a fraction, or out of range,
        // is no `Int`, and nothing is cut off or saturated.
        FieldType::Int => match json {
            serde_json::Value::Number(n) => int_of(n)
                .map(Value::Int)
                .map_err(|what| mismatch("Int", what)),
            _ => Err(mismatch("Int", json_type_name(json))),
        },
        FieldType::Float => match json {
            serde_json::Value::Number(n) => match n.as_f64().and_then(Float::new) {
                Some(f) => Ok(Value::Float(f)),
                None => Err(unknown("expected Float, got non-numeric number".into())),
            },
            _ => Err(mismatch("Float", json_type_name(json))),
        },
        FieldType::Bool => match json {
            serde_json::Value::Bool(b) => Ok(Value::Bool(*b)),
            _ => Err(mismatch("Bool", json_type_name(json))),
        },
        FieldType::List(inner) => match json {
            serde_json::Value::Array(arr) => {
                let mut values = Vec::new();
                for (index, item) in arr.iter().enumerate() {
                    values.push(json_to_typed_value(rt, item, inner, &at.index(index))?);
                }
                Ok(Value::list(values))
            }
            _ => Err(mismatch("List", json_type_name(json))),
        },
        FieldType::Option(inner) => match json {
            serde_json::Value::Null => Ok(Value::variant(bv::NONE, Vec::new())),
            _ => {
                let val = json_to_typed_value(rt, json, inner, at)?;
                Ok(Value::variant(bv::SOME, vec![val]))
            }
        },
        FieldType::Map(inner) => match json {
            serde_json::Value::Object(obj) => {
                let mut map = Vec::with_capacity(obj.len());
                for (key, item) in obj {
                    let val = json_to_typed_value(rt, item, inner, &at.key(key))?;
                    map.push((Value::String(key.clone().into()), val));
                }
                Ok(Value::map(map))
            }
            _ => Err(mismatch("Map", json_type_name(json))),
        },
        FieldType::Tuple(elems) => match json {
            serde_json::Value::Array(arr) if arr.len() == elems.len() => {
                let mut values = Vec::with_capacity(elems.len());
                for (index, (item, elem)) in arr.iter().zip(elems).enumerate() {
                    values.push(json_to_typed_value(rt, item, elem, &at.index(index))?);
                }
                Ok(Value::tuple(values))
            }
            serde_json::Value::Array(arr) => Err(unknown(format!(
                "expected an array of {} elements for a tuple, got {}",
                elems.len(),
                arr.len()
            ))),
            _ => Err(mismatch("Tuple", json_type_name(json))),
        },
        FieldType::Unsupported(declared) => Err(JsonDecodeErr::Unsupported(declared.clone())),
        FieldType::JsonValue => Ok(json_value(json)),
        FieldType::Bytes => Err(JsonDecodeErr::Unsupported("Bytes".into())),
        FieldType::Date => match json {
            serde_json::Value::String(s) => parse_date(s)
                .map(make_date)
                .map_err(|e| unknown(invalid_date_message(s, e))),
            _ => Err(mismatch("date string", json_type_name(json))),
        },
        FieldType::Time => match json {
            serde_json::Value::String(s) => parse_time(s)
                .map(make_time)
                .map_err(|e| unknown(invalid_time_message(s, &e))),
            _ => Err(mismatch("time string", json_type_name(json))),
        },
        FieldType::DateTime => match json {
            serde_json::Value::String(s) => parse_datetime(s).map(make_datetime).map_err(unknown),
            _ => Err(mismatch("datetime string", json_type_name(json))),
        },
        FieldType::Record(id) => {
            let ty = field_record_type(rt, "json.parse", *id)?;
            let fields = decodable_record("json.parse", &ty)?;
            let result = json_to_record(rt, &ty, &fields, json, at)?;
            match result {
                Value::Variant(variant) if variant.is(bv::OK) && variant.fields().len() == 1 => {
                    Ok(variant.fields()[0].clone())
                }
                Value::Variant(variant) if variant.is(bv::ERR) && variant.fields().len() == 1 => {
                    // Re-wrap as JsonDecodeErr::Variant so the outer
                    // caller forwards it unchanged. Its field is
                    // already a JsonError variant value.
                    Err(JsonDecodeErr::Variant(variant.fields()[0].clone()))
                }
                _ => Err(unknown(format!("failed to parse {}", ty.name))),
            }
        }
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
    json_to_typed_value(rt, json, expected, &At::Top).map_err(|e| {
        let error = decode_err_to_silt(e);
        let because = match &error {
            Value::Variant(err) => match err.fields() {
                [Value::Variant(why)] => error_text(why.name(), why.fields()),
                _ => None,
            },
            _ => None,
        };
        because.unwrap_or_else(|| error.to_string())
    })
}

/// The value of type `ty` that the document `parsed` is, for
/// `json.parse` and `json.decode` (`caller`): `Ok` of it, or `Err` of
/// the `JsonError` that says why there is none.
fn decode_document(
    rt: &Runtime,
    caller: &str,
    parsed: Result<serde_json::Value, serde_json::Error>,
    ty: Type,
) -> Result<Value, VmError> {
    let json = match parsed {
        Ok(json) => json,
        Err(e) => return Ok(json_result_err(&e)),
    };
    let field_type = match ty {
        // A primitive type: a JSON scalar, decoded as it is.
        Type::Primitive(field_type) => field_type,
        Type::Named(ty) if ty.id == ty::JSON_VALUE => FieldType::JsonValue,
        Type::Named(ty) => {
            let fields = decodable_record(caller, ty)?;
            return json_to_record(rt, ty, &fields, &json, &At::Top);
        }
    };
    Ok(
        match json_to_typed_value(rt, &json, &field_type, &At::Top) {
            Ok(val) => Value::variant(bv::OK, vec![val]),
            Err(e) => decode_err_to_silt(e),
        },
    )
}

// ── The functions ───────────────────────────────────────────────────

builtins! {
    fn decode(vm, value: &Value, ty: Type) -> Result<Value, VmError> {
        // A `json.Value` holds only what JSON has a form for, so the one
        // way to fail here is a value nested past the limit: an `Err`,
        // as `json.parse` gives one for a document nested too deep.
        let mut json = match value_to_json("json.decode", value, 0) {
            Ok(json) => json.into_value(),
            Err(e) => return Ok(json_unknown_err(e.message)),
        };
        whole_numbers(&mut json);
        decode_document(&vm.runtime, "json.decode", Ok(json), ty)
    }

    fn parse(vm, s: &str, ty: Type) -> Result<Value, VmError> {
        decode_document(&vm.runtime, "json.parse", parse_document(s.as_bytes()), ty)
    }

    fn parse_list(vm, s: &str, ty: Type) -> Result<Value, VmError> {
        let element = element_type("json.parse_list", ty)?;
        Ok(match parse_document(s.as_bytes()) {
            Ok(json) => json_to_list(&vm.runtime, &element, &json),
            Err(e) => json_result_err(&e),
        })
    }

    fn parse_map(vm, s: &str, ty: Type) -> Result<Value, VmError> {
        let element = element_type("json.parse_map", ty)?;
        Ok(match parse_document(s.as_bytes()) {
            Ok(json) => json_to_map(&vm.runtime, &element, &json),
            Err(e) => json_result_err(&e),
        })
    }

    fn stringify(value: &Value) -> Result<String, VmError> {
        Ok(value_to_json("json.stringify", value, 0)?.to_string())
    }

    fn pretty(value: &Value) -> Result<String, VmError> {
        let json = value_to_json("json.pretty", value, 0)?;
        Ok(serde_json::to_string_pretty(&json).unwrap_or_else(|_| json.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A field whose type holds `Bytes` has no decoder, whatever the
    /// document holds there: an empty list, a `null` for an `Option`,
    /// or a list of numbers.
    #[test]
    fn a_field_that_holds_bytes_is_never_decoded() {
        use crate::defs::{DefId, TypeId};
        let vm = crate::vm::Vm::new(crate::vm::HostIo::process());
        for (field, text) in [
            (FieldType::List(Box::new(FieldType::Bytes)), r#"{"b": []}"#),
            (FieldType::List(Box::new(FieldType::Bytes)), r#"{"b": [1]}"#),
            (
                FieldType::Option(Box::new(FieldType::Bytes)),
                r#"{"b": null}"#,
            ),
            (
                FieldType::Tuple(vec![FieldType::Int, FieldType::Bytes]),
                r#"{"b": [1, 2]}"#,
            ),
        ] {
            let shown = shown(&vm.runtime, &field);
            let fields = vec![("b".to_string(), field)];
            let ty = TypeInfo::new_record(TypeId(DefId(9000)), "B", fields.clone());
            let json = serde_json::from_str(text).expect("a document");
            let decoded =
                json_to_record(&vm.runtime, &ty, &fields, &json, &At::Top).expect("a value");
            assert_eq!(
                decoded.to_string(),
                format!(
                    "Err(cannot decode field `b` of `B`: a value of type {shown} cannot be decoded)"
                ),
                "{text}"
            );
        }
    }

    /// A time and a datetime are read with a fraction of a second up to
    /// nanoseconds, as silt shows them, and without one.
    #[test]
    fn times_are_read_with_a_fraction_of_a_second() {
        let time = |s: &str| parse_time(s).map(|t| t.to_string()).ok();
        assert_eq!(time("13:14:15.250000000").as_deref(), Some("13:14:15.250"));
        assert_eq!(
            time("13:14:15.123456789").as_deref(),
            Some("13:14:15.123456789")
        );
        assert_eq!(time("13:14:15").as_deref(), Some("13:14:15"));
        assert_eq!(time("13:14").as_deref(), Some("13:14:00"));
        assert_eq!(time("13:14:15.").as_deref(), None);
        let datetime = |s: &str| parse_datetime(s).map(|t| t.to_string()).ok();
        assert_eq!(
            datetime("2026-10-19T13:14:15.5").as_deref(),
            Some("2026-10-19 13:14:15.500")
        );
        assert_eq!(
            datetime("2026-10-19 13:14:15.5+02:00").as_deref(),
            Some("2026-10-19 11:14:15.500")
        );
        assert_eq!(
            datetime("2026-10-19T13:14").as_deref(),
            Some("2026-10-19 13:14:00")
        );
        assert_eq!(datetime("2026-10-19").as_deref(), None);
        // A year as silt shows it: five digits without a sign, or a sign.
        let date = |s: &str| parse_date(s).map(|d| d.to_string()).ok();
        assert_eq!(date("12026-01-02").as_deref(), Some("+12026-01-02"));
        assert_eq!(date("-001-01-02").as_deref(), Some("-0001-01-02"));
        assert_eq!(date("2026-1-2").as_deref(), Some("2026-01-02"));
        assert_eq!(date("02026-01-02").as_deref(), None);
        assert_eq!(date("002026-01-02").as_deref(), None);
        assert_eq!(datetime("02026-01-02T03:04").as_deref(), None);
        assert_eq!(
            datetime("12026-01-02T03:04").as_deref(),
            Some("+12026-01-02 03:04:00")
        );
    }

    /// A second 60, a leap second, is refused by both readers, at any
    /// minute, with a fraction and with an offset; second 59 is read.
    #[test]
    fn a_leap_second_is_refused() {
        assert_eq!(parse_time("23:59:60"), Err(LEAP_SECOND.to_string()));
        assert_eq!(parse_time("12:30:60.5"), Err(LEAP_SECOND.to_string()));
        assert!(parse_time("23:59:59.999999999").is_ok());
        for s in [
            "2016-12-31T23:59:60",
            "2016-12-31 23:59:60.5",
            "2016-12-31T23:59:60Z",
            "2017-01-01T00:59:60+01:00",
        ] {
            assert_eq!(parse_datetime(s), Err(leap_datetime_message(s)), "{s}");
        }
        assert!(parse_datetime("2016-12-31T23:59:59").is_ok());
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
                let json = value_to_json("json.stringify", &deepest, 0).expect("written");
                let text = json.to_string();
                let pretty = serde_json::to_string_pretty(&json).expect("written");
                drop(json);
                let too_deep = nested(MAX_ENCODE_DEPTH + 2);
                let refused = value_to_json("json.pretty", &too_deep, 0).unwrap_err();
                (text, pretty, refused.message)
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
