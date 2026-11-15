//! The fields of a `Date` and a `Time` value, and their text: what the
//! value's display shows and what the time builtins write.

use crate::value::{Record, Value};

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
