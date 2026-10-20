//! The types of the fields of record and enum types as the decoders see
//! them (`json.parse(text, T)`, `toml.parse(text, T)`,
//! `postgres.query(db, sql, params, T)`), read from the checker's
//! tables: the one description that the compiler's descriptors of the
//! types (which the decoders decode by at run time) and the check of a
//! decoder's type argument are made from.

use super::*;
use crate::typeinfo::FieldType;
use crate::types::canonical::canonicalize;

/// A type as messages write it, a record type by its name.
pub(crate) fn shown(ty: &Type) -> String {
    Type::show_all(&[ty], |_, _| None).remove(0)
}

impl Tables {
    /// The fields of the record type `ty`, with their types as the
    /// checker inferred them (an alias is the type it stands for).
    pub(crate) fn decoded_record_fields(&self, ty: TypeRef) -> Vec<(Symbol, Type)> {
        (self.record_fields_of(ty).iter())
            .map(|(name, ty)| (*name, canonicalize(self.resolver(), ty)))
            .collect()
    }

    /// The variants of the enum type `ty`, each with the types of its
    /// fields as the checker inferred them (an alias is the type it
    /// stands for).
    pub(crate) fn decoded_variant_fields(&self, ty: TypeRef) -> Vec<(Symbol, Vec<Type>)> {
        (self.variant_fields_of(ty).into_iter())
            .map(|(name, types)| {
                let canonical = |ty| canonicalize(self.resolver(), ty);
                (name, types.iter().map(canonical).collect())
            })
            .collect()
    }

    /// The type `ty` of a record field as the decoders see it.
    ///
    /// `Err` carries the part of `ty` no decoder exists for. The record
    /// and enum types the field type refers to are added to `records`.
    pub(crate) fn decoded_field_type(
        &self,
        ty: &Type,
        records: &mut Vec<crate::defs::TypeId>,
    ) -> Result<FieldType, String> {
        use crate::typeinfo::ty as bt;
        match ty {
            Type::Int => Ok(FieldType::Int),
            Type::Float => Ok(FieldType::Float),
            Type::String => Ok(FieldType::String),
            Type::Bool => Ok(FieldType::Bool),
            Type::List(elem) => Ok(FieldType::List(Box::new(
                self.decoded_field_type(elem, records)?,
            ))),
            // The keys of a JSON object or a TOML table are strings.
            Type::Map(key, value) if matches!(**key, Type::String) => Ok(FieldType::Map(Box::new(
                self.decoded_field_type(value, records)?,
            ))),
            Type::Tuple(elems) if !elems.is_empty() => {
                let mut parts = Vec::with_capacity(elems.len());
                for elem in elems {
                    parts.push(self.decoded_field_type(elem, records)?);
                }
                Ok(FieldType::Tuple(parts))
            }
            Type::Generic(t, args) if t.id == bt::OPTION && args.len() == 1 => Ok(
                FieldType::Option(Box::new(self.decoded_field_type(&args[0], records)?)),
            ),
            Type::Generic(t, args) if args.is_empty() => self.decoded_named_type(*t, ty, records),
            // Set, Channel, functions, generic records, ...
            _ => Err(shown(ty)),
        }
    }

    /// A named type with no arguments as the decoders see it: Date, Time,
    /// DateTime, json.Value, Bytes, or a non-generic record or enum type
    /// of the program.
    fn decoded_named_type(
        &self,
        t: TypeRef,
        ty: &Type,
        records: &mut Vec<crate::defs::TypeId>,
    ) -> Result<FieldType, String> {
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
                records.push(id);
                match self.is_enum(t) {
                    true => Ok(FieldType::Enum(id)),
                    false => Ok(FieldType::Record(id)),
                }
            }
            // Builtin enums, the other builtin types.
            _ => Err(shown(ty)),
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
