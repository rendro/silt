//! The one typed decoder of a document: json's, toml's, and a postgres
//! `json` or `jsonb` cell's (through json's rules). What a value of a
//! document is called in an error, the path that ends each error, the
//! text of a date and a time, and how a record type is looked up are
//! decided here for every format.

use std::cell::RefCell;
use std::sync::{Arc, OnceLock};

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};

use super::registry::decoding::{Family, ROW_TYPE_HELP};
use super::registry::registry;
use super::time::{LEAP_SECOND, is_leap_second, leap_datetime_message, out_of_range};
use super::typed::Type;
use crate::defs::TypeId;
use crate::typeinfo::{BuiltinVariant, FieldType, Shape, TypeInfo, bv, ty};
use crate::value::Value;
use crate::vm::{Runtime, VmError};

pub(crate) mod seed;

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

    /// The keys and indexes from the document to here.
    pub(crate) fn path(&self) -> Vec<Seg> {
        let mut path = Vec::new();
        let mut here = self;
        loop {
            match here {
                At::Top => break,
                At::Key(up, key) => {
                    path.push(Seg::Key(key.to_string()));
                    here = up;
                }
                At::Index(up, index) => {
                    path.push(Seg::Index(*index));
                    here = up;
                }
            }
        }
        path.reverse();
        path
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

/// A step of a path into a document: the value of a key, or an
/// element of an array.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Seg {
    Key(std::string::String),
    Index(usize),
}

/// Where in a document a decoder's error is: the path of the value it
/// is about (of a missing field, the table it is missing from), and the
/// byte offset of that value in the text, once the reader of a text has
/// found it there ([`Error::locate`]; 0 until then, and for a
/// `json.Value`, which has no text), with its line and column
/// ([`Error::count_lines`]; 0 where it has no place in a text).
#[derive(Clone, Debug, Default)]
pub(crate) struct Where {
    pub(crate) path: Vec<Seg>,
    pub(crate) offset: usize,
    /// The 1-based line and character column of `offset`.
    pub(crate) line: usize,
    pub(crate) column: usize,
    /// Whether the error is about the key at the end of the path, not
    /// its value: a key no field names is found where the key is.
    pub(crate) key: bool,
    /// The offset of the value when the reader knew it as it read
    /// ([`Error::at_offset`]): of a key written twice, a value the path
    /// would not find (it finds the last).
    pub(crate) exact: Option<usize>,
}

impl Where {
    /// The value `at` a path, not found in a text yet.
    pub(crate) fn of(at: &At) -> Where {
        Where {
            path: at.path(),
            offset: 0,
            line: 0,
            column: 0,
            key: false,
            exact: None,
        }
    }

