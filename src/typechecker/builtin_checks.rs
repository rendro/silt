//! What a builtin asks of its arguments beyond its signature
//! (`Pred::Builtin`): a decoder (`json.parse(text, T)`,
//! `toml.parse(text, T)` and their `_list` / `_map` forms,
//! `postgres.query(db, sql, params, T)` and the other postgres functions
//! that read rows), that its type argument is a type it decodes; a
//! postgres function, that its connection is a `PgPool` or a `PgTx` and
//! a statement's parameters a tuple. What each decoder decodes is its
//! registry row's `Decoding`; the field types of a record are what the
//! decoders decode by at run time (`Tables::decoded_field_type`).

use super::decodable::shown;
use super::solve::Origin;
use super::*;
use crate::builtins::registry::decoding::{Decoding, Family, JSON_VALUE_ELSEWHERE, ROW_TYPE_HELP};
use crate::defs::TypeId;
use crate::types::canonical::canonicalize;
use std::collections::HashSet;

/// A record field, or a field of an enum's variant, that a decoder
/// cannot decode.
struct UndecodableField {
    /// The record type that declares the field, or the enum type that
    /// declares the variant.
    record: String,
    /// The field's name, or the name of the variant whose field it is.
    field: String,
    /// The field is one of a variant's: `record` is an enum type.
    in_variant: bool,
    /// The field's type.
    field_type: String,
    /// The part of the field's type that has no decoder.
    part: String,
    /// The record type is nested in the one given to the decoder (it
    /// may be that one itself, in a field of its own).
    nested: bool,
}

/// The key in `lacking` of a builtin's refusal of the type `ty` at
/// `span`: a type given to a function whose body hands it to a decoder
/// that refused it owes no `json.Decode` refusal of its bound there
/// (`settle_decode_gaps`).
pub(super) fn refused_argument(span: Span, ty: &Type) -> (Span, String) {
    (span, format!("builtin argument {}", shown(ty)))
}

impl TypeChecker {
    /// Check `subject`, the type of an argument of a builtin, as `check`
    /// says, for a use at `origin`. The function or method that passes
    /// the type on to the builtin, `passer`, is named in a note; the
    /// module of the definition that asks it, `owner`, encodes and
    /// decodes its own opaque types (`opaque_encode_refusal`).
    pub(super) fn check_builtin_argument(
        &mut self,
        check: BuiltinCheck,
        subject: &Type,
        passer: Option<Symbol>,
        owner: Option<crate::session::ModuleId>,
        origin: Origin,
    ) {
        let ty = canonicalize(self.tables.resolver(), &self.apply(subject));
        // An annotation variable: what the body asks of it, each use of
        // its function asks of the type it gives (`settle_builtin_needs`).
        if let Type::Rigid(r) = ty {
            let key = (self.rigid_rep_var(r.var), self.body_owner);
            let needs = self.builtin_needs.entry(key);
            let needs = needs.or_default();
            if !needs.contains(&check.unwritten()) {
                needs.push(check.unwritten());
            }
            return;
        }
        if matches!(ty, Type::Var(_) | Type::Error | Type::Never) {
            return;
        }
        // What an encoder is given is encoded at any depth: a type
        // variable in it owes what the whole does (each use of its
        // function, of the type it gives).
        if let BuiltinCheck::Params { builtin } | BuiltinCheck::Encodes { builtin } = check {
            let check = BuiltinCheck::Encodes { builtin };
            self.owe_for_variables_inside(check, &ty, passer, owner, origin);
        }
        let span = origin.span;
        let refusal = match check {
            BuiltinCheck::Decodes {
                builtin,
                decoding,
                written,
            } => self.decode_refusal(&resolve(builtin), decoding, written, &ty, owner, span),
            BuiltinCheck::Connection { builtin } => {
                connection_refusal(&resolve(builtin), &ty, span)
            }
            // (A statement's parameters are sent encoded as json.)
            BuiltinCheck::Params { builtin } => params_refusal(&resolve(builtin), &ty, span)
                .or_else(|| self.opaque_encode_refusal(&ty, span, owner)),
            BuiltinCheck::Encodes { .. } => self.opaque_encode_refusal(&ty, span, owner),
        };
        let Some(mut diagnostic) = refusal else {
            // A decoded list, option, map or tuple: what a type variable
            // in it stands for is decoded too.
            if let BuiltinCheck::Decodes {
                builtin, decoding, ..
            } = check
                && decoded_parts(&ty).is_some()
            {
                let check = BuiltinCheck::Decodes {
                    builtin,
                    decoding,
                    written: None,
                };
                self.owe_for_variables_inside(check, &ty, passer, owner, origin);
            }
            return;
        };
        // One refusal for one type at one place: a definition that gives
        // its type parameter to two decoders that both refuse the type
        // says the first.
        if !self.lacking.insert(refused_argument(span, &ty)) {
            return;
        }
        if let Some(passer) = passer {
            diagnostic =
                diagnostic.with_note(format!("`{passer}` passes it to `{}`", check.builtin()));
        }
        // (The bound that passes it on refused the same encoding there:
        // this one, which names the encoder, takes its place.)
        let bound = (span, format!("opaque {}", diagnostic.message));
        if self.lacking.contains(&bound)
            && let Some(earlier) = (self.errors.iter_mut())
                .find(|e| e.span == span && e.message == diagnostic.message && e.notes.is_empty())
        {
            *earlier = diagnostic;
            return;
        }
        self.errors.push(diagnostic);
    }

