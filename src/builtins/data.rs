//! Data‐format and external‐service builtin functions
//! (`regex.*`, `json.*`, `time.*`, `http.*`).

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Weekday};

use crate::value::Value;
use crate::vm::{Vm, VmError};

// ── Field type for JSON parsing ──────────────────────────────────────

#[derive(Debug, Clone)]
pub(crate) enum FieldType {
    Int,
    Float,
    String,
    Bool,
    List(Box<FieldType>),
    Option(Box<FieldType>),
    Record(std::string::String),
    Date,
    Time,
    DateTime,
}

/// Decode a type encoding string (from compiler metadata) into a FieldType.
pub(crate) fn decode_field_type(s: &str) -> FieldType {
    if let Some(rest) = s.strip_prefix("List:") {
        FieldType::List(Box::new(decode_field_type(rest)))
    } else if let Some(rest) = s.strip_prefix("Option:") {
        FieldType::Option(Box::new(decode_field_type(rest)))
    } else if let Some(rest) = s.strip_prefix("Record:") {
        FieldType::Record(rest.to_string())
    } else {
        match s {
            "Int" => FieldType::Int,
            "Float" => FieldType::Float,
            "String" => FieldType::String,
            "Bool" => FieldType::Bool,
            "Date" => FieldType::Date,
            "Time" => FieldType::Time,
            "DateTime" => FieldType::DateTime,
            other => FieldType::Record(other.to_string()),
        }
    }
}

/// Compute (year, month, day) from Unix epoch seconds.
/// Uses Howard Hinnant's civil_from_days algorithm (public domain).
#[cfg(not(feature = "local-clock"))]
fn civil_from_epoch_secs(secs: i64) -> (i32, u32, u32) {
    let z = secs.div_euclid(86400) + 719468;
    let era = z.div_euclid(146097);
    let doe = (z - era * 146097) as u32; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year [0, 365]
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

/// Returns the number of days in the given month (1-12) for the given year.
fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0) {
                29
            } else {
                28
            }
        }
        _ => 30, // fallback for invalid month
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

fn value_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Int(n) => serde_json::Value::Number((*n).into()),
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::String(s) => serde_json::Value::String(s.clone()),
        Value::List(xs) => serde_json::Value::Array(xs.iter().map(value_to_json).collect()),
        Value::Range(lo, hi) => serde_json::Value::Array(
            (*lo..=*hi)
                .map(|i| serde_json::Value::Number(i.into()))
                .collect(),
        ),
        Value::Map(m) => {
            let obj: serde_json::Map<std::string::String, serde_json::Value> = m
                .iter()
                .map(|(k, v)| (k.to_string(), value_to_json(v)))
                .collect();
            serde_json::Value::Object(obj)
        }
        Value::Tuple(vs) => serde_json::Value::Array(vs.iter().map(value_to_json).collect()),
        Value::Record(_name, fields) => {
            let obj: serde_json::Map<std::string::String, serde_json::Value> = fields
                .iter()
                .map(|(k, v)| (k.clone(), value_to_json(v)))
                .collect();
            serde_json::Value::Object(obj)
        }
        Value::Variant(name, fields) if name == "None" && fields.is_empty() => {
            serde_json::Value::Null
        }
        Value::Variant(name, fields) if name == "Some" && fields.len() == 1 => {
            value_to_json(&fields[0])
        }
        Value::Variant(name, fields) => {
            let mut obj = serde_json::Map::new();
            obj.insert("variant".into(), serde_json::Value::String(name.clone()));
            if !fields.is_empty() {
                obj.insert(
                    "fields".into(),
                    serde_json::Value::Array(fields.iter().map(value_to_json).collect()),
                );
            }
            serde_json::Value::Object(obj)
        }
        Value::Unit => serde_json::Value::Null,
        Value::VariantConstructor(name, _) => serde_json::Value::String(name.clone()),
        _ => serde_json::Value::Null,
    }
}

// ── Time helpers ────────────────────────────────────────────────────

/// Build a Silt `Date` record Value from chrono NaiveDate.
pub(crate) fn make_date(d: NaiveDate) -> Value {
    let mut fields = BTreeMap::new();
    fields.insert("year".into(), Value::Int(d.year() as i64));
    fields.insert("month".into(), Value::Int(d.month() as i64));
    fields.insert("day".into(), Value::Int(d.day() as i64));
    Value::Record("Date".into(), Arc::new(fields))
}

/// Build a Silt `Time` record Value from chrono NaiveTime.
pub(crate) fn make_time(t: NaiveTime) -> Value {
    let mut fields = BTreeMap::new();
    fields.insert("hour".into(), Value::Int(t.hour() as i64));
    fields.insert("minute".into(), Value::Int(t.minute() as i64));
    fields.insert("second".into(), Value::Int(t.second() as i64));
    fields.insert("ns".into(), Value::Int(t.nanosecond() as i64));
    Value::Record("Time".into(), Arc::new(fields))
}

/// Build a Silt `DateTime` record Value from chrono NaiveDateTime.
pub(crate) fn make_datetime(dt: NaiveDateTime) -> Value {
    let date_val = make_date(dt.date());
    let time_val = make_time(dt.time());
    let mut fields = BTreeMap::new();
    fields.insert("date".into(), date_val);
    fields.insert("time".into(), time_val);
    Value::Record("DateTime".into(), Arc::new(fields))
}

/// Build a Silt `Instant` record Value.
fn make_instant(epoch_ns: i64) -> Value {
    let mut fields = BTreeMap::new();
    fields.insert("epoch_ns".into(), Value::Int(epoch_ns));
    Value::Record("Instant".into(), Arc::new(fields))
}

/// Build a Silt `Duration` record Value.
fn make_duration(ns: i64) -> Value {
    let mut fields = BTreeMap::new();
    fields.insert("ns".into(), Value::Int(ns));
    Value::Record("Duration".into(), Arc::new(fields))
}

/// Extract a NaiveDate from a Silt Date record.
fn extract_date(v: &Value) -> Result<NaiveDate, VmError> {
    let Value::Record(name, fields) = v else {
        return Err(VmError::new("expected a Date record".into()));
    };
    if name != "Date" {
        return Err(VmError::new(format!("expected Date, got {name}")));
    }
    let y = match fields.get("year") {
        Some(Value::Int(n)) => *n as i32,
        _ => 0,
    };
    let m = match fields.get("month") {
        Some(Value::Int(n)) => *n as u32,
        _ => 1,
    };
    let d = match fields.get("day") {
        Some(Value::Int(n)) => *n as u32,
        _ => 1,
    };
    NaiveDate::from_ymd_opt(y, m, d)
        .ok_or_else(|| VmError::new(format!("invalid date: {y}-{m}-{d}")))
}