    /// The key at the end of the path `at`, not found in a text yet.
    pub(crate) fn key(at: &At) -> Where {
        Where {
            key: true,
            ..Where::of(at)
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
pub(crate) fn parse_date(s: &str) -> Result<NaiveDate, chrono::ParseError> {
    NaiveDate::parse_from_str(&signed_year(s), DATE)
}

/// How a document writes a date, a time of day (with seconds or without)
/// and a date and time (with an offset or without).
const DATE: &str = "%Y-%m-%d";
const TIME: &str = "%H:%M:%S%.f";
const TIME_MINUTES: &str = "%H:%M";
const DATETIME_OFFSET: [&str; 2] = ["%Y-%m-%dT%H:%M:%S%.f%z", "%Y-%m-%d %H:%M:%S%.f%z"];
const DATETIME: [&str; 3] = [
    "%Y-%m-%dT%H:%M:%S%.f",
    "%Y-%m-%d %H:%M:%S%.f",
    "%Y-%m-%dT%H:%M",
];

/// Why the text of a time of day is none: it is not written as one
/// (`Shape`, in chrono's words), or it is and its fields make none
/// (`Value`: an hour 25, a leap second).
#[derive(Debug, PartialEq)]
pub(crate) enum Unread {
    Shape(std::string::String),
    Value(std::string::String),
}

/// The Url a decoder reads from the string `text`, or the message of
/// its error: a text that is no URL, or a URL longer than a string may
/// be (its normal form escapes what it holds: `é` is six bytes).
pub(crate) fn decoded_url(text: &str) -> Result<crate::url::Url, std::string::String> {
    let url = crate::url::parse(text).map_err(invalid_url_message)?;
    let len = url.as_str().len();
    match len > crate::value::MAX_TEXT_LEN {
        true => Err(format!(
            "invalid url.Url: longer than a string may be ({len} bytes > {} limit)",
            crate::value::MAX_TEXT_LEN
        )),
        false => Ok(url),
    }
}

/// The message of a decoder's error for a string that is no URL: what is
/// wrong and where, without the text, which may hold a password.
fn invalid_url_message(why: crate::url::UrlError) -> std::string::String {
    match why {
        crate::url::UrlError::Invalid(what, at) => {
            format!("invalid url.Url: {what} (character {at})")
        }
        crate::url::UrlError::InvalidPart(what) => format!("invalid url.Url: {what}"),
    }
}

/// The message of a decoder's error for `s`, the string of a date that
/// is none, `why`: how a date is written, for a text that is not
/// written so; else the field that is out of its range.
fn invalid_date_message(s: &str, why: chrono::ParseError) -> std::string::String {
    let range = match date_written(s) == Some("") {
        true => out_of_range(&signed_year(s), DATE, "date", why),
        false => None,
    };
    match range {
        Some(range) => format!("invalid date '{s}': {range}"),
        None => format!("invalid date '{s}' (expected YYYY-MM-DD): {why}"),
    }
}

/// The message of a decoder's error for `s`, the string of a time of
/// day that is none, `why`.
fn invalid_time_message(s: &str, why: &Unread) -> std::string::String {
    match why {
        Unread::Shape(why) => format!("invalid time '{s}' (expected HH:MM:SS): {why}"),
        Unread::Value(why) => format!("invalid time '{s}': {why}"),
    }
}

/// The time of day that the text `s` of a document writes: `HH:MM:SS`,
/// with a fraction of a second up to nanoseconds (`13:14:15.25`), or
/// `HH:MM`. Else why it is none: a second 60 is a leap second, which is
/// refused.
pub(crate) fn parse_time(s: &str) -> Result<NaiveTime, Unread> {
    let t = NaiveTime::parse_from_str(s, TIME).or_else(|full| {
        NaiveTime::parse_from_str(s, TIME_MINUTES).map_err(|short| {
            let range = [(TIME, full), (TIME_MINUTES, short)]
                .into_iter()
                .find_map(|(pattern, err)| out_of_range(s, pattern, "time", err));
            match range.filter(|_| time_written(s) == Some("")) {
                Some(range) => Unread::Value(range),
                None => Unread::Shape(short.to_string()),
            }
        })
    })?;
    match is_leap_second(t) {
        true => Err(Unread::Value(LEAP_SECOND.into())),
        false => Ok(t),
    }
}

/// The date and time that the text `s` of a document writes, in UTC
/// where it has an offset: RFC 3339, the same with a space for the `T`,
/// or without an offset; seconds with a fraction up to nanoseconds, or
/// no seconds (`2024-03-09T07:30`); the year as in [`parse_date`]. Else
/// the message of the decoder's error: a second 60 is a leap second,
/// which is refused, and a text written so whose field is out of its
/// range says which.
pub(crate) fn parse_datetime(s: &str) -> Result<NaiveDateTime, std::string::String> {
    let signed = signed_year(s);
    let read = &*signed;
    let offset = DATETIME_OFFSET.into_iter();
    let naive = DATETIME.into_iter();
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
        None => Err(match datetime_out_of_range(s, read) {
            Some(range) => format!("invalid datetime '{s}': {range}"),
            None => format!("invalid datetime '{s}'"),
        }),
    }
}

/// Why `s` (`read` with its year signed as [`parse_date`] reads it),
/// written as a date and time with or without an offset, is none: the
/// field out of its range. `None` for a text not written so. An offset
/// `Z` is read as `+00:00`, which chrono's `%z` reads.
fn datetime_out_of_range(s: &str, read: &str) -> Option<std::string::String> {
    if datetime_written(s) != Some("") {
        return None;
    }
    let zulu = match read.strip_suffix(['Z', 'z']) {
        Some(before) => format!("{before}+00:00"),
        None => read.to_string(),
    };
    let offset = DATETIME_OFFSET.into_iter().filter_map(|pattern| {
        let err = chrono::DateTime::parse_from_str(&zulu, pattern).err()?;
        Some((pattern, err, &*zulu))
    });
    let naive = DATETIME.into_iter().filter_map(|pattern| {
        let err = NaiveDateTime::parse_from_str(read, pattern).err()?;
        Some((pattern, err, read))
    });
    naive
        .chain(offset)
        .find_map(|(pattern, err, text)| out_of_range(text, pattern, "datetime", err))
}

/// The text after a run of `min` to `max` ASCII digits at the start of
/// `s`, if it starts with one.
fn after_digits(s: &str, min: usize, max: usize) -> Option<&str> {
    let n = s.bytes().take_while(u8::is_ascii_digit).count();
    (min..=max).contains(&n).then(|| &s[n..])
}

/// What follows a date at the start of `s` written as a document writes
/// one, whatever its fields hold: a year of 4 or more digits, or of any
/// number after a sign, `-`, a month of 1 or 2 digits, `-`, a day of 1
/// or 2. `None` if `s` starts with none: a text a message then says how
/// a date is written for.
fn date_written(s: &str) -> Option<&str> {
    let (fewest, s) = match s.strip_prefix(['+', '-']) {
        Some(unsigned) => (1, unsigned),
        None => (4, s),
    };
    let s = after_digits(s, fewest, usize::MAX)?;
    let s = after_digits(s.strip_prefix('-')?, 1, 2)?;
    after_digits(s.strip_prefix('-')?, 1, 2)
}

/// What follows a time of day at the start of `s` written as a document
/// writes one: hours and minutes of 1 or 2 digits each, then seconds, with
/// a fraction, or none. `None` if `s` starts with none.
fn time_written(s: &str) -> Option<&str> {
    let s = after_digits(s, 1, 2)?;
    let s = after_digits(s.strip_prefix(':')?, 1, 2)?;
    let Some(seconds) = s.strip_prefix(':') else {
        return Some(s);
    };
    let s = after_digits(seconds, 1, 2)?;
    match s.strip_prefix('.') {
        Some(fraction) => after_digits(fraction, 1, usize::MAX),
        None => Some(s),
    }
}

/// What follows a date and time at the start of `s` written as a
/// document writes one: a date, `T` or a space, a time of day, and an
/// offset (`Z`, `+hh:mm`, `+hhmm`) or none. `None` if `s` starts
/// with none.
fn datetime_written(s: &str) -> Option<&str> {
    let s = date_written(s)?.strip_prefix(['T', 't', ' '])?;
    let s = time_written(s)?;
    if let Some(after) = s.strip_prefix(['Z', 'z']) {
        return Some(after);
    }
    let Some(offset) = s.strip_prefix(['+', '-']) else {
        return Some(s);
    };
    match after_digits(offset, 4, 4) {
        Some(after) => Some(after),
        None => after_digits(after_digits(offset, 2, 2)?.strip_prefix(':')?, 2, 2),
    }
}

// ── Record types ────────────────────────────────────────────────────

/// The record type `ty` a decoder builds, with its fields. `caller` is
/// the builtin on whose behalf the type is looked up (`json.parse`,
/// `toml.parse_list`, ...); it starts the error message. A builtin
/// record type (`Date`) has no decoder as a record, but for `Instant` and
/// `Duration`, which are read back as the records they are written as.
pub(crate) fn decodable_record<'t>(
    caller: &str,
    ty: &'t TypeInfo,
) -> Result<&'t [(std::string::String, FieldType)], VmError> {
    match &ty.shape {
        Shape::Record(fields)
            if !ty.is_builtin() || [ty::INSTANT, ty::DURATION].contains(&ty.id) =>
        {
            Ok(fields)
        }
        _ => Err(VmError::new(format!(
            "{caller}: unknown record type '{}'",
            ty.name
        ))),
    }
}

