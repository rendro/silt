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
//!   parse a document whose top-level shape is a single `[[items]]` array-of-
//!   tables section.
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
//! `Time` field receives `07:32:00`. This reuses the json code path for
//! date parsing, keeping the two modules' behavior aligned. The encoder
//! writes a `Date`, `Time` and `DateTime` as such a literal
//! (`time_literal`), so each reads back as the value it was.
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

use crate::typeinfo::{FieldType, TypeInfo, bv, ty};
use crate::value::{Float, Value};
use crate::vm::{Runtime, VmError};

use super::json::{
    decodable_record, ensure_encodable_depth, field_record_type, invalid_date_message,
    invalid_time_message, parse_date, parse_datetime, parse_time, unsupported_field_type_message,
    unsupported_kind, without_bytes,
};
use super::time::{make_date, make_datetime, make_time};
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

/// Build `Err(TypeMismatch(expected, actual))`.
pub(crate) fn toml_type_mismatch_err(expected: &str, actual: &str) -> Value {
    toml_err_wrap(Value::variant(
        bv::TOML_TYPE_MISMATCH,
        vec![Value::String(expected.into()), Value::String(actual.into())],
    ))
}

/// Build `Err(MissingField(name))`.
pub(crate) fn toml_missing_field_err(name: &str) -> Value {
    toml_err_wrap(Value::variant(
        bv::TOML_MISSING_FIELD,
        vec![Value::String(name.into())],
    ))
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

fn toml_type_name(v: &::toml::Value) -> &'static str {
    match v {
        ::toml::Value::String(_) => "string",
        ::toml::Value::Integer(_) => "integer",
        ::toml::Value::Float(_) => "float",
        ::toml::Value::Boolean(_) => "boolean",
        ::toml::Value::Datetime(_) => "datetime",
        ::toml::Value::Array(_) => "array",
        ::toml::Value::Table(_) => "table",
    }
}

/// Convert a silt `Value` into a `toml::Value`. Fails for values that TOML
/// cannot represent (e.g. `Unit`, non-finite floats at the top level — TOML
/// 1.0 technically permits `nan`/`inf` but we emit the closest valid
/// representation and reject ambiguous ones like `Unit`).
///
/// `v` is `level` levels inside the value `caller` writes.
fn value_to_toml(caller: &str, v: &Value, level: usize) -> Result<::toml::Value, VmError> {
    ensure_encodable_depth(caller, level)?;
    let inner = |v: &Value| value_to_toml(caller, v, level + 1);
    // `Some(x)` is written as `x` is, and is no level of its own.
    let mut v = v;
    while let Value::Variant(variant) = v
        && variant.is(bv::SOME)
        && let [content] = variant.fields()
    {
        v = content;
    }
    Ok(match v {
        Value::Int(n) => ::toml::Value::Integer(*n),
        Value::Float(f) => ::toml::Value::Float(f.get()),
        Value::Bool(b) => ::toml::Value::Boolean(*b),
        Value::String(s) => ::toml::Value::String(s.to_string()),
        Value::List(xs) => {
            xs.writable()?;
            let items: Result<Vec<_>, _> = xs.iter().map(|x| inner(&x)).collect();
            ::toml::Value::Array(items?)
        }
        Value::Map(m) => {
            let mut table = ::toml::map::Map::new();
            for (k, v) in m.iter() {
                let key = match k {
                    Value::String(s) => s.to_string(),
                    other => other.to_string(),
                };
                table.insert(key, inner(v)?);
            }
            ::toml::Value::Table(table)
        }
        Value::Tuple(vs) => {
            let items: Result<Vec<_>, _> = vs.iter().map(inner).collect();
            ::toml::Value::Array(items?)
        }
        Value::Record(record)
            if matches!(record.type_id(), ty::DATE | ty::TIME | ty::DATE_TIME) =>
        {
            time_literal(v)
        }
        Value::Record(fields) => {
            let mut table = ::toml::map::Map::new();
            for (k, v) in fields.named() {
                // TOML has no null. An `Option::None` field is omitted from the
                // table entirely; on parse-back, a missing Option field decodes
                // back to None (see `toml_to_record`). Emitting a placeholder
                // (e.g. an empty string) would corrupt the round-trip:
                // `Option(String)` None would parse back as `Some("")`, and
                // `Option(Int)` None would fail with a type mismatch.
                if matches!(v, Value::Variant(variant) if variant.is(bv::NONE) && variant.fields().is_empty())
                {
                    continue;
                }
                table.insert(k.to_string(), inner(v)?);
            }
            ::toml::Value::Table(table)
        }
        Value::Variant(variant) if variant.is(bv::NONE) && variant.fields().is_empty() => {
            // TOML has no null and no representation for a freestanding None.
            // Record fields holding None are omitted in the Record arm above;
            // a None reaching here is a bare top-level None, which has no valid
            // TOML rendering. Emit an empty string as a deterministic
            // placeholder so serialization does not fail outright.
            ::toml::Value::String(String::new())
        }
        Value::Variant(variant) => {
            let mut table = ::toml::map::Map::new();
            table.insert(
                "variant".into(),
                ::toml::Value::String(variant.name().into()),
            );
            if !variant.fields().is_empty() {
                let items: Result<Vec<_>, _> = variant.fields().iter().map(inner).collect();
                table.insert("fields".into(), ::toml::Value::Array(items?));
            }
            ::toml::Value::Table(table)
        }
        Value::Unit => {
            return Err(VmError::new(format!(
                "{caller}: TOML cannot represent Unit"
            )));
        }
        _ => return Err(unsupported_kind(caller, v)),
    })
}

