//! The one typed decoder of a document: json's, toml's, and a postgres
//! `json` or `jsonb` cell's (through json's rules). What a value of a
//! document is called in an error, the path that ends each error, the
//! text of a date and a time, and how a record type is looked up are
//! decided here for every format.

use std::cell::RefCell;
use std::sync::{Arc, OnceLock};

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};

use super::registry::decoding::{Family, Leaf, ROW_TYPE_HELP};
use super::registry::registry;
use super::time::{
    LEAP_SECOND, is_leap_second, leap_datetime_message, make_date, make_datetime, make_time,
};
use super::typed::Type;
use crate::defs::TypeId;
use crate::typeinfo::{BuiltinVariant, FieldType, Shape, Tag, TypeInfo, bv, ty};
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
pub(crate) fn parse_date(s: &str) -> Result<NaiveDate, chrono::ParseError> {
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
        FieldType::List(_) => head("List"),
        FieldType::Option(_) => head("Option"),
        FieldType::Map(_) => head("Map"),
        FieldType::Tuple(_) => head("Tuple"),
        FieldType::Applied(id, _) => Some(*id),
        FieldType::Param(_) | FieldType::KeyedMap(..) | FieldType::Unsupported(_) => None,
    };
    match (id, field_type) {
        (Some(id), FieldType::Bytes) => info(id),
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
// reads it twice (`vm::decode`): first collecting each part of such a
// type (`Holes::Collect`: the `json.Value` of its document, the impl),
// then, once the impls have given their values, building the value with
// them in those places (`Holes::Supply`), in the same order. The place
// of a part matters only to an impl's `Err`, which the second reading
// meets there.

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
    holes: RefCell<Holes>,
}