/// Whether `ty` is an enum type a decoder builds: one of the program's
/// (a builtin enum, such as `Weekday`, has no decoder).
pub(crate) fn decodable_enum(ty: &TypeInfo) -> bool {
    matches!(ty.shape, Shape::Enum(_)) && !ty.is_builtin()
}

/// `field_type` as silt writes it, a record or an enum type by its name.
pub(crate) fn shown(rt: &Runtime, field_type: &FieldType) -> std::string::String {
    field_type.shown(&|id| {
        rt.type_info(id)
            .map_or_else(Default::default, |info| info.name.clone())
    })
}

/// The value of the type `field_type`, its descriptor, as a `type a`
/// parameter is given it: what a written `decode` of a generic type
/// decodes a type argument by (`a.decode(v)`), and is called with.
pub(crate) fn descriptor(rt: &Runtime, field_type: &FieldType) -> Value {
    let info = |id| rt.type_info(id).map_or(Value::Unit, Value::TypeDescriptor);
    let head = |name| crate::defs::builtin_type_id(name);
    let id = match field_type {
        FieldType::Int => return Value::PrimitiveDescriptor("Int"),
        FieldType::Float => return Value::PrimitiveDescriptor("Float"),
        FieldType::String => return Value::PrimitiveDescriptor("String"),
        FieldType::Bool => return Value::PrimitiveDescriptor("Bool"),
        FieldType::Record(id) | FieldType::Enum(id) => return info(*id),
        FieldType::Date => return info(ty::DATE),
        FieldType::Time => return info(ty::TIME),
        FieldType::DateTime => return info(ty::DATE_TIME),
        FieldType::JsonValue => return info(ty::JSON_VALUE),
        FieldType::Bytes => head("Bytes"),
        FieldType::Url => head("Url"),
        FieldType::List(_) => head("List"),
        FieldType::Option(_) => head("Option"),
        FieldType::Map(_) => head("Map"),
        FieldType::Tuple(_) => head("Tuple"),
        FieldType::Applied(id, _) => Some(*id),
        FieldType::Param(_) | FieldType::KeyedMap(..) | FieldType::Unsupported(_) => None,
    };
    match (id, field_type) {
        (Some(id), FieldType::Bytes | FieldType::Url) => info(id),
        (Some(id), _) => Value::TypeDescriptor(Arc::new(TypeInfo {
            id,
            name: shown(rt, field_type),
            shape: Shape::Applied(field_type.clone()),
        })),
        (None, _) => Value::Unit,
    }
}

// ── Parts a written impl decodes ────────────────────────────────────
//
// A record or enum type the program wrote a `json.Decode` impl for is
// decoded by the impl, wherever it is in the document: the impl is silt
// code, so the decoder cannot call it. A decoding of such a document
// reads it twice (`seed::Parts`, `vm::decode`): first finding each part
// of such a type (the `json.Value` of its document, the impl), then,
// once the impls have given their values, building the value with them
// in those places. The place of a part matters only to an impl's `Err`,
// which the second reading meets there. A postgres row does the same
// over its cells (`Holes`): the parts of all its cells are found, then
// put in, cell after cell.

/// What a decoder reads with: the runtime, and the impls a program wrote
/// for the types it decodes, if it honours them (json does).
pub(crate) struct Reader<'a> {
    rt: &'a Runtime,
    impls: Option<Impls<'a>>,
}

/// The `decode` impls a decoding honours, and what it does at a part of
/// a type with one.
struct Impls<'a> {
    /// The slot of the `decode` the program wrote for a type.
    written: &'a dyn Fn(TypeId, &[FieldType]) -> Option<u16>,
    #[cfg_attr(not(feature = "postgres"), allow(dead_code))]
    holes: RefCell<Holes>,
}

/// What a postgres row's decoding does at a part of a cell of a type
/// with a written impl (`seed::json_cell`).
#[cfg_attr(not(feature = "postgres"), allow(dead_code))]
pub(crate) enum Holes {
    /// Keep the part, in the order the decoder comes to it.
    Collect(Vec<Hole>),
    /// Put there the next of what the impls gave: the value, or the
    /// `JsonError` an impl returned, which is the decoder's error there
    /// (the format's, with the part's path: `Rules::from_impl`).
    Supply(std::vec::IntoIter<Result<Value, Value>>),
}

/// A part of a document of a type with a written `decode`.
pub(crate) struct Hole {
    /// Its document, as a `json.Value`.
    pub(crate) json: Value,
    /// The slot of the type's `decode`.
    pub(crate) slot: u16,
    /// The type's descriptor, for a generic type (`Box(Int)`): what
    /// its `decode` takes its type arguments from. Unit for any other.
    pub(crate) ty: Value,
}

impl std::ops::Deref for Reader<'_> {
    type Target = Runtime;

    fn deref(&self) -> &Runtime {
        self.rt
    }
}

impl<'a> Reader<'a> {
    /// A reader that decodes every type by its structure.
    #[cfg(test)]
    pub(crate) fn new(rt: &'a Runtime) -> Reader<'a> {
        Reader { rt, impls: None }
    }

    /// A reader that honours the impls `written` says, doing `holes` at
    /// a part of a type with one.
    pub(crate) fn with_impls(
        rt: &'a Runtime,
        written: &'a dyn Fn(TypeId, &[FieldType]) -> Option<u16>,
        holes: Holes,
    ) -> Reader<'a> {
        Reader {
            rt,
            impls: Some(Impls {
                written,
                holes: RefCell::new(holes),
            }),
        }
    }

    /// Whether a written impl decodes values of the type `id` at the type
    /// arguments `args`.
    fn written(&self, id: TypeId, args: &[FieldType]) -> bool {
        self.impls
            .as_ref()
            .is_some_and(|impls| (impls.written)(id, args).is_some())
    }

    /// The holes collected.
    #[cfg(feature = "postgres")]
    pub(crate) fn into_holes(self) -> Vec<Hole> {
        match self.impls.map(|impls| impls.holes.into_inner()) {
            Some(Holes::Collect(holes)) => holes,
            _ => Vec::new(),
        }
    }
}

