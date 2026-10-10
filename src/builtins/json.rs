//! The `json.*` builtin functions and the typed decoding `toml.*` shares.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};

use super::time::{make_date, make_datetime, make_time};
use super::typed::{Type, builtins};
use crate::defs::TypeId;
use crate::typeinfo::{FieldType, Shape, TypeInfo, bv};
use crate::value::{Value, checked_range_len};
use crate::vm::{Vm, VmError};

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
//   JsonSyntax(message, byte_offset)
//   JsonTypeMismatch(expected, actual)
//   JsonMissingField(name)
//   JsonUnknown(message)

fn json_err_wrap(inner: Value) -> Value {
    Value::variant(bv::ERR, vec![inner])
}

/// Classify a `serde_json::Error` into one of the `JsonError` variants.
/// Syntax/Eof errors get `JsonSyntax(msg, column)`. Data and Io errors
/// collapse to `JsonUnknown` because serde_json does not expose the
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
                vec![Value::String(err.to_string()), Value::Int(offset)],
            )
        }
        _ => Value::variant(bv::JSON_UNKNOWN, vec![Value::String(err.to_string())]),
    }
}

/// Build a full `Err(JsonError)` from a `serde_json::Error`.
pub(crate) fn json_result_err(err: &serde_json::Error) -> Value {
    json_err_wrap(json_error_to_variant(err))
}

/// Build `Err(JsonTypeMismatch(expected, actual))`. Used by the
/// hand-written record decoder to report a field whose value has the
/// wrong shape.
pub(crate) fn json_type_mismatch_err(expected: &str, actual: &str) -> Value {
    json_err_wrap(Value::variant(
        bv::JSON_TYPE_MISMATCH,
        vec![Value::String(expected.into()), Value::String(actual.into())],
    ))
}

/// Build `Err(JsonMissingField(name))`.
pub(crate) fn json_missing_field_err(name: &str) -> Value {
    json_err_wrap(Value::variant(
        bv::JSON_MISSING_FIELD,
        vec![Value::String(name.into())],
    ))
}

/// Build `Err(JsonUnknown(msg))` for ad-hoc failures (unknown type
/// descriptor, internal unexpected results, etc.).
pub(crate) fn json_unknown_err<S: Into<String>>(msg: S) -> Value {
    json_err_wrap(Value::variant(
        bv::JSON_UNKNOWN,
        vec![Value::String(msg.into())],
    ))
}