/// Extract a NaiveTime from a Silt Time record.
fn extract_time(v: &Value) -> Result<NaiveTime, VmError> {
    let Value::Record(name, fields) = v else {
        return Err(VmError::new("expected a Time record".into()));
    };
    if name != "Time" {
        return Err(VmError::new(format!("expected Time, got {name}")));
    }
    let h = match fields.get("hour") {
        Some(Value::Int(n)) => *n as u32,
        _ => 0,
    };
    let m = match fields.get("minute") {
        Some(Value::Int(n)) => *n as u32,
        _ => 0,
    };
    let s = match fields.get("second") {
        Some(Value::Int(n)) => *n as u32,
        _ => 0,
    };
    let ns = match fields.get("ns") {
        Some(Value::Int(n)) => *n as u32,
        _ => 0,
    };
    NaiveTime::from_hms_nano_opt(h, m, s, ns)
        .ok_or_else(|| VmError::new(format!("invalid time: {h}:{m}:{s}.{ns}")))
}

/// Extract a NaiveDateTime from a Silt DateTime record.
fn extract_datetime(v: &Value) -> Result<NaiveDateTime, VmError> {
    let Value::Record(name, fields) = v else {
        return Err(VmError::new("expected a DateTime record".into()));
    };
    if name != "DateTime" {
        return Err(VmError::new(format!("expected DateTime, got {name}")));
    }
    let date = fields
        .get("date")
        .ok_or_else(|| VmError::new("DateTime missing date field".into()))?;
    let time = fields
        .get("time")
        .ok_or_else(|| VmError::new("DateTime missing time field".into()))?;
    let d = extract_date(date)?;
    let t = extract_time(time)?;
    Ok(NaiveDateTime::new(d, t))
}

/// Extract epoch_ns from an Instant record.
fn extract_instant(v: &Value) -> Result<i64, VmError> {
    let Value::Record(name, fields) = v else {
        return Err(VmError::new("expected an Instant record".into()));
    };
    if name != "Instant" {
        return Err(VmError::new(format!("expected Instant, got {name}")));
    }
    match fields.get("epoch_ns") {
        Some(Value::Int(n)) => Ok(*n),
        _ => Err(VmError::new("Instant missing epoch_ns field".into())),
    }
}

/// Extract ns from a Duration record.
fn extract_duration(v: &Value) -> Result<i64, VmError> {
    let Value::Record(name, fields) = v else {
        return Err(VmError::new("expected a Duration record".into()));
    };
    if name != "Duration" {
        return Err(VmError::new(format!("expected Duration, got {name}")));
    }
    match fields.get("ns") {
        Some(Value::Int(n)) => Ok(*n),
        _ => Err(VmError::new("Duration missing ns field".into())),
    }
}

// ── JSON helpers ────────────────────────────────────────────────────

/// Load record field info from the `__record_fields__<type>` global metadata.
fn load_record_fields(
    vm: &mut Vm,
    type_name: &str,
) -> Result<Vec<(std::string::String, FieldType)>, VmError> {
    // Check cache first
    if let Some(fields) = vm.record_types.get(type_name) {
        return Ok(fields.clone());
    }
    // Look up the metadata global
    let meta_key = format!("__record_fields__{type_name}");
    let meta = vm.globals.get(&meta_key).cloned();
    match meta {
        Some(Value::List(items)) => {
            let mut fields = Vec::new();
            let mut i = 0;
            while i + 1 < items.len() {
                if let (Value::String(fname), Value::String(ftype)) = (&items[i], &items[i + 1]) {
                    fields.push((fname.clone(), decode_field_type(ftype)));
                }
                i += 2;
            }
            vm.record_types
                .insert(type_name.to_string(), fields.clone());
            Ok(fields)
        }
        _ => Err(VmError::new(format!(
            "json.parse: unknown record type '{type_name}'"
        ))),
    }
}

fn json_to_record(
    vm: &mut Vm,
    type_name: &str,
    fields: &[(std::string::String, FieldType)],
    json: &serde_json::Value,
) -> Result<Value, VmError> {
    let serde_json::Value::Object(obj) = json else {
        return Ok(Value::Variant(
            "Err".into(),
            vec![Value::String(format!(
                "json.parse({type_name}): expected JSON object, got {}",
                json_type_name(json)
            ))],
        ));
    };
    let mut record_fields = BTreeMap::new();
    for (field_name, field_type) in fields {
        match obj.get(field_name) {
            Some(json_val) => {
                match json_to_typed_value(vm, json_val, field_type, type_name, field_name) {
                    Ok(val) => {
                        record_fields.insert(field_name.clone(), val);
                    }
                    Err(e) => {
                        return Ok(Value::Variant(
                            "Err".into(),
                            vec![Value::String(e.message.clone())],
                        ));
                    }
                }
            }
            None => match field_type {
                FieldType::Option(_) => {
                    record_fields.insert(
                        field_name.clone(),
                        Value::Variant("None".into(), Vec::new()),
                    );
                }
                _ => {
                    return Ok(Value::Variant(
                        "Err".into(),
                        vec![Value::String(format!(
                            "json.parse({type_name}): missing field '{field_name}'"
                        ))],
                    ));
                }
            },
        }
    }
    Ok(Value::Variant(
        "Ok".into(),
        vec![Value::Record(
            type_name.to_string(),
            Arc::new(record_fields),
        )],
    ))
}

fn json_to_record_list(
    vm: &mut Vm,
    type_name: &str,
    fields: &[(std::string::String, FieldType)],
    json: &serde_json::Value,
) -> Result<Value, VmError> {
    let serde_json::Value::Array(arr) = json else {
        return Ok(Value::Variant(
            "Err".into(),
            vec![Value::String(format!(
                "json.parse_list({type_name}): expected JSON array, got {}",
                json_type_name(json)
            ))],
        ));
    };
    let mut records = Vec::new();
    for (i, item) in arr.iter().enumerate() {
        let result = json_to_record(vm, type_name, fields, item)?;
        match result {
            Value::Variant(name, inner) if name == "Ok" && inner.len() == 1 => {
                records.push(inner.into_iter().next().unwrap());
            }
            Value::Variant(name, inner) if name == "Err" && inner.len() == 1 => {
                if let Value::String(msg) = &inner[0] {
                    return Ok(Value::Variant(
                        "Err".into(),
                        vec![Value::String(format!(
                            "json.parse_list({type_name}): element {i}: {msg}"
                        ))],
                    ));
                } else {
                    return Ok(Value::Variant(
                        "Err".into(),
                        vec![Value::String(format!(
                            "json.parse_list({type_name}): element {i}: parse error"
                        ))],
                    ));
                }
            }
            _ => {
                return Ok(Value::Variant(
                    "Err".into(),
                    vec![Value::String(format!(
                        "json.parse_list({type_name}): element {i}: unexpected result"
                    ))],
                ));
            }
        }
    }
    Ok(Value::Variant(
        "Ok".into(),
        vec![Value::List(Arc::new(records))],
    ))
}

