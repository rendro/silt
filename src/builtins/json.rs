//! The `json.*` builtin functions and the typed decoding `toml.*` shares.

use std::sync::Arc;

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};

use super::time::{make_date, make_datetime, make_time};
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

/// Build `Err(TypeMismatch(expected, actual))`. Used by the
/// hand-written record decoder to report a field whose value has the
/// wrong shape.
pub(crate) fn json_type_mismatch_err(expected: &str, actual: &str) -> Value {
    json_err_wrap(Value::variant(
        bv::JSON_TYPE_MISMATCH,
        vec![Value::String(expected.into()), Value::String(actual.into())],
    ))
}

/// Build `Err(MissingField(name))`.
pub(crate) fn json_missing_field_err(name: &str) -> Value {
    json_err_wrap(Value::variant(
        bv::JSON_MISSING_FIELD,
        vec![Value::String(name.into())],
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

/// What a number that is no `Int` is called in a type mismatch.
const NOT_IN_RANGE: &str = "a number out of Int's range";

/// The `Int` that the number `f` of a document is: a whole number in
/// `Int`'s range. Else what it is instead, for the type mismatch.
fn whole(f: f64) -> Result<i64, &'static str> {
    // `i64::MAX` is not exactly an `f64` (it rounds up to 2^63), so
    // the upper bound is exclusive.
    const I64_MIN_AS_F64: f64 = i64::MIN as f64;
    const I64_MAX_PLUS_ONE: f64 = 9223372036854775808.0; // exact
    if !f.is_finite() || !(I64_MIN_AS_F64..I64_MAX_PLUS_ONE).contains(&f) {
        Err(NOT_IN_RANGE)
    } else if f.fract() != 0.0 {
        Err("a number with a fraction")
    } else {
        Ok(f as i64)
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

/// `v`, which is `level` levels inside the value `caller` writes, as
/// JSON. A value JSON has no form for (a set, bytes, a function, the
/// constructor of a variant among them, a channel, a handle) is an
/// error, wherever in the value it is.
fn value_to_json(caller: &str, v: &Value, level: usize) -> Result<serde_json::Value, VmError> {
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
        Value::Int(n) => serde_json::Value::Number((*n).into()),
        Value::Float(f) => serde_json::Number::from_f64(f.get())
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::String(s) => serde_json::Value::String(s.to_string()),
        Value::List(xs) => {
            xs.writable()?;
            let items: Result<Vec<_>, _> = xs.iter().map(|x| inner(&x)).collect();
            serde_json::Value::Array(items?)
        }
        Value::Map(m) => {
            let obj: Result<serde_json::Map<std::string::String, serde_json::Value>, VmError> = m
                .iter()
                .map(|(k, v)| Ok((k.to_string(), inner(v)?)))
                .collect();
            serde_json::Value::Object(obj?)
        }
        Value::Tuple(vs) => {
            let items: Result<Vec<_>, _> = vs.iter().map(inner).collect();
            serde_json::Value::Array(items?)
        }
        Value::Record(record) => {
            let obj: Result<serde_json::Map<std::string::String, serde_json::Value>, VmError> =
                record
                    .named()
                    .map(|(k, v)| Ok((k.to_string(), inner(v)?)))
                    .collect();
            serde_json::Value::Object(obj?)
        }
        Value::Variant(variant) if variant.is(bv::NONE) || variant.is(bv::JSON_NULL) => {
            serde_json::Value::Null
        }
        Value::Variant(variant) => {
            let mut obj = serde_json::Map::new();
            obj.insert(
                "variant".into(),
                serde_json::Value::String(variant.name().into()),
            );
            if !variant.fields().is_empty() {
                let items: Result<Vec<_>, _> = variant.fields().iter().map(inner).collect();
                obj.insert("fields".into(), serde_json::Value::Array(items?));
            }
            serde_json::Value::Object(obj)
        }
        Value::Unit => serde_json::Value::Null,
        _ => return Err(unsupported_kind(caller, v)),
    })
}

// ── JSON helpers ────────────────────────────────────────────────────

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

fn json_to_record(
    rt: &Runtime,
    ty: &Arc<TypeInfo>,
    fields: &[(std::string::String, FieldType)],
    json: &serde_json::Value,
) -> Result<Value, VmError> {
    let serde_json::Value::Object(obj) = json else {
        return Ok(json_type_mismatch_err("object", json_type_name(json)));
    };
    // (In the order the type declares them: the record's own.)
    let mut record_fields = Vec::with_capacity(fields.len());
    for (field_name, field_type) in fields {
        match obj.get(field_name) {
            Some(json_val) => match json_to_typed_value(rt, json_val, field_type) {
                Ok(val) => record_fields.push(val),
                Err(JsonDecodeErr::Unsupported(declared)) => {
                    return Ok(json_unknown_err(unsupported_field_type_message(
                        &ty.name, field_name, &declared,
                    )));
                }
                Err(e) => return Ok(decode_err_to_silt(e)),
            },
            None => match field_type {
                FieldType::Option(_) => record_fields.push(Value::variant(bv::NONE, Vec::new())),
                _ => {
                    return Ok(json_missing_field_err(field_name));
                }
            },
        }
    }
    Ok(Value::variant(
        bv::OK,
        vec![Value::record(ty.clone(), record_fields)],
    ))
}

fn json_to_record_list(
    rt: &Runtime,
    ty: &Arc<TypeInfo>,
    fields: &[(std::string::String, FieldType)],
    json: &serde_json::Value,
) -> Result<Value, VmError> {
    let serde_json::Value::Array(arr) = json else {
        return Ok(json_type_mismatch_err("array", json_type_name(json)));
    };
    let mut records = Vec::new();
    for item in arr.iter() {
        let result = json_to_record(rt, ty, fields, item)?;
        match result {
            Value::Variant(variant) if variant.is(bv::OK) && variant.fields().len() == 1 => {
                records.push(variant.fields()[0].clone());
            }
            ref err @ Value::Variant(ref variant) if variant.is(bv::ERR) => {
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
    Ok(Value::variant(bv::OK, vec![Value::list(records)]))
}

fn json_to_map(rt: &Runtime, value_type: Type, json: &serde_json::Value) -> Result<Value, VmError> {
    let serde_json::Value::Object(obj) = json else {
        return Ok(json_type_mismatch_err("object", json_type_name(json)));
    };
    let field_type = match value_type {
        Type::Primitive(field_type) => field_type,
        Type::Named(ty) if ty.id == ty::JSON_VALUE => FieldType::JsonValue,
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
    let mut map = Vec::with_capacity(obj.len());
    for (key, json_val) in obj {
        match json_to_typed_value(rt, json_val, &field_type) {
            Ok(val) => map.push((Value::String(key.clone().into()), val)),
            Err(e) => return Ok(decode_err_to_silt(e)),
        }
    }
    Ok(Value::variant(bv::OK, vec![Value::map(map)]))
}

/// The `json.Value` that holds the document `json`. A number is an
/// `Int` when it is a whole number an `Int` can be, as for an `Int`
/// field, and a `Float` otherwise.
fn json_value(json: &serde_json::Value) -> Value {
    match json {
        serde_json::Value::Null => Value::variant(bv::JSON_NULL, Vec::new()),
        serde_json::Value::Bool(b) => Value::variant(bv::JSON_BOOL, vec![Value::Bool(*b)]),
        serde_json::Value::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => Value::variant(bv::JSON_INT, vec![Value::Int(i)]),
            (None, Some(f)) => match (whole(f), Float::new(f)) {
                (Ok(i), _) => Value::variant(bv::JSON_INT, vec![Value::Int(i)]),
                (Err(_), Some(f)) => Value::variant(bv::JSON_FLOAT, vec![Value::Float(f)]),
                // (A document has no NaN.)
                (Err(_), None) => Value::variant(bv::JSON_NULL, Vec::new()),
            },
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
) -> Result<Value, JsonDecodeErr> {
    // Helper: construct the `TypeMismatch` variant directly.
    let mismatch = |expected: &str, actual: &str| -> JsonDecodeErr {
        JsonDecodeErr::Variant(Value::variant(
            bv::JSON_TYPE_MISMATCH,
            vec![Value::String(expected.into()), Value::String(actual.into())],
        ))
    };
    let unknown = |msg: String| -> JsonDecodeErr {
        JsonDecodeErr::Variant(Value::variant(
            bv::JSON_OTHER,
            vec![Value::String(msg.into())],
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
            serde_json::Value::Number(n) => match (n.as_i64(), n.as_f64().map(whole)) {
                (Some(i), _) | (None, Some(Ok(i))) => Ok(Value::Int(i)),
                (None, Some(Err(what))) => Err(mismatch("Int", what)),
                (None, None) => Err(mismatch("Int", NOT_IN_RANGE)),
            },
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
                for item in arr.iter() {
                    values.push(json_to_typed_value(rt, item, inner)?);
                }
                Ok(Value::list(values))
            }
            _ => Err(mismatch("List", json_type_name(json))),
        },
        FieldType::Option(inner) => match json {
            serde_json::Value::Null => Ok(Value::variant(bv::NONE, Vec::new())),
            _ => {
                let val = json_to_typed_value(rt, json, inner)?;
                Ok(Value::variant(bv::SOME, vec![val]))
            }
        },
        FieldType::Map(inner) => match json {
            serde_json::Value::Object(obj) => {
                let mut map = Vec::with_capacity(obj.len());
                for (key, item) in obj {
                    let val = json_to_typed_value(rt, item, inner)?;
                    map.push((Value::String(key.clone().into()), val));
                }
                Ok(Value::map(map))
            }
            _ => Err(mismatch("Map", json_type_name(json))),
        },
        FieldType::Tuple(elems) => match json {
            serde_json::Value::Array(arr) if arr.len() == elems.len() => {
                let mut values = Vec::with_capacity(elems.len());
                for (item, elem) in arr.iter().zip(elems) {
                    values.push(json_to_typed_value(rt, item, elem)?);
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
            let ty = field_record_type(rt, "json.parse", *id)?;
            let fields = decodable_record("json.parse", &ty)?;
            let result = json_to_record(rt, &ty, &fields, json)?;
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
            return json_to_record(rt, ty, &fields, &json);
        }
    };
    Ok(match json_to_typed_value(rt, &json, &field_type) {
        Ok(val) => Value::variant(bv::OK, vec![val]),
        Err(e) => decode_err_to_silt(e),
    })
}

// ── The functions ───────────────────────────────────────────────────

builtins! {
    fn decode(vm, value: &Value, ty: Type) -> Result<Value, VmError> {
        let json = value_to_json("json.decode", value, 0)?;
        decode_document(&vm.runtime, "json.decode", Ok(json), ty)
    }

    fn parse(vm, s: &str, ty: Type) -> Result<Value, VmError> {
        decode_document(&vm.runtime, "json.parse", serde_json::from_str(s), ty)
    }

    fn parse_list(vm, s: &str, ty: Type) -> Result<Value, VmError> {
        if let Type::Named(ty) = ty
            && ty.id == ty::JSON_VALUE
        {
            return Ok(match serde_json::from_str::<serde_json::Value>(s) {
                Ok(serde_json::Value::Array(items)) => Value::variant(
                    bv::OK,
                    vec![Value::list(items.iter().map(json_value).collect())],
                ),
                Ok(json) => json_type_mismatch_err("array", json_type_name(&json)),
                Err(e) => json_result_err(&e),
            });
        }
        let Type::Named(ty) = ty else {
            return Err(VmError::new(
                "json.parse_list: type argument must be a record type".into(),
            ));
        };
        let fields = decodable_record("json.parse_list", ty)?;
        match serde_json::from_str::<serde_json::Value>(s) {
            Ok(json) => json_to_record_list(&vm.runtime, ty, &fields, &json),
            Err(e) => Ok(json_result_err(&e)),
        }
    }

    fn parse_map(vm, s: &str, value_type: Type) -> Result<Value, VmError> {
        match serde_json::from_str::<serde_json::Value>(s) {
            Ok(json) => json_to_map(&vm.runtime, value_type, &json),
            Err(e) => Ok(json_result_err(&e)),
        }
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
