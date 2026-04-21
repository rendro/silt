//! `toml.*` builtin functions: parse TOML documents into typed silt records
//! and serialize silt values into TOML text.
//!
//! The API mirrors the `json` module in `src/builtins/data.rs`, with each
//! fallible call returning a typed `TomlError` (Phase 1 of the stdlib error
//! redesign — see `docs/proposals/stdlib-errors.md`):
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
//!   to TOML; the `toml` crate's default output is already multi-line and
//!   human-friendly, so `pretty` is an alias for ergonomic symmetry with
//!   `json.pretty`.
//!
//! ## TOML-specific types
//!
//! TOML's native date/time variants (Offset Date-Time, Local Date-Time,
//! Local Date, Local Time) are translated to strings using the same ISO 8601
//! shapes that `json.parse` already accepts for `Date`, `Time`, and `DateTime`
//! fields — so a `Date` field in the record happily receives a bare TOML
//! `1979-05-27`, a `DateTime` field receives `1979-05-27T07:32:00Z`, and a
//! `Time` field receives `07:32:00`. This reuses the json code path for
//! date parsing, keeping the two modules' behavior aligned.
//!
//! ## Semantics
//!
//! - Parse errors pass through the `toml` crate's error text (`format!("{e}")`).
//! - Missing `Option` fields default to `None`.
//! - Missing required fields return `Err(...)`.
//! - Type mismatches return an `Err(...)` naming the field.
//! - Integer overflow: TOML integers are `i64` per spec, which matches silt's
//!   `Int` exactly; no demotion risk.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};

use crate::value::Value;
use crate::vm::{Vm, VmError};

use super::data::{FieldType, load_record_fields, make_date, make_datetime, make_time};

// ── TomlError helpers ────────────────────────────────────────────────
//
// Phase 1 of the stdlib error redesign: every fallible toml.* call now
// surfaces a typed `TomlError` variant wrapped in `Err(...)` instead of
// a bare `Err(String)`. Mirrors `json_*_err` in `src/builtins/data.rs`.
//
// `TomlError` variants:
//   TomlSyntax(message, byte_offset)
//   TomlTypeMismatch(expected, actual)
//   TomlMissingField(name)
//   TomlUnknown(message)

fn toml_err_wrap(inner: Value) -> Value {
    Value::Variant("Err".into(), vec![inner])
}

/// Classify a `toml::de::Error` into one of the `TomlError` variants.
/// The `toml` crate exposes a `span()` method that yields a byte range
/// into the source; we take the start as the offset. For errors that
/// don't have a span (extremely rare in practice) we fall back to 0.
pub(crate) fn toml_de_error_to_variant(err: &::toml::de::Error) -> Value {
    let offset = err.span().map(|s| s.start as i64).unwrap_or(0);
    Value::Variant(
        "TomlSyntax".into(),
        vec![Value::String(err.message().to_string()), Value::Int(offset)],
    )
}

/// Build a full `Err(TomlError)` from a `toml::de::Error`.
pub(crate) fn toml_de_result_err(err: &::toml::de::Error) -> Value {
    toml_err_wrap(toml_de_error_to_variant(err))
}

/// Build `Err(TomlTypeMismatch(expected, actual))`.
pub(crate) fn toml_type_mismatch_err(expected: &str, actual: &str) -> Value {
    toml_err_wrap(Value::Variant(
        "TomlTypeMismatch".into(),
        vec![Value::String(expected.into()), Value::String(actual.into())],
    ))
}

/// Build `Err(TomlMissingField(name))`.
pub(crate) fn toml_missing_field_err(name: &str) -> Value {
    toml_err_wrap(Value::Variant(
        "TomlMissingField".into(),
        vec![Value::String(name.into())],
    ))
}

/// Build `Err(TomlUnknown(msg))` for ad-hoc failures (unknown type,
/// serialization errors, document-shape violations, etc.).
pub(crate) fn toml_unknown_err<S: Into<String>>(msg: S) -> Value {
    toml_err_wrap(Value::Variant(
        "TomlUnknown".into(),
        vec![Value::String(msg.into())],
    ))
}