fn json_to_map(vm: &mut Vm, value_type: &str, json: &serde_json::Value) -> Result<Value, VmError> {
    let serde_json::Value::Object(obj) = json else {
        return Ok(Value::Variant(
            "Err".into(),
            vec![Value::String(format!(
                "json.parse_map: expected JSON object, got {}",
                json_type_name(json)
            ))],
        ));
    };
    let field_type = match value_type {
        "String" => FieldType::String,
        "Int" => FieldType::Int,
        "Float" => FieldType::Float,
        "Bool" => FieldType::Bool,
        record_name => {
            // Check if it's a known record type
            let meta_key = format!("__record_fields__{record_name}");
            if !vm.globals.contains_key(&meta_key) {
                return Ok(Value::Variant(
                    "Err".into(),
                    vec![Value::String(format!(
                        "json.parse_map: unknown value type '{record_name}'"
                    ))],
                ));
            }
            FieldType::Record(record_name.to_string())
        }
    };
    let mut map = BTreeMap::new();
    for (key, json_val) in obj {
        match json_to_typed_value(vm, json_val, &field_type, "Map", key) {
            Ok(val) => {
                map.insert(Value::String(key.clone()), val);
            }
            Err(e) => {
                return Ok(Value::Variant(
                    "Err".into(),
                    vec![Value::String(format!(
                        "json.parse_map: key '{key}': {}",
                        e.message
                    ))],
                ));
            }
        }
    }
    Ok(Value::Variant("Ok".into(), vec![Value::Map(Arc::new(map))]))
}

fn json_to_typed_value(
    vm: &mut Vm,
    json: &serde_json::Value,
    expected: &FieldType,
    parent_type: &str,
    field_name: &str,
) -> Result<Value, VmError> {
    match expected {
        FieldType::String => match json {
            serde_json::Value::String(s) => Ok(Value::String(s.clone())),
            _ => Err(VmError::new(format!(
                "json.parse({parent_type}): field '{field_name}': expected String, got {}",
                json_type_name(json)
            ))),
        },
        FieldType::Int => match json {
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Ok(Value::Int(i))
                } else if let Some(f) = n.as_f64() {
                    Ok(Value::Int(f as i64))
                } else {
                    Err(VmError::new(format!(
                        "json.parse({parent_type}): field '{field_name}': expected Int, got number that doesn't fit"
                    )))
                }
            }
            _ => Err(VmError::new(format!(
                "json.parse({parent_type}): field '{field_name}': expected Int, got {}",
                json_type_name(json)
            ))),
        },
        FieldType::Float => match json {
            serde_json::Value::Number(n) => {
                if let Some(f) = n.as_f64() {
                    Ok(Value::Float(f))
                } else {
                    Err(VmError::new(format!(
                        "json.parse({parent_type}): field '{field_name}': expected Float, got non-numeric number"
                    )))
                }
            }
            _ => Err(VmError::new(format!(
                "json.parse({parent_type}): field '{field_name}': expected Float, got {}",
                json_type_name(json)
            ))),
        },
        FieldType::Bool => match json {
            serde_json::Value::Bool(b) => Ok(Value::Bool(*b)),
            _ => Err(VmError::new(format!(
                "json.parse({parent_type}): field '{field_name}': expected Bool, got {}",
                json_type_name(json)
            ))),
        },
        FieldType::List(inner) => match json {
            serde_json::Value::Array(arr) => {
                let mut values = Vec::new();
                for (i, item) in arr.iter().enumerate() {
                    let idx_name = format!("{field_name}[{i}]");
                    match json_to_typed_value(vm, item, inner, parent_type, &idx_name) {
                        Ok(v) => values.push(v),
                        Err(e) => return Err(e),
                    }
                }
                Ok(Value::List(Arc::new(values)))
            }
            _ => Err(VmError::new(format!(
                "json.parse({parent_type}): field '{field_name}': expected List, got {}",
                json_type_name(json)
            ))),
        },
        FieldType::Option(inner) => match json {
            serde_json::Value::Null => {
                Ok(Value::Variant("None".into(), Vec::new()))
            }
            _ => {
                let val = json_to_typed_value(vm, json, inner, parent_type, field_name)?;
                Ok(Value::Variant("Some".into(), vec![val]))
            }
        },
        FieldType::Date => match json {
            serde_json::Value::String(s) => {
                NaiveDate::parse_from_str(s, "%Y-%m-%d")
                    .map(make_date)
                    .map_err(|e| VmError::new(format!(
                        "json.parse({parent_type}): field '{field_name}': invalid date '{s}' (expected YYYY-MM-DD): {e}"
                    )))
            }
            _ => Err(VmError::new(format!(
                "json.parse({parent_type}): field '{field_name}': expected date string, got {}",
                json_type_name(json)
            ))),
        },
        FieldType::Time => match json {
            serde_json::Value::String(s) => {
                NaiveTime::parse_from_str(s, "%H:%M:%S")
                    .or_else(|_| NaiveTime::parse_from_str(s, "%H:%M"))
                    .map(make_time)
                    .map_err(|e| VmError::new(format!(
                        "json.parse({parent_type}): field '{field_name}': invalid time '{s}' (expected HH:MM:SS): {e}"
                    )))
            }
            _ => Err(VmError::new(format!(
                "json.parse({parent_type}): field '{field_name}': expected time string, got {}",
                json_type_name(json)
            ))),
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
                        .map_err(|_| VmError::new(format!(
                            "json.parse({parent_type}): field '{field_name}': invalid datetime '{s}'"
                        )))
                }
            }
            _ => Err(VmError::new(format!(
                "json.parse({parent_type}): field '{field_name}': expected datetime string, got {}",
                json_type_name(json)
            ))),
        },
        FieldType::Record(rec_name) => {
            let fields = load_record_fields(vm, rec_name)?;
            let result = json_to_record(vm, rec_name, &fields, json)?;
            match &result {
                Value::Variant(name, inner) if name == "Ok" && inner.len() == 1 => {
                    Ok(inner[0].clone())
                }
                Value::Variant(name, inner) if name == "Err" && inner.len() == 1 => {
                    if let Value::String(msg) = &inner[0] {
                        Err(VmError::new(format!(
                            "json.parse({parent_type}): field '{field_name}': {msg}"
                        )))
                    } else {
                        Err(VmError::new(format!(
                            "json.parse({parent_type}): field '{field_name}': failed to parse {rec_name}"
                        )))
                    }
                }
                _ => Err(VmError::new(format!(
                    "json.parse({parent_type}): field '{field_name}': unexpected result"
                ))),
            }
        }
    }
}

// ── Regex dispatch ──────────────────────────────────────────────────

