//! The variant value and the record value.
//!
//! Each points to an object that holds its type and its fields: a
//! variant's in the order its declaration gives them, a record's in the
//! order its type declares them (an anonymous record's in name order).
//! A variant without fields (`None`, `Red`) points to its type alone,
//! the one object all such variants of the type share, and nothing is
//! made for it. A clone is a count: of the object, or of the type.
//!
//! How the two are stored is this file's own. The rest of silt has
//! which variant a variant is ([`Variant::is`], [`Variant::name`],
//! [`Variant::ordinal`], [`Variant::ty`]) and its fields as a slice
//! ([`Variant::fields`]); a record's type ([`Record::ty`]), its fields
//! as a slice in the type's order ([`Record::fields`]), one of them by
//! its name ([`Record::get`]) and all with their names
//! ([`Record::named`]).

use std::sync::Arc;

use super::Value;
use crate::defs::TypeId;
use crate::typeinfo::{BuiltinVariant, Tag, TypeInfo, anon_record_type};

/// What a variant with fields, or a record, points to.
#[derive(Clone)]
struct Obj {
    ty: Arc<TypeInfo>,
    /// In the order the type declares them.
    fields: Box<[Value]>,
}

/// A variant: `None`, `Some(1)`, `Link(1, rest)`.
#[derive(Clone)]
pub struct Variant {
    of: Of,
    /// The position of the variant's declaration in its type.
    ordinal: u16,
}

#[derive(Clone)]
enum Of {
    /// No fields: the type.
    Type(Arc<TypeInfo>),
    /// The type and the fields, of which there is one at least.
    Obj(Arc<Obj>),
}

impl Variant {
    /// The variant `tag` with the fields `fields` ([`Value::variant`]).
    pub(super) fn new(tag: Tag, fields: Vec<Value>) -> Variant {
        debug_assert_eq!(tag.arity(), fields.len(), "the fields of {tag}");
        let ordinal = tag.ordinal();
        let ty = tag.into_ty();
        let of = match fields.is_empty() {
            true => Of::Type(ty),
            false => Of::Obj(Arc::new(Obj {
                ty,
                fields: fields.into_boxed_slice(),
            })),
        };
        Variant { of, ordinal }
    }

    /// The type the variant is of.
    pub fn ty(&self) -> &Arc<TypeInfo> {
        match &self.of {
            Of::Type(ty) => ty,
            Of::Obj(obj) => &obj.ty,
        }
    }

    pub fn type_id(&self) -> TypeId {
        self.ty().id
    }

    /// The position of the variant's declaration in its type.
    pub fn ordinal(&self) -> u16 {
        self.ordinal
    }

    /// Which variant of which type this is.
    pub fn tag(&self) -> Tag {
        Tag::new(self.ty().clone(), self.ordinal)
    }

    pub fn name(&self) -> &str {
        &self.ty().variants()[self.ordinal as usize].name
    }

    /// Whether this is the builtin variant `variant`.
    pub fn is(&self, variant: BuiltinVariant) -> bool {
        self.ordinal == variant.ordinal && self.ty().id == variant.ty
    }

    /// Whether this is a variant of the type `ty`.
    pub fn of(&self, ty: TypeId) -> bool {
        self.ty().id == ty
    }

    /// Whether this is the variant `tag`.
    pub fn has_tag(&self, tag: &Tag) -> bool {
        self.ordinal == tag.ordinal() && self.ty().id == tag.type_id()
    }

    /// The fields, in the order the variant's declaration gives them.
    pub fn fields(&self) -> &[Value] {
        match &self.of {
            Of::Type(_) => &[],
            Of::Obj(obj) => &obj.fields,
        }
    }
}

/// Whether `name` is one of `names`.
fn is_among<'a>(mut names: impl Iterator<Item = &'a str>, name: &str) -> bool {
    names.any(|given| given == name)
}

/// A record: `Pt { x: 1, y: 2 }`, `{name: "a"}`.
#[derive(Clone)]
pub struct Record(Arc<Obj>);

impl Record {
    /// The record of the type `ty` with the fields `fields`, in the
    /// order the type declares them ([`Value::record`]).
    pub(super) fn new(ty: Arc<TypeInfo>, fields: Vec<Value>) -> Record {
        debug_assert_eq!(ty.fields().len(), fields.len(), "the fields of {}", ty.name);
        Record(Arc::new(Obj {
            ty,
            fields: fields.into_boxed_slice(),
        }))
    }

