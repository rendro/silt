use super::*;

/// The error of `Never` written where a program may not write it: it
/// is written only as the whole return type of a function declared with
/// `fn`.
pub(super) const NEVER_ELSEWHERE: &str = "`Never` is written only as the whole return type of a function \
     declared with `fn` (not of a trait's or an impl's method): `fn fail(message: String) -> Never`";

impl TypeChecker {
    // ── Resolved names ──────────────────────────────────────────────

    /// The definition a resolver slot names, if it names one.
    pub(super) fn res_def(&self, res: Option<crate::defs::Res>) -> Option<crate::defs::Def> {
        match res {
            Some(crate::defs::Res::Def(id)) => self.def(id),
            _ => None,
        }
    }

    /// The definition `id`: the session's, or for a checker that has no
    /// program (the builtins), a builtin's.
    pub(super) fn def(&self, id: crate::defs::DefId) -> Option<crate::defs::Def> {
        match &self.defs {
            Some(defs) => Some(*defs.get(id)),
            None => names::builtin_def(id),
        }
    }

    /// The type of a type definition (a record, an enum, an alias). A
    /// builtin type is known by its id alone: the builtin environment,
    /// which the builtin definitions are made from, asks for them.
    pub(super) fn def_type(&self, id: crate::defs::DefId) -> Option<TypeRef> {
        if let Some((name, _)) = crate::defs::builtin_types().get(id.0 as usize) {
            return Some(TypeRef {
                id: crate::defs::TypeId(id),
                name: intern(name),
            });
        }
        let def = self.def(id)?;
        match def.kind {
            crate::defs::DefKind::Type(ty) => Some(TypeRef {
                id: ty,
                name: def.name,
            }),
            crate::defs::DefKind::TypeAlias => Some(TypeRef {
                id: crate::defs::TypeId(id),
                name: def.name,
            }),
            _ => None,
        }
    }

    /// The trait of a trait definition. A builtin trait is known by its
    /// id alone (see `def_type`).
    pub(super) fn def_trait(&self, id: crate::defs::DefId) -> Option<TraitKey> {
        if let Some((name, _)) = crate::defs::builtin_trait_of_id(id) {
            return Some(TraitKey::builtin(name));
        }
        let def = self.def(id)?;
        match def.kind {
            crate::defs::DefKind::Trait(t) => Some(TraitKey {
                id: t,
                name: def.name,
            }),
            _ => None,
        }
    }

    /// The type a resolver slot names.
    pub(super) fn res_type(&self, res: Option<crate::defs::Res>) -> Option<TypeRef> {
        match res {
            Some(crate::defs::Res::Def(id)) => self.def_type(id),
            _ => None,
        }
    }

    /// The type a name written in a type position names: its resolution;
    /// for a name with none (the checker wrote it itself), the module's
    /// own type of that name, else the builtin type of that name.
    pub(super) fn named_type(
        &self,
        res: Option<crate::defs::Res>,
        name: Symbol,
    ) -> Option<TypeRef> {
        if res.is_some() {
            return self.res_type(res);
        }
        if let Some(ty) = self.own_types.get(&name) {
            return Some(*ty);
        }
        crate::defs::builtin_type_id(&resolve(name)).map(|id| TypeRef { id, name })
    }

    /// Whether a type name, or a variant's resolution, names a type
    /// declaration the module's check rejected (see `rejected_types`).
    pub(super) fn names_rejected(&self, res: Option<crate::defs::Res>, name: Symbol) -> bool {
        if self.rejected_types.is_empty() {
            return false;
        }
        self.named_type(res, name)
            .or_else(|| self.res_variant_enum(res))
            .is_some_and(|ty| self.rejected_types.contains(&ty))
    }

    /// The type `ty` with a fresh type variable for each of its
    /// parameters, so that distinct uses do not share the template
    /// variables: `List(a)`, `Box(a)`. An alias is a
    /// `Type::Generic(alias, args)` head the canonicaliser expands. `None` for a
    /// type with no such instance (`Tuple`, `Fn`).
    pub(super) fn type_instance(&mut self, ty: TypeRef) -> Option<Type> {
        if let Some(builtin) = builtin_type_name(ty) {
            match builtin {
                "Int" => return Some(Type::Int),
                "Float" => return Some(Type::Float),
                "Bool" => return Some(Type::Bool),
                "String" => return Some(Type::String),
                "()" => return Some(Type::Unit),
                "List" => return Some(Type::List(Box::new(self.fresh_var()))),
                "Map" => {
                    return Some(Type::Map(
                        Box::new(self.fresh_var()),
                        Box::new(self.fresh_var()),
                    ));
                }
                "Set" => return Some(Type::Set(Box::new(self.fresh_var()))),
                "Channel" => return Some(Type::Channel(Box::new(self.fresh_var()))),
                // Opaque resource / value types from builtin
                // modules: the shape the builtin schemes produce.
                _ if let Some(arity) = opaque_arity(builtin) => {
                    let args = (0..arity).map(|_| self.fresh_var()).collect();
                    return Some(Type::Generic(ty, args));
                }
                _ => {}
            }
        }
        let known = self.tables.records.contains_key(&ty)
            || self.tables.enums.contains_key(&ty)
            || self.tables.type_aliases.contains(&ty);
        if !known {
            return None;
        }
        let arity = self.type_arity(ty).unwrap_or(0);
        Some(Type::Generic(
            ty,
            (0..arity).map(|_| self.fresh_var()).collect(),
        ))
    }

