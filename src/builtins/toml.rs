//! `toml.*` builtin functions: parse TOML documents into typed silt records
//! and serialize silt values into TOML text.
//!
//! The API mirrors the `json` module in `src/builtins/json.rs`, with each
//! fallible call returning a typed `TomlError`. See
//! `module.rs::builtin_error_enum_variants_with_arity` for Phase 0
//! background:
//! - `toml.parse(T: Type, s: String) -> Result(T, TomlError)` — parse a top-
//!   level TOML table into a record of type `T`.
//! - `toml.parse_list(T: Type, s: String) -> Result(List(T), TomlError)` —
//!   parse a document whose top-level table has one key, an array: a
//!   `[[items]]` array of tables for a record type, `xs = [1, 2]` for `Int`.
//! - `toml.parse_map(V: Type, s: String) -> Result(Map(String, V), TomlError)`
//!   — parse a top-level table as a `Map(String, V)`.
//! - `toml.stringify(v) -> Result(String, TomlError)` — serialize a silt
//!   value to compact TOML. Unlike `json.stringify` this is fallible because
//!   TOML requires a table at the top level.
//! - `toml.pretty(v) -> Result(String, TomlError)` — serialize a silt value
//!   to TOML as `stringify` does, except that an array of plain values is
//!   written one element per line.
//!
//! ## TOML-specific types
//!
//! TOML's native date/time variants (Offset Date-Time, Local Date-Time,
//! Local Date, Local Time) are translated to strings using the same ISO 8601
//! shapes that `json.parse` already accepts for `Date`, `Time`, and `DateTime`
//! fields — so a `Date` field in the record happily receives a bare TOML
//! `1979-05-27`, a `DateTime` field receives `1979-05-27T07:32:00Z`, and a
//! `Time` field receives `07:32:00`. The one typed decoder (`decode.rs`)
//! reads a document of either module, so the two stay aligned. The encoder
//! writes a `Date`, `Time` and `DateTime` as such a literal
//! (`literal`), so each reads back as the value it was.
//!
//! ## Semantics
//!
//! - Parse errors pass through the `toml` crate's error text (`format!("{e}")`).
//! - Missing `Option` fields default to `None`.
//! - Missing required fields return `Err(...)`.
//! - Type mismatches return an `Err(...)` naming the field.
//! - Integer overflow: TOML integers are `i64` per spec, which matches silt's
//!   `Int` exactly; no demotion risk.

use std::sync::Arc;

use chrono::{NaiveDate, NaiveTime};

use crate::typeinfo::{TypeInfo, bv};
use crate::value::Value;
use crate::vm::VmError;

use super::data::{self, Data, Format};
use super::decode::{
    self, At, Document, Node, Numbers, Rules, decodable_record, element_type, shown,
};
use super::typed::{Type, builtins};

// ── TomlError helpers ────────────────────────────────────────────────
//
// Phase 1 of the stdlib error redesign: every fallible toml.* call now
// surfaces a typed `TomlError` variant wrapped in `Err(...)` instead of
// a bare `Err(String)`. Mirrors `json_*_err` in `src/builtins/json.rs`.
//
// `TomlError` variants:
//   Syntax(message, byte_offset)
//   TypeMismatch(expected, actual)
//   MissingField(name)
//   Other(message)

fn toml_err_wrap(inner: Value) -> Value {
    Value::variant(bv::ERR, vec![inner])
}

/// Classify a `toml::de::Error` into one of the `TomlError` variants.
/// The `toml` crate exposes a `span()` method that yields a byte range
/// into the source; we take the start as the offset. For errors that
/// don't have a span (extremely rare in practice) we fall back to 0.
pub(crate) fn toml_de_error_to_variant(err: &::toml::de::Error) -> Value {
    let offset = err.span().map(|s| s.start as i64).unwrap_or(0);
    Value::variant(
        bv::TOML_SYNTAX,
        vec![Value::String(err.message().into()), Value::Int(offset)],
    )
}

/// Build a full `Err(TomlError)` from a `toml::de::Error`.
pub(crate) fn toml_de_result_err(err: &::toml::de::Error) -> Value {
    toml_err_wrap(toml_de_error_to_variant(err))
}

/// Build `Err(Other(msg))` for ad-hoc failures (unknown type,
/// serialization errors, document-shape violations, etc.).
pub(crate) fn toml_unknown_err<S: Into<Arc<str>>>(msg: S) -> Value {
    toml_err_wrap(Value::variant(
        bv::TOML_OTHER,
        vec![Value::String(msg.into())],
    ))
}