    /// The anonymous record of the fields `fields`, whose names are
    /// distinct ([`Value::anon_record`]).
    pub(super) fn anon(mut fields: Vec<(&str, Value)>) -> Record {
        fields.sort_by_key(|(name, _)| *name);
        let ty = anon_record_type(fields.iter().map(|(name, _)| *name));
        Record::new(ty, fields.into_iter().map(|(_, value)| value).collect())
    }

    /// The record of the type `ty` a literal writes: `values` are the
    /// fields `names`, in the order the literal gives them. `None` if
    /// they are not the type's fields, each once.
    pub fn written<'a>(
        ty: Arc<TypeInfo>,
        names: impl Iterator<Item = &'a str> + Clone,
        mut values: Vec<Value>,
    ) -> Option<Record> {
        let declared = ty.fields();
        if declared.len() != values.len() {
            return None;
        }
        // Written in the type's order: the values are the fields.
        if declared.iter().map(|(name, _)| &**name).eq(names.clone()) {
            return Some(Record::new(ty, values));
        }
        let mut fields = Vec::with_capacity(values.len());
        for (name, _) in declared {
            let written = names.clone().position(|given| given == name)?;
            fields.push(std::mem::replace(&mut values[written], Value::Unit));
        }
        Some(Record::new(ty, fields))
    }

    /// The type the record is of.
    pub fn ty(&self) -> &Arc<TypeInfo> {
        &self.0.ty
    }

    pub fn type_id(&self) -> TypeId {
        self.0.ty.id
    }

    /// The fields, in the order the record's type declares them.
    pub fn fields(&self) -> &[Value] {
        &self.0.fields
    }

    /// The field `name`.
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.0.fields.get(self.0.ty.field_index(name)?)
    }

    /// The fields with their names, in the order the record's type
    /// declares them.
    pub fn named(&self) -> impl ExactSizeIterator<Item = (&str, &Value)> + Clone {
        let names = self.0.ty.fields().iter().map(|(name, _)| &**name);
        names.zip(self.0.fields.iter())
    }

    /// The fields with their names, in name order: the order of an
    /// anonymous record. A builtin record is shown in it
    /// (src/value/fmt.rs).
    pub(super) fn by_name(&self) -> Vec<(&str, &Value)> {
        let mut fields: Vec<(&str, &Value)> = self.named().collect();
        fields.sort_by_key(|(name, _)| *name);
        fields
    }

    /// The record with `values` as its fields `names`: `r.{ x: 1 }`,
    /// and the fields written after a spread, `{...r, x: 1}`. A name
    /// that is a field of the record replaces it, and the record keeps
    /// its type; a name that is none is added to an anonymous record,
    /// which is then one of more fields. `None` for a name that is no
    /// field of a declared record.
    pub fn updated<'a>(
        mut self,
        names: impl Iterator<Item = &'a str> + Clone,
        values: Vec<Value>,
    ) -> Option<Record> {
        let ty = &self.0.ty;
        let replaces = names.clone().all(|name| ty.field_index(name).is_some());
        if replaces {
            // (The one holder of a record updates it where it is.)
            let obj = Arc::make_mut(&mut self.0);
            for (name, value) in names.zip(values) {
                let index = obj.ty.field_index(name)?;
                obj.fields[index] = value;
            }
            return Some(self);
        }
        if !self.0.ty.is_anon() {
            return None;
        }
        let mut fields: Vec<(&str, Value)> = self
            .named()
            .filter(|(name, _)| !is_among(names.clone(), name))
            .map(|(name, value)| (name, value.clone()))
            .collect();
        for (name, value) in names.zip(values) {
            fields.push((name, value));
        }
        Some(Record::anon(fields))
    }

    /// The anonymous record of the fields but those named `excluded`:
    /// what a rest pattern binds, `{x, ...rest}`, and with none
    /// excluded what a spread starts from, `{...r}`.
    pub fn rest<'a>(&self, excluded: impl Iterator<Item = &'a str> + Clone) -> Record {
        let kept = |name: &str| !is_among(excluded.clone(), name);
        if self.0.ty.is_anon() && self.named().all(|(name, _)| kept(name)) {
            return self.clone();
        }
        Record::anon(
            self.named()
                .filter(|(name, _)| kept(name))
                .map(|(name, value)| (name, value.clone()))
                .collect(),
        )
    }
}
