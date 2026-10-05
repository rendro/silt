use super::*;

impl TypeChecker {
    // ── Generalization / Instantiation ──────────────────────────────

    /// Generalize a type into a scheme by quantifying over free variables
    /// not present in the environment.
    ///
    /// Audit round 19: constraints are no longer unconditionally empty.
    /// We scan `tyvar_trait_constraints` for any recorded constraint whose
    /// tyvar resolves (via `apply`) to one of the quantified vars, and
    /// include those constraints in the resulting scheme. This ensures that
    /// `let f = constrained_fn` and `let f = { x -> constrained_fn(x) }`
    /// preserve where-clause obligations.
    pub(super) fn generalize(&self, env: &TypeEnv, ty: &Type) -> Scheme {
        let ty = self.apply(ty);
        let env_fvs = env.free_vars(self);
        let ty_fvs = free_vars_in(&ty);
        let vars: Vec<TyVar> = ty_fvs
            .into_iter()
            .filter(|v| !env_fvs.contains(v))
            .collect();
        // Collect constraints: for each entry in tyvar_trait_constraints,
        // resolve the tyvar and check if it matches a quantified var.
        let mut constraints: Vec<(TyVar, TraitKey)> = Vec::new();
        if !vars.is_empty() {
            for (&tv, trait_names) in &self.tyvar_trait_constraints {
                let resolved = self.apply(&Type::Var(tv));
                if let Type::Var(rv) = resolved
                    && vars.contains(&rv)
                {
                    for &trait_name in trait_names {
                        if !constraints.contains(&(rv, trait_name)) {
                            constraints.push((rv, trait_name));
                        }
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

    /// Instantiate a scheme by replacing quantified variables with fresh ones.
    pub(super) fn instantiate(&mut self, scheme: &Scheme) -> Type {
        self.instantiate_with_constraints(scheme).0
    }

    /// Instantiate a `MethodEntry`'s template type by generating fresh type
    /// variables for every free type variable in it.
    ///
    /// Method entries store a raw `Type` (not a `Scheme`) for historical
    /// reasons. Without this instantiation, the first call to a polymorphic
    /// auto-derived method (e.g. `equal`) would permanently bind its
    /// parameter type variables via unification, breaking subsequent calls
    /// with different argument types.
    pub(super) fn instantiate_method_type(&mut self, ty: &Type) -> Type {
        let ty = self.apply(ty);
        let fvs = free_vars_in(&ty);
        if fvs.is_empty() {
            return ty;
        }
        let mut mapping: HashMap<TyVar, Type> = HashMap::new();
        for v in fvs {
            mapping.insert(v, self.fresh_var());
        }
        substitute_vars(&ty, &mapping)
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
        // Audit round 19: record constraints on the fresh tyvars so that
        // `generalize` can propagate them into any scheme built from a
        // type that contains these variables (e.g. `let f = constrained_fn`
        // or `let f = { x -> constrained_fn(x) }`).
        for &(tv, trait_name) in &constraints {
            self.tyvar_trait_constraints
                .entry(tv)
                .or_default()
                .push(trait_name);
        }
        // Round 58 soundness fix: propagate trait arg bindings across
        // instantiation so that call sites of `fn f() where a: TryInto(Int)`
        // still see `[Int]` on the fresh tyvar when verifying against
        // impl_trait_args. Without this remap, instantiate would erase the
        // args and verify_trait_obligation would fall back to the bare
        // "implements trait" check, letting mismatched parameterized impls
        // silently satisfy the obligation.
        // Round 73 LATENT (perf): hoist the `trait_arg_bindings` clone
        // out of the inner loop. Previously cloned once per outer
        // iteration (O(M·N) clones for M tyvars and N existing
        // bindings); the inner loop's only reason to clone was to
        // satisfy the borrow checker since `self.trait_arg_bindings`
        // is mutated inside. One clone now serves all iterations.
        let trait_arg_bindings_snapshot = self.trait_arg_bindings.clone();
        for (&old_tv, new_ty) in &mapping {
            if let Type::Var(new_tv) = new_ty {
                for (&(tv, trait_name), args) in trait_arg_bindings_snapshot.iter() {
                    if tv == old_tv {
                        // Round 75 TYPE-1 LATENT: substitute through args
                        // using the same mapping so a polymorphic arg
                        // like `Convertible(b)` carrying a still-quantified
                        // tyvar `b` from the scheme is rewritten to the
                        // FRESH `b'` allocated above. Without this
                        // substitution, two instantiations of the same
                        // scheme would share the scheme's quantified
                        // tyvars across distinct call sites — the
                        // sibling at `:1773-1776` already does this on
                        // the method-table path; the trait-arg
                        // side-channel did not.
                        let new_args: Vec<Type> =
                            args.iter().map(|t| substitute_vars(t, &mapping)).collect();
                        self.trait_arg_bindings
                            .insert((*new_tv, trait_name), new_args);
                    }
                }
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
                // its declaration (`report_unknown_pub_let_types`); its
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

/// Walk two types in parallel and build a mapping from `old` tyvars to
/// `new` tyvars wherever they appear at the same structural position.
/// Used by pass-3 scheme narrowing to remap where-clause constraints
/// from their pass-2 tyvar ids (stored in the original scheme) to the
/// fresh pass-3 tyvar ids that ended up in the narrowed scheme after
/// body inference. Structurally divergent positions are skipped — the
/// caller only uses the mapping for entries whose new tyvar is still
/// free in the narrowed scheme, so spurious matches are harmless.
pub fn align_tyvars(old: &Type, new: &Type) -> HashMap<TyVar, TyVar> {
    let mut map = HashMap::new();
    align_tyvars_into(old, new, &mut map);
    map
}

fn align_tyvars_into(old: &Type, new: &Type, map: &mut HashMap<TyVar, TyVar>) {
    match (old, new) {
        (Type::Var(o), Type::Var(n)) => {
            map.entry(*o).or_insert(*n);
        }
        (Type::Fun(op, or_), Type::Fun(np, nr)) => {
            if op.len() == np.len() {
                for (a, b) in op.iter().zip(np.iter()) {
                    align_tyvars_into(a, b, map);
                }
            }
            align_tyvars_into(or_, nr, map);
        }
        (Type::List(o), Type::List(n)) => align_tyvars_into(o, n, map),
        (Type::Range(o), Type::Range(n)) => align_tyvars_into(o, n, map),
        // Range is a nominal alias for List (see unify arms in
        // src/typechecker/mod.rs). Align element-wise across the
        // List/Range boundary so scheme generalization/instantiation
        // remains sound when a fn returning List(a) flows into a
        // Range-typed binder or vice versa.
        (Type::List(o), Type::Range(n)) | (Type::Range(o), Type::List(n)) => {
            align_tyvars_into(o, n, map)
        }
        (Type::Set(o), Type::Set(n)) => align_tyvars_into(o, n, map),
        (Type::Channel(o), Type::Channel(n)) => align_tyvars_into(o, n, map),
        (Type::Tuple(o), Type::Tuple(n)) if o.len() == n.len() => {
            for (a, b) in o.iter().zip(n.iter()) {
                align_tyvars_into(a, b, map);
            }
        }
        (Type::Map(ok, ov), Type::Map(nk, nv)) => {
            align_tyvars_into(ok, nk, map);
            align_tyvars_into(ov, nv, map);
        }
        (Type::Record(_, of), Type::Record(_, nf)) if of.len() == nf.len() => {
            for ((_, a), (_, b)) in of.iter().zip(nf.iter()) {
                align_tyvars_into(a, b, map);
            }
        }
        (Type::Generic(_, oa), Type::Generic(_, na)) if oa.len() == na.len() => {
            for (a, b) in oa.iter().zip(na.iter()) {
                align_tyvars_into(a, b, map);
            }
        }
        // Round 75 TYPE-2 LATENT: anonymous (structural) records and
        // associated-type projections must walk parallel structure to
        // record old→new tyvar mappings, mirroring the
        // `scheme_narrowed` arms below. Without these arms a method
        // body whose constrained scheme contains a row-poly receiver
        // (`{...r}`) or an `AssocProj` would have its where-clause
        // tyvars stranded on the pre-narrowing ids — the call-site
        // pass-3 remap loop at the trait-impl recheck site (around
        // mod.rs:3696) would then drop those constraints because
        // `remap.get(old_tv)` returns `None`, silently losing the
        // constraint at the narrowed scheme.
        (
            Type::AnonRecord {
                fields: of,
                tail: ot,
            },
            Type::AnonRecord {
                fields: nf,
                tail: nt,
            },
        ) => {
            // Round 79 LATENT TS-L3: walk the new field map by key
            // and align with the old field of the same key when it
            // exists. The previous `of.iter().zip(nf.iter())` form
            // only saw a pair when both maps had the same key at the
            // same position; differing key sets (e.g. pass-3
            // narrowed scheme has additional fields) silently
            // dropped mappings for keys present on both sides at
            // mismatched positions. Iterating by key keeps every
            // common-key alignment regardless of position.
            for (key, n_ty) in nf.iter() {
                if let Some(o_ty) = of.get(key) {
                    align_tyvars_into(o_ty, n_ty, map);
                }
            }
            // Open row tail (`...r`) renaming: pre-narrowed `r` ↦
            // post-narrowed row var if both sides still carry one.
            if let (RowTail::Var(o), RowTail::Var(n)) = (ot, nt) {
                map.entry(*o).or_insert(*n);
            }
        }
        (
            Type::AssocProj {
                receiver: or_,
                trait_name: ot,
                assoc_name: oa,
            },
            Type::AssocProj {
                receiver: nr,
                trait_name: nt,
                assoc_name: na,
            },
        ) if ot == nt && oa == na => {
            align_tyvars_into(or_, nr, map);
        }
        _ => {}
    }
}

/// Round 73 B1 (BROKEN): detect whether `new` is a structural narrowing
/// of `old`. Returns true when the body-pass scheme pinned a tyvar in
/// `old` to a concrete shape in `new` — even when the total `vars.len()`
/// stays equal because a row-tail variable happens to replace the
/// unification variable that got constrained.
///
/// The classic miss: `fn pluck(r) { r.zzznosuchfield }`. Pass-2 generalizes
/// to `Fn(α) -> β` (vars=[α, β]); body inference unifies `α` with
/// `AnonRecord{zzznosuchfield: β, ...γ}`, giving `Fn(AnonRecord{..}) -> β`
/// (vars=[β, γ]). The old `vars.len()` gate compared 2 vs 2 and skipped
/// narrowing — leaving the field-access deferral pool wired to the
/// pass-2 tyvars instead of the pass-3 receiver type, masking the
/// "anon record has no field" diagnostic.
///
/// We walk both trees in lockstep and return true at the first position
/// where `old` is a `Type::Var` and `new` is anything other than a
/// `Type::Var`. (Narrowing is monotone — bound-to-concrete only flows
/// one way; we never see Var→Var rebindings here at the scheme level.)
/// Row-tails follow the same rule: an open `RowTail::Var` narrowing to
/// `RowTail::Closed` (or to a Var bound to a wider AnonRecord) counts
/// as narrowing.
pub(super) fn scheme_narrowed(old: &Type, new: &Type) -> bool {
    match (old, new) {
        // Var → non-Var: narrowed.
        (Type::Var(_), Type::Var(_)) => false,
        (Type::Var(_), _) => true,
        // non-Var → Var: should not happen for monotone narrowing,
        // but treat as "not narrowed" (caller's vars.len() check
        // would have caught any genuine widening as a separate bug).
        (_, Type::Var(_)) => false,
        (Type::Fun(op, or_), Type::Fun(np, nr)) => {
            if op.len() != np.len() {
                return true;
            }
            op.iter().zip(np.iter()).any(|(a, b)| scheme_narrowed(a, b)) || scheme_narrowed(or_, nr)
        }
        (Type::List(o), Type::List(n)) => scheme_narrowed(o, n),
        (Type::Range(o), Type::Range(n)) => scheme_narrowed(o, n),
        (Type::List(o), Type::Range(n)) | (Type::Range(o), Type::List(n)) => scheme_narrowed(o, n),
        (Type::Set(o), Type::Set(n)) => scheme_narrowed(o, n),
        (Type::Channel(o), Type::Channel(n)) => scheme_narrowed(o, n),
        (Type::Tuple(o), Type::Tuple(n)) => {
            o.len() != n.len() || o.iter().zip(n.iter()).any(|(a, b)| scheme_narrowed(a, b))
        }
        (Type::Map(ok, ov), Type::Map(nk, nv)) => {
            scheme_narrowed(ok, nk) || scheme_narrowed(ov, nv)
        }
        (Type::Record(on, of), Type::Record(nn, nf)) => {
            on != nn
                || of.len() != nf.len()
                || of
                    .iter()
                    .zip(nf.iter())
                    .any(|((_, a), (_, b))| scheme_narrowed(a, b))
        }
        (Type::Generic(on, oa), Type::Generic(nn, na)) => {
            on != nn
                || oa.len() != na.len()
                || oa.iter().zip(na.iter()).any(|(a, b)| scheme_narrowed(a, b))
        }
        (
            Type::AnonRecord {
                fields: of,
                tail: ot,
            },
            Type::AnonRecord {
                fields: nf,
                tail: nt,
            },
        ) => {
            // Field set grew (extra row fields pinned), shrunk, or any
            // matching field narrowed — count as narrowing.
            if of.len() != nf.len() {
                return true;
            }
            for ((on, ot_), (nn, nt_)) in of.iter().zip(nf.iter()) {
                if on != nn || scheme_narrowed(ot_, nt_) {
                    return true;
                }
            }
            // Row tail pinned from open to closed.
            matches!((ot, nt), (RowTail::Var(_), RowTail::Closed))
        }
        (
            Type::AssocProj {
                receiver: or_,
                trait_name: ot,
                assoc_name: oa,
            },
            Type::AssocProj {
                receiver: nr,
                trait_name: nt,
                assoc_name: na,
            },
        ) => ot != nt || oa != na || scheme_narrowed(or_, nr),
        // Different head constructors — structural mismatch counts as
        // narrowing (e.g. an old `Type::Var` resolved to a Generic via
        // unification but the comparison happens at a head where the
        // trees diverge).
        _ if std::mem::discriminant(old) != std::mem::discriminant(new) => true,
        _ => false,
    }
}
