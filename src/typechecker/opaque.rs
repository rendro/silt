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

/// `ty` of `module`, as the opaque errors name it: `mail.User`.
pub(super) fn opaque_name(ty: TypeRef, module: Symbol) -> std::string::String {
    format!("{module}.{}", ty.name)
}

/// The help of every opaque error.
pub(super) fn opaque_help(module: Symbol) -> std::string::String {
    format!("use the functions of module '{module}'")
}