/// Dispatch `regex.<name>(args)`.
pub fn call_regex(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "is_match" => {
            if args.len() != 2 {
                return Err(VmError::new(
                    "regex.is_match takes 2 arguments (pattern, text)".into(),
                ));
            }
            let (Value::String(pattern), Value::String(text)) = (&args[0], &args[1]) else {
                return Err(VmError::new(
                    "regex.is_match requires string arguments".into(),
                ));
            };
            let re = Vm::get_regex(&mut vm.regex_cache, pattern)?;
            Ok(Value::Bool(re.is_match(text)))
        }
        "find" => {
            if args.len() != 2 {
                return Err(VmError::new(
                    "regex.find takes 2 arguments (pattern, text)".into(),
                ));
            }
            let (Value::String(pattern), Value::String(text)) = (&args[0], &args[1]) else {
                return Err(VmError::new("regex.find requires string arguments".into()));
            };
            let re = Vm::get_regex(&mut vm.regex_cache, pattern)?;
            match re.find(text) {
                Some(m) => Ok(Value::Variant(
                    "Some".into(),
                    vec![Value::String(m.as_str().to_string())],
                )),
                None => Ok(Value::Variant("None".into(), Vec::new())),
            }
        }
        "find_all" => {
            if args.len() != 2 {
                return Err(VmError::new(
                    "regex.find_all takes 2 arguments (pattern, text)".into(),
                ));
            }
            let (Value::String(pattern), Value::String(text)) = (&args[0], &args[1]) else {
                return Err(VmError::new(
                    "regex.find_all requires string arguments".into(),
                ));
            };
            let re = Vm::get_regex(&mut vm.regex_cache, pattern)?;
            let matches: Vec<Value> = re
                .find_iter(text)
                .map(|m| Value::String(m.as_str().to_string()))
                .collect();
            Ok(Value::List(Arc::new(matches)))
        }
        "split" => {
            if args.len() != 2 {
                return Err(VmError::new(
                    "regex.split takes 2 arguments (pattern, text)".into(),
                ));
            }
            let (Value::String(pattern), Value::String(text)) = (&args[0], &args[1]) else {
                return Err(VmError::new("regex.split requires string arguments".into()));
            };
            let re = Vm::get_regex(&mut vm.regex_cache, pattern)?;
            let parts: Vec<Value> = re
                .split(text)
                .map(|s| Value::String(s.to_string()))
                .collect();
            Ok(Value::List(Arc::new(parts)))
        }
        "replace" => {
            if args.len() != 3 {
                return Err(VmError::new(
                    "regex.replace takes 3 arguments (pattern, text, replacement)".into(),
                ));
            }
            let (Value::String(pattern), Value::String(text), Value::String(replacement)) =
                (&args[0], &args[1], &args[2])
            else {
                return Err(VmError::new(
                    "regex.replace requires string arguments".into(),
                ));
            };
            let re = Vm::get_regex(&mut vm.regex_cache, pattern)?;
            Ok(Value::String(
                re.replace(text, replacement.as_str()).to_string(),
            ))
        }
        "replace_all" => {
            if args.len() != 3 {
                return Err(VmError::new(
                    "regex.replace_all takes 3 arguments (pattern, text, replacement)".into(),
                ));
            }
            let (Value::String(pattern), Value::String(text), Value::String(replacement)) =
                (&args[0], &args[1], &args[2])
            else {
                return Err(VmError::new(
                    "regex.replace_all requires string arguments".into(),
                ));
            };
            let re = Vm::get_regex(&mut vm.regex_cache, pattern)?;
            Ok(Value::String(
                re.replace_all(text, replacement.as_str()).to_string(),
            ))
        }
        "replace_all_with" => {
            if args.len() != 3 {
                return Err(VmError::new(
                    "regex.replace_all_with takes 3 arguments (pattern, text, fn)".into(),
                ));
            }
            let Value::String(pattern) = &args[0] else {
                return Err(VmError::new(
                    "regex.replace_all_with requires a string pattern".into(),
                ));
            };
            let Value::String(text) = &args[1] else {
                return Err(VmError::new(
                    "regex.replace_all_with requires a string text".into(),
                ));
            };
            let callback = args[2].clone();
            let re = Vm::get_regex(&mut vm.regex_cache, pattern)?.clone();
            let mut result = std::string::String::new();
            let mut last_end = 0;
            for m in re.find_iter(text) {
                result.push_str(&text[last_end..m.start()]);
                let replacement =
                    vm.invoke_callable(&callback, &[Value::String(m.as_str().to_string())])?;
                match replacement {
                    Value::String(s) => result.push_str(&s),
                    _ => {
                        return Err(VmError::new(
                            "regex.replace_all_with callback must return a string".into(),
                        ));
                    }
                }
                last_end = m.end();
            }
            result.push_str(&text[last_end..]);
            Ok(Value::String(result))
        }
        "captures" => {
            if args.len() != 2 {
                return Err(VmError::new(
                    "regex.captures takes 2 arguments (pattern, text)".into(),
                ));
            }
            let (Value::String(pattern), Value::String(text)) = (&args[0], &args[1]) else {
                return Err(VmError::new(
                    "regex.captures requires string arguments".into(),
                ));
            };
            let re = Vm::get_regex(&mut vm.regex_cache, pattern)?;
            match re.captures(text) {
                Some(caps) => {
                    let groups: Vec<Value> = caps
                        .iter()
                        .map(|m| match m {
                            Some(m) => Value::String(m.as_str().to_string()),
                            None => Value::String(std::string::String::new()),
                        })
                        .collect();
                    Ok(Value::Variant(
                        "Some".into(),
                        vec![Value::List(Arc::new(groups))],
                    ))
                }
                None => Ok(Value::Variant("None".into(), Vec::new())),
            }
        }
        "captures_all" => {
            if args.len() != 2 {
                return Err(VmError::new(
                    "regex.captures_all takes 2 arguments (pattern, text)".into(),
                ));
            }
            let (Value::String(pattern), Value::String(text)) = (&args[0], &args[1]) else {
                return Err(VmError::new(
                    "regex.captures_all requires string arguments".into(),
                ));
            };
            let re = Vm::get_regex(&mut vm.regex_cache, pattern)?;
            let all_captures: Vec<Value> = re
                .captures_iter(text)
                .map(|caps| {
                    let groups: Vec<Value> = caps
                        .iter()
                        .map(|m| match m {
                            Some(m) => Value::String(m.as_str().to_string()),
                            None => Value::String(std::string::String::new()),
                        })
                        .collect();
                    Value::List(Arc::new(groups))
                })
                .collect();
            Ok(Value::List(Arc::new(all_captures)))
        }
        _ => Err(VmError::new(format!("unknown regex function: {name}"))),
    }
}

// ── JSON dispatch ───────────────────────────────────────────────────