/// `v`, a date, a time of day or a datetime, as TOML writes it: a
/// native literal (`2026-10-19`, `12:30:00.25`, `2026-10-19T12:30:00`),
/// or, for one that no literal holds (a year outside 0000-9999, fields
/// that are no day or time of the calendar), a string of the text silt
/// shows of it. `toml.parse` reads a literal and the text alike.
fn time_literal(v: &Value) -> ::toml::Value {
    use ::toml::value::{Date, Datetime, Time};
    let int = |v: &Value, name: &str| match v {
        Value::Record(record) => match record.get(name) {
            Some(Value::Int(n)) => Some(*n),
            _ => None,
        },
        _ => None,
    };
    let date = |v: &Value| {
        let (year, month, day) = (int(v, "year")?, int(v, "month")?, int(v, "day")?);
        let year = u16::try_from(year).ok().filter(|year| *year <= 9999)?;
        let (month, day) = (u8::try_from(month).ok()?, u8::try_from(day).ok()?);
        NaiveDate::from_ymd_opt(year.into(), month.into(), day.into())?;
        Some(Date { year, month, day })
    };
    let time = |v: &Value| {
        let hour = u8::try_from(int(v, "hour")?).ok()?;
        let minute = u8::try_from(int(v, "minute")?).ok()?;
        let second = u8::try_from(int(v, "second")?).ok()?;
        // (Below a second: chrono takes one more for a leap second.)
        let nanosecond = u32::try_from(int(v, "ns")?)
            .ok()
            .filter(|ns| *ns < 1_000_000_000)?;
        NaiveTime::from_hms_nano_opt(hour.into(), minute.into(), second.into(), nanosecond)?;
        Some(Time {
            hour,
            minute,
            second,
            nanosecond,
        })
    };
    let field = |name: &str| match v {
        Value::Record(record) => record.get(name),
        _ => None,
    };
    let literal = match v {
        Value::Record(record) if record.type_id() == ty::DATE => date(v).map(|date| Datetime {
            date: Some(date),
            time: None,
            offset: None,
        }),
        Value::Record(record) if record.type_id() == ty::TIME => time(v).map(|time| Datetime {
            date: None,
            time: Some(time),
            offset: None,
        }),
        _ => (|| {
            Some(Datetime {
                date: Some(date(field("date")?)?),
                time: Some(time(field("time")?)?),
                offset: None,
            })
        })(),
    };
    literal.map_or_else(
        || ::toml::Value::String(v.to_string()),
        ::toml::Value::Datetime,
    )
}