/// Dispatch the builtin `trait Error for TomlError` method table.
/// Routed through `dispatch_builtin`'s "TomlError" module arm, exactly
/// like `call_io_error_trait`.
pub fn call_toml_error_trait(name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "message" => {
            if args.len() != 1 {
                return Err(VmError::new(format!(
                    "TomlError.message takes 1 argument (self), got {}",
                    args.len()
                )));
            }
            let msg = match &args[0] {
                Value::Variant(tag, fields) => match (tag.as_str(), fields.as_slice()) {
                    ("TomlSyntax", [Value::String(m), Value::Int(offset)]) => {
                        format!("toml syntax error at byte {offset}: {m}")
                    }
                    ("TomlTypeMismatch", [Value::String(exp), Value::String(act)]) => {
                        format!("toml type mismatch: expected {exp}, got {act}")
                    }
                    ("TomlMissingField", [Value::String(n)]) => {
                        format!("toml missing field: {n}")
                    }
                    ("TomlUnknown", [Value::String(m)]) => m.clone(),
                    _ => format!("TomlError: unrecognized variant shape `{tag}`"),
                },
                other => {
                    return Err(VmError::new(format!(
                        "TomlError.message: expected TomlError variant, got {other}"
                    )));
                }
            };
            Ok(Value::String(msg))
        }
        _ => Err(VmError::new(format!(
            "unknown TomlError trait method: {name}"
        ))),
    }
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
fn value_to_toml(v: &Value) -> Result<::toml::Value, VmError> {
    Ok(match v {
        Value::Int(n) => ::toml::Value::Integer(*n),
        Value::Float(f) => ::toml::Value::Float(*f),
        Value::ExtFloat(f) => ::toml::Value::Float(*f),
        Value::Bool(b) => ::toml::Value::Boolean(*b),
        Value::String(s) => ::toml::Value::String(s.clone()),
        Value::List(xs) => {
            let items: Result<Vec<_>, _> = xs.iter().map(value_to_toml).collect();
            ::toml::Value::Array(items?)
        }
        Value::Range(lo, hi) => {
            let mut items = Vec::new();
            let mut i = *lo;
            while i <= *hi {
                items.push(::toml::Value::Integer(i));
                // Guard against overflow on inclusive range termination.
                if i == i64::MAX {
                    break;
                }
                i += 1;
            }
            ::toml::Value::Array(items)
        }
        Value::Map(m) => {
            let mut table = ::toml::map::Map::new();
            for (k, v) in m.iter() {
                let key = match k {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                table.insert(key, value_to_toml(v)?);
            }
            ::toml::Value::Table(table)
        }
        Value::Tuple(vs) => {
            let items: Result<Vec<_>, _> = vs.iter().map(value_to_toml).collect();
            ::toml::Value::Array(items?)
        }
        Value::Record(name, fields) => {
            // Special handling for built-in Date / Time / DateTime records:
            // emit them as TOML native datetime literals so round-trips
            // through `toml.parse` preserve type.
            match name.as_str() {
                "Date" => {
                    if let (Some(Value::Int(y)), Some(Value::Int(m)), Some(Value::Int(d))) =
                        (fields.get("year"), fields.get("month"), fields.get("day"))
                    {
                        let iso = format!("{y:04}-{m:02}-{d:02}");
                        // Parse the string through toml's own Datetime type
                        // so the output renders as a native TOML date.
                        if let Ok(dt) = iso.parse::<::toml::value::Datetime>() {
                            return Ok(::toml::Value::Datetime(dt));
                        }
                        return Ok(::toml::Value::String(iso));
                    }
                    // Fall through to generic record handling if the shape
                    // doesn't match the expected field set.
                }
                "Time" => {
                    if let (Some(Value::Int(h)), Some(Value::Int(m)), Some(Value::Int(s))) = (
                        fields.get("hour"),
                        fields.get("minute"),
                        fields.get("second"),
                    ) {
                        let iso = format!("{h:02}:{m:02}:{s:02}");
                        if let Ok(dt) = iso.parse::<::toml::value::Datetime>() {
                            return Ok(::toml::Value::Datetime(dt));
                        }
                        return Ok(::toml::Value::String(iso));
                    }
                }
                "DateTime" => {
                    if let (
                        Some(Value::Record(_, date_fields)),
                        Some(Value::Record(_, time_fields)),
                    ) = (fields.get("date"), fields.get("time"))
                        && let (
                            Some(Value::Int(y)),
                            Some(Value::Int(mo)),
                            Some(Value::Int(d)),
                            Some(Value::Int(h)),
                            Some(Value::Int(mi)),
                            Some(Value::Int(se)),
                        ) = (
                            date_fields.get("year"),
                            date_fields.get("month"),
                            date_fields.get("day"),
                            time_fields.get("hour"),
                            time_fields.get("minute"),
                            time_fields.get("second"),
                        )
                    {
                        let iso = format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{se:02}");
                        if let Ok(dt) = iso.parse::<::toml::value::Datetime>() {
                            return Ok(::toml::Value::Datetime(dt));
                        }
                        return Ok(::toml::Value::String(iso));
                    }
                }
                _ => {}
            }
            let mut table = ::toml::map::Map::new();
            for (k, v) in fields.iter() {
                table.insert(k.clone(), value_to_toml(v)?);
            }
            ::toml::Value::Table(table)
        }
        Value::Variant(name, fields) if name == "None" && fields.is_empty() => {
            // TOML has no null. Caller should omit Option::None fields; here
            // we emit an empty string as a deterministic placeholder so a
            // freestanding None at top level still serializes.
            ::toml::Value::String(String::new())
        }
        Value::Variant(name, fields) if name == "Some" && fields.len() == 1 => {
            value_to_toml(&fields[0])?
        }
        Value::Variant(name, fields) => {
            let mut table = ::toml::map::Map::new();
            table.insert("variant".into(), ::toml::Value::String(name.clone()));
            if !fields.is_empty() {
                let items: Result<Vec<_>, _> = fields.iter().map(value_to_toml).collect();
                table.insert("fields".into(), ::toml::Value::Array(items?));
            }
            ::toml::Value::Table(table)
        }
        Value::Unit => {
            return Err(VmError::new(
                "toml.stringify: TOML cannot represent Unit".into(),
            ));
        }
        Value::VariantConstructor(name, _) => ::toml::Value::String(name.clone()),
        _ => {
            return Err(VmError::new(format!(
                "toml.stringify: unsupported value kind {v:?}"
            )));
        }
    })
}

/// Render a value at top level. TOML's top-level must be a table — a bare
/// scalar or array at top level is not a valid document. We therefore only
/// accept `Record`, `Map`, or `Table`-shaped values here; everything else
/// returns a descriptive `Err`.
fn value_to_top_level_toml(v: &Value) -> Result<::toml::Value, VmError> {
    match value_to_toml(v)? {
        t @ ::toml::Value::Table(_) => Ok(t),
        other => Err(VmError::new(format!(
            "toml.stringify: top-level value must be a table/record, got {}",
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
        TomlDecodeErr::Vm(err) => toml_unknown_err(err.message),
    }
}

fn toml_to_record(
    vm: &mut Vm,
    type_name: &str,
    fields: &[(String, FieldType)],
    tv: &::toml::Value,
) -> Result<Value, VmError> {
    let ::toml::Value::Table(table) = tv else {
        return Ok(toml_type_mismatch_err("table", toml_type_name(tv)));
    };
    let mut record_fields: BTreeMap<String, Value> = BTreeMap::new();
    for (field_name, field_type) in fields {
        match table.get(field_name) {
            Some(val) => match toml_to_typed_value(vm, val, field_type) {
                Ok(v) => {
                    record_fields.insert(field_name.clone(), v);
                }
                Err(e) => return Ok(decode_err_to_silt(e)),
            },
            None => match field_type {
                FieldType::Option(_) => {
                    record_fields.insert(
                        field_name.clone(),
                        Value::Variant("None".into(), Vec::new()),
                    );
                }
                _ => {
                    return Ok(toml_missing_field_err(field_name));
                }
            },
        }
    }
    let _ = type_name;
    Ok(Value::Variant(
        "Ok".into(),
        vec![Value::Record(
            type_name.to_string(),
            Arc::new(record_fields),
        )],
    ))
}

fn toml_to_record_list(
    vm: &mut Vm,
    type_name: &str,
    fields: &[(String, FieldType)],
    tv: &::toml::Value,
) -> Result<Value, VmError> {
    let ::toml::Value::Array(arr) = tv else {
        return Ok(toml_type_mismatch_err("array", toml_type_name(tv)));
    };
    let mut records = Vec::new();
    for item in arr.iter() {
        let result = toml_to_record(vm, type_name, fields, item)?;
        match result {
            Value::Variant(name, inner) if name == "Ok" && inner.len() == 1 => {
                records.push(inner.into_iter().next().expect("guard guarantees len==1"));
            }
            ref err @ Value::Variant(ref name, _) if name == "Err" => {
                // Already a typed Err(TomlError); forward unchanged.
                return Ok(err.clone());
            }
            _ => {
                return Ok(toml_unknown_err(format!(
                    "toml.parse_list({type_name}): unexpected result"
                )));
            }
        }
    }
    Ok(Value::Variant(
        "Ok".into(),
        vec![Value::List(Arc::new(records))],
    ))
}

fn toml_to_map(vm: &mut Vm, value_type: &str, tv: &::toml::Value) -> Result<Value, VmError> {
    let ::toml::Value::Table(table) = tv else {
        return Ok(toml_type_mismatch_err("table", toml_type_name(tv)));
    };
    let field_type = match value_type {
        "String" => FieldType::String,
        "Int" => FieldType::Int,
        "Float" => FieldType::Float,
        "Bool" => FieldType::Bool,
        record_name => {
            let meta_key = format!("__record_fields__{record_name}");
            if !vm.globals.contains_key(&meta_key) {
                return Ok(toml_unknown_err(format!(
                    "toml.parse_map: unknown value type '{record_name}'"
                )));
            }
            FieldType::Record(record_name.to_string())
        }
    };
    let mut map = BTreeMap::new();
    for (_key, val) in table.iter() {
        match toml_to_typed_value(vm, val, &field_type) {
            Ok(v) => {
                map.insert(Value::String(_key.clone()), v);
            }
            Err(e) => return Ok(decode_err_to_silt(e)),
        }
    }
    Ok(Value::Variant("Ok".into(), vec![Value::Map(Arc::new(map))]))
}

fn toml_to_typed_value(
    vm: &mut Vm,
    tv: &::toml::Value,
    expected: &FieldType,
) -> Result<Value, TomlDecodeErr> {
    let mismatch = |expected: &str, actual: &str| -> TomlDecodeErr {
        TomlDecodeErr::Variant(Value::Variant(
            "TomlTypeMismatch".into(),
            vec![Value::String(expected.into()), Value::String(actual.into())],
        ))
    };
    let unknown = |msg: String| -> TomlDecodeErr {
        TomlDecodeErr::Variant(Value::Variant(
            "TomlUnknown".into(),
            vec![Value::String(msg)],
        ))
    };
    match expected {
        FieldType::String => match tv {
            ::toml::Value::String(s) => Ok(Value::String(s.clone())),
            // TOML datetimes have a canonical string form — accept them as
            // strings so users targeting `String` still receive something.
            ::toml::Value::Datetime(dt) => Ok(Value::String(dt.to_string())),
            _ => Err(mismatch("String", toml_type_name(tv))),
        },
        FieldType::Int => match tv {
            ::toml::Value::Integer(n) => Ok(Value::Int(*n)),
            _ => Err(mismatch("Int", toml_type_name(tv))),
        },
        FieldType::Float => match tv {
            ::toml::Value::Float(f) => Ok(Value::Float(*f)),
            // TOML integers coerce to Float the way JSON numbers do.
            ::toml::Value::Integer(n) => Ok(Value::Float(*n as f64)),
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
                    values.push(toml_to_typed_value(vm, item, inner)?);
                }
                Ok(Value::List(Arc::new(values)))
            }
            _ => Err(mismatch("List", toml_type_name(tv))),
        },
        FieldType::Option(inner) => {
            // TOML has no null. Non-present keys are handled by toml_to_record
            // via Option default; if the key *is* present, delegate to inner.
            let val = toml_to_typed_value(vm, tv, inner)?;
            Ok(Value::Variant("Some".into(), vec![val]))
        }
        FieldType::Date => match tv {
            ::toml::Value::Datetime(dt) => {
                // Preferred path: TOML native date literal.
                let s = dt.to_string();
                NaiveDate::parse_from_str(&s, "%Y-%m-%d")
                    .map(make_date)
                    .map_err(|e| unknown(format!("invalid date '{s}': {e}")))
            }
            ::toml::Value::String(s) => NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .map(make_date)
                .map_err(|e| unknown(format!("invalid date '{s}' (expected YYYY-MM-DD): {e}"))),
            _ => Err(mismatch("date", toml_type_name(tv))),
        },
        FieldType::Time => match tv {
            ::toml::Value::Datetime(dt) => {
                let s = dt.to_string();
                NaiveTime::parse_from_str(&s, "%H:%M:%S")
                    .or_else(|_| NaiveTime::parse_from_str(&s, "%H:%M"))
                    .or_else(|_| NaiveTime::parse_from_str(&s, "%H:%M:%S%.f"))
                    .map(make_time)
                    .map_err(|e| unknown(format!("invalid time '{s}': {e}")))
            }
            ::toml::Value::String(s) => NaiveTime::parse_from_str(s, "%H:%M:%S")
                .or_else(|_| NaiveTime::parse_from_str(s, "%H:%M"))
                .map(make_time)
                .map_err(|e| unknown(format!("invalid time '{s}' (expected HH:MM:SS): {e}"))),
            _ => Err(mismatch("time", toml_type_name(tv))),
        },
        FieldType::DateTime => match tv {
            ::toml::Value::Datetime(dt) => {
                let s = dt.to_string();
                if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&s) {
                    Ok(make_datetime(ts.naive_utc()))
                } else if let Ok(ts) = chrono::DateTime::parse_from_str(&s, "%Y-%m-%dT%H:%M:%S%z") {
                    Ok(make_datetime(ts.naive_utc()))
                } else {
                    NaiveDateTime::parse_from_str(&s, "%Y-%m-%dT%H:%M:%S")
                        .or_else(|_| NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S"))
                        .or_else(|_| NaiveDateTime::parse_from_str(&s, "%Y-%m-%dT%H:%M:%S%.f"))
                        .map(make_datetime)
                        .map_err(|_| unknown(format!("invalid datetime '{s}'")))
                }
            }
            ::toml::Value::String(s) => {
                if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(s) {
                    Ok(make_datetime(ts.naive_utc()))
                } else {
                    NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
                        .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S"))
                        .map(make_datetime)
                        .map_err(|_| unknown(format!("invalid datetime '{s}'")))
                }
            }
            _ => Err(mismatch("datetime", toml_type_name(tv))),
        },
        FieldType::Record(rec_name) => {
            let sub_fields = load_record_fields(vm, rec_name)?;
            let result = toml_to_record(vm, rec_name, &sub_fields, tv)?;
            match result {
                Value::Variant(name, inner) if name == "Ok" && inner.len() == 1 => {
                    Ok(inner.into_iter().next().expect("len==1"))
                }
                Value::Variant(name, inner) if name == "Err" && inner.len() == 1 => {
                    // Already a typed TomlError variant; forward via
                    // our internal decoder-error channel so the caller
                    // surfaces it unchanged.
                    Err(TomlDecodeErr::Variant(
                        inner.into_iter().next().expect("len==1"),
                    ))
                }
                _ => Err(unknown(format!("failed to parse {rec_name}"))),
            }
        }
    }
}

// ── Dispatch ────────────────────────────────────────────────────────

/// Dispatch `toml.<name>(args)`.
pub fn call(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "parse" => {
            if args.len() != 2 {
                return Err(VmError::new(
                    "toml.parse takes 2 arguments: (String, type a)".into(),
                ));
            }
            let Value::String(s) = &args[0] else {
                return Err(VmError::new(
                    "toml.parse: first argument must be a string".into(),
                ));
            };
            let s = s.clone();
            let Value::TypeDescriptor(type_name) = &args[1] else {
                return Err(VmError::new(
                    "toml.parse: type argument must be a record type".into(),
                ));
            };
            let type_name = type_name.clone();
            let fields = load_record_fields(vm, &type_name)?;
            match ::toml::from_str::<::toml::Value>(&s) {
                Ok(tv) => toml_to_record(vm, &type_name, &fields, &tv),
                Err(e) => Ok(toml_de_result_err(&e)),
            }
        }
        "parse_list" => {
            if args.len() != 2 {
                return Err(VmError::new(
                    "toml.parse_list takes 2 arguments: (String, type a)".into(),
                ));
            }
            let Value::String(s) = &args[0] else {
                return Err(VmError::new(
                    "toml.parse_list: first argument must be a string".into(),
                ));
            };
            let s = s.clone();
            let Value::TypeDescriptor(type_name) = &args[1] else {
                return Err(VmError::new(
                    "toml.parse_list: type argument must be a record type".into(),
                ));
            };
            let type_name = type_name.clone();
            let fields = load_record_fields(vm, &type_name)?;
            match ::toml::from_str::<::toml::Value>(&s) {
                Ok(tv) => {
                    // TOML's top-level is always a table. For `parse_list`
                    // we expect that table to contain exactly one array-of-
                    // tables key, whose values are the list elements. This
                    // matches how `[[items]]` naturally renders.
                    let ::toml::Value::Table(table) = &tv else {
                        return Ok(toml_type_mismatch_err("table", toml_type_name(&tv)));
                    };
                    if table.len() != 1 {
                        return Ok(toml_unknown_err(format!(
                            "toml.parse_list({type_name}): expected a document with exactly one top-level array-of-tables key, found {} keys",
                            table.len()
                        )));
                    }
                    let (_k, v) = table.iter().next().expect("len==1 above");
                    toml_to_record_list(vm, &type_name, &fields, v)
                }
                Err(e) => Ok(toml_de_result_err(&e)),
            }
        }
        "parse_map" => {
            if args.len() != 2 {
                return Err(VmError::new(
                    "toml.parse_map takes 2 arguments: (String, type v)".into(),
                ));
            }
            let Value::String(s) = &args[0] else {
                return Err(VmError::new(
                    "toml.parse_map: first argument must be a string".into(),
                ));
            };
            let s = s.clone();
            let value_type = match &args[1] {
                Value::PrimitiveDescriptor(name) => name.clone(),
                Value::TypeDescriptor(name) => name.clone(),
                _ => return Err(VmError::new(
                    "toml.parse_map: type argument must be a type (Int, Float, String, Bool, or a record type)".into()
                )),
            };
            match ::toml::from_str::<::toml::Value>(&s) {
                Ok(tv) => toml_to_map(vm, &value_type, &tv),
                Err(e) => Ok(toml_de_result_err(&e)),
            }
        }
        "stringify" => {
            if args.len() != 1 {
                return Err(VmError::new("toml.stringify takes 1 argument".into()));
            }
            let tv = match value_to_top_level_toml(&args[0]) {
                Ok(t) => t,
                Err(e) => return Ok(toml_unknown_err(e.message)),
            };
            match ::toml::to_string(&tv) {
                Ok(s) => Ok(Value::Variant("Ok".into(), vec![Value::String(s)])),
                Err(e) => Ok(toml_unknown_err(format!("toml.stringify: {e}"))),
            }
        }
        "pretty" => {
            if args.len() != 1 {
                return Err(VmError::new("toml.pretty takes 1 argument".into()));
            }
            let tv = match value_to_top_level_toml(&args[0]) {
                Ok(t) => t,
                Err(e) => return Ok(toml_unknown_err(e.message)),
            };
            match ::toml::to_string_pretty(&tv) {
                Ok(s) => Ok(Value::Variant("Ok".into(), vec![Value::String(s)])),
                Err(e) => Ok(toml_unknown_err(format!("toml.pretty: {e}"))),
            }
        }
        _ => Err(VmError::new(format!("unknown toml function: {name}"))),
    }
}