/// The `JsonError` `error`, which a written impl gave for a part `at` a
/// path, as the decoder's error of a value there: a message ends with
/// the path, a missing field's path starts with it. At the top it is as
/// the impl wrote it.
pub(crate) fn at_part(error: &Value, at: &str) -> Value {
    let Value::Variant(variant) = error else {
        return error.clone();
    };
    if at.is_empty() || !variant.of(ty::JSON_ERROR) {
        return error.clone();
    }
    let ended = |text: &Value| match text {
        Value::String(s, _) => Value::string(format!("{s} at {at}")),
        other => other.clone(),
    };
    let mut fields = variant.fields().to_vec();
    if variant.is(bv::JSON_MISSING_FIELD) {
        if let Some(Value::String(field, _)) = fields.first() {
            fields[0] = Value::string(key_path(at, field));
        }
    } else if variant.is(bv::JSON_TYPE_MISMATCH) {
        if let Some(actual) = fields.get_mut(1) {
            *actual = ended(actual);
        }
    } else if let Some(message) = fields.first_mut() {
        // `Syntax(message, offset)`, `Other(message)`.
        *message = ended(message);
    }
    Value::variant(variant.tag(), fields)
}

/// The path of the key `key` of the object at the path `at` (as a
/// decoder's error writes it): `at.key`, `at["a b"]`, or the key alone
/// at the top.
pub(crate) fn key_path(at: &str, key: &str) -> std::string::String {
    let own = At::Top.key(key).to_string();
    match (at.is_empty(), own.starts_with('[')) {
        (true, _) => own,
        (false, true) => format!("{at}{own}"),
        (false, false) => format!("{at}.{own}"),
    }
}

// ── Types no decoder fills ──────────────────────────────────────────
//
// Whether a decoder has a value of the type it is given is decided once,
// before the document is read (a call's type argument; the rows of a
// postgres result once per result), from the family's description in
// the registry: the type's fields, the fields of each variant of an enum,
// and the record and enum types they name, at any depth. A type with a
// field no decoder fills is refused whatever the document holds, and the
// decoding itself walks no type.

/// Where a field is: of a record type, or of a variant of an enum type.
enum Place {
    /// The field of the record type.
    Field(std::string::String, std::string::String),
    /// A field of the variant of the enum type.
    Variant(std::string::String, std::string::String),
}

impl Place {
    /// The message of the error of a decoder for the field here, whose
    /// type (as silt writes it, `declared`) has no decoder.
    fn message(&self, declared: &str) -> std::string::String {
        match self {
            Place::Field(record, field) => unsupported_field_type_message(record, field, declared),
            Place::Variant(ty, variant) => variant_field_message(ty, variant, declared),
        }
    }
}

/// The message of the error of a decoder for a field of the variant
/// `variant` of the enum type `ty` whose type, `declared`, has no
/// decoder.
fn variant_field_message(ty: &str, variant: &str, declared: &str) -> std::string::String {
    format!(
        "cannot decode a field of variant `{variant}` of `{ty}`: a value of type {declared} \
         cannot be decoded"
    )
}

/// Whether `field_type` names an enum type that no written impl decodes
/// (not counting the fields of the record types it names).
fn names_enum(rt: &Reader, field_type: &FieldType) -> bool {
    match field_type {
        FieldType::Enum(id) => !rt.written(*id, &[]),
        FieldType::Applied(id, args) => {
            !rt.written(*id, args) && rt.type_info(*id).is_some_and(|ty| decodable_enum(&ty))
        }
        FieldType::List(t) | FieldType::Option(t) | FieldType::Map(t) => names_enum(rt, t),
        FieldType::Tuple(ts) => ts.iter().any(|t| names_enum(rt, t)),
        _ => false,
    }
}

/// Whether `field_type`, not counting the fields of the record and enum
/// types it names, holds a part that `family` has no decoder for: it is
/// a type no decoder has, or it holds a leaf the family does not decode,
/// or an enum type where the family decodes none (one with a written
/// impl is the impl's).
fn lacks_here(rt: &Reader, family: Family, field_type: &FieldType) -> bool {
    matches!(
        field_type,
        FieldType::Unsupported(_) | FieldType::Param(_) | FieldType::KeyedMap(..)
    ) || family.lacks(field_type).is_some()
        || (!family.decodes_enums() && names_enum(rt, field_type))
}

/// The types looked into by a walk for a field no decoder fills: each
/// at its type arguments, for a family.
type Seen = Vec<(TypeId, Vec<FieldType>, Family)>;

/// How deep the type arguments of an instance a walk for a field no
/// decoder fills meets may be: past it, the type grows with every level
/// (`n: Option(T(List(a)))` in `type T(a)`), which the checker refuses.
/// (The decoding itself meets a field no decoder fills there: its error.)
const ARGUMENT_DEPTH_LIMIT: usize = 32;

/// Whether the type arguments `args` are deeper than
/// [`ARGUMENT_DEPTH_LIMIT`].
fn too_deep(args: &[FieldType]) -> bool {
    fn depth(ty: &FieldType) -> usize {
        1 + match ty {
            FieldType::List(t) | FieldType::Option(t) | FieldType::Map(t) => depth(t),
            FieldType::Tuple(ts) | FieldType::Applied(_, ts) => {
                ts.iter().map(depth).max().unwrap_or(0)
            }
            _ => 0,
        }
    }
    args.iter().any(|arg| depth(arg) > ARGUMENT_DEPTH_LIMIT)
}

/// The first field of a field of type `field_type` that `family` has no
/// decoder for: the field itself (`None`), or the place of a field of a
/// record or enum type it names (which `family.nested()` decodes); with
/// that field's type as silt writes it. `seen` are the types looked into.
fn lacking(
    rt: &Reader,
    family: Family,
    field_type: &FieldType,
    seen: &mut Seen,
) -> Option<(Option<Place>, std::string::String)> {
    if lacks_here(rt, family, field_type) {
        return Some((None, shown(rt, field_type)));
    }
    let mut named = Vec::new();
    field_type.instances(&mut named);
    for (id, args) in named {
        let Some(info) = rt.type_info(id) else {
            continue;
        };
        if let Some((place, declared)) = lacking_in(rt, family.nested(), &info, args, seen) {
            return Some((Some(place), declared));
        }
    }
    None
}