/// What `TomlError`'s `message` says of the variant `tag` with `fields`:
/// `None` if they are no variant of it.
pub(crate) fn error_text(tag: &str, fields: &[Value]) -> Option<String> {
    Some(match (tag, fields) {
        ("Syntax", [Value::String(m), Value::Int(offset)]) => {
            format!("toml syntax error at byte {offset}: {m}")
        }
        ("TypeMismatch", [Value::String(exp), Value::String(act)]) => {
            format!("toml type mismatch: expected {exp}, got {act}")
        }
        ("MissingField", [Value::String(n)]) => {
            format!("toml missing field: {n}")
        }
        ("Other", [Value::String(m)]) => m.to_string(),
        _ => return None,
    })
}

// ── Helpers ─────────────────────────────────────────────────────────

/// `data` as TOML: a member of a table that is null (a `None`, a
/// `json.Null`) is left out, as TOML has no null, and reads back as
/// `None`; a date and a time are TOML's own literals ([`literal`]).
/// (Data written as TOML holds no other null, no `()` and no `Bytes`:
/// [`data::data`] refuses them.) The writer puts the plain keys of a
/// table before its sub-tables, as TOML has them.
struct Toml<'d>(&'d Data);

impl serde::Serialize for Toml<'_> {
    fn serialize<S: serde::Serializer>(&self, to: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{Error, SerializeMap, SerializeSeq};
        match self.0 {
            Data::Int(n) => to.serialize_i64(*n),
            Data::Float(f) => to.serialize_f64(*f),
            Data::Bool(b) => to.serialize_bool(*b),
            Data::String(s) => to.serialize_str(s),
            Data::Date(_) | Data::Time(_) | Data::DateTime(..) => match literal(self.0) {
                Ok(literal) => literal.serialize(to),
                Err(text) => to.serialize_str(&text),
            },
            Data::Array(items) | Data::Tuple(items) => {
                let mut seq = to.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(&Toml(item))?;
                }
                seq.end()
            }
            Data::Table(members) => {
                let mut map = to.serialize_map(None)?;
                for (key, value) in members {
                    if !matches!(value, Data::Null) {
                        map.serialize_entry(key, &Toml(value))?;
                    }
                }
                map.end()
            }
            Data::Json(content) => Toml(content).serialize(to),
            Data::Null | Data::Unit | Data::Bytes(_) => {
                Err(S::Error::custom("TOML has no form for the value"))
            }
        }
    }
}

/// `data`, a date, a time of day or a datetime, as a TOML literal
/// (`2026-10-19`, `12:30:00.25`, `2026-10-19T12:30:00`), or, for one
/// that no literal holds (a year outside 0000-9999, fields that are no
/// day or time of the calendar), `Err` of the text silt shows of it,
/// which toml writes as a string. `toml.parse` reads a literal and the
/// text alike.
fn literal(data: &Data) -> Result<::toml::value::Datetime, std::string::String> {
    use ::toml::value::Datetime;
    let date = |date: &data::Date| {
        let year = u16::try_from(date.year).ok().filter(|year| *year <= 9999)?;
        let (month, day) = (u8::try_from(date.month).ok()?, u8::try_from(date.day).ok()?);
        NaiveDate::from_ymd_opt(year.into(), month.into(), day.into())?;
        Some(::toml::value::Date { year, month, day })
    };
    let time = |time: &data::Time| {
        let hour = u8::try_from(time.hour).ok()?;
        let minute = u8::try_from(time.minute).ok()?;
        let second = u8::try_from(time.second).ok()?;
        // (Below a second: chrono takes one more for a leap second.)
        let nanosecond = u32::try_from(time.ns)
            .ok()
            .filter(|ns| *ns < 1_000_000_000)?;
        NaiveTime::from_hms_nano_opt(hour.into(), minute.into(), second.into(), nanosecond)?;
        Some(::toml::value::Time {
            hour,
            minute,
            second,
            nanosecond,
        })
    };
    let (literal, text) = match data {
        Data::Date(d) => (date(d).map(|d| (Some(d), None)), d.to_string()),
        Data::Time(t) => (time(t).map(|t| (None, Some(t))), t.to_string()),
        Data::DateTime(at) => {
            let (d, t) = &**at;
            (
                date(d).zip(time(t)).map(|(d, t)| (Some(d), Some(t))),
                format!("{d}T{t}"),
            )
        }
        _ => (None, std::string::String::new()),
    };
    literal
        .map(|(date, time)| Datetime {
            date,
            time,
            offset: None,
        })
        .ok_or(text)
}

