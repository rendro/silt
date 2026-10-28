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

/// Why a map whose keys are no `String`s has no decoder: the keys of a
/// JSON object or a TOML table are strings.
const MAP_KEY: &str = "a decoded map's key type must be String";

/// Whether `ty` is a map whose key type is known and no `String`.
fn map_with_other_keys(ty: &Type) -> bool {
    matches!(ty, Type::Map(key, _) if !matches!(**key, Type::String | Type::Var(_)))
}

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
    /// The part is a map whose keys are no `String`s.
    map_key: bool,
    /// The record type is nested in the one given to the decoder (it
    /// may be that one itself, in a field of its own).
    nested: bool,
    /// The field's type grows with every level: it gives the generic
    /// type that declares it ever deeper arguments (`n:
    /// Option(T(List(a)))` in `type T(a)`); `field_type` is as declared.
    grows: bool,
}

/// The note that names the way a type takes to the builtin `builtin`
/// that refuses it: from `passer`, the function or method the use calls,
/// through `links`, the ones each passes it to in turn.
pub(super) fn passes_note(passer: Symbol, links: &[Symbol], builtin: Symbol) -> String {
    let mut note = format!("`{passer}` passes it to ");
    for link in links {
        note.push_str(&format!("`{link}`, which passes it to "));
    }
    note.push_str(&format!("`{builtin}`"));
    note
}

impl TypeChecker {
    /// The key in `lacking` of a builtin's refusal of the type `ty` at
    /// `span`: a type given to a function whose body hands it to a decoder
    /// that refused it owes no `json.Decode` refusal of its bound there
    /// (`settle_decode_gaps`). (A generic type written with its arguments
    /// open is the generic type, `Box`.)
    pub(super) fn refused_argument(&self, span: Span, ty: &Type) -> (Span, String) {
        let ty = match ty {
            Type::Generic(head, _) if self.open_type_arguments.contains(&span) => {
                Type::Generic(*head, Vec::new())
            }
            ty => ty.clone(),
        };
        (span, format!("builtin argument {}", shown(&ty)))
    }