/// Dispatch the builtin `trait Error for JsonError` method table.
/// Called through the VM's `ERROR_TRAIT_DISPATCH`, exactly
/// like `call_io_error_trait`. Scaffolding lives in
/// `super::dispatch_error_trait`; this site just supplies the
/// variant → message rendering.
pub fn call_json_error_trait(name: &str, args: &[Value]) -> Result<Value, VmError> {
    super::dispatch_error_trait("JsonError", name, args, |tag, fields| {
        Some(match (tag, fields) {
            ("JsonSyntax", [Value::String(m), Value::Int(offset)]) => {
                format!("json syntax error at byte {offset}: {m}")
            }
            ("JsonTypeMismatch", [Value::String(exp), Value::String(act)]) => {
                format!("json type mismatch: expected {exp}, got {act}")
            }
            ("JsonMissingField", [Value::String(n)]) => {
                format!("json missing field: {n}")
            }
            ("JsonUnknown", [Value::String(m)]) => m.clone(),
            _ => return None,
        })
    })
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

fn value_to_json(v: &Value) -> Result<serde_json::Value, VmError> {
    Ok(match v {
        Value::Int(n) => serde_json::Value::Number((*n).into()),
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::String(s) => serde_json::Value::String(s.clone()),
        Value::List(xs) => {
            let items: Result<Vec<_>, _> = xs.iter().map(value_to_json).collect();
            serde_json::Value::Array(items?)
        }
        Value::Range(lo, hi) => {
            checked_range_len(*lo, *hi).map_err(VmError::new)?;
            serde_json::Value::Array(
                (*lo..=*hi)
                    .map(|i| serde_json::Value::Number(i.into()))
                    .collect(),
            )
        }
        Value::Map(m) => {
            let obj: Result<serde_json::Map<std::string::String, serde_json::Value>, VmError> = m
                .iter()
                .map(|(k, v)| Ok((k.to_string(), value_to_json(v)?)))
                .collect();
            serde_json::Value::Object(obj?)
        }
        Value::Tuple(vs) => {
            let items: Result<Vec<_>, _> = vs.iter().map(value_to_json).collect();
            serde_json::Value::Array(items?)
        }
        Value::Record(_name, fields) => {
            let obj: Result<serde_json::Map<std::string::String, serde_json::Value>, VmError> =
                fields
                    .iter()
                    .map(|(k, v)| Ok((k.clone(), value_to_json(v)?)))
                    .collect();
            serde_json::Value::Object(obj?)
        }
        Value::Variant(name, fields) if name.is(bv::NONE) && fields.is_empty() => {
            serde_json::Value::Null
        }
        Value::Variant(name, fields) if name.is(bv::SOME) && fields.len() == 1 => {
            value_to_json(&fields[0])?
        }
        Value::Variant(name, fields) => {
            let mut obj = serde_json::Map::new();
            obj.insert(
                "variant".into(),
                serde_json::Value::String(name.name().into()),
            );
            if !fields.is_empty() {
                let items: Result<Vec<_>, _> = fields.iter().map(value_to_json).collect();
                obj.insert("fields".into(), serde_json::Value::Array(items?));
            }
            serde_json::Value::Object(obj)
        }
        Value::Unit => serde_json::Value::Null,
        Value::VariantConstructor(tag) => serde_json::Value::String(tag.name().into()),
        _ => serde_json::Value::Null,
    })
}

// ── JSON helpers ────────────────────────────────────────────────────

/// The record type `ty` a decoder builds, with its fields. `caller` is
/// the builtin on whose behalf the type is looked up (`json.parse`,
/// `toml.parse_list`, ...); it starts the error message. A builtin
/// record type (`Date`) lists no fields and has no decoder.
pub(crate) fn decodable_record(
    caller: &str,
    ty: &Arc<TypeInfo>,
) -> Result<Vec<(std::string::String, FieldType)>, VmError> {
    match &ty.shape {
        Shape::Record(fields) if !fields.is_empty() || !is_builtin_type(ty) => Ok(fields.clone()),
        _ => Err(VmError::new(format!(
            "{caller}: unknown record type '{}'",
            ty.name
        ))),
    }
}

/// Whether `ty` is a builtin type.
fn is_builtin_type(ty: &TypeInfo) -> bool {
    crate::defs::builtin_types()
        .get(ty.id.0.0 as usize)
        .is_some()
}

/// The record type `id` a record field names, from the program's types.
pub(crate) fn field_record_type(
    vm: &Vm,
    caller: &str,
    id: TypeId,
) -> Result<Arc<TypeInfo>, VmError> {
    vm.types
        .get(id)
        .cloned()
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
    /// written). `json_to_record` turns it into a `JsonUnknown` that names
    /// the record field whose type this is.
    Unsupported(std::string::String),
    /// VM-internal failure (e.g. record type missing from globals).
    /// Caught at the call site and converted to a silt-visible
    /// `Err(JsonUnknown(...))`.
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

fn json_to_record(
    vm: &mut Vm,
    ty: &Arc<TypeInfo>,
    fields: &[(std::string::String, FieldType)],
    json: &serde_json::Value,
) -> Result<Value, VmError> {
    let serde_json::Value::Object(obj) = json else {
        return Ok(json_type_mismatch_err("object", json_type_name(json)));
    };
    let mut record_fields = BTreeMap::new();
    for (field_name, field_type) in fields {
        match obj.get(field_name) {
            Some(json_val) => match json_to_typed_value(vm, json_val, field_type) {
                Ok(val) => {
                    record_fields.insert(field_name.clone(), val);
                }
                Err(JsonDecodeErr::Unsupported(declared)) => {
                    return Ok(json_unknown_err(unsupported_field_type_message(
                        &ty.name, field_name, &declared,
                    )));
                }
                Err(e) => return Ok(decode_err_to_silt(e)),
            },
            None => match field_type {
                FieldType::Option(_) => {
                    record_fields.insert(field_name.clone(), Value::variant(bv::NONE, Vec::new()));
                }
                _ => {
                    return Ok(json_missing_field_err(field_name));
                }
            },
        }
    }
    Ok(Value::variant(
        bv::OK,
        vec![Value::Record(ty.clone(), Arc::new(record_fields))],
    ))
}

fn json_to_record_list(
    vm: &mut Vm,
    ty: &Arc<TypeInfo>,
    fields: &[(std::string::String, FieldType)],
    json: &serde_json::Value,
) -> Result<Value, VmError> {
    let serde_json::Value::Array(arr) = json else {
        return Ok(json_type_mismatch_err("array", json_type_name(json)));
    };
    let mut records = Vec::new();
    for item in arr.iter() {
        let result = json_to_record(vm, ty, fields, item)?;
        match result {
            Value::Variant(name, inner) if name.is(bv::OK) && inner.len() == 1 => {
                records.push(inner.into_iter().next().expect("guard guarantees len==1"));
            }
            ref err @ Value::Variant(ref name, _) if name.is(bv::ERR) => {
                // Already a typed `Err(JsonError)`; forward unchanged
                // so the caller still gets a structured variant.
                return Ok(err.clone());
            }
            _ => {
                return Ok(json_unknown_err(format!(
                    "json.parse_list({}): unexpected result",
                    ty.name
                )));
            }
        }
    }
    Ok(Value::variant(bv::OK, vec![Value::List(Arc::new(records))]))
}

fn json_to_map(vm: &mut Vm, value_type: Type, json: &serde_json::Value) -> Result<Value, VmError> {
    let serde_json::Value::Object(obj) = json else {
        return Ok(json_type_mismatch_err("object", json_type_name(json)));
    };
    let field_type = match value_type {
        Type::Primitive(field_type) => field_type,
        Type::Named(ty) => {
            if decodable_record("json.parse_map", ty).is_err() {
                return Ok(json_unknown_err(format!(
                    "json.parse_map: unknown value type '{}'",
                    ty.name
                )));
            }
            FieldType::Record(ty.id)
        }
    };
    let mut map = BTreeMap::new();
    for (key, json_val) in obj {
        match json_to_typed_value(vm, json_val, &field_type) {
            Ok(val) => {
                map.insert(Value::String(key.clone()), val);
            }
            Err(e) => return Ok(decode_err_to_silt(e)),
        }
    }
    Ok(Value::variant(bv::OK, vec![Value::Map(Arc::new(map))]))
}

/// Decode a `serde_json::Value` into a silt `Value` of the expected
/// shape, short-circuiting with a typed `JsonError` variant the moment
/// a mismatch is found. `JsonDecodeErr::Variant(...)` always carries a
/// ready-built `JsonError` variant (not wrapped in `Err(..)`); the
/// caller wraps it before returning to silt.
fn json_to_typed_value(
    vm: &mut Vm,
    json: &serde_json::Value,
    expected: &FieldType,
) -> Result<Value, JsonDecodeErr> {
    // Helper: construct the `JsonTypeMismatch` variant directly.
    let mismatch = |expected: &str, actual: &str| -> JsonDecodeErr {
        JsonDecodeErr::Variant(Value::variant(
            bv::JSON_TYPE_MISMATCH,
            vec![Value::String(expected.into()), Value::String(actual.into())],
        ))
    };
    let unknown = |msg: String| -> JsonDecodeErr {
        JsonDecodeErr::Variant(Value::variant(bv::JSON_UNKNOWN, vec![Value::String(msg)]))
    };
    match expected {
        FieldType::String => match json {
            serde_json::Value::String(s) => Ok(Value::String(s.clone())),
            _ => Err(mismatch("String", json_type_name(json))),
        },
        FieldType::Int => match json {
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Ok(Value::Int(i))
                } else if let Some(f) = n.as_f64() {
                    // Mirror the `float.to_int` range check (B7). A bare
                    // `f as i64` would saturate to i64::MAX/MIN silently,
                    // turning large JSON numbers like 1e100 into i64::MAX —
                    // a data-corruption hazard. Reject values that aren't
                    // finite or don't fit exactly in the i64 range.
                    const I64_MIN_AS_F64: f64 = i64::MIN as f64;
                    const I64_MAX_PLUS_ONE: f64 = 9223372036854775808.0; // exact
                    if !f.is_finite() || !(I64_MIN_AS_F64..I64_MAX_PLUS_ONE).contains(&f) {
                        return Err(unknown(format!("number {f} out of Int range")));
                    }
                    Ok(Value::Int(f as i64))
                } else {
                    Err(unknown("expected Int, got number that doesn't fit".into()))
                }
            }
            _ => Err(mismatch("Int", json_type_name(json))),
        },
        FieldType::Float => match json {
            serde_json::Value::Number(n) => {
                if let Some(f) = n.as_f64() {
                    Ok(crate::builtins::numeric::float_value(f))
                } else {
                    Err(unknown("expected Float, got non-numeric number".into()))
                }
            }
            _ => Err(mismatch("Float", json_type_name(json))),
        },
        FieldType::Bool => match json {
            serde_json::Value::Bool(b) => Ok(Value::Bool(*b)),
            _ => Err(mismatch("Bool", json_type_name(json))),
        },
        FieldType::List(inner) => match json {
            serde_json::Value::Array(arr) => {
                let mut values = Vec::new();
                for item in arr.iter() {
                    values.push(json_to_typed_value(vm, item, inner)?);
                }
                Ok(Value::List(Arc::new(values)))
            }
            _ => Err(mismatch("List", json_type_name(json))),
        },
        FieldType::Option(inner) => match json {
            serde_json::Value::Null => Ok(Value::variant(bv::NONE, Vec::new())),
            _ => {
                let val = json_to_typed_value(vm, json, inner)?;
                Ok(Value::variant(bv::SOME, vec![val]))
            }
        },
        FieldType::Map(inner) => match json {
            serde_json::Value::Object(obj) => {
                let mut map = BTreeMap::new();
                for (key, item) in obj {
                    let val = json_to_typed_value(vm, item, inner)?;
                    map.insert(Value::String(key.clone()), val);
                }
                Ok(Value::Map(Arc::new(map)))
            }
            _ => Err(mismatch("Map", json_type_name(json))),
        },
        FieldType::Tuple(elems) => match json {
            serde_json::Value::Array(arr) if arr.len() == elems.len() => {
                let mut values = Vec::with_capacity(elems.len());
                for (item, elem) in arr.iter().zip(elems) {
                    values.push(json_to_typed_value(vm, item, elem)?);
                }
                Ok(Value::Tuple(values))
            }
            serde_json::Value::Array(arr) => Err(unknown(format!(
                "expected an array of {} elements for a tuple, got {}",
                elems.len(),
                arr.len()
            ))),
            _ => Err(mismatch("Tuple", json_type_name(json))),
        },
        FieldType::Unsupported(declared) => Err(JsonDecodeErr::Unsupported(declared.clone())),
        FieldType::Date => match json {
            serde_json::Value::String(s) => NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .map(make_date)
                .map_err(|e| unknown(format!("invalid date '{s}' (expected YYYY-MM-DD): {e}"))),
            _ => Err(mismatch("date string", json_type_name(json))),
        },
        FieldType::Time => match json {
            serde_json::Value::String(s) => NaiveTime::parse_from_str(s, "%H:%M:%S")
                .or_else(|_| NaiveTime::parse_from_str(s, "%H:%M"))
                .map(make_time)
                .map_err(|e| unknown(format!("invalid time '{s}' (expected HH:MM:SS): {e}"))),
            _ => Err(mismatch("time string", json_type_name(json))),
        },
        FieldType::DateTime => match json {
            serde_json::Value::String(s) => {
                // Try timezone-aware formats first (RFC 3339 / ISO 8601 with offset),
                // converting to UTC. Then fall back to naive formats.
                if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
                    Ok(make_datetime(dt.naive_utc()))
                } else if let Ok(dt) = chrono::DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%z") {
                    Ok(make_datetime(dt.naive_utc()))
                } else if let Ok(dt) = chrono::DateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%z") {
                    Ok(make_datetime(dt.naive_utc()))
                } else {
                    NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
                        .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S"))
                        .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M"))
                        .map(make_datetime)
                        .map_err(|_| unknown(format!("invalid datetime '{s}'")))
                }
            }
            _ => Err(mismatch("datetime string", json_type_name(json))),
        },
        FieldType::Record(id) => {
            let ty = field_record_type(vm, "json.parse", *id)?;
            let fields = decodable_record("json.parse", &ty)?;
            let result = json_to_record(vm, &ty, &fields, json)?;
            match result {
                Value::Variant(name, inner) if name.is(bv::OK) && inner.len() == 1 => {
                    Ok(inner.into_iter().next().expect("len==1"))
                }
                Value::Variant(name, inner) if name.is(bv::ERR) && inner.len() == 1 => {
                    // Re-wrap as JsonDecodeErr::Variant so the outer
                    // caller forwards it unchanged. `inner[0]` is
                    // already a JsonError variant value.
                    Err(JsonDecodeErr::Variant(
                        inner.into_iter().next().expect("len==1"),
                    ))
                }
                _ => Err(unknown(format!("failed to parse {}", ty.name))),
            }
        }
    }
}

