//! Type inference for expressions, statements, and patterns.
//!
//! This module contains the core inference logic: infer_expr, infer_stmt,
//! bind_pattern, check_pattern, and check_fn_body.

use super::suggest::suggest_similar;
use super::*;

/// GAP (round 17 F5): pick the singular or plural form of a word
/// based on `n`. Used to render arity/field/binding counts in
/// typechecker diagnostics without the awkward "1 argument(s)" that
/// tooling and users had been complaining about.
pub(super) fn plural<'a>(n: usize, singular: &'a str, plural_form: &'a str) -> &'a str {
    if n == 1 { singular } else { plural_form }
}

/// BROKEN (round 26 B2): render a set of symbols for a user-facing
/// diagnostic. `BTreeSet<Symbol>` formatted with `{:?}` leaks the
/// interner's Debug form (`Symbol(6: "x")`) — awful to read and exposes
/// implementation detail. This helper resolves each symbol to its
/// source-level name and joins them inside `{}` braces in sorted order
/// (the BTreeSet iteration order is already lexicographic on symbol
/// id, so we sort by the resolved string to keep output stable across
/// interning permutations). Example output: `{x}`, `{a, b, c}`, `{}`.
pub(super) fn format_symbol_set(set: &BTreeSet<Symbol>) -> String {
    let mut names: Vec<String> = set.iter().map(|s| resolve(*s)).collect();
    names.sort();
    format!("{{{}}}", names.join(", "))
}

/// Format an "undefined variable '<typo>'" error message with an
/// optional "did you mean `<cand>`?" hint appended as a `help:` body
/// line so `SourceError::Display` renders it as a `= help:` continuation
/// below the caret. Sourced candidates come from every in-scope name
/// the typechecker's env chain exposes (locals, fn params, top-level
/// decls, stdlib builtins) — the caller hands us `env`. If no candidate
/// passes the suggest-similar threshold, we emit the plain error.
///
/// Closes round-17 deferred finding #4: see `src/typechecker/suggest.rs`.
/// Lock: tests/diagnostic_suggestion_tests.rs.
pub(super) fn format_undefined_variable_message(
    name: Symbol,
    env: &TypeEnv,
    suffix: &str,
) -> String {
    let name_str = resolve(name);
    let base = if suffix.is_empty() {
        format!("undefined variable '{name_str}'")
    } else {
        format!("undefined variable '{name_str}' {suffix}")
    };
    // If the identifier is a keyword borrowed from another language
    // that silt has no equivalent for, the edit-distance suggestion
    // isn't useful — attach a targeted recommendation instead. These
    // hints used to live in the parser's G1 guard, but that fired on
    // any parenthesized reference and broke formatter roundtrip;
    // resolving them here keeps the UX while staying syntax-neutral.
    let foreign_keyword_hint = match name_str.as_str() {
        "break" | "continue" => {
            Some("silt has no 'break'/'continue' — return early or restructure the recursion")
        }
        _ => None,
    };
    if let Some(hint) = foreign_keyword_hint {
        return format!("{base}\nhelp: {hint}");
    }
    let mut candidates = BTreeSet::new();
    env.collect_names(&mut candidates);
    // Strip fully-qualified builtin names like `list.map` — those are
    // not useful suggestions for a bare identifier typo. Also drop the
    // pseudo-binding for `self` which is handled by the Ident arm.
    let candidate_strs: Vec<String> = candidates
        .iter()
        .map(|s| resolve(*s))
        .filter(|s| !s.contains('.') && s != "self")
        .collect();
    if let Some(hint) = suggest_similar(&name_str, candidate_strs.iter()) {
        format!("{base}\nhelp: did you mean `{hint}`?")
    } else {
        base
    }
}

/// Format an "unknown function '<field>' on module '<module>'" error
/// with a "did you mean `<cand>`?" hint when one of the module's builtin
/// functions is a close edit-distance match. See
/// `src/module.rs::builtin_module_functions` for the candidate source.
pub(super) fn format_unknown_module_function_message(field: Symbol, module_str: &str) -> String {
    let field_str = resolve(field);
    let base = format!("unknown function '{field_str}' on module '{module_str}'");
    let fns = crate::module::builtin_module_functions(module_str);
    let consts = crate::module::builtin_module_constants(module_str);
    // Merge functions and constants so e.g. `math.pj` gets suggested
    // `pi`. The header says "unknown function" either way — the hint is
    // still useful.
    let mut merged: Vec<&str> = fns.into_iter().chain(consts).collect();
    merged.sort();
    merged.dedup();
    if let Some(hint) = suggest_similar(&field_str, merged.iter()) {
        format!("{base}\nhelp: did you mean `{hint}`?")
    } else {
        base
    }
}

/// GAP (round 26 L5): append a "did you mean `<cand>`?" hint when a
/// record-field diagnostic mentions a name that's close in edit
/// distance to one of the record's declared fields. Used by every
/// "record 'X' has no field 'Y'" / "unknown field 'Y' in X" site so
/// `u.nam` on `type User { name, age }` gets `did you mean \`name\`?`.
/// Delegates to `suggest::suggest_similar` for the threshold policy
/// (matches the round-24 short-name tightening — single-edit only for
/// names up to 5 chars; scaled for longer names).
pub(super) fn format_record_field_suggestion(
    base: String,
    field: Symbol,
    record_fields: &[(Symbol, Type)],
) -> String {
    let field_str = resolve(field);
    let candidates: Vec<String> = record_fields.iter().map(|(n, _)| resolve(*n)).collect();
    if let Some(hint) = suggest_similar(&field_str, candidates.iter()) {
        format!("{base}\nhelp: did you mean `{hint}`?")
    } else {
        base
    }
}

/// GAP (round 23 #3): append a "did you mean `<cand>`?" hint to an
/// "unknown method '<field>' on <Type>" diagnostic when the method table
/// has a close edit-distance match for the given type name. The
/// method_table is keyed on `(type_name, method_name)`; we walk it once
/// to collect every method registered on the target type and feed them
/// to the existing `suggest::suggest_similar` policy.
pub(super) fn format_unknown_method_message(
    field: Symbol,
    display_type_name: &str,
    method_table: &HashMap<(Symbol, Symbol), MethodEntry>,
    table_key: Symbol,
) -> String {
    let field_str = resolve(field);
    let base = format!("unknown method '{field_str}' on {display_type_name}");
    let candidates: Vec<String> = method_table
        .keys()
        .filter(|(ty, _)| *ty == table_key)
        .map(|(_, m)| resolve(*m).to_string())
        .collect();
    if let Some(hint) = suggest_similar(&field_str, candidates.iter()) {
        format!("{base}\nhelp: did you mean `{hint}`?")
    } else {
        base
    }
}

impl TypeChecker {
    /// B4 helper: does the enclosing function's active where-clause
    /// constraints cover `trait_name` for the type variable at the
    /// resolved call-site tyvar? We can't simply walk `apply` from the
    /// callee's tyvar, because `unify` may bind the enclosing fn's
    /// constraint-var to the callee's fresh var (giving a chain
    /// `enclosing_tv → callee_tv`); `apply` on the callee side returns
    /// `callee_tv` and active_constraints is keyed on `enclosing_tv`.
    /// So we iterate the active constraints and, for each `(tv, traits)`,
    /// check whether `apply(Type::Var(tv))` lands on the same resolved
    /// tyvar as `resolved`, on either side of the chain.
    fn covered_by_active_constraint(&self, resolved: &Type, trait_name: Symbol) -> bool {
        let resolved = self.apply(resolved);
        let resolved_var = match &resolved {
            Type::Var(v) => *v,
            _ => return false,
        };
        for (tv, traits) in &self.active_constraints {
            if !traits.contains(&trait_name) {
                continue;
            }
            // Direct match: the enclosing fn's constraint tyvar is
            // itself the resolved tyvar.
            if *tv == resolved_var {
                return true;
            }
            // Transitive: apply the enclosing constraint's tyvar and
            // see if it lands on the same resolved tyvar as the call
            // site. This handles the common unify direction where
            // the enclosing tyvar gets bound to the callee's fresh
            // var.
            let applied = self.apply(&Type::Var(*tv));
            if let Type::Var(v) = applied
                && v == resolved_var
            {
                return true;
            }
        }
        false
    }

    /// Dispatch a method lookup through a `MethodEntry`, returning the
    /// instantiated method type AND plumbing any impl- or method-level
    /// where-clause constraints into `pending_where_constraints` for
    /// the finalize-pass check.
    ///
    /// Receiver-method syntax (`receiver.method(...)`) goes through
    /// `method_table` rather than `env`, so prior rounds' fn-call where
    /// enforcement never fired on it. This helper is the single place
    /// that lifts method_table dispatch into the same constraint-check
    /// machinery used by ordinary fn calls: each constraint tyvar gets
    /// a fresh substitution via `instantiate_method_entry`, and the
    /// caller's span + active_constraints get snapshotted for finalize.
    ///
    /// The `receiver_ty` is unified with the method's first parameter
    /// (the `self` slot) BEFORE the constraint check, so impl-level
    /// where clauses see the concrete receiver-element type when the
    /// caller passes a monomorphic receiver. Without this unification,
    /// the impl's `a_fresh` TyVar would stay unbound through the rest of
    /// inference — the Call arm applies args to `params[1..]` only on
    /// method calls, so the `self` param is the one slot no other path
    /// touches.
    ///
    /// For concrete-receiver call sites, the constraint fires immediately
    /// via `type_name_for_impl`; for unresolved-tyvar receivers it defers
    /// via `pending_where_constraints` and resolves during
    /// `finalize_deferred_checks` after all Calls have unified args.
    pub(super) fn dispatch_method_entry(
        &mut self,
        entry: &MethodEntry,
        method_name: Symbol,
        receiver_ty: &Type,
        span: Span,
    ) -> Type {
        self.last_field_access_was_method = true;
        let (instantiated_ty, constraints) = self.instantiate_method_entry(entry);
        // Unify the receiver with the method's self param so concrete
        // receiver element types flow into the impl's tyvars before the
        // constraint check below.
        if let Type::Fun(params, _) = &instantiated_ty
            && let Some(self_param) = params.first()
        {
            self.unify(receiver_ty, self_param, span);
        }
        for (tv, trait_name) in constraints {
            let resolved = self.apply(&Type::Var(tv));
            match &resolved {
                Type::Error | Type::Never => {}
                Type::Var(v) => {
                    // Still a fresh tyvar — either the caller will unify
                    // it with a concrete receiver (handled by finalize)
                    // or the enclosing fn already declared the same
                    // constraint via its own where clause (handled now).
                    if !self.covered_by_active_constraint(&resolved, trait_name) {
                        self.pending_where_constraints.push(PendingWhereConstraint {
                            tyvar: *v,
                            trait_name,
                            callee_fn_name: Some(method_name),
                            span,
                            active_snapshot: self.active_constraints.clone(),
                            param_tyvars: self.current_fn_param_tyvars.clone(),
                        });
                    }
                }
                _ => {
                    // Concrete receiver — check trait impl exists now,
                    // recursively walking the impl's own where clauses
                    // against the receiver's type arguments.
                    self.verify_trait_obligation(trait_name, &resolved, span);
                }
            }
        }
        self.apply(&instantiated_ty)
    }

    /// Expand a list of trait names to include all transitive supertraits.
    ///
    /// Walks each trait's `supertraits` chain: `[Ordered]` with
    /// `trait Ordered: Equal` returns `[Ordered, Equal]`. Used when
    /// populating `active_constraints` so that `where a: Ordered` enables
    /// the `Equal` methods on `a` inside the body — the FieldAccess arm
    /// for `Type::Var(v)` only checks methods of traits listed in
    /// `active_constraints[v]`.
    ///
    /// Cycle-safe: a `seen` set prevents infinite loops on pathological
    /// inputs like `trait A: B { } trait B: A { }`. Cycle behaviour at
    /// the data level is otherwise unspecified for v0.6 — we don't reject
    /// cycles, we just don't blow the stack on them.
    pub(super) fn expand_with_supertraits(&self, traits: &[Symbol]) -> Vec<Symbol> {
        use std::collections::HashSet;
        let mut expanded = Vec::new();
        let mut stack: Vec<Symbol> = traits.to_vec();
        let mut seen: HashSet<Symbol> = HashSet::new();
        while let Some(t) = stack.pop() {
            if seen.insert(t) {
                expanded.push(t);
                if let Some(info) = self.traits.get(&t) {
                    stack.extend(info.supertraits.iter().copied());
                }
            }
        }
        expanded
    }

    // ── Check function body ─────────────────────────────────────────

    pub(super) fn check_fn_body(&mut self, f: &mut FnDecl, env: &TypeEnv) {
        let _ = self.check_fn_body_with_name(f, env, f.name);
    }

    /// Like `check_fn_body`, but looks up the registered scheme under an
    /// explicit name. Used for trait impl methods, which are registered in
    /// the environment under `TargetType.method_name` rather than the bare
    /// `method_name`. Returns the body-constrained function type (with all
    /// substitutions applied) so callers can write it back into derived
    /// tables like `method_table`.
    pub(super) fn check_fn_body_with_name(
        &mut self,
        f: &mut FnDecl,
        env: &TypeEnv,
        lookup_name: Symbol,
    ) -> Option<Type> {
        let mut local_env = env.child();

        // Validate where clauses
        for (type_param, trait_name) in &f.where_clauses {
            if !self.traits.contains_key(trait_name) {
                self.error(
                    format!(
                        "unknown trait '{}' in where clause for '{}'",
                        trait_name, type_param
                    ),
                    f.span,
                );
            }
        }

        // Look up the function's registered type and instantiate it
        let fn_scheme = match env.lookup(lookup_name) {
            Some(s) => s.clone(),
            None => return None, // already reported
        };
        let (fn_type, constraints) = self.instantiate_with_constraints(&fn_scheme);
        let fn_type = self.apply(&fn_type);

        let (param_types, ret_type) = match &fn_type {
            Type::Fun(params, ret) => (params.clone(), *ret.clone()),
            _ => return None,
        };

        // Populate active constraints so method resolution on type variables
        // can check trait methods during body inference. Each declared
        // constraint expands to include the transitive supertrait closure
        // — `where a: Ordered` with `trait Ordered: Equal` makes both
        // `Ordered`'s and `Equal`'s methods callable on `a`.
        let prev_constraints = std::mem::take(&mut self.active_constraints);
        for (tv, trait_name) in &constraints {
            for expanded in self.expand_with_supertraits(&[*trait_name]) {
                let entry = self.active_constraints.entry(*tv).or_default();
                if !entry.contains(&expanded) {
                    entry.push(expanded);
                }
            }
        }

        // B4: capture the instantiated param tyvars so call-site where-
        // clause checks can determine whether a pending obligation
        // touches the enclosing fn's own polymorphism (vs. an unrelated
        // top-level or downstream Var that will resolve via pass-3
        // narrowing). We store just the Var IDs — concrete params are
        // not of interest here.
        let prev_fn_param_tyvars = std::mem::take(&mut self.current_fn_param_tyvars);
        for pt in &param_types {
            if let Type::Var(v) = self.apply(pt) {
                self.current_fn_param_tyvars.push(v);
            }
        }

        // Bind parameters
        for (i, param) in f.params.iter().enumerate() {
            if let Some(ty) = param_types.get(i) {
                self.bind_pattern(&param.pattern, ty, &mut local_env, f.span);
            }
        }

        // Set the expected return type for return and ? validation
        let prev_return_type = self.current_return_type.take();
        self.current_return_type = Some(ret_type.clone());

        // Infer the body and unify with declared return type
        let body_type = self.infer_expr(&mut f.body, &mut local_env);
        self.unify(&body_type, &ret_type, f.body.span);

        // Record the body-constrained function type for scheme narrowing
        let constrained_params: Vec<Type> = param_types.iter().map(|t| self.apply(t)).collect();
        let constrained_ret = self.apply(&ret_type);
        let constrained_fn = Type::Fun(constrained_params, Box::new(constrained_ret));
        self.fn_body_types
            .insert(lookup_name, constrained_fn.clone());

        // Restore previous constraints and return type
        self.current_return_type = prev_return_type;
        self.active_constraints = prev_constraints;
        self.current_fn_param_tyvars = prev_fn_param_tyvars;

        Some(constrained_fn)
    }

