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

impl TypeChecker {
    /// Check `subject`, the type of an argument of a builtin, as `check`
    /// says, for a use at `origin`. The function or method that passes
    /// the type on to the builtin, `passer`, is named in a note.
    pub(super) fn check_builtin_argument(
        &mut self,
        check: BuiltinCheck,
        subject: &Type,
        passer: Option<Symbol>,
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
        let span = origin.span;
        let refusal = match check {
            BuiltinCheck::Decodes {
                builtin,
                decoding,
                written,
            } => self.decode_refusal(&resolve(builtin), decoding, written, &ty, span),
            BuiltinCheck::Connection { builtin } => {
                connection_refusal(&resolve(builtin), &ty, span)
            }
            BuiltinCheck::Params { builtin } => params_refusal(&resolve(builtin), &ty, span),
        };
        let Some(mut diagnostic) = refusal else {
            return;
        };
        // One refusal for one type at one place: a definition that gives
        // its type parameter to two decoders that both refuse the type
        // says the first.
        if !self
            .lacking
            .insert((span, format!("builtin argument {}", shown(&ty))))
        {
            return;
        }
        if let Some(passer) = passer {
            diagnostic =
                diagnostic.with_note(format!("`{passer}` passes it to `{}`", check.builtin()));
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
    /// decodes records is refused as well. For json (`json.Decode`), a
    /// record or enum type with a written `json.Decode` impl is decoded
    /// by the impl, and not looked into.
    pub(super) fn decode_refusal(
        &self,
        builtin_name: &str,
        decoding: Decoding,
        written: Option<Symbol>,
        ty: &Type,
        span: Span,
    ) -> Option<Diagnostic> {
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
        let type_name = resolve(written.unwrap_or(head.name));
        let refusal = |code, message: String, help: &str| {
            Some(Diagnostic::error(code, span, message).with_help(help))
        };
        if crate::defs::builtin_types()
            .get(head.id.0.0 as usize)
            .is_none()
        {
            let enum_type = self.tables.is_enum(head);
            if enum_type && !family.decodes_enums() {
                return refusal(
                    Code::Undecodable,
                    format!(
                        "`{builtin_name}` cannot decode `{type_name}`: it is an enum type, and enums have no decoder"
                    ),
                    record_help,
                );
            }
            if self.tables.is_generic(head) {
                let kind = if enum_type { "enum" } else { "record" };
                let message = match written {
                    // Through an alias that gives the type its arguments
                    // (`type BI = Box(Int)`) the type is one instance;
                    // the decoders read the fields as the type declares
                    // them.
                    Some(_) => format!(
                        "`{builtin_name}` cannot decode `{type_name}`: it names `{ty}`, \
                         an instance of the generic {kind} type `{}`, and a decoder reads \
                         the fields of a {kind} type without parameters",
                        head.name
                    ),
                    // An instance that no alias names.
                    None if free_vars_in(ty).is_empty() => format!(
                        "`{builtin_name}` cannot decode `{ty}`: it is an instance of the \
                         generic {kind} type `{}`, and a decoder reads the fields of a \
                         {kind} type without parameters",
                        head.name
                    ),
                    None => format!(
                        "`{builtin_name}` cannot decode `{type_name}`: it is a generic {kind} \
                         type, and a decoder builds a value of one type"
                    ),
                };
                return refusal(Code::Undecodable, message, generic_help);
            }
            let impls = family == Family::Json;
            let found = self.undecodable_field(head, family, impls, &mut HashSet::new())?;
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
            return refusal(
                Code::Undecodable,
                format!(
                    "`{builtin_name}` cannot decode `{type_name}`: its type argument must be {accepted}"
                ),
                family.type_argument_help(),
            );
        }
        None
    }

    /// The first field that `family` cannot decode, in the record or
    /// enum type `record` or in a record or enum type nested in it (which
    /// `family.nested()` decodes). `None` if there is none. With
    /// `impls`, a nested type with a written `json.Decode` impl is the
    /// impl's to decode.
    fn undecodable_field(
        &self,
        record: TypeRef,
        family: Family,
        impls: bool,
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
            let mut nested = Vec::new();
            let lacking = match self.tables.decoded_field_type(&ty, &mut nested) {
                Err(part) => Some(part),
                Ok(field_type) => family.lacks(&field_type).map(str::to_string).or_else(|| {
                    let enum_type = (nested.iter().map(|id| type_ref(*id)))
                        .find(|t| self.tables.is_enum(*t))?;
                    (!family.decodes_enums()).then(|| resolve(enum_type.name))
                }),
            };
            if let Some(part) = lacking {
                return Some(UndecodableField {
                    record: resolve(record.name),
                    field: resolve(field),
                    in_variant,
                    field_type: shown(&ty),
                    part,
                    nested: false,
                });
            }
            for nested_record in nested {
                let family = family.nested();
                let nested_record = type_ref(nested_record);
                if impls && !self.by_structure(TraitKey::builtin("Decode"), nested_record) {
                    continue;
                }
                if let Some(found) = self.undecodable_field(nested_record, family, impls, seen) {
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
            format!("the connection of `{builtin_name}` must be a PgPool or a PgTx"),
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