    /// The names of the type parameters of `ty` (`Map` has `k, v`):
    /// empty for a type that takes none, and for `Fn` and `Tuple`,
    /// which take any number.
    pub(super) fn type_param_names(&self, ty: TypeRef) -> Vec<String> {
        let declared = (self.tables.records.get(&ty).map(|r| r.params.clone()))
            .or_else(|| self.tables.enums.get(&ty).map(|e| e.params.clone()))
            .or_else(|| (self.tables.resolver.lookup_alias(ty)).map(|info| info.params.clone()));
        if let Some(params) = declared.filter(|params| !params.is_empty()) {
            return params.iter().map(|p| resolve(*p)).collect();
        }
        let arity = match builtin_type_name(ty) {
            Some("Map") => return vec!["k".to_string(), "v".to_string()],
            Some("List" | "Set" | "Channel") => 1,
            Some(b) => opaque_arity(b).unwrap_or(0),
            // (A record named in a field before the declarations are in.)
            None => self.type_arity(ty).unwrap_or(0),
        };
        ('a'..='z').take(arity).map(String::from).collect()
    }

    /// What is wrong with `shown`, a use of the type `ty` with no type
    /// arguments in a type, when `ty` takes some: a generic type is
    /// written with its arguments (`Box(a)`, not `Box`).
    pub(super) fn bare_generic_message(&self, shown: &str, ty: TypeRef) -> Option<String> {
        let params = self.type_param_names(ty);
        if params.is_empty() {
            return None;
        }
        Some(format!(
            "`{shown}` takes {} {} here: write `{shown}({})`",
            params.len(),
            super::inference::plural(params.len(), "type argument", "type arguments"),
            params.join(", ")
        ))
    }

    /// What is wrong with `shown`, a use of the type `ty` with type
    /// arguments it does not take: none, or the number it takes (`Box()`
    /// of a generic `Box`, `Bytes()`, `Bytes(Int)`). A type is written
    /// one way: without parentheses when it takes no arguments.
    /// `type_arguments_message` for the header of an impl of a trait of
    /// type constructors, which writes `_` for the first type argument:
    /// `W(_)`, `W(_, b)`.
    pub(super) fn constructor_arguments_message(&self, shown: &str, ty: TypeRef) -> String {
        let mut params = self.type_param_names(ty);
        let Some(first) = params.first_mut() else {
            return self.type_arguments_message(shown, ty);
        };
        *first = "_".to_string();
        format!(
            "`{shown}` takes {} {} here: write `{shown}({})`",
            params.len(),
            super::inference::plural(params.len(), "type argument", "type arguments"),
            params.join(", ")
        )
    }

    pub(super) fn type_arguments_message(&self, shown: &str, ty: TypeRef) -> String {
        (self.bare_generic_message(shown, ty))
            .unwrap_or_else(|| format!("`{shown}` takes no type arguments: write `{shown}`"))
    }

    /// Report `shown` as `bare_generic_message` says; whether it was.
    pub(super) fn bare_generic(&mut self, shown: &str, ty: TypeRef, span: Span) -> bool {
        let Some(message) = self.bare_generic_message(shown, ty) else {
            return false;
        };
        self.error(Code::ArityMismatch, message, span);
        true
    }

    /// The number of type parameters of a record, enum or alias type;
    /// `None` for a type that is none of these.
    pub(super) fn type_arity(&self, ty: TypeRef) -> Option<usize> {
        self.tables
            .record_param_var_ids
            .get(&ty)
            .map(|v| v.len())
            .or_else(|| self.tables.enums.get(&ty).map(|e| e.params.len()))
            .or_else(|| self.tables.records.contains_key(&ty).then_some(0))
            .or_else(|| self.tables.type_alias_arity.get(&ty).copied())
    }

