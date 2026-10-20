//! The one typed decoder of a document: json's, toml's, and a postgres
//! `json` or `jsonb` cell's (through json's rules). What a value of a
//! document is called in an error, the path that ends each error, the
//! text of a date and a time, and how a record type is looked up are
//! decided here for every format.

use std::sync::Arc;

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};

use super::time::{LEAP_SECOND, is_leap_second, leap_datetime_message};
use crate::defs::TypeId;
use crate::typeinfo::{FieldType, Shape, TypeInfo};
use crate::vm::{Runtime, VmError};

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

// ── Dates and times ─────────────────────────────────────────────────

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

// ── Record types ────────────────────────────────────────────────────

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