    /// The error of the decoder `builtin`, which decodes as `decoding`
    /// says, given the type `ty` (a call names it by the alias
    /// `written`, if by one) at `span`, if it has no decoder for it. A
    /// record type, or for json and toml an enum type, of the program is
    /// decoded if each field the decoder would have to fill has a
    /// decoder; the error names the field and its type. A generic type,
    /// an enum given to postgres, a builtin enum, a builtin
    /// container type, or a primitive type given to a decoder that only
    /// decodes records is refused as well. A record or enum type with a
    /// written `json.Decode` impl is decoded by the impl (toml's and
    /// postgres's decoders too: one data layer, one override), and not
    /// looked into, at the top and as the type of a field. A definition
    /// of `owner` decodes that module's opaque types.
    pub(super) fn decode_refusal(
        &self,
        builtin_name: &str,
        decoding: Decoding,
        written: Option<Symbol>,
        ty: &Type,
        owner: Option<crate::session::ModuleId>,
        span: Span,
    ) -> Option<Diagnostic> {
        // A list, an option, a map with String keys or a tuple
        // (`List(Int)`, `(Int, String)`): a decoder that takes any value
        // reads it as the type of a field, each part as one. (A type
        // with its arguments left open, `List`, is not one type.)
        if decoding.primitives
            && let Some(parts) = decoded_parts(ty)
            && !self.open_type_arguments.contains(&span)
        {
            return (parts.into_iter())
                .find_map(|part| self.part_refusal(builtin_name, decoding, ty, part, owner, span));
        }
        let head = match ty {
            Type::Generic(t, _) => *t,
            Type::Int => TypeRef::builtin("Int"),
            Type::Float => TypeRef::builtin("Float"),
            Type::String => TypeRef::builtin("String"),
            Type::Bool => TypeRef::builtin("Bool"),
            Type::List(_) => TypeRef::builtin("List"),
            Type::Map(..) => TypeRef::builtin("Map"),
            Type::Set(_) => TypeRef::builtin("Set"),
            Type::Channel(_) => TypeRef::builtin("Channel"),
            // A tuple of types (`json.parse(text, (Int, String))`).
            Type::Tuple(_) => {
                let accepted = decoding.type_arguments(str::to_string);
                return Some(
                    Diagnostic::error(
                        Code::Undecodable,
                        span,
                        format!(
                            "`{builtin_name}` cannot decode `{}`: its type argument must be {accepted}",
                            self.show_type(ty)
                        ),
                    )
                    .with_help(decoding.family.type_argument_help()),
                );
            }
            _ => return None,
        };
        let family = decoding.family;
        let decodable_types_help = family.help();
        // What to decode into instead of a type that is no record. A row
        // is decoded into a record of its columns.
        let record_help = family.record_help();
        let generic_help = match family.rows() {
            true => ROW_TYPE_HELP,
            false => decodable_types_help.as_str(),
        };
        // (A type of another module with its module, `time.Date`; an
        // alias by its name.)
        let type_name = match written {
            Some(alias) => resolve(alias),
            None => self.show_type(&Type::Generic(head, Vec::new())),
        };
        let refusal = |code, message: String, help: &str| {
            Some(Diagnostic::error(code, span, message).with_help(help))
        };
        if crate::defs::builtin_types()
            .get(head.id.0.0 as usize)
            .is_none()
        {
            if self.has_decode_impl(head) {
                return None;
            }
            if let Some(refusal) = self.opaque_decode_refusal(head, span, owner) {
                return Some(refusal);
            }
            let enum_type = self.tables.is_enum(head);
            // (A generic type first: no impl can be written for one, so
            // the enum's help below could not be followed.)
            if self.tables.is_generic(head) {
                let (kind, a_kind) = match enum_type {
                    true => ("enum", "an enum"),
                    false => ("record", "a record"),
                };
                let message = match written {
                    // Through an alias that gives the type its arguments
                    // (`type BI = Box(Int)`) the type is one instance;
                    // the decoders read the fields as the type declares
                    // them.
                    Some(_) => format!(
                        "`{builtin_name}` cannot decode `{type_name}`: it names `{}`, \
                         an instance of the generic {kind} type `{}`, and a decoder reads \
                         the fields of {a_kind} type without parameters",
                        self.show_type(ty),
                        self.show_type(&Type::Generic(head, Vec::new()))
                    ),
                    // An instance that no alias names.
                    None if free_vars_in(ty).is_empty() => format!(
                        "`{builtin_name}` cannot decode `{}`: it is an instance of the \
                         generic {kind} type `{type_name}`, and a decoder reads the fields of \
                         {a_kind} type without parameters",
                        self.show_type(ty)
                    ),
                    None => format!(
                        "`{builtin_name}` cannot decode `{type_name}`: it is a generic {kind} \
                         type, and a decoder builds a value of one type"
                    ),
                };
                return refusal(Code::Undecodable, message, generic_help);
            }
            // (A row is decoded into a record of its columns, or by the
            // type's own impl, which the program may write for its enum.)
            if enum_type && !family.decodes_enums() {
                return refusal(
                    Code::Undecodable,
                    format!(
                        "`{builtin_name}` cannot decode `{type_name}`: it is an enum type, and enums have no decoder"
                    ),
                    &format!(
                        "declare a record type, or write a `json.Decode` impl for `{}`",
                        self.written_type(head)
                    ),
                );
            }
            // A toml document is a table: of an enum, only a variant
            // with fields is one.
            if enum_type
                && decoding == Decoding::TOML_RECORD
                && (self.tables.enums.get(&head))
                    .is_some_and(|info| info.variants.iter().all(|v| v.field_types.is_empty()))
            {
                return refusal(
                    Code::Undecodable,
                    format!(
                        "`{builtin_name}` cannot decode `{type_name}`: a toml document is a \
                         table, and no variant of `{}` has fields",
                        self.show_type(&Type::Generic(head, Vec::new()))
                    ),
                    record_help,
                );
            }
            let found = self.undecodable_field(head, family, &mut HashSet::new())?;
            let (place, has, kind) = match found.in_variant {
                true => ("variant", "a field of type", "an enum"),
                false => ("field", "type", "a record"),
            };
            let owner = if found.nested {
                format!(" of `{}`, {kind} type nested in it,", found.record)
            } else {
                String::new()
            };
            let part = if found.part == found.field_type {
                "which has no decoder".to_string()
            } else {
                format!("and `{}` has no decoder", found.part)
            };
            let help = match found.part.as_str() {
                "Bytes" => format!("{decodable_types_help}{}", family.bytes_help()),
                _ => decodable_types_help.clone(),
            };
            return refusal(
                Code::Undecodable,
                format!(
                    "`{builtin_name}` cannot decode `{type_name}`: {place} `{}`{owner} has {has} `{}`, {part}",
                    found.field, found.field_type
                ),
                &help,
            );
        }
        if head.id == crate::typeinfo::ty::JSON_VALUE {
            if decoding.json_value() {
                return None;
            }
            return refusal(
                Code::Undecodable,
                format!("`{builtin_name}` cannot decode `json.Value`, {JSON_VALUE_ELSEWHERE}"),
                record_help,
            );
        }
        if !crate::typeinfo::builtin_type(head.id).variants().is_empty() {
            let why = match family.decodes_enums() {
                true => "it is a builtin enum type, which has no decoder",
                false => "it is an enum type, and enums have no decoder",
            };
            return refusal(
                Code::Undecodable,
                format!("`{builtin_name}` cannot decode `{type_name}`: {why}"),
                record_help,
            );
        }
        // Builtin types, by their declaration. A decoder that takes a
        // primitive type decodes `Int`, `Float`, `String` and `Bool`;
        // the others, and every decoder given a container type such as
        // `List`, need a record type (or `json.Value`).
        let builtin = crate::defs::builtin_types()[head.id.0.0 as usize].0;
        let is_primitive = crate::module::BUILTIN_PRIMITIVE_NAMES.contains(&builtin);
        let is_container = crate::module::BUILTIN_GENERIC_CONTAINER_NAMES.contains(&builtin);
        if is_container || (is_primitive && !decoding.primitives) {
            let accepted = decoding.type_arguments(str::to_string);
            // A container type the call gives its arguments by its
            // arguments (`List(Int)`).
            let type_name = match written.is_none() && free_vars_in(ty).is_empty() {
                true => self.show_type(ty),
                false => type_name,
            };
            // `toml.parse` reads a list or a map with a function of its
            // own; a decoder that takes any value, the containers of the
            // field types.
            let module = (builtin_name.strip_suffix(".parse")).filter(|_| !family.rows());
            let help = match (module, ty) {
                _ if decoding.primitives => family.help(),
                (Some(module), Type::List(_)) => format!("use `{module}.parse_list` for a list"),
                (Some(module), Type::Map(..)) => format!("use `{module}.parse_map` for a map"),
                _ => family.type_argument_help().to_string(),
            };
            return refusal(
                Code::Undecodable,
                format!(
                    "`{builtin_name}` cannot decode `{type_name}`: its type argument must be {accepted}"
                ),
                &help,
            );
        }
        // Any other builtin type (`time.DateTime`, `Bytes`, `task.Handle`)
        // is no type argument of a decoder; a decoder decodes some of
        // them as the type of a record's field.
        if !is_primitive {
            let accepted = decoding.type_arguments(str::to_string);
            let decoded = (family.leaves().iter()).any(|leaf| leaf.name() == type_name);
            let help = match decoded {
                true => format!(
                    "a decoder decodes a `{type_name}` as the type of a record's field: \
                     `type R {{ at: {type_name} }}`"
                ),
                false => family.type_argument_help().to_string(),
            };
            return refusal(
                Code::Undecodable,
                format!(
                    "`{builtin_name}` cannot decode `{type_name}`: its type argument must be {accepted}"
                ),
                &help,
            );
        }
        None
    }

