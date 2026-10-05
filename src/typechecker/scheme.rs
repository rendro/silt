use super::*;

impl TypeChecker {
    // ── Generalization / Instantiation ──────────────────────────────

    /// Generalize a type into a scheme: quantify its unresolved variables
    /// that nothing outside the scope `exit_level` just left mentions
    /// (see [`TyVarSupply`]). A quantified variable keeps the trait bounds
    /// the scope's calls put on it (`let f = constrained_fn`,
    /// `fn wrap(x) { constrained_fn(x) }`), so each use of the scheme owes
    /// them again.
    pub(super) fn generalize(&mut self, ty: &Type) -> Scheme {
        let ty = self.apply(ty);
        let vars: Vec<TyVar> = free_vars_in(&ty)
            .into_iter()
            .filter(|v| self.tables.vars.is_generalizable(*v))
            .collect();
        let mut constraints: Vec<(TyVar, TraitKey)> = Vec::new();
        if !vars.is_empty() {
            for i in self.closed_mark..self.bound_log.len() {
                let (tv, trait_name) = self.bound_log[i];
                if let Type::Var(rv) = self.apply(&Type::Var(tv))
                    && vars.contains(&rv)
                    && !constraints.contains(&(rv, trait_name))
                {
                    constraints.push((rv, trait_name));
                    // The bound's trait arguments are kept under the
                    // variable the bound was made for.
                    if rv != tv
                        && let Some(args) = self.trait_arg_bindings.get(&(tv, trait_name)).cloned()
                    {
                        self.trait_arg_bindings
                            .entry((rv, trait_name))
                            .or_insert(args);
                    }
                }
            }
        }
        Scheme {
            vars,
            ty,
            constraints,
            optional_last_param: false,
        }
    }

    /// The scheme of a top-level function whose body is checked: `ty`,
    /// its type as its body sees it, with the annotation variables
    /// quantified again and bounded as its `where` clauses say (`bounds`).
    pub(super) fn generalize_fn(&mut self, ty: &Type, bounds: &[(TyVar, TraitKey)]) -> Scheme {
        let (ty, rigid_vars) = unrigidify(&self.apply(ty));
        let mut scheme = self.generalize(&ty);
        for v in rigid_vars {
            if !scheme.vars.contains(&v) {
                scheme.vars.push(v);
            }
        }
        for bound in bounds {
            if scheme.vars.contains(&bound.0) && !scheme.constraints.contains(bound) {
                scheme.constraints.push(*bound);
            }
        }
        scheme
    }

    /// Forget the bounds recorded in the scope `exit_level` just left,
    /// and the checks that wait for a variable (`finalize_deferred_checks`),
    /// once everything the scope defines is generalised: except those on a
    /// variable that is still unresolved and belongs to an outer binding
    /// (a top-level `let` that is not generalised), which are owed, and
    /// checked, when a later definition decides it.
    pub(super) fn settle_bounds(&mut self) {
        let recorded = self.bound_log.split_off(self.closed_mark);
        for (tv, trait_name) in recorded {
            if self.waits_for_outer(&Type::Var(tv)) {
                self.bound_log.push((tv, trait_name));
            }
        }
        let mut fields = std::mem::take(&mut self.pending_field_accesses);
        fields.retain(|(obj_ty, ..)| self.waits_for_outer(obj_ty));
        self.pending_field_accesses = fields;
        let mut numeric = std::mem::take(&mut self.pending_numeric_checks);
        numeric.retain(|(ty, ..)| self.waits_for_outer(ty));
        self.pending_numeric_checks = numeric;
        let mut questions = std::mem::take(&mut self.pending_question_marks);
        questions.retain(|(inner_ty, ..)| self.waits_for_outer(inner_ty));
        self.pending_question_marks = questions;
        let mut bounds = std::mem::take(&mut self.pending_where_constraints);
        bounds.retain(|pending| self.waits_for_outer(&Type::Var(pending.tyvar)));
        self.pending_where_constraints = bounds;
    }

    /// Whether `ty` is a variable still unresolved that the scope
    /// `exit_level` just left does not own.
    fn waits_for_outer(&self, ty: &Type) -> bool {
        matches!(self.apply(ty), Type::Var(v) if !self.tables.vars.is_generalizable(v))
    }