    /// Check `subject`, the type of an argument of a builtin, as `check`
    /// says, for a use at `origin`. The function or method that passes
    /// the type on to the builtin, `passer`, is named in a note, with
    /// the ones it passes it `through`; the
    /// module of the definition that asks it, `owner`, encodes and
    /// decodes its own opaque types (`opaque_encode_refusal`).
    pub(super) fn check_builtin_argument(
        &mut self,
        check: BuiltinCheck,
        subject: &Type,
        (passer, through): (Option<Symbol>, &[Symbol]),
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
                self.need_paths.entry(key).or_default().push(Vec::new());
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
            // A decoded list, option, map or tuple, or a generic type at
            // its arguments: what a type variable in it stands for is
            // decoded too.
            if let BuiltinCheck::Decodes {
                builtin, decoding, ..
            } = check
                && !decoded_variables(&ty).is_empty()
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
        if !self.lacking.insert(self.refused_argument(span, &ty)) {
            return;
        }
        if let Some(passer) = passer {
            diagnostic = diagnostic.with_note(passes_note(passer, through, check.builtin()));
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

    /// Whether `ty` mentions a variable of `open_vars`, also through what
    /// the checker has learnt of its variables.
    pub(super) fn holds_open_vars(&self, ty: &Type) -> bool {
        free_vars_in(ty).into_iter().any(|v| {
            self.open_vars.contains(&v)
                || matches!(self.tables.vars.subst.get(v), Some(Some(resolved))
                    if self.holds_open_vars(resolved))
        })
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
        // A generic type's arguments (`Box(Int)`; none for `Box`, also
        // where what it was given for them was fixed later).
        let args: &[Type] = match ty {
            Type::Generic(_, args) if !self.open_type_arguments.contains(&span) => args,
            _ => &[],
        };
        // (A type of another module with its module, `time.Date`; an
        // alias by its name; an instance of a generic type with its
        // arguments.)
        let type_name = match written {
            Some(alias) => resolve(alias),
            None if !args.is_empty() && free_vars_in(ty).is_empty() => self.show_type(ty),
            None => self.show_type(&Type::Generic(head, Vec::new())),
        };
        let refusal = |code, message: String, help: &str| {
            Some(Diagnostic::error(code, span, message).with_help(help))
        };
        if crate::defs::builtin_types()
            .get(head.id.0.0 as usize)
            .is_none()
        {
            // A generic type whose arguments are not all known (`Box`, of
            // whatever `a` is): a decoder builds a value of one type, by
            // its structure or by its impl. (Before the enum's help below,
            // which names an impl.)
            let open =
                self.tables.is_generic(head) && (args.is_empty() || !free_vars_in(ty).is_empty());
            if self.has_decode_impl(head) && !open {
                return None;
            }
            if let Some(refusal) = self.opaque_decode_refusal(ty, span, owner) {
                return Some(refusal);
            }
            let enum_type = self.tables.is_enum(head);
            if open {
                let kind = match enum_type {
                    true => "enum",
                    false => "record",
                };
                let message = format!(
                    "`{builtin_name}` cannot decode `{type_name}`: it is a generic {kind} \
                     type, and a decoder builds a value of one type"
                );
                return refusal(Code::Undecodable, message, generic_help);
            }
            // (A row is decoded into a record of its columns, or by the
            // type's own impl, which the program may write for its enum;
            // not for a generic one.)
            if enum_type && !family.decodes_enums() {
                let help = match self.tables.is_generic(head) {
                    true => generic_help.to_string(),
                    false => format!(
                        "declare a record type, or write a `json.Decode` impl for `{}`",
                        self.written_type(head)
                    ),
                };
                return refusal(
                    Code::Undecodable,
                    format!(
                        "`{builtin_name}` cannot decode `{type_name}`: it is an enum type, and enums have no decoder"
                    ),
                    &help,
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
            let found = self.undecodable_field(head, args, family, &mut Vec::new())?;
            let (place, has, kind) = match found.in_variant {
                true => ("variant", "a field of type", "an enum"),
                false => ("field", "type", "a record"),
            };
            let owner = if found.nested {
                format!(" of `{}`, {kind} type nested in it,", found.record)
            } else {
                String::new()
            };
            let part = if found.grows {
                "which grows with every level".to_string()
            } else if found.part == found.field_type {
                "which has no decoder".to_string()
            } else {
                format!("and `{}` has no decoder", found.part)
            };
            let part = match found.map_key {
                true => format!("{part}, as {MAP_KEY}"),
                false => part,
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
            let type_name = match written.is_none()
                && free_vars_in(ty).is_empty()
                && !self.open_type_arguments.contains(&span)
            {
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
            let why = match decoding.primitives && map_with_other_keys(ty) {
                true => MAP_KEY.to_string(),
                false => format!("its type argument must be {accepted}"),
            };
            return refusal(
                Code::Undecodable,
                format!("`{builtin_name}` cannot decode `{type_name}`: {why}"),
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
                            "`{builtin}` cannot decode `{}`: `{}` has no decoder{}",
                            self.show_type(whole),
                            self.show_type(part),
                            match map_with_other_keys(part) {
                                true => format!(", as {MAP_KEY}"),
                                false => String::new(),
                            }
                        ),
                    )
                    .with_help(help),
                )
            }
        }
    }

    /// The first field that `family` cannot decode, in the record or
    /// enum type `record` at its type arguments `args` (none for a type
    /// without parameters) or in a record or enum type nested in it
    /// (which `family.nested()` decodes). `None` if there is none. A
    /// type variable in a field is what a use gives (it owes the check
    /// of what it stands for). A nested type with a written
    /// `json.Decode` impl is the impl's to decode: it is not looked
    /// into, and an enum type with one is decoded where a family
    /// decodes no enum (a postgres row's field).
    fn undecodable_field(
        &self,
        record: TypeRef,
        args: &[Type],
        family: Family,
        seen: &mut Vec<(TypeId, Vec<Type>, Family)>,
    ) -> Option<UndecodableField> {
        let instance = (record.id, args.to_vec(), family);
        if seen.contains(&instance) {
            return None;
        }
        seen.push(instance);
        let in_variant = self.tables.is_enum(record);
        let fields = match in_variant {
            // (Each field of each variant, by the variant's name.)
            true => (self.tables.decoded_variant_fields(record, args).into_iter())
                .flat_map(|(name, types)| types.into_iter().map(move |ty| (name, ty)))
                .collect(),
            false => self.tables.decoded_record_fields(record, args),
        };
        // (And as the type declares them, over its parameters.)
        let declared: Vec<Type> = match in_variant {
            true => (self.tables.decoded_variant_fields(record, &[]).into_iter())
                .flat_map(|(_, types)| types)
                .collect(),
            false => (self.tables.decoded_record_fields(record, &[]).into_iter())
                .map(|(_, ty)| ty)
                .collect(),
        };
        // (A generic type by its arguments: `Box(Set(Int))`.)
        let record_shown = match args.is_empty() {
            true => resolve(record.name),
            false => self.show_type(&Type::Generic(record, args.to_vec())),
        };
        for ((field, ty), declared) in fields.into_iter().zip(declared) {
            // A type that is unknown where the field writes it is that
            // error alone.
            if ty.holds_error() {
                continue;
            }
            let mut nested = Vec::new();
            let mut map_key = false;
            let variables = type_variables(&ty);
            let lacking = match self.tables.decoded_field_type(&ty, &variables, &mut nested) {
                Err(part) => {
                    map_key = map_with_other_keys(&part);
                    Some(self.show_type(&part))
                }
                Ok(field_type) => family.lacks(&field_type).map(str::to_string).or_else(|| {
                    let (enum_type, _) = (nested.iter())
                        .find(|(t, _)| self.tables.is_enum(*t) && !self.has_decode_impl(*t))?;
                    (!family.decodes_enums()).then(|| match self.other_module(*enum_type) {
                        Some(module) => format!("{module}.{}", enum_type.name),
                        None => resolve(enum_type.name),
                    })
                }),
            };
            if let Some(part) = lacking {
                return Some(UndecodableField {
                    record: record_shown,
                    field: resolve(field),
                    in_variant,
                    field_type: self.show_type(&ty),
                    part,
                    map_key,
                    nested: false,
                    grows: false,
                });
            }
            for (nested_record, nested_args) in nested {
                let family = family.nested();
                if self.has_decode_impl(nested_record) {
                    continue;
                }
                // This field gives a type ever deeper arguments: the type
                // that declares it, the field as it declares it.
                if super::decodable::too_deep(&nested_args) {
                    return Some(UndecodableField {
                        record: resolve(record.name),
                        field: resolve(field),
                        in_variant,
                        field_type: self.show_type(
                            &self.with_names(&declared, &self.tables.named_type_params(record)),
                        ),
                        part: String::new(),
                        map_key: false,
                        nested: false,
                        grows: true,
                    });
                }
                if let Some(found) =
                    self.undecodable_field(nested_record, &nested_args, family, seen)
                {
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

/// The type variables in `ty`, at any depth: those a decoded field
/// type holds where a use of a generic type gives them (`Box(a)`).
fn type_variables(ty: &Type) -> Vec<Type> {
    fn walk(ty: &Type, out: &mut Vec<Type>) {
        match ty {
            Type::Var(_) | Type::Rigid(_) if !out.contains(ty) => out.push(ty.clone()),
            Type::Generic(_, ts) | Type::Tuple(ts) => ts.iter().for_each(|t| walk(t, out)),
            Type::List(t) | Type::Set(t) | Type::Channel(t) => walk(t, out),
            Type::Map(k, v) => {
                walk(k, out);
                walk(v, out);
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(ty, &mut out);
    out
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

/// Whether `t` is a builtin type, no type of the program.
fn is_builtin_type(t: TypeRef) -> bool {
    crate::defs::builtin_types()
        .get(t.id.0.0 as usize)
        .is_some()
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
/// String keys or a tuple `ty` holds, or a generic record or enum type
/// of the program is given (`Box(a)`), at any depth through these.
pub(super) fn decoded_variables(ty: &Type) -> Vec<Type> {
    let parts = match ty {
        Type::Generic(t, args) if !args.is_empty() && !is_builtin_type(*t) => args.iter().collect(),
        _ => decoded_parts(ty).unwrap_or_default(),
    };
    let mut out = Vec::new();
    for part in parts {
        match part {
            Type::Var(_) | Type::Rigid(_) => out.push(part.clone()),
            _ => out.extend(decoded_variables(part)),
        }
    }
    out
}