    /// The trait a trait name names: its resolution; for a name with none
    /// (the checker wrote it itself), the module's own trait of that
    /// name, else the builtin trait of that name.
    pub(super) fn named_trait(
        &self,
        res: Option<crate::defs::Res>,
        name: Symbol,
    ) -> Option<TraitKey> {
        match res {
            Some(crate::defs::Res::Def(id)) => self.def_trait(id),
            Some(_) => None,
            None => self.own_traits.get(&name).copied().or_else(|| {
                crate::defs::builtin_trait_id(&resolve(name)).map(|id| TraitKey { id, name })
            }),
        }
    }

    /// The trait the impl `ti` implements.
    pub(super) fn impl_trait(&self, ti: &TraitImpl) -> Option<TraitKey> {
        self.named_trait(ti.trait_res, ti.trait_name)
    }

    /// The type variable a `type a` parameter introduces (or names, when
    /// an earlier parameter's annotation wrote `a`): where it is written
    /// is kept, as for a variable of an annotation (`var_written`).
    pub(super) fn type_param_var(
        &mut self,
        names: &mut HashMap<Symbol, Type>,
        name: Symbol,
        span: Span,
    ) -> Type {
        if let Some(known) = names.get(&name) {
            return known.clone();
        }
        let (ty, var) = self.fresh_tv();
        self.var_written.insert(var, span);
        self.type_param_vars.insert(var);
        names.insert(name, ty.clone());
        ty
    }

    /// The type the impl `ti` is for, as impls are keyed: the canonical
    /// head of its target (an alias is the type it stands for).
    pub(super) fn impl_target(&self, ti: &TraitImpl) -> Option<TypeRef> {
        let ty = self.named_type(ti.target_res, ti.target_type)?;
        Some(canonical_head(&self.tables.resolver, ty))
    }

    /// What each variant of the enum `ty` resolves to, by name (none in
    /// the builtin environment, which the builtin definitions are made
    /// from).
    pub(super) fn variant_resolutions(&self, ty: TypeRef) -> HashMap<Symbol, crate::defs::Res> {
        let Some(defs) = &self.defs else {
            return HashMap::new();
        };
        defs.variants(ty.id.0)
            .iter()
            .copied()
            .filter_map(|id| Some((self.def(id)?.name, crate::defs::Res::Def(id))))
            .collect()
    }

    /// The enum a resolver slot naming a variant names it of.
    pub(super) fn res_variant_enum(&self, res: Option<crate::defs::Res>) -> Option<TypeRef> {
        let def = self.res_def(res)?;
        match def.kind {
            crate::defs::DefKind::Variant { ty, .. } => self.def_type(ty.0),
            _ => None,
        }
    }

    /// Whether `ty`, a type variable of an annotation, stands for a type
    /// constructor.
    pub(super) fn is_constructor_var(&self, ty: &Type) -> bool {
        match ty {
            Type::Var(v) => self.constructor_vars.contains(v),
            Type::Rigid(r) => self.constructor_vars.contains(&r.var),
            _ => false,
        }
    }

    /// Whether `ty` is a variable that stands for a type constructor:
    /// an annotation's, or one a scheme's instantiation made of one.
    pub(super) fn of_constructors(&self, ty: &Type) -> bool {
        let var = match ty {
            Type::Var(v) => *v,
            Type::Rigid(r) => r.var,
            _ => return false,
        };
        self.constructor_vars.contains(&var) || self.tables.constructor_names.contains_key(&var)
    }

    /// The type `name(args)` of an annotation, where `name` is a type
    /// constructor variable: it takes one type argument, and a type
    /// declaration's parameters are types.
    fn constructor_variable(
        &mut self,
        te: &TypeExpr,
        name: Symbol,
        args: &[TypeExpr],
        param_vars: &mut HashMap<Symbol, Type>,
    ) -> Type {
        let resolved: Vec<Type> = args
            .iter()
            .map(|a| self.resolve_type_expr(a, param_vars))
            .collect();
        if let Some((declared, params)) = &self.declaring_type
            && params.contains(&name)
        {
            let message = format!(
                "type parameter '{name}' of '{declared}' is written with a type argument: the \
                 parameters of a type are types"
            );
            self.error(Code::KindMismatch, message, te.span);
            return Type::Error;
        }
        if resolved.len() != 1 {
            let message = format!(
                "a type constructor variable takes one type argument: '{name}' is given {}",
                resolved.len()
            );
            self.error(Code::KindMismatch, message, te.span);
            return Type::Error;
        }
        let head = match param_vars.get(&name) {
            Some(tv) if self.is_constructor_var(tv) => tv.clone(),
            // A `type m` parameter's: the signature applies it, so it is
            // a type constructor (`fn wrap(x: a, type m) -> m(a)`).
            Some(Type::Var(var)) if self.type_param_vars.contains(var) => {
                let var = *var;
                self.constructor_vars.insert(var);
                self.tables.constructor_names.insert(var, name);
                Type::Var(var)
            }
            // Written without a type argument elsewhere: a signature's
            // own check says so (`check_signature_kinds`), an annotation
            // in its body here.
            Some(tv) => {
                if !self.resolving_signature {
                    let var = match tv {
                        Type::Rigid(r) => Some(r.var),
                        Type::Var(v) => Some(*v),
                        _ => None,
                    };
                    let without = (var.and_then(|v| self.bare_labels.get(&v)))
                        .map_or_else(|| name.to_string(), |label| label.to_string());
                    let message = format!(
                        "'{name}' is written with a type argument in `{}` and without one in \
                         `{without}`: a type variable is a type or a type constructor, not both",
                        type_expr_text(te)
                    );
                    self.error(Code::KindMismatch, message, te.span);
                }
                return Type::Error;
            }
            None => {
                let (tv, var) = self.fresh_tv();
                self.var_written.insert(var, te.span);
                self.constructor_vars.insert(var);
                self.tables.constructor_names.insert(var, name);
                param_vars.insert(name, tv.clone());
                tv
            }
        };
        let arg = resolved.into_iter().next().expect("one argument");
        Type::App(Box::new(head), Box::new(arg))
    }

