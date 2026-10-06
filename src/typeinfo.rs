//! What a value knows of its type at run time.
//!
//! A record or variant value carries its type as an [`Arc<TypeInfo>`]:
//! the type's id (the [`TypeId`] of its definition), the name it prints
//! with, and its variants or fields. Equality, ordering and hashing of
//! variants use the type's id and the variant's ordinal, the position of
//! its declaration, so two enums with variants of one name stay apart and
//! an enum's order is its declaration order whatever else the program
//! declares.
//!
//! The compiler builds a [`TypeInfo`] for each type of the program and
//! hands the VM a [`TypeTable`] of them; the builtin types have theirs in
//! a table of their own, with fixed ids ([`ty`]) and fixed variants
//! ([`bv`]), so a builtin builds `Some(x)` as `Value::variant(bv::SOME,
//! ..)`.

use std::collections::HashMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, OnceLock};

use crate::defs::{DefId, TypeId};

/// A type as values see it at run time.
#[derive(Debug)]
pub struct TypeInfo {
    pub id: TypeId,
    /// The name the type prints with: its name, or, when two types of
    /// the program have that name, its module's name and its name
    /// (`a.Pt`).
    pub name: String,
    pub shape: Shape,
}

/// What a type's values are made of.
#[derive(Debug)]
pub enum Shape {
    /// An enum: its variants in declaration order.
    Enum(Vec<VariantInfo>),
    /// A record: its fields in declaration order, with the type of each
    /// as far as the decoders of `json.parse` and `toml.parse` are
    /// concerned. A builtin record lists none.
    Record(Vec<(String, FieldType)>),
    /// A type whose values are not records or variants (`Int`, `List`).
    Opaque,
}

/// One variant of an enum.
#[derive(Debug)]
pub struct VariantInfo {
    pub name: String,
    pub arity: u16,
}

impl TypeInfo {
    /// The enum `name` with the id `id` and the variants `(name, arity)`,
    /// which prints under its name.
    pub fn new_enum(id: TypeId, name: &str, variants: &[(&str, u16)]) -> Arc<TypeInfo> {
        Arc::new(TypeInfo {
            id,
            name: name.to_string(),
            shape: Shape::Enum(
                variants
                    .iter()
                    .map(|(name, arity)| VariantInfo {
                        name: (*name).to_string(),
                        arity: *arity,
                    })
                    .collect(),
            ),
        })
    }

    /// The record type `name` with the id `id` and the fields `fields`.
    pub fn new_record(id: TypeId, name: &str, fields: Vec<(String, FieldType)>) -> Arc<TypeInfo> {
        Arc::new(TypeInfo {
            id,
            name: name.to_string(),
            shape: Shape::Record(fields),
        })
    }

    /// The variants of an enum; none for any other type.
    pub fn variants(&self) -> &[VariantInfo] {
        match &self.shape {
            Shape::Enum(variants) => variants,
            _ => &[],
        }
    }

    /// The fields of a record; none for any other type.
    pub fn fields(&self) -> &[(String, FieldType)] {
        match &self.shape {
            Shape::Record(fields) => fields,
            _ => &[],
        }
    }

    /// Whether this is the type of the records an anonymous record
    /// literal builds, which stand for any record of their shape.
    pub fn is_anon(&self) -> bool {
        self.id == ty::ANON_RECORD
    }
}

/// The declared type of a record field, as far as the decoders of
/// `json.parse` and `toml.parse` are concerned.
///
/// Both decoders must produce, for every variant, a value of exactly the
/// described type or an error. `Unsupported` is a type no decoder exists
/// for: decoding such a field is an error.
#[derive(Debug, Clone)]
pub enum FieldType {
    Int,
    Float,
    String,
    Bool,
    List(Box<FieldType>),
    Option(Box<FieldType>),
    /// `Map(String, T)`.
    Map(Box<FieldType>),
    Tuple(Vec<FieldType>),
    /// A record type of the program without type parameters.
    Record(TypeId),
    Date,
    Time,
    DateTime,
    /// A type without a decoder; carries the type as written in the
    /// record declaration.
    Unsupported(String),
}

/// The variant of a variant value: its type and the position of its
/// declaration in the type.
#[derive(Clone)]
pub struct Tag {
    ty: Arc<TypeInfo>,
    ordinal: u16,
}

impl Tag {
    /// The `ordinal`th variant of the enum `ty`.
    pub fn new(ty: Arc<TypeInfo>, ordinal: u16) -> Tag {
        debug_assert!((ordinal as usize) < ty.variants().len());
        Tag { ty, ordinal }
    }