/// The first field of the record or enum type `ty` at its type
/// arguments `args` (in the order it declares them, each variant's in
/// turn), or of a type a field names, that `family` has no decoder for:
/// its place and its type as silt writes it.
fn lacking_in(
    rt: &Reader,
    family: Family,
    ty: &TypeInfo,
    args: &[FieldType],
    seen: &mut Seen,
) -> Option<(Place, std::string::String)> {
    let instance = (ty.id, args.to_vec(), family);
    if seen.contains(&instance) || too_deep(args) || rt.written(ty.id, args) {
        return None;
    }
    seen.push(instance);
    let fields: Vec<(Place, &FieldType)> = match &ty.shape {
        Shape::Record(fields) => (fields.iter())
            .map(|(name, field)| (Place::Field(ty.name.clone(), name.clone()), field))
            .collect(),
        Shape::Enum(variants) => (variants.iter())
            .flat_map(|v| {
                let place = || Place::Variant(ty.name.clone(), v.name.clone());
                v.fields.iter().map(move |field| (place(), field))
            })
            .collect(),
        Shape::Opaque | Shape::Applied(_) => Vec::new(),
    };
    for (here, field) in fields {
        let field = field.instantiated(args);
        if let Some((place, declared)) = lacking(rt, family, &field, seen) {
            return Some((place.unwrap_or(here), declared));
        }
    }
    None
}

/// The message of the error of a decoder of `family` given the record or
/// enum type `ty` at its type arguments `args` (none for a type without
/// parameters) to decode into, if a field no decoder fills is in it (or
/// in a record or enum type it names): whatever the document holds.
pub(crate) fn record_refusal(
    rt: &Reader,
    family: Family,
    ty: &TypeInfo,
    args: &[FieldType],
) -> Option<std::string::String> {
    // (Walked once per type and family: see `Runtime::refusals`. With
    // the impls a program wrote, a type with one is not looked into,
    // which the cache does not say; nor does it say a generic type's
    // arguments.)
    if rt.impls.is_some() || !args.is_empty() {
        return lacking_in(rt, family, ty, args, &mut Vec::new())
            .map(|(place, declared)| place.message(&declared));
    }
    let (at, of) = (ty.id.0.0 as usize, family as usize);
    if let Some(Some(found)) = rt.refusals.read().get(at).map(|known| &known[of]) {
        return found.clone();
    }
    let found = lacking_in(rt, family, ty, &[], &mut Vec::new())
        .map(|(place, declared)| place.message(&declared));
    let mut refusals = rt.refusals.write();
    if refusals.len() <= at {
        refusals.resize(at + 1, Default::default());
    }
    refusals[at][of] = Some(found.clone());
    found
}

/// The message of the error of a decoder of `family` given `target`, if
/// a field no decoder fills is in the type: whatever the document holds,
/// an empty one too.
pub(crate) fn refusal(rt: &Reader, family: Family, target: &Target) -> Option<std::string::String> {
    match target {
        Target::Record(ty) => record_refusal(rt, family, ty, &[]),
        Target::Value(FieldType::Enum(id)) => record_refusal(rt, family, &*rt.type_info(*id)?, &[]),
        Target::Value(field_type) => {
            let (place, declared) = lacking(rt, family, field_type, &mut Vec::new())?;
            // (A type argument that is no record or enum is one the
            // family decodes: `target` took it.)
            place.map(|place| place.message(&declared))
        }
    }
}

/// The error a decoder of the format of `rules` gives for `target`, if
/// a field no decoder fills is in the type ([`refusal`]).
pub(crate) fn refused(rt: &Reader, rules: &Rules, target: &Target) -> Option<Value> {
    let message = refusal(rt, rules.family, target)?;
    Some(result(rules, Err(Error::Other(message, Where::default()))))
}

/// Whether a decoder of `family` decodes a value of `field_type`, in the
/// record and enum types it names too.
#[cfg(feature = "postgres")]
pub(crate) fn decodes(rt: &Reader, family: Family, field_type: &FieldType) -> bool {
    lacking(rt, family, field_type, &mut Vec::new()).is_none()
}
/// The record type `id` a record field names, from the program's types.
fn field_record_type(rt: &Runtime, caller: &str, id: TypeId) -> Result<Arc<TypeInfo>, VmError> {
    rt.type_info(id)
        .ok_or_else(|| VmError::new(format!("{caller}: unknown record type")))
}

// ── The rules of a format ─────────────────────────────────────────

/// How a format's documents decode, where formats differ.
pub(crate) struct Rules {
    /// The builtin a record type in a document is looked up for: it
    /// starts the message of a type that is none.
    pub(crate) caller: &'static str,
    /// What a record and a map expect, the kind of a table.
    pub(crate) table: &'static str,
    /// What an enum expects: the string of a variant's name, or the
    /// table of a variant with fields.
    pub(crate) variant: &'static str,
    /// How a number that is no `Int` is told from one.
    pub(crate) numbers: Numbers,
    /// The family of decoders of the format, which says the field types
    /// it decodes.
    pub(crate) family: Family,
    /// The variants `TypeMismatch`, `MissingField` and `Other` of the
    /// format's error (`JsonError`, `TomlError`).
    pub(crate) errors: [BuiltinVariant; 3],
    /// Whether the format's errors end with the byte offset of their
    /// value in the text (json's and toml's do; csv's say a line and a
    /// column in their text).
    pub(crate) offsets: bool,
    /// The variant of the format's error for a key of a record's table
    /// that no field names, where the format refuses one (TOML, which
    /// people write by hand); `None` where such a key is skipped (JSON
    /// and CSV, which programs write and add to).
    pub(crate) unknown: Option<BuiltinVariant>,
    /// The format's error of the `JsonError` that a written
    /// `json.Decode` impl gave for a part `at` a path ([`at_part`]).
    pub(crate) from_impl: fn(&Value, &str) -> Value,
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
    TypeMismatch(&'static str, std::string::String, Where),
    /// `MissingField(path)`: the missing field's own path; where is the
    /// table it is missing from.
    MissingField(std::string::String, Where),
    /// `Other(message)`.
    Other(std::string::String, Where),
    /// `UnknownField(path, expected)`: a key of a record's table that no
    /// field of the record names, where the format refuses one
    /// ([`Rules::unknown`]), and the keys the table takes.
    UnknownField(std::string::String, Vec<std::string::String>, Where),
    /// A value was present for a type no decoder exists for (the type as
    /// written): [`record`] turns it into an `Other` that names the field
    /// whose type it is.
    Unsupported(std::string::String),
    /// The error a written `json.Decode` impl gave for a part, as the
    /// format's error variant; where is the part.
    Given(Value, Where),
}

impl From<VmError> for Error {
    fn from(e: VmError) -> Self {
        Error::Other(e.message, Where::default())
    }
}

impl Error {
    /// The type mismatch of a value `at` a path that is `actual`.
    pub(crate) fn mismatch(expected: &'static str, actual: &str, at: &At) -> Error {
        Error::TypeMismatch(expected, format!("{actual}{}", at.suffix()), Where::of(at))
    }

