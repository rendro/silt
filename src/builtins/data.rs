//! A silt value as data: the one layer that `json.stringify`,
//! `toml.stringify` and the parameters of a postgres statement write a
//! value from.
//!
//! [`data`] turns a value into [`Data`]: `Some(x)` is `x`, a `json.Value`
//! the JSON it holds, a record a table of its fields in the order its
//! type declares them, a map a table sorted by its keys' shown text, a
//! variant the table `{fields, variant}`, a date and a time their
//! fields. It refuses every value the format it writes for has no form
//! for, with one wording per refusal, wherever in the value it is. Each
//! format then writes the data in its own forms: json its numbers and
//! strings, toml its literals and its tables, postgres the text the
//! server reads.

use std::sync::Arc;

use crate::typeinfo::{bv, ty};
use crate::value::{Record, Value};
use crate::vm::VmError;

/// The deepest a value may be nested for a writer: each element of a
/// list or a tuple, field of a record or variant, and value of a map is
/// a level further in than what holds them (the content of a `Some` or
/// of a `json.Value` is not: it is written in its place). The writers go
/// down a value on the native stack, and one nested without bound would
/// overflow it: a value at the bound takes some 5 MiB of it in a debug
/// build, a third of the least a thread that runs silt code has
/// (`scheduler::WORKER_STACK_BYTES`). (A parser stops far sooner:
/// `json.parse` at 127 levels of a document, `toml.parse` at 79.)
pub(crate) const MAX_ENCODE_DEPTH: usize = 1000;

/// The format a value is written in: which values have a form in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Format {
    Json,
    /// TOML has no null: a `None` member of a table is left out, and a
    /// `None` anywhere else, or a `()`, has no form.
    Toml,
    /// The parameter of a statement: `Bytes` has a form at its place and
    /// in an array (`bytea`'s hex text), not in the JSON of a record, a
    /// map, a tuple, a variant.
    Postgres,
}

/// A value as data.
#[derive(Debug)]
pub(crate) enum Data {
    /// `None`, `json.Null`.
    Null,
    Unit,
    Int(i64),
    Float(f64),
    Bool(bool),
    String(Arc<str>),
    /// Only for [`Format::Postgres`], at a parameter's place or in an
    /// array.
    Bytes(#[cfg_attr(not(feature = "postgres"), allow(dead_code))] Arc<Vec<u8>>),
    Date(Date),
    Time(Time),
    DateTime(Date, Time),
    /// A list, the fields of a variant.
    Array(Vec<Data>),
    /// A tuple: an array to json and toml, JSON to postgres where a list
    /// is an array.
    Tuple(Vec<Data>),
    /// A record, a map, a variant, a `Duration`, an `Instant`.
    Table(Vec<(String, Data)>),
    /// A `json.Value` at a postgres parameter's place or in an array: the
    /// JSON it holds, which the server gets as JSON text. (For json and
    /// toml, and inside JSON, a `json.Value` is what it holds.)
    Json(Box<Data>),
}

/// A `Date`'s fields. Shown as the text of a `Date`, the one place it
/// is decided (a `Date` value displays it too): `2026-10-19`,
/// `-043-03-15`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Date {
    pub(crate) year: i64,
    pub(crate) month: i64,
    pub(crate) day: i64,
}

/// A `Time`'s fields. Shown as the text of a `Time`, the one place it
/// is decided (a `Time` value displays it too): `12:30:00`, with a
/// fraction of a second only when it has one, `12:30:00.250000000`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Time {
    pub(crate) hour: i64,
    pub(crate) minute: i64,
    pub(crate) second: i64,
    pub(crate) ns: i64,
}

/// The `Int` field `name` of `record`, or 0. (A record of a builtin
/// type of time holds an `Int` in each field.)
fn int(record: &Record, name: &str) -> i64 {
    match record.get(name) {
        Some(Value::Int(n)) => *n,
        _ => 0,
    }
}

impl Date {
    /// The fields of `record`, a `Date`.
    pub(crate) fn of(record: &Record) -> Date {
        Date {
            year: int(record, "year"),
            month: int(record, "month"),
            day: int(record, "day"),
        }
    }
}

impl Time {
    /// The fields of `record`, a `Time`.
    pub(crate) fn of(record: &Record) -> Time {
        Time {
            hour: int(record, "hour"),
            minute: int(record, "minute"),
            second: int(record, "second"),
            ns: int(record, "ns"),
        }
    }
}

impl std::fmt::Display for Date {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Date { year, month, day } = self;
        write!(f, "{year:04}-{month:02}-{day:02}")
    }
}

impl std::fmt::Display for Time {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Time {
            hour,
            minute,
            second,
            ns,
        } = self;
        write!(f, "{hour:02}:{minute:02}:{second:02}")?;
        match *ns > 0 {
            true => write!(f, ".{ns:09}"),
            false => Ok(()),
        }
    }
}

/// What is said of a value of the kind `kind` that a format has no
/// form for.
pub(crate) fn unsupported(kind: &str) -> String {
    format!("unsupported value kind {kind}")
}

/// The error of `caller` for `v`, a value the format it writes has no
/// form for: it names the kind of the value. (To a program every
/// function is an `Fn`.)
fn refused(caller: &str, v: &Value) -> VmError {
    let kind = match v.is_fn() {
        true => "Fn",
        false => v.kind(),
    };
    VmError::new(format!("{caller}: {}", unsupported(kind)))
}

/// `v` as data in `format`, for `caller` (`json.stringify`), or the
/// error of the first value in it that `format` has no form for: a set,
/// a function (the constructor of a variant among them), a channel, a
/// handle, `Bytes` where the format has none, a value nested deeper
/// than [`MAX_ENCODE_DEPTH`], a list too long to write out; for toml
/// also `()` and a `None` in an array.
pub(crate) fn data(caller: &str, format: Format, v: &Value) -> Result<Data, VmError> {
    Writer { caller, format }.data(v, 0, Place::Value)
}

