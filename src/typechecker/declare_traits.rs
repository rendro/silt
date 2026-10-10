use super::*;

impl TypeChecker {
    // ── Validate trait implementations ────────────────────────────────

    pub(super) fn validate_trait_impls(&mut self) {
        // (a) An unknown supertrait is reported where the trait is
        // registered (`register_trait_decl_inner`).

        // The impls this module writes: an impl is validated where it
        // is written, once, and not again by every module checked after
        // it in the session.
        let impl_pairs: Vec<(TraitKey, TypeRef)> = self.tables.trait_impl_set.added().collect();
        for (trait_name, type_name) in &impl_pairs {
            // The impl block, for the messages. (A row without one is
            // no written impl: nothing to validate.)
            let Some(&diag_span) = self.tables.trait_impl_spans.get(&(*trait_name, *type_name))
            else {
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
            // B1 (round 60): when the supertrait reference carries args
            // (`trait Holds(b): Carry(b)` + `impl Holds(Int) for Bag`),
            // resolve those args through the enclosing trait's
            // params→impl-args mapping and require the matching impl to
            // exist with positionally-compatible args. Otherwise
            // `impl Carry(String) for Bag` would silently satisfy the
            // obligation for `impl Holds(Int) for Bag`, causing a
            // runtime method-resolution failure.
            let enclosing_args: Vec<Type> = self
                .tables
                .impl_trait_args
                .get(&(*trait_name, *type_name))
                .cloned()
                .unwrap_or_default();
            for (i, supertrait) in trait_info.supertraits.iter().enumerate() {
                if !self.head_has_trait(*supertrait, *type_name) {
                    self.error(Code::MissingTraitImpl,
                        format!(
                            "type '{type_name}' implements '{trait_name}' but does not implement supertrait '{supertrait}'"
                        ),
                        diag_span,
                    );
                    continue;
                }
                // Resolve the supertrait's expected args against the
                // enclosing trait's impl args. Skip when there are no
                // supertrait args declared (bare `: Super` form).
                let arg_exprs = trait_info.supertrait_args.get(i);
                let expected_super_args: Vec<Type> = match arg_exprs {
                    Some(exprs) if !exprs.is_empty() => exprs
                        .iter()
                        .map(|te| {
                            crate::typechecker::inference::resolve_supertrait_arg(
                                te,
                                &trait_info,
                                &enclosing_args,
                            )
                        })
                        .collect(),
                    _ => continue,
                };
                let actual_super_args = self
                    .tables
                    .impl_trait_args
                    .get(&(*supertrait, *type_name))
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
                            .map(|t| format!("{t}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    };
                    self.error(Code::InvalidTraitImpl,
                        format!(
                            "impl {}({}) for {} requires impl {}({}) for {}, but found impl {}({}) for {}",
                            resolve(trait_name.name),
                            fmt_args(&enclosing_args),
                            type_name,
                            resolve(supertrait.name),
                            fmt_args(&expected_super_args),
                            type_name,
                            resolve(supertrait.name),
                            fmt_args(&actual_super_args),
                            type_name,
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
                    .get(*type_name, *method_name, *trait_name)
                    .is_some();
                if !provided && !trait_info.default_method_bodies.contains_key(method_name) {
                    // No impl method AND the trait does not provide a
                    // default body — the impl is genuinely missing a
                    // required method.
                    self.error(
                        Code::InvalidTraitImpl,
                        format!(
                            "trait impl '{}' for '{}' is missing method '{}'",
                            trait_name, type_name, method_name
                        ),
                        diag_span,
                    );
                    // That is the impl's one error. The type has the
                    // trait by this impl for every question, a call of
                    // the method among them: it is entered with the
                    // type the trait declares, so a use of it is not
                    // reported as well.
                    let impl_key = (*trait_name, *type_name);
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
                        MethodEntry {
                            method_type: substitute_vars(method_ty, &mapping),
                            structural: false,
                            trait_name: Some(*trait_name),
                            receiver: trait_info.receivers.contains(method_name),
                            preds,
                        },
                    );
                }
            }
        }
    }

    // ── Register trait declarations ─────────────────────────────────

    /// User-source entry point for trait declaration registration.
    /// Round 62 (item 3 of type-design improvements): trait registration
    /// was previously two paths — this one for user `trait X { ... }`
    /// declarations, and a hand-rolled `register_builtin_trait_decl` for
    /// Display/Compare/Equal/Hash plus a one-off block for `Error`. The
    /// two drifted: the built-in path didn't run the duplicate-method
    /// check, didn't honor where_clauses or supertraits in general, and
    /// any future feature added here had to be manually mirrored into
    /// the built-in path.
    ///
    /// The unification: built-in traits now synthesize `TraitDecl` AST
    /// nodes in `builtin_trait_decls()` and feed them through
    /// `register_trait_decl_inner` directly (skipping only the
    /// redefinition guard, which is keyed off user input). Future
    /// trait-decl features automatically apply to built-ins.
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
        // GAP (round 35 F6): duplicate method names in a trait
        // declaration used to silently overwrite each other in the
        // trait's `methods` Vec (first entry won for method lookup but
        // the second's signature won for any HashMap-based bookkeeping
        // like `default_method_bodies`). Emit a diagnostic per dup.
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
                var_names: Vec::new(),
                default_method_bodies: HashMap::new(),
                assoc_types: pre_assoc_types,
                private_to,
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
                let args: Vec<Type> = wc
                    .trait_args
                    .iter()
                    .map(|te| self.resolve_type_expr(te, &mut param_map))
                    .collect();
                bounds.push(Pred::bound(tv, bound, args));
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
                var_names,
                default_method_bodies,
                assoc_types,
                private_to,
            },
        );
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
    /// Round 74 Fix #1: include the `Unit`/`()` arm so a user-declared
    /// `trait T for Unit { ... }` impl receives a `Type::Unit` self_type
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
            let message = if resolve(trait_name) == "Number" {
                "trait 'Number' cannot be implemented by hand: it is the types arithmetic \
                 is on, Int and Float"
                    .to_string()
            } else {
                format!(
                    "trait '{trait_name}' cannot be implemented by hand: it is derived \
                     structurally for every type whose fields support it — remove this \
                     impl; Equal, Compare and Hash are derived"
                )
            };
            self.error(Code::InvalidTraitImpl, message, span);
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
    /// - A call that names the impl where it is written (`y.m()` with
    ///   `y: Y`, `Y.m`), in a module `X`: `X` has `T` and `Y` in its
    ///   types. A type comes into a module's types only through what
    ///   the module imports, and a method of `T` is selected only where
    ///   `T`'s module is imported, directly or through imports
    ///   (`method_entry`). So `X` imports `I`, or is `I`.
    /// - A call decided where the code runs (`x.m()` with `x: a where
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
        let (tr, ty) = (resolve(ti.trait_name), resolve(ti.target_type));
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
        // Phase B: canonicalise the target-type symbol so an impl
        // `trait Foo for Range(a)` registers under the same key
        // (`"List"`) that dispatch lookup will use for both `Range(_)`
        // and `List(_)` receivers. Round 61's
        // dispatch fix collapsed receivers to `List` at runtime; with phase B's `type_name_for_impl`
        // canonicalising at the lookup side too, the impl table
        // would otherwise be unreachable for an explicitly
        // Range-targeted impl. Without this canonicalisation the
        // round-61 lock test
        // `user_trait_method_on_list_dispatches_for_range_receiver`
        // (and its siblings) regresses to "type 'Range' does not
        // implement trait 'Foo'".
        //
        // Round-23 GAP #1: reject trait impls whose target type was never
        // declared: `trait Greet for Widget { ... }` with no `type
        // Widget` would attach methods to a phantom type. A lowercase
        // target (`trait Display for a { ... }`) names a type variable,
        // not a type: there is nothing to register it for.
        if self.names_rejected(ti.target_res, ti.target_type) {
            return;
        }
        let Some(written) = self.named_type(ti.target_res, ti.target_type) else {
            let name_str = resolve(ti.target_type);
            if !name_str.starts_with(|c: char| c.is_lowercase()) {
                self.error(
                    Code::UnknownType,
                    format!("trait impl target '{name_str}' is not a declared type"),
                    ti.span,
                );
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
        let impl_key = (trait_key, target_type);

        // Coherence check: reject duplicate user-defined impls.
        if self.tables.trait_impl_set.contains(&impl_key) {
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
                        ti.trait_name, ti.target_type
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

        self.tables.trait_impl_set.insert(impl_key);
        // GAP-2: record the impl block's real span for validate_trait_impls
        // to use when reporting missing-method diagnostics.
        self.tables.trait_impl_spans.insert(impl_key, ti.span);

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

        // Construct the self_type. Three cases:
        //   1. Bare-target form (`trait X for Int`): target_type_args is
        //      empty. For primitive types, fall through to type_from_name.
        //      For parameterized user types, synthesize fresh-var args to
        //      match the record's / enum's arity — otherwise the receiver-
        //      unify step in dispatch_method_entry would fail with arity
        //      mismatch when the caller passes a concrete instantiation
        //      like `Box { value: 42 }` (Generic("Box", [Int])) against a
        //      zero-arg self_type (Generic("Box", [])).
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
                    // Bare builtin-container targets (`trait T for List`,
                    // Map/Set/Channel; a bare `Range` target arrives here
                    // as `List` via `canonical_head`) mirror
                    // `resolve_type_expr`'s bare-name annotation
                    // semantics: synthesize a fresh var per element slot
                    // so the self_type unifies with any concrete receiver
                    // (`List(Int)`, `Map(String, Bool)`, ...). Pre-fix
                    // these fell through to `type_from_name`'s
                    // `Generic("List", [])`, which unify's catch-all
                    // rejected against `Type::List(Int)` with the
                    // self-contradictory "type mismatch: expected List,
                    // got List(Int)" — even though the SAME impl
                    // dispatched fine through a where-bound fn
                    // (head-keyed obligation + runtime dispatch). The
                    // fresh vars are per-registration, like the alias /
                    // user-arity branches: `method_scheme`
                    // quantifies them, new at each use.
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
            // record_param_var_ids / self.tables.enums, AND builtin parameterized
            // containers (List, Set, Channel, Map) whose arities are fixed
            // by the language. Without the builtin arm, `trait X for List(a, b)`
            // fell through to `_ => Type::Generic("List", [a, b])` below,
            // silently producing a phantom 2-arg List type with no diagnostic.
            // Derive fixed-arity builtin entries from the authoritative
            // table. Variadic shapes (`Tuple`, `Fn`, `Fun`, `Handle`)
            // carry `arity: None` and are intentionally skipped — they
            // do not participate in this trait-impl arity check.
            let builtin_arity: Option<(usize, &'static str)> = builtin_type_name(written)
                .and_then(crate::types::builtins::lookup)
                .filter(|b| b.kind == crate::types::builtins::BuiltinKind::Container)
                .and_then(|b| b.arity.map(|a| (a as usize, "builtin")));
            // Round 74 Fix #2: include user-declared type aliases in the
            // arity table. Without this, `trait Show for Pair(a)` where
            // `type Pair(a) = (a, a)` skipped the arity check (the alias
            // is not a record / enum / builtin container) and fell
            // through to the `_ => Type::Generic("Pair", [tv])` arm at
            // the bottom — producing an impl whose self_type never
            // unifies with any concrete `(Int, Int)` receiver.
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
                .map(|arg_te| self.resolve_type_expr(arg_te, &mut impl_param_map))
                .collect();
            // Round 74 Fix #2: parametric alias as trait-impl target —
            // expand the alias by substituting `resolved_args` through
            // the alias's stored target. Mirrors the non-parametric
            // alias path at line ~5617 (which calls
            // `self.tables.resolver.lookup_alias(written)` and walks
            // `info.target` with each `param_var_ids[i]` mapped to a
            // fresh tyvar). Here we map `param_var_ids[i]` → the
            // user-supplied type-arg at the same index.
            //
            // Without this, `trait Show for Pair(a)` (with
            // `type Pair(a) = (a, a)`) produced
            // `Type::Generic("Pair", [tv])` as the self_type. A
            // concrete `(1, 2)` receiver typed as `(Int, Int)` —
            // canonicalised by `resolve_type_expr` of the annotation
            // `Pair(Int)` to `(Int, Int)` — would not unify with the
            // phantom `Generic("Pair", _)`.
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
                    "Range" if resolved_args.len() == 1 => {
                        Type::Range(Box::new(resolved_args.into_iter().next().unwrap()))
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
                    _ => Type::Generic(written, resolved_args),
                }
            }
        };

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
            .insert((trait_key, target_type), self_type.clone());

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
                        resolve(ti.trait_name),
                        resolve(ti.target_type)
                    ),
                    ti.span,
                );
                continue;
            };
            let trait_name = &trait_key;
            // Round 101: the bound's arity must match the trait's
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
            match impl_param_map.get(type_param) {
                Some(ty) => {
                    let resolved = self.apply(ty);
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
                None => {
                    self.error(Code::InvalidTypeAnnotation,
                        format!(
                            "type variable '{}' in impl-level where clause is not declared in the target type arguments; \
                             declare it as a target parameter: `trait {} for {}({}, ...)`",
                            resolve(*type_param),
                            resolve(ti.trait_name),
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
                .insert((trait_key, target_type), impl_level_constraints.clone());
        }

        // GAP (round 35 F5): extraneous trait-impl methods — methods on
        // the impl whose names aren't declared in the trait — used to
        // get silently registered into the method_table. Reject each
        // method whose name is not in the trait's declared method list.
        //
        // GAP (round 35 F6): duplicate method names within a single
        // trait impl used to silently overwrite the earlier definition
        // in the method_table. Track a seen-set and reject the second
        // (and subsequent) occurrences.
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
                        resolve(ti.trait_name),
                        expected,
                        inference::plural(expected, "type argument", "type arguments"),
                        ti.trait_args.len(),
                        resolve(ti.target_type),
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
                    .insert((trait_key, target_type), resolved_trait_args.clone());
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
                        resolve(ti.trait_name),
                        resolve(ti.target_type),
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
                        resolve(ti.trait_name),
                    ),
                    binding.span,
                );
                continue;
            }
            let resolved = self.resolve_type_expr(&binding.ty, &mut impl_param_map);
            // Register into the canonical assoc-binding registry. The
            // registry keys on the canonical target head (so Range and
            // List collapse), parallel to method_table's
            // canonical_head routing above. Round 76 BROKEN T1:
            // the registry refuses self- or mutually-referential
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
                        resolve(ti.trait_name),
                        resolve(ti.target_type),
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
                            resolve(ti.trait_name),
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
                        method.name, ti.trait_name, ti.target_type
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
                        method.name, ti.trait_name
                    ),
                    method.span,
                );
            }
        }

        self.methods_named_like_fields(ti, target_type, trait_info_clone.as_ref());

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
                Some(expected) => match self.unify_types(&written, expected) {
                    Ok(()) => expected.clone(),
                    Err(mismatch) => {
                        self.report_mismatch(mismatch, method.span);
                        written
                    }
                },
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
                // Round 101: bound arity must match the trait's declared
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
                                            ti.trait_name,
                                            ti.target_type,
                                            type_param,
                                            self.show_bound(*trait_name, &resolved_bound_args)
                                        ),
                                    )
                                    .with_help(format!(
                                        "a call through the trait would not owe it: declare the \
                                         bound on the method in trait '{}', or on the impl's header",
                                        ti.trait_name
                                    )),
                                );
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
                MethodEntry {
                    method_type: fn_type,
                    structural: false,
                    trait_name: Some(trait_key),
                    receiver: declared_receiver.unwrap_or_else(|| writes_self(method)),
                    preds: method_constraints.clone(),
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
                });
            }
            if let Type::Fun(params, ret) = body_type {
                self.impl_sigs.insert(
                    (target_type, method.name, trait_key),
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
                method_constraints.extend(Self::bounds_under(info, *name, &mapping));
                self.register_method_entry(
                    target_type,
                    *name,
                    MethodEntry {
                        method_type: substitute_vars(ty, &mapping),
                        structural: false,
                        trait_name: Some(trait_key),
                        receiver: info.receivers.contains(name),
                        preds: method_constraints,
                    },
                );
            }
        }
    }

    /// Report each method the impl `ti` gives its type, the record
    /// `target_type`, that is named like a field of the record: by the
    /// name alone, whatever the field holds. `r.name(..)` is then a call
    /// of what the field holds, on every record, and a method's call on
    /// everything else, whether or not the receiver's type is known
    /// where the call is written.
    ///
    /// A method the impl writes is reported where it is written; a
    /// default method of the trait the impl leaves out, at the impl.
    /// (A supertrait's methods are those of the supertrait's own impl.)
    fn methods_named_like_fields(
        &mut self,
        ti: &TraitImpl,
        target_type: TypeRef,
        info: Option<&TraitInfo>,
    ) {
        let Some(record) = self.tables.records.get(&target_type).cloned() else {
            return;
        };
        // Where the record declares the field `name`, if it has one.
        let field_span = |name: Symbol| {
            let at = record.fields.iter().position(|(field, _)| *field == name)?;
            record.field_spans.get(at).copied()
        };
        // The impl as it is written (its target may be an alias of the
        // record), the record by its name.
        let (tr, of) = (ti.trait_name, ti.target_type);
        let ty = self.show_type(&Type::Generic(target_type, vec![]));
        let mut found: Vec<(Span, String, Symbol, Span)> = Vec::new();
        let mut written: Vec<Symbol> = Vec::new();
        for method in &ti.methods {
            if written.contains(&method.name) {
                continue;
            }
            written.push(method.name);
            // (A method the trait does not declare is reported as that.)
            let declared =
                info.is_none_or(|info| info.methods.iter().any(|(name, _)| *name == method.name));
            if declared && let Some(field) = field_span(method.name) {
                let message = format!(
                    "method '{}' of the impl of '{tr}' for '{of}' is named like a field of '{ty}'",
                    method.name
                );
                found.push((method.name_span, message, method.name, field));
            }
        }
        let defaults = info.into_iter().flat_map(|info| {
            info.methods
                .iter()
                .map(|(name, _)| *name)
                .filter(|name| info.default_method_bodies.contains_key(name))
        });
        for name in defaults {
            if written.contains(&name) {
                continue;
            }
            if let Some(field) = field_span(name) {
                let message = format!(
                    "the impl of '{tr}' for '{of}' takes the default method '{name}' of the \
                     trait, which is named like a field of '{ty}'"
                );
                found.push((ti.span, message, name, field));
            }
        }
        for (span, message, name, field) in found {
            let mut d = Diagnostic::error(Code::MethodNamedLikeField, span, message);
            // (A builtin record's fields are declared in no file.)
            if field.is_in_source() {
                d = d.with_label(field, format!("the field '{name}' of '{ty}'"));
            }
            self.errors.push(d.with_help(
                "a field and a method of one type cannot share a name: rename the method or \
                 the field",
            ));
        }
    }

    /// Report the impl method `method`, written with a number of
    /// parameters other than the `declared` ones of its trait.
    fn impl_method_arity(&mut self, ti: &TraitImpl, method: &FnDecl, declared: usize) {
        let (name, tr) = (method.name, ti.trait_name);
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
        let (name, tr) = (method.name, ti.trait_name);
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
            if let Pred::Trait { tr, args, subject } = pred
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
    fn register_method_entry(&mut self, target_type: TypeRef, method: Symbol, entry: MethodEntry) {
        let trait_key = entry.trait_name.expect("an impl's method has its trait");
        self.tables
            .impl_methods
            .insert(target_type, method, trait_key, entry);
    }

    /// Round 60 G1, extended round 101: a where-clause bound must
    /// supply exactly the trait's declared number of type arguments.
    /// Returns `true` when the arity matches (or the trait is unknown
    /// — the caller has already reported that).
    ///
    /// Both directions are soundness-relevant, not just hygiene: the
    /// parameterized-trait verification in `verify_trait_obligation`
    /// only runs its round-58 positional arg-compatibility zip when
    /// `impl_args.len() == bound_trait_args.len()`, so a
    /// length-mismatched bound like `where a: Cast(Int, String)` on a
    /// one-param `trait Cast(to)` silently degraded to a bare
    /// "implements Cast" check — matching (and dispatching through!)
    /// any `Cast(*)` impl. Zero args on a parameterized trait
    /// additionally leaves the implied params unresolved (the original
    /// round-60 direction).
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

/// Whether the method `method` is written with a receiver: its first
/// parameter is `self`.
fn writes_self(method: &FnDecl) -> bool {
    method.params.first().is_some_and(
        |param| matches!(&param.pattern.kind, PatternKind::Ident(n) if resolve(*n) == "self"),
    )
}