    /// The `Other` of a value `at` a path: `message` ends with the path.
    fn other(message: impl std::fmt::Display, at: &At) -> Error {
        Error::Other(format!("{message}{}", at.suffix()), Where::of(at))
    }

    /// Where the error is in the document, for one that says.
    fn place_mut(&mut self) -> Option<&mut Where> {
        match self {
            Error::TypeMismatch(_, _, at)
            | Error::MissingField(_, at)
            | Error::Other(_, at)
            | Error::UnknownField(_, _, at)
            | Error::Given(_, at) => Some(at),
            Error::Unsupported(_) => None,
        }
    }

    /// The error with the byte offset of its value in the text a reader
    /// read, which `find` gives for the path of the value (and whether
    /// it is the key at its end that is meant).
    pub(crate) fn locate(&mut self, find: impl FnOnce(&[Seg], bool) -> usize) {
        if let Some(at) = self.place_mut() {
            at.offset = match at.exact {
                Some(offset) => offset,
                None => find(&at.path, at.key),
            };
        }
    }

    /// The error with the offset `offset` of its value, where the reader
    /// knows it, found instead of by its path ([`Error::locate`]).
    pub(crate) fn at_offset(mut self, offset: Option<usize>) -> Error {
        if let Some(at) = self.place_mut() {
            at.exact = offset;
        }
        self
    }

    /// The error with the line and the column of its offset in `text`,
    /// the text its offset was found in ([`Error::locate`]).
    pub(crate) fn count_lines(&mut self, text: &[u8]) {
        if let Some(at) = self.place_mut() {
            (at.line, at.column) = line_column(text, at.offset);
        }
    }

    /// The error with its offset `by` bytes later: of a text read after
    /// as many bytes (a byte order mark).
    pub(crate) fn shift(&mut self, by: usize) {
        if let Some(at) = self.place_mut() {
            at.offset += by;
        }
    }

    /// The variant of the error of the format of `rules`.
    pub(crate) fn variant(self, rules: &Rules) -> Value {
        let [mismatch, missing, other] = rules.errors;
        let string = |s: std::string::String| Value::string(s);
        // (The offset, the line and the column, last, where the format's
        // errors have them.)
        let fields = |mut fields: Vec<Value>, at: &Where| {
            if rules.offsets {
                fields.extend(place(at.offset, at.line, at.column));
            }
            fields
        };
        match self {
            Error::TypeMismatch(expected, actual, at) => Value::variant(
                mismatch,
                fields(vec![Value::string(expected), string(actual)], &at),
            ),
            Error::MissingField(path, at) => {
                Value::variant(missing, fields(vec![string(path)], &at))
            }
            Error::Other(message, at) => Value::variant(other, fields(vec![string(message)], &at)),
            Error::UnknownField(path, expected, at) => match rules.unknown {
                Some(unknown) => {
                    let expected = Value::list(expected.into_iter().map(string).collect());
                    Value::variant(unknown, fields(vec![string(path), expected], &at))
                }
                // (Only a format that refuses such a key has the error.)
                None => Value::variant(
                    other,
                    fields(vec![string(format!("unknown field {path}"))], &at),
                ),
            },
            // An impl's error is placed at its part: what offset, line
            // and column the impl gave, it read no text.
            Error::Given(error, at) => match &error {
                Value::Variant(given) if rules.offsets && given.fields().len() >= 3 => {
                    let mut fields = given.fields().to_vec();
                    let own = fields.len() - 3;
                    fields.truncate(own);
                    fields.extend(place(at.offset, at.line, at.column));
                    Value::variant(given.tag(), fields)
                }
                _ => error,
            },
            // (Only a record's field holds such a type, and `record`
            // names it: this is the fallback.)
            Error::Unsupported(declared) => Value::variant(
                other,
                fields(
                    vec![string(format!(
                        "a value of type {declared} cannot be decoded"
                    ))],
                    &Where::default(),
                ),
            ),
        }
    }
}

/// The 1-based line and the 1-based column, counted in characters, of
/// the byte `offset` of `text` (as silt's own errors count them); a
/// byte order mark at the start is no column. An offset past the end is
/// the end.
pub(crate) fn line_column(text: &[u8], offset: usize) -> (usize, usize) {
    let before = &text[..offset.min(text.len())];
    let line_start = before
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(0, |at| at + 1);
    let line = 1 + before.iter().filter(|b| **b == b'\n').count();
    let mut part = &before[line_start..];
    if line_start == 0 {
        part = part.strip_prefix("\u{feff}".as_bytes()).unwrap_or(part);
    }
    // (A byte that starts a character: no continuation byte.)
    let column = 1 + part.iter().filter(|b| (**b & 0xC0) != 0x80).count();
    (line, column)
}

/// The last three fields of a `JsonError` and a `TomlError`: the byte
/// `offset` and its `line` and `column` (0 and 0 where it has no place
/// in a text).
pub(crate) fn place(offset: usize, line: usize, column: usize) -> [Value; 3] {
    let int = |n: usize| Value::Int(i64::try_from(n).unwrap_or(i64::MAX));
    [int(offset), int(line), int(column)]
}

/// The place of an error of no place in a text: `toml.stringify`'s, or
/// one of a `json.Value` (`json.decode`).
pub(crate) const NO_PLACE: [Value; 3] = [Value::Int(0), Value::Int(0), Value::Int(0)];

/// `Ok` of the value `decoded`, or `Err` of its error in the format of
/// `rules`.
pub(crate) fn result(rules: &Rules, decoded: Result<Value, Error>) -> Value {
    match decoded {
        Ok(value) => Value::variant(bv::OK, [value]),
        Err(e) => Value::variant(bv::ERR, [e.variant(rules)]),
    }
}

/// What the type argument of a decoding builtin names, as its decoder
/// decodes it.
pub(crate) enum Target<'a> {
    /// A primitive type, or `json.Value`: decoded as a field of the type
    /// is.
    Value(FieldType),
    /// A record type of the program.
    Record(&'a Arc<TypeInfo>),
}

impl Target<'_> {
    /// The type a decoder decodes the document as: of the elements a
    /// `toml.parse_list` decodes, of the values a `toml.parse_map` decodes.
    pub(crate) fn element(&self) -> FieldType {
        match self {
            Target::Value(field_type) => field_type.clone(),
            Target::Record(ty) => FieldType::Record(ty.id),
        }
    }
}

