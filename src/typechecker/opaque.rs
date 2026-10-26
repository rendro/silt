//! Opaque types (`pub opaque type T { .. }`): another module sees the
//! type and none of what is in it. Outside the type's own module, a
//! value of it cannot be built, taken apart, updated, read, spread, or
//! passed into an open row; the module's functions are the only way in
//! or out. Each of those is one error, with one help. "Outside" is the
//! module of the code at hand, the use, whatever module wrote the
//! function the value goes through.

use super::*;

impl TypeChecker {
    /// The module of the opaque type `ty` when the module being checked
    /// is another one: it sees the type, not what is in it.
    pub(super) fn opaque_outside(&self, ty: TypeRef) -> Option<Symbol> {
        let def = self.def(ty.id.0)?;
        if def.vis != crate::defs::Vis::Opaque || def.module == self.module {
            return None;
        }
        self.tables.module_names.get(&def.module).copied()
    }

    /// `ty`, as far as it is known, when it is an opaque type of another
    /// module: the type and its module.
    pub(super) fn opaque_value(&self, ty: &Type) -> Option<(TypeRef, Symbol)> {
        let ty = crate::types::canonical::canonicalize(&self.tables.resolver, &self.apply(ty));
        match ty {
            Type::Generic(r, _) => self.opaque_outside(r).map(|module| (r, module)),
            _ => None,
        }
    }

    /// `scheme`, the type of the definition `res`, instantiated for a
    /// use. The rows of a definition of another module are that
    /// module's: its opaque types pass into them.
    pub(super) fn instantiate_def(
        &mut self,
        scheme: &Scheme,
        res: Option<crate::defs::Res>,
    ) -> Type {
        let ty = self.instantiate(scheme);
        if let Some(def) = self.res_def(res)
            && def.module != self.module
        {
            let module = def.module;
            let mut rows = Vec::new();
            super::row_tail_vars(&ty, &mut rows);
            for row in rows {
                self.row_owners.insert(row, module);
            }
        }
        ty
    }

    /// Whether a value of the opaque type `ty` of another module may pass
    /// into the row whose tail is `tail`: a row of a definition of the
    /// type's own module.
    pub(super) fn row_of_types_module(&self, ty: TypeRef, tail: &RowTail) -> bool {
        let RowTail::Var(v) = tail else {
            return false;
        };
        let owner = self.row_owners.get(v);
        owner.is_some() && owner.copied() == self.def(ty.id.0).map(|def| def.module)
    }

    /// Report `message`, about a value of an opaque type of `module`.
    pub(super) fn opaque_error(
        &mut self,
        message: std::string::String,
        module: Symbol,
        span: Span,
    ) {
        self.errors.push(
            Diagnostic::error(Code::OpaqueType, span, message).with_help(opaque_help(module)),
        );
    }
}

impl TypeChecker {
    /// The opaque type of another module that decoding `head` would
    /// build by its structure: `head` itself, or a record or enum type
    /// in its fields, at any depth. A type with a written `json.Decode`
    /// impl is the impl's to decode, and is not looked into.
    fn opaque_decoded(
        &self,
        head: TypeRef,
        seen: &mut std::collections::HashSet<crate::defs::TypeId>,
    ) -> Option<(TypeRef, Symbol)> {
        if !seen.insert(head.id) || self.has_decode_impl(head) {
            return None;
        }
        if let Some(module) = self.opaque_outside(head) {
            return Some((head, module));
        }
        let fields: Vec<Type> = match self.tables.is_enum(head) {
            true => (self.tables.decoded_variant_fields(head).into_iter())
                .flat_map(|(_, types)| types)
                .collect(),
            false => (self.tables.decoded_record_fields(head).into_iter())
                .map(|(_, ty)| ty)
                .collect(),
        };
        for ty in fields {
            let mut nested = Vec::new();
            let _ = self.tables.decoded_field_type(&ty, &mut nested);
            for id in nested {
                let Some(def) = self.def(id.0) else {
                    continue;
                };
                let nested = TypeRef { id, name: def.name };
                if let Some(found) = self.opaque_decoded(nested, seen) {
                    return Some(found);
                }
            }
        }
        None
    }

    /// The refusal of decoding `head` at `span` outside the module of an
    /// opaque type it would build by its structure, if it would build
    /// one: a decoder builds what a constructor does.
    pub(super) fn opaque_decode_refusal(&self, head: TypeRef, span: Span) -> Option<Diagnostic> {
        let (ty, module) = self.opaque_decoded(head, &mut std::collections::HashSet::new())?;
        Some(
            Diagnostic::error(
                Code::Undecodable,
                span,
                format!(
                    "type '{}' cannot be decoded outside module '{module}': it is opaque, and \
                     '{module}' writes no 'json.Decode' impl for it",
                    opaque_name(ty, module)
                ),
            )
            .with_help(format!(
                "decode it inside module '{module}', or write `trait json.Decode for {}` there",
                ty.name
            )),
        )
    }
}

/// `ty` of `module`, as the opaque errors name it: `mail.User`.
pub(super) fn opaque_name(ty: TypeRef, module: Symbol) -> std::string::String {
    format!("{module}.{}", ty.name)
}

/// The help of every opaque error.
pub(super) fn opaque_help(module: Symbol) -> std::string::String {
    format!("use the functions of module '{module}'")
}