/// Dispatch `json.<name>(args)`.
pub fn call_json(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "parse" => {
            if args.len() != 2 {
                return Err(VmError::new(
                    "json.parse takes 2 arguments: (Type, String)".into(),
                ));
            }
            let Value::RecordDescriptor(type_name) = &args[0] else {
                return Err(VmError::new(
                    "json.parse: first argument must be a record type".into(),
                ));
            };
            let type_name = type_name.clone();
            let Value::String(s) = &args[1] else {
                return Err(VmError::new(
                    "json.parse: second argument must be a string".into(),
                ));
            };
            let s = s.clone();
            let fields = load_record_fields(vm, &type_name)?;
            match serde_json::from_str::<serde_json::Value>(&s) {
                Ok(json_val) => json_to_record(vm, &type_name, &fields, &json_val),
                Err(e) => Ok(Value::Variant(
                    "Err".into(),
                    vec![Value::String(format!("json.parse: {e}"))],
                )),
            }
        }
        "parse_list" => {
            if args.len() != 2 {
                return Err(VmError::new(
                    "json.parse_list takes 2 arguments: (Type, String)".into(),
                ));
            }
            let Value::RecordDescriptor(type_name) = &args[0] else {
                return Err(VmError::new(
                    "json.parse_list: first argument must be a record type".into(),
                ));
            };
            let type_name = type_name.clone();
            let Value::String(s) = &args[1] else {
                return Err(VmError::new(
                    "json.parse_list: second argument must be a string".into(),
                ));
            };
            let s = s.clone();
            let fields = load_record_fields(vm, &type_name)?;
            match serde_json::from_str::<serde_json::Value>(&s) {
                Ok(json_val) => json_to_record_list(vm, &type_name, &fields, &json_val),
                Err(e) => Ok(Value::Variant(
                    "Err".into(),
                    vec![Value::String(format!("json.parse_list: {e}"))],
                )),
            }
        }
        "parse_map" => {
            if args.len() != 2 {
                return Err(VmError::new(
                    "json.parse_map takes 2 arguments: (ValueType, String)".into(),
                ));
            }
            let value_type = match &args[0] {
                Value::PrimitiveDescriptor(name) => name.clone(),
                Value::RecordDescriptor(name) => name.clone(),
                _ => return Err(VmError::new(
                    "json.parse_map: first argument must be a type (Int, Float, String, Bool, or a record type)".into()
                )),
            };
            let Value::String(s) = &args[1] else {
                return Err(VmError::new(
                    "json.parse_map: second argument must be a string".into(),
                ));
            };
            let s = s.clone();
            match serde_json::from_str::<serde_json::Value>(&s) {
                Ok(json_val) => json_to_map(vm, &value_type, &json_val),
                Err(e) => Ok(Value::Variant(
                    "Err".into(),
                    vec![Value::String(format!("json.parse_map: {e}"))],
                )),
            }
        }
        "stringify" => {
            if args.len() != 1 {
                return Err(VmError::new("json.stringify takes 1 argument".into()));
            }
            let j = value_to_json(&args[0]);
            Ok(Value::String(j.to_string()))
        }
        "pretty" => {
            if args.len() != 1 {
                return Err(VmError::new("json.pretty takes 1 argument".into()));
            }
            let j = value_to_json(&args[0]);
            Ok(Value::String(
                serde_json::to_string_pretty(&j).unwrap_or_else(|_| j.to_string()),
            ))
        }
        _ => Err(VmError::new(format!("unknown json function: {name}"))),
    }
}

// ── Time dispatch ───────────────────────────────────────────────────