/// A decoding builtin as its body names it: what it decodes is its
/// registry row's [`Decoding`], looked up once.
///
/// [`Decoding`]: super::registry::Decoding
pub(crate) struct Decoder {
    /// Its name as a program calls it (`json.parse`), which begins its
    /// errors.
    pub(crate) name: &'static str,
    decoding: OnceLock<Option<super::registry::Decoding>>,
}

impl Decoder {
    pub(crate) const fn new(name: &'static str) -> Decoder {
        Decoder {
            name,
            decoding: OnceLock::new(),
        }
    }

    /// What its registry row says it decodes.
    fn decoding(&self) -> Option<super::registry::Decoding> {
        *self
            .decoding
            .get_or_init(|| registry().named(self.name).and_then(|row| row.decoding))
    }
}

/// What the type argument `ty` of the decoding builtin `decoder` names:
/// a type its registry row's [`Decoding`] says it decodes into, or the
/// error of the program. (The checker refuses the others wherever a
/// program gives one, `Pred::Builtin`: this is the decoders' guard.)
///
/// [`Decoding`]: super::registry::Decoding
pub(crate) fn target<'a>(decoder: &Decoder, ty: Type<'a>) -> Result<Target<'a>, VmError> {
    let caller = decoder.name;
    let decoding = decoder
        .decoding()
        .ok_or_else(|| VmError::new(format!("{caller} decodes no type")))?;
    // (The texts of the errors are written only for an error.)
    let accepted = || decoding.type_arguments(str::to_string, str::to_string);
    // A row is decoded into a record of its columns: the error says what
    // the type is not.
    let not_a_row = |name: &str| {
        VmError::new(format!(
            "{caller}: a row decodes into {}, not {name}; {ROW_TYPE_HELP}",
            accepted()
        ))
    };
    match ty {
        Type::Field(field_type) if decoding.primitives => Ok(Target::Value(field_type)),
        // A generic record or enum type at its arguments (`Box(Int)`),
        // which a decoder of records decodes as it does a record type.
        Type::Field(field_type @ FieldType::Applied(..)) => Ok(Target::Value(field_type)),
        Type::Field(field_type) => Err(match decoding.family.rows() {
            true => not_a_row(&field_type.shown(&|_| std::string::String::new())),
            false => VmError::new(format!("{caller}: type argument must be {}", accepted())),
        }),
        Type::Named(ty) if ty.id == ty::JSON_VALUE && decoding.json_value() => {
            Ok(Target::Value(FieldType::JsonValue))
        }
        Type::Named(ty) if decoding.family.decodes_enums() && decodable_enum(ty) => {
            Ok(Target::Value(FieldType::Enum(ty.id)))
        }
        Type::Named(ty) => match decodable_record(caller, ty) {
            Ok(_) => Ok(Target::Record(ty)),
            Err(_) if decoding.family.rows() => Err(not_a_row(&ty.name)),
            Err(e) => Err(e),
        },
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
        let leap = || Err(Unread::Value(LEAP_SECOND.to_string()));
        assert_eq!(parse_time("23:59:60"), leap());
        assert_eq!(parse_time("12:30:60.5"), leap());
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
            Err(Error::TypeMismatch(_, actual, _)) => format!("TypeMismatch at {}", path(&actual)),
            Err(Error::MissingField(at, _)) => format!("MissingField {at}"),
            Err(Error::Other(message, _)) => format!("Other {message}"),
            Err(Error::UnknownField(at, _, _)) => format!("UnknownField {at}"),
            Err(Error::Unsupported(declared)) => format!("Unsupported {declared}"),
            Err(Error::Given(error, _)) => format!("Given {error}"),
        }
    }

    /// What the decoder makes of the JSON text `text` and of the TOML
    /// text `toml` as a value of `expected`.
    fn decoded(rt: &Reader, expected: &FieldType, json: &str, toml: &str) -> [String; 2] {
        let parts = seed::Parts::finding();
        let from_json = match seed::json_text(rt, expected, json.as_bytes(), &parts) {
            Ok(decoded) => outcome(decoded),
            Err(e) => format!("Syntax {e}"),
        };
        let top = seed::Top::One(expected);
        let from_toml = match crate::builtins::toml::read_text(rt, &top, toml, &parts) {
            Ok(decoded) => outcome(decoded),
            Err(result) => format!("Syntax {result}"),
        };
        [from_json, from_toml]
    }

    /// A document written in JSON and the same document in TOML decode
    /// alike into a record and a map: to the same value, or to an error
    /// of the same kind at the same path. Where the formats differ on
    /// purpose (a whole float, a datetime literal, null, `json.Value`)
    /// each does what its documentation says.
    #[test]
    fn json_and_toml_decode_a_document_of_one_shape_alike() {
        use crate::defs::DefId;
        use FieldType as F;
        let vm = crate::vm::Vm::new(crate::vm::HostIo::process());
        let rt = &Reader::new(&vm.runtime);
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
        let object = |text: &str| match serde_json::from_str(text).expect("JSON") {
            serde_json::Value::Object(members) => members,
            other => panic!("{other}"),
        };
        // The whole document with the members of `change` in its place,
        // a member that is null left out.
        let with = |change: &str| {
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
                "Cfg { name: \"x\", inner: Inner { n: 2, tag: None }, xs: [1, 2], \
                 m: #{ \"a\": 1.0, \"b c\": 2.5 }, t: (1, \"s\"), d: 2026-10-19, \
                 tm: 12:30:00.250, at: 2026-10-19T10:30:00, o: None, \
                 rows: [Inner { n: 1, tag: Some(\"a\") }, Inner { n: 2, tag: None }] }",
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
                "Other invalid date '2026-13-01': the month is out of range at d",
            ),
            (
                with(r#"{"d": "11-23-2026"}"#),
                "Other invalid date '11-23-2026' (expected YYYY-MM-DD): input is out of range at d",
            ),
            (
                with(r#"{"d": "2025-13-01x"}"#),
                "Other invalid date '2025-13-01x' (expected YYYY-MM-DD): input is out of range at d",
            ),
            (with(r#"{"d": 20261019}"#), "TypeMismatch at d"),
            (
                with(r#"{"tm": "25:00"}"#),
                "Other invalid time '25:00': the hour is out of range at tm",
            ),
            (
                with(r#"{"tm": "930"}"#),
                "Other invalid time '930' (expected HH:MM:SS): input is out of range at tm",
            ),
            (
                with(r#"{"at": "2026-02-30T10:00:00Z"}"#),
                "Other invalid datetime '2026-02-30T10:00:00Z': the day is out of range: February 2026 has 28 days at at",
            ),
            (
                with(r#"{"at": "11-23-2026 10:00"}"#),
                "Other invalid datetime '11-23-2026 10:00' at at",
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
        let toml_text = |json: &str| {
            let doc = serde_json::from_str(json).expect("JSON");
            ::toml::to_string(&toml_of(&doc).expect("a table")).expect("TOML")
        };
        for (text, expected) in &records {
            let decoded = decoded(rt, &F::Record(cfg.id), text, &toml_text(text));
            assert_eq!(decoded[0], decoded[1], "{text}");
            assert!(
                decoded[0] == *expected || decoded[0].contains(&format!(", {expected}")),
                "{text}: {}",
                decoded[0]
            );
        }
        // A field whose type holds `Bytes`, which neither format has: the
        // type is refused before the document is read.
        for family in [Family::Json, Family::Toml] {
            assert_eq!(
                refusal(rt, family, &Target::Record(&bytes)).as_deref(),
                Some(
                    "cannot decode field `b` of `B`: a value of type List(Bytes) cannot be decoded"
                )
            );
        }
        // A map of records.
        let maps = [
            (
                r#"{"a": {"n": 1}}"#,
                "#{ \"a\": Inner { n: 1, tag: None } }",
            ),
            (r#"{"a": {"n": 1}, "b": {}}"#, "MissingField b.n"),
        ];
        let map = F::Map(Box::new(F::Record(inner.id)));
        for (text, expected) in maps {
            let decoded = decoded(rt, &map, text, &toml_text(text));
            assert_eq!(decoded, [expected; 2].map(String::from), "map {text}");
        }
        // Where the formats differ on purpose: a field `v` of each type.
        let shown = |expected: &FieldType, json: &str, toml: &str| {
            static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(9100);
            let id = TypeId(DefId(
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            ));
            let v = TypeInfo::new_record(id, "V", vec![("v".into(), expected.clone())]);
            rt.types.lock().insert(v.clone());
            let json = format!("{{\"v\": {json}}}");
            decoded(rt, &F::Record(id), &json, toml)
        };
        // A whole float: an `Int` by its value to JSON, a float to TOML.
        assert_eq!(
            shown(&F::Int, "2.0", "v = 2.0"),
            ["V { v: 2 }", "TypeMismatch at v"]
        );
        // A datetime literal: a `Date` as its string is, no `String`.
        assert_eq!(
            shown(&F::Date, r#""2026-10-19""#, "v = 2026-10-19"),
            ["V { v: 2026-10-19 }"; 2]
        );
        assert_eq!(
            shown(&F::String, r#""2026-10-19""#, "v = 2026-10-19"),
            ["V { v: \"2026-10-19\" }", "TypeMismatch at v"]
        );
        // A null: JSON's is a `None` and no `String`; TOML leaves it out.
        assert_eq!(
            shown(&F::Option(Box::new(F::Int)), "null", ""),
            ["V { v: None }"; 2]
        );
        assert_eq!(shown(&F::String, "null", "")[0], "TypeMismatch at v");
        // A `json.Value` is decoded from both: TOML's array is a
        // `json.Array` of its values.
        assert_eq!(
            shown(&F::JsonValue, "[1, 2.5]", "v = [1, 2.5]"),
            ["V { v: Array([Int(1), Float(2.5)]) }"; 2]
        );
    }

    /// Lines and columns count from 1, columns in characters; a byte
    /// order mark is no column; an offset past the end is the end.
    #[test]
    fn a_byte_has_its_line_and_character_column() {
        let line_column = super::line_column;
        let text = "ab\nλx\r\n\ny".as_bytes();
        assert_eq!(line_column(text, 0), (1, 1));
        assert_eq!(line_column(text, 2), (1, 3));
        assert_eq!(line_column(text, 3), (2, 1));
        // `λ` is two bytes, one column.
        assert_eq!(line_column(text, 5), (2, 2));
        assert_eq!(line_column(text, 6), (2, 3));
        assert_eq!(line_column(text, 8), (3, 1));
        assert_eq!(line_column(text, 9), (4, 1));
        assert_eq!(line_column(text, 10), (4, 2));
        assert_eq!(line_column(text, 99), (4, 2));
        let marked = "\u{feff}ab".as_bytes();
        assert_eq!(line_column(marked, 3), (1, 1));
        assert_eq!(line_column(marked, 4), (1, 2));
        assert_eq!(line_column(b"", 0), (1, 1));
    }
}