    // ── Deferred check finalization ─────────────────────────────────

    /// Resolve any deferred field-access and numeric-op checks that were
    /// recorded against type variables during inference. Called after all
    /// function bodies have been processed so we can see the final
    /// substitution.
    ///
    /// Important architectural note: Silt uses Algorithm W with
    /// let-polymorphism, so the body of a polymorphic function is
    /// inferred once using fresh instantiated vars that are NEVER unified
    /// with call-site concrete types (each call instantiates *another*
    /// set of fresh vars). This means that if a polymorphic function's
    /// body uses an ambiguous field access or arithmetic op, the body-
    /// inference-time vars stay unresolved at finalization. We cannot
    /// emit errors on those, because that would reject legitimate
    /// polymorphic definitions like `fn add(a, b) { a + b }` or
    /// `fn get_x(obj) { obj.x }`. Instead, the deferred-check pass ONLY
    /// fires when the operand / receiver has resolved to a concrete,
    /// non-conforming type (e.g. a monomorphic `let s = "hi"; -s`).
    pub(super) fn finalize_deferred_checks(&mut self) {
        // B4: pending field accesses on type variables. Only flag when
        // the receiver resolved to a concrete type.
        let pending_fields = std::mem::take(&mut self.pending_field_accesses);
        for (obj_ty, field, result_ty, span) in pending_fields {
            let resolved = self.apply(&obj_ty);
            match &resolved {
                Type::Error | Type::Never => {}
                Type::Var(_) => {
                    // Polymorphic / unresolved — leave alone (see above).
                }
                Type::Record(_, rec_fields) => {
                    if let Some((_, field_ty)) = rec_fields.iter().find(|(n, _)| *n == field) {
                        let ft = field_ty.clone();
                        self.unify(&result_ty, &ft, span);
                    } else {
                        self.error(format!("unknown field '{field}' on type {resolved}"), span);
                    }
                }
                Type::Generic(type_name, type_args) => {
                    // User-declared records with or without type parameters
                    // are represented as Type::Generic(name, args). Look up
                    // the record definition and validate the field.
                    let type_name = *type_name;
                    let type_args = type_args.clone();
                    if let Some(rec_info) = self.records.get(&type_name).cloned()
                        && let Some((_, ft)) = rec_info.fields.iter().find(|(n, _)| *n == field)
                    {
                        // Same fresh-var fallback as in infer_expr (T1 audit fix):
                        // never return the template TyVar; if the caller's
                        // type_args are missing/mismatched, use fresh vars.
                        let field_ty = if let Some(param_var_ids) =
                            self.record_param_var_ids.get(&type_name).cloned()
                        {
                            let mapping: HashMap<TyVar, Type> =
                                if type_args.len() == param_var_ids.len() {
                                    param_var_ids
                                        .iter()
                                        .zip(type_args.iter())
                                        .map(|(&v, t)| (v, t.clone()))
                                        .collect()
                                } else {
                                    param_var_ids
                                        .iter()
                                        .map(|&v| (v, self.fresh_var()))
                                        .collect()
                                };
                            let substituted = substitute_vars(ft, &mapping);
                            self.apply(&substituted)
                        } else {
                            self.apply(ft)
                        };
                        self.unify(&result_ty, &field_ty, span);
                        continue;
                    }
                    // Also check the method table for trait methods.
                    if let Some(entry) = self.method_table.get(&(type_name, field)).cloned() {
                        let instantiated = self.dispatch_method_entry(&entry, field, &obj_ty, span);
                        let method_ty = self.apply(&instantiated);
                        // Method types include `self` as the first param.
                        // When the call site originally saw this field
                        // access as an unknown Var, it unified the var with
                        // a function type built from the *explicit* args
                        // only (no receiver). Strip `self` when adapting.
                        let result_resolved = self.apply(&result_ty);
                        match (&result_resolved, &method_ty) {
                            (
                                Type::Fun(call_params, call_ret),
                                Type::Fun(method_params, method_ret),
                            ) if method_params.len() == call_params.len() + 1 => {
                                for (cp, mp) in call_params.iter().zip(method_params.iter().skip(1))
                                {
                                    self.unify(cp, mp, span);
                                }
                                self.unify(call_ret, method_ret, span);
                            }
                            _ => {
                                self.unify(&result_ty, &method_ty, span);
                            }
                        }
                        continue;
                    }
                    self.error(
                        format!("unknown field or method '{field}' on type {type_name}"),
                        span,
                    );
                }
                _ => {
                    self.error(
                        format!("unknown field or method '{field}' on type {resolved}"),
                        span,
                    );
                }
            }
        }

        // B5 / B2 / B3: pending numeric / comparison checks on type variables.
        let pending_numeric = std::mem::take(&mut self.pending_numeric_checks);
        for (ty, op_desc, span) in pending_numeric {
            let resolved = self.apply(&ty);
            // Early-exit for types that never participate in operator errors.
            if matches!(resolved, Type::Error | Type::Never) {
                continue;
            }
            // If the operand is still a type variable at the end of inference,
            // it's either (a) a function parameter that's genuinely polymorphic
            // (e.g. `fn add(a, b) { a + b }` that's never called) — in which
            // case the fn was never monomorphized so we can't validate, or
            // (b) a body inference var from a polymorphic fn template whose
            // call sites were processed using fresh instantiated vars (so the
            // template var never got constrained). Both cases are harmless;
            // the concrete error would fire on the call site's operand.
            if matches!(resolved, Type::Var(_)) {
                continue;
            }
            // Classify the op based on its recorded tag (string literals set
            // at the binary-op or unary-op site).
            let valid = match op_desc {
                // Arithmetic that allows strings (Add).
                "'+'" => is_valid_arith_operand(&resolved, true),
                // Numeric-only arithmetic.
                "'-'" | "'*'" | "'/'" | "'%'" | "unary '-'" => {
                    is_valid_arith_operand(&resolved, false)
                        && !matches!(resolved, Type::String | Type::Var(_))
                }
                // Equality: anything comparable.
                "'=='/'!='" => is_valid_compare_operand(&resolved, true),
                // Ordering comparison: stricter domain.
                "ordering comparison" => is_valid_compare_operand(&resolved, false),
                _ => true,
            };
            if !valid {
                let domain = match op_desc {
                    "'+'" => "Int, Float, ExtFloat, or String",
                    "'-'" | "'*'" | "'/'" | "'%'" | "unary '-'" => "Int, Float, or ExtFloat",
                    "'=='/'!='" => "a comparable type",
                    "ordering comparison" => {
                        "Int, Float, ExtFloat, String, List, Range, Record, or Variant"
                    }
                    _ => "a valid operand",
                };
                self.error(
                    format!("operator {op_desc} requires {domain}, got '{resolved}'"),
                    span,
                );
            }
        }

        // B4: deferred where-clause obligations. At call site we push
        // `(tyvar, trait, fn_name, span, active_constraints_snapshot,
        // fn_param_tyvars_snapshot)` for any call whose resolved type
        // arg was still a type variable at the time. Re-apply the
        // substitution now — if the var resolved to a concrete type
        // with a matching impl the obligation is satisfied; if it
        // resolved to another type variable equivalent to one of the
        // enclosing fn's param tyvars AND that param is not covered
        // by the enclosing fn's own where-clause, emit a clean
        // propagation error. Otherwise (the var is unrelated to the
        // enclosing fn's polymorphism — e.g. a top-level let whose
        // scheme was over-general at pass 2) drop it silently; the
        // value is already concrete from the caller's perspective.
        let pending_where = std::mem::take(&mut self.pending_where_constraints);
        for pending in pending_where {
            let PendingWhereConstraint {
                tyvar,
                trait_name,
                callee_fn_name,
                span,
                active_snapshot,
                param_tyvars,
            } = pending;
            let resolved = self.apply(&Type::Var(tyvar));
            if matches!(resolved, Type::Error | Type::Never) {
                continue;
            }
            if self.type_name_for_impl(&resolved).is_some() {
                // Recursively walk the matched impl's where clauses
                // against the resolved type's arguments.
                self.verify_trait_obligation(trait_name, &resolved, span);
                continue;
            }
            if let Type::Var(v) = &resolved {
                // Still a type variable. First, test equivalence to any
                // of the enclosing fn's param tyvars at the time of the
                // call — if none match, this is not the enclosing fn's
                // concern (e.g. a top-level `a = id(5)` whose scheme was
                // over-general during pass 2). In that case, drop.
                let mut touches_fn_param = false;
                for &pv in &param_tyvars {
                    if pv == *v {
                        touches_fn_param = true;
                        break;
                    }
                    let applied = self.apply(&Type::Var(pv));
                    if let Type::Var(av) = applied
                        && av == *v
                    {
                        touches_fn_param = true;
                        break;
                    }
                }
                if !touches_fn_param {
                    continue;
                }

                // The var is linked to the enclosing fn's polymorphism.
                // Check the snapshot of active constraints captured at
                // the original call site for the matching trait.
                let mut covered = false;
                for (tv, traits) in &active_snapshot {
                    if !traits.contains(&trait_name) {
                        continue;
                    }
                    if *tv == *v {
                        covered = true;
                        break;
                    }
                    let applied = self.apply(&Type::Var(*tv));
                    if let Type::Var(av) = applied
                        && av == *v
                    {
                        covered = true;
                        break;
                    }
                }
                if !covered {
                    let fn_label = callee_fn_name
                        .map(|s| format!("'{}'", resolve(s)))
                        .unwrap_or_else(|| "<callee>".to_string());
                    self.error(
                        format!(
                            "enclosing function does not declare constraint required by call to {fn_label}: `a: {trait_name}`"
                        ),
                        span,
                    );
                }
            }
        }
    }

    // ── Let-pattern refutability check ─────────────────────────────
    //
    // B1: a `let` binding must not destructure a variant that is only
    // one of several enum constructors. `let Square(n) = shape` where
    // `Shape = Circle(Int) | Square(String)` was previously accepted
    // by the typechecker and produced silent payload corruption at
    // runtime — the VM read `Circle(5)`'s Int payload into `n` and
    // the error cascaded into a misleading `+ Int String` at the
    // first use of `n`. Walk the pattern and reject any Constructor
    // pattern whose parent enum has more than one variant. Match arms
    // do NOT call this check — refutable patterns are legal there.
    pub(super) fn reject_refutable_constructor_in_let(&mut self, pattern: &Pattern, span: Span) {
        match &pattern.kind {
            PatternKind::Constructor(name, sub_pats) => {
                if let Some(enum_name) = self.variant_to_enum.get(name).cloned()
                    && let Some(enum_info) = self.enums.get(&enum_name).cloned()
                    && enum_info.variants.len() > 1
                {
                    self.error(
                        format!(
                            "refutable pattern in `let`: constructor '{}' is only one of {} variants of enum '{}'; use a `match` or `when let ... else` instead",
                            name,
                            enum_info.variants.len(),
                            enum_name
                        ),
                        span,
                    );
                }
                for p in sub_pats {
                    self.reject_refutable_constructor_in_let(p, span);
                }
            }
            PatternKind::Tuple(pats) => {
                for p in pats {
                    self.reject_refutable_constructor_in_let(p, span);
                }
            }
            PatternKind::List(elems, rest) => {
                for p in elems {
                    self.reject_refutable_constructor_in_let(p, span);
                }
                if let Some(r) = rest {
                    self.reject_refutable_constructor_in_let(r, span);
                }
            }
            PatternKind::Record { fields, .. } => {
                for (_, sub_pat) in fields {
                    if let Some(p) = sub_pat {
                        self.reject_refutable_constructor_in_let(p, span);
                    }
                }
            }
            PatternKind::Or(alts) => {
                for p in alts {
                    self.reject_refutable_constructor_in_let(p, span);
                }
            }
            // Non-constructor patterns are out of scope for this
            // B1 fix. Literal/map/pin patterns also happen to be
            // refutable in `let`, but existing programs already
            // reach them through the existing typechecker paths;
            // leave them alone to avoid regressing unrelated
            // tests.
            _ => {}
        }
    }

    // ── Pattern type binding ────────────────────────────────────────