    /// Instantiate a scheme by replacing quantified variables with fresh ones.
    pub(super) fn instantiate(&mut self, scheme: &Scheme) -> Type {
        self.instantiate_with_constraints(scheme).0
    }

    /// Instantiate a `MethodEntry`'s template type AND its where-clause
    /// constraints through a single shared substitution, so the returned
    /// `(Type, Vec<(TyVar, TraitKey)>)` pair uses consistent fresh TyVars.
    ///
    /// Constraint TyVars that appear in `method_type`'s free-var set map
    /// through the same fresh-var substitution as the type itself; any
    /// constraint TyVars not in the free set (edge case: a constraint on
    /// a binder that doesn't appear in the method's signature — possible
    /// in principle for phantom binders, but not reachable today) get
    /// their own fresh substitution so downstream handling stays uniform.
    ///
    /// Callers push the returned constraints into `pending_where_constraints`
    /// with the current call-site span; the finalize pass then checks each
    /// obligation against concrete types and caller-active constraints,
    /// same as fn-call sites registered at the Call arm of `infer_expr`.
    pub(super) fn instantiate_method_entry(
        &mut self,
        entry: &MethodEntry,
    ) -> (Type, Vec<(TyVar, TraitKey, Vec<Type>)>) {
        let ty = self.apply(&entry.method_type);
        let mut fvs: Vec<TyVar> = free_vars_in(&ty);
        for (tv, _, args) in &entry.method_constraints {
            if !fvs.contains(tv) {
                fvs.push(*tv);
            }
            for arg in args {
                for v in free_vars_in(arg) {
                    if !fvs.contains(&v) {
                        fvs.push(v);
                    }
                }
            }
        }
        if fvs.is_empty() {
            // Side-channel propagation: even on the no-fresh-var fast
            // path, surface the bound's args via `trait_arg_bindings`
            // so call sites looking up `(tv, trait)` find them.
            for (tv, trait_name, args) in &entry.method_constraints {
                if !args.is_empty() {
                    self.trait_arg_bindings
                        .insert((*tv, *trait_name), args.clone());
                }
            }
            return (ty, entry.method_constraints.clone());
        }
        let mut mapping: HashMap<TyVar, Type> = HashMap::new();
        for v in fvs {
            mapping.insert(v, self.fresh_var());
        }
        let new_ty = substitute_vars(&ty, &mapping);
        let new_constraints: Vec<(TyVar, TraitKey, Vec<Type>)> = entry
            .method_constraints
            .iter()
            .map(|(tv, trait_name, args)| {
                let new_tv = match mapping.get(tv) {
                    Some(Type::Var(new_tv)) => *new_tv,
                    _ => *tv,
                };
                let new_args: Vec<Type> =
                    args.iter().map(|t| substitute_vars(t, &mapping)).collect();
                (new_tv, *trait_name, new_args)
            })
            .collect();
        // Round 58 soundness fix (extension): propagate the bound's
        // trait args under the fresh tyvar key so the call-site
        // `bound_args` lookup in `dispatch_method_entry` finds them
        // when checking the obligation against `impl_trait_args`.
        // Without this, impl-level / method-level where-clause
        // bounds with trait args would silently pass when matched
        // against a mismatched impl (the BROKEN-1 / BROKEN-2 hole).
        for (tv, trait_name, args) in &new_constraints {
            if !args.is_empty() {
                self.trait_arg_bindings
                    .insert((*tv, *trait_name), args.clone());
            }
        }
        (new_ty, new_constraints)
    }