/// Render a value at top level. TOML's top-level must be a table — a bare
/// scalar or array at top level is not a valid document. We therefore only
/// accept `Record`, `Map`, or `Table`-shaped values here; everything else
/// returns a descriptive `Err`.
fn value_to_top_level_toml(caller: &str, v: &Value) -> Result<::toml::Value, VmError> {
    match value_to_toml(caller, v, 0)? {
        t @ ::toml::Value::Table(_) => Ok(t),
        other => Err(VmError::new(format!(
            "{caller}: top-level value must be a table/record, got {}",
            toml_type_name(&other)
        ))),
    }
}

// ── Conversion: toml::Value → silt typed Value ──────────────────────

/// Inner-decoder error type. Each variant carries an already-built
/// `TomlError` variant value (unwrapped — the outer caller wraps in
/// `Err(...)`). Keeps recursive decoding cheap while letting us
/// distinguish clean silt-visible failures from VM-internal bugs.
enum TomlDecodeErr {
    Variant(Value),
    /// A value was present for a type no decoder exists for (the type as
    /// written). `toml_to_record` turns it into a `Other` that names
    /// the record field whose type this is.
    Unsupported(String),
    Vm(VmError),
}

impl From<VmError> for TomlDecodeErr {
    fn from(e: VmError) -> Self {
        TomlDecodeErr::Vm(e)
    }
}

fn decode_err_to_silt(e: TomlDecodeErr) -> Value {
    match e {
        TomlDecodeErr::Variant(v) => toml_err_wrap(v),
        // Only record fields carry unsupported types, and
        // `toml_to_record` names the field; this arm is the fallback.
        TomlDecodeErr::Unsupported(declared) => {
            toml_unknown_err(format!("a value of type {declared} cannot be decoded"))
        }
        TomlDecodeErr::Vm(err) => toml_unknown_err(err.message),
    }
}

fn toml_to_record(
    rt: &Runtime,
    ty: &Arc<TypeInfo>,
    fields: &[(String, FieldType)],
    tv: &::toml::Value,
) -> Result<Value, VmError> {
    let ::toml::Value::Table(table) = tv else {
        return Ok(toml_type_mismatch_err("table", toml_type_name(tv)));
    };
    // (In the order the type declares them: the record's own.)
    let mut record_fields = Vec::with_capacity(fields.len());
    for (field_name, field_type) in fields {
        let unsupported = without_bytes(rt, field_type);
        let field_type = unsupported.as_ref().unwrap_or(field_type);
        match table.get(field_name) {
            Some(val) => match toml_to_typed_value(rt, val, field_type) {
                Ok(v) => record_fields.push(v),
                Err(TomlDecodeErr::Unsupported(declared)) => {
                    return Ok(toml_unknown_err(unsupported_field_type_message(
                        &ty.name, field_name, &declared,
                    )));
                }
                Err(e) => return Ok(decode_err_to_silt(e)),
            },
            None => match field_type {
                FieldType::Option(_) => record_fields.push(Value::variant(bv::NONE, Vec::new())),
                _ => {
                    return Ok(toml_missing_field_err(field_name));
                }
            },
        }
    }
    Ok(Value::variant(
        bv::OK,
        vec![Value::record(ty.clone(), record_fields)],
    ))
}