    /// Bind names in a pattern to their types in the environment.
    pub(super) fn bind_pattern(
        &mut self,
        pattern: &Pattern,
        ty: &Type,
        env: &mut TypeEnv,
        span: Span,
    ) {
        match &pattern.kind {
            PatternKind::Wildcard => {}
            PatternKind::Ident(name) => {
                env.define(*name, Scheme::mono(ty.clone()));
            }
            PatternKind::Int(_) => {}
            PatternKind::Float(_) => {}
            PatternKind::Bool(_) => {}
            PatternKind::StringLit(..) => {}
            PatternKind::Tuple(pats) => {
                // BROKEN (round 15): bind_pattern Pattern::Tuple used to
                // silently fall through to fresh vars when the scrutinee
                // wasn't already a tuple, letting `let (a, b) = 42` slip
                // past the type checker and blow up at runtime. Build the
                // expected tuple shape up front and either unify against
                // the scrutinee (general mismatch) or emit a dedicated
                // arity error whose wording reads from the pattern's
                // perspective ("expected 3, got 2"). The two message
                // orderings differ because unify's tuple-tuple arm puts
                // the first arg as "expected", while its fallback
                // general-mismatch arm puts the second arg as "expected".
                //
                // BROKEN (round 23 #1): the empty-tuple pattern `()` is
                // the unit pattern. `resolve_type_expr` normalizes the
                // empty tuple type expr to `Type::Unit` (mod.rs around
                // the `TypeExpr::Tuple` arm). Unifying the scrutinee
                // against `Type::Tuple(vec![])` instead of `Type::Unit`
                // produced a nonsense "expected (), got ()" diagnostic
                // because the two types render identically but aren't
                // equal. Match the type-expr side of the language and
                // unify against `Type::Unit` when `pats.is_empty()`.
                if pats.is_empty() {
                    self.unify(ty, &Type::Unit, span);
                    return;
                }
                let resolved_pre = self.apply(ty);
                if let Type::Tuple(scrutinee_elems) = &resolved_pre {
                    if scrutinee_elems.len() == pats.len() {
                        let elems = scrutinee_elems.clone();
                        for (p, t) in pats.iter().zip(elems.iter()) {
                            self.bind_pattern(p, t, env, span);
                        }
                    } else {
                        // Arity mismatch — emit the pattern-centric error
                        // directly so the message reads "expected <N>, got
                        // <M>" from the pattern's point of view.
                        self.error(
                            format!(
                                "tuple length mismatch: expected {}, got {}",
                                pats.len(),
                                scrutinee_elems.len()
                            ),
                            span,
                        );
                        for p in pats {
                            let tv = self.fresh_var();
                            self.bind_pattern(p, &tv, env, span);
                        }
                    }
                } else {
                    // Non-tuple scrutinee (or an unresolved var). Unify
                    // against a fresh tuple shape so a) Var scrutinees get
                    // the correct tuple type, and b) concrete non-tuple
                    // scrutinees produce "expected (..), got <type>".
                    let shape_elems: Vec<Type> = pats.iter().map(|_| self.fresh_var()).collect();
                    let shape = Type::Tuple(shape_elems.clone());
                    self.unify(ty, &shape, span);
                    // After unify, if the scrutinee unified into a tuple
                    // (via a fresh var), recurse properly; otherwise fall
                    // back to the shape vars.
                    let resolved_post = self.apply(ty);
                    match &resolved_post {
                        Type::Tuple(elems) if elems.len() == pats.len() => {
                            let elems = elems.clone();
                            for (p, t) in pats.iter().zip(elems.iter()) {
                                self.bind_pattern(p, t, env, span);
                            }
                        }
                        _ => {
                            for (p, t) in pats.iter().zip(shape_elems.iter()) {
                                self.bind_pattern(p, t, env, span);
                            }
                        }
                    }
                }
            }
            PatternKind::Constructor(name, sub_pats) => {
                // Look up the constructor to find inner types
                if let Some(enum_name) = self.variant_to_enum.get(name).cloned()
                    && let Some(enum_info) = self.enums.get(&enum_name).cloned()
                    && let Some(var_info) = enum_info.variants.iter().find(|v| v.name == *name)
                {
                    if sub_pats.len() != var_info.field_types.len() {
                        let expected = var_info.field_types.len();
                        // Fix A: point the caret at the constructor pattern
                        // itself, not at the enclosing let/when scrutinee.
                        self.error(
                            format!(
                                "constructor '{}' expects {} {}, but pattern has {}",
                                name,
                                expected,
                                plural(expected, "field", "fields"),
                                sub_pats.len()
                            ),
                            pattern.span,
                        );
                    }
                    // BROKEN (round 15): unify the scrutinee against
                    // `Generic(enum_name, fresh args)` BEFORE recursing,
                    // so `let Ok(x) = 42` is caught at typecheck rather
                    // than deferred to a runtime `DestructVariant` crash.
                    // Try to reuse existing type args if the scrutinee is
                    // already a Generic of the right enum.
                    let resolved_pre = self.apply(ty);
                    let type_args: Vec<Type> = match &resolved_pre {
                        Type::Generic(n, args) if *n == enum_name => args.clone(),
                        _ => enum_info.params.iter().map(|_| self.fresh_var()).collect(),
                    };
                    let enum_shape = Type::Generic(enum_name, type_args.clone());
                    self.unify(ty, &enum_shape, span);
                    for (i, sp) in sub_pats.iter().enumerate() {
                        if i < var_info.field_types.len() {
                            let field_ty = substitute_enum_params(
                                &var_info.field_types[i],
                                &enum_info.param_var_ids,
                                &type_args,
                            );
                            self.bind_pattern(sp, &field_ty, env, span);
                        } else {
                            let tv = self.fresh_var();
                            self.bind_pattern(sp, &tv, env, span);
                        }
                    }
                    return;
                }
                // LATENT (round 26 L1): mirror round-23's check_pattern
                // behavior — if `name` refers to a declared record type,
                // emit the record-syntax hint instead of the generic
                // "undefined constructor" message. The previous fallback
                // only existed on check_pattern, so `let Circle(r) = c`
                // gave a confusing error when the real issue was shape,
                // not existence.
                // LATENT (round 26 L3): also point the caret at
                // `pattern.span`, not the outer `span` (the outer span
                // is the enclosing let/match scrutinee).
                if self.records.contains_key(name) {
                    self.error(
                        format!(
                            "'{name}' is a record type; use record-pattern syntax `{name} {{ ... }}` instead of constructor-pattern syntax"
                        ),
                        pattern.span,
                    );
                } else {
                    self.error(
                        format!("undefined constructor '{name}' in pattern"),
                        pattern.span,
                    );
                }
                for sp in sub_pats {
                    let tv = self.fresh_var();
                    self.bind_pattern(sp, &tv, env, span);
                }
            }
            PatternKind::List(pats, rest) => {
                let elem_ty = self.fresh_var();
                let list_ty = Type::List(Box::new(elem_ty.clone()));
                self.unify(ty, &list_ty, span);
                let resolved_elem = self.apply(&elem_ty);
                for p in pats {
                    self.bind_pattern(p, &resolved_elem, env, span);
                }
                if let Some(rest_pat) = rest {
                    let rest_ty = Type::List(Box::new(resolved_elem));
                    self.bind_pattern(rest_pat, &rest_ty, env, span);
                }
            }
            PatternKind::Record { name, fields, .. } => {
                // BROKEN-4: `let Name { f } = v` used to silently bind `f`
                // to a fresh TyVar when the base wasn't a record, or when
                // the field didn't exist. Both were deferred to VM runtime
                // errors. Reject them at the type-check stage.
                let resolved = self.apply(ty);
                let pattern_record: Option<(Symbol, Vec<(Symbol, Type)>)> = if let Some(rec_name) =
                    name
                    && let Some(rec_info) = self.records.get(rec_name).cloned()
                {
                    let instantiated_fields: Vec<(Symbol, Type)> = if let Some(param_var_ids) =
                        self.record_param_var_ids.get(rec_name).cloned()
                    {
                        let mapping: HashMap<TyVar, Type> = param_var_ids
                            .iter()
                            .map(|&v| (v, self.fresh_var()))
                            .collect();
                        rec_info
                            .fields
                            .iter()
                            .map(|(n, t)| (*n, substitute_vars(t, &mapping)))
                            .collect()
                    } else {
                        rec_info.fields.clone()
                    };
                    Some((*rec_name, instantiated_fields))
                } else if let Some(rec_name) = name {
                    self.error(
                        format!("undefined record type '{rec_name}' in pattern"),
                        span,
                    );
                    None
                } else {
                    None
                };
                if let Some((pname, pfields)) = &pattern_record {
                    let rec_ty = Type::Record(*pname, pfields.clone());
                    self.unify(ty, &rec_ty, span);
                }
                let resolved = self.apply(&resolved);

                // R1 (round 15): when the scrutinee's type surfaces as
                // `Type::Generic(name, args)` and `name` names a declared
                // record (common for records passed through fn boundaries
                // — `resolve_type_expr` maps user record annotations to
                // `Type::Generic`), instantiate the record's field
                // templates and bind sub-patterns directly. The named
                // pattern case — `let Pair { a, b } = p` — has already
                // computed these fields in `pattern_record`; prefer those
                // so the declared and inferred instantiations stay linked.
                let generic_record_fields: Option<(Symbol, Vec<(Symbol, Type)>)> =
                    if let Type::Generic(type_name, type_args) = &resolved
                        && let Some(rec_info) = self.records.get(type_name).cloned()
                    {
                        let fields = if let Some((pname, pfields)) = &pattern_record
                            && *pname == *type_name
                        {
                            pfields.clone()
                        } else if let Some(param_var_ids) =
                            self.record_param_var_ids.get(type_name).cloned()
                        {
                            let mapping: HashMap<TyVar, Type> =
                                if type_args.len() == param_var_ids.len() {
                                    param_var_ids
                                        .iter()
                                        .zip(type_args.iter())
                                        .map(|(&v, t)| (v, t.clone()))
                                        .collect()
                                } else {
                                    param_var_ids
                                        .iter()
                                        .map(|&v| (v, self.fresh_var()))
                                        .collect()
                                };
                            rec_info
                                .fields
                                .iter()
                                .map(|(n, t)| (*n, substitute_vars(t, &mapping)))
                                .collect()
                        } else {
                            rec_info.fields.clone()
                        };
                        Some((*type_name, fields))
                    } else {
                        None
                    };

                if let Type::Record(rec_name, field_types) = &resolved {
                    for (field_name, sub_pat) in fields {
                        if let Some((_, ft)) = field_types.iter().find(|(n, _)| n == field_name) {
                            if let Some(sp) = sub_pat {
                                self.bind_pattern(sp, ft, env, span);
                            } else {
                                env.define(*field_name, Scheme::mono(ft.clone()));
                            }
                        } else {
                            // GAP (round 26 L5): append a did-you-mean
                            // hint when a near edit-distance field
                            // exists on this record.
                            let base =
                                format!("record '{rec_name}' has no field '{field_name}'");
                            self.error(
                                format_record_field_suggestion(base, *field_name, field_types),
                                span,
                            );
                            if let Some(sp) = sub_pat {
                                let tv = self.fresh_var();
                                self.bind_pattern(sp, &tv, env, span);
                            } else {
                                let tv = self.fresh_var();
                                env.define(*field_name, Scheme::mono(tv));
                            }
                        }
                    }
                } else if let Some((rec_name, field_types)) = generic_record_fields {
                    for (field_name, sub_pat) in fields {
                        if let Some((_, ft)) = field_types.iter().find(|(n, _)| n == field_name) {
                            if let Some(sp) = sub_pat {
                                self.bind_pattern(sp, ft, env, span);
                            } else {
                                env.define(*field_name, Scheme::mono(ft.clone()));
                            }
                        } else {
                            // GAP (round 26 L5): same hint on the generic
                            // resolution path.
                            let base =
                                format!("record '{rec_name}' has no field '{field_name}'");
                            self.error(
                                format_record_field_suggestion(base, *field_name, &field_types),
                                span,
                            );
                            if let Some(sp) = sub_pat {
                                let tv = self.fresh_var();
                                self.bind_pattern(sp, &tv, env, span);
                            } else {
                                let tv = self.fresh_var();
                                env.define(*field_name, Scheme::mono(tv));
                            }
                        }
                    }
                } else if matches!(resolved, Type::Error | Type::Var(_) | Type::Never) {
                    for (field_name, sub_pat) in fields {
                        if let Some(sp) = sub_pat {
                            let tv = self.fresh_var();
                            self.bind_pattern(sp, &tv, env, span);
                        } else {
                            let tv = self.fresh_var();
                            env.define(*field_name, Scheme::mono(tv));
                        }
                    }
                } else {
                    self.error(
                        format!(
                            "record pattern requires a record value, but '{resolved}' is not a record type"
                        ),
                        span,
                    );
                    for (field_name, sub_pat) in fields {
                        if let Some(sp) = sub_pat {
                            let tv = self.fresh_var();
                            self.bind_pattern(sp, &tv, env, span);
                        } else {
                            let tv = self.fresh_var();
                            env.define(*field_name, Scheme::mono(tv));
                        }
                    }
                }
            }
            PatternKind::Or(alts) => {
                // Validate that all alternatives bind the same set of variables.
                if alts.len() >= 2 {
                    let first_vars: BTreeSet<Symbol> =
                        collect_pattern_vars(&alts[0]).into_iter().collect();
                    for (i, alt) in alts.iter().enumerate().skip(1) {
                        let alt_vars: BTreeSet<Symbol> =
                            collect_pattern_vars(alt).into_iter().collect();
                        if first_vars != alt_vars {
                            // BROKEN (round 26 B2): `{:?}` on a BTreeSet<Symbol>
                            // leaks `Symbol(N: "x")` debug output into a
                            // user-facing diagnostic. Render the sets as
                            // sorted comma-separated lists of resolved names.
                            self.error(
                                format!(
                                    "or-pattern alternatives must bind the same variables; \
                                     first alternative binds {}, alternative {} binds {}",
                                    format_symbol_set(&first_vars),
                                    i + 1,
                                    format_symbol_set(&alt_vars)
                                ),
                                span,
                            );
                        }
                    }
                }
                // Bind each alternative into a scratch sub-environment so we
                // can collect the per-alternative type for every variable the
                // or-pattern binds, then unify those types pairwise. This
                // enforces that the alternatives agree on each binding's
                // type (e.g. `Left(x) | Right(x)` where `x: Int` on one side
                // and `x: String` on the other must be rejected).
                let mut per_alt_types: Vec<HashMap<Symbol, Type>> = Vec::with_capacity(alts.len());
                for alt in alts {
                    let mut alt_env = env.child();
                    self.bind_pattern(alt, ty, &mut alt_env, span);
                    let mut names: HashMap<Symbol, Type> = HashMap::new();
                    for name in collect_pattern_vars(alt) {
                        if let Some(scheme) = alt_env.bindings.get(&name) {
                            names.insert(name, scheme.ty.clone());
                        }
                    }
                    per_alt_types.push(names);
                }
                // Pairwise-unify the first alt's types with each other alt.
                if per_alt_types.len() >= 2 {
                    let (first, rest) = per_alt_types.split_first().unwrap();
                    for other in rest {
                        for (name, first_ty) in first {
                            if let Some(other_ty) = other.get(name) {
                                let a = self.apply(first_ty);
                                let b = self.apply(other_ty);
                                if a != b {
                                    // Try to unify — if they're still
                                    // incompatible, report a targeted error.
                                    let err_count = self.errors.len();
                                    self.unify(&a, &b, span);
                                    if self.errors.len() > err_count {
                                        // Replace the generic unify error with
                                        // a clearer or-pattern-specific one.
                                        self.errors.truncate(err_count);
                                        self.error(
                                            format!(
                                                "or-pattern alternatives bind '{}' to conflicting types: {} vs {}",
                                                name, a, b
                                            ),
                                            span,
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                // Finally, bind the first alternative's variables into the
                // real environment so downstream code sees them.
                if let Some(first_alt) = alts.first() {
                    self.bind_pattern(first_alt, ty, env, span);
                }
            }
            PatternKind::Range(_, _) => {
                self.unify(ty, &Type::Int, span);
            }
            PatternKind::FloatRange(_, _) => {
                self.unify(ty, &Type::Float, span);
            }
            PatternKind::Map(entries) => {
                // L3: Map patterns are currently restricted to String keys at
                // parse time — `PatternKind::Map(Vec<(String, Pattern)>)` in
                // src/ast.rs. If the scrutinee has a non-String key type, give
                // a targeted error rather than the cryptic unification failure.
                let val_ty = self.fresh_var();
                let resolved_scrutinee = self.apply(ty);
                if let Type::Map(existing_key, _) = &resolved_scrutinee {
                    let existing_key = self.apply(existing_key);
                    if !matches!(existing_key, Type::String | Type::Var(_) | Type::Error) {
                        self.error(
                            format!(
                                "map patterns currently only match string keys; your scrutinee has key type '{existing_key}'"
                            ),
                            span,
                        );
                    }
                }
                let key_ty = Type::String;
                let map_ty = Type::Map(Box::new(key_ty), Box::new(val_ty.clone()));
                self.unify(ty, &map_ty, span);
                let resolved_val = self.apply(&val_ty);
                for (_key, pat) in entries {
                    self.bind_pattern(pat, &resolved_val, env, span);
                }
            }
            PatternKind::Pin(name) => {
                // Pin does not introduce a new binding — it checks against an
                // existing variable.  Look it up in the parent (pre-match) scope
                // first, then fall back to the current scope for when/let contexts.
                let found = env
                    .parent
                    .as_ref()
                    .and_then(|p| p.lookup(*name).cloned())
                    .or_else(|| env.lookup(*name).cloned());
                if let Some(scheme) = found {
                    let pinned_ty = self.instantiate(&scheme);
                    self.unify(ty, &pinned_ty, span);
                } else {
                    // LATENT (round 26 L4): point the caret at the pin
                    // pattern, not the enclosing match/let scrutinee.
                    let msg = format_undefined_variable_message(*name, env, "in pin pattern");
                    self.error(msg, pattern.span);
                }
            }
        }
    }

    // ── Expression type inference ───────────────────────────────────

    pub(super) fn infer_expr(&mut self, expr: &mut Expr, env: &mut TypeEnv) -> Type {
        let span = expr.span;
        let ty = match &mut expr.kind {
            ExprKind::Int(_) => Type::Int,
            ExprKind::Float(_) => Type::Float,
            ExprKind::Bool(_) => Type::Bool,
            ExprKind::StringLit(..) => Type::String,
            ExprKind::Unit => Type::Unit,

            ExprKind::StringInterp(parts) => {
                // Each part is either a literal or an expression
                for part in parts {
                    if let StringPart::Expr(e) = part {
                        let expr_span = e.span;
                        let t = self.infer_expr(e, env);
                        let resolved = self.apply(&t);
                        if let Some(type_name) = self.type_name_for_impl(&resolved)
                            && !self
                                .trait_impl_set
                                .contains(&(intern("Display"), type_name))
                        {
                            self.error(
                                format!(
                                    "type '{}' does not implement Display (required for string interpolation)",
                                    type_name
                                ),
                                expr_span,
                            );
                        }
                    }
                }
                Type::String
            }

            ExprKind::List(elems) => {
                if elems.is_empty() {
                    let tv = self.fresh_var();
                    Type::List(Box::new(tv))
                } else {
                    // Infer each element first (without unifying), so we can
                    // produce a single targeted "list elements must have the
                    // same type" error pointing at the first mismatching
                    // element instead of the old "expected X, got Y" which
                    // read as if the user had declared the first type.
                    let mut elem_infos: Vec<(Type, Span, bool)> = Vec::with_capacity(elems.len());
                    for elem in elems.iter_mut() {
                        match elem {
                            ListElem::Single(e) => {
                                let t = self.infer_expr(e, env);
                                elem_infos.push((t, e.span, false));
                            }
                            ListElem::Spread(e) => {
                                let t = self.infer_expr(e, env);
                                elem_infos.push((t, e.span, true));
                            }
                        }
                    }

                    // Establish the "first element type" once, up front.
                    let first_ty = {
                        let (t, _, is_spread) = &elem_infos[0];
                        if *is_spread {
                            // Spread contributes a List(inner); extract inner
                            // for the "first element" description.
                            let applied = self.apply(t);
                            match applied {
                                Type::List(inner) => *inner,
                                _ => t.clone(),
                            }
                        } else {
                            t.clone()
                        }
                    };

                    let elem_type = self.fresh_var();
                    self.unify(&elem_type, &first_ty, elem_infos[0].1);

                    for (idx, (t, espan, is_spread)) in elem_infos.iter().enumerate() {
                        let err_count = self.errors.len();
                        if *is_spread {
                            let expected = Type::List(Box::new(elem_type.clone()));
                            self.unify(&expected, t, *espan);
                        } else {
                            self.unify(&elem_type, t, *espan);
                        }
                        if self.errors.len() > err_count {
                            // Replace the raw unify diagnostic with a
                            // clearer list-level message.
                            self.errors.truncate(err_count);
                            let elem_ty = if *is_spread {
                                let applied = self.apply(t);
                                match applied {
                                    Type::List(inner) => *inner,
                                    other => other,
                                }
                            } else {
                                self.apply(t)
                            };
                            let first_resolved = self.apply(&first_ty);
                            self.error(
                                format!(
                                    "list elements must have the same type: first element is {}, but element {} is {}",
                                    first_resolved,
                                    idx + 1,
                                    elem_ty
                                ),
                                *espan,
                            );
                        }
                    }

                    Type::List(Box::new(elem_type))
                }
            }

            ExprKind::Map(entries) => {
                if entries.is_empty() {
                    let k = self.fresh_var();
                    let v = self.fresh_var();
                    Type::Map(Box::new(k), Box::new(v))
                } else {
                    let mut iter = entries.iter_mut();
                    let first_entry = iter.next().unwrap();
                    let first_k = self.infer_expr(&mut first_entry.0, env);
                    let first_v = self.infer_expr(&mut first_entry.1, env);
                    for (k, v) in iter {
                        let k_span = k.span;
                        let v_span = v.span;
                        let kt = self.infer_expr(k, env);
                        let vt = self.infer_expr(v, env);
                        self.unify(&first_k, &kt, k_span);
                        self.unify(&first_v, &vt, v_span);
                    }
                    Type::Map(Box::new(first_k), Box::new(first_v))
                }
            }

            ExprKind::SetLit(elems) => {
                if elems.is_empty() {
                    let tv = self.fresh_var();
                    Type::Set(Box::new(tv))
                } else {
                    let elem_type = self.fresh_var();
                    for e in elems.iter_mut() {
                        let t = self.infer_expr(e, env);
                        self.unify(&elem_type, &t, e.span);
                    }
                    Type::Set(Box::new(elem_type))
                }
            }

            ExprKind::Tuple(elems) => {
                let types: Vec<Type> = elems.iter_mut().map(|e| self.infer_expr(e, env)).collect();
                Type::Tuple(types)
            }

            ExprKind::Ident(name) => {
                let name = *name;
                if let Some(scheme) = env.lookup(name) {
                    let scheme = scheme.clone();
                    self.instantiate(&scheme)
                } else if name == intern("self") {
                    // `self` is resolved at runtime — allow without error
                    self.fresh_var()
                } else {
                    let msg = format_undefined_variable_message(name, env, "");
                    self.error(msg, span);
                    self.fresh_var()
                }
            }

            ExprKind::FieldAccess(obj, field) => {
                self.last_field_access_was_method = false;
                let field = *field;
                // Capture module name before mutable borrow for inference
                let module_name = if let ExprKind::Ident(n) = &obj.kind {
                    Some(*n)
                } else {
                    None
                };

                // Check for module-style access first (e.g., string.split)
                // Do this BEFORE inferring obj to avoid false "possibly undefined variable" warnings
                // for stdlib module names like list, string, map, io, etc.
                if let Some(module_name) = module_name {
                    let qualified = intern(&format!("{module_name}.{field}"));
                    if let Some(scheme) = env.lookup(qualified) {
                        let scheme = scheme.clone();
                        let result = self.instantiate(&scheme);
                        let resolved = self.apply(&result);
                        expr.ty = Some(resolved.clone());
                        return resolved;
                    }
                    // Qualified variant access: `EnumName.Variant`. Variants
                    // are registered globally by bare name, so when the LHS
                    // is an enum type and the RHS is one of its variants,
                    // resolve to the variant's scheme. Handles both unit
                    // variants used as values and variants about to be
                    // called with args (the outer `Call` path reuses the
                    // resolved scheme).
                    if self.enums.contains_key(&module_name) {
                        match self.variant_to_enum.get(&field).copied() {
                            Some(owner) if owner == module_name => {
                                if let Some(scheme) = env.lookup(field).cloned() {
                                    let result = self.instantiate(&scheme);
                                    let resolved = self.apply(&result);
                                    expr.ty = Some(resolved.clone());
                                    return resolved;
                                }
                            }
                            Some(owner) => {
                                self.error(
                                    format!(
                                        "'{}' is not a variant of enum '{}' (it belongs to '{}')",
                                        resolve(field),
                                        resolve(module_name),
                                        resolve(owner),
                                    ),
                                    span,
                                );
                                let fresh = self.fresh_var();
                                expr.ty = Some(fresh.clone());
                                return fresh;
                            }
                            None => {
                                self.error(
                                    format!(
                                        "enum '{}' has no variant '{}'",
                                        resolve(module_name),
                                        resolve(field),
                                    ),
                                    span,
                                );
                                let fresh = self.fresh_var();
                                expr.ty = Some(fresh.clone());
                                return fresh;
                            }
                        }
                    }
                    // G5: when `<module>` is a known builtin module (list,
                    // string, map, ...) and `<member>` is not registered,
                    // emit a specific "unknown function on module" error
                    // BEFORE falling through to the generic obj-inference
                    // path (which would misleadingly report `undefined
                    // variable '<module>'`). We deliberately do NOT short
                    // circuit: we emit the error and return a fresh var
                    // so downstream inference continues.
                    let module_str = resolve(module_name);
                    if crate::module::is_builtin_module(&module_str) {
                        let msg = format_unknown_module_function_message(field, &module_str);
                        self.error(msg, span);
                        let fresh = self.fresh_var();
                        expr.ty = Some(fresh.clone());
                        return fresh;
                    }
                }

                // Could be record.field — infer the object type
                let obj_ty = self.infer_expr(obj, env);
                let obj_ty = self.apply(&obj_ty);

                // Field / method access
                match &obj_ty {
                    Type::Record(rec_name, fields) => {
                        // Direct field access first
                        if let Some((_, ft)) = fields.iter().find(|(n, _)| *n == field) {
                            ft.clone()
                        } else if let Some(entry) =
                            self.method_table.get(&(*rec_name, field)).cloned()
                        {
                            let instantiated =
                                self.dispatch_method_entry(&entry, field, &obj_ty, span);
                            let resolved = self.apply(&instantiated);
                            expr.ty = Some(resolved.clone());
                            return resolved;
                        } else {
                            // GAP (round 26 L5): append a did-you-mean
                            // hint when a near edit-distance field
                            // exists on this record.
                            let base =
                                format!("record {rec_name} has no field or method '{field}'");
                            self.error(
                                format_record_field_suggestion(base, field, fields),
                                span,
                            );
                            Type::Error
                        }
                    }
                    Type::Generic(type_name, type_args) => {
                        // Check record field definitions, substituting type parameters
                        if let Some(rec_info) = self.records.get(type_name).cloned()
                            && let Some((_, ft)) = rec_info.fields.iter().find(|(n, _)| *n == field)
                        {
                            // Substitute the record's type parameters with concrete type args.
                            // When `type_args` is empty but the record is actually
                            // parameterized (which can now only happen if unification
                            // reached here with mismatched arity), instantiate fresh
                            // type vars for each param — never return the shared
                            // template TyVar, which would get mutated across uses
                            // (T1 audit fix; mirrors the check_pattern path).
                            let resolved = if let Some(param_var_ids) =
                                self.record_param_var_ids.get(type_name).cloned()
                            {
                                let mapping: HashMap<TyVar, Type> =
                                    if type_args.len() == param_var_ids.len() {
                                        param_var_ids
                                            .iter()
                                            .zip(type_args.iter())
                                            .map(|(&v, t)| (v, t.clone()))
                                            .collect()
                                    } else {
                                        // Arity mismatch (already reported elsewhere).
                                        // Fall back to fresh vars to avoid leaking
                                        // the shared template TyVars.
                                        param_var_ids
                                            .iter()
                                            .map(|&v| (v, self.fresh_var()))
                                            .collect()
                                    };
                                let substituted = substitute_vars(ft, &mapping);
                                self.apply(&substituted)
                            } else {
                                self.apply(ft)
                            };
                            expr.ty = Some(resolved.clone());
                            return resolved;
                        }
                        // Check method table (trait methods)
                        if let Some(entry) = self.method_table.get(&(*type_name, field)).cloned() {
                            let instantiated =
                                self.dispatch_method_entry(&entry, field, &obj_ty, span);
                            let resolved = self.apply(&instantiated);
                            expr.ty = Some(resolved.clone());
                            return resolved;
                        }
                        // Legacy fallback: check TypeEnv for "TypeName.method"
                        let key = intern(&format!("{type_name}.{field}"));
                        if let Some(scheme) = env.lookup(key) {
                            let scheme = scheme.clone();
                            let result = self.instantiate(&scheme);
                            let resolved = self.apply(&result);
                            expr.ty = Some(resolved.clone());
                            return resolved;
                        }
                        self.error(
                            format!("unknown field or method '{field}' on type {type_name}"),
                            span,
                        );
                        Type::Error
                    }
                    // Primitive types — check method table for trait methods
                    Type::Int | Type::Float | Type::Bool | Type::String | Type::Unit => {
                        let type_name = match &obj_ty {
                            Type::Int => intern("Int"),
                            Type::Float => intern("Float"),
                            Type::Bool => intern("Bool"),
                            Type::String => intern("String"),
                            Type::Unit => intern("()"),
                            _ => unreachable!(),
                        };
                        if let Some(entry) = self.method_table.get(&(type_name, field)).cloned() {
                            let instantiated =
                                self.dispatch_method_entry(&entry, field, &obj_ty, span);
                            let resolved = self.apply(&instantiated);
                            expr.ty = Some(resolved.clone());
                            return resolved;
                        }
                        // GAP (round 23 #3): append "did you mean ...?"
                        // when a near edit-distance method is registered
                        // on this type. Keep the existing "on type <Name>"
                        // header so prior-lock tests that match only the
                        // header prefix still pass; the hint is appended
                        // on its own `help:` line.
                        let display = format!("type {type_name}");
                        self.error(
                            format_unknown_method_message(
                                field,
                                &display,
                                &self.method_table,
                                type_name,
                            ),
                            span,
                        );
                        Type::Error
                    }
                    // Collection types
                    Type::List(_) => {
                        if let Some(entry) =
                            self.method_table.get(&(intern("List"), field)).cloned()
                        {
                            let instantiated =
                                self.dispatch_method_entry(&entry, field, &obj_ty, span);
                            let resolved = self.apply(&instantiated);
                            expr.ty = Some(resolved.clone());
                            return resolved;
                        }
                        self.error(
                            format_unknown_method_message(
                                field,
                                "List",
                                &self.method_table,
                                intern("List"),
                            ),
                            span,
                        );
                        Type::Error
                    }
                    Type::Tuple(_) => {
                        if let Some(entry) =
                            self.method_table.get(&(intern("Tuple"), field)).cloned()
                        {
                            let instantiated =
                                self.dispatch_method_entry(&entry, field, &obj_ty, span);
                            let resolved = self.apply(&instantiated);
                            expr.ty = Some(resolved.clone());
                            return resolved;
                        }
                        self.error(
                            format_unknown_method_message(
                                field,
                                "Tuple",
                                &self.method_table,
                                intern("Tuple"),
                            ),
                            span,
                        );
                        Type::Error
                    }
                    Type::Map(_, _) => {
                        if let Some(entry) = self.method_table.get(&(intern("Map"), field)).cloned()
                        {
                            let instantiated =
                                self.dispatch_method_entry(&entry, field, &obj_ty, span);
                            let resolved = self.apply(&instantiated);
                            expr.ty = Some(resolved.clone());
                            return resolved;
                        }
                        self.error(
                            format_unknown_method_message(
                                field,
                                "Map",
                                &self.method_table,
                                intern("Map"),
                            ),
                            span,
                        );
                        Type::Error
                    }
                    Type::Set(_) => {
                        if let Some(entry) = self.method_table.get(&(intern("Set"), field)).cloned()
                        {
                            let instantiated =
                                self.dispatch_method_entry(&entry, field, &obj_ty, span);
                            let resolved = self.apply(&instantiated);
                            expr.ty = Some(resolved.clone());
                            return resolved;
                        }
                        self.error(
                            format_unknown_method_message(
                                field,
                                "Set",
                                &self.method_table,
                                intern("Set"),
                            ),
                            span,
                        );
                        Type::Error
                    }
                    Type::Var(v) => {
                        // Check if this type variable has trait constraints
                        if let Some(trait_names) = self.active_constraints.get(v).cloned() {
                            // Collect all traits that provide this method
                            let mut matches: Vec<(Symbol, Type)> = Vec::new();
                            for trait_name in &trait_names {
                                if let Some(trait_info) = self.traits.get(trait_name).cloned()
                                    && let Some((_, method_ty)) =
                                        trait_info.methods.iter().find(|(n, _)| *n == field)
                                {
                                    matches.push((*trait_name, method_ty.clone()));
                                }
                            }
                            if matches.len() > 1 {
                                let trait_list = matches
                                    .iter()
                                    .map(|(name, _)| format!("{name}"))
                                    .collect::<Vec<_>>()
                                    .join(", ");
                                self.error(
                                    format!(
                                        "ambiguous method '{field}': provided by multiple traits ({trait_list})"
                                    ),
                                    span,
                                );
                                Type::Error
                            } else if let Some((_, method_ty)) = matches.first() {
                                self.last_field_access_was_method = true;
                                // Instantiate with fresh TyVars rather than
                                // returning the trait declaration's template
                                // type directly. TraitInfo.methods stores
                                // bare Type values whose TyVars were allocated
                                // once at register_trait_decl time and shared
                                // across all call sites. Without instantiation,
                                // unification at the downstream Call arm binds
                                // those shared template TyVars in self.subst,
                                // so a second constrained call site on a
                                // different concrete type sees the first
                                // site's bindings instead of polymorphic vars.
                                // This surfaces observably when trait methods
                                // have polymorphic return types (beyond Self):
                                // first site binds the return TyVar to one
                                // concrete type, second site inherits it and
                                // produces spurious "type mismatch" errors.
                                let instantiated = self.instantiate_method_type(method_ty);
                                let resolved = self.apply(&instantiated);
                                expr.ty = Some(resolved.clone());
                                return resolved;
                            } else {
                                // Method not found on any constrained trait — error
                                let traits_str = trait_names
                                    .iter()
                                    .map(|s| format!("{s}"))
                                    .collect::<Vec<_>>()
                                    .join(" + ");
                                self.error(
                                    format!(
                                        "no method '{field}' found in trait constraints ({traits_str})"
                                    ),
                                    span,
                                );
                                Type::Error
                            }
                        } else {
                            // B4: Unconstrained type variable — may resolve later to
                            // a record/variant. Record the obligation and re-check at
                            // the end of inference. Return a fresh var for the result
                            // type so downstream inference can continue.
                            let result_ty = self.fresh_var();
                            self.pending_field_accesses.push((
                                obj_ty.clone(),
                                field,
                                result_ty.clone(),
                                span,
                            ));
                            result_ty
                        }
                    }
                    Type::Error => {
                        // Prior error — propagate to prevent cascading false positives
                        Type::Error
                    }
                    _ => {
                        self.error(
                            format!("unknown field or method '{field}' on type {obj_ty}"),
                            span,
                        );
                        Type::Error
                    }
                }
            }

            ExprKind::Binary(lhs, op, rhs) => {
                let op = *op;
                let lhs_span = lhs.span;
                let rhs_span = rhs.span;
                let lt = self.infer_expr(lhs, env);
                let rt = self.infer_expr(rhs, env);

                match op {
                    // ── Implicit Float → ExtFloat widening ─────────────────
                    //
                    // Mixed Float/ExtFloat operands are widened to ExtFloat
                    // *without* going through unification. This is intentional:
                    // Float and ExtFloat are distinct concrete types that do not
                    // unify, but arithmetic between them should silently promote
                    // to the wider type (analogous to f32 → f64 in other
                    // languages).
                    //
                    // For Div the result is always ExtFloat when *either* operand
                    // is a float type, because division may produce fractional
                    // results even from two Floats.
                    //
                    // IMPORTANT: any new numeric binary operators must replicate
                    // this widening logic; otherwise mixed Float/ExtFloat
                    // expressions will produce a unification error.
                    // ─────────────────────────────────────────────────────────
                    BinOp::Add => {
                        let resolved_l = self.apply(&lt);
                        let resolved_r = self.apply(&rt);
                        match (&resolved_l, &resolved_r) {
                            (Type::Float, Type::ExtFloat)
                            | (Type::ExtFloat, Type::Float)
                            | (Type::ExtFloat, Type::ExtFloat) => Type::ExtFloat,
                            _ => {
                                self.unify(&lt, &rt, span);
                                // B2: enforce operand domain — Add accepts
                                // Int/Float/ExtFloat or String (concatenation).
                                let resolved = self.apply(&lt);
                                match &resolved {
                                    Type::Var(_) => {
                                        // Still unresolved — defer to final pass.
                                        self.pending_numeric_checks.push((
                                            resolved.clone(),
                                            "'+'",
                                            span,
                                        ));
                                    }
                                    _ if !is_valid_arith_operand(&resolved, true) => {
                                        self.error(
                                            format!(
                                                "operator '+' requires Int, Float, ExtFloat, or String, got '{resolved}'"
                                            ),
                                            span,
                                        );
                                    }
                                    _ => {}
                                }
                                lt
                            }
                        }
                    }
                    BinOp::Sub | BinOp::Mul | BinOp::Mod => {
                        let op_str = match op {
                            BinOp::Sub => "'-'",
                            BinOp::Mul => "'*'",
                            BinOp::Mod => "'%'",
                            _ => unreachable!(),
                        };
                        let resolved_l = self.apply(&lt);
                        let resolved_r = self.apply(&rt);
                        match (&resolved_l, &resolved_r) {
                            (Type::Float, Type::ExtFloat)
                            | (Type::ExtFloat, Type::Float)
                            | (Type::ExtFloat, Type::ExtFloat) => Type::ExtFloat,
                            (Type::String, _) | (_, Type::String) => {
                                self.error(
                                    "type mismatch: operator requires numeric types, got String"
                                        .to_string(),
                                    span,
                                );
                                lt
                            }
                            _ => {
                                self.unify(&lt, &rt, span);
                                // B2: enforce numeric-only operand domain.
                                let resolved = self.apply(&lt);
                                match &resolved {
                                    Type::Var(_) => {
                                        self.pending_numeric_checks.push((
                                            resolved.clone(),
                                            op_str,
                                            span,
                                        ));
                                    }
                                    _ if !is_valid_arith_operand(&resolved, false) => {
                                        self.error(
                                            format!(
                                                "operator {op_str} requires Int, Float, or ExtFloat, got '{resolved}'"
                                            ),
                                            span,
                                        );
                                    }
                                    _ => {}
                                }
                                lt
                            }
                        }
                    }
                    BinOp::Div => {
                        let resolved_l = self.apply(&lt);
                        let resolved_r = self.apply(&rt);
                        match (&resolved_l, &resolved_r) {
                            (Type::Float, Type::Float)
                            | (Type::Float, Type::ExtFloat)
                            | (Type::ExtFloat, Type::Float)
                            | (Type::ExtFloat, Type::ExtFloat) => Type::ExtFloat,
                            _ => {
                                self.unify(&lt, &rt, span);
                                // B2: enforce numeric-only operand domain.
                                let resolved = self.apply(&lt);
                                match &resolved {
                                    Type::Var(_) => {
                                        self.pending_numeric_checks.push((
                                            resolved.clone(),
                                            "'/'",
                                            span,
                                        ));
                                    }
                                    _ if !is_valid_arith_operand(&resolved, false) => {
                                        self.error(
                                            format!(
                                                "operator '/' requires Int, Float, or ExtFloat, got '{resolved}'"
                                            ),
                                            span,
                                        );
                                    }
                                    _ => {}
                                }
                                lt
                            }
                        }
                    }
                    BinOp::Eq | BinOp::Neq | BinOp::Lt | BinOp::Gt | BinOp::Leq | BinOp::Geq => {
                        let is_equality = matches!(op, BinOp::Eq | BinOp::Neq);
                        let op_str = match op {
                            BinOp::Eq => "'=='",
                            BinOp::Neq => "'!='",
                            BinOp::Lt => "'<'",
                            BinOp::Gt => "'>'",
                            BinOp::Leq => "'<='",
                            BinOp::Geq => "'>='",
                            _ => unreachable!(),
                        };
                        let resolved_l = self.apply(&lt);
                        let resolved_r = self.apply(&rt);
                        match (&resolved_l, &resolved_r) {
                            (Type::Float, Type::ExtFloat) | (Type::ExtFloat, Type::Float) => {
                                // Accept mixed Float/ExtFloat without unification
                            }
                            _ => {
                                self.unify(&lt, &rt, span);
                            }
                        }
                        // B3: enforce comparison operand domain. The VM's
                        // compare() (src/vm/arithmetic.rs) only supports
                        // Int/Float/ExtFloat/String/List/Range/Record/Variant
                        // for ordering. Equality additionally supports
                        // Tuple/Map/Set/Bool/Unit via Value's PartialEq.
                        let resolved = self.apply(&lt);
                        match &resolved {
                            Type::Var(_) => {
                                // Defer — may resolve later.
                                self.pending_numeric_checks.push((
                                    resolved.clone(),
                                    if is_equality {
                                        "'=='/'!='"
                                    } else {
                                        "ordering comparison"
                                    },
                                    span,
                                ));
                            }
                            _ if !is_valid_compare_operand(&resolved, is_equality) => {
                                let domain = if is_equality {
                                    "a comparable type"
                                } else {
                                    "Int, Float, ExtFloat, String, List, Range, Record, or Variant"
                                };
                                self.error(
                                    format!(
                                        "operator {op_str} requires {domain}, got '{resolved}'"
                                    ),
                                    span,
                                );
                            }
                            _ => {}
                        }
                        Type::Bool
                    }
                    BinOp::And | BinOp::Or => {
                        self.unify(&lt, &Type::Bool, lhs_span);
                        self.unify(&rt, &Type::Bool, rhs_span);
                        Type::Bool
                    }
                }
            }

            ExprKind::Unary(op, operand) => {
                let op = *op;
                let operand_span = operand.span;
                let t = self.infer_expr(operand, env);
                match op {
                    UnaryOp::Neg => {
                        let resolved = self.apply(&t);
                        match &resolved {
                            Type::Int | Type::Float | Type::ExtFloat => {}
                            Type::Error | Type::Never => {}
                            Type::Var(_) => {
                                // B5: unresolved — defer until after all bodies are
                                // inferred. If still a Var at that point, it's an
                                // ambiguity error.
                                self.pending_numeric_checks.push((
                                    resolved.clone(),
                                    "unary '-'",
                                    operand_span,
                                ));
                            }
                            _ => {
                                self.error(
                                    format!(
                                        "unary '-' requires Int, Float, or ExtFloat, got '{}'",
                                        resolved
                                    ),
                                    operand_span,
                                );
                            }
                        }
                        t
                    }
                    UnaryOp::Not => {
                        self.unify(&t, &Type::Bool, operand_span);
                        Type::Bool
                    }
                }
            }

            ExprKind::Pipe(lhs, rhs) => {
                let lhs_span = lhs.span;
                let arg_type = self.infer_expr(lhs, env);

                // Pipe semantics: a |> f(b) means f(a, b)
                // If the RHS is a Call, we prepend the pipe LHS as the first argument.
                // Check if rhs is a Call before mutable borrow
                let rhs_is_call = matches!(&rhs.kind, ExprKind::Call(..));

                if rhs_is_call {
                    // Destructure rhs.kind mutably to get at callee and call_args
                    if let ExprKind::Call(callee, call_args) = &mut rhs.kind {
                        // Capture callee name for where clause check
                        let callee_fn_name = if let ExprKind::Ident(n) = &callee.kind {
                            Some(*n)
                        } else {
                            None
                        };
                        // Capture arg spans before mutable inference
                        let arg_spans: Vec<Span> = call_args.iter().map(|a| a.span).collect();

                        // If callee is a named function, use instantiate_with_constraints
                        let (callee_ty, where_constraints) = if let Some(name) = callee_fn_name {
                            if let Some(scheme) = env.lookup(name).cloned() {
                                let (ty, constraints) = self.instantiate_with_constraints(&scheme);
                                (self.apply(&ty), constraints)
                            } else {
                                let ty = self.infer_expr(callee, env);
                                (self.apply(&ty), vec![])
                            }
                        } else {
                            let ty = self.infer_expr(callee, env);
                            (self.apply(&ty), vec![])
                        };

                        // Infer types for the explicit call args
                        let explicit_arg_types: Vec<Type> = call_args
                            .iter_mut()
                            .map(|a| self.infer_expr(a, env))
                            .collect();

                        // All args = [pipe_lhs, ...explicit_args]
                        let mut all_arg_types = vec![arg_type];
                        all_arg_types.extend(explicit_arg_types);

                        let result_ty = match &callee_ty {
                            Type::Fun(params, ret) => {
                                // Arity check — piped-through arg counts as
                                // the first positional arg. Mirrors the
                                // non-pipe Call branch below.
                                if params.len() != all_arg_types.len() {
                                    let expected = params.len();
                                    self.error(
                                        format!(
                                            "function expects {} {}, got {}",
                                            expected,
                                            plural(expected, "argument", "arguments"),
                                            all_arg_types.len()
                                        ),
                                        span,
                                    );
                                }
                                let min_len = params.len().min(all_arg_types.len());
                                for i in 0..min_len {
                                    let s = if i == 0 { lhs_span } else { arg_spans[i - 1] };
                                    self.unify(&all_arg_types[i], &params[i], s);
                                }
                                *ret.clone()
                            }
                            Type::Var(_) => {
                                let ret = self.fresh_var();
                                let fn_ty = Type::Fun(all_arg_types.clone(), Box::new(ret.clone()));
                                self.unify(&callee_ty, &fn_ty, span);
                                ret
                            }
                            _ => self.fresh_var(),
                        };

                        // Check where clause constraints using instantiated TyVars
                        for (tyvar, trait_name) in &where_constraints {
                            let resolved = self.apply(&Type::Var(*tyvar));
                            if self.type_name_for_impl(&resolved).is_some() {
                                // Recursively walk the matched impl's where
                                // clauses against the resolved type's args.
                                self.verify_trait_obligation(*trait_name, &resolved, span);
                            } else if matches!(&resolved, Type::Var(_))
                                && !self.covered_by_active_constraint(&resolved, *trait_name)
                            {
                                // B4: defer — the tyvar may still resolve
                                // to a concrete type in a later body
                                // (e.g. a lambda's param pinned after
                                // the enclosing function body unifies
                                // it at the top-level call site). We
                                // re-check in `finalize_deferred_checks`.
                                if let Type::Var(v) = resolved {
                                    self.pending_where_constraints.push(PendingWhereConstraint {
                                        tyvar: v,
                                        trait_name: *trait_name,
                                        callee_fn_name,
                                        span,
                                        active_snapshot: self.active_constraints.clone(),
                                        param_tyvars: self.current_fn_param_tyvars.clone(),
                                    });
                                }
                            }
                        }

                        result_ty
                    } else {
                        unreachable!()
                    }
                } else {
                    // RHS is a plain function/lambda, not a call
                    let fn_type = self.infer_expr(rhs, env);
                    let fn_type = self.apply(&fn_type);

                    match &fn_type {
                        Type::Fun(params, ret) => {
                            // B6: A plain function reference on the RHS of `|>`
                            // must have arity 1. Piping into a multi-arg function
                            // without an explicit call forgets the remaining args.
                            if params.len() != 1 {
                                let n = params.len();
                                self.error(
                                    format!(
                                        "cannot pipe into function taking {} {}; wrap in a call or use partial application",
                                        n,
                                        plural(n, "argument", "arguments")
                                    ),
                                    span,
                                );
                            }
                            if !params.is_empty() {
                                self.unify(&arg_type, &params[0], span);
                            }
                            *ret.clone()
                        }
                        Type::Var(_) => {
                            let ret = self.fresh_var();
                            let fn_ty = Type::Fun(vec![arg_type], Box::new(ret.clone()));
                            self.unify(&fn_type, &fn_ty, span);
                            ret
                        }
                        _ => {
                            self.error(
                                "pipe operator requires a function on the right-hand side"
                                    .to_string(),
                                rhs.span,
                            );
                            self.fresh_var()
                        }
                    }
                }
            }

            ExprKind::Range(start, end) => {
                let start_span = start.span;
                let end_span = end.span;
                let st = self.infer_expr(start, env);
                let et = self.infer_expr(end, env);
                self.unify(&st, &Type::Int, start_span);
                self.unify(&et, &Type::Int, end_span);
                Type::List(Box::new(Type::Int))
            }

            ExprKind::QuestionMark(inner) => {
                let inner_ty = self.infer_expr(inner, env);
                let inner_ty = self.apply(&inner_ty);

                // ? operator on Result(a,e) returns a, propagates Err(e)
                // ? operator on Option(a) returns a, propagates None
                match &inner_ty {
                    Type::Generic(name, args) if *name == intern("Result") && args.len() == 2 => {
                        if let Some(expected_ret) = self.current_return_type.clone() {
                            let err_ty = args[1].clone();
                            let fresh_ok = self.fresh_var();
                            let expected_result =
                                Type::Generic(intern("Result"), vec![fresh_ok, err_ty]);
                            self.unify(&expected_ret, &expected_result, span);
                        } else {
                            self.error(
                                "? operator can only be used inside a function that returns Result or Option".to_string(),
                                span,
                            );
                        }
                        args[0].clone()
                    }
                    Type::Generic(name, args) if *name == intern("Option") && args.len() == 1 => {
                        if let Some(expected_ret) = self.current_return_type.clone() {
                            let fresh_inner = self.fresh_var();
                            let expected_option =
                                Type::Generic(intern("Option"), vec![fresh_inner]);
                            self.unify(&expected_ret, &expected_option, span);
                        } else {
                            self.error(
                                "? operator can only be used inside a function that returns Result or Option".to_string(),
                                span,
                            );
                        }
                        args[0].clone()
                    }
                    Type::Var(_) => {
                        // Unresolved type variable — stay lenient
                        self.fresh_var()
                    }
                    _ => {
                        self.error(
                            format!(
                                "'?' operator requires Result or Option type, got '{inner_ty}'"
                            ),
                            span,
                        );
                        self.fresh_var()
                    }
                }
            }

            ExprKind::Ascription(inner, type_expr) => {
                let inner_ty = self.infer_expr(inner, env);
                let declared = self.resolve_type_expr(type_expr, &mut HashMap::new());
                self.unify(&inner_ty, &declared, span);
                declared
            }

            ExprKind::Call(callee, args) => {
                // Capture callee name and arg spans before mutable inference
                let callee_fn_name = if let ExprKind::Ident(n) = &callee.kind {
                    Some(*n)
                } else {
                    None
                };
                // Detect module-qualified calls (mod.fn(args)) for arity
                // tolerance: some builtins register an optional trailing
                // param (e.g. test.assert_eq(a, a, String)), so module
                // calls allow args == params OR args + 1 == params.
                let is_module_call = match &callee.kind {
                    ExprKind::FieldAccess(obj, field) => {
                        if let ExprKind::Ident(mod_name) = &obj.kind {
                            let qualified = intern(&format!("{}.{field}", resolve(*mod_name)));
                            env.lookup(qualified).is_some()
                        } else {
                            false
                        }
                    }
                    _ => false,
                };
                let arg_spans: Vec<Span> = args.iter().map(|a| a.span).collect();

                // Option B (parser-recovery cascade fix): if the callee
                // resolves to a parser-recovery stub, we cannot trust its
                // signature — the user's real error is the parse failure
                // that produced the stub, not whatever arity/arg-type
                // mismatch we'd find here. Skip all checks and return a
                // fresh TyVar so downstream expressions continue to
                // typecheck without bogus cascade errors.
                let is_stub_callee = match callee_fn_name {
                    Some(name) => self.recovery_stub_names.contains(&name),
                    None => false,
                };
                if is_stub_callee {
                    // Walk arg expressions for inference side-effects (so
                    // genuine errors inside the args still fire), but
                    // discard any arity/arg-type checks against the stub.
                    for arg in args.iter_mut() {
                        let _ = self.infer_expr(arg, env);
                    }
                    let fresh = self.fresh_var();
                    expr.ty = Some(self.apply(&fresh));
                    return fresh;
                }

                // If callee is a named function, use instantiate_with_constraints
                // to get where clause constraints with remapped type variables.
                // Reset the method-dispatch flag so stale values from prior
                // FieldAccess evaluations don't leak into this Call.
                self.last_field_access_was_method = false;
                let (callee_ty, where_constraints) = if let Some(name) = callee_fn_name {
                    if let Some(scheme) = env.lookup(name).cloned() {
                        let (ty, constraints) = self.instantiate_with_constraints(&scheme);
                        (self.apply(&ty), constraints)
                    } else {
                        let ty = self.infer_expr(callee, env);
                        (self.apply(&ty), vec![])
                    }
                } else {
                    let ty = self.infer_expr(callee, env);
                    (self.apply(&ty), vec![])
                };

                // Read the method-dispatch flag BEFORE inferring args
                // (which may trigger nested FieldAccess and overwrite it).
                let is_method_call = self.last_field_access_was_method;

                let arg_types: Vec<Type> =
                    args.iter_mut().map(|a| self.infer_expr(a, env)).collect();

                let result_ty = match &callee_ty {
                    Type::Fun(params, ret) => {
                        // Unify argument types with parameter types
                        let min_len = params.len().min(arg_types.len());
                        for i in 0..min_len {
                            self.unify(&arg_types[i], &params[i], arg_spans[i]);
                        }
                        // Check arity:
                        // - method call (dispatch_method_entry set the flag):
                        //   args + 1 == params (implicit self)
                        // - module call: args == params, or args + 1 == params
                        //   (some builtins have an optional trailing param)
                        // - field/normal call: args == params
                        let arity_ok = if is_method_call {
                            arg_types.len() + 1 == params.len()
                        } else if is_module_call {
                            arg_types.len() == params.len() || arg_types.len() + 1 == params.len()
                        } else {
                            arg_types.len() == params.len()
                        };
                        if !arity_ok {
                            let expected = params.len();
                            self.error(
                                format!(
                                    "function expects {} {}, got {}",
                                    expected,
                                    plural(expected, "argument", "arguments"),
                                    arg_types.len()
                                ),
                                span,
                            );
                        }
                        *ret.clone()
                    }
                    Type::Var(_) => {
                        // The callee is an unresolved type variable - create a function type
                        let ret = self.fresh_var();
                        let fn_ty = Type::Fun(arg_types.clone(), Box::new(ret.clone()));
                        self.unify(&callee_ty, &fn_ty, span);
                        ret
                    }
                    Type::Error => Type::Error,
                    Type::Never => Type::Never,
                    _ => {
                        self.error(format!("type '{}' is not callable", callee_ty), span);
                        self.fresh_var()
                    }
                };

                // Check where clause constraints using instantiated TyVars
                for (tyvar, trait_name) in &where_constraints {
                    let resolved = self.apply(&Type::Var(*tyvar));
                    if self.type_name_for_impl(&resolved).is_some() {
                        // Recursively walk the matched impl's where clauses
                        // against the resolved type's arguments.
                        self.verify_trait_obligation(*trait_name, &resolved, span);
                    } else if matches!(&resolved, Type::Var(_))
                        && !self.covered_by_active_constraint(&resolved, *trait_name)
                    {
                        // B4: defer — the tyvar may still resolve to a
                        // concrete type in a later body. See the pipe
                        // arm for details; both sites push to the same
                        // pending list re-examined by finalize.
                        if let Type::Var(v) = resolved {
                            self.pending_where_constraints.push(PendingWhereConstraint {
                                tyvar: v,
                                trait_name: *trait_name,
                                callee_fn_name,
                                span,
                                active_snapshot: self.active_constraints.clone(),
                                param_tyvars: self.current_fn_param_tyvars.clone(),
                            });
                        }
                    }
                }

                result_ty
            }

            ExprKind::Lambda { params, body } => {
                let mut local_env = env.child();
                let param_types: Vec<Type> = params
                    .iter()
                    .map(|p| {
                        let ty = if let Some(te) = &p.ty {
                            self.resolve_type_expr(te, &mut HashMap::new())
                        } else {
                            self.fresh_var()
                        };
                        self.bind_pattern(&p.pattern, &ty, &mut local_env, span);
                        ty
                    })
                    .collect();

                let body_type = self.infer_expr(body, &mut local_env);
                Type::Fun(param_types, Box::new(body_type))
            }

            ExprKind::RecordCreate { name, fields } => {
                let name = *name;
                if let Some(rec_info) = self.records.get(&name).cloned() {
                    // For parameterized record types, create fresh type variables
                    // for each type parameter and substitute them into field types.
                    // This prevents different instantiations from sharing the same
                    // template variables (e.g., Box { value: 42 } and Box { value: "hi" }).
                    let instantiated_fields: Vec<(Symbol, Type)> = if let Some(param_var_ids) =
                        self.record_param_var_ids.get(&name).cloned()
                    {
                        let mapping: HashMap<TyVar, Type> = param_var_ids
                            .iter()
                            .map(|&v| (v, self.fresh_var()))
                            .collect();
                        rec_info
                            .fields
                            .iter()
                            .map(|(n, t)| (*n, substitute_vars(t, &mapping)))
                            .collect()
                    } else {
                        rec_info.fields.clone()
                    };

                    let field_types: Vec<(Symbol, Type)> = fields
                        .iter_mut()
                        .map(|(n, e)| {
                            let ty = self.infer_expr(e, env);
                            (*n, ty)
                        })
                        .collect();

                    // Unify with declared field types (using instantiated copies)
                    for (field_name, inferred_ty) in &field_types {
                        if let Some((_, declared_ty)) =
                            instantiated_fields.iter().find(|(n, _)| n == field_name)
                        {
                            self.unify(inferred_ty, declared_ty, span);
                        }
                    }

                    // Check for missing fields
                    let provided: std::collections::HashSet<Symbol> =
                        field_types.iter().map(|(n, _)| *n).collect();
                    let missing: Vec<Symbol> = rec_info
                        .fields
                        .iter()
                        .filter(|(n, _)| !provided.contains(n))
                        .map(|(n, _)| *n)
                        .collect();
                    if !missing.is_empty() {
                        let missing_str: Vec<String> =
                            missing.iter().map(|s| format!("{s}")).collect();
                        self.error(
                            format!(
                                "missing field{} in {}: {}",
                                if missing.len() > 1 { "s" } else { "" },
                                name,
                                missing_str.join(", "),
                            ),
                            span,
                        );
                    }

                    // Check for extra fields not in the record type
                    let declared: std::collections::HashSet<Symbol> =
                        rec_info.fields.iter().map(|(n, _)| *n).collect();
                    for (field_name, _) in &field_types {
                        if !declared.contains(field_name) {
                            // GAP (round 26 L5): append a did-you-mean
                            // hint for record-literal typos — e.g.
                            // `User { nam: ... }` → `did you mean \`name\`?`.
                            let base = format!("unknown field '{}' in {}", field_name, name);
                            self.error(
                                format_record_field_suggestion(
                                    base,
                                    *field_name,
                                    &rec_info.fields,
                                ),
                                span,
                            );
                        }
                    }

                    Type::Record(name, instantiated_fields)
                } else {
                    // G2: Unknown record type — this used to silently synthesize
                    // an anonymous record. Emit an error so the user notices a
                    // typo or missing type declaration. We still walk the field
                    // expressions so nested errors are reported, but return
                    // Type::Error to prevent downstream cascades.
                    for (_, e) in fields.iter_mut() {
                        let _ = self.infer_expr(e, env);
                    }
                    self.error(format!("undefined type '{name}'"), span);
                    Type::Error
                }
            }

            ExprKind::RecordUpdate { expr: base, fields } => {
                let base_span = base.span;
                let base_ty = self.infer_expr(base, env);
                let resolved = self.apply(&base_ty);
                // Three cases:
                //  1. Concrete `Type::Record(name, fields)` — validate directly.
                //  2. `Type::Generic(name, args)` resolving to a declared
                //     record (happens when the base is a param annotated
                //     with a user-defined record type). BROKEN-1.
                //  3. Anything else — compile-time reject. BROKEN-2.
                let mut handled = false;
                if let Type::Record(rec_name, rec_fields) = &resolved {
                    let declared: std::collections::HashMap<Symbol, Type> =
                        rec_fields.iter().map(|(n, t)| (*n, t.clone())).collect();
                    for (field_name, field_expr) in &mut *fields {
                        let ft = self.infer_expr(field_expr, env);
                        if let Some(declared_ty) = declared.get(field_name) {
                            self.unify(&ft, declared_ty, span);
                        } else {
                            // GAP (round 26 L5): did-you-mean on record-update.
                            let base =
                                format!("unknown field '{field_name}' in {rec_name}");
                            self.error(
                                format_record_field_suggestion(base, *field_name, rec_fields),
                                span,
                            );
                        }
                    }
                    handled = true;
                } else if let Type::Generic(type_name, type_args) = &resolved
                    && let Some(rec_info) = self.records.get(type_name).cloned()
                {
                    let instantiated_fields: Vec<(Symbol, Type)> = if let Some(param_var_ids) =
                        self.record_param_var_ids.get(type_name).cloned()
                    {
                        let mapping: HashMap<TyVar, Type> =
                            if type_args.len() == param_var_ids.len() {
                                param_var_ids
                                    .iter()
                                    .zip(type_args.iter())
                                    .map(|(&v, t)| (v, t.clone()))
                                    .collect()
                            } else {
                                param_var_ids
                                    .iter()
                                    .map(|&v| (v, self.fresh_var()))
                                    .collect()
                            };
                        rec_info
                            .fields
                            .iter()
                            .map(|(n, t)| (*n, substitute_vars(t, &mapping)))
                            .collect()
                    } else {
                        rec_info.fields.clone()
                    };
                    let declared: std::collections::HashMap<Symbol, Type> = instantiated_fields
                        .iter()
                        .map(|(n, t)| (*n, t.clone()))
                        .collect();
                    for (field_name, field_expr) in &mut *fields {
                        let ft = self.infer_expr(field_expr, env);
                        if let Some(declared_ty) = declared.get(field_name) {
                            self.unify(&ft, declared_ty, span);
                        } else {
                            // GAP (round 26 L5): did-you-mean on the
                            // generic-record update path.
                            let base =
                                format!("unknown field '{field_name}' in {type_name}");
                            self.error(
                                format_record_field_suggestion(
                                    base,
                                    *field_name,
                                    &instantiated_fields,
                                ),
                                span,
                            );
                        }
                    }
                    handled = true;
                }
                if !handled {
                    // BROKEN (round 23 #2): when the receiver is still a
                    // bare type variable (e.g. `fn f(r) { r.{ aeg: ... } }`)
                    // we used to silently infer each field expr and drop
                    // the field name on the floor — the typo `aeg` would
                    // crash the VM at runtime or, worse, silently corrupt
                    // the record.
                    //
                    // Two-pronged fix:
                    //  a) Push each (base, field_name) pair to the B4
                    //     `pending_field_accesses` pool so that when the
                    //     base DOES narrow to a concrete record (e.g. via
                    //     scheme narrowing or re-check), the standard
                    //     finalize path validates the field.
                    //  b) Eagerly reject field names that aren't declared
                    //     on ANY record type in the program. For truly
                    //     polymorphic bases this is the only compile-time
                    //     signal we get — if the field name is a typo
                    //     that doesn't match any declared record field,
                    //     no call-site narrowing can rescue it. This is
                    //     narrow enough to avoid false positives on
                    //     valid polymorphic updates like `r.{ age: n }`
                    //     (age IS declared on at least one record).
                    let is_var_base = matches!(resolved, Type::Var(_));
                    // Collect the set of field names across all declared
                    // records once so the per-field check is O(1). A
                    // HashSet keeps this independent of record count.
                    let known_record_fields: std::collections::HashSet<Symbol> = if is_var_base {
                        self.records
                            .values()
                            .flat_map(|r| r.fields.iter().map(|(n, _)| *n))
                            .collect()
                    } else {
                        std::collections::HashSet::new()
                    };
                    for (field_name, field_expr) in &mut *fields {
                        let ft = self.infer_expr(field_expr, env);
                        if is_var_base {
                            self.pending_field_accesses.push((
                                base_ty.clone(),
                                *field_name,
                                ft,
                                span,
                            ));
                            if !known_record_fields.contains(field_name) {
                                // Typo guaranteed: no record in the
                                // program has a field with this name,
                                // so regardless of how `r` narrows at
                                // call sites, this update would fail.
                                self.error(
                                    format!(
                                        "unknown field '{field_name}' — not declared on any record type in scope"
                                    ),
                                    span,
                                );
                            }
                        }
                    }
                    if !matches!(resolved, Type::Error | Type::Var(_) | Type::Never) {
                        self.error(
                            format!(
                                "record update requires a record base, but '{resolved}' is not a record type"
                            ),
                            base_span,
                        );
                    }
                }
                base_ty
            }

            ExprKind::Match {
                expr: scrutinee,
                arms,
            } => {
                match scrutinee {
                    Some(scrutinee) => {
                        let scrutinee_span = scrutinee.span;
                        let scrutinee_ty = self.infer_expr(scrutinee, env);
                        let result_ty = self.fresh_var();

                        for arm in arms.iter_mut() {
                            let mut arm_env = env.child();
                            self.check_pattern(
                                &arm.pattern,
                                &scrutinee_ty,
                                &mut arm_env,
                                scrutinee_span,
                            );

                            if let Some(ref mut guard) = arm.guard {
                                let guard_span = guard.span;
                                let guard_ty = self.infer_expr(guard, &mut arm_env);
                                self.unify(&guard_ty, &Type::Bool, guard_span);
                            }

                            let body_span = arm.body.span;
                            let arm_ty = self.infer_expr(&mut arm.body, &mut arm_env);
                            self.unify(&result_ty, &arm_ty, body_span);
                        }

                        // Check exhaustiveness after pattern checking, so the
                        // scrutinee type is fully resolved through unification.
                        let resolved_scrutinee_ty = self.apply(&scrutinee_ty);
                        self.check_exhaustiveness(arms, &resolved_scrutinee_ty, scrutinee_span);

                        result_ty
                    }
                    None => {
                        // Guardless match: each arm's guard is a boolean condition
                        let result_ty = self.fresh_var();

                        for arm in arms.iter_mut() {
                            let mut arm_env = env.child();

                            if let Some(ref mut guard) = arm.guard {
                                let guard_span = guard.span;
                                let guard_ty = self.infer_expr(guard, &mut arm_env);
                                self.unify(&guard_ty, &Type::Bool, guard_span);
                            }

                            let body_span = arm.body.span;
                            let arm_ty = self.infer_expr(&mut arm.body, &mut arm_env);
                            self.unify(&result_ty, &arm_ty, body_span);
                        }

                        // No exhaustiveness checking for guardless match
                        result_ty
                    }
                }
            }

            ExprKind::Return(maybe_expr) => {
                let ret_val_ty = if let Some(e) = maybe_expr {
                    self.infer_expr(e, env)
                } else {
                    Type::Unit
                };
                if let Some(expected_ret) = self.current_return_type.clone() {
                    self.unify(&ret_val_ty, &expected_ret, span);
                }
                Type::Never
            }

            ExprKind::Block(stmts) => {
                let mut last_ty = Type::Unit;
                let mut block_env = env.child();

                for stmt in stmts {
                    last_ty = self.infer_stmt(stmt, &mut block_env);
                }

                last_ty
            }

            ExprKind::Loop { bindings, body } => {
                let mut loop_env = env.child();
                let mut binding_types = Vec::new();
                for (name, value) in bindings.iter_mut() {
                    let ty = self.infer_expr(value, env);
                    binding_types.push(ty.clone());
                    loop_env.define(*name, Scheme::mono(ty));
                }
                let prev_loop = self.loop_binding_types.take();
                self.loop_binding_types = Some(binding_types);
                let result = self.infer_expr(body, &mut loop_env);
                self.loop_binding_types = prev_loop;
                result
            }

            ExprKind::Recur(args) => {
                let recur_count = args.len();
                let arg_types: Vec<Type> = args
                    .iter_mut()
                    .map(|arg| self.infer_expr(arg, env))
                    .collect();
                if let Some(binding_types) = self.loop_binding_types.clone() {
                    if recur_count != binding_types.len() {
                        let bindings_n = binding_types.len();
                        self.error(
                            format!(
                                "loop has {} {}, but recur supplies {} {}",
                                bindings_n,
                                plural(bindings_n, "binding", "bindings"),
                                recur_count,
                                plural(recur_count, "argument", "arguments")
                            ),
                            span,
                        );
                    }
                    // Unify each recur arg with its corresponding loop binding type
                    for (i, arg_ty) in arg_types.iter().enumerate() {
                        if let Some(binding_ty) = binding_types.get(i) {
                            self.unify(arg_ty, binding_ty, span);
                        }
                    }
                } else {
                    self.error("recur outside of loop".to_string(), span);
                }
                self.fresh_var()
            }

            ExprKind::FloatElse(expr, fallback) => {
                let expr_ty = self.infer_expr(expr, env);
                let fallback_ty = self.infer_expr(fallback, env);
                self.unify(&expr_ty, &Type::ExtFloat, expr.span);
                self.unify(&fallback_ty, &Type::Float, fallback.span);
                Type::Float
            }
        };
        let resolved = self.apply(&ty);
        expr.ty = Some(resolved.clone());
        resolved
    }

    // ── Statement type inference ────────────────────────────────────

    fn infer_stmt(&mut self, stmt: &mut Stmt, env: &mut TypeEnv) -> Type {
        match stmt {
            Stmt::Let { pattern, ty, value } => {
                let value_span = value.span;
                let is_value = is_syntactic_value(&value.kind);
                let val_ty = self.infer_expr(value, env);

                if let Some(te) = &ty {
                    let declared = self.resolve_type_expr(te, &mut HashMap::new());
                    self.unify(&val_ty, &declared, value_span);
                }

                // Generalize for let-polymorphism, but apply the value
                // restriction: only generalize syntactic values (literals,
                // lambdas, identifiers). Function calls may return types
                // with mutable state (e.g. channels) that must remain
                // monomorphic so that the element type is shared across
                // all uses.
                let scheme = if is_value {
                    self.generalize(env, &val_ty)
                } else {
                    Scheme::mono(self.apply(&val_ty))
                };

                // Bind names in the pattern
                // For let-polymorphism we need to bind with the generalized scheme
                match &pattern.kind {
                    PatternKind::Ident(name) => {
                        env.define(*name, scheme);
                    }
                    _ => {
                        // B1: reject refutable Constructor patterns in
                        // `let` before binding, so we produce a clean
                        // typecheck error instead of silent runtime
                        // payload corruption from a tag mismatch.
                        self.reject_refutable_constructor_in_let(pattern, value_span);
                        self.bind_pattern(pattern, &val_ty, env, value_span);
                    }
                }

                Type::Unit
            }

            Stmt::When {
                pattern,
                expr,
                else_body,
            } => {
                let expr_span = expr.span;
                let expr_ty = self.infer_expr(expr, env);

                // Type check the else body — it must diverge (return / panic)
                let else_ty = self.infer_expr(else_body, env);
                let resolved_else = self.apply(&else_ty);
                if !matches!(resolved_else, Type::Never | Type::Error) {
                    self.error(
                        "'when let' else body must diverge — use 'return' or 'panic'".to_string(),
                        else_body.span,
                    );
                }

                // Bind the pattern in the current scope (type narrowing).
                // bind_pattern handles all pattern kinds including constructors
                // (enum lookup, param substitution, recursive sub-pattern binding).
                self.bind_pattern(pattern, &expr_ty, env, expr_span);

                Type::Unit
            }

            Stmt::WhenBool {
                condition,
                else_body,
            } => {
                let cond_ty = self.infer_expr(condition, env);
                self.unify(&cond_ty, &Type::Bool, condition.span);

                // Type check the else body — it must diverge (return / panic)
                let else_ty = self.infer_expr(else_body, env);
                let resolved_else = self.apply(&else_ty);
                if !matches!(resolved_else, Type::Never | Type::Error) {
                    self.error(
                        "'when' else body must diverge — use 'return' or 'panic'".to_string(),
                        else_body.span,
                    );
                }

                Type::Unit
            }

            Stmt::Expr(expr) => self.infer_expr(expr, env),
        }
    }

    // ── Pattern checking (type check, not just bind) ────────────────

    fn check_pattern(&mut self, pattern: &Pattern, expected: &Type, env: &mut TypeEnv, span: Span) {
        match &pattern.kind {
            PatternKind::Wildcard => {}
            PatternKind::Ident(name) => {
                env.define(*name, Scheme::mono(expected.clone()));
            }
            PatternKind::Int(_) => {
                self.unify(expected, &Type::Int, span);
            }
            PatternKind::Float(_) => {
                self.unify(expected, &Type::Float, span);
            }
            PatternKind::Bool(_) => {
                self.unify(expected, &Type::Bool, span);
            }
            PatternKind::StringLit(..) => {
                self.unify(expected, &Type::String, span);
            }
            PatternKind::Tuple(pats) => {
                // BROKEN (round 23 #1): mirror bind_pattern — `()` is the
                // unit pattern, not a zero-arity tuple. See the comment on
                // PatternKind::Tuple in bind_pattern for background.
                if pats.is_empty() {
                    self.unify(expected, &Type::Unit, span);
                } else {
                    let elem_types: Vec<Type> = pats.iter().map(|_| self.fresh_var()).collect();
                    let tuple_ty = Type::Tuple(elem_types.clone());
                    self.unify(expected, &tuple_ty, span);

                    for (p, t) in pats.iter().zip(elem_types.iter()) {
                        self.check_pattern(p, t, env, span);
                    }
                }
            }
            PatternKind::Constructor(name, sub_pats) => {
                // Look up the constructor type
                if let Some(scheme) = env.lookup(*name).cloned() {
                    let ctor_ty = self.instantiate(&scheme);
                    let ctor_ty = self.apply(&ctor_ty);

                    match &ctor_ty {
                        Type::Fun(params, ret) => {
                            self.unify(expected, ret, span);
                            if sub_pats.len() != params.len() {
                                let expected = params.len();
                                // Fix A: the arity error is about the
                                // pattern itself — point at the
                                // constructor pattern's own span rather
                                // than the enclosing match scrutinee.
                                // LATENT (round 26 L2): include the
                                // constructor name to match bind_pattern's
                                // wording ("constructor 'Some' expects ..."),
                                // otherwise the user has no idea which
                                // alternative arm is wrong when multiple
                                // constructors appear in a match.
                                self.error(
                                    format!(
                                        "constructor '{}' expects {} {}, but pattern has {}",
                                        name,
                                        expected,
                                        plural(expected, "field", "fields"),
                                        sub_pats.len()
                                    ),
                                    pattern.span,
                                );
                            }
                            for (i, sp) in sub_pats.iter().enumerate() {
                                if i < params.len() {
                                    self.check_pattern(sp, &params[i], env, span);
                                }
                            }
                        }
                        _ => {
                            // Zero-arg constructor
                            if sub_pats.is_empty() {
                                self.unify(expected, &ctor_ty, span);
                            } else if self.records.contains_key(name) {
                                // GAP (round 23 #4): the user wrote
                                // `Circle(r)` where `Circle` is a record
                                // type, not an enum constructor. The old
                                // error said "expects 0 fields, but
                                // pattern has N", which is misleading —
                                // record types DO have fields, they just
                                // use `Circle { radius: r }` pattern
                                // syntax. Surface the real issue and
                                // point at the correct shape.
                                self.error(
                                    format!(
                                        "'{name}' is a record type; use record-pattern syntax `{name} {{ ... }}` instead of constructor-pattern syntax"
                                    ),
                                    pattern.span,
                                );
                                for sp in sub_pats {
                                    let tv = self.fresh_var();
                                    self.check_pattern(sp, &tv, env, span);
                                }
                            } else {
                                self.error(
                                    format!(
                                        "constructor '{}' expects 0 fields, but pattern has {}",
                                        name,
                                        sub_pats.len()
                                    ),
                                    pattern.span,
                                );
                            }
                        }
                    }
                } else {
                    // Unknown constructor — report error and bind sub-patterns with fresh vars.
                    // LATENT (round 26 L3): point the caret at the
                    // constructor pattern, not the enclosing match
                    // scrutinee — round-17 F4 threaded pattern.span
                    // through arity sites but missed this fallback.
                    self.error(
                        format!("undefined constructor '{name}' in pattern"),
                        pattern.span,
                    );
                    for sp in sub_pats {
                        let tv = self.fresh_var();
                        self.check_pattern(sp, &tv, env, span);
                    }
                }
            }
            PatternKind::List(pats, rest) => {
                let elem_ty = self.fresh_var();
                let list_ty = Type::List(Box::new(elem_ty.clone()));
                self.unify(expected, &list_ty, span);
                let resolved_elem = self.apply(&elem_ty);
                for p in pats {
                    self.check_pattern(p, &resolved_elem, env, span);
                }
                if let Some(rest_pat) = rest {
                    let rest_ty = Type::List(Box::new(resolved_elem));
                    self.check_pattern(rest_pat, &rest_ty, env, span);
                }
            }
            PatternKind::Record { name, fields, .. } => {
                if let Some(rec_name) = name {
                    if let Some(rec_info) = self.records.get(rec_name).cloned() {
                        let instantiated_fields: Vec<(Symbol, Type)> = if let Some(param_var_ids) =
                            self.record_param_var_ids.get(rec_name).cloned()
                        {
                            let mapping: HashMap<TyVar, Type> = param_var_ids
                                .iter()
                                .map(|&v| (v, self.fresh_var()))
                                .collect();
                            rec_info
                                .fields
                                .iter()
                                .map(|(n, t)| (*n, substitute_vars(t, &mapping)))
                                .collect()
                        } else {
                            rec_info.fields.clone()
                        };

                        let rec_ty = Type::Record(*rec_name, instantiated_fields.clone());
                        self.unify(expected, &rec_ty, span);

                        for (field_name, sub_pat) in fields {
                            if let Some((_, ft)) =
                                instantiated_fields.iter().find(|(n, _)| n == field_name)
                            {
                                if let Some(sp) = sub_pat {
                                    self.check_pattern(sp, ft, env, span);
                                } else {
                                    env.define(*field_name, Scheme::mono(ft.clone()));
                                }
                            } else {
                                // BROKEN-3: Reject unknown field names in
                                // match record patterns at compile time.
                                // GAP (round 26 L5): append a did-you-mean
                                // hint when a near edit-distance field
                                // exists on the record.
                                let base =
                                    format!("record '{rec_name}' has no field '{field_name}'");
                                self.error(
                                    format_record_field_suggestion(
                                        base,
                                        *field_name,
                                        &instantiated_fields,
                                    ),
                                    span,
                                );
                                if let Some(sp) = sub_pat {
                                    let tv = self.fresh_var();
                                    self.check_pattern(sp, &tv, env, span);
                                }
                            }
                        }
                    } else {
                        self.error(
                            format!("undefined record type '{rec_name}' in pattern"),
                            span,
                        );
                        for (_, sub_pat) in fields {
                            if let Some(sp) = sub_pat {
                                let tv = self.fresh_var();
                                self.check_pattern(sp, &tv, env, span);
                            }
                        }
                    }
                } else {
                    for (field_name, sub_pat) in fields {
                        let tv = self.fresh_var();
                        if let Some(sp) = sub_pat {
                            self.check_pattern(sp, &tv, env, span);
                        } else {
                            env.define(*field_name, Scheme::mono(tv));
                        }
                    }
                }
            }
            PatternKind::Or(alts) => {
                // Validate that all alternatives bind the same set of variables.
                if alts.len() >= 2 {
                    let first_vars: BTreeSet<Symbol> =
                        collect_pattern_vars(&alts[0]).into_iter().collect();
                    for (i, alt) in alts.iter().enumerate().skip(1) {
                        let alt_vars: BTreeSet<Symbol> =
                            collect_pattern_vars(alt).into_iter().collect();
                        if first_vars != alt_vars {
                            // BROKEN (round 26 B2): `{:?}` on a BTreeSet<Symbol>
                            // leaks `Symbol(N: "x")` debug output into a
                            // user-facing diagnostic. Render the sets as
                            // sorted comma-separated lists of resolved names.
                            self.error(
                                format!(
                                    "or-pattern alternatives must bind the same variables; \
                                     first alternative binds {}, alternative {} binds {}",
                                    format_symbol_set(&first_vars),
                                    i + 1,
                                    format_symbol_set(&alt_vars)
                                ),
                                span,
                            );
                        }
                    }
                }
                // Check each alternative into a scratch sub-environment so
                // we can collect the per-alternative type for every variable
                // the or-pattern binds, then unify those types pairwise.
                let mut per_alt_types: Vec<HashMap<Symbol, Type>> = Vec::with_capacity(alts.len());
                for alt in alts {
                    let mut alt_env = env.child();
                    self.check_pattern(alt, expected, &mut alt_env, span);
                    let mut names: HashMap<Symbol, Type> = HashMap::new();
                    for name in collect_pattern_vars(alt) {
                        if let Some(scheme) = alt_env.bindings.get(&name) {
                            names.insert(name, scheme.ty.clone());
                        }
                    }
                    per_alt_types.push(names);
                }
                if per_alt_types.len() >= 2 {
                    let (first, rest) = per_alt_types.split_first().unwrap();
                    for other in rest {
                        for (name, first_ty) in first {
                            if let Some(other_ty) = other.get(name) {
                                let a = self.apply(first_ty);
                                let b = self.apply(other_ty);
                                if a != b {
                                    let err_count = self.errors.len();
                                    self.unify(&a, &b, span);
                                    if self.errors.len() > err_count {
                                        self.errors.truncate(err_count);
                                        self.error(
                                            format!(
                                                "or-pattern alternatives bind '{}' to conflicting types: {} vs {}",
                                                name, a, b
                                            ),
                                            span,
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                if let Some(first_alt) = alts.first() {
                    self.check_pattern(first_alt, expected, env, span);
                }
            }
            PatternKind::Range(_, _) => {
                self.unify(expected, &Type::Int, span);
            }
            PatternKind::FloatRange(_, _) => {
                self.unify(expected, &Type::Float, span);
            }
            PatternKind::Map(entries) => {
                // L3: Map patterns are restricted to String keys (parser
                // invariant — see PatternKind::Map in src/ast.rs). Give a
                // targeted error if the scrutinee has a non-String key type.
                let val_ty = self.fresh_var();
                let resolved_scrutinee = self.apply(expected);
                if let Type::Map(existing_key, _) = &resolved_scrutinee {
                    let existing_key = self.apply(existing_key);
                    if !matches!(existing_key, Type::String | Type::Var(_) | Type::Error) {
                        self.error(
                            format!(
                                "map patterns currently only match string keys; your scrutinee has key type '{existing_key}'"
                            ),
                            span,
                        );
                    }
                }
                let key_ty = Type::String;
                let map_ty = Type::Map(Box::new(key_ty), Box::new(val_ty.clone()));
                self.unify(expected, &map_ty, span);
                let resolved_val = self.apply(&val_ty);
                for (_key, pat) in entries {
                    self.check_pattern(pat, &resolved_val, env, span);
                }
            }
            PatternKind::Pin(name) => {
                // Look up the pinned variable in the parent (pre-match) scope,
                // falling back to current scope for when/let contexts.
                let found = env
                    .parent
                    .as_ref()
                    .and_then(|p| p.lookup(*name).cloned())
                    .or_else(|| env.lookup(*name).cloned());
                if let Some(scheme) = found {
                    let pinned_ty = self.instantiate(&scheme);
                    self.unify(expected, &pinned_ty, span);
                } else {
                    // LATENT (round 26 L4): point the caret at the pin
                    // pattern, not the enclosing match scrutinee.
                    let msg = format_undefined_variable_message(*name, env, "in pin pattern");
                    self.error(msg, pattern.span);
                }
            }
        }
    }
}

/// Returns true if the given type is a valid operand for arithmetic operators.
/// `allow_string` widens the domain for `+`, which supports string concatenation.
/// Type variables and `Type::Error` are treated as "maybe valid" (caller handles
/// the Var case via deferred checks).
pub(super) fn is_valid_arith_operand(ty: &Type, allow_string: bool) -> bool {
    match ty {
        Type::Int | Type::Float | Type::ExtFloat | Type::Error | Type::Never => true,
        Type::Var(_) => true,
        Type::String if allow_string => true,
        _ => false,
    }
}

/// Returns true if the given type is a valid operand for comparison operators.
/// `is_equality` widens the domain to include types supported by Value's
/// PartialEq implementation but not `Value::cmp` (Tuple, Map, Set, Bool, Unit).
/// Type variables and `Type::Error` are treated as "maybe valid".
pub(super) fn is_valid_compare_operand(ty: &Type, is_equality: bool) -> bool {
    match ty {
        Type::Int
        | Type::Float
        | Type::ExtFloat
        | Type::String
        | Type::List(_)
        | Type::Record(..)
        | Type::Generic(..)
        | Type::Error
        | Type::Never => true,
        Type::Var(_) => true,
        Type::Bool | Type::Unit | Type::Tuple(_) | Type::Map(..) | Type::Set(_) if is_equality => {
            true
        }
        _ => false,
    }
}

/// Returns true if an expression is a syntactic value for the purpose of the
/// value restriction on let-generalization. Syntactic values (literals,
/// lambdas, identifiers, constructors of values) are safe to generalize;
/// function applications are not, because they may produce types with
/// shared mutable state (e.g. channels) that must remain monomorphic.
pub(super) fn is_syntactic_value(kind: &ExprKind) -> bool {
    match kind {
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Bool(_)
        | ExprKind::StringLit(..)
        | ExprKind::Unit
        | ExprKind::Ident(_)
        | ExprKind::Lambda { .. } => true,
        ExprKind::Tuple(elems) => elems.iter().all(|e| is_syntactic_value(&e.kind)),
        ExprKind::List(elems) => elems.iter().all(|e| match e {
            ListElem::Single(expr) => is_syntactic_value(&expr.kind),
            ListElem::Spread(_) => false,
        }),
        ExprKind::RecordCreate { fields, .. } => {
            fields.iter().all(|(_, e)| is_syntactic_value(&e.kind))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;

    fn assert_no_errors(input: &str) {
        let tokens = crate::lexer::Lexer::new(input)
            .tokenize()
            .expect("lexer error");
        let mut program = crate::parser::Parser::new(tokens)
            .parse_program()
            .expect("parse error");
        let errors = check(&mut program);
        let hard: Vec<_> = errors
            .iter()
            .filter(|e| e.severity == Severity::Error)
            .collect();
        assert!(
            hard.is_empty(),
            "expected no type errors, got:\n{}",
            hard.iter()
                .map(|e| format!("  {e}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    fn assert_has_error(input: &str, expected: &str) {
        let tokens = crate::lexer::Lexer::new(input)
            .tokenize()
            .expect("lexer error");
        let mut program = crate::parser::Parser::new(tokens)
            .parse_program()
            .expect("parse error");
        let errors = check(&mut program);
        assert!(
            errors.iter().any(|e| e.message.contains(expected)),
            "expected error containing '{expected}', got: {:?}",
            errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    // ── Unary operator inference ────────────────────────────────────

    #[test]
    fn test_unary_negate_int() {
        assert_no_errors(
            r#"
fn main() {
  let x = -42
  x
}
        "#,
        );
    }

    #[test]
    fn test_unary_negate_float() {
        assert_no_errors(
            r#"
fn main() {
  let x = -3.14
  x
}
        "#,
        );
    }

    #[test]
    fn test_unary_not_bool() {
        assert_no_errors(
            r#"
fn main() {
  let x = !true
  x
}
        "#,
        );
    }

    #[test]
    fn test_unary_not_non_bool() {
        assert_has_error(
            r#"
fn main() {
  !42
}
        "#,
            "type mismatch",
        );
    }

    // ── Or-pattern binding ──────────────────────────────────────────

    #[test]
    fn test_or_pattern_binds_variable() {
        assert_no_errors(
            r#"
fn classify(x) {
  match x {
    1 | 2 | 3 -> "small"
    _ -> "big"
  }
}
fn main() { classify(2) }
        "#,
        );
    }

    #[test]
    fn test_or_pattern_with_constructor_binding() {
        assert_no_errors(
            r#"
fn extract(x) {
  match x {
    Ok(v) | Err(v) -> v
  }
}
fn main() { extract(Ok(42)) }
        "#,
        );
    }

    // ── Map pattern binding ─────────────────────────────────────────

    #[test]
    fn test_map_pattern_in_match() {
        assert_no_errors(
            r#"
fn main() {
  let m = #{ "x": 1, "y": 2 }
  match m {
    #{ "x": val } -> val
    _ -> 0
  }
}
        "#,
        );
    }

    // ── Pin pattern ─────────────────────────────────────────────────

    #[test]
    fn test_pin_pattern_matches_value() {
        assert_no_errors(
            r#"
fn main() {
  let expected = 42
  match 42 {
    ^expected -> "matched"
    _ -> "no match"
  }
}
        "#,
        );
    }

    // ── Return expression ───────────────────────────────────────────

    #[test]
    fn test_return_with_value() {
        assert_no_errors(
            r#"
fn early(x) {
  when x > 0 else { return 0 }
  x * 2
}
fn main() { early(5) }
        "#,
        );
    }

    #[test]
    fn test_return_no_value() {
        assert_no_errors(
            r#"
fn side_effect(x) {
  when x > 0 else { return () }
  println(x)
}
fn main() { side_effect(1) }
        "#,
        );
    }

    // ── String interpolation inference ──────────────────────────────

    #[test]
    fn test_string_interp_with_int_and_bool() {
        assert_no_errors(
            r#"
fn main() {
  let n = 42
  let b = true
  "n={n}, b={b}"
}
        "#,
        );
    }

    // ── Ascription ──────────────────────────────────────────────────

    #[test]
    fn test_ascription_correct_type() {
        assert_no_errors(
            r#"
fn main() {
  let x = 42 as Int
  x
}
        "#,
        );
    }

    #[test]
    fn test_ascription_mismatch() {
        assert_has_error(
            r#"
fn main() {
  "hello" as Int
}
        "#,
            "type mismatch",
        );
    }

    // ── Pipe operator inference ─────────────────────────────────────

    #[test]
    fn test_pipe_chains_types() {
        assert_no_errors(
            r#"
fn double(x) = x * 2
fn add_one(x) = x + 1
fn main() {
  5 |> double |> add_one
}
        "#,
        );
    }

    // ── Record update inference ─────────────────────────────────────

    #[test]
    fn test_record_update_preserves_type() {
        assert_no_errors(
            r#"
type Point { x: Int, y: Int }
fn main() {
  let p = Point { x: 1, y: 2 }
  let q = p.{ x: 10 }
  q.y
}
        "#,
        );
    }

    #[test]
    fn test_record_create_wrong_field_type() {
        assert_has_error(
            r#"
type Point { x: Int, y: Int }
fn main() {
  Point { x: "hello", y: 2 }
}
        "#,
            "type mismatch",
        );
    }

    // ── check_pattern error cases ───────────────────────────────────

    #[test]
    fn test_check_pattern_wrong_type() {
        assert_has_error(
            r#"
fn main() {
  match 42 {
    true -> "yes"
    false -> "no"
  }
}
        "#,
            "type mismatch",
        );
    }

    #[test]
    fn test_list_rest_pattern_binds() {
        assert_no_errors(
            r#"
fn sum_list(xs) {
  match xs {
    [] -> 0
    [head, ..tail] -> head + sum_list(tail)
  }
}
fn main() { sum_list([1, 2, 3]) }
        "#,
        );
    }

    // ── Range pattern type checking ─────────────────────────────────

    #[test]
    fn test_range_pattern_int() {
        assert_no_errors(
            r#"
fn classify(n) {
  match n {
    1..10 -> "small"
    _ -> "big"
  }
}
fn main() { classify(5) }
        "#,
        );
    }

    // ── When-bool statement ─────────────────────────────────────────

    #[test]
    fn test_when_bool_condition_must_be_bool() {
        assert_has_error(
            r#"
fn check(x) {
  when 42 else { return 0 }
  x
}
fn main() { check(1) }
        "#,
            "type mismatch",
        );
    }

    // ── Loop/recur type inference ───────────────────────────────────

    #[test]
    fn test_loop_bindings_inferred() {
        assert_no_errors(
            r#"
fn factorial(n) {
  loop i = n, acc = 1 {
    match i <= 1 {
      true -> acc
      false -> loop(i - 1, acc * i)
    }
  }
}
fn main() { factorial(5) }
        "#,
        );
    }

    // ── Trait constraint checking at definition ────────────────────

    #[test]
    fn test_trait_constraint_method_resolved() {
        // A constrained type variable should allow calling trait methods
        assert_no_errors(
            r#"
trait Display for a {
  fn display(self) -> String { "?" }
}
fn show(x: a) -> String where a: Display {
  x.display()
}
fn main() { show(42) }
        "#,
        );
    }

    #[test]
    fn test_trait_constraint_unknown_method_errors() {
        // A constrained type variable should NOT allow calling methods not in the trait
        assert_has_error(
            r#"
trait Display for a {
  fn display(self) -> String { "?" }
}
fn show(x: a) -> String where a: Display {
  x.nonexistent()
}
fn main() { show(42) }
        "#,
            "no method 'nonexistent' found in trait constraints",
        );
    }

    // ── Error type propagation ─────────────────────────────────────

    #[test]
    fn test_error_type_does_not_produce_fresh_var() {
        // Accessing a field on an error type should propagate the error,
        // not create a new unresolved type variable
        assert_has_error(
            r#"
fn main() {
  let x = undefined_var
  x.field
}
        "#,
            "undefined",
        );
    }

    // ── B2: String Sub/Mul/Mod should be rejected ──────────────────

    #[test]
    fn test_string_add_is_allowed() {
        assert_no_errors(
            r#"
fn main() {
  "hello" + " world"
}
        "#,
        );
    }

    #[test]
    fn test_string_sub_is_rejected() {
        assert_has_error(
            r#"
fn main() {
  "hello" - "world"
}
        "#,
            "operator requires numeric types",
        );
    }

    #[test]
    fn test_string_mul_is_rejected() {
        assert_has_error(
            r#"
fn main() {
  "hello" * "world"
}
        "#,
            "operator requires numeric types",
        );
    }

    #[test]
    fn test_string_mod_is_rejected() {
        assert_has_error(
            r#"
fn main() {
  "hello" % "world"
}
        "#,
            "operator requires numeric types",
        );
    }

    // ── L1: Parameterized records should get fresh type vars ───────

    #[test]
    fn test_parameterized_record_different_instantiations() {
        assert_no_errors(
            r#"
type Box(a) { value: a }
fn main() {
  let int_box = Box { value: 42 }
  let str_box = Box { value: "hello" }
  int_box.value + 1
  str_box.value + " world"
}
        "#,
        );
    }

    // ── Constructor arity in let bindings ──────────────────────────

    #[test]
    fn test_let_constructor_wrong_arity_is_type_error() {
        assert_has_error(
            r#"
type Maybe(T) {
  None
  Some(T)
}
fn main() {
  let Some(x, y) = Some(42)
  0
}
        "#,
            "constructor 'Some' expects 1 field, but pattern has 2",
        );
    }

    #[test]
    fn test_let_nested_constructor_wrong_arity_is_type_error() {
        assert_has_error(
            r#"
type Maybe(T) {
  None
  Some(T)
}
type Pair(A, B) {
  P(A, B)
}
fn main() {
  let P(Some(x, y), b) = P(Some(42), 1)
  0
}
        "#,
            "constructor 'Some' expects 1 field, but pattern has 2",
        );
    }
}