    /// The variant of the enum `ty` named `name`.
    pub fn named(ty: &Arc<TypeInfo>, name: &str) -> Option<Tag> {
        let ordinal = ty.variants().iter().position(|v| v.name == name)?;
        Some(Tag::new(ty.clone(), ordinal as u16))
    }

    pub fn ty(&self) -> &Arc<TypeInfo> {
        &self.ty
    }

    pub fn type_id(&self) -> TypeId {
        self.ty.id
    }

    pub fn ordinal(&self) -> u16 {
        self.ordinal
    }

    pub fn name(&self) -> &str {
        &self.ty.variants()[self.ordinal as usize].name
    }

    pub fn arity(&self) -> usize {
        self.ty.variants()[self.ordinal as usize].arity as usize
    }

    /// Whether this is the builtin variant `variant`.
    pub fn is(&self, variant: BuiltinVariant) -> bool {
        self.ty.id == variant.ty && self.ordinal == variant.ordinal
    }

    /// Whether this is a variant of the type `ty`.
    pub fn of(&self, ty: TypeId) -> bool {
        self.ty.id == ty
    }
}

impl PartialEq for Tag {
    fn eq(&self, other: &Tag) -> bool {
        self.ty.id == other.ty.id && self.ordinal == other.ordinal
    }
}

impl Eq for Tag {}

impl PartialOrd for Tag {
    fn partial_cmp(&self, other: &Tag) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Variants of one type order by declaration; variants of two types
/// (which a program cannot compare) by their types' ids.
impl Ord for Tag {
    fn cmp(&self, other: &Tag) -> std::cmp::Ordering {
        (self.ty.id, self.ordinal).cmp(&(other.ty.id, other.ordinal))
    }
}

impl Hash for Tag {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.ty.id.hash(state);
        self.ordinal.hash(state);
    }
}

impl fmt::Display for Tag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl fmt::Debug for Tag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl From<BuiltinVariant> for Tag {
    fn from(variant: BuiltinVariant) -> Tag {
        variant.tag()
    }
}

/// A variant of a builtin enum, by its type's fixed id and its ordinal.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct BuiltinVariant {
    pub ty: TypeId,
    pub ordinal: u16,
}

impl BuiltinVariant {
    pub const fn new(ty: TypeId, ordinal: u16) -> BuiltinVariant {
        BuiltinVariant { ty, ordinal }
    }

    pub fn tag(self) -> Tag {
        Tag::new(builtin_type(self.ty).clone(), self.ordinal)
    }
}

/// The types of a program, by id, as the compiler describes them to the
/// VM. A builtin type is not in it: [`TypeTable::get`] finds those in
/// the builtin table.
#[derive(Debug, Clone, Default)]
pub struct TypeTable {
    types: HashMap<TypeId, Arc<TypeInfo>>,
}

impl TypeTable {
    pub fn get(&self, id: TypeId) -> Option<&Arc<TypeInfo>> {
        self.types
            .get(&id)
            .or_else(|| builtin_types().get(id.0.0 as usize))
    }

    /// Enter the description of a program's type. `get` finds a builtin
    /// type by its id as an index of the builtin table, which is sound
    /// because a program's definitions have ids after every builtin
    /// definition (see the test `program_ids_follow_the_builtin_ids`).
    pub fn insert(&mut self, info: Arc<TypeInfo>) {
        debug_assert!(
            builtin_types().get(info.id.0.0 as usize).is_none(),
            "a program's type has the id of a builtin type"
        );
        self.types.insert(info.id, info);
    }

