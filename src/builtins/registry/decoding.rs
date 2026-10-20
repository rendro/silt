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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtins::decode::{self, Error};
    use crate::builtins::registry::{Row, registry};
    use crate::builtins::typed::Type;
    use crate::typeinfo::{TypeInfo, ty};

    /// The leaves as a program writes them.
    fn written(leaf: Leaf) -> &'static str {
        match leaf {
            Leaf::Date => "time.Date",
            Leaf::Time => "time.Time",
            Leaf::DateTime => "time.DateTime",
            other => other.name(),
        }
    }

    /// What a program imports: `postgres` where it is built.
    const IMPORTS: &str = match cfg!(feature = "postgres") {
        true => "import json\nimport postgres\nimport time\nimport toml\n",
        false => "import json\nimport time\nimport toml\n",
    };

    /// A program with the record types `R { f: field }` and `Inner { x:
    /// leaf }`, and `body`.
    fn program(leaf: &str, field: &str, body: &str) -> String {
        format!(
            "{IMPORTS}type Color {{ Red, Blue }}\ntype Gen(t) {{ v: t }}\n\
             type Inner {{ x: {leaf} }}\ntype R {{ f: {field} }}\n{body}\n"
        )
    }

    /// A call of `row` whose type argument is `ty`, its other arguments
    /// values of their types (`db` and `tx` a pool and a transaction).
    fn call(row: &Row, ty: &str) -> String {
        let (label, params) = row.qualified_signature(row.module).expect("a function");
        let args: Vec<String> = params
            .iter()
            .map(|[start, end]| {
                let param = &label[*start as usize..*end as usize];
                let param_type = param.split_once(": ").map_or("", |(_, t)| t);
                match param_type {
                    _ if param.starts_with("type ") => ty.to_string(),
                    "String" => "\"\"".to_string(),
                    "Int" => "1".to_string(),
                    "Value" => "json.Null".to_string(),
                    "PgTx" => "tx".to_string(),
                    // The connection, a pool; the parameters, none.
                    "a" => "db".to_string(),
                    "p" => "()".to_string(),
                    other => panic!("no argument for `{param}` ({other}) of {}", row.qualified()),
                }
            })
            .collect();
        format!("{}({})", row.qualified(), args.join(", "))
    }

    /// The function that calls `row` with the type `ty`.
    fn caller(row: &Row, ty: &str) -> String {
        let params = match row.module {
            "postgres" => "db: postgres.PgPool, tx: postgres.PgTx",
            _ => "",
        };
        format!("fn call({params}) {{\n  {}\n}}", call(row, ty))
    }

    /// The compile errors of `source`, none if it compiles.
    fn compiled(
        source: &str,
    ) -> Result<crate::session::Program, Vec<crate::diagnostic::Diagnostic>> {
        crate::session::testing::compile_decls_str(source)
    }

    /// The leaves a help text of the compiler names: those it lists
    /// before `List(T)`.
    fn helped(help: &str) -> Vec<&str> {
        let list = help
            .strip_prefix("decodable field types are ")
            .and_then(|rest| rest.split_once(", List(T)"))
            .map_or("", |(leaves, _)| leaves);
        list.split(", ").collect()
    }

    /// The part a compile error of a field says has no decoder.
    fn lacking_part(message: &str) -> Option<&str> {
        match message.split_once(", and `") {
            Some((_, rest)) => rest.split_once('`').map(|(part, _)| part),
            None => message
                .rsplit_once("has type ")
                .and_then(|(_, rest)| rest.split('`').nth(1)),
        }
    }

    /// Every decoding builtin of the registry, for field types made of
    /// each leaf (the leaf, a list, a map and a tuple of it, a record
    /// type that has a field of it and a list of that record) and of
    /// types no decoder has: the compiler accepts a record with a field
    /// of the type exactly when the run time decodes it (json's and
    /// toml's decoders reading a document; postgres's plan, which needs
    /// no server, by its first step), and the help of a refusal names the
    /// leaves the family decodes and not the leaf it lacks.
    #[test]
    fn compile_check_help_and_run_time_agree_on_every_field_type() {
        let rows: Vec<&Row> = registry()
            .modules
            .iter()
            .flat_map(|module| module.enabled_rows())
            .filter(|row| row.decoding.is_some())
            .collect();
        assert!(rows.len() >= 7, "the decoding rows: {}", rows.len());
        let mut fields: Vec<(&str, String)> = Vec::new();
        for leaf in Leaf::ALL {
            let l = written(leaf);
            for field in [
                l.to_string(),
                format!("List({l})"),
                format!("Map(String, {l})"),
                format!("(Int, {l})"),
                "Inner".to_string(),
                "Option(List(Inner))".to_string(),
            ] {
                fields.push((l, field));
            }
        }
        for field in ["Set(Int)", "Color", "Map(Int, Int)", "Gen(Int)"] {
            fields.push(("Int", field.to_string()));
        }
        let vm = crate::vm::Vm::new(crate::vm::HostIo::process());
        let rt = &vm.runtime;
        let mut compared = 0;
        for (inner, field) in &fields {
            // The types as the run time has them: through `type a`, which
            // the compiler does not check.
            let generic = program(
                inner,
                field,
                "fn go(type a) -> a {\n  panic(\"\")\n}\nfn main() {\n  let _: R = go(R)\n}",
            );
            let program_types = compiled(&generic)
                .unwrap_or_else(|e| panic!("{field}: {}\n{generic}", e[0].message))
                .types;
            for info in program_types.iter() {
                rt.types.lock().insert(info.clone());
            }
            let r: &std::sync::Arc<TypeInfo> = program_types
                .iter()
                .find(|info| info.name == "R")
                .expect("R is described");
            let (_, f) = &r.fields()[0];
            for row in &rows {
                let decoding = row.decoding.expect("a decoder");
                let family = decoding.family;
                let name = row.qualified();
                // Accepted at run time: the type argument is taken, and
                // the field is decoded or found missing, not refused.
                let target = decode::target(&name, Type::Named(r)).expect("a record type is taken");
                let decoded = match family {
                    Family::Json => decode::document(rt, &target, &serde_json::json!({})),
                    Family::Toml => {
                        decode::document(rt, &target, &::toml::Value::Table(Default::default()))
                    }
                    Family::Postgres => match decode::undecodable(rt, family, "R", "f", f) {
                        Some(message) => Err(Error::Other(message)),
                        None => Ok(crate::value::Value::Unit),
                    },
                };
                let run_time = !matches!(
                    &decoded,
                    Err(Error::Other(message)) if message.starts_with("cannot decode field")
                );
                let direct = program(inner, field, &caller(row, "R"));
                let compile = compiled(&direct);
                assert_eq!(
                    compile.is_ok(),
                    run_time,
                    "{name}, a field of {field}: the compiler {} it, the run time {}\n{direct}",
                    if compile.is_ok() {
                        "accepts"
                    } else {
                        "refuses"
                    },
                    match &decoded {
                        Err(Error::Other(message)) => message.clone(),
                        _ => "decodes it".to_string(),
                    }
                );
                let help = family.help();
                let listed = helped(&help);
                assert_eq!(
                    listed,
                    family
                        .leaves()
                        .iter()
                        .map(|leaf| leaf.name())
                        .collect::<Vec<_>>(),
                    "{help}"
                );
                match compile {
                    Ok(_) => {
                        // Every leaf of an accepted field is one the help names.
                        for leaf in Leaf::ALL {
                            let l = written(leaf);
                            if field.contains(l) || (field.contains("Inner") && l == *inner) {
                                assert!(listed.contains(&leaf.name()), "{name}, {field}: {help}");
                            }
                        }
                    }
                    Err(errors) => {
                        let error = &errors[0];
                        let help_text = error.help.join("\n");
                        if let Some(part) = lacking_part(&error.message)
                            && Leaf::ALL.iter().any(|leaf| leaf.name() == part)
                        {
                            match family.decodes(
                                Leaf::ALL
                                    .into_iter()
                                    .find(|leaf| leaf.name() == part)
                                    .expect("a leaf"),
                            ) {
                                // Decoded in some places only: the help says where.
                                true => assert!(
                                    help_text.contains(family.bytes_help())
                                        && !family.bytes_help().is_empty(),
                                    "{name}, {field}: {help_text}"
                                ),
                                false => assert!(
                                    !helped(&help_text).contains(&part),
                                    "{name}, {field}: {help_text}"
                                ),
                            }
                        }
                    }
                }
                compared += 1;
            }
        }
        assert_eq!(compared, fields.len() * rows.len());
        // A type argument that is a primitive type or `json.Value`: the
        // compiler takes it where the run time does.
        for row in &rows {
            let name = row.qualified();
            for (written, ty) in [
                ("Int", Type::Primitive(FieldType::Int)),
                ("String", Type::Primitive(FieldType::String)),
                (
                    "json.Value",
                    Type::Named(crate::typeinfo::builtin_type(ty::JSON_VALUE)),
                ),
            ] {
                let direct = program("Int", "Int", &caller(row, written));
                assert_eq!(
                    compiled(&direct).is_ok(),
                    decode::target(&name, ty).is_ok(),
                    "{name}({written})\n{direct}"
                );
            }
        }
    }
}