    /// The error of the decoder `builtin`, which decodes as `decoding`
    /// says, given `whole` at `span`, if `part` of it, a type its list,
    /// option, map or tuple holds, has no decoder: a record or enum type
    /// as the decoder judges it given that type.
    fn part_refusal(
        &self,
        builtin: &str,
        decoding: Decoding,
        whole: &Type,
        part: &Type,
        owner: Option<crate::session::ModuleId>,
        span: Span,
    ) -> Option<Diagnostic> {
        use crate::builtins::registry::decoding::Leaf;
        use crate::typeinfo::ty as bt;
        let family = decoding.family.nested();
        let leaf = match part {
            // (What a type variable stands for owes the check: see
            // `check_builtin_argument`.)
            Type::Var(_) | Type::Rigid(_) | Type::Error | Type::Never => return None,
            Type::Int => Some(Leaf::Int),
            Type::Float => Some(Leaf::Float),
            Type::String => Some(Leaf::String),
            Type::Bool => Some(Leaf::Bool),
            Type::Generic(t, args) if args.is_empty() => match t.id {
                id if id == bt::DATE => Some(Leaf::Date),
                id if id == bt::TIME => Some(Leaf::Time),
                id if id == bt::DATE_TIME => Some(Leaf::DateTime),
                id if id == bt::JSON_VALUE => Some(Leaf::JsonValue),
                id if id == TypeRef::builtin("Bytes").id => Some(Leaf::Bytes),
                _ => None,
            },
            _ => None,
        };
        match (leaf, decoded_parts(part), part) {
            (Some(leaf), ..) if family.decodes(leaf) => None,
            (None, Some(parts), _) => (parts.into_iter())
                .find_map(|inner| self.part_refusal(builtin, decoding, whole, inner, owner, span)),
            // A record or enum type of the program.
            (None, None, Type::Generic(t, _))
                if crate::defs::builtin_types()
                    .get(t.id.0.0 as usize)
                    .is_none() =>
            {
                let decoding = Decoding {
                    family,
                    primitives: true,
                };
                self.decode_refusal(builtin, decoding, None, part, owner, span)
            }
            _ => {
                let help = match leaf {
                    Some(Leaf::Bytes) => format!("{}{}", family.help(), family.bytes_help()),
                    _ => family.help(),
                };
                Some(
                    Diagnostic::error(
                        Code::Undecodable,
                        span,
                        format!(
                            "`{builtin}` cannot decode `{}`: `{}` has no decoder",
                            self.show_type(whole),
                            self.show_type(part)
                        ),
                    )
                    .with_help(help),
                )
            }
        }
    }

