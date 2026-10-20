//! The one typed decoder of a document: json's, toml's, and a postgres
//! `json` or `jsonb` cell's (through json's rules). What a value of a
//! document is called in an error, the path that ends each error, the
//! text of a date and a time, and how a record type is looked up are
//! decided here for every format.

use std::sync::Arc;

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};

use super::time::{
    LEAP_SECOND, is_leap_second, leap_datetime_message, make_date, make_datetime, make_time,
};
use super::typed::Type;
use crate::defs::TypeId;
use crate::typeinfo::{BuiltinVariant, FieldType, Shape, TypeInfo, bv, ty};
use crate::value::{Float, Value};
use crate::vm::{Runtime, VmError};

/// The message of the error a decoder returns for a field whose declared
/// type has no decoder. `field` is the field of the record type `record`
/// whose type is, or contains, `declared`.
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
/// (`12026-01-01`, `-001-01-01`).
fn parse_date(s: &str) -> Result<NaiveDate, chrono::ParseError> {
    NaiveDate::parse_from_str(&signed_year(s), "%Y-%m-%d")
}

/// The message of a decoder's error for `s`, the string of a date that
/// is none, `why`.
fn invalid_date_message(s: &str, why: chrono::ParseError) -> std::string::String {
    format!("invalid date '{s}' (expected YYYY-MM-DD): {why}")
}

/// The message of a decoder's error for `s`, the string of a time of
/// day that is none, `why`.
fn invalid_time_message(s: &str, why: &str) -> std::string::String {
    format!("invalid time '{s}' (expected HH:MM:SS): {why}")
}

/// The time of day that the text `s` of a document writes: `HH:MM:SS`,
/// with a fraction of a second up to nanoseconds (`13:14:15.25`), or
/// `HH:MM`. Else why it is none: a second 60 is a leap second, which is
/// refused.
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
/// which is refused.
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
pub(crate) fn decodable_record<'t>(
    caller: &str,
    ty: &'t TypeInfo,
) -> Result<&'t [(std::string::String, FieldType)], VmError> {
    match &ty.shape {
        Shape::Record(fields) if !ty.is_builtin() => Ok(fields),
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
fn without_bytes(rt: &Runtime, field_type: &FieldType) -> Option<FieldType> {
    field_type
        .holds_bytes()
        .then(|| FieldType::Unsupported(shown(rt, field_type)))
}

/// The record type `id` a record field names, from the program's types.
fn field_record_type(rt: &Runtime, caller: &str, id: TypeId) -> Result<Arc<TypeInfo>, VmError> {
    rt.type_info(id)
        .ok_or_else(|| VmError::new(format!("{caller}: unknown record type")))
}

// ── The document ────────────────────────────────────────────────────

/// A document a decoder reads: the values its parser makes of a text
/// (`serde_json::Value`, `toml::Value`), each one of a few kinds, and the
/// rules of its format.
pub(crate) trait Document: Sized + 'static {
    /// The table (JSON's object) of the document.
    type Table;
    /// The rules of the format.
    const RULES: &'static Rules;
    /// What this value of the document is.
    fn node(&self) -> Node<'_, Self>;
    /// What the format calls the kind of this value in a type mismatch
    /// (`number`, `integer`, `object`, `table`).
    fn kind(&self) -> &'static str;
    /// The value of `key` in `table`.
    fn get<'d>(table: &'d Self::Table, key: &str) -> Option<&'d Self>;
    /// The keys and values of `table`, in its order.
    fn entries(table: &Self::Table) -> impl ExactSizeIterator<Item = (&str, &Self)>;
}