    /// Take in the types of `other`; a type of both keeps `other`'s
    /// description.
    pub fn extend(&mut self, other: &TypeTable) {
        for (id, info) in &other.types {
            self.types.insert(*id, info.clone());
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<TypeInfo>> {
        self.types.values()
    }
}

/// The ids of the builtin types values are built of: the first
/// definitions of the builtins ([`crate::defs::builtin_types`] begins
/// with [`RUNTIME_BUILTIN_TYPES`]).
pub mod ty {
    use crate::defs::{DefId, TypeId};

    pub const RESULT: TypeId = TypeId(DefId(0));
    pub const OPTION: TypeId = TypeId(DefId(1));
    pub const STEP: TypeId = TypeId(DefId(2));
    pub const CHANNEL_RESULT: TypeId = TypeId(DefId(3));
    pub const CHANNEL_OP: TypeId = TypeId(DefId(4));
    pub const WEEKDAY: TypeId = TypeId(DefId(5));
    pub const METHOD: TypeId = TypeId(DefId(6));
    pub const IO_ERROR: TypeId = TypeId(DefId(7));
    pub const JSON_ERROR: TypeId = TypeId(DefId(8));
    pub const TOML_ERROR: TypeId = TypeId(DefId(9));
    pub const PARSE_ERROR: TypeId = TypeId(DefId(10));
    pub const HTTP_ERROR: TypeId = TypeId(DefId(11));
    pub const REGEX_ERROR: TypeId = TypeId(DefId(12));
    pub const PG_ERROR: TypeId = TypeId(DefId(13));
    pub const TCP_ERROR: TypeId = TypeId(DefId(14));
    pub const TIME_ERROR: TypeId = TypeId(DefId(15));
    pub const BYTES_ERROR: TypeId = TypeId(DefId(16));
    pub const CHANNEL_ERROR: TypeId = TypeId(DefId(17));
    pub const PG_POOL: TypeId = TypeId(DefId(18));
    pub const PG_TX: TypeId = TypeId(DefId(19));
    pub const PG_CURSOR: TypeId = TypeId(DefId(20));
    pub const PG_VALUE: TypeId = TypeId(DefId(21));
    pub const DATE: TypeId = TypeId(DefId(22));
    pub const TIME: TypeId = TypeId(DefId(23));
    pub const DATE_TIME: TypeId = TypeId(DefId(24));
    pub const DURATION: TypeId = TypeId(DefId(25));
    pub const INSTANT: TypeId = TypeId(DefId(26));
    pub const FILE_STAT: TypeId = TypeId(DefId(27));
    pub const RESPONSE: TypeId = TypeId(DefId(28));
    pub const REQUEST: TypeId = TypeId(DefId(29));
    pub const QUERY_RESULT: TypeId = TypeId(DefId(30));
    pub const EXEC_RESULT: TypeId = TypeId(DefId(31));
    pub const ANON_RECORD: TypeId = TypeId(DefId(32));

    /// The types of the run time's own marker values, which no program
    /// can name: ids past any definition.
    pub const STREAM_ERROR: TypeId = TypeId(DefId(u32::MAX));
    pub const MAP_ERROR: TypeId = TypeId(DefId(u32::MAX - 1));
    pub const NOTIFICATION: TypeId = TypeId(DefId(u32::MAX - 3));
}

/// The variants of the builtin enums, and of the builtin handle types a
/// builtin builds as variants (`PgPool(id)`).
pub mod bv {
    use super::{BuiltinVariant, ty};

    pub const OK: BuiltinVariant = BuiltinVariant::new(ty::RESULT, 0);
    pub const ERR: BuiltinVariant = BuiltinVariant::new(ty::RESULT, 1);
    pub const SOME: BuiltinVariant = BuiltinVariant::new(ty::OPTION, 0);
    pub const NONE: BuiltinVariant = BuiltinVariant::new(ty::OPTION, 1);
    pub const STOP: BuiltinVariant = BuiltinVariant::new(ty::STEP, 0);
    pub const CONTINUE: BuiltinVariant = BuiltinVariant::new(ty::STEP, 1);
    pub const MESSAGE: BuiltinVariant = BuiltinVariant::new(ty::CHANNEL_RESULT, 0);
    pub const CLOSED: BuiltinVariant = BuiltinVariant::new(ty::CHANNEL_RESULT, 1);
    pub const SENT: BuiltinVariant = BuiltinVariant::new(ty::CHANNEL_RESULT, 2);
    pub const EMPTY: BuiltinVariant = BuiltinVariant::new(ty::CHANNEL_RESULT, 3);
    pub const RECV: BuiltinVariant = BuiltinVariant::new(ty::CHANNEL_OP, 0);
    pub const SEND: BuiltinVariant = BuiltinVariant::new(ty::CHANNEL_OP, 1);
    pub const MONDAY: BuiltinVariant = BuiltinVariant::new(ty::WEEKDAY, 0);
    pub const TUESDAY: BuiltinVariant = BuiltinVariant::new(ty::WEEKDAY, 1);
    pub const WEDNESDAY: BuiltinVariant = BuiltinVariant::new(ty::WEEKDAY, 2);
    pub const THURSDAY: BuiltinVariant = BuiltinVariant::new(ty::WEEKDAY, 3);
    pub const FRIDAY: BuiltinVariant = BuiltinVariant::new(ty::WEEKDAY, 4);
    pub const SATURDAY: BuiltinVariant = BuiltinVariant::new(ty::WEEKDAY, 5);
    pub const SUNDAY: BuiltinVariant = BuiltinVariant::new(ty::WEEKDAY, 6);
    pub const GET: BuiltinVariant = BuiltinVariant::new(ty::METHOD, 0);
    pub const POST: BuiltinVariant = BuiltinVariant::new(ty::METHOD, 1);
    pub const PUT: BuiltinVariant = BuiltinVariant::new(ty::METHOD, 2);
    pub const PATCH: BuiltinVariant = BuiltinVariant::new(ty::METHOD, 3);
    pub const DELETE: BuiltinVariant = BuiltinVariant::new(ty::METHOD, 4);
    pub const HEAD: BuiltinVariant = BuiltinVariant::new(ty::METHOD, 5);
    pub const OPTIONS: BuiltinVariant = BuiltinVariant::new(ty::METHOD, 6);
    pub const IO_NOT_FOUND: BuiltinVariant = BuiltinVariant::new(ty::IO_ERROR, 0);
    pub const IO_PERMISSION_DENIED: BuiltinVariant = BuiltinVariant::new(ty::IO_ERROR, 1);
    pub const IO_ALREADY_EXISTS: BuiltinVariant = BuiltinVariant::new(ty::IO_ERROR, 2);
    pub const IO_INVALID_INPUT: BuiltinVariant = BuiltinVariant::new(ty::IO_ERROR, 3);
    pub const IO_INTERRUPTED: BuiltinVariant = BuiltinVariant::new(ty::IO_ERROR, 4);
    pub const IO_UNEXPECTED_EOF: BuiltinVariant = BuiltinVariant::new(ty::IO_ERROR, 5);
    pub const IO_WRITE_ZERO: BuiltinVariant = BuiltinVariant::new(ty::IO_ERROR, 6);
    pub const IO_UNKNOWN: BuiltinVariant = BuiltinVariant::new(ty::IO_ERROR, 7);
    pub const JSON_SYNTAX: BuiltinVariant = BuiltinVariant::new(ty::JSON_ERROR, 0);
    pub const JSON_TYPE_MISMATCH: BuiltinVariant = BuiltinVariant::new(ty::JSON_ERROR, 1);
    pub const JSON_MISSING_FIELD: BuiltinVariant = BuiltinVariant::new(ty::JSON_ERROR, 2);
    pub const JSON_UNKNOWN: BuiltinVariant = BuiltinVariant::new(ty::JSON_ERROR, 3);
    pub const TOML_SYNTAX: BuiltinVariant = BuiltinVariant::new(ty::TOML_ERROR, 0);
    pub const TOML_TYPE_MISMATCH: BuiltinVariant = BuiltinVariant::new(ty::TOML_ERROR, 1);
    pub const TOML_MISSING_FIELD: BuiltinVariant = BuiltinVariant::new(ty::TOML_ERROR, 2);
    pub const TOML_UNKNOWN: BuiltinVariant = BuiltinVariant::new(ty::TOML_ERROR, 3);
    pub const PARSE_EMPTY: BuiltinVariant = BuiltinVariant::new(ty::PARSE_ERROR, 0);
    pub const PARSE_INVALID_DIGIT: BuiltinVariant = BuiltinVariant::new(ty::PARSE_ERROR, 1);
    pub const PARSE_OVERFLOW: BuiltinVariant = BuiltinVariant::new(ty::PARSE_ERROR, 2);
    pub const PARSE_UNDERFLOW: BuiltinVariant = BuiltinVariant::new(ty::PARSE_ERROR, 3);
    pub const HTTP_CONNECT: BuiltinVariant = BuiltinVariant::new(ty::HTTP_ERROR, 0);
    pub const HTTP_TLS: BuiltinVariant = BuiltinVariant::new(ty::HTTP_ERROR, 1);
    pub const HTTP_TIMEOUT: BuiltinVariant = BuiltinVariant::new(ty::HTTP_ERROR, 2);
    pub const HTTP_INVALID_URL: BuiltinVariant = BuiltinVariant::new(ty::HTTP_ERROR, 3);
    pub const HTTP_INVALID_RESPONSE: BuiltinVariant = BuiltinVariant::new(ty::HTTP_ERROR, 4);
    pub const HTTP_CLOSED_EARLY: BuiltinVariant = BuiltinVariant::new(ty::HTTP_ERROR, 5);
    pub const HTTP_STATUS_CODE: BuiltinVariant = BuiltinVariant::new(ty::HTTP_ERROR, 6);
    pub const HTTP_UNKNOWN: BuiltinVariant = BuiltinVariant::new(ty::HTTP_ERROR, 7);
    pub const REGEX_INVALID_PATTERN: BuiltinVariant = BuiltinVariant::new(ty::REGEX_ERROR, 0);
    pub const REGEX_TOO_BIG: BuiltinVariant = BuiltinVariant::new(ty::REGEX_ERROR, 1);
    pub const PG_CONNECT: BuiltinVariant = BuiltinVariant::new(ty::PG_ERROR, 0);
    pub const PG_TLS: BuiltinVariant = BuiltinVariant::new(ty::PG_ERROR, 1);
    pub const PG_AUTH_FAILED: BuiltinVariant = BuiltinVariant::new(ty::PG_ERROR, 2);
    pub const PG_QUERY: BuiltinVariant = BuiltinVariant::new(ty::PG_ERROR, 3);
    pub const PG_TYPE_MISMATCH: BuiltinVariant = BuiltinVariant::new(ty::PG_ERROR, 4);
    pub const PG_NO_SUCH_COLUMN: BuiltinVariant = BuiltinVariant::new(ty::PG_ERROR, 5);
    pub const PG_CLOSED: BuiltinVariant = BuiltinVariant::new(ty::PG_ERROR, 6);
    pub const PG_TIMEOUT: BuiltinVariant = BuiltinVariant::new(ty::PG_ERROR, 7);
    pub const PG_TXN_ABORTED: BuiltinVariant = BuiltinVariant::new(ty::PG_ERROR, 8);
    pub const PG_UNKNOWN: BuiltinVariant = BuiltinVariant::new(ty::PG_ERROR, 9);
    pub const TCP_CONNECT: BuiltinVariant = BuiltinVariant::new(ty::TCP_ERROR, 0);
    pub const TCP_TLS: BuiltinVariant = BuiltinVariant::new(ty::TCP_ERROR, 1);
    pub const TCP_CLOSED: BuiltinVariant = BuiltinVariant::new(ty::TCP_ERROR, 2);
    pub const TCP_TIMEOUT: BuiltinVariant = BuiltinVariant::new(ty::TCP_ERROR, 3);
    pub const TCP_UNKNOWN: BuiltinVariant = BuiltinVariant::new(ty::TCP_ERROR, 4);
    pub const TIME_PARSE_FORMAT: BuiltinVariant = BuiltinVariant::new(ty::TIME_ERROR, 0);
    pub const TIME_OUT_OF_RANGE: BuiltinVariant = BuiltinVariant::new(ty::TIME_ERROR, 1);
    pub const BYTES_INVALID_UTF8: BuiltinVariant = BuiltinVariant::new(ty::BYTES_ERROR, 0);
    pub const BYTES_INVALID_HEX: BuiltinVariant = BuiltinVariant::new(ty::BYTES_ERROR, 1);
    pub const BYTES_INVALID_BASE64: BuiltinVariant = BuiltinVariant::new(ty::BYTES_ERROR, 2);
    pub const BYTES_BYTE_OUT_OF_RANGE: BuiltinVariant = BuiltinVariant::new(ty::BYTES_ERROR, 3);
    pub const BYTES_OUT_OF_BOUNDS: BuiltinVariant = BuiltinVariant::new(ty::BYTES_ERROR, 4);
    pub const CHANNEL_TIMEOUT: BuiltinVariant = BuiltinVariant::new(ty::CHANNEL_ERROR, 0);
    pub const CHANNEL_CLOSED: BuiltinVariant = BuiltinVariant::new(ty::CHANNEL_ERROR, 1);
    pub const PG_POOL: BuiltinVariant = BuiltinVariant::new(ty::PG_POOL, 0);
    pub const PG_TX: BuiltinVariant = BuiltinVariant::new(ty::PG_TX, 0);
    pub const PG_CURSOR: BuiltinVariant = BuiltinVariant::new(ty::PG_CURSOR, 0);
    pub const V_INT: BuiltinVariant = BuiltinVariant::new(ty::PG_VALUE, 0);
    pub const V_STR: BuiltinVariant = BuiltinVariant::new(ty::PG_VALUE, 1);
    pub const V_BOOL: BuiltinVariant = BuiltinVariant::new(ty::PG_VALUE, 2);
    pub const V_FLOAT: BuiltinVariant = BuiltinVariant::new(ty::PG_VALUE, 3);
    pub const V_NULL: BuiltinVariant = BuiltinVariant::new(ty::PG_VALUE, 4);
    pub const V_LIST: BuiltinVariant = BuiltinVariant::new(ty::PG_VALUE, 5);
    pub const STREAM_ERROR: BuiltinVariant = BuiltinVariant::new(ty::STREAM_ERROR, 0);
    pub const MAP_ERROR: BuiltinVariant = BuiltinVariant::new(ty::MAP_ERROR, 0);
}

/// The builtin types whose ids are the constants of [`ty`], in that
/// order.
pub const RUNTIME_BUILTIN_TYPES: &[&str] = &[
    "Result",
    "Option",
    "Step",
    "ChannelResult",
    "ChannelOp",
    "Weekday",
    "Method",
    "IoError",
    "JsonError",
    "TomlError",
    "ParseError",
    "HttpError",
    "RegexError",
    "PgError",
    "TcpError",
    "TimeError",
    "BytesError",
    "ChannelError",
    "PgPool",
    "PgTx",
    "PgCursor",
    "Value",
    "Date",
    "Time",
    "DateTime",
    "Duration",
    "Instant",
    "FileStat",
    "Response",
    "Request",
    "QueryResult",
    "ExecResult",
    "<anon>",
];

/// The variants of the builtin handle types a builtin builds as variants;
/// the builtin enums' variants are those of
/// [`crate::module::builtin_prelude_enum_variants_with_arity`] and
/// [`crate::module::builtin_error_enum_variants_with_arity`].
const HANDLE_VARIANTS: &[(&str, &[(&str, usize)])] = &[
    ("PgPool", &[("PgPool", 1)]),
    ("PgTx", &[("PgTx", 1)]),
    ("PgCursor", &[("PgCursor", 1)]),
    (
        "Value",
        &[
            ("VInt", 1),
            ("VStr", 1),
            ("VBool", 1),
            ("VFloat", 1),
            ("VNull", 0),
            ("VList", 1),
        ],
    ),
];

/// The builtin record types a builtin builds.
const BUILTIN_RECORDS: &[&str] = &[
    "Date",
    "Time",
    "DateTime",
    "Duration",
    "Instant",
    "FileStat",
    "Response",
    "Request",
    "QueryResult",
    "ExecResult",
];

/// The description of every builtin type, indexed by id.
fn builtin_types() -> &'static [Arc<TypeInfo>] {
    static TYPES: OnceLock<Vec<Arc<TypeInfo>>> = OnceLock::new();
    TYPES.get_or_init(|| {
        let enums: HashMap<&str, &[(&str, usize)]> =
            crate::module::builtin_prelude_enum_variants_with_arity()
                .iter()
                .chain(crate::module::builtin_error_enum_variants_with_arity())
                .chain(HANDLE_VARIANTS)
                .map(|(name, variants)| (*name, *variants))
                .collect();
        crate::defs::builtin_types()
            .iter()
            .enumerate()
            .map(|(k, (name, _))| {
                let shape = match enums.get(name) {
                    Some(variants) => Shape::Enum(
                        variants
                            .iter()
                            .map(|(name, arity)| VariantInfo {
                                name: (*name).to_string(),
                                arity: *arity as u16,
                            })
                            .collect(),
                    ),
                    None if BUILTIN_RECORDS.contains(name) || k == ty::ANON_RECORD.0.0 as usize => {
                        Shape::Record(Vec::new())
                    }
                    None => Shape::Opaque,
                };
                Arc::new(TypeInfo {
                    id: TypeId(DefId(k as u32)),
                    name: (*name).to_string(),
                    shape,
                })
            })
            .collect()
    })
}

/// The run time's own marker types (see [`ty::STREAM_ERROR`]).
fn marker_types() -> &'static [Arc<TypeInfo>] {
    static TYPES: OnceLock<Vec<Arc<TypeInfo>>> = OnceLock::new();
    TYPES.get_or_init(|| {
        let variant = |id: TypeId, name: &str, arity: u16| {
            Arc::new(TypeInfo {
                id,
                name: name.to_string(),
                shape: Shape::Enum(vec![VariantInfo {
                    name: name.to_string(),
                    arity,
                }]),
            })
        };
        let record = |id: TypeId, name: &str| {
            Arc::new(TypeInfo {
                id,
                name: name.to_string(),
                shape: Shape::Record(Vec::new()),
            })
        };
        vec![
            variant(ty::STREAM_ERROR, "__StreamTypeError__", 1),
            variant(ty::MAP_ERROR, "__MapMapTypeError__", 0),
            record(ty::NOTIFICATION, "Notification"),
        ]
    })
}