    /// The first field that `family` cannot decode, in the record or
    /// enum type `record` or in a record or enum type nested in it (which
    /// `family.nested()` decodes). `None` if there is none. A nested
    /// type with a written `json.Decode` impl is the impl's to decode:
    /// it is not looked into, and an enum type with one is decoded
    /// where a family decodes no enum (a postgres row's field).
    fn undecodable_field(
        &self,
        record: TypeRef,
        family: Family,
        seen: &mut HashSet<(TypeId, Family)>,
    ) -> Option<UndecodableField> {
        if !seen.insert((record.id, family)) {
            return None;
        }
        let type_ref = |id: TypeId| {
            let name = self
                .defs
                .as_ref()
                .map_or(record.name, |defs| defs.get(id.0).name);
            TypeRef { id, name }
        };
        let in_variant = self.tables.is_enum(record);
        let fields = match in_variant {
            // (Each field of each variant, by the variant's name.)
            true => (self.tables.decoded_variant_fields(record).into_iter())
                .flat_map(|(name, types)| types.into_iter().map(move |ty| (name, ty)))
                .collect(),
            false => self.tables.decoded_record_fields(record),
        };
        for (field, ty) in fields {
            // A type that is unknown where the field writes it is that
            // error alone.
            if ty.holds_error() {
                continue;
            }
            let mut nested = Vec::new();
            let lacking = match self.tables.decoded_field_type(&ty, &mut nested) {
                Err(part) => Some(part),
                Ok(field_type) => family.lacks(&field_type).map(str::to_string).or_else(|| {
                    let enum_type = (nested.iter().map(|id| type_ref(*id)))
                        .find(|t| self.tables.is_enum(*t) && !self.has_decode_impl(*t))?;
                    (!family.decodes_enums()).then(|| match self.other_module(enum_type) {
                        Some(module) => format!("{module}.{}", enum_type.name),
                        None => resolve(enum_type.name),
                    })
                }),
            };
            if let Some(part) = lacking {
                return Some(UndecodableField {
                    record: resolve(record.name),
                    field: resolve(field),
                    in_variant,
                    field_type: self.show_type(&ty),
                    part,
                    nested: false,
                });
            }
            for nested_record in nested {
                let family = family.nested();
                let nested_record = type_ref(nested_record);
                if self.has_decode_impl(nested_record) {
                    continue;
                }
                if let Some(found) = self.undecodable_field(nested_record, family, seen) {
                    return Some(UndecodableField {
                        nested: true,
                        ..found
                    });
                }
            }
        }
        None
    }
}