    /// Whether the bound `name: tr` of a `where` clause fits the kind of
    /// the variable `var`, a type or a type constructor, as the trait's;
    /// what does not is reported at `span` (with a help where the
    /// variable could be written applied: not an impl header's).
    pub(super) fn bound_fits_kind(
        &mut self,
        var: &Type,
        (name, appliable): (Symbol, bool),
        tr: TraitKey,
        span: Span,
    ) -> bool {
        let constructor = self.is_constructor_var(var);
        if constructor == self.is_constructor_trait(tr) {
            return true;
        }
        let shown = self.show_trait(tr);
        let (message, help) = match constructor {
            false => (
                format!("'{name}' is a type, but '{shown}' is a trait of type constructors"),
                appliable.then(|| {
                    format!("write the variable with its type argument, as in `xs: {name}(Int)`")
                }),
            ),
            true => (
                format!("'{name}' is a type constructor, but '{shown}' is a trait of types"),
                None,
            ),
        };
        self.error_help(Code::KindMismatch, (message, help), span);
        false
    }

    /// The type `Self(args)` of an annotation: `Self` takes one type
    /// argument, and stands for a type constructor only in a trait of
    /// type constructors and its impls.
    fn self_applied(
        &mut self,
        te: &TypeExpr,
        args: &[TypeExpr],
        param_vars: &mut HashMap<Symbol, Type>,
    ) -> Type {
        let resolved: Vec<Type> = args
            .iter()
            .map(|a| self.resolve_type_expr(a, param_vars))
            .collect();
        if resolved.len() != 1 {
            let message = format!(
                "a type constructor variable takes one type argument: 'Self' is given {}",
                resolved.len()
            );
            self.error(Code::KindMismatch, message, te.span);
            return Type::Error;
        }
        let arg = resolved.into_iter().next().expect("one argument");
        match param_vars.get(&intern("Self")).cloned() {
            Some(head @ Type::Con(..)) => Type::applied(head, arg),
            Some(head) if self.is_constructor_var(&head) => Type::applied(head, arg),
            Some(Type::Error) => Type::Error,
            Some(head) => {
                let message = format!(
                    "'Self' is the type '{}' here, not a type constructor: write it without a \
                     type argument",
                    self.show_type(&head)
                );
                self.error(Code::KindMismatch, message, te.span);
                Type::Error
            }
            // (Outside a trait, as a bare `Self` there.)
            None => self.self_outside_a_trait(te.span),
        }
    }

    /// The error of a `Self` written where it means nothing: outside a
    /// trait's declaration and an impl's methods, where it is the type
    /// the impl is for. (An impl's header writes that type itself.)
    fn self_outside_a_trait(&mut self, span: Span) -> Type {
        let message = "'Self' stands only in a trait and in the methods of an impl, where it is \
                       the impl's type";
        let help = "write the type itself here".to_string();
        self.error_help(
            Code::InvalidTypeAnnotation,
            (message.to_string(), Some(help)),
            span,
        );
        Type::Error
    }