/// Dispatch `time.<name>(args)`.
pub fn call_time(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "now" => {
            if !args.is_empty() {
                return Err(VmError::new("time.now takes 0 arguments".into()));
            }
            let epoch_ns = vm.epoch_ms()? * 1_000_000;
            Ok(make_instant(epoch_ns))
        }

        "today" => {
            if !args.is_empty() {
                return Err(VmError::new("time.today takes 0 arguments".into()));
            }
            #[cfg(feature = "local-clock")]
            {
                let today = chrono::Local::now().date_naive();
                Ok(make_date(today))
            }
            #[cfg(not(feature = "local-clock"))]
            {
                let secs = vm.epoch_ms()? / 1000;
                let (y, m, d) = civil_from_epoch_secs(secs);
                let date = NaiveDate::from_ymd_opt(y, m, d)
                    .ok_or_else(|| VmError::new("time.today: date out of range".into()))?;
                Ok(make_date(date))
            }
        }

        "date" => {
            if args.len() != 3 {
                return Err(VmError::new(
                    "time.date takes 3 arguments (year, month, day)".into(),
                ));
            }
            let (Value::Int(y), Value::Int(m), Value::Int(d)) = (&args[0], &args[1], &args[2])
            else {
                return Err(VmError::new("time.date requires Int arguments".into()));
            };
            match NaiveDate::from_ymd_opt(*y as i32, *m as u32, *d as u32) {
                Some(date) => Ok(Value::Variant("Ok".into(), vec![make_date(date)])),
                None => Ok(Value::Variant(
                    "Err".into(),
                    vec![Value::String(format!("invalid date: {y}-{m}-{d}"))],
                )),
            }
        }

        "time" => {
            if args.len() != 3 {
                return Err(VmError::new(
                    "time.time takes 3 arguments (hour, min, sec)".into(),
                ));
            }
            let (Value::Int(h), Value::Int(m), Value::Int(s)) = (&args[0], &args[1], &args[2])
            else {
                return Err(VmError::new("time.time requires Int arguments".into()));
            };
            match NaiveTime::from_hms_opt(*h as u32, *m as u32, *s as u32) {
                Some(t) => Ok(Value::Variant("Ok".into(), vec![make_time(t)])),
                None => Ok(Value::Variant(
                    "Err".into(),
                    vec![Value::String(format!("invalid time: {h}:{m}:{s}"))],
                )),
            }
        }

        "datetime" => {
            if args.len() != 2 {
                return Err(VmError::new(
                    "time.datetime takes 2 arguments (date, time)".into(),
                ));
            }
            let d = extract_date(&args[0])?;
            let t = extract_time(&args[1])?;
            Ok(make_datetime(NaiveDateTime::new(d, t)))
        }

        "to_datetime" => {
            if args.len() != 2 {
                return Err(VmError::new(
                    "time.to_datetime takes 2 arguments (instant, offset_minutes)".into(),
                ));
            }
            let epoch_ns = extract_instant(&args[0])?;
            let Value::Int(offset_min) = &args[1] else {
                return Err(VmError::new("time.to_datetime requires Int offset".into()));
            };
            let epoch_secs = epoch_ns / 1_000_000_000;
            let nano_remainder = (epoch_ns % 1_000_000_000) as u32;
            let utc_dt = DateTime::from_timestamp(epoch_secs, nano_remainder)
                .ok_or_else(|| VmError::new("instant out of range".into()))?
                .naive_utc();
            let offset = chrono::Duration::minutes(*offset_min);
            let local_dt = utc_dt + offset;
            Ok(make_datetime(local_dt))
        }

        "to_instant" => {
            if args.len() != 2 {
                return Err(VmError::new(
                    "time.to_instant takes 2 arguments (datetime, offset_minutes)".into(),
                ));
            }
            let dt = extract_datetime(&args[0])?;
            let Value::Int(offset_min) = &args[1] else {
                return Err(VmError::new("time.to_instant requires Int offset".into()));
            };
            let offset = chrono::Duration::minutes(*offset_min);
            let utc_dt = dt - offset;
            let epoch_ns = utc_dt
                .and_utc()
                .timestamp_nanos_opt()
                .ok_or_else(|| VmError::new("datetime out of range for nanosecond epoch".into()))?;
            Ok(make_instant(epoch_ns))
        }

        "to_utc" => {
            if args.len() != 1 {
                return Err(VmError::new(
                    "time.to_utc takes 1 argument (instant)".into(),
                ));
            }
            let epoch_ns = extract_instant(&args[0])?;
            let epoch_secs = epoch_ns / 1_000_000_000;
            let nano_remainder = (epoch_ns % 1_000_000_000) as u32;
            let dt = DateTime::from_timestamp(epoch_secs, nano_remainder)
                .ok_or_else(|| VmError::new("instant out of range".into()))?
                .naive_utc();
            Ok(make_datetime(dt))
        }

        "from_utc" => {
            if args.len() != 1 {
                return Err(VmError::new(
                    "time.from_utc takes 1 argument (datetime)".into(),
                ));
            }
            let dt = extract_datetime(&args[0])?;
            let epoch_ns = dt
                .and_utc()
                .timestamp_nanos_opt()
                .ok_or_else(|| VmError::new("datetime out of range for nanosecond epoch".into()))?;
            Ok(make_instant(epoch_ns))
        }

        "format" => {
            if args.len() != 2 {
                return Err(VmError::new(
                    "time.format takes 2 arguments (datetime, pattern)".into(),
                ));
            }
            let dt = extract_datetime(&args[0])?;
            let Value::String(pattern) = &args[1] else {
                return Err(VmError::new("time.format requires a String pattern".into()));
            };
            Ok(Value::String(dt.format(pattern).to_string()))
        }

        "format_date" => {
            if args.len() != 2 {
                return Err(VmError::new(
                    "time.format_date takes 2 arguments (date, pattern)".into(),
                ));
            }
            let d = extract_date(&args[0])?;
            let Value::String(pattern) = &args[1] else {
                return Err(VmError::new(
                    "time.format_date requires a String pattern".into(),
                ));
            };
            Ok(Value::String(d.format(pattern).to_string()))
        }

        "parse" => {
            if args.len() != 2 {
                return Err(VmError::new(
                    "time.parse takes 2 arguments (string, pattern)".into(),
                ));
            }
            let (Value::String(s), Value::String(pattern)) = (&args[0], &args[1]) else {
                return Err(VmError::new("time.parse requires String arguments".into()));
            };
            match NaiveDateTime::parse_from_str(s, pattern) {
                Ok(dt) => Ok(Value::Variant("Ok".into(), vec![make_datetime(dt)])),
                Err(e) => Ok(Value::Variant(
                    "Err".into(),
                    vec![Value::String(format!("parse error: {e}"))],
                )),
            }
        }

        "parse_date" => {
            if args.len() != 2 {
                return Err(VmError::new(
                    "time.parse_date takes 2 arguments (string, pattern)".into(),
                ));
            }
            let (Value::String(s), Value::String(pattern)) = (&args[0], &args[1]) else {
                return Err(VmError::new(
                    "time.parse_date requires String arguments".into(),
                ));
            };
            // Parse as NaiveDateTime with a dummy time appended, then extract the date.
            let padded = format!("{s}T00:00:00");
            let padded_fmt = format!("{pattern}T%H:%M:%S");
            match NaiveDateTime::parse_from_str(&padded, &padded_fmt) {
                Ok(dt) => Ok(Value::Variant("Ok".into(), vec![make_date(dt.date())])),
                Err(_) => {
                    // Fallback: try direct NaiveDate parse (works on native)
                    match NaiveDate::parse_from_str(s, pattern) {
                        Ok(d) => Ok(Value::Variant("Ok".into(), vec![make_date(d)])),
                        Err(e) => Ok(Value::Variant(
                            "Err".into(),
                            vec![Value::String(format!("parse error: {e}"))],
                        )),
                    }
                }
            }
        }

        "add_days" => {
            if args.len() != 2 {
                return Err(VmError::new(
                    "time.add_days takes 2 arguments (date, days)".into(),
                ));
            }
            let d = extract_date(&args[0])?;
            let Value::Int(days) = &args[1] else {
                return Err(VmError::new("time.add_days requires Int days".into()));
            };
            let result = d + chrono::Duration::days(*days);
            Ok(make_date(result))
        }

        "add_months" => {
            if args.len() != 2 {
                return Err(VmError::new(
                    "time.add_months takes 2 arguments (date, months)".into(),
                ));
            }
            let d = extract_date(&args[0])?;
            let Value::Int(months) = &args[1] else {
                return Err(VmError::new("time.add_months requires Int months".into()));
            };
            let months = *months;
            // Calculate target year and month
            let total_months = d.year() as i64 * 12 + (d.month() as i64 - 1) + months;
            let target_year = (total_months.div_euclid(12)) as i32;
            let target_month = (total_months.rem_euclid(12) + 1) as u32;
            // Clamp day to last valid day of target month
            let max_day = days_in_month(target_year, target_month);
            let target_day = d.day().min(max_day);
            let result = NaiveDate::from_ymd_opt(target_year, target_month, target_day)
                .ok_or_else(|| {
                    VmError::new(format!(
                        "add_months overflow: {target_year}-{target_month}-{target_day}"
                    ))
                })?;
            Ok(make_date(result))
        }

        "add" => {
            if args.len() != 2 {
                return Err(VmError::new(
                    "time.add takes 2 arguments (instant, duration)".into(),
                ));
            }
            let epoch_ns = extract_instant(&args[0])?;
            let dur_ns = extract_duration(&args[1])?;
            Ok(make_instant(epoch_ns + dur_ns))
        }

        "since" => {
            if args.len() != 2 {
                return Err(VmError::new(
                    "time.since takes 2 arguments (from, to)".into(),
                ));
            }
            let from_ns = extract_instant(&args[0])?;
            let to_ns = extract_instant(&args[1])?;
            Ok(make_duration(to_ns - from_ns))
        }

        "hours" => {
            if args.len() != 1 {
                return Err(VmError::new("time.hours takes 1 argument".into()));
            }
            let Value::Int(n) = &args[0] else {
                return Err(VmError::new("time.hours requires an Int".into()));
            };
            Ok(make_duration(*n * 3_600_000_000_000))
        }

        "minutes" => {
            if args.len() != 1 {
                return Err(VmError::new("time.minutes takes 1 argument".into()));
            }
            let Value::Int(n) = &args[0] else {
                return Err(VmError::new("time.minutes requires an Int".into()));
            };
            Ok(make_duration(*n * 60_000_000_000))
        }

        "seconds" => {
            if args.len() != 1 {
                return Err(VmError::new("time.seconds takes 1 argument".into()));
            }
            let Value::Int(n) = &args[0] else {
                return Err(VmError::new("time.seconds requires an Int".into()));
            };
            Ok(make_duration(*n * 1_000_000_000))
        }

        "ms" => {
            if args.len() != 1 {
                return Err(VmError::new("time.ms takes 1 argument".into()));
            }
            let Value::Int(n) = &args[0] else {
                return Err(VmError::new("time.ms requires an Int".into()));
            };
            Ok(make_duration(*n * 1_000_000))
        }

        "weekday" => {
            if args.len() != 1 {
                return Err(VmError::new("time.weekday takes 1 argument (date)".into()));
            }
            let d = extract_date(&args[0])?;
            let day_name = match d.weekday() {
                Weekday::Mon => "Monday",
                Weekday::Tue => "Tuesday",
                Weekday::Wed => "Wednesday",
                Weekday::Thu => "Thursday",
                Weekday::Fri => "Friday",
                Weekday::Sat => "Saturday",
                Weekday::Sun => "Sunday",
            };
            Ok(Value::Variant(day_name.into(), vec![]))
        }

        "days_between" => {
            if args.len() != 2 {
                return Err(VmError::new(
                    "time.days_between takes 2 arguments (from, to)".into(),
                ));
            }
            let from = extract_date(&args[0])?;
            let to = extract_date(&args[1])?;
            let diff = to.signed_duration_since(from).num_days();
            Ok(Value::Int(diff))
        }

        "days_in_month" => {
            if args.len() != 2 {
                return Err(VmError::new(
                    "time.days_in_month takes 2 arguments (year, month)".into(),
                ));
            }
            let (Value::Int(y), Value::Int(m)) = (&args[0], &args[1]) else {
                return Err(VmError::new(
                    "time.days_in_month requires Int arguments".into(),
                ));
            };
            Ok(Value::Int(days_in_month(*y as i32, *m as u32) as i64))
        }

        "is_leap_year" => {
            if args.len() != 1 {
                return Err(VmError::new("time.is_leap_year takes 1 argument".into()));
            }
            let Value::Int(y) = &args[0] else {
                return Err(VmError::new("time.is_leap_year requires an Int".into()));
            };
            let y = *y as i32;
            let leap = (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0);
            Ok(Value::Bool(leap))
        }

        "sleep" => {
            if args.len() != 1 {
                return Err(VmError::new(
                    "time.sleep takes 1 argument (duration)".into(),
                ));
            }
            let dur_ns = extract_duration(&args[0])?;
            if dur_ns <= 0 {
                return Ok(Value::Unit);
            }
            let dur_ms = dur_ns / 1_000_000;
            let target_ms = vm.epoch_ms()? + dur_ms;
            while vm.epoch_ms()? < target_ms {
                let remaining_ms = target_ms - vm.epoch_ms()?;
                if remaining_ms <= 0 {
                    break;
                }
                // On native, sleep in small increments to stay responsive
                #[cfg(not(target_arch = "wasm32"))]
                std::thread::sleep(std::time::Duration::from_millis(
                    (remaining_ms as u64).min(1),
                ));
            }
            Ok(Value::Unit)
        }

        _ => Err(VmError::new(format!("unknown time function: {name}"))),
    }
}