/// A value of a document `D`.
pub(crate) enum Node<'d, D: Document> {
    Null,
    Bool(bool),
    /// A number held as an `Int`: TOML's integer, or a JSON number that is
    /// a whole number in `Int`'s range.
    Int(i64),
    /// Any other number.
    Float(f64),
    String(&'d str),
    /// A date or time literal (TOML's), as its text.
    Datetime(std::string::String),
    Array(&'d [D]),
    Table(&'d D::Table),
}

/// How a format's documents decode, where formats differ.
pub(crate) struct Rules {
    /// The builtin a record type in a document is looked up for: it
    /// starts the message of a type that is none.
    pub(crate) caller: &'static str,
    /// What a record and a map expect, the kind of a table.
    pub(crate) table: &'static str,
    /// What a `Date`, a `Time` and a `DateTime` expect.
    pub(crate) dates: [&'static str; 3],
    /// How a number that is no `Int` is told from one.
    pub(crate) numbers: Numbers,
    /// Whether a `json.Value` is decoded from the document.
    pub(crate) json_value: bool,
    /// The variants `TypeMismatch`, `MissingField` and `Other` of the
    /// format's error (`JsonError`, `TomlError`).
    pub(crate) errors: [BuiltinVariant; 3],
}

/// How a number that is no `Int` is told from one.
#[derive(PartialEq, Eq)]
pub(crate) enum Numbers {
    /// By its value (JSON has one kind of number): a number held as no
    /// `Int` is one with a fraction or out of `Int`'s range.
    Values,
    /// By its kind (TOML says which kind a number is): a float is no
    /// `Int`, whatever its value.
    Kinds,
}

/// `i64::MAX` is not exactly an `f64` (it rounds up to 2^63): the
/// bound of `Int`'s range that an `f64` can hold, exclusive.
pub(crate) const I64_MAX_PLUS_ONE: f64 = 9223372036854775808.0; // exact

// ── The decoder ─────────────────────────────────────────────────────

/// Why a document is no value of the type it is decoded into: one of the
/// format's error variants, made of it by [`Error::variant`].
#[derive(Debug)]
pub(crate) enum Error {
    /// `TypeMismatch(expected, actual)`: `actual` ends with the path.
    TypeMismatch(&'static str, std::string::String),
    /// `MissingField(path)`: the missing field's own path.
    MissingField(std::string::String),
    /// `Other(message)`.
    Other(std::string::String),
    /// A value was present for a type no decoder exists for (the type as
    /// written): [`record`] turns it into an `Other` that names the field
    /// whose type it is.
    Unsupported(std::string::String),
}

impl From<VmError> for Error {
    fn from(e: VmError) -> Self {
        Error::Other(e.message)
    }
}

impl Error {
    /// The type mismatch of a value `at` a path that is `actual`.
    pub(crate) fn mismatch(expected: &'static str, actual: &str, at: &At) -> Error {
        Error::TypeMismatch(expected, format!("{actual}{}", at.suffix()))
    }

    /// The `Other` of a value `at` a path: `message` ends with the path.
    fn other(message: impl std::fmt::Display, at: &At) -> Error {
        Error::Other(format!("{message}{}", at.suffix()))
    }

    /// The variant of the error of the format of `rules`.
    pub(crate) fn variant(self, rules: &Rules) -> Value {
        let [mismatch, missing, other] = rules.errors;
        let string = |s: std::string::String| Value::String(s.into());
        match self {
            Error::TypeMismatch(expected, actual) => Value::variant(
                mismatch,
                vec![Value::String(expected.into()), string(actual)],
            ),
            Error::MissingField(path) => Value::variant(missing, vec![string(path)]),
            Error::Other(message) => Value::variant(other, vec![string(message)]),
            // (Only a record's field holds such a type, and `record`
            // names it: this is the fallback.)
            Error::Unsupported(declared) => Value::variant(
                other,
                vec![string(format!(
                    "a value of type {declared} cannot be decoded"
                ))],
            ),
        }
    }
}

/// `Ok` of the value `decoded`, or `Err` of its error in the format of
/// `rules`.
pub(crate) fn result(rules: &Rules, decoded: Result<Value, Error>) -> Value {
    match decoded {
        Ok(value) => Value::variant(bv::OK, vec![value]),
        Err(e) => Value::variant(bv::ERR, vec![e.variant(rules)]),
    }
}

/// The type of the elements a `parse_list` and of the values a
/// `parse_map` (`caller`) of the format of `rules` decode, which their
/// type argument `ty` names: `Int`, `Float`, `String`, `Bool`, a record
/// type, and `json.Value` where the format decodes one. Any other type
/// is an error.
pub(crate) fn element_type(rules: &Rules, caller: &str, ty: Type) -> Result<FieldType, VmError> {
    Ok(match ty {
        Type::Primitive(field_type) => field_type,
        Type::Named(ty) if rules.json_value && ty.id == ty::JSON_VALUE => FieldType::JsonValue,
        Type::Named(ty) => {
            decodable_record(caller, ty)?;
            FieldType::Record(ty.id)
        }
    })
}

/// The record of type `ty` (of the `fields`) that `doc`, which is `at` a
/// path, is.
pub(crate) fn record<D: Document>(
    rt: &Runtime,
    ty: &Arc<TypeInfo>,
    fields: &[(std::string::String, FieldType)],
    doc: &D,
    at: &At,
) -> Result<Value, Error> {
    let Node::Table(table) = doc.node() else {
        return Err(Error::mismatch(D::RULES.table, doc.kind(), at));
    };
    // (In the order the type declares them: the record's own.)
    let mut values = Vec::with_capacity(fields.len());
    for (name, field) in fields {
        let unsupported = without_bytes(rt, field);
        let field = unsupported.as_ref().unwrap_or(field);
        // A field no decoder exists for is the error whatever the
        // document holds there, its key left out too.
        if let FieldType::Unsupported(declared) = field {
            let message = unsupported_field_type_message(&ty.name, name, declared);
            return Err(Error::Other(message));
        }
        let at = at.key(name);
        values.push(match D::get(table, name) {
            Some(item) => value(rt, item, field, &at).map_err(|e| match e {
                Error::Unsupported(declared) => {
                    Error::Other(unsupported_field_type_message(&ty.name, name, &declared))
                }
                e => e,
            })?,
            None => match field {
                FieldType::Option(_) => Value::variant(bv::NONE, Vec::new()),
                _ => return Err(Error::MissingField(at.to_string())),
            },
        });
    }
    Ok(Value::record(ty.clone(), values))
}

/// The list of values of the field type `element` that `doc`, an array
/// `at` a path, holds.
pub(crate) fn list<D: Document>(
    rt: &Runtime,
    element: &FieldType,
    doc: &D,
    at: &At,
) -> Result<Value, Error> {
    let Node::Array(items) = doc.node() else {
        return Err(Error::mismatch("array", doc.kind(), at));
    };
    let mut values = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        values.push(value(rt, item, element, &at.index(index))?);
    }
    Ok(Value::list(values))
}

/// The map of values of the field type `element` that `doc`, a table
/// `at` a path, holds.
pub(crate) fn map<D: Document>(
    rt: &Runtime,
    element: &FieldType,
    doc: &D,
    at: &At,
) -> Result<Value, Error> {
    let Node::Table(table) = doc.node() else {
        return Err(Error::mismatch(D::RULES.table, doc.kind(), at));
    };
    let entries = D::entries(table);
    let mut map = Vec::with_capacity(entries.len());
    for (key, item) in entries {
        map.push((
            Value::String(key.into()),
            value(rt, item, element, &at.key(key))?,
        ));
    }
    Ok(Value::map(map))
}

/// The value of the field type `expected` that `doc`, which is `at` a
/// path, is.
pub(crate) fn value<D: Document>(
    rt: &Runtime,
    doc: &D,
    expected: &FieldType,
    at: &At,
) -> Result<Value, Error> {
    let rules = D::RULES;
    let mismatch = |expected: &'static str| Error::mismatch(expected, doc.kind(), at);
    let node = doc.node();
    match expected {
        // (A datetime literal is no string, as a float is no `Int`.)
        FieldType::String => match node {
            Node::String(s) => Ok(Value::String(s.into())),
            _ => Err(mismatch("String")),
        },
        // A number is an `Int` when it is held as one; else, where the
        // format tells numbers apart by their value, one with a fraction
        // or out of range is no `Int`, and nothing is cut off or
        // saturated.
        FieldType::Int => match node {
            Node::Int(n) => Ok(Value::Int(n)),
            Node::Float(f) if rules.numbers == Numbers::Values => {
                let what = match f.abs() < I64_MAX_PLUS_ONE {
                    true => "a number with a fraction",
                    false => "a number out of Int's range",
                };
                Err(Error::mismatch("Int", what, at))
            }
            _ => Err(mismatch("Int")),
        },
        // (An integer is a `Float` too. A `Float` is finite, so TOML's
        // `nan` and `inf` do not decode.)
        FieldType::Float => match node {
            Node::Int(n) => Ok(Value::Float(Float::from(n))),
            Node::Float(f) => match Float::new(f) {
                Some(f) => Ok(Value::Float(f)),
                None => Err(Error::mismatch("Float", "a non-finite float", at)),
            },
            _ => Err(mismatch("Float")),
        },
        FieldType::Bool => match node {
            Node::Bool(b) => Ok(Value::Bool(b)),
            _ => Err(mismatch("Bool")),
        },
        FieldType::List(inner) => match node {
            Node::Array(items) => {
                let mut values = Vec::with_capacity(items.len());
                for (index, item) in items.iter().enumerate() {
                    values.push(value(rt, item, inner, &at.index(index))?);
                }
                Ok(Value::list(values))
            }
            _ => Err(mismatch("List")),
        },
        // (TOML has no null: a key it leaves out is a `None`, which
        // `record` decodes.)
        FieldType::Option(inner) => match node {
            Node::Null => Ok(Value::variant(bv::NONE, Vec::new())),
            _ => Ok(Value::variant(bv::SOME, vec![value(rt, doc, inner, at)?])),
        },
        FieldType::Map(inner) => match node {
            Node::Table(table) => {
                let entries = D::entries(table);
                let mut map = Vec::with_capacity(entries.len());
                for (key, item) in entries {
                    let item = value(rt, item, inner, &at.key(key))?;
                    map.push((Value::String(key.into()), item));
                }
                Ok(Value::map(map))
            }
            _ => Err(mismatch("Map")),
        },
        FieldType::Tuple(elems) => match node {
            Node::Array(items) if items.len() == elems.len() => {
                let mut values = Vec::with_capacity(elems.len());
                for (index, (item, elem)) in items.iter().zip(elems).enumerate() {
                    values.push(value(rt, item, elem, &at.index(index))?);
                }
                Ok(Value::tuple(values))
            }
            Node::Array(items) => Err(Error::other(
                format_args!(
                    "expected an array of {} elements for a tuple, got {}",
                    elems.len(),
                    items.len()
                ),
                at,
            )),
            _ => Err(mismatch("Tuple")),
        },
        FieldType::Unsupported(declared) => Err(Error::Unsupported(declared.clone())),
        FieldType::JsonValue if rules.json_value => Ok(json_value(doc)),
        FieldType::JsonValue => Err(Error::Unsupported("json.Value".into())),
        FieldType::Bytes => Err(Error::Unsupported("Bytes".into())),
        // A date and a time are a string of their text, or a literal of
        // the format.
        FieldType::Date => match node {
            Node::String(s) => parse_date(s)
                .map(make_date)
                .map_err(|e| Error::other(invalid_date_message(s, e), at)),
            Node::Datetime(s) => parse_date(&s)
                .map(make_date)
                .map_err(|e| Error::other(format_args!("invalid date '{s}': {e}"), at)),
            _ => Err(mismatch(rules.dates[0])),
        },
        FieldType::Time => match node {
            Node::String(s) => parse_time(s)
                .map(make_time)
                .map_err(|e| Error::other(invalid_time_message(s, &e), at)),
            Node::Datetime(s) => parse_time(&s)
                .map(make_time)
                .map_err(|e| Error::other(format_args!("invalid time '{s}': {e}"), at)),
            _ => Err(mismatch(rules.dates[1])),
        },
        FieldType::DateTime => {
            let text = match &node {
                Node::String(s) => *s,
                Node::Datetime(s) => s,
                _ => return Err(mismatch(rules.dates[2])),
            };
            parse_datetime(text)
                .map(make_datetime)
                .map_err(|e| Error::other(e, at))
        }
        FieldType::Record(id) => {
            let ty = field_record_type(rt, rules.caller, *id)?;
            record(rt, &ty, decodable_record(rules.caller, &ty)?, doc, at)
        }
    }
}

/// The `json.Value` that holds `doc`: a number an `Int` when it is held
/// as one, as for an `Int` field, and a `Float` otherwise.
pub(crate) fn json_value<D: Document>(doc: &D) -> Value {
    match doc.node() {
        Node::Null => Value::variant(bv::JSON_NULL, Vec::new()),
        Node::Bool(b) => Value::variant(bv::JSON_BOOL, vec![Value::Bool(b)]),
        Node::Int(n) => Value::variant(bv::JSON_INT, vec![Value::Int(n)]),
        Node::Float(f) => match Float::new(f) {
            Some(f) => Value::variant(bv::JSON_FLOAT, vec![Value::Float(f)]),
            // (A document has no NaN.)
            None => Value::variant(bv::JSON_NULL, Vec::new()),
        },
        Node::String(s) => Value::variant(bv::JSON_STRING, vec![Value::String(s.into())]),
        // (A format with such literals decodes no `json.Value`.)
        Node::Datetime(s) => Value::variant(bv::JSON_STRING, vec![Value::String(s.into())]),
        Node::Array(items) => Value::variant(
            bv::JSON_ARRAY,
            vec![Value::list(items.iter().map(json_value).collect())],
        ),
        Node::Table(table) => Value::variant(
            bv::JSON_OBJECT,
            vec![Value::map(D::entries(table).map(|(key, item)| {
                (Value::String(key.into()), json_value(item))
            }))],
        ),
    }
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

    /// The TOML document of the same shape as the JSON document `json`:
    /// a member that is `null` is left out, as TOML has no null.
    fn toml_of(json: &serde_json::Value) -> Option<::toml::Value> {
        use ::toml::Value as T;
        use serde_json::Value as J;
        Some(match json {
            J::Null => return None,
            J::Bool(b) => T::Boolean(*b),
            J::Number(n) => match n.as_i64() {
                Some(n) => T::Integer(n),
                None => T::Float(n.as_f64()?),
            },
            J::String(s) => T::String(s.clone()),
            J::Array(items) => T::Array(items.iter().filter_map(toml_of).collect()),
            J::Object(members) => T::Table(
                members
                    .iter()
                    .filter_map(|(key, value)| Some((key.clone(), toml_of(value)?)))
                    .collect(),
            ),
        })
    }

    /// What a decoder made of a document, as far as two formats agree on
    /// it: the value; or the kind of the error, with the path it ends
    /// with (and the whole message of an `Other`).
    fn outcome(decoded: Result<Value, Error>) -> String {
        let path = |text: &str| match text.rsplit_once(" at ") {
            Some((_, path)) => path.to_string(),
            None => "(top)".to_string(),
        };
        match decoded {
            Ok(value) => format!("{value}"),
            Err(Error::TypeMismatch(_, actual)) => format!("TypeMismatch at {}", path(&actual)),
            Err(Error::MissingField(at)) => format!("MissingField {at}"),
            Err(Error::Other(message)) => format!("Other {message}"),
            Err(Error::Unsupported(declared)) => format!("Unsupported {declared}"),
        }
    }

    /// A document written in JSON and the same document in TOML decode
    /// alike into a record, a list and a map: to the same value, or to an
    /// error of the same kind at the same path. Where the formats differ
    /// on purpose (a whole float, a datetime literal, null, `json.Value`)
    /// each does what its documentation says.
    #[test]
    fn json_and_toml_decode_a_document_of_one_shape_alike() {
        use crate::defs::DefId;
        use FieldType as F;
        let vm = crate::vm::Vm::new(crate::vm::HostIo::process());
        let rt = &vm.runtime;
        let inner = TypeInfo::new_record(
            TypeId(DefId(9001)),
            "Inner",
            vec![
                ("n".into(), F::Int),
                ("tag".into(), F::Option(Box::new(F::String))),
            ],
        );
        let cfg = TypeInfo::new_record(
            TypeId(DefId(9002)),
            "Cfg",
            vec![
                ("name".into(), F::String),
                ("inner".into(), F::Record(inner.id)),
                ("xs".into(), F::List(Box::new(F::Int))),
                ("m".into(), F::Map(Box::new(F::Float))),
                ("t".into(), F::Tuple(vec![F::Int, F::String])),
                ("d".into(), F::Date),
                ("tm".into(), F::Time),
                ("at".into(), F::DateTime),
                ("o".into(), F::Option(Box::new(F::Bool))),
                ("rows".into(), F::List(Box::new(F::Record(inner.id)))),
            ],
        );
        let bytes = TypeInfo::new_record(
            TypeId(DefId(9003)),
            "B",
            vec![("b".into(), F::List(Box::new(F::Bytes)))],
        );
        rt.types.lock().insert(inner.clone());
        rt.types.lock().insert(cfg.clone());
        // Each document as JSON, with what both decoders make of it (a
        // value that is not the whole record by the fields it shows).
        let whole = r#"{"name": "x", "inner": {"n": 2}, "xs": [1, 2],
            "m": {"a": 1, "b c": 2.5}, "t": [1, "s"], "d": "2026-10-19",
            "tm": "12:30:00.25", "at": "2026-10-19T12:30:00+02:00",
            "rows": [{"n": 1, "tag": "a"}, {"n": 2}]}"#;
        // The whole document with the members of `change` in its place,
        // a member that is null left out.
        let with = |change: &str| {
            let object = |text: &str| match serde_json::from_str(text).expect("JSON") {
                serde_json::Value::Object(members) => members,
                other => panic!("{other}"),
            };
            let mut doc = object(whole);
            for (key, value) in object(change) {
                match value {
                    serde_json::Value::Null => doc.remove(&key),
                    value => doc.insert(key, value),
                };
            }
            serde_json::Value::Object(doc).to_string()
        };
        let records = [
            (
                with("{}"),
                "Cfg {name: x, inner: Inner {n: 2, tag: None}, xs: [1, 2], \
                 m: #{\"a\": 1.0, \"b c\": 2.5}, t: (1, s), d: 2026-10-19, \
                 tm: 12:30:00.250000000, at: 2026-10-19T10:30:00, o: None, \
                 rows: [Inner {n: 1, tag: Some(a)}, Inner {n: 2, tag: None}]}",
            ),
            (with(r#"{"o": true}"#), "o: Some(true)"),
            (
                with(r#"{"xs": [9223372036854775807, -1]}"#),
                "xs: [9223372036854775807, -1]",
            ),
            (with(r#"{"name": null}"#), "MissingField name"),
            (with(r#"{"name": true}"#), "TypeMismatch at name"),
            (with(r#"{"inner": 5}"#), "TypeMismatch at inner"),
            (with(r#"{"inner": {"n": "x"}}"#), "TypeMismatch at inner.n"),
            (with(r#"{"inner": {"n": 1.5}}"#), "TypeMismatch at inner.n"),
            (with(r#"{"inner": {"tag": "a"}}"#), "MissingField inner.n"),
            (with(r#"{"xs": [1, "a"]}"#), "TypeMismatch at xs[1]"),
            (with(r#"{"xs": {}}"#), "TypeMismatch at xs"),
            (with(r#"{"m": {"b c": "x"}}"#), "TypeMismatch at m[\"b c\"]"),
            (with(r#"{"m": []}"#), "TypeMismatch at m"),
            (with(r#"{"t": [1, 2]}"#), "TypeMismatch at t[1]"),
            (
                with(r#"{"t": [1, "s", 3]}"#),
                "Other expected an array of 2 elements for a tuple, got 3 at t",
            ),
            (
                with(r#"{"d": "2026-13-01"}"#),
                "Other invalid date '2026-13-01' (expected YYYY-MM-DD): input is out of range at d",
            ),
            (with(r#"{"d": 20261019}"#), "TypeMismatch at d"),
            (
                with(r#"{"tm": "25:00"}"#),
                "Other invalid time '25:00' (expected HH:MM:SS): input is out of range at tm",
            ),
            (
                with(r#"{"at": "2016-12-31T23:59:60Z"}"#),
                "Other invalid datetime '2016-12-31T23:59:60Z': a leap second (second 60) is not read at at",
            ),
            (
                with(r#"{"rows": [{"n": 1}, {"tag": "a"}]}"#),
                "MissingField rows[1].n",
            ),
            (
                with(r#"{"rows": [{"n": 1, "tag": 5}]}"#),
                "TypeMismatch at rows[0].tag",
            ),
        ];
        let records = records
            .iter()
            .map(|(text, expected)| (&cfg, text.as_str(), *expected));
        // A field whose type holds `Bytes`, which neither format has.
        let no_bytes = [
            (
                r#"{"b": []}"#,
                "Other cannot decode field `b` of `B`: a value of type List(Bytes) cannot be decoded",
            ),
            (
                r#"{}"#,
                "Other cannot decode field `b` of `B`: a value of type List(Bytes) cannot be decoded",
            ),
        ];
        let no_bytes = no_bytes
            .iter()
            .map(|(text, expected)| (&bytes, *text, *expected));
        for (ty, text, expected) in records.chain(no_bytes) {
            let Shape::Record(fields) = &ty.shape else {
                unreachable!()
            };
            let json = crate::builtins::json::parse_document(text.as_bytes()).expect("JSON");
            let toml = toml_of(&json).expect("a table");
            let decoded = [
                outcome(record(rt, ty, fields, &json, &At::Top)),
                outcome(record(rt, ty, fields, &toml, &At::Top)),
            ];
            assert_eq!(decoded[0], decoded[1], "{text}");
            assert!(
                decoded[0] == expected || decoded[0].contains(&format!(", {expected}")),
                "{text}: {}",
                decoded[0]
            );
        }
        // A list and a map of records.
        let rows = [
            (
                r#"[{"n": 1}, {"n": 2, "tag": "b"}]"#,
                "[Inner {n: 1, tag: None}, Inner {n: 2, tag: Some(b)}]",
            ),
            (r#"[{"n": 1}, {"n": "2"}]"#, "TypeMismatch at [1].n"),
            (r#"{"n": 1}"#, "TypeMismatch at (top)"),
        ];
        let maps = [
            (r#"{"a": {"n": 1}}"#, "#{\"a\": Inner {n: 1, tag: None}}"),
            (r#"{"a": {"n": 1}, "b": {}}"#, "MissingField b.n"),
            (r#"[1]"#, "TypeMismatch at (top)"),
        ];
        let element = F::Record(inner.id);
        for (shape, cases) in [("list", rows), ("map", maps)] {
            for (text, expected) in cases {
                let json = crate::builtins::json::parse_document(text.as_bytes()).expect("JSON");
                let toml = toml_of(&json).expect("a document");
                let decoded = match shape {
                    "list" => [
                        outcome(list(rt, &element, &json, &At::Top)),
                        outcome(list(rt, &element, &toml, &At::Top)),
                    ],
                    _ => [
                        outcome(map(rt, &element, &json, &At::Top)),
                        outcome(map(rt, &element, &toml, &At::Top)),
                    ],
                };
                assert_eq!(decoded, [expected; 2].map(String::from), "{shape} {text}");
            }
        }
        // Where the formats differ on purpose.
        let json = |text: &str, expected: &FieldType| {
            let doc = crate::builtins::json::parse_document(text.as_bytes()).expect("JSON");
            let decoded = value(rt, &doc, expected, &At::Top.key("v"));
            decoded.map_err(|e| format!("{:?}", e.variant(serde_json::Value::RULES)))
        };
        let toml = |text: &str, expected: &FieldType| {
            let doc: ::toml::Value = ::toml::from_str(text).expect("TOML");
            let doc = doc.get("v").expect("v").clone();
            let decoded = value(rt, &doc, expected, &At::Top.key("v"));
            decoded.map_err(|e| format!("{:?}", e.variant(::toml::Value::RULES)))
        };
        let shown = |r: Result<Value, std::string::String>| match r {
            Ok(value) => format!("Ok({value})"),
            Err(e) => format!("Err({e})"),
        };
        // A whole float: an `Int` by its value to JSON, a float to TOML.
        assert_eq!(shown(json("2.0", &F::Int)), "Ok(2)");
        assert_eq!(
            shown(toml("v = 2.0", &F::Int)),
            "Err(TypeMismatch(\"Int\", \"float at v\"))"
        );
        // A datetime literal: a `Date` as its string is, no `String`.
        assert_eq!(shown(json(r#""2026-10-19""#, &F::Date)), "Ok(2026-10-19)");
        assert_eq!(shown(toml("v = 2026-10-19", &F::Date)), "Ok(2026-10-19)");
        assert_eq!(shown(json(r#""2026-10-19""#, &F::String)), "Ok(2026-10-19)");
        assert_eq!(
            shown(toml("v = 2026-10-19", &F::String)),
            "Err(TypeMismatch(\"String\", \"datetime at v\"))"
        );
        // A null: JSON's is a `None`, and no `String`.
        assert_eq!(
            shown(json("null", &F::Option(Box::new(F::Int)))),
            "Ok(None)"
        );
        assert_eq!(
            shown(json("null", &F::String)),
            "Err(TypeMismatch(\"String\", \"null at v\"))"
        );
        // A `json.Value` is decoded from JSON only.
        assert_eq!(
            shown(json("[1, 2.5]", &F::JsonValue)),
            "Ok(Array([Int(1), Float(2.5)]))"
        );
        assert_eq!(
            shown(toml("v = [1, 2.5]", &F::JsonValue)),
            "Err(Other(\"a value of type json.Value cannot be decoded\"))"
        );
    }
}