    /// Report a type variable that the signature of `f` writes both with
    /// a type argument and without one: it is a type or a type
    /// constructor, not both.
    pub(super) fn check_signature_kinds(&mut self, f: &FnDecl) -> KindLabels {
        // (Each variable: where it is first written applied and where
        // bare, as the parameter or the result writes it. The variables
        // of one place share its label: a parameter of many of them is
        // no copy of its text each.)
        let mut uses = KindUses::default();
        let mut places: Vec<(Rc<str>, &TypeExpr)> = Vec::new();
        for param in &f.params {
            if let (ParamKind::Data, Some(te)) = (&param.kind, &param.ty) {
                let label = match &param.pattern.kind {
                    PatternKind::Ident(n) => format!("{n}: {}", type_expr_text(te)),
                    _ => format!("_: {}", type_expr_text(te)),
                };
                places.push((label.into(), te));
            }
        }
        if let Some(te) = &f.return_type {
            places.push((format!("-> {}", type_expr_text(te)).into(), te));
        }
        for (label, te) in &places {
            self.kind_uses(te, label, &mut uses);
        }
        let with_of: HashMap<Symbol, &Rc<str>> = (uses.applied.iter())
            .map(|(name, with)| (*name, with))
            .collect();
        // (A variable is bare at one place: `uses` keeps its first.)
        for (name, without, span) in &uses.bare {
            let Some(with) = with_of.get(name) else {
                continue;
            };
            let message = format!(
                "'{name}' is written with a type argument in `{with}` and without one in \
                 `{without}`: a type variable is a type or a type constructor, not both"
            );
            self.error(Code::KindMismatch, message, *span);
        }
        let bare = (uses.bare.into_iter())
            .map(|(name, label, _)| (name, label))
            .collect();
        (uses.applied, bare)
    }

    /// Keep where the signature that resolved `param_map` first wrote
    /// each of its type variables applied and bare (`labels`, from
    /// `check_signature_kinds`), for an annotation in its body that
    /// writes one the other way.
    pub(super) fn remember_constructor_labels(
        &mut self,
        (applied, bare): KindLabels,
        param_map: &HashMap<Symbol, Type>,
    ) {
        let var_of = |name: &Symbol| match param_map.get(name) {
            Some(Type::Var(v)) => Some(*v),
            Some(Type::Rigid(r)) => Some(r.var),
            _ => None,
        };
        for (name, label) in applied {
            if let Some(v) = var_of(&name) {
                self.constructor_labels.insert(v, label);
            }
        }
        for (name, label) in bare {
            if let Some(v) = var_of(&name) {
                self.bare_labels.insert(v, label);
            }
        }
    }

    /// The type variables `te` writes with a type argument and without
    /// one, into `uses`, each first at `label`.
    fn kind_uses(&self, te: &TypeExpr, label: &Rc<str>, uses: &mut KindUses) {
        let is_variable = |this: &Self, name: Symbol| {
            let text = resolve(name);
            this.named_type(te.res, name).is_none()
                && text.chars().next().is_some_and(char::is_lowercase)
        };
        match &te.kind {
            TypeExprKind::Named {
                module: None, name, ..
            } => {
                if is_variable(self, *name) && uses.seen_bare.insert(*name) {
                    uses.bare.push((*name, Rc::clone(label), te.span));
                }
            }
            TypeExprKind::Generic {
                module, name, args, ..
            } => {
                if module.is_none() && is_variable(self, *name) && uses.seen_applied.insert(*name) {
                    uses.applied.push((*name, Rc::clone(label)));
                }
                for arg in args {
                    self.kind_uses(arg, label, uses);
                }
            }
            TypeExprKind::Tuple(parts) => {
                for part in parts {
                    self.kind_uses(part, label, uses);
                }
            }
            TypeExprKind::Function(params, ret) => {
                for part in params.iter().chain([&**ret]) {
                    self.kind_uses(part, label, uses);
                }
            }
            TypeExprKind::AnonRecord { fields, .. } => {
                for (_, part) in fields {
                    self.kind_uses(part, label, uses);
                }
            }
            _ => {}
        }
    }

    /// Resolve a TypeExpr AST node to our internal Type representation.
    pub(super) fn resolve_type_expr(
        &mut self,
        te: &TypeExpr,
        param_vars: &mut HashMap<Symbol, Type>,
    ) -> Type {
        // Canonicalise the result so a type written through an alias
        // arrives at the unifier (and at every other typechecker
        // consumer) as the type it stands for.
        //
        // Idempotence note: canonicalize is structurally idempotent
        // (see canonicalize_idempotent test in src/types/canonical.rs),
        // so the recursive `self.resolve_type_expr(...)` calls inside
        // the match each canonicalise their subtree, and the outer
        // call canonicalises the already-canonical result — a no-op.
        let resolved = self.resolve_type_expr_inner(te, param_vars);
        crate::types::canonical::canonicalize(&self.tables.resolver, &resolved)
    }