/// What a decoding does at a part of a type with a written impl.
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

    /// The value of the part `doc`, `at` a path, of the type `expected`
    /// (a record or enum type, a generic one at its type arguments), if
    /// a written impl decodes it: a stand-in while the parts are
    /// collected, then what the impl gave. A part with no `json.Value`
    /// (a TOML float that is no number) is the decoder's error.
    fn hole<D: Document>(
        &self,
        expected: &FieldType,
        doc: &D,
        at: &At,
    ) -> Option<Result<Value, Error>> {
        let (id, args) = match expected {
            FieldType::Record(id) | FieldType::Enum(id) => (id, &[][..]),
            FieldType::Applied(id, args) => (id, args.as_slice()),
            _ => return None,
        };
        let impls = self.impls.as_ref()?;
        let slot = (impls.written)(*id, args)?;
        Some(match &mut *impls.holes.borrow_mut() {
            Holes::Collect(holes) => json_part(doc, at).map(|json| {
                let ty = match expected {
                    FieldType::Applied(..) => descriptor(self.rt, expected),
                    _ => Value::Unit,
                };
                holes.push(Hole { json, slot, ty });
                Value::Unit
            }),
            Holes::Supply(values) => match values.next() {
                Some(Ok(value)) => Ok(value),
                Some(Err(error)) => {
                    Err(Error::Given((D::RULES.from_impl)(&error, &at.to_string())))
                }
                None => Ok(Value::Unit),
            },
        })
    }

    /// The holes collected.
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
        Value::String(s) => Value::String(format!("{s} at {at}").into()),
        other => other.clone(),
    };
    let mut fields = variant.fields().to_vec();
    if variant.is(bv::JSON_MISSING_FIELD) {
        if let Some(Value::String(field)) = fields.first() {
            fields[0] = Value::String(key_path(at, field).into());
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
    Some(result(rules, Err(Error::Other(message))))
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
    TypeMismatch(&'static str, std::string::String),
    /// `MissingField(path)`: the missing field's own path.
    MissingField(std::string::String),
    /// `Other(message)`.
    Other(std::string::String),
    /// A value was present for a type no decoder exists for (the type as
    /// written): [`record`] turns it into an `Other` that names the field
    /// whose type it is.
    Unsupported(std::string::String),
    /// The error a written `json.Decode` impl gave for a part, as the
    /// format's error variant.
    Given(Value),
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
            Error::TypeMismatch(expected, actual) => {
                Value::variant(mismatch, [Value::String(expected.into()), string(actual)])
            }
            Error::MissingField(path) => Value::variant(missing, [string(path)]),
            Error::Other(message) => Value::variant(other, [string(message)]),
            Error::Given(error) => error,
            // (Only a record's field holds such a type, and `record`
            // names it: this is the fallback.)
            Error::Unsupported(declared) => Value::variant(
                other,
                [string(format!(
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
    let accepted = || decoding.type_arguments(str::to_string);
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

/// The record of type `ty` (of the `fields`) that `doc`, which is `at` a
/// path, is.
pub(crate) fn record<D: Document>(
    rt: &Reader,
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
        // (A field no decoder fills was refused before the document was
        // read: see [`refusal`].)
        let at = at.key(name);
        values.push(match D::get(table, name) {
            Some(item) => value(rt, item, field, &at).map_err(|e| match e {
                Error::Unsupported(declared) => {
                    Error::Other(unsupported_field_type_message(&ty.name, name, &declared))
                }
                e => e,
            })?,
            None => match field {
                FieldType::Option(_) => Value::variant(bv::NONE, []),
                _ => return Err(Error::MissingField(at.to_string())),
            },
        });
    }
    Ok(Value::record(ty.clone(), values))
}

/// The list of values of the field type `element` that `doc`, an array
/// `at` a path, holds.
pub(crate) fn list<D: Document>(
    rt: &Reader,
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
    rt: &Reader,
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
    rt: &Reader,
    doc: &D,
    expected: &FieldType,
    at: &At,
) -> Result<Value, Error> {
    // A record or enum type with a written impl: the impl's.
    if let Some(decoded) = rt.hole(expected, doc, at) {
        return decoded;
    }
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
            Node::Null => Ok(Value::variant(bv::NONE, [])),
            _ => Ok(Value::variant(bv::SOME, [value(rt, doc, inner, at)?])),
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
        FieldType::JsonValue if rules.family.decodes(Leaf::JsonValue) => Ok(json_value(doc)),
        FieldType::JsonValue => Err(Error::Unsupported("json.Value".into())),
        FieldType::Bytes => Err(Error::Unsupported("Bytes".into())),
        // A date and a time are a string of their text, or a literal of
        // the format; a text that is none has the one message either way.
        FieldType::Date => match node {
            Node::String(s) => parse_date(s)
                .map(make_date)
                .map_err(|e| Error::other(invalid_date_message(s, e), at)),
            Node::Datetime(s) => parse_date(&s)
                .map(make_date)
                .map_err(|e| Error::other(invalid_date_message(&s, e), at)),
            _ => Err(mismatch("time.Date")),
        },
        FieldType::Time => match node {
            Node::String(s) => parse_time(s)
                .map(make_time)
                .map_err(|e| Error::other(invalid_time_message(s, &e), at)),
            Node::Datetime(s) => parse_time(&s)
                .map(make_time)
                .map_err(|e| Error::other(invalid_time_message(&s, &e), at)),
            _ => Err(mismatch("time.Time")),
        },
        FieldType::DateTime => {
            let text = match &node {
                Node::String(s) => *s,
                Node::Datetime(s) => s,
                _ => return Err(mismatch("time.DateTime")),
            };
            parse_datetime(text)
                .map(make_datetime)
                .map_err(|e| Error::other(e, at))
        }
        FieldType::Record(id) => {
            let ty = field_record_type(rt, rules.caller, *id)?;
            record(rt, &ty, decodable_record(rules.caller, &ty)?, doc, at)
        }
        FieldType::Enum(id) if rules.family.decodes_enums() => {
            let ty = field_record_type(rt, rules.caller, *id)?;
            variant(rt, &ty, &[], doc, at)
        }
        FieldType::Enum(_) => Err(Error::Unsupported(shown(rt, expected))),
        // A generic type's fields, each type parameter its argument.
        FieldType::Applied(id, args) => {
            let ty = field_record_type(rt, rules.caller, *id)?;
            match &ty.shape {
                Shape::Enum(_) if rules.family.decodes_enums() && decodable_enum(&ty) => {
                    variant(rt, &ty, args, doc, at)
                }
                Shape::Enum(_) => Err(Error::Unsupported(shown(rt, expected))),
                _ => {
                    let fields: Vec<_> = (decodable_record(rules.caller, &ty)?.iter())
                        .map(|(name, field)| (name.clone(), field.instantiated(args).into_owned()))
                        .collect();
                    record(rt, &ty, &fields, doc, at)
                }
            }
        }
        // (A type parameter no type argument gave: refused before.)
        FieldType::Param(_) | FieldType::KeyedMap(..) => {
            Err(Error::Unsupported(shown(rt, expected)))
        }
    }
}

/// The variant of the enum type `ty` at its type arguments `args` (none
/// for a type without parameters) that `doc`, which is `at` a path, is,
/// as `json.stringify` writes a variant: the string of its name
/// (`"Active"`) for one without fields, and the object
/// `{"fields":[...],"variant":"Name"}` for one with them, each field
/// decoded as the variant declares it (other keys of the object are
/// ignored).
pub(crate) fn variant<D: Document>(
    rt: &Reader,
    ty: &Arc<TypeInfo>,
    args: &[FieldType],
    doc: &D,
    at: &At,
) -> Result<Value, Error> {
    let count = |n: usize| match n {
        1 => "1 field".to_string(),
        n => format!("{n} fields"),
    };
    let declared = |tag: &Tag| &ty.variants()[tag.ordinal() as usize].fields;
    let no_variant = |name: &str, at: &At| {
        Error::other(format_args!("`{}` has no variant `{name}`", ty.name), at)
    };
    let table = match doc.node() {
        Node::String(name) => {
            let tag = Tag::named(ty, name).ok_or_else(|| no_variant(name, at))?;
            let fields = declared(&tag);
            if !fields.is_empty() {
                let message = format_args!(
                    "variant `{name}` of `{}` has {}, got 0",
                    ty.name,
                    count(fields.len())
                );
                return Err(Error::other(message, at));
            }
            return Ok(Value::variant(tag, []));
        }
        Node::Table(table) => table,
        _ => return Err(Error::mismatch(D::RULES.variant, doc.kind(), at)),
    };
    let at_name = at.key("variant");
    let name = match D::get(table, "variant").map(|name| (name, name.node())) {
        Some((_, Node::String(name))) => name,
        Some((name, _)) => return Err(Error::mismatch("String", name.kind(), &at_name)),
        None => return Err(Error::MissingField(at_name.to_string())),
    };
    let tag = Tag::named(ty, name).ok_or_else(|| no_variant(name, &at_name))?;
    let declared = declared(&tag);
    // (A variant without fields is its name, never an object.)
    if declared.is_empty() {
        return Err(Error::mismatch("String", doc.kind(), at));
    }
    let at_fields = at.key("fields");
    let items = match D::get(table, "fields").map(|items| (items, items.node())) {
        Some((_, Node::Array(items))) => items,
        Some((items, _)) => return Err(Error::mismatch("array", items.kind(), &at_fields)),
        None => &[],
    };
    if items.len() != declared.len() {
        let message = format_args!(
            "variant `{name}` of `{}` has {}, got {}",
            ty.name,
            count(declared.len()),
            items.len()
        );
        return Err(Error::other(message, &at_fields));
    }
    let mut fields = Vec::with_capacity(items.len());
    // (A field no decoder fills was refused before the document was
    // read; this is the fallback.)
    let refused = |declared: &str| Error::Other(variant_field_message(&ty.name, name, declared));
    for (index, (item, field)) in items.iter().zip(declared).enumerate() {
        let decoded = value(rt, item, &field.instantiated(args), &at_fields.index(index));
        fields.push(decoded.map_err(|e| match e {
            Error::Unsupported(declared) => refused(&declared),
            e => e,
        })?);
    }
    Ok(Value::variant(tag, fields))
}

/// The `json.Value` that holds `doc`, a part `at` a path that a written
/// impl decodes, as [`json_value`] makes it; a float that is no number
/// (TOML's `nan` and `inf`), which no `json.Value` holds, is a type
/// mismatch at its own path.
fn json_part<D: Document>(doc: &D, at: &At) -> Result<Value, Error> {
    Ok(match doc.node() {
        Node::Float(f) if Float::new(f).is_none() => {
            return Err(Error::mismatch("json.Value", "a non-finite float", at));
        }
        Node::Array(items) => {
            let mut values = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                values.push(json_part(item, &at.index(index))?);
            }
            Value::variant(bv::JSON_ARRAY, [Value::list(values)])
        }
        Node::Table(table) => {
            let entries = D::entries(table);
            let mut members = Vec::with_capacity(entries.len());
            for (key, item) in entries {
                members.push((Value::String(key.into()), json_part(item, &at.key(key))?));
            }
            Value::variant(bv::JSON_OBJECT, [Value::map(members)])
        }
        _ => json_value(doc),
    })
}

/// The `json.Value` that holds `doc`: a number an `Int` when it is held
/// as one, as for an `Int` field, and a `Float` otherwise.
pub(crate) fn json_value<D: Document>(doc: &D) -> Value {
    match doc.node() {
        Node::Null => Value::variant(bv::JSON_NULL, []),
        Node::Bool(b) => Value::variant(bv::JSON_BOOL, [Value::Bool(b)]),
        Node::Int(n) => Value::variant(bv::JSON_INT, [Value::Int(n)]),
        Node::Float(f) => match Float::new(f) {
            Some(f) => Value::variant(bv::JSON_FLOAT, [Value::Float(f)]),
            // (A document has no NaN.)
            None => Value::variant(bv::JSON_NULL, []),
        },
        Node::String(s) => Value::variant(bv::JSON_STRING, [Value::String(s.into())]),
        // (A format with such literals decodes no `json.Value`.)
        Node::Datetime(s) => Value::variant(bv::JSON_STRING, [Value::String(s.into())]),
        Node::Array(items) => Value::variant(
            bv::JSON_ARRAY,
            [Value::list(items.iter().map(json_value).collect())],
        ),
        Node::Table(table) => Value::variant(
            bv::JSON_OBJECT,
            [Value::map(D::entries(table).map(|(key, item)| {
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
            Err(Error::Given(error)) => format!("Given {error}"),
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
            // As json.parse and toml.parse decode it: the type is
            // refused before the document is read.
            let target = Target::Record(ty);
            let whole = |family, decoded: Result<Value, Error>| match refusal(rt, family, &target) {
                Some(message) => outcome(Err(Error::Other(message))),
                None => outcome(decoded),
            };
            let decoded = [
                whole(Family::Json, record(rt, ty, fields, &json, &At::Top)),
                whole(Family::Toml, record(rt, ty, fields, &toml, &At::Top)),
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