fn toml_to_record_list(
    rt: &Runtime,
    ty: &Arc<TypeInfo>,
    fields: &[(String, FieldType)],
    tv: &::toml::Value,
) -> Result<Value, VmError> {
    let ::toml::Value::Array(arr) = tv else {
        return Ok(toml_type_mismatch_err("array", toml_type_name(tv)));
    };
    let mut records = Vec::new();
    for item in arr.iter() {
        let result = toml_to_record(rt, ty, fields, item)?;
        match result {
            Value::Variant(variant) if variant.is(bv::OK) && variant.fields().len() == 1 => {
                records.push(variant.fields()[0].clone());
            }
            ref err @ Value::Variant(ref variant) if variant.is(bv::ERR) => {
                // Already a typed Err(TomlError); forward unchanged.
                return Ok(err.clone());
            }
            _ => {
                return Ok(toml_unknown_err(format!(
                    "toml.parse_list({}): unexpected result",
                    ty.name
                )));
            }
        }
    }
    Ok(Value::variant(bv::OK, vec![Value::list(records)]))
}

fn toml_to_map(rt: &Runtime, value_type: Type, tv: &::toml::Value) -> Result<Value, VmError> {
    let ::toml::Value::Table(table) = tv else {
        return Ok(toml_type_mismatch_err("table", toml_type_name(tv)));
    };
    let field_type = match value_type {
        Type::Primitive(field_type) => field_type,
        Type::Named(ty) => {
            if decodable_record("toml.parse_map", ty).is_err() {
                return Ok(toml_unknown_err(format!(
                    "toml.parse_map: unknown value type '{}'",
                    ty.name
                )));
            }
            FieldType::Record(ty.id)
        }
    };
    let mut map = Vec::with_capacity(table.len());
    for (key, val) in table.iter() {
        match toml_to_typed_value(rt, val, &field_type) {
            Ok(v) => map.push((Value::String(key.clone().into()), v)),
            Err(e) => return Ok(decode_err_to_silt(e)),
        }
    }
    Ok(Value::variant(bv::OK, vec![Value::map(map)]))
}