    pub(super) fn resolve_type_expr_inner(
        &mut self,
        te: &TypeExpr,
        param_vars: &mut HashMap<Symbol, Type>,
    ) -> Type {
        match &te.kind {
            TypeExprKind::Named { module, name, .. } => {
                // What the name means, the resolver said: nothing (it
                // reported why, or the type comes from a module that
                // failed to load), a type variable, or a type.
                if te.res == Some(crate::defs::Res::Error) {
                    return Type::Error;
                }
                if module.is_none()
                    && let Some(tv) = param_vars.get(name)
                {
                    // A type parameter variable. (One written with a type
                    // argument elsewhere is a type constructor: a
                    // signature's own check says so,
                    // `check_signature_kinds`; an annotation in its body
                    // says so here.)
                    if self.is_constructor_var(tv) {
                        if !self.resolving_signature {
                            let var = match tv {
                                Type::Rigid(r) => r.var,
                                Type::Var(v) => *v,
                                _ => unreachable!("a constructor variable is a variable"),
                            };
                            let with = (self.constructor_labels.get(&var))
                                .map_or_else(|| format!("{name}(a)"), |label| label.to_string());
                            let message = format!(
                                "'{name}' is written with a type argument in `{with}` and without \
                                 one in `{name}`: a type variable is a type or a type constructor, \
                                 not both"
                            );
                            self.error(Code::KindMismatch, message, te.span);
                        }
                        return Type::Error;
                    }
                    return tv.clone();
                }
                let name_str = resolve(*name);
                if self.names_rejected(te.res, *name) {
                    return Type::Error;
                }
                // The type of what never returns: a builtin's signature
                // says it anywhere (`panic`), a program only as a
                // function's return type (`fn fail(..) -> Never`), unless
                // it declares a type of that name.
                if module.is_none()
                    && name_str == "Never"
                    && self.named_type(te.res, *name).is_none()
                {
                    if self.registry_rows || self.never_return {
                        return Type::Never;
                    }
                    self.error(Code::InvalidTypeAnnotation, NEVER_ELSEWHERE, te.span);
                    return Type::Error;
                }
                let Some(ty) = self.named_type(te.res, *name) else {
                    // Lowercase names in type annotations are type variables
                    // (e.g., `a` in `List(a)` or `fn foo(x: a) -> a`)
                    let first_char = name_str.chars().next().unwrap_or('A');
                    if first_char.is_lowercase() {
                        // ... unless the name is a known type spelled
                        // in lowercase (`x: int`): that is a typo for
                        // the type, not a type variable.
                        if self.case_mismatched_type_name(&name_str, false).is_some() {
                            self.error(
                                Code::UnknownType,
                                self.unknown_type_message(&name_str, false),
                                te.span,
                            );
                            return Type::Error;
                        }
                        let (tv, var) = self.fresh_tv();
                        self.var_written.insert(var, te.span);
                        param_vars.insert(*name, tv.clone());
                        return tv;
                    }
                    // An uppercase name that refers to
                    // nothing is reported here rather than becoming a
                    // ghost type that cascades into "does not implement
                    // Display" and "type mismatch" far from the
                    // annotation.
                    self.error(
                        Code::UnknownType,
                        self.unknown_type_message(&name_str, false),
                        te.span,
                    );
                    return Type::Error;
                };
                if let Some(message) = impl_only_type_message(ty) {
                    self.error(Code::UnknownType, message, te.span);
                    return Type::Error;
                }
                if self.bare_generic(&type_expr_text(te), ty, te.span) {
                    return Type::Error;
                }
                match self.type_instance(ty) {
                    Some(t) => t,
                    None => {
                        self.error(
                            Code::UnknownType,
                            self.unknown_type_message(&name_str, false),
                            te.span,
                        );
                        Type::Error
                    }
                }
            }
            TypeExprKind::Generic {
                module, name, args, ..
            } => {
                // A type the resolver resolved to nothing takes any
                // arguments; nothing is known about it.
                if te.res == Some(crate::defs::Res::Error) {
                    for arg in args {
                        let _ = self.resolve_type_expr_inner(arg, param_vars);
                    }
                    return Type::Error;
                }
                let name_str = resolve(*name);
                // `Never(..)`: `Never` takes no arguments, and is no type
                // where it has them (unless a type of that name is
                // declared). One error: its arguments are not looked at.
                if module.is_none()
                    && name_str == "Never"
                    && self.named_type(te.res, *name).is_none()
                {
                    self.error(Code::InvalidTypeAnnotation, NEVER_ELSEWHERE, te.span);
                    return Type::Error;
                }
                // `Self(a)`: the type constructor of a trait of type
                // constructors, or of its impl, applied.
                if module.is_none()
                    && name_str == "Self"
                    && self.named_type(te.res, *name).is_none()
                {
                    return self.self_applied(te, args, param_vars);
                }
                // A lowercase name applied is a type constructor variable
                // (`m(Int)`), unless it is a type spelled in lowercase.
                if module.is_none()
                    && self.named_type(te.res, *name).is_none()
                    && name_str.chars().next().is_some_and(char::is_lowercase)
                    && self.case_mismatched_type_name(&name_str, true).is_none()
                {
                    return self.constructor_variable(te, *name, args, param_vars);
                }
                let resolved_args: Vec<Type> = args
                    .iter()
                    .map(|a| self.resolve_type_expr(a, param_vars))
                    .collect();
                if self.names_rejected(te.res, *name) {
                    return Type::Error;
                }
                let ty = self.named_type(te.res, *name);
                if let Some(message) = ty.and_then(impl_only_type_message) {
                    self.error(Code::UnknownType, message, te.span);
                    return Type::Error;
                }
                let shown = Qualifier::written(*module, *name);
                // (`Bytes()`, `Box()`: empty parentheses are no spelling
                // of a type.)
                if let Some(ty) = ty
                    && args.is_empty()
                {
                    let message = self.type_arguments_message(&shown, ty);
                    self.error(Code::ArityMismatch, message, te.span);
                    return Type::Error;
                }
                if let Some(builtin) = ty.and_then(builtin_type_name) {
                    let n = resolved_args.len();
                    let mut it = resolved_args.clone().into_iter();
                    let mut arg = |this: &mut Self| it.next().unwrap_or_else(|| this.fresh_var());
                    match (builtin, n) {
                        ("List", 1) => return Type::List(Box::new(arg(self))),
                        ("Map", 2) => {
                            let k = arg(self);
                            let v = arg(self);
                            return Type::Map(Box::new(k), Box::new(v));
                        }
                        ("Set", 1) => return Type::Set(Box::new(arg(self))),
                        ("Channel", 1) => return Type::Channel(Box::new(arg(self))),
                        (b, n) if let Some(arity) = opaque_arity(b) => {
                            let ty = ty.expect("a builtin type");
                            if n == arity {
                                return Type::Generic(ty, resolved_args);
                            }
                            let message = self.type_arguments_message(&shown, ty);
                            self.error(Code::ArityMismatch, message, te.span);
                            return Type::Error;
                        }
                        _ => {}
                    }
                }
                // Enforce arity for user-declared parameterized
                // records, enums and aliases: a mismatched arity dropped
                // at unify time would leave a runtime type error
                // at first use of the field. A parameterless record has
                // arity 0, so `Point(Bool)` is reported too.
                // The generic form `Frobnitz(Int)` for an
                // undeclared `Frobnitz` reports "unknown type
                // 'Frobnitz'" at the annotation span, matching the
                // bare-name path.
                // (A builtin type given a number of arguments it does
                // not take, `Map(String)`, `List(Int, Int)`, `Int(a)`.)
                if let Some(ty) = ty
                    && builtin_type_name(ty).is_some()
                    && self.type_arity(ty).is_none()
                {
                    let message = self.type_arguments_message(&shown, ty);
                    self.error(Code::ArityMismatch, message, te.span);
                    return Type::Error;
                }
                let Some((ty, expected)) = ty.and_then(|ty| Some((ty, self.type_arity(ty)?)))
                else {
                    self.error(
                        Code::UnknownType,
                        self.unknown_type_message(&name_str, true),
                        te.span,
                    );
                    return Type::Error;
                };
                if expected != resolved_args.len() {
                    let message = self.type_arguments_message(&shown, ty);
                    self.error(Code::ArityMismatch, message, te.span);
                    // Return Error so the subsequent unify doesn't
                    // cascade a second "arity mismatch" diagnostic
                    // (the Generic/Generic arm would re-detect the
                    // same problem). The first report already has
                    // the user-facing span; extras only confuse.
                    return Type::Error;
                }
                Type::Generic(ty, resolved_args)
            }
            TypeExprKind::Tuple(elems) => {
                // `()` is the canonical unit type — not a zero-arity tuple.
                if elems.is_empty() {
                    return Type::Unit;
                }
                let types: Vec<Type> = elems
                    .iter()
                    .map(|e| self.resolve_type_expr(e, param_vars))
                    .collect();
                Type::Tuple(types)
            }
            TypeExprKind::Function(params, ret) => {
                let param_types: Vec<Type> = params
                    .iter()
                    .map(|p| self.resolve_type_expr(p, param_vars))
                    .collect();
                let ret_type = self.resolve_type_expr(ret, param_vars);
                Type::Fun(param_types, Box::new(ret_type))
            }
            TypeExprKind::SelfType => match param_vars.get(&intern("Self")) {
                Some(ty) => ty.clone(),
                None => self.self_outside_a_trait(te.span),
            },
            TypeExprKind::AnonRecord { fields, tail } => {
                use std::collections::BTreeMap;
                let mut field_map: BTreeMap<Symbol, Type> = BTreeMap::new();
                let mut seen: std::collections::HashSet<Symbol> = std::collections::HashSet::new();
                for (n, t) in fields {
                    if !seen.insert(*n) {
                        self.error(
                            Code::DuplicateRecordField,
                            format!("duplicate field '{}' in anon record type", n),
                            te.span,
                        );
                        continue;
                    }
                    field_map.insert(*n, self.resolve_type_expr(t, param_vars));
                }
                let row_tail = match tail {
                    None => RowTail::Closed,
                    Some((rname, _)) => {
                        // Row-tail variable name: bind it through param_vars
                        // so multiple annotations in the same scope sharing
                        // a row name refer to the same row variable. The
                        // entry in param_vars is `Type::Var(id)` for normal
                        // type vars; for row tails we still use the same
                        // map but carry the id directly.
                        let key = intern(&format!("__row__{}", resolve(*rname)));
                        match param_vars.get(&key).cloned() {
                            Some(Type::Var(v)) => RowTail::Var(v),
                            // Inside the declaration that introduced it.
                            Some(Type::Rigid(r)) => RowTail::Rigid(r),
                            // A row that was reported (a type
                            // declaration's): the record is not known.
                            Some(Type::Error) => return Type::Error,
                            _ => {
                                let id = self.fresh_tyvar_id();
                                param_vars.insert(key, Type::Var(id));
                                RowTail::Var(id)
                            }
                        }
                    }
                };
                Type::AnonRecord {
                    fields: field_map,
                    tail: row_tail,
                }
            }
        }
    }
}

