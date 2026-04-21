//! Data‐format and external‐service builtin functions
//! (`regex.*`, `json.*`, `time.*`, `http.*`).

use std::collections::BTreeMap;
use std::sync::Arc;
#[cfg(feature = "http")]
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(feature = "http")]
use std::time::Duration;

use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Weekday};

use crate::value::{IoCompletion, TaskHandle, Value, checked_range_len};
use crate::vm::{BlockReason, BuiltinIterKind, Vm, VmError};

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

fn value_to_json(v: &Value) -> Result<serde_json::Value, VmError> {
    Ok(match v {
        Value::Int(n) => serde_json::Value::Number((*n).into()),
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::ExtFloat(f) if f.is_finite() => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::ExtFloat(_) => serde_json::Value::Null,
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
        Value::Variant(name, fields) if name == "None" && fields.is_empty() => {
            serde_json::Value::Null
        }
        Value::Variant(name, fields) if name == "Some" && fields.len() == 1 => {
            value_to_json(&fields[0])?
        }
        Value::Variant(name, fields) => {
            let mut obj = serde_json::Map::new();
            obj.insert("variant".into(), serde_json::Value::String(name.clone()));
            if !fields.is_empty() {
                let items: Result<Vec<_>, _> = fields.iter().map(value_to_json).collect();
                obj.insert("fields".into(), serde_json::Value::Array(items?));
            }
            serde_json::Value::Object(obj)
        }
        Value::Unit => serde_json::Value::Null,
        Value::VariantConstructor(name, _) => serde_json::Value::String(name.clone()),
        _ => serde_json::Value::Null,
    })
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

/// Convert a Silt `Int` field on a record to an `i32`, rejecting
/// values that don't fit with a clean `VmError`. `default` is used
/// when the field is missing. Previously this was done via `as i32`
/// casts that silently truncated, letting `year = u32::MAX + 1999`
/// wrap to `1999` inside `NaiveDate::from_ymd_opt`.
fn field_as_i32(
    fields: &BTreeMap<String, Value>,
    name: &str,
    default: i32,
) -> Result<i32, VmError> {
    match fields.get(name) {
        Some(Value::Int(n)) => i32::try_from(*n)
            .map_err(|_| VmError::new(format!("time: {name} {n} out of range for i32"))),
        _ => Ok(default),
    }
}

/// Same as [`field_as_i32`] but for `u32`-typed components (month,
/// day, hour, minute, second, nanosecond). Silently truncating
/// `hour = u32::MAX + 9` to `9` previously let bogus timestamps
/// slip past `NaiveTime::from_hms_nano_opt`'s validation.
fn field_as_u32(
    fields: &BTreeMap<String, Value>,
    name: &str,
    default: u32,
) -> Result<u32, VmError> {
    match fields.get(name) {
        Some(Value::Int(n)) => u32::try_from(*n)
            .map_err(|_| VmError::new(format!("time: {name} {n} out of range for u32"))),
        _ => Ok(default),
    }
}

/// What kind of value is being formatted — determines which strftime
/// specifiers are compatible with the receiver.
#[derive(Debug, Clone, Copy)]
enum StrftimeReceiver {
    /// A `NaiveDate` — rejects any specifier that requires a time
    /// component (`%H`, `%M`, `%S`, etc.) or timezone (`%z`, `%Z`).
    Date,
    /// A `NaiveDateTime` — has date + time, but no timezone, so
    /// timezone specifiers (`%z`, `%Z`) still can't render.
    DateTime,
}

/// Validate a chrono strftime pattern before calling `format()` on
/// it. Chrono's `Display` impl for `DelayedFormat` writes to the
/// formatter and calls `panic!("a Display implementation returned an
/// error unexpectedly")` whenever the pattern contains:
///   1. An unknown specifier like `%Q` (yields `Item::Error`), or
///   2. A valid specifier that the receiver can't render — e.g.
///      `%H` on a `NaiveDate` (no time component) or `%z` on a
///      `NaiveDateTime` (naive = no TZ).
///
/// That panic is caught by our `catch_builtin_panic` wrapper, but the
/// default panic hook still writes a 3-line "thread 'main' panicked"
/// notice to stderr before the recovery. We classify each parsed
/// `Item` against the receiver type and surface a clean error up
/// front so no panic is ever raised.
fn validate_strftime_pattern(
    fn_name: &str,
    pattern: &str,
    receiver: StrftimeReceiver,
) -> Result<(), VmError> {
    use chrono::format::{Fixed, Item, Numeric, StrftimeItems};

    for item in StrftimeItems::new(pattern) {
        match item {
            Item::Error => {
                return Err(VmError::new(format!(
                    "{fn_name}: invalid format specifier in '{pattern}'"
                )));
            }
            Item::Numeric(ref n, _) => {
                // Time-component numeric specifiers cannot render
                // against a bare Date. Everything else (year, month,
                // day, week, ordinal, etc.) is date-level and safe.
                let is_time_only = matches!(
                    n,
                    Numeric::Hour
                        | Numeric::Hour12
                        | Numeric::Minute
                        | Numeric::Second
                        | Numeric::Nanosecond
                        | Numeric::Timestamp
                );
                if is_time_only && matches!(receiver, StrftimeReceiver::Date) {
                    return Err(VmError::new(format!(
                        "{fn_name}: time specifier in '{pattern}' is not \
                         valid for a Date; use time.format with a DateTime instead"
                    )));
                }
            }
            Item::Fixed(ref fx) => {
                // Time-only fixed specifiers.
                let is_time_only = matches!(
                    fx,
                    Fixed::LowerAmPm
                        | Fixed::UpperAmPm
                        | Fixed::Nanosecond
                        | Fixed::Nanosecond3
                        | Fixed::Nanosecond6
                        | Fixed::Nanosecond9
                );
                if is_time_only && matches!(receiver, StrftimeReceiver::Date) {
                    return Err(VmError::new(format!(
                        "{fn_name}: time specifier in '{pattern}' is not \
                         valid for a Date; use time.format with a DateTime instead"
                    )));
                }
                // Timezone specifiers: NaiveDate has no time AND no
                // TZ; NaiveDateTime has no TZ. Reject for both.
                let is_tz = matches!(
                    fx,
                    Fixed::TimezoneName
                        | Fixed::TimezoneOffset
                        | Fixed::TimezoneOffsetColon
                        | Fixed::TimezoneOffsetDoubleColon
                        | Fixed::TimezoneOffsetTripleColon
                        | Fixed::TimezoneOffsetColonZ
                        | Fixed::TimezoneOffsetZ
                        | Fixed::RFC2822
                        | Fixed::RFC3339
                );
                if is_tz {
                    let what = match receiver {
                        StrftimeReceiver::Date => "Date",
                        StrftimeReceiver::DateTime => "naive DateTime",
                    };
                    return Err(VmError::new(format!(
                        "{fn_name}: timezone specifier in '{pattern}' is not \
                         valid for a {what}; silt DateTimes are naive (no TZ)"
                    )));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Extract a NaiveDate from a Silt Date record.
fn extract_date(v: &Value) -> Result<NaiveDate, VmError> {
    let Value::Record(name, fields) = v else {
        return Err(VmError::new("expected a Date record".into()));
    };
    if name != "Date" {
        return Err(VmError::new(format!("expected Date, got {name}")));
    }
    let y = field_as_i32(fields, "year", 0)?;
    let m = field_as_u32(fields, "month", 1)?;
    let d = field_as_u32(fields, "day", 1)?;
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
    let h = field_as_u32(fields, "hour", 0)?;
    let m = field_as_u32(fields, "minute", 0)?;
    let s = field_as_u32(fields, "second", 0)?;
    let ns = field_as_u32(fields, "ns", 0)?;
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
pub(crate) fn extract_duration(v: &Value) -> Result<i64, VmError> {
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
pub(crate) fn load_record_fields(
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
                records.push(inner.into_iter().next().expect("guard guarantees len==1"));
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
                    // Mirror the `float.to_int` range check (B7). A bare
                    // `f as i64` would saturate to i64::MAX/MIN silently,
                    // turning large JSON numbers like 1e100 into i64::MAX —
                    // a data-corruption hazard. Reject values that aren't
                    // finite or don't fit exactly in the i64 range.
                    const I64_MIN_AS_F64: f64 = i64::MIN as f64;
                    const I64_MAX_PLUS_ONE: f64 = 9223372036854775808.0; // exact
                    if !f.is_finite() || !(I64_MIN_AS_F64..I64_MAX_PLUS_ONE).contains(&f) {
                        return Err(VmError::new(format!(
                            "json.parse({parent_type}): field '{field_name}': number {f} out of Int range"
                        )));
                    }
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
            // Materialize match spans and match texts.  Spans are re-derived
            // deterministically from (pattern, text) on resume so we don't
            // need to persist them.
            let re = Vm::get_regex(&mut vm.regex_cache, pattern)?.clone();
            let mut spans: Vec<(usize, usize)> = Vec::new();
            let mut items: Vec<Value> = Vec::new();
            for m in re.find_iter(text) {
                spans.push((m.start(), m.end()));
                items.push(Value::String(m.as_str().to_string()));
            }
            // Use iterate_builtin with ListMap semantics to collect the
            // replacement strings, with correct yield/resume handling.
            let replacements_val =
                vm.iterate_builtin(BuiltinIterKind::ListMap, items, args[2].clone(), args)?;
            let Value::List(replacements) = replacements_val else {
                return Err(VmError::new(
                    "internal: regex.replace_all_with iterate_builtin returned non-list".into(),
                ));
            };
            // Validate that all callback results are strings.
            for val in replacements.iter() {
                if !matches!(val, Value::String(_)) {
                    return Err(VmError::new(
                        "regex.replace_all_with callback must return a string".into(),
                    ));
                }
            }
            // Interleave text slices and replacements.
            let Value::String(text_string) = &args[1] else {
                unreachable!();
            };
            let mut result = std::string::String::new();
            let mut last_end = 0;
            for ((start, end), replacement) in spans.iter().zip(replacements.iter()) {
                result.push_str(&text_string[last_end..*start]);
                if let Value::String(s) = replacement {
                    result.push_str(s);
                }
                last_end = *end;
            }
            result.push_str(&text_string[last_end..]);
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
        "captures_named" => {
            if args.len() != 2 {
                return Err(VmError::new(
                    "regex.captures_named takes 2 arguments (pattern, text)".into(),
                ));
            }
            let (Value::String(pattern), Value::String(text)) = (&args[0], &args[1]) else {
                return Err(VmError::new(
                    "regex.captures_named requires string arguments".into(),
                ));
            };
            let re = Vm::get_regex(&mut vm.regex_cache, pattern)?;
            // `capture_names()` yields one entry per group, including the
            // implicit whole-match group at index 0 (whose name is
            // `None`) and any numbered-only groups (also `None`). We
            // count only the *named* entries to decide whether the
            // pattern is "nameless" — if so, the contract says `None`.
            let named_count = re.capture_names().flatten().count();
            if named_count == 0 {
                return Ok(Value::Variant("None".into(), Vec::new()));
            }
            let Some(caps) = re.captures(text) else {
                return Ok(Value::Variant("None".into(), Vec::new()));
            };
            // Collect (name → match) pairs. Skip any named group that
            // did not participate in the match — per the spec we omit
            // it entirely rather than mapping to "".
            let mut out: BTreeMap<Value, Value> = BTreeMap::new();
            for name in re.capture_names().flatten() {
                if let Some(m) = caps.name(name) {
                    out.insert(
                        Value::String(name.to_string()),
                        Value::String(m.as_str().to_string()),
                    );
                }
            }
            Ok(Value::Variant(
                "Some".into(),
                vec![Value::Map(Arc::new(out))],
            ))
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
                    "json.parse takes 2 arguments: (String, type a)".into(),
                ));
            }
            let Value::String(s) = &args[0] else {
                return Err(VmError::new(
                    "json.parse: first argument must be a string".into(),
                ));
            };
            let s = s.clone();
            let Value::TypeDescriptor(type_name) = &args[1] else {
                return Err(VmError::new(
                    "json.parse: type argument must be a record type".into(),
                ));
            };
            let type_name = type_name.clone();
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
                    "json.parse_list takes 2 arguments: (String, type a)".into(),
                ));
            }
            let Value::String(s) = &args[0] else {
                return Err(VmError::new(
                    "json.parse_list: first argument must be a string".into(),
                ));
            };
            let s = s.clone();
            let Value::TypeDescriptor(type_name) = &args[1] else {
                return Err(VmError::new(
                    "json.parse_list: type argument must be a record type".into(),
                ));
            };
            let type_name = type_name.clone();
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
                    "json.parse_map takes 2 arguments: (String, type v)".into(),
                ));
            }
            let Value::String(s) = &args[0] else {
                return Err(VmError::new(
                    "json.parse_map: first argument must be a string".into(),
                ));
            };
            let s = s.clone();
            let value_type = match &args[1] {
                Value::PrimitiveDescriptor(name) => name.clone(),
                Value::TypeDescriptor(name) => name.clone(),
                _ => return Err(VmError::new(
                    "json.parse_map: type argument must be a type (Int, Float, String, Bool, or a record type)".into()
                )),
            };
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
            let j = value_to_json(&args[0])?;
            Ok(Value::String(j.to_string()))
        }
        "pretty" => {
            if args.len() != 1 {
                return Err(VmError::new("json.pretty takes 1 argument".into()));
            }
            let j = value_to_json(&args[0])?;
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
            let epoch_ms = vm.epoch_ms()?;
            let epoch_ns = epoch_ms.checked_mul(1_000_000).ok_or_else(|| {
                VmError::new("time arithmetic overflow: time.now epoch_ms * 1_000_000".into())
            })?;
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
            // Reject silently-truncated `as i32`/`as u32` values: a
            // year of `u32::MAX + 1999` used to silently wrap to 1999.
            let y32 = i32::try_from(*y)
                .map_err(|_| VmError::new(format!("time.date: year {y} out of range for i32")))?;
            let m32 = u32::try_from(*m)
                .map_err(|_| VmError::new(format!("time.date: month {m} out of range for u32")))?;
            let d32 = u32::try_from(*d)
                .map_err(|_| VmError::new(format!("time.date: day {d} out of range for u32")))?;
            match NaiveDate::from_ymd_opt(y32, m32, d32) {
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
            let h32 = u32::try_from(*h)
                .map_err(|_| VmError::new(format!("time.time: hour {h} out of range for u32")))?;
            let m32 = u32::try_from(*m)
                .map_err(|_| VmError::new(format!("time.time: minute {m} out of range for u32")))?;
            let s32 = u32::try_from(*s)
                .map_err(|_| VmError::new(format!("time.time: second {s} out of range for u32")))?;
            match NaiveTime::from_hms_opt(h32, m32, s32) {
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
            // Rust `i64 % i64` carries the sign of the dividend, so for
            // negative `epoch_ns` whose magnitude isn't a multiple of 1e9
            // the remainder is negative; casting to `u32` wraps it to a
            // huge value and chrono then rejects the instant. Use
            // div_euclid/rem_euclid so the remainder is always in
            // `[0, 1_000_000_000)` and seconds round toward negative
            // infinity, which matches chrono's own expectations.
            let epoch_secs = epoch_ns.div_euclid(1_000_000_000);
            let nano_remainder = epoch_ns.rem_euclid(1_000_000_000) as u32;
            let utc_dt = DateTime::from_timestamp(epoch_secs, nano_remainder)
                .ok_or_else(|| VmError::new("instant out of range".into()))?
                .naive_utc();
            // `chrono::Duration::minutes(i64)` panics when the value is
            // outside a roughly `i64::MAX / 60000` window. Use the
            // fallible constructor so a pathological offset surfaces as
            // a clean VmError rather than a builtin panic.
            let offset = chrono::Duration::try_minutes(*offset_min).ok_or_else(|| {
                VmError::new(format!(
                    "time.to_datetime: offset {offset_min} minutes out of range"
                ))
            })?;
            // Even a valid chrono::Duration can still push the naive
            // datetime past chrono's ±262143-year range; `NaiveDateTime
            // + Duration` panics on overflow, so use the checked form.
            // (In practice `Instant.epoch_ns` is an i64, so the combined
            // epoch-ns + i32-minute-offset input cannot reach chrono's
            // ±262143-year boundary from Silt user code — we keep the
            // check as defence in depth against future Instant
            // widenings or chrono internal assumption changes.)
            let local_dt = utc_dt.checked_add_signed(offset).ok_or_else(|| {
                VmError::new("time.to_datetime: datetime + offset out of range".into())
            })?;
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
            let offset = chrono::Duration::try_minutes(*offset_min).ok_or_else(|| {
                VmError::new(format!(
                    "time.to_instant: offset {offset_min} minutes out of range"
                ))
            })?;
            // `NaiveDateTime - Duration` panics on overflow (chrono's
            // valid range is ±262143 years). Use the checked form so a
            // pathological offset/datetime combination surfaces as a
            // clean VmError.
            let utc_dt = dt.checked_sub_signed(offset).ok_or_else(|| {
                VmError::new("time.to_instant: datetime - offset out of range".into())
            })?;
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
            // See `to_datetime` above: signed `%` on negative epoch_ns
            // yields a negative remainder, which `as u32` wraps into a
            // huge value and chrono then rejects. div_euclid/rem_euclid
            // keep the remainder in `[0, 1_000_000_000)` unconditionally.
            let epoch_secs = epoch_ns.div_euclid(1_000_000_000);
            let nano_remainder = epoch_ns.rem_euclid(1_000_000_000) as u32;
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
            validate_strftime_pattern("time.format", pattern, StrftimeReceiver::DateTime)?;
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
            validate_strftime_pattern("time.format_date", pattern, StrftimeReceiver::Date)?;
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
            // chrono::Duration::days panics when `days * 86_400_000` overflows
            // i64 milliseconds (i.e. for inputs beyond roughly ±106_751_991 days).
            // We reject such values up front so the panic can never escape
            // the builtin. Further, `NaiveDate::checked_add_signed` returns
            // None for out-of-range dates (chrono's valid range spans ±262_143
            // years).  In both failure modes we produce a clean VmError.
            const MAX_DAYS: i64 = 100_000_000; // safely below chrono's panic threshold
            if days.unsigned_abs() > MAX_DAYS as u64 {
                return Err(VmError::new(format!(
                    "time arithmetic overflow: time.add_days days={days} out of range"
                )));
            }
            let delta = chrono::Duration::days(*days);
            let result = d.checked_add_signed(delta).ok_or_else(|| {
                VmError::new(format!(
                    "time arithmetic overflow: time.add_days result out of range for {d} + {days} days"
                ))
            })?;
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
            // Calculate target year and month using checked arithmetic so
            // extreme `months` inputs (e.g. i64::MAX) don't panic in debug
            // builds or silently wrap in release builds.
            let base_year = d.year() as i64;
            let total_months = base_year
                .checked_mul(12)
                .and_then(|y| y.checked_add(d.month() as i64 - 1))
                .and_then(|m| m.checked_add(months))
                .ok_or_else(|| {
                    VmError::new(format!(
                        "time arithmetic overflow: time.add_months months={months} out of range"
                    ))
                })?;
            let target_year_i64 = total_months.div_euclid(12);
            // Cast to i32 only after verifying it fits.
            if target_year_i64 < i32::MIN as i64 || target_year_i64 > i32::MAX as i64 {
                return Err(VmError::new(format!(
                    "time arithmetic overflow: time.add_months target year {target_year_i64} out of i32 range"
                )));
            }
            let target_year = target_year_i64 as i32;
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
            let result = epoch_ns.checked_add(dur_ns).ok_or_else(|| {
                VmError::new("time arithmetic overflow: time.add instant + duration".into())
            })?;
            Ok(make_instant(result))
        }

        "since" => {
            if args.len() != 2 {
                return Err(VmError::new(
                    "time.since takes 2 arguments (from, to)".into(),
                ));
            }
            let from_ns = extract_instant(&args[0])?;
            let to_ns = extract_instant(&args[1])?;
            let result = to_ns.checked_sub(from_ns).ok_or_else(|| {
                VmError::new("time arithmetic overflow: time.since to - from".into())
            })?;
            Ok(make_duration(result))
        }

        "hours" => {
            if args.len() != 1 {
                return Err(VmError::new("time.hours takes 1 argument".into()));
            }
            let Value::Int(n) = &args[0] else {
                return Err(VmError::new("time.hours requires an Int".into()));
            };
            let ns = n.checked_mul(3_600_000_000_000).ok_or_else(|| {
                VmError::new(format!(
                    "time arithmetic overflow: time.hours({n}) exceeds i64 nanoseconds"
                ))
            })?;
            Ok(make_duration(ns))
        }

        "minutes" => {
            if args.len() != 1 {
                return Err(VmError::new("time.minutes takes 1 argument".into()));
            }
            let Value::Int(n) = &args[0] else {
                return Err(VmError::new("time.minutes requires an Int".into()));
            };
            let ns = n.checked_mul(60_000_000_000).ok_or_else(|| {
                VmError::new(format!(
                    "time arithmetic overflow: time.minutes({n}) exceeds i64 nanoseconds"
                ))
            })?;
            Ok(make_duration(ns))
        }

        "seconds" => {
            if args.len() != 1 {
                return Err(VmError::new("time.seconds takes 1 argument".into()));
            }
            let Value::Int(n) = &args[0] else {
                return Err(VmError::new("time.seconds requires an Int".into()));
            };
            let ns = n.checked_mul(1_000_000_000).ok_or_else(|| {
                VmError::new(format!(
                    "time arithmetic overflow: time.seconds({n}) exceeds i64 nanoseconds"
                ))
            })?;
            Ok(make_duration(ns))
        }

        "ms" => {
            if args.len() != 1 {
                return Err(VmError::new("time.ms takes 1 argument".into()));
            }
            let Value::Int(n) = &args[0] else {
                return Err(VmError::new("time.ms requires an Int".into()));
            };
            let ns = n.checked_mul(1_000_000).ok_or_else(|| {
                VmError::new(format!(
                    "time arithmetic overflow: time.ms({n}) exceeds i64 nanoseconds"
                ))
            })?;
            Ok(make_duration(ns))
        }

        "micros" => {
            if args.len() != 1 {
                return Err(VmError::new("time.micros takes 1 argument".into()));
            }
            let Value::Int(n) = &args[0] else {
                return Err(VmError::new("time.micros requires an Int".into()));
            };
            let ns = n.checked_mul(1_000).ok_or_else(|| {
                VmError::new(format!(
                    "time arithmetic overflow: time.micros({n}) exceeds i64 nanoseconds"
                ))
            })?;
            Ok(make_duration(ns))
        }

        "nanos" => {
            if args.len() != 1 {
                return Err(VmError::new("time.nanos takes 1 argument".into()));
            }
            let Value::Int(n) = &args[0] else {
                return Err(VmError::new("time.nanos requires an Int".into()));
            };
            // No multiplication: the input is already in nanoseconds.
            // Still keep the pattern consistent with the siblings so
            // callers reading the dispatcher see the uniform shape.
            Ok(make_duration(*n))
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
            // Previously these were `*y as i32` / `*m as u32`, which
            // silently wrapped: `days_in_month(2024, u32::MAX + 2)`
            // returned 29. Require the arguments to fit.
            let y32 = i32::try_from(*y).map_err(|_| {
                VmError::new(format!("time.days_in_month: year {y} out of range for i32"))
            })?;
            let m32 = u32::try_from(*m).map_err(|_| {
                VmError::new(format!(
                    "time.days_in_month: month {m} out of range for u32"
                ))
            })?;
            Ok(Value::Int(days_in_month(y32, m32) as i64))
        }

        "is_leap_year" => {
            if args.len() != 1 {
                return Err(VmError::new("time.is_leap_year takes 1 argument".into()));
            }
            let Value::Int(y) = &args[0] else {
                return Err(VmError::new("time.is_leap_year requires an Int".into()));
            };
            let y32 = i32::try_from(*y).map_err(|_| {
                VmError::new(format!("time.is_leap_year: year {y} out of range for i32"))
            })?;
            let leap = (y32 % 4 == 0 && y32 % 100 != 0) || (y32 % 400 == 0);
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
            // Sync (non-task) call: block the caller thread. Correct
            // semantics on the main thread, and keeps tests/examples that
            // use `time.sleep` outside of a spawned task working.
            if !vm.is_scheduled_task {
                #[cfg(not(target_arch = "wasm32"))]
                std::thread::sleep(std::time::Duration::from_nanos(dur_ns as u64));
                return Ok(Value::Unit);
            }
            // Resume path: if we previously parked on a sleep completion,
            // io_entry_guard returns the completion's Unit result.
            if let Some(r) = vm.io_entry_guard(args)? {
                return Ok(r);
            }
            // Fresh scheduled-task call: submit to the shared timer thread
            // (NOT the I/O pool — we don't want to burn a worker thread
            // per sleeper) and park cooperatively.
            let completion = IoCompletion::new();
            vm.runtime.timer.schedule_completion(
                std::time::Duration::from_nanos(dur_ns as u64),
                completion.clone(),
            );
            vm.pending_io = Some(completion.clone());
            vm.block_reason = Some(BlockReason::Io(completion));
            for arg in args {
                vm.push(arg.clone());
            }
            Err(VmError::yield_signal())
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
        Some(Value::Int(n)) => match u16::try_from(*n) {
            Ok(s) => s,
            Err(_) => {
                return Err(VmError::new(format!(
                    "Response.status out of range: {n} is not a valid HTTP status (0..=65535)"
                )));
            }
        },
        _ => return Err(VmError::new("Response.status must be an Int".into())),
    };
    let body = match fields.get("body") {
        Some(Value::String(s)) => s.clone(),
        _ => return Err(VmError::new("Response.body must be a String".into())),
    };
    Ok((status, body, fields))
}

/// Max number of bytes accepted in an HTTP request body by `http.serve`.
/// 10 MiB — large enough for typical form posts and JSON payloads, small
/// enough that a single unauthenticated client cannot OOM the server
/// (HIGH-1). Larger uploads must use chunked/streaming handlers, which
/// the current API does not expose.
#[cfg(feature = "http")]
const HTTP_SERVE_MAX_BODY_BYTES: u64 = 10 * 1024 * 1024;

/// How long `server.recv_timeout` will block waiting for the next request
/// before looping. The accept loop re-checks the shutdown flag each time,
/// so this bounds how long shutdown takes. It does NOT per-connection
/// cap slow header reads inside tiny_http's internal pool (that is a
/// library limitation — see HIGH-2 notes).
#[cfg(feature = "http")]
const HTTP_SERVE_RECV_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum number of concurrent request handler threads spawned by
/// `http.serve`. Requests beyond this cap are rejected with HTTP 503
/// so a slowloris / burst cannot force unbounded thread spawning.
#[cfg(feature = "http")]
const HTTP_SERVE_MAX_CONCURRENT_HANDLERS: usize = 128;

/// Extract a silt Response record and send it as an HTTP response.
/// Used by the per-request handler threads in `http.serve`.
#[cfg(feature = "http")]
fn send_http_response(response_val: &Value, req: tiny_http::Request) {
    match extract_http_response(response_val) {
        Ok((status, resp_body, resp_fields)) => {
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
        Err(e) => {
            // Security: don't leak VmError contents (call stack, line
            // numbers, internal function names, possibly-sensitive panic
            // payloads) over the HTTP wire (MED-1). Log internally,
            // respond generically.
            eprintln!("http.serve: handler returned malformed Response: {e}");
            let resp = tiny_http::Response::from_string("Internal Server Error")
                .with_status_code(tiny_http::StatusCode(500));
            let _ = req.respond(resp);
        }
    }
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

/// Scrub URL userinfo (`user:password@`) from an error message.
///
/// `ureq::Error`'s Display impl can include the request URL, and if
/// the caller embedded credentials in the URL (`https://user:tok@h`),
/// those credentials leak into the `Err` string that's handed back
/// to the silt program (F12). Replace the userinfo segment with
/// `***@` for any `http://` / `https://` substring that carries one.
///
/// Grammar: the userinfo component of RFC 3986 is
/// `*( unreserved / pct-encoded / sub-delims / ":" )`. We accept a
/// conservative superset: alphanumerics, `%` escapes, and the common
/// URL-safe punctuation (`._~-+`) in the user segment, optionally
/// followed by `:<password>` where password is any non-`@`,
/// non-whitespace run. The `@` terminates userinfo.
#[cfg(feature = "http")]
pub fn redact_http_url_userinfo(msg: &str) -> String {
    let mut out = String::with_capacity(msg.len());
    let bytes = msg.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Look for scheme prefix at position `i`.
        let rest = &msg[i..];
        let scheme_len = if rest.starts_with("https://") {
            8
        } else if rest.starts_with("http://") {
            7
        } else {
            out.push(bytes[i] as char);
            i += 1;
            continue;
        };
        // Scan forward for `@` before any URL terminator char.
        // Valid userinfo chars: alphanumeric, `%`, `.`, `_`, `~`, `-`, `+`, `:`.
        let host_start = i + scheme_len;
        let mut j = host_start;
        let mut at_pos: Option<usize> = None;
        while j < bytes.len() {
            let b = bytes[j];
            // Terminators for the authority component.
            if b == b'@' {
                at_pos = Some(j);
                break;
            }
            if b == b'/' || b == b'?' || b == b'#' || b == b' ' || b == b'\t' || b == b'\n' {
                break;
            }
            // Accept any non-terminator byte (covers alphanumeric, `%`
            // escapes, `:` separator, and the `._~-+` URL-safe set).
            j += 1;
        }
        // Copy scheme verbatim.
        out.push_str(&msg[i..host_start]);
        if let Some(at) = at_pos {
            // Only scrub if we actually have content before the `@`
            // (otherwise `scheme://@host` stays as-is).
            if at > host_start {
                out.push_str("***@");
                i = at + 1; // skip the original userinfo + `@`
                continue;
            }
        }
        i = host_start;
    }
    out
}

/// Perform a synchronous HTTP GET and return a `Value`.
#[cfg(feature = "http")]
fn do_http_get(url: &str) -> Value {
    // Security: conservative default timeouts so a slow/unreachable peer
    // cannot hang the underlying OS socket indefinitely (HIGH-3). These
    // apply even if the silt task's SILT_IO_TIMEOUT unblocks the VM task,
    // so we don't leak real file descriptors.
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_global(Some(Duration::from_secs(60)))
        .build()
        .into();
    match agent.get(url).call() {
        Ok(response) => match ureq_response_to_value(response) {
            Ok(resp) => Value::Variant("Ok".into(), vec![resp]),
            Err(e) => Value::Variant("Err".into(), vec![Value::String(e.message)]),
        },
        Err(e) => Value::Variant(
            "Err".into(),
            vec![Value::String(redact_http_url_userinfo(&format!("{e}")))],
        ),
    }
}

/// Perform a synchronous HTTP request and return a `Value`.
#[cfg(feature = "http")]
fn do_http_request(method_tag: &str, url: &str, body: &str, headers: &[(String, String)]) -> Value {
    // Security: conservative default timeouts (HIGH-3). See do_http_get.
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_global(Some(Duration::from_secs(60)))
        .build()
        .into();

    let result = match method_tag {
        "POST" => {
            let mut req = agent.post(url);
            for (key, val) in headers {
                req = req.header(key.as_str(), val.as_str());
            }
            if body.is_empty() {
                req.send_empty()
            } else {
                req.send(body)
            }
        }
        "PUT" => {
            let mut req = agent.put(url);
            for (key, val) in headers {
                req = req.header(key.as_str(), val.as_str());
            }
            if body.is_empty() {
                req.send_empty()
            } else {
                req.send(body)
            }
        }
        "PATCH" => {
            let mut req = agent.patch(url);
            for (key, val) in headers {
                req = req.header(key.as_str(), val.as_str());
            }
            if body.is_empty() {
                req.send_empty()
            } else {
                req.send(body)
            }
        }
        "GET" => {
            let mut req = agent.get(url);
            for (key, val) in headers {
                req = req.header(key.as_str(), val.as_str());
            }
            req.call()
        }
        "DELETE" => {
            let mut req = agent.delete(url);
            for (key, val) in headers {
                req = req.header(key.as_str(), val.as_str());
            }
            req.call()
        }
        "HEAD" => {
            let mut req = agent.head(url);
            for (key, val) in headers {
                req = req.header(key.as_str(), val.as_str());
            }
            req.call()
        }
        "OPTIONS" => {
            let mut req = agent.options(url);
            for (key, val) in headers {
                req = req.header(key.as_str(), val.as_str());
            }
            req.call()
        }
        other => {
            return Value::Variant(
                "Err".into(),
                vec![Value::String(format!(
                    "http.request: unknown method: {other}"
                ))],
            );
        }
    };

    match result {
        Ok(response) => match ureq_response_to_value(response) {
            Ok(resp) => Value::Variant("Ok".into(), vec![resp]),
            Err(e) => Value::Variant("Err".into(), vec![Value::String(e.message)]),
        },
        Err(e) => Value::Variant(
            "Err".into(),
            vec![Value::String(redact_http_url_userinfo(&format!("{e}")))],
        ),
    }
}

/// Shared implementation of `http.serve` and `http.serve_all`.
///
/// `bind_host` is the interface portion of the bind address ("127.0.0.1"
/// for `http.serve`, "0.0.0.0" for `http.serve_all`). `name_for_err` is
/// the user-visible builtin name used in error messages.
#[cfg(feature = "http")]
fn do_http_serve_inner(
    vm: &mut Vm,
    bind_host: &str,
    name_for_err: &str,
    args: &[Value],
) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new(format!(
            "{name_for_err} takes 2 arguments (port, handler)"
        )));
    }
    let Value::Int(port) = &args[0] else {
        return Err(VmError::new(format!("{name_for_err}: port must be an Int")));
    };
    let handler = args[1].clone();

    let addr = format!("{bind_host}:{port}");
    let server = Arc::new(
        tiny_http::Server::http(&addr)
            .map_err(|e| VmError::new(format!("{name_for_err}: failed to bind: {e}")))?,
    );

    // Create a template child VM for spawning per-request handlers.
    // spawn_child() clones globals (which include builtins) and shares
    // runtime via Arc, so each request handler gets a fully functional VM.
    let template_vm = vm.spawn_child();
    let task_id = vm.next_task_id();
    let handle = Arc::new(TaskHandle::new(task_id));
    let serve_handle = handle.clone();

    // Counter of live per-request handler threads. Caps total
    // concurrent handlers at HTTP_SERVE_MAX_CONCURRENT_HANDLERS
    // so bursts / slowloris cannot force unbounded thread
    // spawning (HIGH-2).
    let inflight = Arc::new(AtomicUsize::new(0));

    // Spawn the accept loop on a dedicated OS thread so it doesn't
    // block a scheduler worker or the main thread.
    std::thread::spawn(move || {
        loop {
            // Use recv_timeout so the accept loop periodically
            // unblocks and can notice a shutdown. Note: this
            // does NOT per-connection bound the time tiny_http
            // spends reading headers from a slow client — tiny_http
            // does that inside its internal task pool and doesn't
            // expose the TcpStream to let us call
            // set_read_timeout. The concurrent-handler cap below
            // bounds the blast radius. (HIGH-2)
            let mut req = match server.recv_timeout(HTTP_SERVE_RECV_TIMEOUT) {
                Ok(Some(req)) => req,
                Ok(None) => continue, // timeout, re-loop
                Err(_) => break,      // server shut down
            };

            // Enforce concurrency cap. If we're at the cap, fast-reject
            // with 503 instead of spawning another thread.
            if inflight.load(Ordering::Acquire) >= HTTP_SERVE_MAX_CONCURRENT_HANDLERS {
                let resp = tiny_http::Response::from_string("Service Unavailable")
                    .with_status_code(tiny_http::StatusCode(503));
                let _ = req.respond(resp);
                continue;
            }

            // For each accepted request, spawn a handler thread
            // with its own child VM for concurrent request handling.
            let handler = handler.clone();
            let mut request_vm = template_vm.spawn_child();
            let inflight_guard = inflight.clone();
            inflight_guard.fetch_add(1, Ordering::AcqRel);

            std::thread::spawn(move || {
                // Guard that decrements inflight on thread exit
                // even if a panic or early-return path fires.
                struct Decrement(Arc<AtomicUsize>);
                impl Drop for Decrement {
                    fn drop(&mut self) {
                        self.0.fetch_sub(1, Ordering::AcqRel);
                    }
                }
                let _dec = Decrement(inflight_guard);

                // Parse the HTTP method
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
                        return;
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

                // Fast-reject oversized bodies based on the
                // declared Content-Length. Prevents a client from
                // forcing us to consume the whole body just to
                // discover we'd reject it. (HIGH-1)
                if let Some(declared) = req.body_length()
                    && declared as u64 > HTTP_SERVE_MAX_BODY_BYTES
                {
                    let resp = tiny_http::Response::from_string("Payload Too Large")
                        .with_status_code(tiny_http::StatusCode(413));
                    let _ = req.respond(resp);
                    return;
                }

                // Read body with a hard cap. `take(N+1)` + length
                // check lets us detect overrun (e.g. chunked
                // encoding that lies about total length). (HIGH-1)
                let mut body_bytes: Vec<u8> = Vec::new();
                let cap = HTTP_SERVE_MAX_BODY_BYTES;
                let read_result = std::io::Read::read_to_end(
                    &mut std::io::Read::take(req.as_reader(), cap + 1),
                    &mut body_bytes,
                );
                if read_result.is_err() || body_bytes.len() as u64 > cap {
                    let resp = tiny_http::Response::from_string("Payload Too Large")
                        .with_status_code(tiny_http::StatusCode(413));
                    let _ = req.respond(resp);
                    return;
                }
                // The Request API hands us body as a String; we
                // lossy-convert so non-UTF-8 bodies don't silently
                // drop. Handlers that need raw bytes should use
                // a separate API (future work).
                let body = std::string::String::from_utf8_lossy(&body_bytes).into_owned();

                // Build Request record
                let request_val = make_http_request_value(method_str, &path, &query, headers, body);

                // Run the user's handler on the per-request child VM
                match request_vm.invoke_callable(&handler, &[request_val]) {
                    Ok(response_val) => {
                        send_http_response(&response_val, req);
                    }
                    Err(e) => {
                        // Security: do NOT include VmError details
                        // (call stack, line numbers, panic payload)
                        // in the response body — that leaks
                        // implementation details and potentially
                        // sensitive values across the security
                        // boundary (MED-1). Log to stderr instead.
                        eprintln!("http.serve: handler error: {e}");
                        let resp = tiny_http::Response::from_string("Internal Server Error")
                            .with_status_code(tiny_http::StatusCode(500));
                        let _ = req.respond(resp);
                    }
                }
            });
        }
        // Accept loop ended (server shut down) — complete the handle.
        serve_handle.complete(Ok(Value::Unit));
    });

    // If running as a scheduled task, yield and let the scheduler
    // park us until the serve handle completes (i.e. server shuts down).
    if vm.is_scheduled_task {
        vm.block_reason = Some(BlockReason::Join(handle.clone()));
        for arg in args {
            vm.push(arg.clone());
        }
        return Err(VmError::yield_signal());
    }

    // Main thread: block until the server shuts down.
    match handle.join() {
        Ok(val) => Ok(val),
        Err(mut inner) => {
            inner.message = format!("{name_for_err} failed: {}", inner.message);
            Err(inner)
        }
    }
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

                if let Some(r) = vm.io_entry_guard(args)? {
                    return Ok(r);
                }
                if vm.is_scheduled_task {
                    let url = url.clone();
                    let completion = vm.runtime.io_pool.submit(move || do_http_get(&url));
                    vm.pending_io = Some(completion.clone());
                    vm.block_reason = Some(BlockReason::Io(completion));
                    for arg in args {
                        vm.push(arg.clone());
                    }
                    return Err(VmError::yield_signal());
                }
                // Main thread: synchronous fallback.
                Ok(do_http_get(url))
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

                if let Some(r) = vm.io_entry_guard(args)? {
                    return Ok(r);
                }
                if vm.is_scheduled_task {
                    let method_tag = method_tag.clone();
                    let url = url.clone();
                    let body = body.clone();
                    let headers: Vec<(String, String)> = header_map
                        .iter()
                        .filter_map(|(k, v)| {
                            if let (Value::String(key), Value::String(val)) = (k, v) {
                                Some((key.clone(), val.clone()))
                            } else {
                                None
                            }
                        })
                        .collect();
                    let completion = vm
                        .runtime
                        .io_pool
                        .submit(move || do_http_request(&method_tag, &url, &body, &headers));
                    vm.pending_io = Some(completion.clone());
                    vm.block_reason = Some(BlockReason::Io(completion));
                    for arg in args {
                        vm.push(arg.clone());
                    }
                    return Err(VmError::yield_signal());
                }

                // Main thread: synchronous fallback
                let headers: Vec<(String, String)> = header_map
                    .iter()
                    .filter_map(|(k, v)| {
                        if let (Value::String(key), Value::String(val)) = (k, v) {
                            Some((key.clone(), val.clone()))
                        } else {
                            None
                        }
                    })
                    .collect();
                Ok(do_http_request(method_tag, url, body, &headers))
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
                // Security: default to loopback only (HIGH-5). Developers who
                // want to expose the server on all interfaces must opt in via
                // `http.serve_all`.
                do_http_serve_inner(vm, "127.0.0.1", "http.serve", args)
            }
            #[cfg(not(feature = "http"))]
            {
                let _ = args;
                Err(VmError::new(
                    "http.serve requires the 'http' feature".into(),
                ))
            }
        }

        "serve_all" => {
            #[cfg(feature = "http")]
            {
                // Explicit opt-in to binding 0.0.0.0 (all interfaces). (HIGH-5)
                do_http_serve_inner(vm, "0.0.0.0", "http.serve_all", args)
            }
            #[cfg(not(feature = "http"))]
            {
                let _ = args;
                Err(VmError::new(
                    "http.serve_all requires the 'http' feature".into(),
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

        "parse_query" => {
            if args.len() != 1 {
                return Err(VmError::new(
                    "http.parse_query takes 1 argument (query)".into(),
                ));
            }
            let Value::String(raw) = &args[0] else {
                return Err(VmError::new("http.parse_query requires a String".into()));
            };
            // Accept a leading `?` for convenience — e.g. directly
            // passing a URL fragment like `?a=1&b=2` shouldn't require
            // the caller to strip it first.
            let body = raw.strip_prefix('?').unwrap_or(raw);
            // Preserve insertion order of first appearance for each
            // key. BTreeMap gives us stable ordering by key, which is
            // fine for a value-semantic Map — repeated keys always
            // append to the same List in encounter order.
            let mut out: BTreeMap<Value, Value> = BTreeMap::new();
            if body.is_empty() {
                return Ok(Value::Map(Arc::new(out)));
            }
            for (i, segment) in body.split('&').enumerate() {
                // Empty segments (leading `&`, `&&`, trailing `&`) are
                // skipped, matching WHATWG's form-urlencoded parser and
                // `encoding.form_decode`.
                if segment.is_empty() {
                    continue;
                }
                // Split on the FIRST `=`. Missing `=` → value is "".
                // The spec says a bare key with no separator means
                // "present with empty value", which matches how forms
                // serialize a checkbox with value "".
                let (raw_key, raw_val) = match segment.find('=') {
                    Some(pos) => (&segment[..pos], &segment[pos + 1..]),
                    None => (segment, ""),
                };
                let key = crate::builtins::encoding::form_decode_component(raw_key)
                    .map_err(|msg| VmError::new(format!("http.parse_query: pair {i} key: {msg}")))?;
                let val = crate::builtins::encoding::form_decode_component(raw_val)
                    .map_err(|msg| VmError::new(format!("http.parse_query: pair {i} value: {msg}")))?;
                let entry = out
                    .entry(Value::String(key))
                    .or_insert_with(|| Value::List(Arc::new(Vec::new())));
                if let Value::List(list) = entry {
                    // `Arc::make_mut` clones the Vec only on the
                    // second and later pushes for the same key; the
                    // first push sees refcount 1 and mutates in place.
                    Arc::make_mut(list).push(Value::String(val));
                }
            }
            Ok(Value::Map(Arc::new(out)))
        }

        _ => Err(VmError::new(format!("unknown http function: {name}"))),
    }
}

#[cfg(all(test, feature = "http"))]
mod http_response_tests {
    use super::*;

    fn make_response(status: i64) -> Value {
        let mut fields: BTreeMap<String, Value> = BTreeMap::new();
        fields.insert("status".to_string(), Value::Int(status));
        fields.insert("body".to_string(), Value::String(String::new()));
        fields.insert("headers".to_string(), Value::Map(Arc::new(BTreeMap::new())));
        Value::Record("Response".to_string(), Arc::new(fields))
    }

    #[test]
    fn test_response_status_out_of_u16_range_rejected() {
        let val = make_response(99999);
        let err = extract_http_response(&val).unwrap_err();
        assert!(
            err.message.contains("out of range") && err.message.contains("99999"),
            "expected 'out of range' error mentioning 99999, got: {}",
            err.message
        );
    }

    #[test]
    fn test_response_status_negative_rejected() {
        let val = make_response(-1);
        let err = extract_http_response(&val).unwrap_err();
        assert!(
            err.message.contains("out of range"),
            "expected out-of-range error for negative status, got: {}",
            err.message
        );
    }

    #[test]
    fn test_response_status_at_u16_max_ok() {
        let val = make_response(65535);
        let result = extract_http_response(&val);
        assert!(result.is_ok(), "status 65535 should be accepted");
        assert_eq!(result.unwrap().0, 65535);
    }

    #[test]
    fn test_response_status_zero_ok() {
        let val = make_response(0);
        let result = extract_http_response(&val);
        assert!(result.is_ok(), "status 0 should be accepted");
        assert_eq!(result.unwrap().0, 0);
    }
}