    /// Instantiate a scheme and remap its where clause constraints.
    /// Returns (instantiated_type, remapped_constraints).
    pub(super) fn instantiate_with_constraints(
        &mut self,
        scheme: &Scheme,
    ) -> (Type, Vec<(TyVar, TraitKey)>) {
        let mut mapping: HashMap<TyVar, Type> = HashMap::new();
        for &v in &scheme.vars {
            mapping.insert(v, self.fresh_var());
        }
        let ty = substitute_vars(&scheme.ty, &mapping);
        let constraints: Vec<(TyVar, TraitKey)> = scheme
            .constraints
            .iter()
            .map(|(v, trait_name)| match mapping.get(v) {
                Some(Type::Var(new_v)) => (*new_v, *trait_name),
                _ => (*v, *trait_name),
            })
            .collect();
        // The fresh variables owe the scheme's bounds: `generalize` puts
        // them in the scheme of whatever is generalised over them, and
        // the bound's trait arguments (`where a: TryInto(Int)`) follow
        // each to its fresh variable, instantiated like the type.
        for (&(old, _), &(new, trait_name)) in scheme.constraints.iter().zip(&constraints) {
            self.bound_log.push((new, trait_name));
            if let Some(args) = self.trait_arg_bindings.get(&(old, trait_name)) {
                let new_args: Vec<Type> =
                    args.iter().map(|t| substitute_vars(t, &mapping)).collect();
                self.trait_arg_bindings.insert((new, trait_name), new_args);
            }
        }
        (ty, constraints)
    }

    /// Enter the scheme of each definition the module declares in the
    /// session's tables, from the module's scope `env`: what an importer
    /// of the module reads.
    pub(super) fn enter_schemes(&mut self, env: &TypeEnv) {
        let Some(defs) = self.defs.clone() else {
            return;
        };
        for id in defs.of_module(self.module) {
            let def = defs.get(*id);
            // A variant's scheme is entered when its type is.
            if matches!(
                def.kind,
                crate::defs::DefKind::Variant { .. }
                    | crate::defs::DefKind::Trait(_)
                    | crate::defs::DefKind::TypeAlias
            ) {
                continue;
            }
            if let Some(scheme) = env.lookup(def.name) {
                let mut scheme = scheme.clone();
                // A `pub let` whose type is partly unknown is reported at
                // its declaration (`report_unknown_let_types`); its
                // importers see the unknown part as an error type, so no
                // importer fixes it.
                if def.kind == crate::defs::DefKind::Let
                    && def.vis == crate::defs::Vis::Pub
                    && !self.is_cell
                {
                    let ty = self.apply(&scheme.ty);
                    let mut rows = Vec::new();
                    row_tail_vars(&ty, &mut rows);
                    let unknown: HashMap<TyVar, Type> = free_vars_in(&ty)
                        .into_iter()
                        .filter(|v| !scheme.vars.contains(v) && !rows.contains(v))
                        .map(|v| (v, Type::Error))
                        .collect();
                    if !unknown.is_empty() {
                        scheme.ty = substitute_vars(&ty, &unknown);
                    }
                }
                self.tables.schemes.insert(*id, scheme);
            }
        }
    }

    /// The scheme of a definition a name resolves to that is not one
    /// of the module's own functions and `let`s (those are in the
    /// module's scope): a variant's constructor, a type written as a
    /// value, a member of another module, a builtin.
    pub(super) fn def_scheme(
        &self,
        res: Option<crate::defs::Res>,
        env: &TypeEnv,
    ) -> Option<Scheme> {
        let Some(crate::defs::Res::Def(id)) = res else {
            return None;
        };
        let def = self.def(id)?;
        if def.module.is_builtin() {
            return builtin_scheme(&def);
        }
        if self.names_rejected(res, def.name) {
            return Some(Scheme::mono(Type::Error));
        }
        if def.module == self.module && !matches!(def.kind, crate::defs::DefKind::Variant { .. }) {
            return env.lookup(def.name).cloned();
        }
        self.tables.schemes.get(&id).cloned()
    }

    /// The scheme of a method called through its type (`Pt.show(p)`):
    /// its template, generalized over its free type variables, with its
    /// where-clause constraints.
    pub(super) fn method_scheme(entry: &MethodEntry) -> Scheme {
        let mut constraints: Vec<(TyVar, TraitKey)> = Vec::new();
        for (tv, trait_name, _) in &entry.method_constraints {
            if !constraints.contains(&(*tv, *trait_name)) {
                constraints.push((*tv, *trait_name));
            }
        }
        Scheme {
            vars: free_vars_in(&entry.method_type),
            ty: entry.method_type.clone(),
            constraints,
            optional_last_param: false,
        }
    }
}