/// `data` as the document `caller` writes. TOML's top level must be a
/// table: a value that is no record, no map (a scalar, an array, a
/// `None`) is an error naming what it is.
fn top_level(caller: &str, data: Data) -> Result<Data, VmError> {
    let kind = match &data {
        Data::Table(_) => return Ok(data),
        Data::Array(_) | Data::Tuple(_) => "array",
        Data::Int(_) => "integer",
        Data::Float(_) => "float",
        Data::Bool(_) => "boolean",
        Data::String(_) => "string",
        Data::Date(_) | Data::Time(_) | Data::DateTime(..) => match literal(&data) {
            Ok(_) => "datetime",
            Err(_) => "string",
        },
        Data::Null | Data::Unit | Data::Bytes(_) | Data::Json(_) => "null",
    };
    Err(VmError::new(format!(
        "{caller}: top-level value must be a table/record, got {kind}"
    )))
}

// ── Decoding a document ─────────────────────────────────────────────

/// How a TOML document decodes: TOML says which kind a number is, so a
/// float is no `Int` whatever its value; a date and a time are TOML's
/// literals or strings; TOML has no null (a key left out is a `None`);
/// no `json.Value` is decoded.
const RULES: Rules = Rules {
    caller: "toml.parse",
    table: "table",
    dates: ["date", "time", "datetime"],
    numbers: Numbers::Kinds,
    json_value: false,
    errors: [
        bv::TOML_TYPE_MISMATCH,
        bv::TOML_MISSING_FIELD,
        bv::TOML_OTHER,
    ],
};

impl Document for ::toml::Value {
    type Table = ::toml::Table;
    const RULES: &'static Rules = &RULES;

    fn node(&self) -> Node<'_, Self> {
        match self {
            ::toml::Value::String(s) => Node::String(s),
            ::toml::Value::Integer(n) => Node::Int(*n),
            ::toml::Value::Float(f) => Node::Float(*f),
            ::toml::Value::Boolean(b) => Node::Bool(*b),
            ::toml::Value::Datetime(dt) => Node::Datetime(dt.to_string()),
            ::toml::Value::Array(items) => Node::Array(items),
            ::toml::Value::Table(table) => Node::Table(table),
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            ::toml::Value::String(_) => "string",
            ::toml::Value::Integer(_) => "integer",
            ::toml::Value::Float(_) => "float",
            ::toml::Value::Boolean(_) => "boolean",
            ::toml::Value::Datetime(_) => "datetime",
            ::toml::Value::Array(_) => "array",
            ::toml::Value::Table(_) => "table",
        }
    }

    fn get<'d>(table: &'d ::toml::Table, key: &str) -> Option<&'d Self> {
        table.get(key)
    }

    fn entries(table: &::toml::Table) -> impl ExactSizeIterator<Item = (&str, &Self)> {
        table.iter().map(|(key, value)| (key.as_str(), value))
    }
}

// ── The functions ───────────────────────────────────────────────────

/// The record type a `type a` argument of `toml.<name>` is.
fn record_type<'a>(name: &str, ty: Type<'a>) -> Result<&'a Arc<TypeInfo>, VmError> {
    match ty {
        Type::Named(ty) => Ok(ty),
        Type::Primitive(_) => Err(VmError::new(format!(
            "toml.{name}: type argument must be a record type"
        ))),
    }
}

/// `value` as TOML text, for `toml.<name>`, `pretty` or not:
/// `Ok(text)`, or the `TomlError` of a value that TOML has no form for.
fn written(name: &str, value: &Value, pretty: bool) -> Value {
    let caller = format!("toml.{name}");
    let data = match data::data(&caller, Format::Toml, value).and_then(|d| top_level(&caller, d)) {
        Ok(data) => data,
        Err(e) => return toml_unknown_err(e.message),
    };
    let text = match pretty {
        true => ::toml::to_string_pretty(&Toml(&data)),
        false => ::toml::to_string(&Toml(&data)),
    };
    match text {
        Ok(s) => Value::variant(bv::OK, vec![Value::String(s.into())]),
        Err(e) => toml_unknown_err(format!("{caller}: {e}")),
    }
}