/// The error of the postgres function `builtin_name` given the
/// connection of the type `ty` at `span`, if it is no `PgPool` and no
/// `PgTx`.
fn connection_refusal(builtin_name: &str, ty: &Type, span: Span) -> Option<Diagnostic> {
    match ty {
        Type::Generic(t, _)
            if t.id == crate::typeinfo::ty::PG_POOL || t.id == crate::typeinfo::ty::PG_TX =>
        {
            None
        }
        _ => Some(Diagnostic::error(
            Code::TypeMismatch,
            span,
            format!(
                "the connection of `{builtin_name}` must be a postgres.PgPool or a postgres.PgTx"
            ),
        )),
    }
}

/// The error of the postgres function `builtin_name` given the
/// parameters of the type `ty` at `span`, if they are no tuple and not
/// `()` (`(id)` is `id`: the help says to write `(id,)`).
fn params_refusal(builtin_name: &str, ty: &Type, span: Span) -> Option<Diagnostic> {
    match ty {
        Type::Tuple(_) | Type::Unit => None,
        other => Some(
            Diagnostic::error(
                Code::TypeMismatch,
                span,
                format!(
                    "the parameters of `{builtin_name}` must be a tuple, got `{}`",
                    shown(other)
                ),
            )
            .with_help("write (id,)"),
        ),
    }
}

/// The types a list, an option, a map with String keys or a tuple `ty`
/// holds, which a decoder reads as the types of fields; `None` for any
/// other type.
pub(super) fn decoded_parts(ty: &Type) -> Option<Vec<&Type>> {
    match ty {
        Type::List(t) => Some(vec![&**t]),
        Type::Map(k, v) if matches!(**k, Type::String) => Some(vec![&**v]),
        Type::Tuple(ts) if !ts.is_empty() => Some(ts.iter().collect()),
        Type::Generic(t, args) if t.id == crate::typeinfo::ty::OPTION && args.len() == 1 => {
            Some(vec![&args[0]])
        }
        _ => None,
    }
}

/// The type variables among the types a list, an option, a map with
/// String keys or a tuple `ty` holds, at any depth through these.
pub(super) fn decoded_variables(ty: &Type) -> Vec<Type> {
    let mut out = Vec::new();
    for part in decoded_parts(ty).unwrap_or_default() {
        match part {
            Type::Var(_) | Type::Rigid(_) => out.push(part.clone()),
            _ => out.extend(decoded_variables(part)),
        }
    }
    out
}
