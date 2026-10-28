//! The types of the fields of record and enum types as the decoders see
//! them (`json.parse(text, T)`, `toml.parse(text, T)`,
//! `postgres.query(db, sql, params, T)`), read from the checker's
//! tables: the one description that the compiler's descriptors of the
//! types (which the decoders decode by at run time) and the check of a
//! decoder's type argument are made from.

use super::*;
use crate::typeinfo::FieldType;
use crate::types::canonical::canonicalize;

/// How deep the type arguments of an instance of a generic type that a
/// walk through the fields of a decoded type meets may be: past it, the
/// type grows with every level (`n: Option(T(List(a)))` in `type T(a)`
/// gives `T` ever deeper arguments), and has no end of instances.
pub(crate) const ARGUMENT_DEPTH_LIMIT: usize = 32;

/// Whether the type arguments `args` are deeper than
/// [`ARGUMENT_DEPTH_LIMIT`].
pub(crate) fn too_deep(args: &[Type]) -> bool {
    fn depth(ty: &Type) -> usize {
        1 + match ty {
            Type::Generic(_, ts) | Type::Tuple(ts) => ts.iter().map(depth).max().unwrap_or(0),
            Type::List(t) | Type::Set(t) | Type::Channel(t) => depth(t),
            Type::Map(k, v) => depth(k).max(depth(v)),
            _ => 0,
        }
    }
    args.iter().any(|arg| depth(arg) > ARGUMENT_DEPTH_LIMIT)
}

/// A type as messages write it, a record type by its name.
pub(crate) fn shown(ty: &Type) -> String {
    Type::show_all(&[ty], |_, _| None).remove(0)
}

impl Tables {
    /// The type parameters of the record or enum type `t`, as the
    /// variables the types of its fields name; none for a type without.
    pub(crate) fn type_params(&self, t: TypeRef) -> Vec<TyVar> {
        match self.enums.get(&t) {
            Some(info) => info.param_var_ids.clone(),
            None => (self.record_param_var_ids.get(&t).cloned()).unwrap_or_default(),
        }
    }

    /// The type parameters of the record or enum type `t`, each with its
    /// name.
    pub(crate) fn named_type_params(&self, t: TypeRef) -> Vec<(TyVar, Symbol)> {
        let names = match self.enums.get(&t) {
            Some(info) => info.params.clone(),
            None => self
                .records
                .get(&t)
                .map_or_else(Vec::new, |info| info.params.clone()),
        };
        self.type_params(t).into_iter().zip(names).collect()
    }

    /// The type `ty`, a field's as its type declares it, with each of
    /// the type's parameters the argument at its place in `args` (none:
    /// as declared).
    fn at_args(&self, t: TypeRef, args: &[Type], ty: &Type) -> Type {
        let ty = canonicalize(self.resolver(), ty);
        if args.is_empty() {
            return ty;
        }
        let mapping = (self.type_params(t).into_iter()).zip(args.iter().cloned());
        crate::types::substitute_vars(&ty, &mapping.collect())
    }

    /// The fields of the record type `ty` at its type arguments `args`
    /// (none: as it declares them), with their types as the checker
    /// inferred them (an alias is the type it stands for).
    pub(crate) fn decoded_record_fields(&self, ty: TypeRef, args: &[Type]) -> Vec<(Symbol, Type)> {
        (self.record_fields_of(ty).iter())
            .map(|(name, field)| (*name, self.at_args(ty, args, field)))
            .collect()
    }

    /// The variants of the enum type `ty` at its type arguments `args`
    /// (none: as it declares them), each with the types of its fields as
    /// the checker inferred them (an alias is the type it stands for).
    pub(crate) fn decoded_variant_fields(
        &self,
        ty: TypeRef,
        args: &[Type],
    ) -> Vec<(Symbol, Vec<Type>)> {
        (self.variant_fields_of(ty).into_iter())
            .map(|(name, types)| {
                let at_args = |field| self.at_args(ty, args, field);
                (name, types.iter().map(at_args).collect())
            })
            .collect()
    }