/// The number of type arguments of the builtin type `name` when it is
/// opaque (`Bytes`, `tcp.TcpStream`, `task.Handle(a)`, `TypeOf(a)`): it
/// has no variants and no fields. `None` for any other builtin type.
/// What is wrong with a builtin type name that names a kind of type
/// only in the header of an impl (`trait T for Tuple`, `trait T for
/// Fn`), written in a type: a tuple type and a function type are
/// written with their parts.
fn impl_only_type_message(ty: TypeRef) -> Option<String> {
    let written = match builtin_type_name(ty)? {
        "Tuple" => "a tuple type is written with its parts: `(Int, String)`",
        "Fn" | "Fun" => "a function type is written with its parts: `Fn(Int) -> String`",
        _ => return None,
    };
    let name = builtin_type_name(ty)?;
    Some(format!(
        "`{name}` names a type only in the header of an impl (`trait Show for {name}`): {written}"
    ))
}

fn opaque_arity(name: &str) -> Option<usize> {
    crate::defs::opaque_type_arity(name)
}

/// `te` as a program writes it, for a message that quotes it.
/// Where a signature writes each type variable with a type argument and
/// where without one (`check_signature_kinds`): `xs: m(Int)`, `-> m`.
pub(super) type KindLabels = (Vec<(Symbol, Rc<str>)>, Vec<(Symbol, Rc<str>)>);