// ── HTTP dispatch ───────────────────────────────────────────────────

#[cfg(feature = "http")]
fn make_http_response(
    status: u16,
    headers: BTreeMap<Value, Value>,
    body: std::string::String,
) -> Value {
    let mut fields = BTreeMap::new();
    fields.insert("status".into(), Value::Int(status as i64));
    fields.insert("body".into(), Value::String(body));
    fields.insert("headers".into(), Value::Map(Arc::new(headers)));
    Value::Record("Response".into(), Arc::new(fields))
}

#[cfg(feature = "http")]
fn make_http_request_value(
    method: &str,
    path: &str,
    query: &str,
    headers: BTreeMap<Value, Value>,
    body: std::string::String,
) -> Value {
    let mut fields = BTreeMap::new();
    fields.insert("method".into(), Value::Variant(method.into(), vec![]));
    fields.insert("path".into(), Value::String(path.into()));
    fields.insert("query".into(), Value::String(query.into()));
    fields.insert("headers".into(), Value::Map(Arc::new(headers)));
    fields.insert("body".into(), Value::String(body));
    Value::Record("Request".into(), Arc::new(fields))
}

#[cfg(feature = "http")]
fn extract_http_response(
    val: &Value,
) -> Result<
    (
        u16,
        std::string::String,
        &BTreeMap<std::string::String, Value>,
    ),
    VmError,
> {
    let Value::Record(name, fields) = val else {
        return Err(VmError::new("handler must return a Response record".into()));
    };
    if name != "Response" {
        return Err(VmError::new(format!(
            "handler must return Response, got {name}"
        )));
    }
    let status = match fields.get("status") {
        Some(Value::Int(n)) => *n as u16,
        _ => return Err(VmError::new("Response.status must be an Int".into())),
    };
    let body = match fields.get("body") {
        Some(Value::String(s)) => s.clone(),
        _ => return Err(VmError::new("Response.body must be a String".into())),
    };
    Ok((status, body, fields))
}

#[cfg(feature = "http")]
fn ureq_response_to_value(
    mut response: ureq::http::Response<ureq::Body>,
) -> Result<Value, VmError> {
    let status = response.status().as_u16();
    let mut headers = BTreeMap::new();
    for (name, value) in response.headers().iter() {
        if let Ok(v) = value.to_str() {
            headers.insert(
                Value::String(name.as_str().to_string()),
                Value::String(v.to_string()),
            );
        }
    }
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|e| VmError::new(format!("http: failed to read body: {e}")))?;
    Ok(make_http_response(status, headers, body))
}

