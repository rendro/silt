//! Opaque types (`pub opaque type T { .. }`): another module sees the
//! type and none of what is in it. Outside the type's own module, a
//! value of it cannot be built, taken apart, updated, read, spread, or
//! passed into an open row; the module's functions are the only way in
//! or out. Each of those is one error, with one help. "Outside" is the
//! module of the code at hand, the use, whatever module wrote the
//! function the value goes through.

use super::*;
use std::collections::HashSet;

impl TypeChecker {
    /// The module of the opaque type `ty` when the module being checked
    /// is another one: it sees the type, not what is in it.
    pub(super) fn opaque_outside(&self, ty: TypeRef) -> Option<Symbol> {
        self.opaque_outside_of(ty, self.module)
    }

    /// The module of the opaque type `ty` when `viewer` is another one.
    fn opaque_outside_of(&self, ty: TypeRef, viewer: crate::session::ModuleId) -> Option<Symbol> {
        let def = self.def(ty.id.0)?;
        if def.vis != crate::defs::Vis::Opaque || def.module == viewer {
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
    /// use. The rows and type variables of a definition of another
    /// module are that module's: its opaque types pass into them.
    pub(super) fn instantiate_def(
        &mut self,
        scheme: &Scheme,
        res: Option<crate::defs::Res>,
    ) -> Type {
        let module = (self.res_def(res).map(|def| def.module)).filter(|m| *m != self.module);
        let around = std::mem::replace(&mut self.instantiating, module);
        let ty = self.instantiate(scheme);
        self.instantiating = around;
        if let Some(def) = self.res_def(res)
            && def.module != self.module
        {
            let module = def.module;
            let mut vars = free_vars_in(&ty);
            super::row_tail_vars(&ty, &mut vars);
            for var in vars {
                self.use_owners.insert(var, module);
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
        let owner = self.use_owners.get(v);
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
        seen: &mut HashSet<crate::defs::TypeId>,
        viewer: crate::session::ModuleId,
    ) -> Option<(TypeRef, Symbol)> {
        if !seen.insert(head.id) || self.has_decode_impl(head) {
            return None;
        }
        if let Some(module) = self.opaque_outside_of(head, viewer) {
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
                if let Some(found) = self.opaque_decoded(nested, seen, viewer) {
                    return Some(found);
                }
            }
        }
        None
    }

    /// The module of the opaque type of another module that decoding
    /// `ty` would build by its structure, if there is one.
    pub(super) fn opaque_decoded_module(&self, ty: &Type) -> Option<crate::session::ModuleId> {
        let Type::Generic(head, _) = ty else {
            return None;
        };
        let (found, _) = self.opaque_decoded(*head, &mut HashSet::new(), self.module)?;
        self.def(found.id.0).map(|def| def.module)
    }

    /// The opaque type of a module other than `viewer`, without a
    /// `json.Encode` impl of it, that encoding `ty` would read by its
    /// structure: `ty` itself, or a type in its parts at any depth.
    fn opaque_encoded(
        &self,
        ty: &Type,
        seen: &mut HashSet<crate::defs::TypeId>,
        viewer: crate::session::ModuleId,
    ) -> Option<(TypeRef, Symbol)> {
        let encode = TraitKey::builtin("Encode");
        let ty = crate::types::canonical::canonicalize(&self.tables.resolver, &self.apply(ty));
        match &ty {
            Type::Generic(name, args) => {
                let head = canonical_head(&self.tables.resolver, *name);
                if !self.by_structure(encode, head) || !seen.insert(head.id) {
                    return None;
                }
                if let Some(module) = self.opaque_outside_of(head, viewer) {
                    return Some((head, module));
                }
                let mut parts: Vec<Type> = args.clone();
                if let Some(info) = self.tables.records.get(&head) {
                    parts.extend(info.fields.iter().map(|(_, t)| t.clone()));
                } else if let Some(info) = self.tables.enums.get(&head) {
                    for variant in &info.variants {
                        parts.extend(variant.field_types.iter().cloned());
                    }
                }
                parts
                    .iter()
                    .find_map(|part| self.opaque_encoded(part, seen, viewer))
            }
            Type::List(t) | Type::Set(t) | Type::Channel(t) => self.opaque_encoded(t, seen, viewer),
            Type::Map(k, v) => (self.opaque_encoded(k, seen, viewer))
                .or_else(|| self.opaque_encoded(v, seen, viewer)),
            Type::Tuple(parts) => parts
                .iter()
                .find_map(|part| self.opaque_encoded(part, seen, viewer)),
            Type::AnonRecord { fields, .. } => fields
                .values()
                .find_map(|part| self.opaque_encoded(part, seen, viewer)),
            _ => None,
        }
    }

    /// Owe, for each type variable inside `ty` (at any depth, the whole
    /// excepted), that the encoder `builtin` may encode what it stands
    /// for: an annotation variable as a need of its function's uses, any
    /// other one once it is decided or generalised (`BuiltinCheck::Encodes`).
    /// A definition of `owner` asks it, if one does.
    pub(super) fn encode_variables_inside(
        &mut self,
        builtin: Symbol,
        ty: &Type,
        passer: Option<Symbol>,
        owner: Option<crate::session::ModuleId>,
        origin: super::solve::Origin,
    ) {
        fn walk(ty: &Type, top: bool, out: &mut Vec<Type>) {
            match ty {
                Type::Var(_) | Type::Rigid(_) if !top => out.push(ty.clone()),
                Type::Generic(_, args) | Type::Tuple(args) => {
                    args.iter().for_each(|t| walk(t, false, out))
                }
                Type::List(t) | Type::Set(t) | Type::Channel(t) => walk(t, false, out),
                Type::Map(k, v) => {
                    walk(k, false, out);
                    walk(v, false, out);
                }
                Type::AnonRecord { fields, tail } => {
                    fields.values().for_each(|t| walk(t, false, out));
                    match tail {
                        RowTail::Var(v) => out.push(Type::Var(*v)),
                        RowTail::Rigid(r) => out.push(Type::Rigid(*r)),
                        RowTail::Closed => {}
                    }
                }
                _ => {}
            }
        }
        let mut inside = Vec::new();
        walk(ty, true, &mut inside);
        for var in inside {
            let check = crate::types::BuiltinCheck::Encodes { builtin };
            match var {
                Type::Rigid(_) => self.check_builtin_argument(check, &var, passer, None, origin),
                _ => {
                    let pred = Pred::Builtin {
                        check,
                        subject: var,
                        passer,
                    };
                    let around = std::mem::replace(&mut self.instantiating, owner);
                    let _ = self.want(pred, origin);
                    self.instantiating = around;
                }
            }
        }
    }

    /// The error of encoding `ty` at `span` outside the module of an
    /// opaque type it would read by its structure, if it would read one:
    /// encoding reads what is in it. A definition of the type's own
    /// module (`owner`, the module of the definition that asks it)
    /// encodes it wherever it is used: what is in the type is judged as
    /// that module sees it.
    pub(super) fn opaque_encode_refusal(
        &self,
        ty: &Type,
        span: Span,
        owner: Option<crate::session::ModuleId>,
    ) -> Option<Diagnostic> {
        let mut found = self.opaque_encoded(ty, &mut HashSet::new(), self.module)?;
        if let Some(owner) = owner
            && self
                .def(found.0.id.0)
                .is_some_and(|def| def.module == owner)
        {
            found = self.opaque_encoded(ty, &mut HashSet::new(), owner)?;
        }
        let (found, module) = found;
        Some(
            Diagnostic::error(
                Code::Undecodable,
                span,
                format!(
                    "type '{}' cannot be encoded outside module '{module}': it is opaque, and \
                     '{module}' writes no 'json.Encode' impl for it",
                    opaque_name(found, module)
                ),
            )
            .with_help(format!(
                "encode it inside module '{module}', or write `trait json.Encode for {}` there",
                found.name
            )),
        )
    }

    /// The refusal of decoding `head` at `span` outside the module of an
    /// opaque type it would build by its structure, if it would build
    /// one: a decoder builds what a constructor does. A definition of the
    /// type's own module (`owner`) decodes it wherever it is used, as
    /// `opaque_encode_refusal` encodes.
    pub(super) fn opaque_decode_refusal(
        &self,
        head: TypeRef,
        span: Span,
        owner: Option<crate::session::ModuleId>,
    ) -> Option<Diagnostic> {
        let mut found = self.opaque_decoded(head, &mut HashSet::new(), self.module)?;
        if let Some(owner) = owner
            && self
                .def(found.0.id.0)
                .is_some_and(|def| def.module == owner)
        {
            found = self.opaque_decoded(head, &mut HashSet::new(), owner)?;
        }
        let (ty, module) = found;
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