/// The type variables a signature writes with a type argument and
/// without one, each at the first place that does (`kind_uses`).
#[derive(Default)]
struct KindUses {
    applied: Vec<(Symbol, Rc<str>)>,
    bare: Vec<(Symbol, Rc<str>, Span)>,
    seen_applied: std::collections::HashSet<Symbol>,
    seen_bare: std::collections::HashSet<Symbol>,
}

pub(crate) fn type_expr_text(te: &TypeExpr) -> String {
    let all = |types: &[TypeExpr]| {
        types
            .iter()
            .map(type_expr_text)
            .collect::<Vec<_>>()
            .join(", ")
    };
    match &te.kind {
        TypeExprKind::Named { module, name, .. } => Qualifier::written(*module, *name),
        TypeExprKind::Generic {
            module, name, args, ..
        } => format!("{}({})", Qualifier::written(*module, *name), all(args)),
        TypeExprKind::Tuple(parts) => format!("({})", all(parts)),
        TypeExprKind::Function(params, ret) => {
            format!("Fn({}) -> {}", all(params), type_expr_text(ret))
        }
        TypeExprKind::SelfType => "Self".to_string(),
        TypeExprKind::AnonRecord { fields, tail } => {
            let mut parts: Vec<String> = (fields.iter())
                .map(|(n, t)| format!("{n}: {}", type_expr_text(t)))
                .collect();
            if let Some((row, _)) = tail {
                parts.push(format!("...{row}"));
            }
            format!("{{{}}}", parts.join(", "))
        }
    }
}