fn toml_to_typed_value(
    rt: &Runtime,
    tv: &::toml::Value,
    expected: &FieldType,
) -> Result<Value, TomlDecodeErr> {
    let mismatch = |expected: &str, actual: &str| -> TomlDecodeErr {
        TomlDecodeErr::Variant(Value::variant(
            bv::TOML_TYPE_MISMATCH,
            vec![Value::String(expected.into()), Value::String(actual.into())],
        ))
    };
    let unknown = |msg: String| -> TomlDecodeErr {
        TomlDecodeErr::Variant(Value::variant(
            bv::TOML_OTHER,
            vec![Value::String(msg.into())],
        ))
    };
    match expected {
        FieldType::String => match tv {
            // (A datetime literal is no string, as a float is no `Int`.)
            ::toml::Value::String(s) => Ok(Value::String(s.clone().into())),
            _ => Err(mismatch("String", toml_type_name(tv))),
        },
        // (TOML says which kind a number is: a float is no `Int`,
        // whatever its value. JSON has one kind of number, and its
        // decoder takes a whole one.)
        FieldType::Int => match tv {
            ::toml::Value::Integer(n) => Ok(Value::Int(*n)),
            _ => Err(mismatch("Int", toml_type_name(tv))),
        },
        FieldType::Float => match tv {
            // A `Float` is finite, so TOML's `nan` and `inf` do not decode.
            ::toml::Value::Float(f) => match Float::new(*f) {
                Some(f) => Ok(Value::Float(f)),
                None => Err(mismatch("Float", "a non-finite float")),
            },
            // TOML integers coerce to Float the way JSON numbers do.
            ::toml::Value::Integer(n) => Ok(Value::Float(Float::from(*n))),
            _ => Err(mismatch("Float", toml_type_name(tv))),
        },
        FieldType::Bool => match tv {
            ::toml::Value::Boolean(b) => Ok(Value::Bool(*b)),
            _ => Err(mismatch("Bool", toml_type_name(tv))),
        },
        FieldType::List(inner) => match tv {
            ::toml::Value::Array(arr) => {
                let mut values = Vec::new();
                for item in arr.iter() {
                    values.push(toml_to_typed_value(rt, item, inner)?);
                }
                Ok(Value::list(values))
            }
            _ => Err(mismatch("List", toml_type_name(tv))),
        },
        FieldType::Option(inner) => {
            // TOML has no null. Non-present keys are handled by toml_to_record
            // via Option default; if the key *is* present, delegate to inner.
            let val = toml_to_typed_value(rt, tv, inner)?;
            Ok(Value::variant(bv::SOME, vec![val]))
        }
        FieldType::Map(inner) => match tv {
            ::toml::Value::Table(table) => {
                let mut map = Vec::with_capacity(table.len());
                for (key, item) in table.iter() {
                    let val = toml_to_typed_value(rt, item, inner)?;
                    map.push((Value::String(key.clone().into()), val));
                }
                Ok(Value::map(map))
            }
            _ => Err(mismatch("Map", toml_type_name(tv))),
        },
        FieldType::Tuple(elems) => match tv {
            ::toml::Value::Array(arr) if arr.len() == elems.len() => {
                let mut values = Vec::with_capacity(elems.len());
                for (item, elem) in arr.iter().zip(elems) {
                    values.push(toml_to_typed_value(rt, item, elem)?);
                }
                Ok(Value::tuple(values))
            }
            ::toml::Value::Array(arr) => Err(unknown(format!(
                "expected an array of {} elements for a tuple, got {}",
                elems.len(),
                arr.len()
            ))),
            _ => Err(mismatch("Tuple", toml_type_name(tv))),
        },
        FieldType::Unsupported(declared) => Err(TomlDecodeErr::Unsupported(declared.clone())),
        FieldType::JsonValue => Err(TomlDecodeErr::Unsupported("json.Value".into())),
        FieldType::Bytes => Err(TomlDecodeErr::Unsupported("Bytes".into())),
        FieldType::Date => match tv {
            ::toml::Value::Datetime(dt) => {
                // Preferred path: TOML native date literal.
                let s = dt.to_string();
                parse_date(&s)
                    .map(make_date)
                    .map_err(|e| unknown(format!("invalid date '{s}': {e}")))
            }
            ::toml::Value::String(s) => parse_date(s)
                .map(make_date)
                .map_err(|e| unknown(invalid_date_message(s, e))),
            _ => Err(mismatch("date", toml_type_name(tv))),
        },
        FieldType::Time => match tv {
            ::toml::Value::Datetime(dt) => {
                let s = dt.to_string();
                parse_time(&s)
                    .map(make_time)
                    .map_err(|e| unknown(format!("invalid time '{s}': {e}")))
            }
            ::toml::Value::String(s) => parse_time(s)
                .map(make_time)
                .map_err(|e| unknown(invalid_time_message(s, &e))),
            _ => Err(mismatch("time", toml_type_name(tv))),
        },
        FieldType::DateTime => match tv {
            // (A TOML datetime literal as its text.)
            ::toml::Value::Datetime(dt) => parse_datetime(&dt.to_string())
                .map(make_datetime)
                .map_err(unknown),
            ::toml::Value::String(s) => parse_datetime(s).map(make_datetime).map_err(unknown),
            _ => Err(mismatch("datetime", toml_type_name(tv))),
        },
        FieldType::Record(id) => {
            let ty = field_record_type(rt, "toml.parse", *id)?;
            let sub_fields = decodable_record("toml.parse", &ty)?;
            let result = toml_to_record(rt, &ty, &sub_fields, tv)?;
            match result {
                Value::Variant(variant) if variant.is(bv::OK) && variant.fields().len() == 1 => {
                    Ok(variant.fields()[0].clone())
                }
                Value::Variant(variant) if variant.is(bv::ERR) && variant.fields().len() == 1 => {
                    // Already a typed TomlError variant; forward via
                    // our internal decoder-error channel so the caller
                    // surfaces it unchanged.
                    Err(TomlDecodeErr::Variant(variant.fields()[0].clone()))
                }
                _ => Err(unknown(format!("failed to parse {}", ty.name))),
            }
        }
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

/// `value` as TOML text, for `toml.<name>`: `Ok(text)`, or the
/// `TomlError` of a value that TOML has no form for.
fn written(
    name: &str,
    value: &Value,
    write: fn(&::toml::Value) -> Result<String, ::toml::ser::Error>,
) -> Value {
    let caller = format!("toml.{name}");
    let tv = match value_to_top_level_toml(&caller, value) {
        Ok(tv) => tv,
        Err(e) => return toml_unknown_err(e.message),
    };
    match write(&tv) {
        Ok(s) => Value::variant(bv::OK, vec![Value::String(s.into())]),
        Err(e) => toml_unknown_err(format!("{caller}: {e}")),
    }
}

builtins! {
    fn parse(vm, s: &str, ty: Type) -> Result<Value, VmError> {
        let ty = record_type("parse", ty)?;
        let fields = decodable_record("toml.parse", ty)?;
        match ::toml::from_str::<::toml::Value>(s) {
            Ok(tv) => toml_to_record(&vm.runtime, ty, &fields, &tv),
            Err(e) => Ok(toml_de_result_err(&e)),
        }
    }

    fn parse_list(vm, s: &str, ty: Type) -> Result<Value, VmError> {
        let ty = record_type("parse_list", ty)?;
        let fields = decodable_record("toml.parse_list", ty)?;
        let tv = match ::toml::from_str::<::toml::Value>(s) {
            Ok(tv) => tv,
            Err(e) => return Ok(toml_de_result_err(&e)),
        };
        // TOML's top-level is always a table. For `parse_list` we
        // expect that table to contain exactly one array-of-tables key,
        // whose values are the list elements. This matches how
        // `[[items]]` naturally renders.
        let ::toml::Value::Table(table) = &tv else {
            return Ok(toml_type_mismatch_err("table", toml_type_name(&tv)));
        };
        let mut values = table.values();
        let (Some(only), None) = (values.next(), values.next()) else {
            return Ok(toml_unknown_err(format!(
                "toml.parse_list({}): expected a document with exactly one top-level array-of-tables key, found {} keys",
                ty.name,
                table.len()
            )));
        };
        toml_to_record_list(&vm.runtime, ty, &fields, only)
    }

    fn parse_map(vm, s: &str, value_type: Type) -> Result<Value, VmError> {
        match ::toml::from_str::<::toml::Value>(s) {
            Ok(tv) => toml_to_map(&vm.runtime, value_type, &tv),
            Err(e) => Ok(toml_de_result_err(&e)),
        }
    }

    fn stringify(value: &Value) -> Value {
        written("stringify", value, ::toml::to_string)
    }

    fn pretty(value: &Value) -> Value {
        written("pretty", value, ::toml::to_string_pretty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A field whose type holds `Bytes` has no decoder, whatever the
    /// document holds there, an empty array too.
    #[test]
    fn a_field_that_holds_bytes_is_never_decoded() {
        use crate::defs::{DefId, TypeId};
        let vm = crate::vm::Vm::new(crate::vm::HostIo::process());
        let fields = vec![("b".to_string(), FieldType::List(Box::new(FieldType::Bytes)))];
        let ty = TypeInfo::new_record(TypeId(DefId(9000)), "B", fields.clone());
        for text in ["b = []", "b = [1]"] {
            let doc: ::toml::Value = ::toml::from_str(text).expect("a document");
            let decoded = toml_to_record(&vm.runtime, &ty, &fields, &doc).expect("a value");
            assert_eq!(
                decoded.to_string(),
                "Err(cannot decode field `b` of `B`: a value of type List(Bytes) cannot be decoded)",
                "{text}"
            );
        }
    }
    use crate::builtins::json::MAX_ENCODE_DEPTH;

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
                let text = written("stringify", &nested(MAX_ENCODE_DEPTH), ::toml::to_string);
                let pretty = written(
                    "pretty",
                    &nested(MAX_ENCODE_DEPTH),
                    ::toml::to_string_pretty,
                );
                let refused = written(
                    "pretty",
                    &nested(MAX_ENCODE_DEPTH + 1),
                    ::toml::to_string_pretty,
                );
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