builtins! {
    fn parse(vm, s: &str, ty: Type) -> Result<Value, VmError> {
        let ty = record_type("parse", ty)?;
        let fields = decodable_record("toml.parse", ty)?;
        Ok(match ::toml::from_str::<::toml::Value>(s) {
            Ok(tv) => decode::result(&RULES, decode::record(&vm.runtime, ty, fields, &tv, &At::Top)),
            Err(e) => toml_de_result_err(&e),
        })
    }

    fn parse_list(vm, s: &str, ty: Type) -> Result<Value, VmError> {
        let element = element_type(&RULES, "toml.parse_list", ty)?;
        let tv = match ::toml::from_str::<::toml::Value>(s) {
            Ok(tv) => tv,
            Err(e) => return Ok(toml_de_result_err(&e)),
        };
        // TOML's top level is always a table: the list is the array
        // under its one key (`[[points]]` for records, `xs = [1, 2]`).
        let ::toml::Value::Table(table) = &tv else {
            let error = decode::Error::mismatch("table", tv.kind(), &At::Top);
            return Ok(decode::result(&RULES, Err(error)));
        };
        let mut entries = table.iter();
        let (Some((key, only)), None) = (entries.next(), entries.next()) else {
            return Ok(toml_unknown_err(format!(
                "toml.parse_list({}): expected a document with exactly one top-level key, an array, found {} keys",
                shown(&vm.runtime, &element),
                table.len()
            )));
        };
        let at = At::Top.key(key);
        Ok(decode::result(&RULES, decode::list(&vm.runtime, &element, only, &at)))
    }

    fn parse_map(vm, s: &str, ty: Type) -> Result<Value, VmError> {
        let element = element_type(&RULES, "toml.parse_map", ty)?;
        Ok(match ::toml::from_str::<::toml::Value>(s) {
            Ok(tv) => decode::result(&RULES, decode::map(&vm.runtime, &element, &tv, &At::Top)),
            Err(e) => toml_de_result_err(&e),
        })
    }

    fn stringify(value: &Value) -> Value {
        written("stringify", value, false)
    }

    fn pretty(value: &Value) -> Value {
        written("pretty", value, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typeinfo::FieldType;

    /// A field whose type holds `Bytes` has no decoder, whatever the
    /// document holds there, an empty array too, or nothing.
    #[test]
    fn a_field_that_holds_bytes_is_never_decoded() {
        use crate::defs::{DefId, TypeId};
        let vm = crate::vm::Vm::new(crate::vm::HostIo::process());
        let fields = vec![("b".to_string(), FieldType::List(Box::new(FieldType::Bytes)))];
        let ty = TypeInfo::new_record(TypeId(DefId(9000)), "B", fields.clone());
        for text in ["b = []", "b = [1]", ""] {
            let doc: ::toml::Value = ::toml::from_str(text).expect("a document");
            let decoded = decode::result(
                &RULES,
                decode::record(&vm.runtime, &ty, &fields, &doc, &At::Top),
            );
            assert_eq!(
                decoded.to_string(),
                "Err(cannot decode field `b` of `B`: a value of type List(Bytes) cannot be decoded)",
                "{text}"
            );
        }
    }
    use crate::builtins::data::MAX_ENCODE_DEPTH;

    /// The deepest value the encoder writes (a map in a list in a map,
    /// and so on) is written on a stack of 16 MiB, the least a thread
    /// that runs silt code has (`scheduler::WORKER_STACK_BYTES` where
    /// pointers have 32 bits), in a debug build too; and a value one
    /// level deeper is refused with a `Other`.
    #[test]
    fn the_deepest_value_is_written_on_the_smallest_stack() {
        let (written, refused) = std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                // `levels` levels above the Int: a map at the even
                // ones, a list of it at the odd ones.
                let nested = |levels: usize| {
                    (0..levels)
                        .rev()
                        .fold(Value::Int(1), |inner, level| match level % 2 {
                            0 => Value::map([(Value::String("k".into()), inner)]),
                            _ => Value::list(vec![inner]),
                        })
                };
                let text = written("stringify", &nested(MAX_ENCODE_DEPTH), false);
                let pretty = written("pretty", &nested(MAX_ENCODE_DEPTH), true);
                let refused = written("pretty", &nested(MAX_ENCODE_DEPTH + 1), true);
                (
                    [text, pretty].map(|v| v.to_string().len()),
                    format!("{refused:?}"),
                )
            })
            .expect("a thread")
            .join()
            .expect("the thread ends");
        assert!(
            written.iter().all(|len| *len > MAX_ENCODE_DEPTH),
            "{written:?}"
        );
        assert_eq!(
            refused,
            format!(
                "Err(Other(\"toml.pretty: the value is nested more than \
                 {MAX_ENCODE_DEPTH} levels deep\"))"
            )
        );
    }
}