/// Dispatch `http.<name>(args)`.
pub fn call_http(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "get" => {
            #[cfg(feature = "http")]
            {
                if args.len() != 1 {
                    return Err(VmError::new("http.get takes 1 argument (url)".into()));
                }
                let Value::String(url) = &args[0] else {
                    return Err(VmError::new("http.get requires a String url".into()));
                };
                let agent: ureq::Agent = ureq::Agent::config_builder()
                    .http_status_as_error(false)
                    .build()
                    .into();
                match agent.get(url).call() {
                    Ok(response) => {
                        let resp = ureq_response_to_value(response)?;
                        Ok(Value::Variant("Ok".into(), vec![resp]))
                    }
                    Err(e) => Ok(Value::Variant(
                        "Err".into(),
                        vec![Value::String(format!("{e}"))],
                    )),
                }
            }
            #[cfg(not(feature = "http"))]
            {
                let _ = args;
                Err(VmError::new("http.get requires the 'http' feature".into()))
            }
        }

        "request" => {
            #[cfg(feature = "http")]
            {
                if args.len() != 4 {
                    return Err(VmError::new(
                        "http.request takes 4 arguments (method, url, body, headers)".into(),
                    ));
                }
                let Value::Variant(method_tag, method_args) = &args[0] else {
                    return Err(VmError::new(
                        "http.request: first argument must be a Method".into(),
                    ));
                };
                if !method_args.is_empty() {
                    return Err(VmError::new("http.request: invalid Method variant".into()));
                }
                let Value::String(url) = &args[1] else {
                    return Err(VmError::new("http.request: url must be a String".into()));
                };
                let Value::String(body) = &args[2] else {
                    return Err(VmError::new("http.request: body must be a String".into()));
                };
                let Value::Map(header_map) = &args[3] else {
                    return Err(VmError::new("http.request: headers must be a Map".into()));
                };

                let agent: ureq::Agent = ureq::Agent::config_builder()
                    .http_status_as_error(false)
                    .build()
                    .into();

                // Methods split into WithBody (POST/PUT/PATCH) and WithoutBody (GET/DELETE/HEAD/OPTIONS)
                let result = match method_tag.as_str() {
                    "POST" => {
                        let mut req = agent.post(url);
                        for (k, v) in header_map.iter() {
                            if let (Value::String(key), Value::String(val)) = (k, v) {
                                req = req.header(key.as_str(), val.as_str());
                            }
                        }
                        if body.is_empty() {
                            req.send_empty()
                        } else {
                            req.send(body.as_str())
                        }
                    }
                    "PUT" => {
                        let mut req = agent.put(url);
                        for (k, v) in header_map.iter() {
                            if let (Value::String(key), Value::String(val)) = (k, v) {
                                req = req.header(key.as_str(), val.as_str());
                            }
                        }
                        if body.is_empty() {
                            req.send_empty()
                        } else {
                            req.send(body.as_str())
                        }
                    }
                    "PATCH" => {
                        let mut req = agent.patch(url);
                        for (k, v) in header_map.iter() {
                            if let (Value::String(key), Value::String(val)) = (k, v) {
                                req = req.header(key.as_str(), val.as_str());
                            }
                        }
                        if body.is_empty() {
                            req.send_empty()
                        } else {
                            req.send(body.as_str())
                        }
                    }
                    "GET" => {
                        let mut req = agent.get(url);
                        for (k, v) in header_map.iter() {
                            if let (Value::String(key), Value::String(val)) = (k, v) {
                                req = req.header(key.as_str(), val.as_str());
                            }
                        }
                        req.call()
                    }
                    "DELETE" => {
                        let mut req = agent.delete(url);
                        for (k, v) in header_map.iter() {
                            if let (Value::String(key), Value::String(val)) = (k, v) {
                                req = req.header(key.as_str(), val.as_str());
                            }
                        }
                        req.call()
                    }
                    "HEAD" => {
                        let mut req = agent.head(url);
                        for (k, v) in header_map.iter() {
                            if let (Value::String(key), Value::String(val)) = (k, v) {
                                req = req.header(key.as_str(), val.as_str());
                            }
                        }
                        req.call()
                    }
                    "OPTIONS" => {
                        let mut req = agent.options(url);
                        for (k, v) in header_map.iter() {
                            if let (Value::String(key), Value::String(val)) = (k, v) {
                                req = req.header(key.as_str(), val.as_str());
                            }
                        }
                        req.call()
                    }
                    other => {
                        return Err(VmError::new(format!(
                            "http.request: unknown method: {other}"
                        )));
                    }
                };

                match result {
                    Ok(response) => {
                        let resp = ureq_response_to_value(response)?;
                        Ok(Value::Variant("Ok".into(), vec![resp]))
                    }
                    Err(e) => Ok(Value::Variant(
                        "Err".into(),
                        vec![Value::String(format!("{e}"))],
                    )),
                }
            }
            #[cfg(not(feature = "http"))]
            {
                let _ = args;
                Err(VmError::new(
                    "http.request requires the 'http' feature".into(),
                ))
            }
        }

        "serve" => {
            #[cfg(feature = "http")]
            {
                if args.len() != 2 {
                    return Err(VmError::new(
                        "http.serve takes 2 arguments (port, handler)".into(),
                    ));
                }
                let Value::Int(port) = &args[0] else {
                    return Err(VmError::new("http.serve: port must be an Int".into()));
                };
                let handler = args[1].clone();

                let addr = format!("0.0.0.0:{port}");
                let server = tiny_http::Server::http(&addr)
                    .map_err(|e| VmError::new(format!("http.serve: failed to bind: {e}")))?;

                loop {
                    let mut req = match server.recv() {
                        Ok(req) => req,
                        Err(e) => {
                            return Err(VmError::new(format!("http.serve: recv error: {e}")));
                        }
                    };

                    // Convert method to silt Method variant
                    let method_str = match req.method() {
                        tiny_http::Method::Get => "GET",
                        tiny_http::Method::Post => "POST",
                        tiny_http::Method::Put => "PUT",
                        tiny_http::Method::Patch => "PATCH",
                        tiny_http::Method::Delete => "DELETE",
                        tiny_http::Method::Head => "HEAD",
                        tiny_http::Method::Options => "OPTIONS",
                        _ => {
                            let resp = tiny_http::Response::from_string("Method Not Allowed")
                                .with_status_code(tiny_http::StatusCode(405));
                            let _ = req.respond(resp);
                            continue;
                        }
                    };

                    // Parse URL into path and query
                    let url = req.url().to_string();
                    let (path, query) = match url.split_once('?') {
                        Some((p, q)) => (p.to_string(), q.to_string()),
                        None => (url, std::string::String::new()),
                    };

                    // Collect headers
                    let mut headers = BTreeMap::new();
                    for header in req.headers() {
                        headers.insert(
                            Value::String(header.field.as_str().to_string()),
                            Value::String(header.value.as_str().to_string()),
                        );
                    }

                    // Read body
                    let mut body = std::string::String::new();
                    let _ = std::io::Read::read_to_string(req.as_reader(), &mut body);

                    // Build Request record
                    let request_val =
                        make_http_request_value(method_str, &path, &query, headers, body);

                    // Call handler
                    let response_val = vm.invoke_callable(&handler, &[request_val])?;

                    // Extract Response record and send
                    let (status, resp_body, resp_fields) = extract_http_response(&response_val)?;

                    let mut response = tiny_http::Response::from_string(&resp_body)
                        .with_status_code(tiny_http::StatusCode(status));

                    if let Some(Value::Map(resp_headers)) = resp_fields.get("headers") {
                        for (k, v) in resp_headers.iter() {
                            if let (Value::String(key), Value::String(val)) = (k, v)
                                && let Ok(header) =
                                    tiny_http::Header::from_bytes(key.as_bytes(), val.as_bytes())
                            {
                                response = response.with_header(header);
                            }
                        }
                    }

                    let _ = req.respond(response);
                }
            }
            #[cfg(not(feature = "http"))]
            {
                let _ = args;
                Err(VmError::new(
                    "http.serve requires the 'http' feature".into(),
                ))
            }
        }

        "segments" => {
            if args.len() != 1 {
                return Err(VmError::new("http.segments takes 1 argument (path)".into()));
            }
            let Value::String(path) = &args[0] else {
                return Err(VmError::new("http.segments requires a String".into()));
            };
            let segments: Vec<Value> = path
                .split('/')
                .filter(|s| !s.is_empty())
                .map(|s| Value::String(s.to_string()))
                .collect();
            Ok(Value::List(Arc::new(segments)))
        }

        _ => Err(VmError::new(format!("unknown http function: {name}"))),
    }
}
