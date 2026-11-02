use super::typeexpr::type_expr_text;
use super::*;

/// What a trait asks of a type variable for a type to have it: the
/// variable, the trait and the trait arguments (none: any).
type Need = (TyVar, TraitKey, Vec<Type>);

impl TypeChecker {
    /// Whether the impl of `tr` for `head` was reported for a type
    /// variable of its type that its header does not give a trait the
    /// supertrait `sup`, at the trait arguments `sup_args`, asks of it at
    /// that type (`'a' needs 'Equal'`, `'a' needs 'Sup(Int)'`).
    fn supertrait_needs_reported(
        &mut self,
        key: ImplKey,
        sup: TraitKey,
        sup_args: &[Type],
        span: Span,
    ) -> bool {
        let (tr, _, _) = key;
        let Some(self_ty) = self.tables.impl_self_types.get(&key).cloned() else {
            return false;
        };
        let mut needs: Vec<Need> = Vec::new();
        if !self.trait_needs(sup, sup_args, &self_ty, 0, &mut needs) {
            // (A part that is no variable lacks it: the impl's type has no
            // impl of the supertrait. Of the form with trait arguments,
            // the caller says which it has.)
            if !sup_args.is_empty() {
                return false;
            }
            // (The impl's whole type: `Box(String)` of impls of the
            // supertrait for other instantiations.)
            let rigid: Vec<RigidId> = (self.impl_var_names.get(&key))
                .into_iter()
                .flatten()
                .map(|&(var, name)| RigidId { var, name })
                .collect();
            let (ty, tr, sup) = (
                self.show_type(&rigidify(&self_ty, &rigid)),
                self.show_trait(tr),
                self.show_trait(sup),
            );
            self.error(
                Code::MissingTraitImpl,
                format!("type '{ty}' implements '{tr}' but does not implement supertrait '{sup}'"),
                span,
            );
            return true;
        }
        // What the header gives, with the supertraits of each bound (at
        // any trait arguments: those of a bound's supertraits are not
        // read here).
        let mut given: Vec<(TyVar, TraitKey, Option<Vec<Type>>)> = Vec::new();
        for pred in self.tables.impl_preds.get(&key).into_iter().flatten() {
            if let Pred::Trait {
                tr: bound,
                subject: Type::Var(v),
                args,
                ..
            } = pred
            {
                given.push((*v, *bound, Some(args.clone())));
                given.extend((self.supertraits_of(*bound).into_iter()).map(|s| (*v, s, None)));
            }
        }
        // (A bound at other trait arguments gives no need at these: one
        // impl is no other.)
        let gives = |(v, t, args): &Need| {
            (given.iter()).any(|(gv, gt, gargs)| {
                gv == v
                    && gt == t
                    && (args.is_empty()
                        || gargs.as_ref().is_none_or(|gargs| {
                            gargs.len() == args.len()
                                && (gargs.iter().zip(args))
                                    .all(|(g, a)| self.apply(g) == self.apply(a))
                        }))
            })
        };
        let mut missing: Vec<Need> = Vec::new();
        for need in needs.iter().filter(|need| !gives(need)) {
            if !missing.contains(need) {
                missing.push(need.clone());
            }
        }
        let Some((var, trait_needed, needed_args)) = missing.first().cloned() else {
            return false;
        };
        // The impl's type variables by the names its header writes
        // (letters for those it does not name: a `Tuple` impl's).
        let mut names: Vec<(TyVar, Symbol)> =
            self.impl_var_names.get(&key).cloned().unwrap_or_default();
        let named = !names.is_empty();
        for v in free_vars_in(&self_ty) {
            if !names.iter().any(|(known, _)| *known == v) {
                let letter = char::from(b'a' + (names.len() % 26) as u8);
                names.push((v, intern(&letter.to_string())));
            }
        }
        let mapping: HashMap<TyVar, Type> = names
            .iter()
            .map(|(v, name)| {
                (
                    *v,
                    Type::Rigid(RigidId {
                        var: *v,
                        name: *name,
                    }),
                )
            })
            .collect();
        let shown_ty = self.show_type(&substitute_vars(&self_ty, &mapping));
        let name_of = |var: TyVar| {
            (names.iter())
                .find(|(v, _)| *v == var)
                .map_or_else(|| "a".to_string(), |(_, name)| resolve(*name))
        };
        let var_name = name_of(var);
        // (Of traits with parameters, at the impl's and the supertrait's
        // trait arguments.)
        let impl_args = self
            .tables
            .impl_trait_args
            .get(&key)
            .cloned()
            .unwrap_or_default();
        let (tr_shown, sup_shown, needed) = (
            self.show_bound(tr, &impl_args),
            self.show_bound(sup, sup_args),
            self.show_bound(trait_needed, &needed_args),
        );
        // The help names every variable that needs a bound.
        let bounds: Vec<String> = (missing.iter())
            .map(|(v, t, args)| format!("{}: {}", name_of(*v), self.show_bound(*t, args)))
            .collect();
        let bounds = bounds.join(", ");
        // (An impl for every tuple names no part, and no bound can
        // help: the supertrait holds for a tuple by its parts.)
        if !named && key.1.is_builtin("Tuple") {
            let message = format!(
                "trait '{tr_shown}' cannot be implemented for every tuple: its supertrait \
                 '{sup_shown}' holds for a tuple only when it holds for the tuple's parts, and \
                 an impl for every tuple has no bounds on its parts"
            );
            self.error(Code::MissingTraitImpl, message, span);
            return true;
        }
        let help = match named {
            true => format!("add `where {bounds}` to the impl"),
            false => format!("write the impl for `{shown_ty}`, with `where {bounds}`"),
        };
        self.error_help(
            Code::MissingTraitImpl,
            (
                format!(
                    "type '{shown_ty}' implements '{tr_shown}' but does not implement supertrait \
                     '{sup_shown}': '{var_name}' needs '{needed}'"
                ),
                Some(help),
            ),
            span,
        );
        true
    }

    /// What the trait `tr` at the trait arguments `args` asks of the
    /// type variables of `ty` for `ty` to have it, gathered into
    /// `needs`: the variable itself; a part of a type that has the trait
    /// by its structure; what the header of the impl written for the
    /// type's head (at those trait arguments) asks, at the type's
    /// arguments. `false` where a part that is no variable lacks it.
    fn trait_needs(
        &self,
        tr: TraitKey,
        args: &[Type],
        ty: &Type,
        depth: usize,
        needs: &mut Vec<Need>,
    ) -> bool {
        if depth > 16 {
            return true;
        }
        let ty = crate::types::canonical::canonicalize(&self.tables.resolver, &self.apply(ty));
        if let Type::Var(v) = ty {
            let need = (v, tr, args.to_vec());
            if !needs.contains(&need) {
                needs.push(need);
            }
            return true;
        }
        let Some(head) = self.type_name_for_impl(&ty) else {
            return true;
        };
        if self.by_structure(tr, head) {
            // (What json decodes is the decoder's to say.)
            if Self::is_decode(tr) {
                return true;
            }
            let mut walk = super::solve::Walk::default();
            if self.structure_gap(tr, &ty, &mut walk, 0).is_some() {
                return false;
            }
            for part in walk.open {
                if let Type::Var(v) = part
                    && !needs.contains(&(v, tr, Vec::new()))
                {
                    needs.push((v, tr, Vec::new()));
                }
            }
            return (walk.written.iter())
                .all(|part| self.trait_needs(tr, &[], part, depth + 1, needs));
        }
        if !self.tables.trait_impl_set.contains(&(tr, head)) {
            return false;
        }
        // (The impl a part's type reaches: of a trait with parameters,
        // the one at the trait arguments, or the one impl it has for the
        // type if it has one.)
        // (Of impls for some instantiations of the type, the one for
        // every type `ty` stands for: an impl for `Box(Int)` is none for
        // `Box(a)`.)
        let key = match self.has_instances(tr, head) {
            true => {
                let fitting: Vec<ImplKey> = (self.impls_fitting(tr, head, Some(&ty), args))
                    .into_iter()
                    .filter(|key| {
                        self.impl_instance(key).is_none_or(|instance| {
                            super::solve::match_one_way(&instance, &ty, &mut HashMap::new())
                        })
                    })
                    .collect();
                match fitting.as_slice() {
                    [key] => *key,
                    _ => return false,
                }
            }
            false => {
                let at = self.impl_reached(tr, head, Some(&ty), args);
                let Some(key) = at.or_else(|| self.tables.one_impl(tr, head)) else {
                    return true;
                };
                key
            }
        };
        let Some(impl_ty) = self.tables.impl_self_types.get(&key) else {
            return true;
        };
        let mut mapping: HashMap<TyVar, Type> = HashMap::new();
        super::builtin_needs::match_type(impl_ty, &ty, &free_vars_in(impl_ty), &mut mapping);
        if let Some(impl_args) = self.tables.impl_trait_args.get(&key) {
            for (written, asked) in impl_args.iter().zip(args) {
                super::builtin_needs::match_type(
                    written,
                    asked,
                    &free_vars_in(written),
                    &mut mapping,
                );
            }
        }
        (self.tables.impl_preds.get(&key).into_iter().flatten()).all(|pred| match pred {
            Pred::Trait {
                tr: bound,
                subject,
                args: bound_args,
                ..
            } => {
                let bound_args: Vec<Type> = (bound_args.iter())
                    .map(|arg| substitute_vars(arg, &mapping))
                    .collect();
                self.trait_needs(
                    *bound,
                    &bound_args,
                    &substitute_vars(subject, &mapping),
                    depth + 1,
                    needs,
                )
            }
            _ => true,
        })
    }

    /// The type of the impl `key` as a message names it: by its head
    /// (`Box`), a type constructor as its header writes it
    /// (`Result(_, e)`).
    fn impl_type_shown(&self, key: &ImplKey) -> String {
        match self.tables.impl_self_types.get(key) {
            Some(con @ Type::Con(..)) => {
                let rigid: Vec<RigidId> = (self.impl_var_names.get(key))
                    .into_iter()
                    .flatten()
                    .map(|&(var, name)| RigidId { var, name })
                    .collect();
                self.show_type(&rigidify(con, &rigid))
            }
            _ => self.show_type(&Type::Generic(key.1, Vec::new())),
        }
    }

    // ── Validate trait implementations ────────────────────────────────

    pub(super) fn validate_trait_impls(&mut self) {
        // (a) An unknown supertrait is reported where the trait is
        // registered (`register_trait_decl_inner`).

        // The impls this module writes: an impl is validated where it
        // is written, once, and not again by every module checked after
        // it in the session.
        let impl_keys: Vec<ImplKey> = self.tables.impl_keys.added().collect();
        for impl_key in &impl_keys {
            let (trait_name, type_name, args_key) = (&impl_key.0, &impl_key.1, impl_key.2);
            // The impl block, for the messages. (A row without one is
            // no written impl: nothing to validate.)
            let Some(&diag_span) = self.tables.trait_impl_spans.get(impl_key) else {
                continue;
            };

            // Check that the trait exists first.
            let Some(trait_info) = self.tables.traits.get(trait_name).cloned() else {
                self.error(
                    Code::UnknownTrait,
                    format!("trait '{trait_name}' is not declared"),
                    diag_span,
                );
                continue;
            };

            // (b) Supertrait obligation: implementing a trait on a type
            // requires every supertrait to also be implemented for the
            // same type. A structural supertrait (Display/Equal/Hash/
            // Compare) is the judgement's to answer (`head_has_trait`),
            // so `trait Ordered: Equal { ... }` followed by
            // `trait Ordered for MyType { ... }` holds when MyType has
            // Equal by its structure.
            //
            // When the supertrait reference carries args
            // (`trait Holds(b): Carry(b)` + `impl Holds(Int) for Bag`),
            // resolve those args through the enclosing trait's
            // params→impl-args mapping and require the matching impl to
            // exist with positionally-compatible args. Otherwise
            // `impl Carry(String) for Bag` would silently satisfy the
            // obligation for `impl Holds(Int) for Bag`, causing a
            // runtime method-resolution failure.
            // A trait that has `json.Decode` among its supertraits decodes
            // the impl's type, through a bound, a type call or a value
            // call: the type is what the json decoder decodes, as the
            // type's own module sees it.
            let self_type = self.tables.impl_self_types.get(impl_key).cloned();
            if let Some(refusal) =
                self.decode_impl_refusal(*trait_name, *type_name, self_type, diag_span)
            {
                self.errors.push(refusal);
                continue;
            }
            let enclosing_args: Vec<Type> = self
                .tables
                .impl_trait_args
                .get(impl_key)
                .cloned()
                .unwrap_or_default();
            for (i, supertrait) in trait_info.supertraits.iter().enumerate() {
                // (`json.Decode` is the decoder's judgement, above.)
                if Self::is_decode(*supertrait) {
                    continue;
                }
                if !self.head_has_trait(*supertrait, *type_name) {
                    let (ty, tr, sup) = (
                        self.impl_type_shown(impl_key),
                        self.show_trait(*trait_name),
                        self.show_trait(*supertrait),
                    );
                    self.error(Code::MissingTraitImpl,
                        format!(
                            "type '{ty}' implements '{tr}' but does not implement supertrait '{sup}'"
                        ),
                        diag_span,
                    );
                    continue;
                }
                // Resolve the supertrait's expected args against the
                // enclosing trait's impl args (none for the bare
                // `: Super` form).
                let expected_super_args: Vec<Type> = (trait_info.supertrait_args.get(i))
                    .into_iter()
                    .flatten()
                    .map(|te| {
                        crate::typechecker::inference::resolve_supertrait_arg(
                            te,
                            &trait_info,
                            &enclosing_args,
                        )
                    })
                    .collect();
                // The supertrait holds at the impl's own type, its type
                // variables having what the impl's header gives them
                // (`trait Same for Box(a) where a: Equal`), not at the
                // head alone: a `Box(a)` has `Equal` when its `a` does,
                // `Sup(Int)` when its `a` has `Sup(Int)`.
                if self.supertrait_needs_reported(
                    *impl_key,
                    *supertrait,
                    &expected_super_args,
                    diag_span,
                ) {
                    continue;
                }
                // (Skipped for the bare form.)
                if expected_super_args.is_empty() {
                    continue;
                }
                // (The supertrait's impl for those trait arguments.)
                let self_type = self.tables.impl_self_types.get(impl_key);
                let actual_super_args =
                    (self.impl_reached(*supertrait, *type_name, self_type, &expected_super_args))
                        .or_else(|| self.tables.one_impl(*supertrait, *type_name))
                        .and_then(|key| self.tables.impl_trait_args.get(&key))
                        .cloned()
                        .unwrap_or_default();
                let len_ok = actual_super_args.len() == expected_super_args.len();
                let pos_ok = len_ok
                    && expected_super_args
                        .iter()
                        .zip(actual_super_args.iter())
                        .all(|(e, a)| {
                            let e = self.apply(e);
                            let a = self.apply(a);
                            self.trait_arg_compatible(&e, &a)
                        });
                if !pos_ok {
                    let fmt_args = |args: &[Type]| -> String {
                        args.iter()
                            .map(|t| self.show_type(&self.apply(t)))
                            .collect::<Vec<_>>()
                            .join(", ")
                    };
                    let (tr, sup) = (self.show_trait(*trait_name), self.show_trait(*supertrait));
                    let ty = self.show_type(&Type::Generic(*type_name, Vec::new()));
                    // (The impls the type has, where none is the one: each at
                    // its trait arguments.)
                    let found: Vec<String> = match actual_super_args.is_empty() {
                        false => vec![format!("{sup}({})", fmt_args(&actual_super_args))],
                        true => (self.tables.impl_args_of(*supertrait, *type_name).iter())
                            .filter_map(|at| {
                                (self.tables.impl_trait_args).get(&(*supertrait, *type_name, *at))
                            })
                            .map(|args| self.show_bound(*supertrait, args))
                            .collect(),
                    };
                    let found: Vec<String> = found
                        .iter()
                        .map(|impl_| format!("impl {impl_} for {ty}"))
                        .collect();
                    let found = match found.is_empty() {
                        true => format!("found no impl of {sup} for {ty}"),
                        false => format!("found {}", found.join(" and ")),
                    };
                    self.error(
                        Code::InvalidTraitImpl,
                        format!(
                            "impl {tr}({}) for {ty} requires impl {sup}({}) for {ty}, but {found}",
                            fmt_args(&enclosing_args),
                            fmt_args(&expected_super_args),
                        ),
                        diag_span,
                    );
                }
            }

            // Every method the trait declares is registered for the type:
            // written in the impl, or the trait's default
            // (`register_trait_impl`).
            for (method_name, method_ty) in &trait_info.methods {
                let provided = self
                    .tables
                    .impl_methods
                    .get_at(*type_name, *method_name, *trait_name, args_key)
                    .is_some();
                if !provided && !trait_info.default_method_bodies.contains_key(method_name) {
                    // No impl method AND the trait does not provide a
                    // default body — the impl is genuinely missing a
                    // required method.
                    // (A trait with parameters is named at the impl's.)
                    let shown = match self.tables.impl_trait_args.get(impl_key) {
                        Some(args) if !args.is_empty() => self.show_bound(*trait_name, args),
                        _ => self.show_trait(*trait_name),
                    };
                    let target = self.impl_type_shown(impl_key);
                    self.error(
                        Code::InvalidTraitImpl,
                        format!(
                            "trait impl '{}' for '{}' is missing method '{}'",
                            shown, target, method_name
                        ),
                        diag_span,
                    );
                    // That is the impl's one error. The type has the
                    // trait by this impl for every question, a call of
                    // the method among them: it is entered with the
                    // type the trait declares, so a use of it is not
                    // reported as well.
                    let impl_key = *impl_key;
                    let mut mapping: HashMap<TyVar, Type> = HashMap::new();
                    if let Some(self_type) = self.tables.impl_self_types.get(&impl_key) {
                        mapping.insert(trait_info.self_var, self_type.clone());
                    }
                    if let Some(args) = self.tables.impl_trait_args.get(&impl_key)
                        && args.len() == trait_info.param_var_ids.len()
                    {
                        mapping.extend(trait_info.param_var_ids.iter().copied().zip(args.clone()));
                    }
                    for v in free_vars_in(method_ty) {
                        mapping.entry(v).or_insert_with(|| self.fresh_var());
                    }
                    let mut preds = self
                        .tables
                        .impl_preds
                        .get(&impl_key)
                        .cloned()
                        .unwrap_or_default();
                    preds.extend(Self::bounds_under(&trait_info, *method_name, &mapping));
                    self.register_method_entry(
                        *type_name,
                        *method_name,
                        args_key,
                        MethodEntry {
                            method_type: substitute_vars(method_ty, &mapping),
                            structural: false,
                            trait_name: Some(*trait_name),
                            preds,
                        },
                    );
                }
            }
        }
    }

