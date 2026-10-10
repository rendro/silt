//! The variant value.
//!
//! A variant with fields points to an object that holds its type and
//! its fields, in the order its declaration gives them; a variant
//! without fields (`None`, `Red`) points to its type alone, the one
//! object all such variants of the type share, and nothing is made for
//! it. A clone is a count: of the object, or of the type.
//!
//! How a variant is stored is this file's own. The rest of silt has
//! which variant it is ([`Variant::is`], [`Variant::name`],
//! [`Variant::ordinal`], [`Variant::ty`]) and its fields as a slice
//! ([`Variant::fields`]).

use std::sync::Arc;

use super::Value;
use crate::defs::TypeId;
use crate::typeinfo::{BuiltinVariant, Tag, TypeInfo};

/// What a variant with fields points to.
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