    /// The type `ty` of a record field as the decoders see it, each type
    /// variable among `params` the parameter at its place (a generic
    /// type's field, `v: a` of `type Box(a)`).
    ///
    /// `Err` carries the part of `ty` no decoder exists for. The record
    /// and enum types the field type refers to are added to `records`,
    /// each with the type arguments it is given (`Box(Int)`: `[Int]`).
    pub(crate) fn decoded_field_type(
        &self,
        ty: &Type,
        params: &[Type],
        records: &mut Vec<(TypeRef, Vec<Type>)>,
    ) -> Result<FieldType, Type> {
        use crate::typeinfo::ty as bt;
        let mut part = |ty: &Type| self.decoded_field_type(ty, params, records);
        match ty {
            Type::Int => Ok(FieldType::Int),
            Type::Float => Ok(FieldType::Float),
            Type::String => Ok(FieldType::String),
            Type::Bool => Ok(FieldType::Bool),
            Type::Var(_) | Type::Rigid(_) => match params.iter().position(|p| p == ty) {
                Some(at) => Ok(FieldType::Param(at as u16)),
                None => Err(ty.clone()),
            },
            Type::List(elem) => Ok(FieldType::List(Box::new(part(elem)?))),
            // The keys of a JSON object or a TOML table are strings.
            Type::Map(key, value) if matches!(**key, Type::String) => {
                Ok(FieldType::Map(Box::new(part(value)?)))
            }
            Type::Tuple(elems) if !elems.is_empty() => {
                let parts: Result<Vec<_>, _> = elems.iter().map(part).collect();
                Ok(FieldType::Tuple(parts?))
            }
            Type::Generic(t, args) if t.id == bt::OPTION && args.len() == 1 => {
                Ok(FieldType::Option(Box::new(part(&args[0])?)))
            }
            Type::Generic(t, args) if args.is_empty() => self.decoded_named_type(*t, ty, records),
            // A generic record or enum type of the program at its type
            // arguments.
            Type::Generic(t, args)
                if crate::defs::builtin_types()
                    .get(t.id.0.0 as usize)
                    .is_none()
                    && self.is_generic(*t) =>
            {
                let parts: Result<Vec<_>, _> = args.iter().map(part).collect();
                let parts = parts?;
                records.push((*t, args.clone()));
                Ok(FieldType::Applied(t.id, parts))
            }
            // Set, Channel, functions, ...
            _ => Err(ty.clone()),
        }
    }

    /// A named type with no arguments as the decoders see it: Date, Time,
    /// DateTime, json.Value, Bytes, or a non-generic record or enum type
    /// of the program.
    fn decoded_named_type(
        &self,
        t: TypeRef,
        ty: &Type,
        records: &mut Vec<(TypeRef, Vec<Type>)>,
    ) -> Result<FieldType, Type> {
        use crate::typeinfo::ty as bt;
        match t.id {
            id if id == bt::DATE => Ok(FieldType::Date),
            id if id == bt::TIME => Ok(FieldType::Time),
            id if id == bt::DATE_TIME => Ok(FieldType::DateTime),
            id if id == bt::JSON_VALUE => Ok(FieldType::JsonValue),
            id if id == TypeRef::builtin("Bytes").id => Ok(FieldType::Bytes),
            // A non-generic record or enum type of the program.
            id if crate::defs::builtin_types().get(id.0.0 as usize).is_none()
                && self.declares_non_generic(t) =>
            {
                records.push((t, Vec::new()));
                match self.is_enum(t) {
                    true => Ok(FieldType::Enum(id)),
                    false => Ok(FieldType::Record(id)),
                }
            }
            // Builtin enums, the other builtin types.
            _ => Err(ty.clone()),
        }
    }

    /// Whether the program declares the type `t` (a record or an enum
    /// type) without type parameters.
    fn declares_non_generic(&self, t: TypeRef) -> bool {
        (self.records.get(&t).is_some() || self.enums.get(&t).is_some()) && !self.is_generic(t)
    }

    /// Whether the program declares the record or enum type `t` with
    /// type parameters.
    pub(super) fn is_generic(&self, t: TypeRef) -> bool {
        self.record_param_var_ids.contains_key(&t)
            || self
                .enums
                .get(&t)
                .is_some_and(|info| !info.params.is_empty())
    }

    /// Whether `t` is an enum type: one with variants.
    pub(super) fn is_enum(&self, t: TypeRef) -> bool {
        self.enums
            .get(&t)
            .is_some_and(|info| !info.variants.is_empty())
    }
}