    // ── Register trait declarations ─────────────────────────────────

    /// User-source entry point for trait declaration registration.
    /// The builtin traits are `TraitDecl` nodes too
    /// (`builtin_trait_decls()`), fed to `register_trait_decl_inner`
    /// directly: they skip only the redefinition guard, which is about
    /// what a program writes.
    pub(super) fn register_trait_decl_user(&mut self, t: &TraitDecl) {
        // Reject redefinition of builtin trait names. The compiler has
        // already preregistered TraitInfo and the structural traits for these
        // names; letting a user `trait Equal { fn eq(self) -> Bool }`
        // overwrite them would produce a cascade of bogus
        // "missing method" errors when validate_trait_impls runs the
        // preregistered impls against the user's new body.
        let trait_name_str = resolve(t.name);
        if BUILTIN_TRAIT_NAMES.contains(&trait_name_str.as_str()) {
            self.error(
                Code::InvalidTraitDeclaration,
                format!("trait '{trait_name_str}' is a builtin trait and cannot be redefined"),
                t.name_span,
            );
            return;
        }
        self.register_trait_decl_inner(t);
    }

    /// Whether `tr` is a trait of type constructors (`Functor`), not of
    /// types.
    pub(super) fn is_constructor_trait(&self, tr: TraitKey) -> bool {
        self.constructor_traits.contains(&tr)
            || self
                .tables
                .traits
                .get(&tr)
                .is_some_and(|info| info.constructor)
    }

