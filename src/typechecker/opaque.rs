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