/// Where in the value being written a value is, as far as the format
/// tells them apart.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Place {
    /// At the top, or in a list at the top (a postgres parameter's place
    /// or an array).
    Value,
    /// A member of a table.
    Member,
    /// An element of an array that is no list at the top (a tuple, the
    /// fields of a variant, a list inside one of them).
    Element,
}

struct Writer<'c> {
    caller: &'c str,
    format: Format,
}

impl Writer<'_> {
    /// `v`, which is `level` levels inside the value and at `place`.
    fn data(&self, v: &Value, level: usize, place: Place) -> Result<Data, VmError> {
        if level > MAX_ENCODE_DEPTH {
            return Err(VmError::new(format!(
                "{}: the value is nested more than {MAX_ENCODE_DEPTH} levels deep",
                self.caller
            )));
        }
        // `Some(x)` is written as `x` is, and so is a `json.Value` as the
        // JSON it holds (`json.Int(1)` as `1`); neither is a level of its
        // own.
        let mut v = v;
        let mut json = false;
        while let Value::Variant(variant) = v
            && (variant.is(bv::SOME) || variant.of(ty::JSON_VALUE))
            && let [content] = variant.fields()
        {
            json |= variant.of(ty::JSON_VALUE);
            v = content;
        }
        if let Value::Variant(variant) = v {
            json |= variant.is(bv::JSON_NULL);
        }
        // Postgres sends a `json.Value` at a parameter's place, or in an
        // array, as JSON text.
        if json && self.format == Format::Postgres && place == Place::Value {
            return Ok(Data::Json(Box::new(self.content(
                v,
                level,
                Place::Member,
            )?)));
        }
        self.content(v, level, place)
    }

    /// `v`, which is no `Some` and no `json.Value` with content.
    fn content(&self, v: &Value, level: usize, place: Place) -> Result<Data, VmError> {
        // A list keeps the place of a list at the top; inside anything
        // else its elements are elements.
        let element = match place {
            Place::Value => Place::Value,
            _ => Place::Element,
        };
        let inner = |v: &Value, place| self.data(v, level + 1, place);
        Ok(match v {
            Value::Int(n) => Data::Int(*n),
            Value::Float(f) => Data::Float(f.get()),
            Value::Bool(b) => Data::Bool(*b),
            Value::String(s) => Data::String(s.clone()),
            Value::Bytes(bytes) if self.format == Format::Postgres && place == Place::Value => {
                Data::Bytes(bytes.clone())
            }
            Value::List(xs) => {
                xs.writable()?;
                let items: Result<Vec<_>, _> = xs.iter().map(|x| inner(&x, element)).collect();
                Data::Array(items?)
            }
            Value::Map(m) => {
                let entries: Result<Vec<_>, VmError> = m
                    .iter()
                    .map(|(k, v)| {
                        let key = match k {
                            Value::String(s) => s.to_string(),
                            other => other.to_string(),
                        };
                        Ok((key, inner(v, Place::Member)?))
                    })
                    .collect();
                // Sorted by key; of a key shown twice the last value
                // counts.
                let members: std::collections::BTreeMap<_, _> = entries?.into_iter().collect();
                Data::Table(members.into_iter().collect())
            }
            Value::Tuple(vs) => {
                let items: Result<Vec<_>, _> =
                    vs.iter().map(|v| inner(v, Place::Element)).collect();
                Data::Tuple(items?)
            }
            Value::Record(record) if record.type_id() == ty::DATE => Data::Date(Date::of(record)),
            Value::Record(record) if record.type_id() == ty::TIME => Data::Time(Time::of(record)),
            Value::Record(record) if record.type_id() == ty::DATE_TIME => {
                match (record.get("date"), record.get("time")) {
                    (Some(Value::Record(date)), Some(Value::Record(time))) => {
                        Data::DateTime(Date::of(date), Time::of(time))
                    }
                    // (A `DateTime` holds a `Date` and a `Time`.)
                    _ => return Err(refused(self.caller, v)),
                }
            }
            Value::Record(record) => {
                let members: Result<Vec<_>, VmError> = record
                    .named()
                    .map(|(k, v)| Ok((k.to_string(), inner(v, Place::Member)?)))
                    .collect();
                Data::Table(members?)
            }
            Value::Variant(variant) if variant.is(bv::NONE) || variant.is(bv::JSON_NULL) => {
                // (A `None` at the top is no table: toml says so.)
                if self.format == Format::Toml && place != Place::Member && level > 0 {
                    let null = match variant.is(bv::NONE) {
                        true => "None",
                        false => "json.Null",
                    };
                    return Err(VmError::new(format!(
                        "{}: TOML has no null: a {null} in an array",
                        self.caller
                    )));
                }
                Data::Null
            }
            // (`fields` before `variant`, as the keys of a table sort.)
            Value::Variant(variant) => {
                let mut members = Vec::with_capacity(2);
                if !variant.fields().is_empty() {
                    let items: Result<Vec<_>, _> = variant
                        .fields()
                        .iter()
                        .map(|v| inner(v, Place::Element))
                        .collect();
                    members.push(("fields".into(), Data::Array(items?)));
                }
                members.push(("variant".into(), Data::String(variant.name().into())));
                Data::Table(members)
            }
            Value::Unit if self.format == Format::Toml => {
                return Err(VmError::new(format!(
                    "{}: TOML cannot represent Unit",
                    self.caller
                )));
            }
            Value::Unit => Data::Unit,
            _ => return Err(refused(self.caller, v)),
        })
    }
}