/// The description of the builtin type `id`. Panics on the id of a type
/// of a program.
pub fn builtin_type(id: TypeId) -> &'static Arc<TypeInfo> {
    let k = id.0.0 as usize;
    builtin_types().get(k).unwrap_or_else(|| {
        marker_types()
            .iter()
            .find(|info| info.id == id)
            .expect("a builtin type")
    })
}

/// The builtin type named `name`.
pub fn builtin_type_named(name: &str) -> Option<&'static Arc<TypeInfo>> {
    crate::defs::builtin_type_id(name).map(builtin_type)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first definition a program declares has an id after every
    /// builtin one, so `TypeTable::get` can find a builtin type by its id
    /// as an index.
    #[test]
    fn program_ids_follow_the_builtin_ids() {
        let mut defs = crate::typechecker::names::new_def_table();
        let id = defs.add(crate::defs::Def {
            module: crate::session::ModuleId(0),
            name: crate::intern::intern("Pt"),
            span: crate::source::Span::BUILTIN,
            vis: crate::defs::Vis::Pub,
            kind: crate::defs::DefKind::Fn,
        });
        assert!(builtin_types().get(id.0 as usize).is_none());
        assert!(id.0 as usize >= crate::defs::builtin_types().len());
    }

    /// Each `bv::` constant is the variant it is named after, with the
    /// ordinal and the arity the checker's builtin declarations give it.
    #[test]
    fn the_builtin_variant_constants_are_the_declared_variants() {
        let (defs, _) = crate::typechecker::names::builtins();
        let constants: &[(BuiltinVariant, &str)] = &[
            (bv::OK, "Ok"),
            (bv::ERR, "Err"),
            (bv::SOME, "Some"),
            (bv::NONE, "None"),
            (bv::STOP, "Stop"),
            (bv::CONTINUE, "Continue"),
            (bv::MESSAGE, "Message"),
            (bv::CLOSED, "Closed"),
            (bv::SENT, "Sent"),
            (bv::EMPTY, "Empty"),
            (bv::RECV, "Recv"),
            (bv::SEND, "Send"),
            (bv::MONDAY, "Monday"),
            (bv::TUESDAY, "Tuesday"),
            (bv::WEDNESDAY, "Wednesday"),
            (bv::THURSDAY, "Thursday"),
            (bv::FRIDAY, "Friday"),
            (bv::SATURDAY, "Saturday"),
            (bv::SUNDAY, "Sunday"),
            (bv::GET, "GET"),
            (bv::POST, "POST"),
            (bv::PUT, "PUT"),
            (bv::PATCH, "PATCH"),
            (bv::DELETE, "DELETE"),
            (bv::HEAD, "HEAD"),
            (bv::OPTIONS, "OPTIONS"),
            (bv::IO_NOT_FOUND, "IoNotFound"),
            (bv::IO_PERMISSION_DENIED, "IoPermissionDenied"),
            (bv::IO_ALREADY_EXISTS, "IoAlreadyExists"),
            (bv::IO_INVALID_INPUT, "IoInvalidInput"),
            (bv::IO_INTERRUPTED, "IoInterrupted"),
            (bv::IO_UNEXPECTED_EOF, "IoUnexpectedEof"),
            (bv::IO_WRITE_ZERO, "IoWriteZero"),
            (bv::IO_UNKNOWN, "IoUnknown"),
            (bv::JSON_SYNTAX, "JsonSyntax"),
            (bv::JSON_TYPE_MISMATCH, "JsonTypeMismatch"),
            (bv::JSON_MISSING_FIELD, "JsonMissingField"),
            (bv::JSON_UNKNOWN, "JsonUnknown"),
            (bv::TOML_SYNTAX, "TomlSyntax"),
            (bv::TOML_TYPE_MISMATCH, "TomlTypeMismatch"),
            (bv::TOML_MISSING_FIELD, "TomlMissingField"),
            (bv::TOML_UNKNOWN, "TomlUnknown"),
            (bv::PARSE_EMPTY, "ParseEmpty"),
            (bv::PARSE_INVALID_DIGIT, "ParseInvalidDigit"),
            (bv::PARSE_OVERFLOW, "ParseOverflow"),
            (bv::PARSE_UNDERFLOW, "ParseUnderflow"),
            (bv::HTTP_CONNECT, "HttpConnect"),
            (bv::HTTP_TLS, "HttpTls"),
            (bv::HTTP_TIMEOUT, "HttpTimeout"),
            (bv::HTTP_INVALID_URL, "HttpInvalidUrl"),
            (bv::HTTP_INVALID_RESPONSE, "HttpInvalidResponse"),
            (bv::HTTP_CLOSED_EARLY, "HttpClosedEarly"),
            (bv::HTTP_STATUS_CODE, "HttpStatusCode"),
            (bv::HTTP_UNKNOWN, "HttpUnknown"),
            (bv::REGEX_INVALID_PATTERN, "RegexInvalidPattern"),
            (bv::REGEX_TOO_BIG, "RegexTooBig"),
            (bv::PG_CONNECT, "PgConnect"),
            (bv::PG_TLS, "PgTls"),
            (bv::PG_AUTH_FAILED, "PgAuthFailed"),
            (bv::PG_QUERY, "PgQuery"),
            (bv::PG_TYPE_MISMATCH, "PgTypeMismatch"),
            (bv::PG_NO_SUCH_COLUMN, "PgNoSuchColumn"),
            (bv::PG_CLOSED, "PgClosed"),
            (bv::PG_TIMEOUT, "PgTimeout"),
            (bv::PG_TXN_ABORTED, "PgTxnAborted"),
            (bv::PG_UNKNOWN, "PgUnknown"),
            (bv::TCP_CONNECT, "TcpConnect"),
            (bv::TCP_TLS, "TcpTls"),
            (bv::TCP_CLOSED, "TcpClosed"),
            (bv::TCP_TIMEOUT, "TcpTimeout"),
            (bv::TCP_UNKNOWN, "TcpUnknown"),
            (bv::TIME_PARSE_FORMAT, "TimeParseFormat"),
            (bv::TIME_OUT_OF_RANGE, "TimeOutOfRange"),
            (bv::BYTES_INVALID_UTF8, "BytesInvalidUtf8"),
            (bv::BYTES_INVALID_HEX, "BytesInvalidHex"),
            (bv::BYTES_INVALID_BASE64, "BytesInvalidBase64"),
            (bv::BYTES_BYTE_OUT_OF_RANGE, "BytesByteOutOfRange"),
            (bv::BYTES_OUT_OF_BOUNDS, "BytesOutOfBounds"),
            (bv::CHANNEL_TIMEOUT, "ChannelTimeout"),
            (bv::CHANNEL_CLOSED, "ChannelClosed"),
            (bv::PG_POOL, "PgPool"),
            (bv::PG_TX, "PgTx"),
            (bv::PG_CURSOR, "PgCursor"),
            (bv::V_INT, "VInt"),
            (bv::V_STR, "VStr"),
            (bv::V_BOOL, "VBool"),
            (bv::V_FLOAT, "VFloat"),
            (bv::V_NULL, "VNull"),
            (bv::V_LIST, "VList"),
        ];
        for (variant, name) in constants {
            let tag = variant.tag();
            assert_eq!(tag.name(), *name);
            // The checker declares the error enums of the postgres and tcp
            // modules only when those features are built.
            if (variant.ty == ty::PG_ERROR && !cfg!(feature = "postgres"))
                || (variant.ty == ty::TCP_ERROR && !cfg!(feature = "tcp"))
            {
                continue;
            }
            // The variants of the builtin handle types (`PgPool`,
            // postgres `Value`) are the run time's own: the checker sees
            // the types as opaque.
            let Some(declared) = defs.variants.get(&variant.ty.0) else {
                assert!(
                    HANDLE_VARIANTS
                        .iter()
                        .any(|(_, vs)| vs.iter().any(|(v, _)| v == name)),
                    "{name} is declared by the checker"
                );
                continue;
            };
            let declared = declared[variant.ordinal as usize];
            let def = defs.defs[declared.0 as usize];
            assert_eq!(
                crate::intern::resolve(def.name),
                *name,
                "the declaration of {name}"
            );
            let crate::defs::DefKind::Variant { ordinal, arity, .. } = def.kind else {
                panic!("{name} is declared as a variant");
            };
            assert_eq!(ordinal, variant.ordinal, "the ordinal of {name}");
            assert_eq!(arity as usize, tag.arity(), "the arity of {name}");
        }
        assert_eq!(builtin_type(ty::ANON_RECORD).name, crate::defs::ANON_RECORD);
    }

    #[test]
    fn the_builtin_variants_have_the_ordinals_of_their_definitions() {
        let (defs, _) = crate::typechecker::names::builtins();
        for (ty, variants) in &defs.variants {
            let info = builtin_type(TypeId(*ty));
            let names: Vec<&str> = info.variants().iter().map(|v| v.name.as_str()).collect();
            let declared: Vec<String> = variants
                .iter()
                .map(|v| crate::intern::resolve(defs.defs[v.0 as usize].name))
                .collect();
            assert_eq!(names, declared, "the variants of {}", info.name);
        }
    }
}