// ── The functions ───────────────────────────────────────────────────

builtins! {
    fn parse(vm, s: &str, ty: Type) -> Result<Value, VmError> {
        let parsed = serde_json::from_str::<serde_json::Value>(s);
        match ty {
            // A primitive type: a JSON scalar, decoded as it is.
            Type::Primitive(field_type) => Ok(match parsed {
                Ok(json) => match json_to_typed_value(vm, &json, &field_type) {
                    Ok(val) => Value::variant(bv::OK, vec![val]),
                    Err(e) => decode_err_to_silt(e),
                },
                Err(e) => json_result_err(&e),
            }),
            Type::Named(ty) => {
                let fields = decodable_record("json.parse", ty)?;
                match parsed {
                    Ok(json) => json_to_record(vm, ty, &fields, &json),
                    Err(e) => Ok(json_result_err(&e)),
                }
            }
        }
    }

    fn parse_list(vm, s: &str, ty: Type) -> Result<Value, VmError> {
        let Type::Named(ty) = ty else {
            return Err(VmError::new(
                "json.parse_list: type argument must be a record type".into(),
            ));
        };
        let fields = decodable_record("json.parse_list", ty)?;
        match serde_json::from_str::<serde_json::Value>(s) {
            Ok(json) => json_to_record_list(vm, ty, &fields, &json),
            Err(e) => Ok(json_result_err(&e)),
        }
    }

    fn parse_map(vm, s: &str, value_type: Type) -> Result<Value, VmError> {
        match serde_json::from_str::<serde_json::Value>(s) {
            Ok(json) => json_to_map(vm, value_type, &json),
            Err(e) => Ok(json_result_err(&e)),
        }
    }

    fn stringify(value: &Value) -> Result<String, VmError> {
        Ok(value_to_json(value)?.to_string())
    }

    fn pretty(value: &Value) -> Result<String, VmError> {
        let json = value_to_json(value)?;
        Ok(serde_json::to_string_pretty(&json).unwrap_or_else(|_| json.to_string()))
    }
}