    /// Settle which traits of `decls` are traits of type constructors
    /// (`constructor_traits`), before any is registered: a trait whose
    /// methods write `Self` with a type argument, and a trait that writes
    /// it in neither form and has such a supertrait. What a trait
    /// declares against its kind is reported.
    pub(super) fn settle_trait_kinds(&mut self, decls: &[Decl]) {
        let traits: Vec<&TraitDecl> = (decls.iter())
            .filter_map(|decl| match decl {
                Decl::Trait(t) => Some(t),
                _ => None,
            })
            .collect();
        let keys: Vec<TraitKey> = traits.iter().map(|t| self.own_trait(t.name)).collect();
        for key in &keys {
            self.constructor_traits.remove(key);
        }
        // How each trait's methods write `Self`: with a type argument
        // (`Some(true)`), without one, or not at all.
        let mut written: Vec<Option<bool>> = Vec::with_capacity(traits.len());
        for (t, key) in traits.iter().zip(&keys) {
            let mut applied: Option<Symbol> = None;
            let mut bare: Option<(Symbol, Span)> = None;
            for m in &t.methods {
                let mut uses = SelfUses::default();
                for param in &m.params {
                    if let (ParamKind::Data, Some(te)) = (&param.kind, &param.ty) {
                        uses.read(te);
                    }
                }
                if let Some(te) = &m.return_type {
                    uses.read(te);
                }
                if uses.applied && applied.is_none() {
                    applied = Some(m.name);
                }
                if let Some(span) = uses.bare
                    && bare.is_none()
                {
                    bare = Some((m.name, span));
                }
            }
            if let (Some(with), Some((without, span))) = (applied, bare) {
                let message = format!(
                    "'Self' is written with a type argument in '{with}' and without one in \
                     '{without}': a trait is for types or for type constructors, not both"
                );
                self.error(Code::KindMismatch, message, span);
            }
            written.push(match (applied, bare) {
                (Some(_), _) => Some(true),
                (None, Some(_)) => Some(false),
                (None, None) => None,
            });
            if applied.is_some() {
                self.constructor_traits.insert(*key);
            }
        }
        // A trait that does not say is of its supertraits' kind.
        loop {
            let mut changed = false;
            for (i, t) in traits.iter().enumerate() {
                if written[i].is_some() || self.constructor_traits.contains(&keys[i]) {
                    continue;
                }
                let of_constructors = (t.supertraits.iter()).any(|r| {
                    self.named_trait(r.res, r.name)
                        .is_some_and(|sup| self.is_constructor_trait(sup))
                });
                if of_constructors {
                    self.constructor_traits.insert(keys[i]);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        for (i, t) in traits.iter().enumerate() {
            let constructor = self.constructor_traits.contains(&keys[i]);
            for r in &t.supertraits {
                let Some(sup) = self.named_trait(r.res, r.name) else {
                    continue;
                };
                if self.is_constructor_trait(sup) == constructor {
                    continue;
                }
                let sup_name = self.show_trait(sup);
                let message = match constructor {
                    true => format!(
                        "trait '{}' is a trait of type constructors; its supertrait '{sup_name}' \
                         is a trait of types",
                        t.name
                    ),
                    false => format!(
                        "trait '{}' is a trait of types; its supertrait '{sup_name}' is a trait \
                         of type constructors",
                        t.name
                    ),
                };
                self.error(Code::KindMismatch, message, r.span);
            }
            if !constructor {
                continue;
            }
            if !t.params.is_empty() {
                let params: Vec<String> = t.params.iter().map(|p| resolve(*p)).collect();
                let message = format!(
                    "a trait of type constructors has no type parameters: '{}({})'",
                    t.name,
                    params.join(", ")
                );
                self.error(Code::InvalidTraitDeclaration, message, t.name_span);
            }
            for a in &t.assoc_types {
                let message = format!(
                    "a trait of type constructors has no associated types: '{}'",
                    a.name
                );
                self.error(Code::InvalidTraitDeclaration, message, a.span);
            }
            for m in &t.methods {
                if let Some(param) = m.params.first()
                    && writes_self(m)
                    && param.ty.is_none()
                {
                    self.error(
                        Code::InvalidTraitDeclaration,
                        "a trait of type constructors names its parameters: write `xs: Self(a)` \
                         instead of `self`",
                        param.pattern.span,
                    );
                }
            }
        }
    }

    /// Report each supertrait of `t` written with another number of type
    /// arguments than the supertrait has parameters
    /// (`trait Bad: Holds(Int, String)` for `trait Holds(a)`).
    pub(super) fn check_supertrait_arity(&mut self, t: &TraitDecl) {
        for r in &t.supertraits {
            let Some(expected) = self
                .named_trait(r.res, r.name)
                .and_then(|sup| self.tables.traits.get(&sup))
                .map(|info| info.params.len())
            else {
                continue;
            };
            if expected != r.args.len() {
                self.error(
                    Code::ArityMismatch,
                    format!(
                        "trait '{}' expects {expected} {} as a supertrait of '{}', got {}",
                        r.name,
                        inference::plural(expected, "type argument", "type arguments"),
                        t.name,
                        r.args.len()
                    ),
                    r.span,
                );
            }
        }
    }

    /// Shared trait-registration body. Runs for both user-source decls
    /// (after the redefinition guard in `register_trait_decl_user`) and
    /// built-in synthetic decls (via `builtin_trait_decls`).
    pub(super) fn register_trait_decl_inner(&mut self, t: &TraitDecl) {
        let key = self.own_trait(t.name);
        let constructor = self.constructor_traits.contains(&key);
        // The supertraits and the associated types' bounds. A trait name
        // that names no trait is reported here, unless the resolver
        // reported it.
        let mut supertraits: Vec<(TraitKey, Vec<TypeExpr>)> = Vec::new();
        for r in &t.supertraits {
            match self.named_trait(r.res, r.name) {
                Some(sup) => supertraits.push((sup, r.args.clone())),
                None if r.res == Some(crate::defs::Res::Error) => {}
                None => self.error(
                    Code::UnknownTrait,
                    format!("trait '{}' lists unknown supertrait '{}'", t.name, r.name),
                    t.span,
                ),
            }
        }
        let assoc_types: Vec<AssocTypeInfo> = t
            .assoc_types
            .iter()
            .map(|a| AssocTypeInfo {
                name: a.name,
                bounds: a
                    .bounds
                    .iter()
                    .filter_map(|b| Some((self.named_trait(b.res, b.name)?, b.args.clone())))
                    .collect(),
                span: a.span,
            })
            .collect();
        for a in &t.assoc_types {
            for b in &a.bounds {
                if b.res != Some(crate::defs::Res::Error)
                    && self.named_trait(b.res, b.name).is_none()
                {
                    self.error(
                        Code::UnknownTrait,
                        format!(
                            "unknown trait '{}' in bound on associated type '{}::{}'",
                            b.name, t.name, a.name
                        ),
                        a.span,
                    );
                }
            }
        }
        // A method name written twice in a trait declaration is an
        // error, one per duplicate: the two would otherwise disagree
        // (the first wins a method lookup, the second the HashMap-based
        // bookkeeping like `default_method_bodies`).
        {
            let mut seen: std::collections::HashSet<Symbol> = std::collections::HashSet::new();
            for m in &t.methods {
                if !seen.insert(m.name) {
                    self.error(
                        Code::DuplicateDeclaration,
                        format!("duplicate method '{}' in trait '{}'", m.name, t.name),
                        m.span,
                    );
                }
            }
        }

        // Pre-register a stub TraitInfo for `t` carrying just its
        // supertraits and assoc_types. This lets `resolve_type_expr`
        // resolve `Self::AssocName` projections inside method
        // signatures by walking the supertrait chain — e.g. a `Sub`
        // method declaring `Self::Item` (where `Item` lives on
        // `Super`) needs `self.tables.traits[Sub].supertraits = [Super]` to
        // be visible during method-type resolution. The stub is
        // overwritten with the fully-populated TraitInfo at the end
        // of this function.
        let pre_assoc_types = assoc_types.clone();
        let private_to = (!t.is_pub).then_some((self.module, self.module_name));
        let pre_supertraits: Vec<TraitKey> = supertraits.iter().map(|(s, _)| *s).collect();
        self.tables.traits.insert(
            key,
            TraitInfo {
                params: t.params.clone(),
                param_var_ids: Vec::new(),
                supertraits: pre_supertraits,
                supertrait_args: Vec::new(),
                param_where_clauses: Vec::new(),
                methods: Vec::new(),
                receivers: std::collections::HashSet::new(),
                method_bounds: HashMap::new(),
                self_var: 0,
                constructor,
                var_names: Vec::new(),
                default_method_bodies: HashMap::new(),
                assoc_types: pre_assoc_types,
                private_to,
                builtin_needs: HashMap::new(),
                via_needs: HashMap::new(),
            },
        );

        let self_var = self.fresh_var();
        // Allocate a fresh TyVar for each trait-level parameter. These
        // are in scope across every method signature — writing
        // `trait TryInto(b) { fn try_into(self) -> Result(b, Error) }`
        // makes `b` resolve to the same TyVar in the method.
        let trait_param_vars: Vec<(Symbol, Type)> =
            t.params.iter().map(|p| (*p, self.fresh_var())).collect();
        let param_var_ids: Vec<TyVar> = trait_param_vars
            .iter()
            .map(|(_, ty)| match ty {
                Type::Var(v) => *v,
                _ => unreachable!("fresh_var always returns Type::Var"),
            })
            .collect();
        let Type::Var(self_var_id) = self_var else {
            unreachable!("fresh_var always returns Type::Var")
        };
        let self_sym = intern("Self");
        let mut var_names: Vec<(TyVar, Symbol)> = vec![(self_var_id, self_sym)];
        if constructor {
            self.constructor_vars.insert(self_var_id);
            self.tables.constructor_names.insert(self_var_id, self_sym);
        }
        var_names.extend(param_var_ids.iter().copied().zip(t.params.iter().copied()));
        // A trait declares complete signatures: each parameter but `self`
        // is annotated, and a method without a return type returns `()`.
        // An impl's method has the declared type whatever the impl
        // writes, and a default body is checked against it.
        let mut methods: Vec<(Symbol, Type)> = Vec::with_capacity(t.methods.len());
        let mut receivers: std::collections::HashSet<Symbol> = std::collections::HashSet::new();
        let mut method_bounds: HashMap<Symbol, Vec<Pred>> = HashMap::new();
        for m in &t.methods {
            if writes_self(m) {
                receivers.insert(m.name);
            }
            let mut param_map = HashMap::new();
            param_map.insert(self_sym, self_var.clone());
            for (name, ty) in &trait_param_vars {
                param_map.insert(*name, ty.clone());
            }
            let applied = self.check_signature_kinds(m);
            let outer_signature = std::mem::replace(&mut self.resolving_signature, true);
            let mut param_types = Vec::new();
            for param in &m.params {
                let ty = match param.kind {
                    ParamKind::Type => {
                        let name = match &param.pattern.kind {
                            PatternKind::Ident(n) => *n,
                            _ => {
                                unreachable!("parser guarantees `type` params use an Ident pattern")
                            }
                        };
                        let var = self.type_param_var(&mut param_map, name, param.pattern.span);
                        if let Type::Var(v) = var {
                            self.valued_vars.insert(v);
                        }
                        Type::type_of(var)
                    }
                    ParamKind::Data => {
                        if let Some(te) = &param.ty {
                            self.resolve_type_expr(te, &mut param_map)
                        } else if matches!(&param.pattern.kind,
                            PatternKind::Ident(n) if *n == intern("self"))
                        {
                            // Bare `self` is `Self`.
                            self_var.clone()
                        } else {
                            let what = match &param.pattern.kind {
                                PatternKind::Ident(n) => format!("parameter '{n}'"),
                                _ => "a parameter".to_string(),
                            };
                            self.errors.push(
                                Diagnostic::error(
                                    Code::InvalidTraitDeclaration,
                                    param.pattern.span,
                                    format!(
                                        "{what} of trait method '{}.{}' has no type annotation",
                                        t.name, m.name
                                    ),
                                )
                                .with_help(
                                    "a trait declares complete signatures: annotate each \
                                     parameter but `self` (`other: Self`, `x: Int`, `x: a`)",
                                ),
                            );
                            Type::Error
                        }
                    }
                };
                param_types.push(ty);
            }
            let ret_type = match &m.return_type {
                Some(te) => self.resolve_type_expr(te, &mut param_map),
                None => Type::Unit,
            };
            self.resolving_signature = outer_signature;
            self.remember_constructor_labels(applied, &param_map);
            // The type variables the method's annotations introduce.
            let mut own: Vec<(TyVar, Symbol)> = param_map
                .iter()
                .filter_map(|(name, ty)| match ty {
                    // (A row variable, `{x: Int, ...r}`, is kept under
                    // `__row__r`: it is named `r`.)
                    Type::Var(v) if !var_names.iter().any(|(known, _)| known == v) => {
                        let name = match resolve(*name).strip_prefix("__row__") {
                            Some(row) => intern(row),
                            None => *name,
                        };
                        Some((*v, name))
                    }
                    _ => None,
                })
                .collect();
            own.sort_by_key(|(v, _)| *v);
            var_names.extend(own);
            // The method's own bounds. (An unknown trait in one is
            // reported where a body is checked against the method.)
            let mut bounds: Vec<Pred> = Vec::new();
            for wc in &m.where_clauses {
                let (Some(Type::Var(tv)), Some(bound)) = (
                    param_map.get(&wc.type_param).cloned(),
                    self.named_trait(wc.trait_res, wc.trait_name)
                        .filter(|bound| self.tables.traits.contains_key(bound) || *bound == key),
                ) else {
                    continue;
                };
                if !self.bound_fits_kind(
                    &Type::Var(tv),
                    (wc.type_param, true),
                    bound,
                    wc.trait_name_span,
                ) {
                    continue;
                }
                let args: Vec<Type> = wc
                    .trait_args
                    .iter()
                    .map(|te| self.resolve_type_expr(te, &mut param_map))
                    .collect();
                if bound.is_builtin("Decode") {
                    (self.bound_methods).insert(tv, intern(&format!("{}.{}", t.name, m.name)));
                }
                bounds.push(Pred::bound(tv, bound, args));
            }
            // A type variable of the method's result that no parameter
            // has and a bound names: as of a function's
            // (`register_fn_decl`), which impl the bound means would be
            // each caller's to choose.
            let result_only = super::declare_fns::return_only(&param_types, &ret_type);
            let mut written: Vec<(TyVar, Symbol)> = (param_map.iter())
                .filter_map(|(name, ty)| match ty {
                    Type::Var(v) if *v != self_var_id && !param_var_ids.contains(v) => {
                        Some((*v, *name))
                    }
                    _ => None,
                })
                .filter(|(v, _)| result_only.contains(v))
                .filter(|(v, _)| bounds.iter().any(|pred| pred.mentions(*v)))
                .collect();
            written.sort_by_key(|(v, _)| *v);
            for (_, name) in written {
                self.return_only_variable(name, m.span);
            }
            if !bounds.is_empty() {
                method_bounds.insert(m.name, bounds);
            }
            methods.push((m.name, Type::Fun(param_types, Box::new(ret_type))));
        }

        // Collect default-bodied methods. Methods whose `is_signature_only`
        // flag is false carry a real (non-placeholder) body and are eligible
        // to be cloned into impls that omit them.
        let default_method_bodies: HashMap<Symbol, FnDecl> = t
            .methods
            .iter()
            .filter(|m| !m.is_signature_only)
            .map(|m| (m.name, (*m).clone()))
            .collect();

        // Trait-level where bounds on params. Only the (param_name,
        // trait_name) shape is kept; trait_args on the bound are not
        // yet honored (reserved for a future extension where bounds
        // can themselves reference other trait args).
        let param_where_clauses: Vec<(Symbol, TraitKey)> = t
            .param_where_clauses
            .iter()
            .filter_map(|wc| {
                Some((
                    wc.type_param,
                    self.named_trait(wc.trait_res, wc.trait_name)?,
                ))
            })
            .collect();

        let supertrait_names: Vec<TraitKey> = supertraits.iter().map(|(s, _)| *s).collect();
        let supertrait_args: Vec<Vec<TypeExpr>> =
            supertraits.into_iter().map(|(_, args)| args).collect();

        // Reject duplicate assoc-type names within the same trait.
        // Mirrors the duplicate-method check above.
        let mut seen_assoc: std::collections::HashSet<Symbol> = std::collections::HashSet::new();
        for a in &t.assoc_types {
            if !seen_assoc.insert(a.name) {
                self.error(
                    Code::DuplicateDeclaration,
                    format!(
                        "duplicate associated type '{}' in trait '{}'",
                        resolve(a.name),
                        resolve(t.name)
                    ),
                    a.span,
                );
            }
        }
        self.tables.traits.insert(
            key,
            TraitInfo {
                params: t.params.clone(),
                param_var_ids,
                supertraits: supertrait_names,
                supertrait_args,
                param_where_clauses,
                methods,
                receivers,
                method_bounds,
                self_var: self_var_id,
                constructor,
                var_names,
                default_method_bodies,
                assoc_types,
                private_to,
                builtin_needs: HashMap::new(),
                via_needs: HashMap::new(),
            },
        );
        // A generic type among a supertrait's arguments is written with
        // its own (`trait Sub: Super(Box(Int))`).
        if let Some(info) = self.tables.traits.get(&key).cloned() {
            for te in info.supertrait_args.iter().flatten() {
                self.check_supertrait_arg_parametric_arity(te, &info);
            }
        }
    }

    /// The signature a default method's body is checked against, once,
    /// in its trait: the method's declared type with `Self`, the trait's
    /// parameters and the method's own type variables rigid. `Self` is
    /// bounded by the trait (and so by its supertraits), and each
    /// parameter by the trait's `where` clauses: the body may use what
    /// those promise of every implementing type, and nothing else.
    pub(super) fn default_method_sig(&self, trait_name: Symbol, method: Symbol) -> Option<FnSig> {
        // (A redeclared builtin trait is rejected, not registered: it has
        // no entry.)
        let key = self.own_trait(trait_name);
        let info = self.tables.traits.get(&key)?;
        let (_, Type::Fun(params, ret)) = info.methods.iter().find(|(n, _)| *n == method)? else {
            return None;
        };
        let rigid: Vec<RigidId> = info
            .var_names
            .iter()
            .map(|(var, name)| RigidId {
                var: *var,
                name: *name,
            })
            .collect();
        // `Self` implements the trait, at the trait's own parameters.
        let own_args = info.param_var_ids.iter().map(|v| Type::Var(*v)).collect();
        let mut bounds = vec![Pred::bound(info.self_var, key, own_args)];
        for (param, tr) in &info.param_where_clauses {
            if let Some(i) = info.params.iter().position(|p| p == param) {
                bounds.push(Pred::bound(info.param_var_ids[i], *tr, Vec::new()));
            }
        }
        bounds.extend(
            info.method_bounds
                .get(&method)
                .into_iter()
                .flatten()
                .cloned(),
        );
        Some(FnSig {
            params: params.iter().map(|t| rigidify(t, &rigid)).collect(),
            ret: rigidify(ret, &rigid),
            names: rigid.iter().map(|r| (r.name, Type::Rigid(*r))).collect(),
            rigid,
            bounds,
            complete: true,
        })
    }

    // ── Register trait implementations ──────────────────────────────

    /// Convert a type name Symbol to a Type.
    ///
    /// The `Unit`/`()` arm gives a user-declared
    /// `trait T for Unit { ... }` impl a `Type::Unit` self_type
    /// rather than the `Type::Generic("Unit", [])` fallback, which never
    /// unifies with the canonical `Type::Unit` receiver.
    fn type_from_name(ty: TypeRef) -> Type {
        match builtin_type_name(ty) {
            Some("Int") => Type::Int,
            Some("Float") => Type::Float,
            Some("Bool") => Type::Bool,
            Some("String") => Type::String,
            Some("Unit") => Type::Unit,
            _ => Type::Generic(ty, vec![]),
        }
    }

    /// Reject every hand-written impl of `Equal`, `Compare` or `Hash` and
    /// drop it from `decls`. These traits are sealed: every type has
    /// them by its structure, and `==` / `<` never dispatch to an impl,
    /// so a hand-written one could only disagree with them.
    pub(super) fn reject_sealed_trait_impls(&mut self, decls: &mut Vec<Decl>) {
        let mut errors = Vec::new();
        decls.retain(|decl| match decl {
            Decl::TraitImpl(ti)
                if self
                    .impl_trait(ti)
                    .is_some_and(|t| SEALED_TRAIT_NAMES.iter().any(|n| t.is_builtin(n))) =>
            {
                errors.push((ti.trait_name, ti.span));
                false
            }
            _ => true,
        });
        for (trait_name, span) in errors {
            let text = if resolve(trait_name) == "Number" {
                let message = "trait 'Number' cannot be implemented by hand: it is the \
                               types arithmetic is on, Int and Float";
                (message.to_string(), None)
            } else {
                let message = format!(
                    "trait '{trait_name}' cannot be implemented by hand: it is derived \
                     structurally for every type whose fields support it — remove this \
                     impl; Equal, Compare and Hash are derived"
                );
                let help = "to compare, order or hash by something else (a key, a \
                            normalised value), see \"Equality and order idioms\" in \
                            docs/language/traits.md";
                (message, Some(help.to_string()))
            };
            self.error_help(Code::InvalidTraitImpl, text, span);
        }
    }

    /// Whether the impl `ti`, of a trait for the type `target_type`, is
    /// written where an impl may be: in the module that declares its
    /// trait or in the module that declares its type. If not, that is
    /// reported, with the modules it may be written in.
    ///
    /// A builtin trait or type is declared in no module: an impl of
    /// `Display` goes with its type, an impl for `Int` with its trait.
    /// The entries of one REPL session count as one module: each is run
    /// to its end before the next is read.
    ///
    /// What this gives: no two modules can each write the impl, and the
    /// impl's module is initialised before the impl is called from
    /// another module. Why, for the impl of trait `T` for type `Y`,
    /// written in module `I` (`T`'s or `Y`'s):
    ///
    /// - A call that names the impl where it is written (`T.m(y)` with
    ///   `y: Y`), in a module `X`: `X` has `T` and `Y` in its
    ///   types. A type comes into a module's types only through what
    ///   the module imports, and a method of `T` is selected only where
    ///   `T`'s module is imported, directly or through imports
    ///   (`method_entry`). So `X` imports `I`, or is `I`.
    /// - A call decided where the code runs (`T.m(x)` with `x: a where
    ///   a: T`, a default method, the formatter for `Display`), in a
    ///   module that need not import `Y`'s module. If `I` is `T`'s
    ///   module, the calling code names `T`, so its module imports `I`,
    ///   or is `I`. If `I` is `Y`'s module, the call has a value of
    ///   `Y` in hand: one exists only once code of `Y`'s module made
    ///   it, or code of a module that imports it; in the second case
    ///   `I` is complete.
    /// - What is left is `I` calling its own impl, or handing a value
    ///   to code outside that does, while `I` is initialised: the order
    ///   of one module's `let`s, which `init_order` decides (a function
    ///   of another module that is named reaches every impl `I` writes
    ///   for traits it does not declare).
    fn impl_in_its_module(&mut self, ti: &TraitImpl, target_type: TypeRef) -> bool {
        if self.impl_is_here(ti, target_type) {
            return true;
        }
        let Some(defs) = self.defs.clone() else {
            return true;
        };
        let Some(tr) = self.impl_trait(ti) else {
            return true;
        };
        // (An alias by the type it names, `shapes.Box(String)`: the
        // module declares the alias, not the type.)
        let ty = match ti.target_type == target_type.name {
            true => self.impl_target_name(ti, target_type),
            false => {
                self.enter_level();
                let written = self.impl_written_args(ti, false);
                let head = (written.head.as_ref()).filter(|head| self.is_instance(head));
                let ty = match head {
                    Some(head) => self.show_type(&self.with_names(head, &written.names)),
                    None => self.show_type(&Type::Generic(target_type, Vec::new())),
                };
                self.exit_level();
                ty
            }
        };
        let of_trait = defs.get(tr.id.0).module;
        let of_type = defs.get(target_type.id.0).module;
        let named = |module: crate::session::ModuleId| {
            (!module.is_builtin()).then(|| {
                self.tables
                    .module_names
                    .get(&module)
                    .map_or_else(|| "?".to_string(), |name| resolve(*name))
            })
        };
        let tr = self.show_trait(tr);
        let place = match (named(of_trait), named(of_type)) {
            (Some(t), Some(y)) if t == y => {
                format!("it may be written in module '{t}', which declares both")
            }
            (Some(t), Some(y)) => format!(
                "it may be written in module '{t}', which declares the trait, \
                 or in module '{y}', which declares the type"
            ),
            (Some(t), None) => {
                format!("it may be written in module '{t}', which declares the trait")
            }
            (None, Some(y)) => {
                format!("it may be written in module '{y}', which declares the type")
            }
            (None, None) => "both are builtin, so no module may write it".to_string(),
        };
        self.error(
            Code::ImplElsewhere,
            format!(
                "the impl of trait '{tr}' for type '{ty}' is written in module '{}', which \
                 declares neither the trait nor the type; {place}",
                resolve(self.module_name),
            ),
            ti.span,
        );
        false
    }

    /// Whether both the trait `tr` and the type `ty` are builtin.
    fn trait_and_type_are_builtin(&self, tr: TraitKey, ty: TypeRef) -> bool {
        self.defs.as_ref().is_some_and(|defs| {
            defs.get(tr.id.0).module.is_builtin() && defs.get(ty.id.0).module.is_builtin()
        })
    }

    /// The trait the impl `ti` implements, as messages name it: as a
    /// program writes it, a trait of another module with its module
    /// (`json.Encode`, `traits.Area`, whatever the impl wrote).
    fn impl_trait_name(&self, ti: &TraitImpl) -> String {
        self.impl_trait(ti)
            .map_or_else(|| resolve(ti.trait_name), |tr| self.show_trait(tr))
    }

    /// The target of the impl `ti`, for the type `target_type`, as
    /// messages name it: as the impl writes it (an alias of the type
    /// keeps its name), a type of another module with its module
    /// (`time.Date`, `shapes.Square`).
    fn impl_target_name(&self, ti: &TraitImpl, target_type: TypeRef) -> String {
        let head = match self.other_module(target_type) {
            Some(module) if ti.target_type == target_type.name => {
                format!("{module}.{}", ti.target_type)
            }
            _ => ti.target_type.to_string(),
        };
        // A type constructor, with its arguments (`Result(_, e)`).
        match ti.target_type_args.iter().any(is_hole) {
            true => {
                let args: Vec<String> = ti.target_type_args.iter().map(type_expr_text).collect();
                format!("{head}({})", args.join(", "))
            }
            false => head,
        }
    }

    /// Whether the header of the impl `ti` of `tr`, for the type
    /// `written`, fits the trait's kind: an impl of a trait of type
    /// constructors writes `_` for its type's first type argument
    /// (`trait Functor for Result(_, e)`), an impl of a trait of types
    /// names every type argument. What does not fit is reported.
    fn impl_header_fits(&mut self, ti: &TraitImpl, tr: TraitKey, written: TypeRef) -> bool {
        let holes: Vec<usize> = (ti.target_type_args.iter().enumerate())
            .filter(|(_, arg)| is_hole(arg))
            .map(|(i, _)| i)
            .collect();
        let trait_name = self.impl_trait_name(ti);
        let head = resolve(ti.target_type);
        let arity = self.type_arity(written).or_else(|| {
            builtin_type_name(written)
                .and_then(crate::types::builtins::lookup)
                .and_then(|b| b.arity)
                .map(|a| a as usize)
        });
        // (A type of no type arguments is no constructor, whatever the
        // trait: `trait Greet for Int(_)` as `trait Functor for Int(_)`.)
        let constructor = self.is_constructor_trait(tr);
        if arity == Some(0) && (constructor || !holes.is_empty()) {
            let message =
                format!("type '{head}' takes no type arguments, so it is not a type constructor");
            self.error(Code::KindMismatch, message, ti.header_span);
            return false;
        }
        if !constructor {
            if ti.target_type_args.is_empty() {
                let shown = Qualifier::written(ti.target_module, ti.target_type);
                return !self.bare_generic(&shown, written, ti.target_type_span);
            }
            if holes.is_empty() {
                return true;
            }
            // Each `_` a name the header does not use yet.
            let mut letters = ('a'..='z')
                .map(|c| c.to_string())
                .filter(|name| !ti.target_param_names.iter().any(|p| resolve(*p) == *name));
            let args: Vec<String> = (ti.target_type_args.iter())
                .map(|arg| match is_hole(arg) {
                    true => letters.next().unwrap_or_else(|| "_".to_string()),
                    false => type_expr_text(arg),
                })
                .collect();
            let message = format!(
                "trait '{trait_name}' is for types, not type constructors: name every type argument"
            );
            let help = format!("write `trait {trait_name} for {head}({})`", args.join(", "));
            self.error_help(Code::KindMismatch, (message, Some(help)), ti.header_span);
            return false;
        }
        match holes.as_slice() {
            [0] => true,
            // No `_`, or one more after the first: the header with `_`
            // first and names after it.
            [] | [0, ..] => {
                // The header with `_` first: the arguments it writes after
                // the first (a name for a second `_`), else the type's
                // parameters.
                let mut fresh = ('b'..='z')
                    .map(|c| c.to_string())
                    .filter(|name| !ti.target_param_names.iter().any(|p| resolve(*p) == *name));
                let rest: Vec<String> = match ti.target_type_args.split_first() {
                    Some((_, rest)) => (rest.iter())
                        .map(|arg| match is_hole(arg) {
                            true => fresh.next().unwrap_or_else(|| "_".to_string()),
                            false => type_expr_text(arg),
                        })
                        .collect(),
                    None => {
                        let declared: Vec<String> = (self.tables.enums.get(&written))
                            .map(|e| e.params.iter().skip(1).map(|p| resolve(*p)).collect())
                            .unwrap_or_default();
                        let mut letters = ('b'..='z').map(|c| c.to_string());
                        (1..arity.unwrap_or(1))
                            .map(|i| match declared.get(i - 1) {
                                Some(name) => name.clone(),
                                None => letters.next().unwrap_or_else(|| "_".to_string()),
                            })
                            .collect()
                    }
                };
                let args: Vec<String> = std::iter::once("_".to_string()).chain(rest).collect();
                let message = format!(
                    "trait '{trait_name}' is for type constructors: its impl writes '_' for the \
                     first type argument"
                );
                let help = format!("write `trait {trait_name} for {head}({})`", args.join(", "));
                self.error_help(Code::KindMismatch, (message, Some(help)), ti.header_span);
                false
            }
            _ => {
                let message = format!(
                    "'_' must be the first type argument of '{head}': a type constructor stands \
                     for its first type argument"
                );
                let help = match builtin_type_name(written) {
                    Some("Result") => {
                        Some("to change the error of a Result, use `result.map_err`".to_string())
                    }
                    Some(_) => None,
                    None => Some(format!(
                        "to map another argument of '{head}', declare it first in '{head}'"
                    )),
                };
                self.error_help(Code::KindMismatch, (message, help), ti.header_span);
                false
            }
        }
    }

    /// Whether the impl `ti`, for the type `target_type`, is written
    /// where it may be: in the module of its trait or of its type (the
    /// rule `impl_in_its_module` reports). An impl elsewhere is no impl:
    /// nothing of it is registered, and nothing may count on it.
    pub(super) fn impl_is_here(&self, ti: &TraitImpl, target_type: TypeRef) -> bool {
        let Some(defs) = &self.defs else {
            return true;
        };
        // An unknown trait is reported elsewhere.
        let Some(tr) = self.impl_trait(ti) else {
            return true;
        };
        let here = |module: crate::session::ModuleId| {
            module == self.module || (self.is_cell && self.cells.contains(&module))
        };
        here(defs.get(tr.id.0).module) || here(defs.get(target_type.id.0).module)
    }

    /// Register the impl `ti`: its methods for its type, with the types
    /// its trait declares, and what its bodies are checked against. (One
    /// level deep, as every declaration: its variables are a
    /// declaration's, not an outer value's.)
    pub(super) fn register_trait_impl(&mut self, ti: &TraitImpl) {
        self.enter_level();
        self.declare_trait_impl(ti);
        self.exit_level();
    }

    fn declare_trait_impl(&mut self, ti: &TraitImpl) {
        // An impl of a trait or for a type the resolver resolved to
        // nothing: it reported why.
        if ti.trait_res == Some(crate::defs::Res::Error)
            || ti.target_res == Some(crate::defs::Res::Error)
        {
            self.unresolved_impl_methods
                .extend(ti.methods.iter().map(|m| m.name));
            return;
        }
        // Reject trait impls whose target type was never
        // declared: `trait Greet for Widget { ... }` with no `type
        // Widget` would attach methods to a phantom type. A lowercase
        // target (`trait Display for a { ... }`) names a type variable,
        // not a type: an impl is for a declared type, never for every
        // type.
        if self.names_rejected(ti.target_res, ti.target_type) {
            return;
        }
        let Some(written) = self.named_type(ti.target_res, ti.target_type) else {
            let name_str = resolve(ti.target_type);
            self.error(
                Code::UnknownType,
                format!("trait impl target '{name_str}' is not a declared type"),
                ti.span,
            );
            self.unresolved_impl_methods
                .extend(ti.methods.iter().map(|m| m.name));
            if name_str.starts_with(char::is_lowercase)
                && let Some(tr) = self.impl_trait(ti)
            {
                self.blanket_traits.insert(tr);
            }
            return;
        };
        let target_type = canonical_head(&self.tables.resolver, written);
        let Some(trait_key) = self.impl_trait(ti) else {
            self.error(
                Code::UnknownTrait,
                format!("trait '{}' is not declared", ti.trait_name),
                ti.span,
            );
            return;
        };
        if !self.impl_header_fits(ti, trait_key, written) {
            self.unresolved_impl_methods
                .extend(ti.methods.iter().map(|m| m.name));
            return;
        }
        let args_key = self.impl_args_key_of(ti);
        let impl_key = (trait_key, target_type, args_key);

        // Coherence check: one impl for a trait, a type and its trait
        // arguments: two whose trait arguments could be one are two impls
        // one use could mean.
        // An impl whose trait arguments name an unknown type is that
        // error and no impl.
        let header = self.impl_written_args(ti, false);
        if header.args.iter().any(Type::holds_error) {
            self.impl_written_args(ti, true);
            self.duplicated_impls.insert((trait_key, target_type));
            return;
        }
        if Self::reached_at_any_depth(trait_key)
            && !trait_key.is_builtin("Decode")
            && let Some(head) = (header.head.as_ref()).filter(|head| self.is_instance(head))
        {
            self.instance_impl_reached_by_type(ti, trait_key, target_type, head, &header);
            self.duplicated_impls.insert((trait_key, target_type));
            return;
        }
        // A builtin trait's impl for a builtin type is no module's to
        // write, though silt has one (`trait Functor for List(_)`): that
        // is the error, not a duplicate of silt's.
        if self.trait_and_type_are_builtin(trait_key, target_type)
            && !self.impl_in_its_module(ti, target_type)
        {
            return;
        }
        let met = self.impl_met(ti, trait_key, target_type, args_key);
        if let Some(met) = met {
            self.duplicate_impl(ti, trait_key, target_type, args_key, met);
            self.duplicated_impls.insert((trait_key, target_type));
            return;
        }
        (self.impl_written).insert(impl_key, header);
        if self
            .tables
            .trait_impl_set
            .contains(&(trait_key, target_type))
            && self.tables.impl_args_of(trait_key, target_type).is_empty()
        {
            // A builtin type is stamped with the structural traits it
            // has: that is no impl. (Where an impl of one may be
            // written is `impl_in_its_module`'s to say.)
            let structural_method = STRUCTURAL_METHODS
                .iter()
                .find(|(name, _)| trait_key.is_builtin(name))
                .map(|(_, method)| intern(method));
            let stamp = structural_method.is_some_and(|method| {
                self.tables
                    .impl_methods
                    .get(target_type, method, trait_key)
                    .is_none()
            });
            if !stamp {
                self.error(
                    Code::DuplicateDeclaration,
                    format!(
                        "duplicate implementation of trait '{}' for type '{}'",
                        self.impl_trait_name(ti),
                        self.impl_target_name(ti, target_type)
                    ),
                    ti.span,
                );
                return;
            }
        }

        // Nothing of an impl written elsewhere is registered: no call
        // goes through it.
        if !self.impl_in_its_module(ti, target_type) {
            return;
        }
        // Build the impl-level parameter map. For a parameterized target
        // like `trait X for Box(a)`, each lowercase binder in
        // `target_param_names` becomes a fresh type variable that is
        // shared across every method in the impl — so `fn get(self) -> a`
        // and `fn put(self, x: a)` in the same impl refer to the SAME
        // type variable, mirroring the fn-signature convention.
        let mut impl_param_map: HashMap<Symbol, Type> = HashMap::new();
        for &param_name in &ti.target_param_names {
            impl_param_map.insert(param_name, self.fresh_var());
        }
        let self_type = self.impl_self_type(ti, written, target_type, &mut impl_param_map);

        // (The same of a trait that has `json.Decode` among its
        // supertraits, for a generic type, of the program or a builtin
        // container, over type variables (`Box(a)`, `List(a)`): its impl
        // would be a way to decode the type, of whatever `a` is, unless
        // its header asks `json.Decode` of each of them: its dictionary
        // then carries the type in full, `Box(Int)`, which the decoder
        // builds. An instance, `Box(Int)`, is the decoder's to judge
        // (`decode_impl_refusal`).)
        let each_decoded = (ti.target_param_names.iter()).all(|param| {
            (ti.where_clauses.iter()).any(|wc| {
                wc.type_param == *param
                    && self
                        .named_trait(wc.trait_res, wc.trait_name)
                        .is_some_and(Self::is_decode)
            })
        });
        if self.generic_for_decode(target_type)
            && self.implies_decode(trait_key)
            && !free_vars_in(&self.apply(&self_type)).is_empty()
            && !each_decoded
        {
            self.error(
                Code::InvalidTraitImpl,
                format!(
                    "trait `{}` cannot be implemented for the generic type `{}`: it has \
                     `json.Decode` as a supertrait, and a decoder builds a value of one type",
                    self.impl_trait_name(ti),
                    target_type.name
                ),
                ti.span,
            );
            return;
        }

        self.tables.add_impl(impl_key);
        // Record the impl block's real span for validate_trait_impls
        // to use when reporting missing-method diagnostics.
        self.tables.trait_impl_spans.insert(impl_key, ti.span);

        // Record the impl's full self type under the canonical head key so
        // `verify_trait_obligation` can compare an obligated type's
        // positional args against the impl's — closing the alias-expansion
        // soundness hole where `trait Total for Bytes2` (with
        // `type Bytes2 = List(Int)`) satisfied a where-bound for ANY
        // `List(T)`. Overwrites are fine: coherence rejects duplicate user
        // impls above, and the one permitted overwrite (a user Display
        // impl in the place of the structural one) should win here too.
        self.tables
            .impl_self_types
            .insert(impl_key, self_type.clone());
        // The names the header gives the impl's type variables, for a
        // message about one (`validate_trait_impls`).
        self.impl_var_names.insert(
            impl_key,
            (impl_param_map.iter())
                .filter_map(|(name, ty)| match self.apply(ty) {
                    Type::Var(v) => Some((v, *name)),
                    _ => None,
                })
                .collect(),
        );

        // Resolve impl-level where clauses (e.g. `trait X for Box(a) where
        // a: Show`) to `(TyVar, trait)` pairs against the impl_param_map.
        // These apply to every method in the impl: they are the bounds
        // in scope in each method's body (its `FnSig`), and each call of
        // a method owes them (its MethodEntry).
        //
        // Multi-trait bounds (`where a: Show + Hash`) arrive pre-flattened
        // from parse_where_clauses_opt as separate (tv, trait) entries
        // sharing a type_var, so the resolution loop handles both forms
        // with a single path.
        let mut impl_level_constraints: Vec<Pred> = Vec::new();
        // (The trait arguments' variables an impl-level bound was refused
        // on: `trait Conv(a) for W where a: Def`.)
        let mut refused_trait_arg_bounds = Vec::new();
        let impl_trait = trait_key;
        for wc in &ti.where_clauses {
            let unknown = wc.trait_res == Some(crate::defs::Res::Error)
                || self
                    .named_trait(wc.trait_res, wc.trait_name)
                    .is_none_or(|t| !self.tables.traits.contains_key(&t));
            if unknown && let Some(Type::Var(tv)) = impl_param_map.get(&wc.type_param) {
                self.unknown_bounds.insert(*tv);
            }
            // A bound the resolver resolved to nothing: it reported why.
            if wc.trait_res == Some(crate::defs::Res::Error) {
                continue;
            }
            let type_param = &wc.type_param;
            let trait_args = &wc.trait_args;
            let Some(trait_key) = self
                .named_trait(wc.trait_res, wc.trait_name)
                .filter(|t| self.tables.traits.contains_key(t))
            else {
                self.error(
                    Code::UnknownTrait,
                    format!(
                        "unknown trait '{}' in where clause on trait impl '{} for {}'",
                        resolve(wc.trait_name),
                        self.impl_trait_name(ti),
                        resolve(ti.target_type)
                    ),
                    ti.span,
                );
                continue;
            };
            let trait_name = &trait_key;
            // The runtime runs an impl of a builtin trait found by the
            // value (println shows a value, json.stringify encodes it)
            // with no caller to pass what a bound of a program's trait
            // needs: such an impl asks only builtin traits.
            if !super::evidence::has_dictionary(impl_trait)
                && super::evidence::has_dictionary(*trait_name)
            {
                let shown = self.show_trait(impl_trait);
                let message = format!(
                    "an impl of '{shown}' can ask only 'Display', 'Equal', 'Compare', 'Hash', \
                     'json.Encode', 'json.Decode' or 'Error' of its type's variables: the runtime \
                     runs it with nothing else"
                );
                // (Each help points at what the impl can do instead.)
                let help = [
                    (
                        "Display",
                        "show the part with `{x}` and bound it by Display",
                    ),
                    (
                        "Encode",
                        "encode the part with `json.encode(x)` and bound it by json.Encode",
                    ),
                    (
                        "Decode",
                        "decode the part with `json.decode` and bound it by json.Decode",
                    ),
                    ("Error", "show the part with `{x}` and bound it by Display"),
                ]
                .iter()
                .find(|(tr, _)| impl_trait.is_builtin(tr))
                .map(|(_, help)| help.to_string());
                self.error_help(Code::InvalidTraitImpl, (message, help), ti.span);
                // (What the body asks of the variable is not reported
                // again.)
                if let Some(Type::Var(tv)) = impl_param_map.get(type_param) {
                    self.unknown_bounds.insert(*tv);
                }
                continue;
            }
            // The bound's arity must match the trait's
            // declared param count in BOTH directions (see
            // check_where_bound_arity). Skip registering the malformed
            // bound — a length-mismatched arg list would sail past
            // verify_trait_obligation's equal-length zip guard and
            // degrade to a bare "implements the trait" check.
            if !self.check_where_bound_arity(*trait_name, trait_args.len(), ti.span) {
                continue;
            }
            // Resolve the bound's trait args through the impl's
            // param_map so any lowercase tyvars from the impl header
            // bind to the impl's fresh tyvars. For concrete args
            // (e.g. `where a: Conv(Int)`) this is the identity walk.
            // Empty when the bound trait has no args.
            let resolved_bound_args: Vec<Type> = trait_args
                .iter()
                .map(|te| self.resolve_type_expr(te, &mut impl_param_map))
                .collect();
            let names_it = |te: &TypeExpr| {
                let mut vars = Vec::new();
                super::names::type_vars_of(te, &mut vars);
                vars.contains(type_param)
            };
            match impl_param_map.get(type_param) {
                Some(ty) => {
                    let resolved = self.apply(ty);
                    if !self.bound_fits_kind(
                        &resolved,
                        (*type_param, false),
                        *trait_name,
                        wc.trait_name_span,
                    ) {
                        if let Type::Var(tv) = resolved {
                            self.unknown_bounds.insert(tv);
                        }
                        continue;
                    }
                    if let Type::Var(tv) = resolved {
                        impl_level_constraints.push(Pred::bound(
                            tv,
                            *trait_name,
                            resolved_bound_args.clone(),
                        ));
                    }
                    // If resolved is concrete (shouldn't happen — impl_param_map
                    // only inserts fresh Var entries) treat it as a tautology
                    // and register no positional obligation.
                }
                // A trait argument of the impl (`trait Conv(a) for W`): its
                // bound goes on the method that needs it. (What the body
                // asks of the variable is not reported again.)
                None if ti.trait_args.iter().any(names_it) => {
                    let method = ti.methods.iter().find(|m| {
                        (m.params.iter().filter_map(|p| p.ty.as_ref())).any(names_it)
                            || m.return_type.as_ref().is_some_and(names_it)
                    });
                    let example = match method {
                        Some(m) => format!(
                            ": `fn {}(...) where {}: {}`",
                            m.name,
                            resolve(*type_param),
                            self.show_bound(*trait_name, &resolved_bound_args)
                        ),
                        None => String::new(),
                    };
                    self.error(
                        Code::InvalidTypeAnnotation,
                        format!(
                            "type variable '{}' in impl-level where clause is a trait argument, \
                             not a target type argument; write the bound on the method that needs it{example}",
                            resolve(*type_param),
                        ),
                        ti.span,
                    );
                    refused_trait_arg_bounds.push(*type_param);
                }
                None => {
                    self.error(Code::InvalidTypeAnnotation,
                        format!(
                            "type variable '{}' in impl-level where clause is not declared in the target type arguments; \
                             declare it as a target parameter: `trait {} for {}({}, ...)`",
                            resolve(*type_param),
                            self.impl_trait_name(ti),
                            resolve(ti.target_type),
                            resolve(*type_param)
                        ),
                        ti.span,
                    );
                }
            }
        }
        if !impl_level_constraints.is_empty() {
            self.tables
                .impl_preds
                .insert(impl_key, impl_level_constraints.clone());
        }

        // A method of the impl whose name the trait does not declare is
        // rejected, and so is each second (and later) method of one
        // name in the impl: neither enters the method_table.
        // Validate trait_args against the trait's declared parameter
        // count. `trait Foo(a, b)` must be implemented as `trait Foo(X, Y) for T`;
        // a parameterless trait must be `trait Foo for T` (no args).
        // When the count matches and the trait declared param-level
        // where bounds, verify that each supplied arg satisfies the
        // declared bounds — concrete types are checked now via
        // `verify_trait_obligation`; unresolved tyvars are added as
        // impl-level constraints so body checking sees them.
        let trait_info_clone = self.tables.traits.get(&trait_key).cloned();
        if let Some(trait_info) = &trait_info_clone {
            if ti.trait_args.len() != trait_info.params.len() {
                let expected = trait_info.params.len();
                self.error(
                    Code::ArityMismatch,
                    format!(
                        "trait '{}' expects {} {}, got {} in impl for '{}'",
                        self.impl_trait_name(ti),
                        expected,
                        inference::plural(expected, "type argument", "type arguments"),
                        ti.trait_args.len(),
                        self.impl_target_name(ti, target_type),
                    ),
                    ti.span,
                );
            } else if !ti.trait_args.is_empty() {
                // Resolve each supplied trait arg through impl_param_map
                // so lowercase names bind to the same fresh tyvars used
                // by the impl's methods. Stash for later verification by
                // `verify_trait_obligation` (closes the trait-args
                // soundness hole: `where a: TryInto(Int)` against a
                // `trait TryInto(Float) for String` impl must now reject).
                let resolved_trait_args: Vec<Type> = ti
                    .trait_args
                    .iter()
                    .map(|te| self.resolve_type_expr(te, &mut impl_param_map))
                    .collect();
                self.tables
                    .impl_trait_args
                    .insert(impl_key, resolved_trait_args.clone());
                for name in &refused_trait_arg_bounds {
                    if let Some(Type::Var(tv)) = impl_param_map.get(name) {
                        self.unknown_bounds.insert(*tv);
                    }
                }
                if !trait_info.param_where_clauses.is_empty() {
                    for (param_name, bound_trait) in &trait_info.param_where_clauses {
                        let Some(idx) = trait_info.params.iter().position(|p| p == param_name)
                        else {
                            continue;
                        };
                        let Some(arg_ty) = resolved_trait_args.get(idx) else {
                            continue;
                        };
                        let applied = self.apply(arg_ty);
                        match &applied {
                            Type::Var(_) => {
                                // Deferred — the impl's own where clause path
                                // will propagate it; skip here.
                            }
                            _ => {
                                // Parameterless sub-bound: `trait Foo(a) where a: Display`
                                // — no args to thread.
                                self.verify_declared(*bound_trait, &[], &applied, ti.span);
                            }
                        }
                    }
                }
            }
        }

        // ── Associated-type bindings ───────────────────────────────
        //
        // For each `type Name = T` in the impl:
        //   - reject duplicate bindings;
        //   - resolve the bound type through impl_param_map (so impl-
        //     level type-vars are visible);
        //   - register the binding into the canonical assoc-binding
        //     registry under (trait_name, target_canonical_head, name)
        //     so the canonicaliser can reduce `<T as Trait>::Name`.
        // After processing the impl-supplied bindings, verify that
        // every assoc-type the trait declares has a binding here, and
        // that each binding satisfies the trait's declared bounds.
        let trait_assoc_types: Vec<AssocTypeInfo> = self
            .tables
            .traits
            .get(&trait_key)
            .map(|info| info.assoc_types.clone())
            .unwrap_or_default();
        // Index the impl's bindings by name for lookup + duplicate check.
        let mut impl_binding_map: HashMap<Symbol, Type> = HashMap::new();
        let mut seen_binding: std::collections::HashSet<Symbol> = std::collections::HashSet::new();
        for binding in &ti.assoc_type_bindings {
            if !seen_binding.insert(binding.name) {
                self.error(
                    Code::DuplicateDeclaration,
                    format!(
                        "duplicate associated-type binding '{}' in impl of '{}' for '{}'",
                        resolve(binding.name),
                        self.impl_trait_name(ti),
                        self.impl_target_name(ti, target_type),
                    ),
                    binding.span,
                );
                continue;
            }
            // Reject bindings whose name is not declared in the trait.
            // (Trait info may be missing if the trait itself was
            // unknown — in that case the per-impl unknown-trait error
            // already fired and we silently skip the binding.)
            let known = trait_assoc_types.iter().any(|a| a.name == binding.name);
            if (!trait_assoc_types.is_empty() || self.tables.traits.contains_key(&trait_key))
                && self.tables.traits.contains_key(&trait_key)
                && !known
            {
                self.error(
                    Code::InvalidTraitImpl,
                    format!(
                        "associated type '{}' is not declared in trait '{}'",
                        resolve(binding.name),
                        self.impl_trait_name(ti),
                    ),
                    binding.span,
                );
                continue;
            }
            let resolved = self.resolve_type_expr(&binding.ty, &mut impl_param_map);
            // Register into the canonical assoc-binding registry. The
            // registry keys on the canonical target head (so an alias
            // and its target collapse), parallel to method_table's
            // canonical_head routing above.
            // The registry refuses self- or mutually-referential
            // bindings that would otherwise drive `canonicalize` into
            // infinite recursion (stack overflow). Skip the
            // `impl_binding_map` insertion on cycle so the per-method
            // signature check below reports a clean "missing required
            // assoc-type" rather than walking through a poisoned entry.
            match self.tables.resolver.register_assoc_binding(
                trait_key,
                target_type,
                binding.name,
                resolved.clone(),
                &self_type,
            ) {
                Ok(()) => {
                    impl_binding_map.insert(binding.name, resolved);
                }
                Err(cycle) => {
                    let via_msg = if (cycle.via.0, cycle.via.1, cycle.via.2.as_str())
                        == (cycle.trait_name, cycle.head, cycle.assoc_name.as_str())
                    {
                        String::new()
                    } else {
                        format!(
                            " (cycle via <{} as {}>::{})",
                            cycle.via.1, cycle.via.0, cycle.via.2
                        )
                    };
                    self.error(Code::InvalidTraitImpl,
                        format!(
                            "associated type binding for <{} as {}>::{} is self-referential{via_msg}",
                            cycle.head, cycle.trait_name, cycle.assoc_name,
                        ),
                        binding.span,
                    );
                }
            }
        }
        // Verify each declared assoc-type has a binding and that the
        // binding satisfies any declared bounds.
        for assoc in &trait_assoc_types {
            let Some(bound_ty) = impl_binding_map.get(&assoc.name) else {
                self.error(
                    Code::InvalidTraitImpl,
                    format!(
                        "impl of '{}' for '{}' is missing required associated type '{}'",
                        self.impl_trait_name(ti),
                        self.impl_target_name(ti, target_type),
                        resolve(assoc.name),
                    ),
                    ti.span,
                );
                continue;
            };
            let applied = self.apply(bound_ty);
            for (bound_trait, bound_args) in &assoc.bounds {
                if !self.tables.traits.contains_key(bound_trait) {
                    self.error(
                        Code::UnknownTrait,
                        format!(
                            "unknown trait '{}' in bound on associated type '{}::{}'",
                            resolve(bound_trait.name),
                            self.impl_trait_name(ti),
                            resolve(assoc.name),
                        ),
                        assoc.span,
                    );
                    continue;
                }
                // Resolve bound trait args at the impl site so any
                // lowercase tyvars from the impl are visible.
                let resolved_bound_args: Vec<Type> = bound_args
                    .iter()
                    .map(|te| self.resolve_type_expr(te, &mut impl_param_map))
                    .collect();
                // (A binding that is one of the impl's own variables,
                // `type Item = a`, says nothing here: the impl's uses
                // owe what the variable needs.)
                self.verify_declared(*bound_trait, &resolved_bound_args, &applied, assoc.span);
            }
        }

        let trait_method_names: Option<std::collections::HashSet<Symbol>> = self
            .tables
            .traits
            .get(&trait_key)
            .map(|info| info.methods.iter().map(|(n, _)| *n).collect());
        let mut seen_impl_methods: std::collections::HashSet<Symbol> =
            std::collections::HashSet::new();
        for method in &ti.methods {
            if !seen_impl_methods.insert(method.name) {
                self.error(
                    Code::DuplicateDeclaration,
                    format!(
                        "duplicate method '{}' in trait impl '{} for {}'",
                        method.name,
                        self.impl_trait_name(ti),
                        ti.target_type
                    ),
                    method.span,
                );
            }
            if let Some(names) = &trait_method_names
                && !names.contains(&method.name)
            {
                self.error(
                    Code::InvalidTraitImpl,
                    format!(
                        "method '{}' is not declared in trait '{}'",
                        method.name,
                        self.impl_trait_name(ti)
                    ),
                    method.span,
                );
            }
        }

        // ── The methods ─────────────────────────────────────────────
        //
        // An impl's method has the type its trait declares, with the
        // impl's type for `Self` and the impl's trait arguments for the
        // trait's parameters. What the impl writes (annotations, or
        // none) must agree with it; the body is checked against it.
        //
        // In the bodies the type variables of the impl's header are
        // rigid.
        let mut rigid: Vec<RigidId> = Vec::new();
        let mut impl_names: HashMap<Symbol, Type> = HashMap::new();
        for (name, ty) in &impl_param_map {
            if let Type::Var(var) = ty {
                let id = RigidId {
                    var: *var,
                    name: match resolve(*name).strip_prefix("__row__") {
                        Some(row) => intern(row),
                        None => *name,
                    },
                };
                rigid.push(id);
                impl_names.insert(*name, Type::Rigid(id));
            }
        }
        rigid.sort_by_key(|r| r.var);
        // The type variables of a target written without its arguments
        // (`trait T for Box`, `trait T for List`) are rigid as well: the
        // impl is for every `Box(a)`. They take the first free letters.
        let mut letters = ('a'..='z').map(|c| intern(&c.to_string()));
        for var in free_vars_in(&self.apply(&self_type)) {
            if rigid.iter().any(|r| r.var == var) {
                continue;
            }
            // (The self type of an impl for every function or tuple
            // is shown by the target's name.)
            let name = match self.shape_vars.contains_key(&var) {
                true => target_type.name,
                false => letters
                    .by_ref()
                    .find(|name| !impl_param_map.contains_key(name))
                    .unwrap_or_else(|| intern("_")),
            };
            rigid.push(RigidId { var, name });
        }
        let body_self = rigidify(&self_type, &rigid);
        impl_names.insert(intern("Self"), body_self.clone());
        let trait_info = trait_info_clone;
        let mut seed: HashMap<TyVar, Type> = HashMap::new();
        if let Some(info) = &trait_info {
            seed.insert(info.self_var, self_type.clone());
            let args = self
                .tables
                .impl_trait_args
                .get(&impl_key)
                .cloned()
                .unwrap_or_default();
            if args.len() == info.param_var_ids.len() {
                seed.extend(info.param_var_ids.iter().copied().zip(args));
            }
        }

        let self_sym = intern("self");
        for method in &ti.methods {
            // The declared type, for this impl. The method's own type
            // variables are new ones for this impl, rigid in its body
            // under the names the trait wrote.
            let mut method_rigid = rigid.clone();
            let declared = trait_info.as_ref().and_then(|info| {
                let (_, ty) = info.methods.iter().find(|(n, _)| *n == method.name)?;
                Some((info, ty))
            });
            // The bounds the trait declares for the method, on this
            // impl's variables.
            let mut declared_bounds: Vec<Pred> = Vec::new();
            let seeded: Option<Type> = declared.map(|(info, ty)| {
                let mut mapping = seed.clone();
                for v in free_vars_in(ty) {
                    if mapping.contains_key(&v) {
                        continue;
                    }
                    let (fresh, fresh_id) = self.fresh_tv();
                    mapping.insert(v, fresh);
                    if let Some((_, name)) = info.var_names.iter().find(|(known, _)| *known == v) {
                        method_rigid.push(RigidId {
                            var: fresh_id,
                            name: *name,
                        });
                    }
                }
                declared_bounds = Self::bounds_under(info, method.name, &mapping);
                // A `json.Decode` bound asks what this body asks of its
                // variable; a note names the method `Src.load`.
                let passer = intern(&format!("{target_type}.{}", method.name));
                for pred in &mut declared_bounds {
                    if let Pred::Trait {
                        subject: Type::Var(var),
                        asker: Some(asker),
                        ..
                    } = pred
                    {
                        if let Asked::Bound(declared) = asker.by {
                            self.bound_impls.entry(declared).or_default().push(*var);
                        }
                        *asker = Asker {
                            by: Asked::Bound(*var),
                            passer: Some(passer),
                        };
                    }
                }
                substitute_vars(ty, &mapping)
            });
            let has_declared_type = seeded.is_some();
            // Whether the trait declares the method with a receiver.
            let declared_receiver = declared.map(|(info, _)| info.receivers.contains(&method.name));
            let expected = seeded.as_ref().map(|ty| rigidify(ty, &method_rigid));
            let (expected_params, expected_ret) = match &expected {
                Some(Type::Fun(params, ret)) => (params.clone(), Some((**ret).clone())),
                _ => (Vec::new(), None),
            };

            // What the impl writes. A part it leaves out is the declared
            // one.
            let applied = self.check_signature_kinds(method);
            let outer_signature = std::mem::replace(&mut self.resolving_signature, true);
            let mut param_map = impl_names.clone();
            let mut param_types = Vec::new();
            for (i, param) in method.params.iter().enumerate() {
                let ty = match param.kind {
                    ParamKind::Type => {
                        let name = match &param.pattern.kind {
                            PatternKind::Ident(n) => *n,
                            _ => {
                                unreachable!("parser guarantees `type` params use an Ident pattern")
                            }
                        };
                        let var = self.type_param_var(&mut param_map, name, param.pattern.span);
                        if let Type::Var(v) = var {
                            self.valued_vars.insert(v);
                        }
                        Type::type_of(var)
                    }
                    ParamKind::Data => {
                        if let Some(te) = &param.ty {
                            self.resolve_type_expr(te, &mut param_map)
                        } else if let Some(declared) = expected_params.get(i) {
                            declared.clone()
                        } else if i == 0
                            && matches!(&param.pattern.kind, PatternKind::Ident(n) if *n == self_sym)
                        {
                            // Bare `self` of a method the trait does not
                            // declare (reported above): the impl's type.
                            body_self.clone()
                        } else {
                            self.fresh_var()
                        }
                    }
                };
                param_types.push(ty);
            }
            let ret_type = match (&method.return_type, expected_ret) {
                (Some(te), _) => self.resolve_type_expr(te, &mut param_map),
                (None, Some(declared)) => declared,
                (None, None) => self.fresh_var(),
            };
            self.resolving_signature = outer_signature;
            self.remember_constructor_labels(applied, &param_map);
            let written = Type::Fun(param_types, Box::new(ret_type));
            // Where the two disagree, the body is checked against what
            // the impl wrote, so the disagreement is reported once.
            let body_type = match &expected {
                // The impl writes the parameters the trait declares, the
                // receiver among them: one more or one less is not a
                // method of the trait.
                Some(Type::Fun(declared, _)) if declared.len() != method.params.len() => {
                    self.impl_method_arity(ti, method, declared.len());
                    written
                }
                // And the receiver where the trait declares one, there
                // only: `self` is no name for a parameter of a method
                // that is called on the type.
                Some(_) if declared_receiver.is_some_and(|takes| takes != writes_self(method)) => {
                    self.impl_method_receiver(ti, method);
                    written
                }
                Some(expected) => {
                    let unified = self.unify_types(&written, expected);
                    match unified {
                        Ok(()) => expected.clone(),
                        Err(mismatch) => {
                            self.report_mismatch(mismatch, method.span);
                            written
                        }
                    }
                }
                None => written,
            };
            // Callers see the declared type.
            let fn_type = match seeded {
                Some(ty) => ty,
                None => unrigidify(&self.apply(&body_type)).0,
            };

            // Two traits may each provide a method of one name for one
            // type (two modules' `Show` for `Int`): each impl is kept, by
            // its trait, and a call means the one whose trait the module
            // of the call sees (`method_entry`); a call that
            // sees both is ambiguous.

            // What a use of the method owes: the bounds of the impl's
            // header and the bounds the trait declares for the method.
            // The method's own `where` clauses may restate these; one
            // that adds a bound is an error below.
            let mut method_constraints = impl_level_constraints.clone();
            method_constraints.extend(declared_bounds);
            for wc in &method.where_clauses {
                // A bound the resolver resolved to nothing: it reported why.
                if wc.trait_res == Some(crate::defs::Res::Error) {
                    continue;
                }
                let type_param = &wc.type_param;
                let trait_args = &wc.trait_args;
                let Some(trait_key) = self
                    .named_trait(wc.trait_res, wc.trait_name)
                    .filter(|t| self.tables.traits.contains_key(t))
                else {
                    self.error(
                        Code::UnknownTrait,
                        format!(
                            "unknown trait '{}' in where clause on '{}.{}'",
                            resolve(wc.trait_name),
                            resolve(ti.target_type),
                            resolve(method.name)
                        ),
                        method.span,
                    );
                    continue;
                };
                let trait_name = &trait_key;
                // The bound's arity must match the trait's declared
                // param count (see check_where_bound_arity — it dedupes
                // against the identical diagnostic the check of the
                // method's body emits for the same span).
                if !self.check_where_bound_arity(*trait_name, trait_args.len(), method.span) {
                    continue;
                }
                // Resolve the bound's trait args through the method's
                // param_map (which sees both the impl-level binders
                // and any method-local type annos). Empty for
                // parameterless trait bounds. Without storing these
                // alongside the (tv, trait) pair, downstream
                // `verify_trait_obligation` would fall back to the
                // bare "trait implemented" check — letting
                // `where a: Conv(Int)` accept any `Conv(*) for ...`.
                let resolved_bound_args: Vec<Type> = trait_args
                    .iter()
                    .map(|te| {
                        let arg = self.resolve_type_expr(te, &mut param_map);
                        unrigidify(&self.apply(&arg)).0
                    })
                    .collect();
                match param_map.get(type_param) {
                    Some(ty) => {
                        // The bound is on the variable as callers see
                        // it: the one the rigid variable is in the
                        // method's type.
                        let bounded = match self.apply(ty) {
                            Type::Var(tv) => Some(tv),
                            Type::Rigid(r) => Some(r.var),
                            _ => None,
                        };
                        if let Some(tv) = bounded {
                            // The trait's signature is what a caller
                            // through the trait knows: the impl's method
                            // may restate a bound the trait or the impl's
                            // header declares, not add one.
                            let declared = self.bound_follows(
                                &method_constraints,
                                tv,
                                *trait_name,
                                &resolved_bound_args,
                            );
                            if !declared && has_declared_type {
                                self.errors.push(
                                    Diagnostic::error(
                                        Code::InvalidTraitImpl,
                                        method.span,
                                        format!(
                                            "method '{}' of the impl of '{}' for '{}' adds the bound \
                                             `{}: {}`, which the trait does not declare for it",
                                            method.name,
                                            self.impl_trait_name(ti),
                                            self.impl_target_name(ti, target_type),
                                            type_param,
                                            self.show_bound(*trait_name, &resolved_bound_args)
                                        ),
                                    )
                                    .with_help(format!(
                                        "a call through the trait would not owe it: declare the \
                                         bound on the method in trait '{}', or on the impl's header",
                                        self.impl_trait_name(ti)
                                    )),
                                );
                                // (A bound refused beside the trait's
                                // own bound of its trait is not the
                                // method's: its body has the trait's,
                                // and no second error comes of the two.)
                                let of_trait = method_constraints.iter().any(|pred| {
                                    matches!(pred, Pred::Trait { tr, subject, .. }
                                        if tr == trait_name && *subject == Type::Var(tv))
                                });
                                if of_trait {
                                    continue;
                                }
                            }
                            method_constraints.push(Pred::bound(
                                tv,
                                *trait_name,
                                resolved_bound_args.clone(),
                            ));
                        }
                    }
                    None => {
                        // Give the user the full "declare it in the sig or
                        // target" hint — this is the same spirit as the
                        // register_fn_decl error at mod.rs:5276.
                        self.error(Code::InvalidTypeAnnotation,
                            format!(
                                "type variable '{}' in where clause on '{}.{}' is not declared in the impl target \
                                 arguments or in the method's parameter annotations",
                                resolve(*type_param),
                                resolve(ti.target_type),
                                resolve(method.name)
                            ),
                            method.span,
                        );
                    }
                }
            }

            self.register_method_entry(
                target_type,
                method.name,
                args_key,
                MethodEntry {
                    method_type: fn_type,
                    structural: false,
                    trait_name: Some(trait_key),
                    preds: (method_constraints.iter().cloned())
                        .chain(self.given_to(&method_rigid, impl_key, method.name))
                        .collect(),
                },
            );

            // What the body is checked against (`check_decl_bodies`).
            // A self type that is a variable (an impl for every function
            // or tuple) has the trait in the body: `self.other()` is
            // the impl's own method.
            let mut body_bounds = method_constraints;
            if let Type::Var(var) = &self_type
                && self.shape_vars.contains_key(var)
            {
                body_bounds.push(Pred::Trait {
                    tr: trait_key,
                    args: self
                        .tables
                        .impl_trait_args
                        .get(&impl_key)
                        .cloned()
                        .unwrap_or_default(),
                    subject: self_type.clone(),
                    asker: None,
                });
            }
            if let Type::Fun(params, ret) = body_type {
                self.impl_sigs.insert(
                    (target_type, method.name, trait_key, args_key),
                    FnSig {
                        params,
                        ret: *ret,
                        names: param_map,
                        rigid: method_rigid,
                        bounds: body_bounds,
                        complete: true,
                    },
                );
            }
        }

        // A method the impl leaves out that the trait has a default for:
        // the type has it, with the declared type. Its body is the
        // trait's, checked there.
        if let Some(info) = &trait_info {
            for (name, ty) in &info.methods {
                if ti.methods.iter().any(|m| m.name == *name)
                    || !info.default_method_bodies.contains_key(name)
                {
                    continue;
                }
                let mut mapping = seed.clone();
                for v in free_vars_in(ty) {
                    mapping.entry(v).or_insert_with(|| self.fresh_var());
                }
                let mut method_constraints = impl_level_constraints.clone();
                let passer = intern(&format!("{target_type}.{name}"));
                // (A `json.Decode` bound asks what the trait's body asks.)
                method_constraints.extend(
                    Self::bounds_under(info, *name, &mapping)
                        .into_iter()
                        .map(|pred| match pred {
                            Pred::Trait {
                                tr,
                                args,
                                subject,
                                asker: Some(asker),
                            } => Pred::Trait {
                                tr,
                                args,
                                subject,
                                asker: Some(Asker {
                                    passer: Some(passer),
                                    ..asker
                                }),
                            },
                            other => other,
                        }),
                );
                method_constraints
                    .extend(self.given_to_default(trait_key, info, *name, &mapping, passer));
                self.register_method_entry(
                    target_type,
                    *name,
                    args_key,
                    MethodEntry {
                        method_type: substitute_vars(ty, &mapping),
                        structural: false,
                        trait_name: Some(trait_key),
                        preds: method_constraints,
                    },
                );
            }
        }
    }

    /// Report the impl method `method`, written with a number of
    /// parameters other than the `declared` ones of its trait.
    fn impl_method_arity(&mut self, ti: &TraitImpl, method: &FnDecl, declared: usize) {
        let (name, tr) = (method.name, self.impl_trait_name(ti));
        let written = method.params.len();
        let takes_self = writes_self(method);
        let mut d = Diagnostic::error(
            Code::ArityMismatch,
            method.span,
            format!(
                "method `{name}` takes {written} {} in this impl, but trait `{tr}` \
                 declares it with {declared}",
                super::inference::plural(written, "parameter", "parameters")
            ),
        );
        if declared == 0 && takes_self {
            d = d.with_help(format!(
                "trait `{tr}` declares `{name}` without `self`: it is called on the type \
                 (`{}.{name}()`), not on a value",
                resolve(ti.target_type)
            ));
        } else if declared == 1 && written == 0 {
            d = d.with_help(format!(
                "trait `{tr}` declares `{name}` with a receiver: write `fn {name}(self)`"
            ));
        }
        self.errors.push(d);
    }

    /// Report the impl method `method`, written with `self` where its
    /// trait declares none, or without where it declares one (with as
    /// many parameters as the trait's).
    fn impl_method_receiver(&mut self, ti: &TraitImpl, method: &FnDecl) {
        let (name, tr) = (method.name, self.impl_trait_name(ti));
        let (message, help) = match writes_self(method) {
            true => (
                format!(
                    "method `{name}` takes `self` in this impl, but trait `{tr}` declares it \
                     without"
                ),
                format!(
                    "trait `{tr}` declares `{name}` without `self`: it is called on the type \
                     (`{}.{name}(..)`), not on a value; name the parameter as the trait does",
                    resolve(ti.target_type)
                ),
            ),
            false => (
                format!(
                    "method `{name}` takes no `self` in this impl, but trait `{tr}` declares \
                     it with one"
                ),
                format!(
                    "trait `{tr}` declares `{name}` with a receiver: write `self` as its first \
                     parameter"
                ),
            ),
        };
        self.errors
            .push(Diagnostic::error(Code::ArityMismatch, method.span, message).with_help(help));
    }

    /// Whether `var: tr(args)` follows from the bounds `declared`: it is
    /// one of them, or a supertrait of one, at the same trait arguments.
    fn bound_follows(
        &mut self,
        declared: &[Pred],
        var: TyVar,
        tr: TraitKey,
        args: &[Type],
    ) -> bool {
        // What the declared bounds say of `var`, supertraits included.
        let outer = self.bounds.remove(&var);
        for pred in declared {
            if let Pred::Trait {
                tr, args, subject, ..
            } = pred
                && *subject == Type::Var(var)
            {
                self.declare_bound(var, *tr, args.clone());
            }
        }
        let said = self.bounds.remove(&var).unwrap_or_default();
        if let Some(outer) = outer {
            self.bounds.insert(var, outer);
        }
        said.iter().any(|(said_tr, said_args)| {
            *said_tr == tr
                && said_args.len() == args.len()
                && said_args
                    .iter()
                    .zip(args)
                    .all(|(a, b)| self.apply(a) == self.apply(b))
        })
    }

    /// The bounds the trait `info` declares for its method `method`,
    /// with the trait's variables replaced as `mapping` says.
    fn bounds_under(info: &TraitInfo, method: Symbol, mapping: &HashMap<TyVar, Type>) -> Vec<Pred> {
        info.method_bounds
            .get(&method)
            .into_iter()
            .flatten()
            .map(|pred| pred.substitute(mapping))
            .collect()
    }

    /// Enter a method of an impl among the impls' methods, by its type,
    /// its name and its trait: the row is the module's that writes the
    /// impl.
    /// What a use of the method `method` of an impl for `target` owes
    /// for the annotation variables `rigid` of its body: what the body
    /// asks of each for the builtins it gives it to (`Pred::Given`); a
    /// note names it `Src.load`.
    fn given_to(&self, rigid: &[RigidId], impl_key: ImplKey, method: Symbol) -> Vec<Pred> {
        if self.registry_rows {
            return Vec::new();
        }
        let (trait_key, target, _) = impl_key;
        let passer = intern(&format!("{target}.{method}"));
        let body = super::builtin_needs::body_key(trait_key, &impl_owner(impl_key), method);
        (rigid.iter())
            .map(|r| Pred::Given {
                subject: Type::Var(r.var),
                given: r.var,
                written: None,
                passer,
                body: Some(body),
                group: None,
            })
            .collect()
    }

    /// What a use of the default method of the trait `info` (`key`) that
    /// an impl takes owes, its variables as `mapping` makes them the
    /// impl's: what the trait's body asks of each. The trait of another
    /// module is checked: what it asks is known.
    fn given_to_default(
        &self,
        key: TraitKey,
        info: &TraitInfo,
        method: Symbol,
        mapping: &HashMap<TyVar, Type>,
        passer: Symbol,
    ) -> Vec<Pred> {
        let body = super::builtin_needs::body_key(key, "", method);
        if self.registry_rows {
            return Vec::new();
        }
        let own = self.tables.traits.is_own(&key);
        // The variables of this method's body: `Self`, the trait's
        // parameters and the method's own.
        let of_method: Vec<TyVar> = (info.methods.iter())
            .filter(|(name, _)| *name == method)
            .flat_map(|(_, ty)| free_vars_in(ty))
            .chain([info.self_var])
            .chain(info.param_var_ids.iter().copied())
            .collect();
        let mut preds = Vec::new();
        for (var, _) in info
            .var_names
            .iter()
            .filter(|(var, _)| of_method.contains(var))
        {
            let subject = substitute_vars(&Type::Var(*var), mapping);
            if own {
                preds.push(Pred::Given {
                    subject,
                    given: *var,
                    written: None,
                    passer,
                    body: Some(body),
                    group: None,
                });
                continue;
            }
            for check in info
                .builtin_needs
                .get(&(*var, method))
                .into_iter()
                .flatten()
            {
                preds.push(Pred::Builtin {
                    check: *check,
                    subject: subject.clone(),
                    passer: Some(passer),
                });
            }
            for via in info.via_needs.get(&(*var, method)).into_iter().flatten() {
                preds.push(Pred::Via {
                    tr: via.tr,
                    method: via.method,
                    receiver: substitute_vars(&via.receiver, mapping),
                    method_ty: substitute_vars(&via.method_ty, mapping),
                    passer: Some(passer),
                    at: None,
                });
            }
        }
        preds
    }

    /// The type the impl `ti` is for, its type variables those of
    /// `impl_param_map`: `written` is the type its header names, of the
    /// head `target_type`.
    fn impl_self_type(
        &mut self,
        ti: &TraitImpl,
        written: TypeRef,
        target_type: TypeRef,
        impl_param_map: &mut HashMap<Symbol, Type>,
    ) -> Type {
        // The impl of a trait of type constructors is for the
        // constructor its header writes, `_` for the argument it maps
        // over (`impl_header_fits`): the self type is read with `_` a
        // variable, and is that type's constructor.
        let hole = intern("_");
        let mapped = (ti.target_type_args.iter().any(is_hole))
            .then(|| self.fresh_tv())
            .map(|(ty, var)| {
                impl_param_map.insert(hole, ty);
                var
            });

        // Construct the self_type. Three cases:
        //   1. Bare-target form (`trait X for Int`): target_type_args is
        //      empty. For primitive types, fall through to type_from_name.
        //      A generic type written bare (`trait X for Box`) is refused
        //      (`impl_header_fits`); what reads such an impl's header
        //      after sees its type with a fresh variable per parameter.
        //   2. Parameterized user type (`trait X for Box(a)`): resolve
        //      each arg through impl_param_map. Arity enforced against
        //      the record's param_var_ids or the enum's declared params.
        //   3. Built-in parameterized form (`trait X for List(a)`):
        //      resolve_type_expr handles List/Map/Set/Channel/Tuple/Fn
        //      already; reuse it.
        //
        // A builtin type, or a record, enum or alias type: a target the
        // checker does not know (a builtin type with no impls, `Tuple`
        // aside) is reported.
        {
            let name_str = resolve(written.name);
            let known = match builtin_type_name(written) {
                Some(builtin) => {
                    crate::types::builtins::is_primitive(builtin)
                        || crate::types::builtins::is_container(builtin)
                        || self.tables.enums.contains_key(&written)
                        || self.tables.records.contains_key(&written)
                }
                None => {
                    self.tables.records.contains_key(&written)
                        || self.tables.enums.contains_key(&written)
                        || self.tables.type_aliases.contains(&written)
                }
            };
            if !known {
                self.error(
                    Code::UnknownType,
                    format!("trait impl target '{name_str}' is not a declared type"),
                    ti.span,
                );
            }
        }

        let self_type = if ti.target_type_args.is_empty() {
            // Phase D: if the bare target is a user alias, synthesize
            // the self-type from the alias's canonical target rather
            // than treating the alias name as a nominal head. Without
            // this, `trait T for Bytes` (where `Bytes = List(Int)`)
            // would produce a `Generic("Bytes", [])` self-type that
            // would never unify with any concrete `List(Int)`
            // receiver. Callers downstream still see the canonical
            // form (`canonical_head` collapses the impl_key
            // to `"List"`), so the dispatch lookup arrives at the
            // right method.
            if let Some(info) = self.tables.resolver.lookup_alias(written) {
                // Build a fresh-var instantiation per alias parameter so
                // the impl methods see polymorphic vars rather than
                // shared template tyvars. For `Bytes = List(Int)` (no
                // params), the substitution is identity and the result
                // is `List(Int)` after canonicalisation. For
                // `Pair(a) = (a, a)`, params get fresh vars and the
                // self-type is `(a', a')`.
                let mut mapping: HashMap<TyVar, Type> = HashMap::new();
                for &var_id in &info.param_var_ids {
                    mapping.insert(var_id, self.fresh_var());
                }
                let substituted = crate::types::substitute_vars(&info.target, &mapping);
                crate::types::canonical::canonicalize(&self.tables.resolver, &substituted)
            } else {
                let user_arity = self
                    .tables
                    .record_param_var_ids
                    .get(&written)
                    .map(|v| v.len())
                    .or_else(|| self.tables.enums.get(&written).map(|e| e.params.len()))
                    .unwrap_or(0);
                if user_arity == 0 {
                    // A refused bare container target (`trait T for
                    // List`, Map/Set/Channel): a fresh var per element
                    // slot, as for a generic user type.
                    //
                    // `Fn` and `Tuple` have no one shape: the impl is
                    // for every function, or every tuple, and its self
                    // type is a variable of the impl (`shape_vars`),
                    // which a use decides by the receiver's type. In
                    // the impl's bodies it is rigid like the `a` of
                    // `List(a)`: `self` is some function, to be handed
                    // on but not called, and not a function of any
                    // signature one may wish for.
                    match builtin_type_name(target_type) {
                        Some("List") => Type::List(Box::new(self.fresh_var())),
                        Some("Set") => Type::Set(Box::new(self.fresh_var())),
                        Some("Channel") => Type::Channel(Box::new(self.fresh_var())),
                        Some("Map") => {
                            Type::Map(Box::new(self.fresh_var()), Box::new(self.fresh_var()))
                        }
                        Some(shape @ ("Fn" | "Tuple")) => {
                            let (ty, var) = self.fresh_tv();
                            let about = match shape {
                                "Fn" => ("function", "its parameters and its result"),
                                _ => ("tuple", "its elements"),
                            };
                            self.shape_vars.insert(var, about);
                            ty
                        }
                        _ => Self::type_from_name(target_type),
                    }
                } else {
                    let args: Vec<Type> = (0..user_arity).map(|_| self.fresh_var()).collect();
                    Type::Generic(target_type, args)
                }
            }
        } else {
            // Arity check. Covers user-declared record/enum targets via
            // record_param_var_ids / self.tables.enums, AND the builtin
            // types whose arities are fixed by the language: the
            // containers (List, Set, Channel, Map) and the primitives,
            // which take none. Without the builtin arm, `trait X for
            // List(a, b)` or `trait X for Int(a)` fell through to
            // `_ => Type::Generic(..)` below, silently producing a phantom
            // type that no value has. Derive fixed-arity builtin entries
            // from the authoritative table. Variadic shapes (`Tuple`,
            // `Fn`, `Fun`, `Handle`) carry `arity: None` and are
            // intentionally skipped — they do not participate in this
            // trait-impl arity check.
            let builtin_arity: Option<(usize, &'static str)> = builtin_type_name(written)
                .and_then(crate::types::builtins::lookup)
                .and_then(|b| b.arity.map(|a| (a as usize, "builtin")));
            // User-declared type aliases are in the arity table too:
            // `trait Show for Pair(a)` where `type Pair(a) = (a, a)` is
            // checked for its arity, and does not fall through to the
            // `_ => Type::Generic("Pair", [tv])` arm at the bottom, an
            // impl whose self_type would never unify with a concrete
            // `(Int, Int)` receiver.
            let alias_arity: Option<(usize, &'static str)> = self
                .tables
                .type_alias_arity
                .get(&written)
                .copied()
                .map(|a| (a, "alias"));
            let expected_arity = self
                .tables
                .record_param_var_ids
                .get(&written)
                .map(|v| (v.len(), "record"))
                .or_else(|| {
                    self.tables
                        .enums
                        .get(&written)
                        .map(|e| (e.params.len(), "enum"))
                })
                .or(alias_arity)
                .or(builtin_arity);
            if let Some((expected, kind)) = expected_arity
                && expected != ti.target_type_args.len()
            {
                self.error(Code::ArityMismatch,
                    format!(
                        "type argument count mismatch for {kind} '{}' in trait impl: expected {expected}, got {}",
                        resolve(written.name),
                        ti.target_type_args.len()
                    ),
                    ti.span,
                );
            }
            // Resolve through a dedicated Generic form so the head symbol
            // is preserved alongside the impl-level tyvar args.
            let resolved_args: Vec<Type> = ti
                .target_type_args
                .iter()
                .map(|arg_te| self.resolve_type_expr(arg_te, impl_param_map))
                .collect();
            // A parametric alias as trait-impl target:
            // expand the alias by substituting `resolved_args` through
            // the alias's stored target. Mirrors the non-parametric
            // alias path at line ~5617 (which calls
            // `self.tables.resolver.lookup_alias(written)` and walks
            // `info.target` with each `param_var_ids[i]` mapped to a
            // fresh tyvar). Here we map `param_var_ids[i]` → the
            // user-supplied type-arg at the same index.
            //
            // So `trait Show for Pair(a)` (with `type Pair(a) = (a, a)`)
            // has the self type `(a, a)`, not a phantom
            // `Type::Generic("Pair", [tv])`, which a concrete `(1, 2)`
            // receiver typed as `(Int, Int)` (as `resolve_type_expr`
            // canonicalises the annotation `Pair(Int)`) would not unify
            // with.
            if let Some(info) = self.tables.resolver.lookup_alias(written) {
                let mut mapping: HashMap<TyVar, Type> = HashMap::new();
                for (i, &var_id) in info.param_var_ids.iter().enumerate() {
                    if let Some(arg_ty) = resolved_args.get(i) {
                        mapping.insert(var_id, arg_ty.clone());
                    }
                }
                let substituted = crate::types::substitute_vars(&info.target, &mapping);
                crate::types::canonical::canonicalize(&self.tables.resolver, &substituted)
            } else {
                match builtin_type_name(written).unwrap_or_default() {
                    "List" if resolved_args.len() == 1 => {
                        Type::List(Box::new(resolved_args.into_iter().next().unwrap()))
                    }
                    "Set" if resolved_args.len() == 1 => {
                        Type::Set(Box::new(resolved_args.into_iter().next().unwrap()))
                    }
                    "Channel" if resolved_args.len() == 1 => {
                        Type::Channel(Box::new(resolved_args.into_iter().next().unwrap()))
                    }
                    "Map" if resolved_args.len() == 2 => {
                        let mut iter = resolved_args.into_iter();
                        Type::Map(
                            Box::new(iter.next().unwrap()),
                            Box::new(iter.next().unwrap()),
                        )
                    }
                    // A tuple of the impl's arity: `(a, b)` of
                    // `Tuple(a, b)`, which a pair's type is.
                    "Tuple" if resolved_args.len() >= 2 => Type::Tuple(resolved_args),
                    // A primitive takes no arguments (the error above
                    // says so): the impl is for the type itself, not a
                    // phantom `Int(_)` that no value has.
                    _ if matches!(expected_arity, Some((0, "builtin"))) => {
                        Self::type_from_name(target_type)
                    }
                    _ => Type::Generic(written, resolved_args),
                }
            }
        };
        match mapped {
            None => self_type,
            Some(var) => {
                impl_param_map.remove(&hole);
                match self.apply(&self_type).split_constructor() {
                    Some((head, rest, Type::Var(first)))
                        if first == var && !rest.iter().any(|t| free_vars_in(t).contains(&var)) =>
                    {
                        Type::Con(head, rest)
                    }
                    // (An alias whose first parameter is not its type's
                    // first argument.)
                    _ => {
                        let message = format!(
                            "'_' must be the first type argument of '{}': a type constructor \
                             stands for its first type argument",
                            resolve(ti.target_type)
                        );
                        self.error(Code::KindMismatch, message, ti.span);
                        Type::Error
                    }
                }
            }
        }
    }

    pub(super) fn register_method_entry(
        &mut self,
        target_type: TypeRef,
        method: Symbol,
        args: ImplArgs,
        entry: MethodEntry,
    ) {
        let trait_key = entry.trait_name.expect("an impl's method has its trait");
        self.tables
            .impl_methods
            .insert(target_type, method, trait_key, args, entry);
    }

    /// The key of the trait arguments `ti` writes, and of the type it is
    /// for where it is for some instantiations of its type only
    /// (`ImplArgs`). Kept by the impl's span for what reads the impl
    /// after (`impl_args_keys`).
    pub(super) fn impl_args_key_of(&mut self, ti: &TraitImpl) -> ImplArgs {
        if let Some(key) = self.impl_args_keys.get(&ti.span) {
            return *key;
        }
        let written = self.impl_written_args(ti, false);
        // (A trait whose method shows or encodes a value at any depth is
        // reached by the value's type alone, whatever its type arguments:
        // of it, a type has one impl. A decoder is given the type in
        // full: each instantiation may have its own `json.Decode` impl.)
        let by_type = (self.impl_trait(ti))
            .is_some_and(|tr| Self::reached_at_any_depth(tr) && !tr.is_builtin("Decode"));
        let instance = (written.head.as_ref()).filter(|head| !by_type && self.is_instance(head));
        let key = self.impl_args_key(&written.args, instance);
        self.impl_args_keys.insert(ti.span, key);
        key
    }

    /// The trait arguments `ti` writes and the type it is for, each type
    /// variable of its header a new one, with the names of the
    /// variables. What is wrong with the trait arguments is reported with
    /// `report`, else where the impl's trait arguments are resolved for
    /// good; what is wrong with the type, where the impl's self type is.
    fn impl_written_args(&mut self, ti: &TraitImpl, report: bool) -> WrittenImpl {
        let mut scratch: HashMap<Symbol, Type> = HashMap::new();
        for &name in &ti.target_param_names {
            scratch.insert(name, self.fresh_var());
        }
        let reported = self.errors.len();
        let args: Vec<Type> = (ti.trait_args.iter())
            .map(|te| self.resolve_type_expr(te, &mut scratch))
            .collect();
        if !report {
            self.errors.truncate(reported);
        }
        let reported = self.errors.len();
        let head = (self.named_type(ti.target_res, ti.target_type)).map(|written| {
            let target_type = canonical_head(&self.tables.resolver, written);
            let head = self.impl_self_type(ti, written, target_type, &mut scratch);
            crate::types::canonical::canonicalize(&self.tables.resolver, &self.apply(&head))
        });
        self.errors.truncate(reported);
        let names = (scratch.iter())
            .filter_map(|(name, ty)| match self.apply(ty) {
                Type::Var(v) => Some((v, *name)),
                _ => None,
            })
            .collect();
        WrittenImpl { args, head, names }
    }

    /// Whether the impls of `tr` are reached by the type of a value
    /// alone, at any depth of a value shown, encoded or decoded
    /// (`Display`, `json.Encode`, `json.Decode`): a type has one.
    fn reached_at_any_depth(tr: TraitKey) -> bool {
        STRUCTURAL_METHODS
            .iter()
            .any(|(name, _)| tr.is_builtin(name))
    }

    /// Whether the impl `ti` is for some instantiations of its type only
    /// (`trait Display for Box(Int)`, or through `type IB = Box(Int)`).
    pub(super) fn impl_for_instance(&mut self, ti: &TraitImpl) -> bool {
        self.enter_level();
        let written = self.impl_written_args(ti, false);
        let instance = (written.head.as_ref()).is_some_and(|head| self.is_instance(head));
        self.exit_level();
        instance
    }

    /// Report the impl `ti` of `tr` for `head`, some instantiations of
    /// the type `ty` (`trait Display for Box(Int)`), of `Display` or
    /// `json.Encode`, whose impls a value's type alone reaches: of these
    /// a type has one impl, for every instantiation.
    fn instance_impl_reached_by_type(
        &mut self,
        ti: &TraitImpl,
        tr: TraitKey,
        ty: TypeRef,
        head: &Type,
        written: &WrittenImpl,
    ) {
        let trait_name = self.impl_trait_name(ti);
        let instance = self.show_type(&self.with_names(head, &written.names));
        let done = match tr.is_builtin("Encode") {
            true => "encoded",
            false => "shown",
        };
        let params: Vec<String> = (('a'..='z').map(|c| c.to_string()))
            .take(self.type_arity(ty).unwrap_or(1))
            .collect();
        let every = format!(
            "{}({})",
            self.show_type(&Type::Generic(ty, Vec::new())),
            params.join(", ")
        );
        self.error_help(
            Code::InvalidTraitImpl,
            (
                format!(
                    "an impl of trait '{trait_name}' for '{instance}' alone cannot be: a value \
                     is {done} by the impl of its type, whatever its type arguments"
                ),
                Some(format!(
                    "write it for every instantiation: `trait {trait_name} for {every}`"
                )),
            ),
            ti.header_span,
        );
    }

    /// `ty`, each type variable by its name in `names`.
    pub(super) fn with_names(&self, ty: &Type, names: &[(TyVar, Symbol)]) -> Type {
        let mapping: HashMap<TyVar, Type> = (names.iter())
            .map(|(var, name)| {
                let rigid = RigidId {
                    var: *var,
                    name: *name,
                };
                (*var, Type::Rigid(rigid))
            })
            .collect();
        substitute_vars(&self.apply(ty), &mapping)
    }

    /// The trait `tr` at the trait arguments `args`, each type variable
    /// by its name in `names` (`_` for one without).
    fn show_named(&self, tr: TraitKey, args: &[Type], names: &[(TyVar, Symbol)]) -> String {
        let named: Vec<Type> = (args.iter())
            .map(|arg| self.with_names(arg, names))
            .collect();
        self.show_bound(tr, &named)
    }

    /// The impl of `tr` for `ty` written before that one use could mean
    /// as well as `ti`: whose trait arguments could be those `ti` writes,
    /// and whose type could be the one `ti` is for (`Box(a)` and
    /// `Box(Int)`).
    fn impl_met(
        &mut self,
        ti: &TraitImpl,
        tr: TraitKey,
        ty: TypeRef,
        args: ImplArgs,
    ) -> Option<ImplArgs> {
        let written = self.impl_written_args(ti, false);
        (self.tables.impl_args_of(tr, ty).iter())
            .copied()
            .find(|earlier| {
                let key = (tr, ty, *earlier);
                let theirs = (self.tables.impl_trait_args.get(&key)).map_or(&[][..], Vec::as_slice);
                let (mine, theirs) = match (&written.head, self.tables.impl_self_types.get(&key)) {
                    (Some(_), Some(head)) => (
                        written.full(),
                        theirs.iter().chain([head]).cloned().collect(),
                    ),
                    _ => (written.args.clone(), theirs.to_vec()),
                };
                *earlier == args || self.could_unify(&mine, &theirs).is_some()
            })
    }

    /// Report the impl `ti` of `tr` for `ty` at the trait arguments
    /// `args`, which one use could mean as well as the impl written before
    /// at `met`.
    fn duplicate_impl(
        &mut self,
        ti: &TraitImpl,
        tr: TraitKey,
        ty: TypeRef,
        args: ImplArgs,
        met: ImplArgs,
    ) {
        let trait_name = self.impl_trait_name(ti);
        let target = self.impl_target_name(ti, ty);
        let mine = self.impl_written_args(ti, false);
        let instance = |checker: &Self, written: &WrittenImpl| {
            (written.head.as_ref()).is_some_and(|head| checker.is_instance(head))
        };
        let written = match mine.args.is_empty() {
            true => trait_name.clone(),
            false => self.show_named(tr, &mine.args, &mine.names),
        };
        // (An impl for every tuple or every function overlaps one for
        // some of them, by its type: said as two impls for one type.)
        let every_shape = |checker: &Self, head: Option<&Type>| match head.map(|h| checker.apply(h))
        {
            Some(Type::Var(v)) => checker.shape_vars.contains_key(&v),
            Some(Type::Rigid(r)) => checker.shape_vars.contains_key(&r.var),
            _ => false,
        };
        let earlier_head = (self.impl_written.get(&(tr, ty, met)))
            .and_then(|earlier| earlier.head.clone())
            .or_else(|| self.tables.impl_self_types.get(&(tr, ty, met)).cloned());
        let shaped =
            every_shape(self, mine.head.as_ref()) || every_shape(self, earlier_head.as_ref());
        let (message, help) = if args == met || shaped {
            // (Of an impl for some instantiations of its type, the type.)
            let target = match (mine.head.as_ref()).filter(|_| instance(self, &mine) && !shaped) {
                Some(head) => self.show_type(&self.with_names(head, &mine.names)),
                None => target,
            };
            (
                format!("duplicate implementation of trait '{written}' for type '{target}'"),
                None,
            )
        } else {
            // (The trait arguments and the type a use of both would have.
            // The impls' variables by their names, as written.)
            let earlier = match self.impl_written.get(&(tr, ty, met)) {
                Some(earlier) => earlier.clone(),
                None => WrittenImpl {
                    args: (self.tables.impl_trait_args.get(&(tr, ty, met)))
                        .cloned()
                        .unwrap_or_default(),
                    head: self.tables.impl_self_types.get(&(tr, ty, met)).cloned(),
                    names: Vec::new(),
                },
            };
            let by_type = instance(self, &mine) || instance(self, &earlier);
            let (mine_all, earlier_all) = match by_type {
                true => (mine.full(), earlier.full()),
                false => (mine.args.clone(), earlier.args.clone()),
            };
            let subst = self
                .could_unify(&mine_all, &earlier_all)
                .unwrap_or_default();
            let both = WrittenImpl {
                args: Vec::new(),
                head: None,
                names: (mine.names.iter().chain(&earlier.names)).copied().collect(),
            };
            let both_all: Vec<Type> = (mine_all.iter())
                .map(|arg| {
                    let mut arg = arg.clone();
                    for _ in 0..=subst.len() {
                        arg = substitute_vars(&arg, &subst);
                    }
                    arg
                })
                .collect();
            let n = mine.args.len();
            let show = |checker: &Self, all: &[Type], names: &[(TyVar, Symbol)]| {
                let args = checker.show_named(tr, &all[..n.min(all.len())], names);
                let head =
                    (all.get(n)).map(|head| checker.show_type(&checker.with_names(head, names)));
                (args, head.unwrap_or_default())
            };
            let (earlier_args, earlier_head) = show(self, &earlier_all, &earlier.names);
            let (written_args, written_head) = show(self, &mine_all, &mine.names);
            let (both_args, both_head) = show(self, &both_all, &both.names);
            match (by_type, n) {
                (false, _) => (
                    format!(
                        "duplicate implementation of trait '{trait_name}' for type '{target}': the \
                         impls of '{earlier_args}' and '{written_args}' overlap: both are for \
                         '{both_args}'"
                    ),
                    Some(
                        "give each impl trait arguments of its own, which no other impl's could be"
                            .to_string(),
                    ),
                ),
                (true, 0) => (
                    format!(
                        "duplicate implementation of trait '{trait_name}' for type '{target}': the \
                         impls for '{earlier_head}' and '{written_head}' overlap: both are for \
                         '{both_head}'"
                    ),
                    Some(
                        "give each impl a type of its own, which no other impl's could be"
                            .to_string(),
                    ),
                ),
                (true, _) => (
                    format!(
                        "duplicate implementation of trait '{trait_name}' for type '{target}': the \
                         impls of '{earlier_args}' for '{earlier_head}' and '{written_args}' for \
                         '{written_head}' overlap: both are of '{both_args}' for '{both_head}'"
                    ),
                    Some(
                        "give each impl trait arguments or a type of its own, which no other \
                         impl's could be"
                            .to_string(),
                    ),
                ),
            }
        };
        self.error_help(Code::DuplicateDeclaration, (message, help), ti.span);
    }

    /// A where-clause bound must
    /// supply exactly the trait's declared number of type arguments.
    /// Returns `true` when the arity matches (or the trait is unknown
    /// — the caller has already reported that).
    ///
    /// Both directions are soundness-relevant, not just hygiene: the
    /// parameterized-trait verification in `verify_trait_obligation`
    /// only runs its positional arg-compatibility zip when
    /// `impl_args.len() == bound_trait_args.len()`, so a
    /// length-mismatched bound like `where a: Cast(Int, String)` on a
    /// one-param `trait Cast(to)` would degrade to a bare
    /// "implements Cast" check, matching (and dispatching through!)
    /// any `Cast(*)` impl. Zero args on a parameterized trait
    /// would leave the implied params unresolved as well.
    ///
    /// Callers: the fn-level where-clause loop in
    /// `check_body`, and the impl-level and method-level
    /// where-clause loops in `register_trait_impl`. Method-level
    /// bounds pass through BOTH the registration site and the body
    /// check, so the emit dedupes on (message, span) to keep the
    /// diagnostic single.
    pub(super) fn check_where_bound_arity(
        &mut self,
        trait_name: TraitKey,
        got: usize,
        span: Span,
    ) -> bool {
        let Some(info) = self.tables.traits.get(&trait_name) else {
            return true;
        };
        let n = info.params.len();
        if got == n {
            return true;
        }
        let msg = format!(
            "trait '{}' expects {} {} in bound, got {}",
            resolve(trait_name.name),
            n,
            inference::plural(n, "type argument", "type arguments"),
            got
        );
        if !self
            .errors
            .iter()
            .any(|e| e.message == msg && e.span == span)
        {
            self.error(Code::ArityMismatch, msg, span);
        }
        false
    }
}

/// Whether the impl header argument `arg` is `_`, the argument an impl
/// of a trait of type constructors maps over.
fn is_hole(arg: &TypeExpr) -> bool {
    matches!(&arg.kind, TypeExprKind::Named { module: None, name, .. } if resolve(*name) == "_")
}

/// Where a signature writes `Self`: with a type argument somewhere
/// (`Self(a)`), and the first place without one.
#[derive(Default)]
struct SelfUses {
    applied: bool,
    bare: Option<Span>,
}

impl SelfUses {
    fn read(&mut self, te: &TypeExpr) {
        match &te.kind {
            TypeExprKind::SelfType => {
                self.bare.get_or_insert(te.span);
            }
            TypeExprKind::Generic {
                module, name, args, ..
            } => {
                if module.is_none() && resolve(*name) == "Self" {
                    self.applied = true;
                }
                for arg in args {
                    self.read(arg);
                }
            }
            TypeExprKind::Tuple(parts) => {
                for part in parts {
                    self.read(part);
                }
            }
            TypeExprKind::Function(params, ret) => {
                for part in params.iter().chain([&**ret]) {
                    self.read(part);
                }
            }
            TypeExprKind::AnonRecord { fields, .. } => {
                for (_, part) in fields {
                    self.read(part);
                }
            }
            TypeExprKind::AssocProj { receiver, .. } => self.read(receiver),
            TypeExprKind::Named { .. } => {}
        }
    }
}

/// Whether the method `method` is written with a receiver: its first
/// parameter is `self`.
fn writes_self(method: &FnDecl) -> bool {
    method.params.first().is_some_and(
        |param| matches!(&param.pattern.kind, PatternKind::Ident(n) if resolve(*n) == "self"),
    )
}

/// What the header of an impl writes, each type variable a new one
/// (`TypeChecker::impl_written_args`).
#[derive(Clone)]
pub(crate) struct WrittenImpl {
    /// Its trait arguments.
    pub(crate) args: Vec<Type>,
    /// The type it is for (`Box(a)`, `Box(Int)`), where its type is known.
    pub(crate) head: Option<Type>,
    /// The names of its type variables.
    pub(crate) names: Vec<(TyVar, Symbol)>,
}

impl WrittenImpl {
    /// Its trait arguments, then the type it is for.
    fn full(&self) -> Vec<Type> {
        self.args.iter().chain(&self.head).cloned().collect()
    }
}
