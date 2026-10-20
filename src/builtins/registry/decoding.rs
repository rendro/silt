//! What the decoding builtins decode: the one description that the
//! compiler's check of a decoder's type argument, its help texts, the
//! decoders' refusals at run time and the reference pages read.
//!
//! A decoding builtin's row (`json.parse`, `toml.parse_list`,
//! `postgres.query`, ...) carries a [`Decoding`]: its [`Family`], which
//! says the field types a record given to it may have, and whether its
//! type argument may be a primitive type.

use crate::module::BUILTIN_PRIMITIVE_NAMES;
use crate::typeinfo::FieldType;

/// What to do with a type given to a postgres function to decode rows
/// into that is no record type and no `json.Value`.
pub const ROW_TYPE_HELP: &str = "declare a record type with a field for each column";

/// A family of decoding builtins: they decode the same field types.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Family {
    Json,
    Toml,
    /// The rows of a result: `postgres.query(db, sql, params, T)`.
    Postgres,
}

/// A field type that holds no other: the field types the families tell
/// apart.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Leaf {
    Int,
    Float,
    String,
    Bool,
    Bytes,
    Date,
    Time,
    DateTime,
    JsonValue,
}

impl Leaf {
    /// Every leaf, in the order the help texts name them.
    pub const ALL: [Leaf; 9] = [
        Leaf::Int,
        Leaf::Float,
        Leaf::String,
        Leaf::Bool,
        Leaf::Bytes,
        Leaf::Date,
        Leaf::Time,
        Leaf::DateTime,
        Leaf::JsonValue,
    ];

    /// The leaf as silt writes it.
    pub fn name(self) -> &'static str {
        match self {
            Leaf::Int => "Int",
            Leaf::Float => "Float",
            Leaf::String => "String",
            Leaf::Bool => "Bool",
            Leaf::Bytes => "Bytes",
            Leaf::Date => "Date",
            Leaf::Time => "Time",
            Leaf::DateTime => "DateTime",
            Leaf::JsonValue => "json.Value",
        }
    }

    /// The leaf `field_type` is; `None` for a field type that holds
    /// others, a record type and a type no decoder has.
    pub fn of(field_type: &FieldType) -> Option<Leaf> {
        Some(match field_type {
            FieldType::Int => Leaf::Int,
            FieldType::Float => Leaf::Float,
            FieldType::String => Leaf::String,
            FieldType::Bool => Leaf::Bool,
            FieldType::Bytes => Leaf::Bytes,
            FieldType::Date => Leaf::Date,
            FieldType::Time => Leaf::Time,
            FieldType::DateTime => Leaf::DateTime,
            FieldType::JsonValue => Leaf::JsonValue,
            _ => return None,
        })
    }
}

impl Family {
    /// The leaves the family decodes, in the order of [`Leaf::ALL`].
    /// Every family decodes `List(T)`, `Option(T)`, `Map(String, T)`,
    /// tuples and non-generic record types of what it decodes.
    pub fn leaves(self) -> &'static [Leaf] {
        use Leaf::*;
        match self {
            Family::Json => &[Int, Float, String, Bool, Date, Time, DateTime, JsonValue],
            Family::Toml => &[Int, Float, String, Bool, Date, Time, DateTime],
            // `Bytes` from a `bytea` column.
            Family::Postgres => &[
                Int, Float, String, Bool, Bytes, Date, Time, DateTime, JsonValue,
            ],
        }
    }

    /// Whether the family decodes the leaf.
    pub fn decodes(self, leaf: Leaf) -> bool {
        self.leaves().contains(&leaf)
    }

    /// The family of what a map, a tuple or a nested record of a field
    /// holds: a column fills those only with a json document, so for
    /// postgres it is json's.
    pub fn nested(self) -> Family {
        match self {
            Family::Postgres => Family::Json,
            other => other,
        }
    }

    /// Whether the type argument is the type of a row of a result.
    pub fn rows(self) -> bool {
        self == Family::Postgres
    }

    /// The part of `field_type` that this family has no decoder for,
    /// not counting the fields of the record types it names: a leaf it
    /// does not decode, where the field type holds it.
    pub fn lacks(self, field_type: &FieldType) -> Option<&'static str> {
        match field_type {
            FieldType::List(t) | FieldType::Option(t) => self.lacks(t),
            FieldType::Map(t) => self.nested().lacks(t),
            FieldType::Tuple(ts) => ts.iter().find_map(|t| self.nested().lacks(t)),
            _ => Leaf::of(field_type)
                .filter(|leaf| !self.decodes(*leaf))
                .map(Leaf::name),
        }
    }

    /// The field types it decodes, as shown in compile errors.
    pub fn help(self) -> String {
        let leaves: Vec<&str> = self.leaves().iter().map(|leaf| leaf.name()).collect();
        format!(
            "decodable field types are {}, List(T), Option(T), Map(String, T), tuples, \
             non-generic record types, and aliases of these",
            leaves.join(", ")
        )
    }

    /// What the help adds where the part that has no decoder is
    /// `Bytes`, which the family decodes in some places only.
    pub fn bytes_help(self) -> &'static str {
        match self.decodes(Leaf::Bytes) && !self.nested().decodes(Leaf::Bytes) {
            true => {
                "; Bytes only as a field, in an Option or in a List (a bytea column), \
                 not in a Map, a tuple or a nested record"
            }
            false => "",
        }
    }
}

/// What a decoding builtin decodes into: a record type, a primitive
/// type where it takes one, and `json.Value` where its family decodes
/// one.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Decoding {
    pub family: Family,
    /// Whether the type argument may be `Int`, `Float`, `String` or
    /// `Bool`.
    pub primitives: bool,
}

impl Decoding {
    /// `json.parse`, `json.parse_list`, `json.parse_map`, `json.decode`.
    pub const JSON: Decoding = Decoding {
        family: Family::Json,
        primitives: true,
    };
    /// `toml.parse`: a document is a table, a record.
    pub const TOML_RECORD: Decoding = Decoding {
        family: Family::Toml,
        primitives: false,
    };
    /// `toml.parse_list`, `toml.parse_map`.
    pub const TOML: Decoding = Decoding {
        family: Family::Toml,
        primitives: true,
    };
    /// The postgres functions that decode rows: a row is a record of its
    /// columns, or a `json.Value`.
    pub const POSTGRES: Decoding = Decoding {
        family: Family::Postgres,
        primitives: false,
    };

    /// Whether the type argument may be `json.Value`.
    pub fn json_value(self) -> bool {
        self.family.decodes(Leaf::JsonValue)
    }

    /// What the type argument may be, each type as `name` writes it:
    /// `Int, Float, String, Bool, json.Value, or a record type`, `a
    /// record type or json.Value`.
    pub fn type_arguments(self, name: impl Fn(&str) -> String) -> String {
        let json_value = self.json_value().then(|| name(Leaf::JsonValue.name()));
        match self.primitives {
            true => {
                let mut types: Vec<String> =
                    BUILTIN_PRIMITIVE_NAMES.iter().map(|t| name(t)).collect();
                types.extend(json_value);
                types.push("or a record type".to_string());
                types.join(", ")
            }
            false => match json_value {
                Some(json_value) => format!("a record type or {json_value}"),
                None => "a record type".to_string(),
            },
        }
    }
}
