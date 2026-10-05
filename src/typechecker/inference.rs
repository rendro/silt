//! Type inference for expressions, statements, and patterns.
//!
//! This module contains the core inference logic: infer_expr, infer_stmt,
//! bind_pattern, check_pattern, and check_fn_body.

use super::infer::pattern::*;
use super::suggest::suggest_similar;
use super::*;

/// Where an expression stands relative to its enclosing `loop`, for the
/// check that `loop(...)` is in tail position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecurPos {
    /// The loop's result: a `loop(...)` here restarts the loop.
    Tail,
    /// Somewhere whose value is used.
    NotTail,
    /// Inside a closure in the loop body.
    InClosure,
}

impl RecurPos {
    /// The position of a sub-expression whose value is used.
    fn inner(self) -> RecurPos {
        match self {
            RecurPos::InClosure => RecurPos::InClosure,
            RecurPos::Tail | RecurPos::NotTail => RecurPos::NotTail,
        }
    }
}

/// GAP (round 17 F5): pick the singular or plural form of a word
/// based on `n`. Used to render arity/field/binding counts in
/// typechecker diagnostics without the awkward "1 argument(s)" that
/// tooling and users had been complaining about.
pub(super) fn plural<'a>(n: usize, singular: &'a str, plural_form: &'a str) -> &'a str {
    if n == 1 { singular } else { plural_form }
}

/// The arity rule of every call form — `f(a, b)`, `a |> f(b)` and
/// `a |> f`: a call supplies exactly as many arguments as the callee has
/// parameters, or one fewer when the callee's signature declares its last
/// parameter optional (`Scheme::optional_last_param`).
///
/// `supplied` counts every argument the callee receives, including a
/// piped value and the implicit `self` of a method call.
pub(super) fn call_arity_matches(
    params: usize,
    optional_last_param: bool,
    supplied: usize,
) -> bool {
    supplied == params || (optional_last_param && supplied + 1 == params)
}

/// The argument counts a callee accepts, worded for a diagnostic:
/// "1 argument", "2 arguments", or "2 or 3 arguments" for a callee whose
/// last parameter is optional.
pub(super) fn accepted_arity_text(params: usize, optional_last_param: bool) -> String {
    if optional_last_param && params > 0 {
        format!("{} or {} arguments", params - 1, params)
    } else {
        format!("{} {}", params, plural(params, "argument", "arguments"))
    }
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

/// Format an "undefined variable '<typo>'" error message, with an
/// optional "did you mean `<cand>`?" help line. Sourced candidates come from every in-scope name
/// the typechecker's env chain exposes (locals, fn params, top-level
/// decls, stdlib builtins) — the caller hands us `env`. If no candidate
/// passes the suggest-similar threshold, we emit the plain error.
///
/// Closes round-17 deferred finding #4: see `src/typechecker/suggest.rs`.
/// Lock: tests/lang/diagnostic_suggestion_tests.rs.
pub(super) fn format_undefined_variable_message(
    name: Symbol,
    env: &TypeEnv,
    suffix: &str,
) -> (String, Option<String>) {
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
        // F12 (round 67): mirror parser.rs G1 (parser.rs:2245-2255) for
        // the expression-position case. The parser only catches these
        // at statement position (and gates on a "looks like a mistake"
        // lookahead); in expression position the parser parses `if` /
        // `while` / `for` as bare identifiers and we land here with an
        // "undefined variable" diagnostic. Wording is copied verbatim
        // from parser.rs so both paths give the user identical advice.
        "if" => Some("silt has no 'if' keyword — use 'match cond { true -> ..., false -> ... }'"),
        "while" | "for" => Some(
            "silt has no 'while'/'for' keywords — use tail-recursive 'loop' or 'list.each' / 'list.map'",
        ),
        _ => None,
    };
    if let Some(hint) = foreign_keyword_hint {
        return (base, Some(hint.to_string()));
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
    let help = suggest_similar(&name_str, candidate_strs.iter())
        .map(|hint| format!("did you mean `{hint}`?"));
    (base, help)
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
) -> (String, Option<String>) {
    let field_str = resolve(field);
    let candidates: Vec<String> = record_fields.iter().map(|(n, _)| resolve(*n)).collect();
    let help = suggest_similar(&field_str, candidates.iter())
        .map(|hint| format!("did you mean `{hint}`?"));
    (base, help)
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
    method_table: &HashMap<(TypeRef, Symbol), MethodEntry>,
    table_key: TypeRef,
) -> (String, Option<String>) {
    let field_str = resolve(field);
    let base = format!("unknown method '{field_str}' on {display_type_name}");
    let candidates: Vec<String> = method_table
        .keys()
        .filter(|(ty, _)| *ty == table_key)
        .map(|(_, m)| resolve(*m).to_string())
        .collect();
    let help = suggest_similar(&field_str, candidates.iter())
        .map(|hint| format!("did you mean `{hint}`?"));
    (base, help)
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
    fn covered_by_active_constraint(&self, resolved: &Type, trait_name: TraitKey) -> bool {
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

    /// Report a call of `method`, of the trait `trait_name` another module
    /// declares without `pub`: its methods can be called only there.
    pub(super) fn private_method(&mut self, trait_name: TraitKey, method: Symbol, span: Span) {
        let module = self.private_owner(trait_name).unwrap_or(self.module_name);
        self.errors.push(
            Diagnostic::error(
                Code::PrivateItem,
                span,
                format!(
                    "method `{method}` belongs to trait '{trait_name}', which is private to \
                     module '{module}'"
                ),
            )
            .with_help(format!(
                "mark it `pub trait {trait_name}` in module '{module}' to call its methods \
                 from another module"
            )),
        );
    }

    /// The module that declares the trait `trait_name` without `pub`, by
    /// name, when it is another module: the trait's methods cannot be
    /// called here.
    pub(super) fn private_owner(&self, trait_name: TraitKey) -> Option<Symbol> {
        let (owner, module) = self.tables.traits.get(&trait_name)?.private_to?;
        (owner != self.module).then_some(module)
    }

    /// The private trait of another module that alone provides `method`:
    /// every impl that has a method of that name is of such a trait, and
    /// no trait this module may name declares it.
    fn only_private_provider(&self, method: Symbol) -> Option<TraitKey> {
        let mut providers = self
            .tables
            .method_table
            .iter()
            .filter(|((_, m), _)| *m == method)
            .map(|(_, entry)| entry.trait_name);
        let first = providers.next()??;
        if self.private_owner(first).is_none()
            || providers.any(|t| t.is_none_or(|t| self.private_owner(t).is_none()))
        {
            return None;
        }
        let visible_declares = self.tables.traits.iter().any(|(key, info)| {
            self.private_owner(*key).is_none() && info.methods.iter().any(|(n, _)| *n == method)
        });
        (!visible_declares).then_some(first)
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
        let head = self.type_name_for_impl(&self.apply(receiver_ty));
        // A call that names its trait (a derived impl's body) calls that
        // trait's method.
        let forced_entry = match (self.forced_trait, head) {
            (Some(t), Some(head)) if self.entry_trait(entry, method_name) != Some(t) => {
                self.trait_method_entry(head, method_name, t)
            }
            _ => None,
        };
        let entry = forced_entry.as_ref().unwrap_or(entry);
        // A method of a trait another module declares without `pub` can
        // be called only in that module.
        if let Some(trait_name) = entry.trait_name
            && self.private_owner(trait_name).is_some()
        {
            self.private_method(trait_name, method_name, span);
            return Type::Error;
        }
        if self.forced_trait.is_none()
            && let Some(head) = head
            && self.ambiguous_method_call(head, method_name, span)
        {
            return Type::Error;
        }
        self.method_trait = self
            .forced_trait
            .or_else(|| self.entry_trait(entry, method_name));
        let (instantiated_ty, constraints) = self.instantiate_method_entry(entry);
        // Reject value-receiver calls on no-self trait methods (`empty`,
        // `default`, etc.). The method has no slot for the receiver, so
        // invoking it via `instance.method()` is meaningless. Point the
        // user at the type-level form `TypeName.method()` and return
        // `Type::Error` so the downstream Call arm doesn't pile an arity
        // mismatch on top of the real diagnostic.
        if let Type::Fun(params, _) = &instantiated_ty
            && params.is_empty()
        {
            let suggestion = self
                .type_name_for_impl(&self.apply(receiver_ty))
                .map(|ty| format!("`{ty}.{method_name}()`"))
                .unwrap_or_else(|| format!("`SomeType.{method_name}()`"));
            self.error(
                Code::InvalidMethodCall,
                format!(
                    "method `{method_name}` takes no `self` — \
                     call it on the type instead: {suggestion}"
                ),
                span,
            );
            return Type::Error;
        }
        // Unify the receiver with the method's self param so concrete
        // receiver element types flow into the impl's tyvars before the
        // constraint check below.
        if let Type::Fun(params, _) = &instantiated_ty
            && let Some(self_param) = params.first()
        {
            self.unify(receiver_ty, self_param, span);
        }
        for (tv, trait_name, entry_bound_args) in constraints {
            let resolved = self.apply(&Type::Var(tv));
            // Prefer the bound's own trait args carried on the
            // MethodEntry constraint triple — they're the source of
            // truth for impl- / method-level `where a: Conv(Int)`
            // clauses. Fall back to the side-channel
            // `trait_arg_bindings` map for the legacy fn-decl-level
            // path (round 58) which populates that map directly.
            // Empty when the trait has no parameters.
            let bound_args = if !entry_bound_args.is_empty() {
                entry_bound_args.clone()
            } else {
                self.trait_arg_bindings
                    .get(&(tv, trait_name))
                    .cloned()
                    .or_else(|| {
                        if let Type::Var(v) = &resolved {
                            self.trait_arg_bindings.get(&(*v, trait_name)).cloned()
                        } else {
                            None
                        }
                    })
                    .unwrap_or_default()
            };
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
                            bound_trait_args: bound_args,
                        });
                    }
                }
                _ => {
                    // Concrete receiver — check trait impl exists now,
                    // recursively walking the impl's own where clauses
                    // against the receiver's type arguments.
                    self.verify_trait_obligation(trait_name, &bound_args, &resolved, span);
                }
            }
        }
        self.apply(&instantiated_ty)
    }

    /// Resolve a method on a type descriptor (`TypeOf(inner)`). The
    /// descriptor is a type carrier — lookup uses `inner`'s effective type
    /// name (for concrete inners) or the active trait constraints (for a
    /// type variable inner). Returns the method's function type with all
    /// `Self` references substituted to `inner`.
    ///
    /// Unlike value-receiver dispatch, the descriptor does NOT occupy an
    /// argument slot of the method. Callers signal this by leaving
    /// `last_field_access_was_method = false` after this returns, so the
    /// downstream Call arm unifies args with params[0..] rather than
    /// params[1..].
    pub(super) fn resolve_type_descriptor_method(
        &mut self,
        inner: &Type,
        field: Symbol,
        span: Span,
    ) -> Option<Type> {
        let inner = self.apply(inner);
        match &inner {
            Type::Var(v) => {
                // Look up trait methods via the constraints on `v`. The
                // where-clause guarantees at least one impl exists at
                // every call site; dispatch happens at runtime via the
                // descriptor's carried type name.
                let Some(trait_names) = self.active_constraints.get(v).cloned() else {
                    self.error(
                        Code::UnknownMethod,
                        format!(
                            "no method '{field}' on `type {inner}` — \
                             the type variable has no trait constraints. \
                             Add a `where` clause such as `where {inner}: SomeTrait`."
                        ),
                        span,
                    );
                    return None;
                };
                let mut matches: Vec<(TraitKey, Type)> = Vec::new();
                for trait_name in &trait_names {
                    if let Some(trait_info) = self.tables.traits.get(trait_name).cloned()
                        && let Some((_, method_ty)) =
                            trait_info.methods.iter().find(|(n, _)| *n == field)
                    {
                        // Substitute trait-level parameters with the
                        // concrete args supplied by the enclosing where
                        // clause (`where v: Trait(X)`). Without this,
                        // `a.try_into()` on `a: TryInto(Int)` would
                        // return the trait's template `b` TyVar instead
                        // of `Int`.
                        let substituted = if let Some(bound_args) =
                            self.trait_arg_bindings.get(&(*v, *trait_name))
                            && bound_args.len() == trait_info.param_var_ids.len()
                        {
                            let mapping: HashMap<TyVar, Type> = trait_info
                                .param_var_ids
                                .iter()
                                .zip(bound_args.iter())
                                .map(|(&tv, arg)| (tv, arg.clone()))
                                .collect();
                            substitute_vars(method_ty, &mapping)
                        } else {
                            method_ty.clone()
                        };
                        matches.push((*trait_name, substituted));
                    }
                }
                if matches.is_empty() {
                    let traits_str = trait_names
                        .iter()
                        .map(|s| format!("{s}"))
                        .collect::<Vec<_>>()
                        .join(" + ");
                    self.error(
                        Code::UnknownMethod,
                        format!(
                            "no method '{field}' found on `type {inner}` \
                             in trait constraints ({traits_str})"
                        ),
                        span,
                    );
                    return None;
                }
                if let Some(t) = self.forced_trait {
                    matches.retain(|(n, _)| *n == t);
                }
                if matches.len() > 1 {
                    let trait_list = matches
                        .iter()
                        .map(|(name, _)| self.show_trait(*name))
                        .collect::<Vec<_>>()
                        .join(", ");
                    self.error(
                        Code::AmbiguousMethod,
                        format!(
                            "ambiguous method '{field}' on `type {inner}`: \
                             provided by multiple traits ({trait_list})"
                        ),
                        span,
                    );
                    return None;
                }
                // Instantiate the trait-method template with fresh vars,
                // then rebind `Self` to the descriptor's inner type.
                // TraitInfo.methods stores bare Types whose TyVars were
                // allocated once at register_trait_decl; instantiate so
                // repeated call sites don't share bindings.
                self.method_trait = Some(matches[0].0);
                let instantiated = self.instantiate_method_type(&matches[0].1);
                let resolved = self.apply(&instantiated);
                Some(resolved)
            }
            _ => {
                // Concrete inner — look up via the method table, keyed on
                // the effective type name (same path as
                // `type_name_for_impl`).
                let name = self.type_name_for_impl(&inner)?;
                let entry = self.tables.method_table.get(&(name, field)).cloned()?;
                if let Some(trait_name) = entry.trait_name
                    && self.private_owner(trait_name).is_some()
                {
                    self.private_method(trait_name, field, span);
                    return Some(Type::Error);
                }
                if self.ambiguous_method_call(name, field, span) {
                    return Some(Type::Error);
                }
                self.method_trait = self.entry_trait(&entry, field);
                let (instantiated, _constraints) = self.instantiate_method_entry(&entry);
                Some(self.apply(&instantiated))
            }
        }
    }

    /// Report `T.field` where the type `T` has no method `field`. A builtin
    /// module named like the type with a function of that name (`int.parse`
    /// for `Int.parse`) is suggested.
    fn no_type_method(&mut self, type_name: &str, field: Symbol, span: Span) {
        let module = type_name.to_lowercase();
        let mut d = Diagnostic::error(
            Code::UnresolvedName,
            span,
            format!("type '{type_name}' has no method '{field}'"),
        );
        let qualified = intern(&format!("{module}.{field}"));
        if crate::module::is_builtin_module(&module) && builtin_env_has(qualified) {
            d = d.with_help(format!(
                "did you mean `{module}.{field}`, a function of module `{module}`?"
            ));
        }
        self.errors.push(d);
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
    pub(super) fn expand_with_supertraits(&self, traits: &[TraitKey]) -> Vec<TraitKey> {
        use std::collections::HashSet;
        let mut expanded = Vec::new();
        let mut stack: Vec<TraitKey> = traits.to_vec();
        let mut seen: HashSet<TraitKey> = HashSet::new();
        while let Some(t) = stack.pop() {
            if seen.insert(t) {
                expanded.push(t);
                if let Some(info) = self.tables.traits.get(&t) {
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
        for wc in &f.where_clauses {
            // A bound the resolver resolved to nothing: it reported why.
            if wc.trait_res == Some(crate::defs::Res::Error) {
                continue;
            }
            let type_param = &wc.type_param;
            let trait_args = &wc.trait_args;
            let Some(trait_name) = self
                .named_trait(wc.trait_res, wc.trait_name)
                .filter(|t| self.tables.traits.contains_key(t))
            else {
                self.error(
                    Code::UnknownTrait,
                    format!(
                        "unknown trait '{}' in where clause for '{}'",
                        wc.trait_name, type_param
                    ),
                    f.span,
                );
                continue;
            };
            // G1 (round 60, extended round 101): a where-clause bound
            // must supply exactly the trait's declared number of type
            // arguments. Zero args on a parameterized trait
            // (`trait Cast(to)` + `where a: Cast`) leaves the implied
            // `to` unresolved; a nonzero length MISMATCH
            // (`where a: Cast(Int, String)`) is worse — the
            // parameterized-trait verification in
            // `verify_trait_obligation` only runs its round-58
            // positional arg-compatibility zip when the bound's and
            // the impl's arg lists have equal length, so a mismatched
            // bound silently degraded to a bare "implements Cast"
            // check and matched ANY `Cast(*)` impl. The arity check
            // lives at the user-written syntax sites (here, plus the
            // impl-level and method-level where-clause loops in
            // `register_trait_impl`) because bare `&[]` is legitimate
            // for supertrait sub-obligations inside
            // `verify_trait_obligation` itself.
            self.check_where_bound_arity(trait_name, trait_args.len(), f.span);
        }

        // Look up the function's registered type and instantiate it.
        // A failed lookup has already been reported by an earlier
        // pass, so no diagnostic is added here; `?` hands `None`
        // back to the caller.
        let fn_scheme = env.lookup(lookup_name)?.clone();
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
        //
        // For parameterized supertraits (`trait Sub(a): Super(a)`), the
        // enclosing trait's args flow into the supertrait via the name
        // mapping stored in `supertrait_args` / `params`. When we expand
        // `v: Sub(Int)` to also register `v: Super`, we substitute the
        // supertrait reference's arg-list through Sub's param → arg map
        // and stash the result in `trait_arg_bindings` so later
        // descriptor method resolution sees Super's concrete args.
        let prev_constraints = std::mem::take(&mut self.active_constraints);
        for (tv, trait_name) in &constraints {
            for expanded in self.expand_with_supertraits(&[*trait_name]) {
                let entry = self.active_constraints.entry(*tv).or_default();
                if !entry.contains(&expanded) {
                    entry.push(expanded);
                }
            }
            // Propagate supertrait args from the enclosing trait's
            // bindings to each named supertrait.
            if let Some(info) = self.tables.traits.get(trait_name).cloned() {
                let base_args: Vec<Type> = self
                    .trait_arg_bindings
                    .get(&(*tv, *trait_name))
                    .cloned()
                    .unwrap_or_default();
                for (i, super_name) in info.supertraits.iter().enumerate() {
                    let arg_exprs = info.supertrait_args.get(i);
                    let resolved_args: Vec<Type> = match arg_exprs {
                        Some(exprs) if !exprs.is_empty() => {
                            // LATENT (round 88): before resolving, walk each
                            // supertrait-arg TypeExpr and emit a diagnostic
                            // when a bare parametric type name (e.g. `Box`
                            // where `type Box(a) { ... }`) is used without
                            // its type arguments — the free resolver would
                            // otherwise silently produce a 0-arity Generic
                            // that fails to match any impl downstream with
                            // no user-facing error.
                            for te in exprs.iter() {
                                self.check_supertrait_arg_parametric_arity(te, &info);
                            }
                            exprs
                                .iter()
                                .map(|te| resolve_supertrait_arg(te, &info, &base_args))
                                .collect()
                        }
                        _ => continue,
                    };
                    self.trait_arg_bindings
                        .insert((*tv, *super_name), resolved_args);
                }
            }
        }

        // Round 64 item 6B: record the fn name we're checking so the
        // Call arm can detect recursive call sites and attach the
        // polymorphic-recursion-hint note when an unannotated fn's
        // body recurses with a different concrete type. We track the
        // bare AST decl name (`f.name`) — recursive references inside
        // the body always use that name, not the method-table-style
        // `Type.method` lookup key used for trait impls.
        let prev_fn_name = self.current_fn_name.replace(f.name);

        // B4: capture the instantiated param tyvars so call-site where-
        // clause checks can determine whether a pending obligation
        // touches the enclosing fn's own polymorphism (vs. an unrelated
        // top-level or downstream Var that will resolve via pass-3
        // narrowing). We store just the Var IDs — concrete params are
        // not of interest here.
        let prev_fn_param_tyvars = std::mem::take(&mut self.current_fn_param_tyvars);
        for pt in &param_types {
            let applied = self.apply(pt);
            match &applied {
                Type::Var(v) => self.current_fn_param_tyvars.push(*v),
                Type::Generic(name, args)
                    if name.is_builtin(crate::defs::TYPE_OF) && args.len() == 1 =>
                {
                    if let Type::Var(v) = self.apply(&args[0]) {
                        self.current_fn_param_tyvars.push(v);
                    }
                }
                _ => {}
            }
        }

        // Bind parameters
        // Soundness: reject duplicate binding names across the whole fn
        // param list before we start defining them in the env. Without
        // this, `fn f(a: Int, a: Int)` typechecks and the second param
        // silently shadows the first. See `check_fn_params_duplicate_bindings`.
        self.check_fn_params_duplicate_bindings(&f.params);
        for (i, param) in f.params.iter().enumerate() {
            if let Some(ty) = param_types.get(i) {
                self.bind_irrefutable_pattern(
                    &param.pattern,
                    ty,
                    &mut local_env,
                    f.span,
                    BindingSite::FnParam,
                );
            }
        }

        // Set the expected return type for return and ? validation
        let prev_return_type = self.current_return_type.take();
        self.current_return_type = Some(ret_type.clone());
        // Round 93: fresh `?`-site tracking per fn body, so a failed
        // body/return unify can point back at the `?` that demanded a
        // Result/Option return.
        let prev_qmark_spans = std::mem::take(&mut self.current_qmark_spans);

        // Infer the body and unify with declared return type
        let body_type = self.infer_expr(&mut f.body, &mut local_env);
        let ret_unify_err_count = self.errors.len();
        self.unify(&body_type, &ret_type, f.body.span);
        self.retarget_ok_wrap_fixes(ret_unify_err_count, &f.body);
        self.note_qmark_requirement_on_ret_mismatch(ret_unify_err_count, &ret_type);

        // Record the body-constrained function type for scheme narrowing
        let constrained_params: Vec<Type> = param_types.iter().map(|t| self.apply(t)).collect();
        let constrained_ret = self.apply(&ret_type);
        let constrained_fn = Type::Fun(constrained_params, Box::new(constrained_ret));
        self.fn_body_types
            .insert(lookup_name, constrained_fn.clone());

        // Restore previous constraints and return type
        self.current_return_type = prev_return_type;
        self.current_qmark_spans = prev_qmark_spans;
        self.active_constraints = prev_constraints;
        self.current_fn_param_tyvars = prev_fn_param_tyvars;
        self.current_fn_name = prev_fn_name;

        Some(constrained_fn)
    }

    /// The Ok-wrap fixes of the diagnostics from `from` on that are about
    /// the whole `body` of a function: a block is wrapped at its tail
    /// expression, not around its braces, and a block without one gets
    /// no fix.
    fn retarget_ok_wrap_fixes(&mut self, from: usize, body: &Expr) {
        let tail = match &body.kind {
            ExprKind::Block(stmts) => match stmts.last() {
                Some(Stmt::Expr(e)) => Some(e.span),
                _ => None,
            },
            _ => return,
        };
        for d in self.errors.iter_mut().skip(from) {
            if d.span != body.span {
                continue;
            }
            match tail {
                Some(tail) => {
                    for fix in &mut d.fixes {
                        fix.edits = vec![
                            (Span::point(tail.file, tail.start), "Ok(".to_string()),
                            (Span::point(tail.file, tail.end), ")".to_string()),
                        ];
                    }
                }
                None => d.fixes.clear(),
            }
        }
    }

    /// GAP (round 93): when the body/return-type unify fails AND the
    /// return type was driven to Result/Option by a `?` in the body,
    /// append a note naming the `?` site. Without this, `?` inside an
    /// unannotated (effectively unit-returning) fn surfaces as a bare
    /// "type mismatch: expected Result(_, String), got ()" with the
    /// caret on the fn header — no pointer to the `?` that created the
    /// requirement. Companion to `chain_hint` (mod.rs), which covers
    /// the inverse direction (got Result/Option, expected plain): both
    /// append a continuation line to the raw unify mismatch rather
    /// than emitting a separate diagnostic.
    fn note_qmark_requirement_on_ret_mismatch(&mut self, err_count_before: usize, ret_ty: &Type) {
        if self.errors.len() == err_count_before {
            return;
        }
        let Some(qspan) = self.current_qmark_spans.first().copied() else {
            return;
        };
        // Only when the return type is actually Result/Option-shaped —
        // i.e. the shape `?` itself unifies into the return type. If the
        // user annotated something else, the `?` site already got its own
        // error and this note would mislead.
        let ret_resolved = self.apply(ret_ty);
        let is_qmark_shape = matches!(
            &ret_resolved,
            Type::Generic(n, _) if n.is_builtin("Result") || n.is_builtin("Option")
        );
        if !is_qmark_shape {
            return;
        }
        if let Some(err) = self.errors.last_mut() {
            err.labels.push((
                qspan,
                format!("this `?` requires this function to return {ret_resolved}"),
            ));
            // The `?` made the return type a `Result`; wrapping the body
            // in `Ok(...)` would not fix the function.
            err.fixes.clear();
        }
    }

    // ── Named records and variants ──────────────────────────────────
    //
    // What a record literal, record pattern or variant pattern names
    // (`util.Pt { x: 1 }`, `shapes.Circle(r)`), the resolver decided; the
    // helpers below turn its resolution into the checker's tables.

    /// Instantiate a record's declared field types, substituting fresh
    /// type variables for the record's type parameters (if any) so
    /// distinct uses don't share template variables. Shared by the
    /// record-literal and record-pattern paths.
    pub(super) fn instantiate_record_fields(
        &mut self,
        rec_info: &RecordInfo,
        param_var_ids: Option<&[TyVar]>,
    ) -> Vec<(Symbol, Type)> {
        if let Some(param_var_ids) = param_var_ids {
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
        }
    }

    /// Whether a name written as a constructor names a record type.
    pub(super) fn names_record(&self, res: Option<crate::defs::Res>, name: Symbol) -> bool {
        self.res_type(res)
            .or_else(|| self.named_type(None, name))
            .is_some_and(|ty| self.tables.records.contains_key(&ty))
    }

    /// The record type a record pattern or literal names, with its type
    /// parameters' variables: the one the resolver resolved it to.
    /// Reports a name that names no record type, unless the resolver
    /// reported it already.
    pub(super) fn named_record(
        &mut self,
        res: Option<crate::defs::Res>,
        name: Symbol,
        span: Span,
        in_pattern: bool,
    ) -> Option<(TypeRef, RecordInfo, Option<Vec<TyVar>>)> {
        if res == Some(crate::defs::Res::Error) || self.names_rejected(res, name) {
            return None;
        }
        let ty = self.named_type(res, name);
        let name = ty.map_or(name, |ty| ty.name);
        match ty.and_then(|ty| Some((ty, self.tables.records.get(&ty).cloned()?))) {
            Some((ty, info)) => {
                let ids = self.tables.record_param_var_ids.get(&ty).cloned();
                Some((ty, info, ids))
            }
            None => {
                let message = if in_pattern {
                    format!("undefined record type '{name}' in pattern")
                } else {
                    format!("undefined type '{name}'")
                };
                self.error(Code::UndefinedType, message, span);
                None
            }
        }
    }

    /// Whether `callee` is a member of a module, `m.f`, as the resolver
    /// resolved it: the Call arm then uses the qualified `m.f` scheme
    /// directly, so that its where-clause constraints flow through
    /// (`instantiate_with_constraints` carries them; the FieldAccess arm's
    /// `instantiate` would drop them).
    fn callee_module_is_in_scope(&self, callee: &Expr, _env: &TypeEnv) -> bool {
        let ExprKind::FieldAccess(obj, _, _) = &callee.kind else {
            return false;
        };
        matches!(obj.res, Some(crate::defs::Res::Module(_)))
            && !matches!(callee.res, Some(crate::defs::Res::Error))
    }

    /// Whether the function that `callee` names declares its last
    /// parameter optional (see `Scheme::optional_last_param`).
    ///
    /// Only a bare function name or a `module.function` path names a
    /// signature. Any other callee expression is a function value, and a
    /// function value is called with its full arity. A `module.function`
    /// path counts only when it really is a module-qualified name, by
    /// the same test the Call arm uses to pick the qualified scheme.
    fn callee_declares_optional_last_param(&self, callee: &Expr, env: &TypeEnv) -> bool {
        self.callee_scheme(callee, env)
            .is_some_and(|scheme| scheme.optional_last_param)
    }

    /// The scheme of the function a callee names: a bare name (a local,
    /// a function of the module or one it imports, a builtin) or a
    /// `module.function` path. `None` for any other callee, a function
    /// value.
    fn callee_scheme(&self, callee: &Expr, env: &TypeEnv) -> Option<Scheme> {
        match &callee.kind {
            ExprKind::Ident(name) => {
                if callee.res == Some(crate::defs::Res::Error) {
                    return None;
                }
                self.def_scheme(callee.res, env)
                    .or_else(|| env.lookup(*name).cloned())
            }
            ExprKind::FieldAccess(..) if self.callee_module_is_in_scope(callee, env) => {
                self.def_scheme(callee.res, env)
            }
            _ => None,
        }
    }

    // ── Expression type inference ───────────────────────────────────

    pub(super) fn infer_expr(&mut self, expr: &mut Expr, env: &mut TypeEnv) -> Type {
        if !matches!(expr.kind, ExprKind::FieldAccess(..)) {
            return self.infer_expr_kind(expr, env);
        }
        // A method call names its method's trait: the access records it
        // (`Expr::res`), and the compiler keys the call by it.
        let outer = self.method_trait.take();
        // A trait the access names already (a derived impl's body) is
        // taken off it while it is inferred, and written back below.
        let forced = match expr.res {
            Some(crate::defs::Res::Def(id)) => self.trait_key(id),
            _ => None,
        };
        if forced.is_some() {
            expr.res = None;
        }
        let outer_forced = std::mem::replace(&mut self.forced_trait, forced);
        let ty = self.infer_expr_kind(expr, env);
        self.forced_trait = outer_forced;
        if let Some(t) = self.method_trait.take() {
            expr.res = Some(crate::defs::Res::Def(t.id.0));
        }
        self.method_trait = outer;
        ty
    }

    fn infer_expr_kind(&mut self, expr: &mut Expr, env: &mut TypeEnv) -> Type {
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
                                .tables
                                .trait_impl_set
                                .contains(&(TraitKey::builtin("Display"), type_name))
                        {
                            self.error(Code::MissingTraitImpl,
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
                            let (first_shown, elem_shown) =
                                self.show_apart(&first_resolved, &elem_ty);
                            self.error(Code::TypeMismatch,
                                format!(
                                    "list elements must have the same type: first element is {}, but element {} is {}",
                                    first_shown,
                                    idx + 1,
                                    elem_shown
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
                    // GAP (round 93): the unify calls here used to pass the
                    // established type as t1 and the deviant entry as t2,
                    // reversing the documented t1=got/t2=expected convention
                    // (`#{ "a": 1, 2: 3 }` reported "expected Int, got
                    // String"). Fixed direction, and — mirroring the list-
                    // literal arm above — replace the raw unify mismatch
                    // with a curated map-level message.
                    for (idx, (k, v)) in iter.enumerate() {
                        let entry_num = idx + 2; // 1-based; entry 1 set the type
                        let k_span = k.span;
                        let v_span = v.span;
                        let kt = self.infer_expr(k, env);
                        let vt = self.infer_expr(v, env);
                        let err_count = self.errors.len();
                        self.unify(&kt, &first_k, k_span);
                        if self.errors.len() > err_count {
                            self.errors.truncate(err_count);
                            let first_resolved = self.apply(&first_k);
                            let kt_resolved = self.apply(&kt);
                            self.error(Code::TypeMismatch,
                                format!(
                                    "map keys must have the same type: first key is {first_resolved}, but key {entry_num} is {kt_resolved}"
                                ),
                                k_span,
                            );
                        }
                        let err_count = self.errors.len();
                        self.unify(&vt, &first_v, v_span);
                        if self.errors.len() > err_count {
                            self.errors.truncate(err_count);
                            let first_resolved = self.apply(&first_v);
                            let vt_resolved = self.apply(&vt);
                            self.error(Code::TypeMismatch,
                                format!(
                                    "map values must have the same type: first value is {first_resolved}, but value {entry_num} is {vt_resolved}"
                                ),
                                v_span,
                            );
                        }
                    }
                    Type::Map(Box::new(first_k), Box::new(first_v))
                }
            }

            ExprKind::SetLit(elems) => {
                if elems.is_empty() {
                    let tv = self.fresh_var();
                    Type::Set(Box::new(tv))
                } else {
                    // GAP (round 93): same direction fix + curated message
                    // as the map arm above — `#[1, 2, "x"]` used to report
                    // "expected String, got Int". Mirror the list-literal
                    // wording for set elements.
                    let elem_type = self.fresh_var();
                    for (idx, e) in elems.iter_mut().enumerate() {
                        let espan = e.span;
                        let t = self.infer_expr(e, env);
                        let err_count = self.errors.len();
                        self.unify(&t, &elem_type, espan);
                        if self.errors.len() > err_count {
                            self.errors.truncate(err_count);
                            let first_resolved = self.apply(&elem_type);
                            let t_resolved = self.apply(&t);
                            self.error(Code::TypeMismatch,
                                format!(
                                    "set elements must have the same type: first element is {}, but element {} is {}",
                                    first_resolved,
                                    idx + 1,
                                    t_resolved
                                ),
                                espan,
                            );
                        }
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
                if expr.res == Some(crate::defs::Res::Error) {
                    // The resolver reported the name, or it comes from a
                    // module that failed to load.
                    Type::Error
                } else if let Some(scheme) = self
                    .def_scheme(expr.res, env)
                    .or_else(|| env.lookup(name).cloned())
                {
                    self.instantiate(&scheme)
                } else if name == intern("self") {
                    // `self` is resolved at runtime — allow without error
                    self.fresh_var()
                } else if self.res_def(expr.res).is_some_and(|def| {
                    matches!(
                        def.kind,
                        crate::defs::DefKind::TypeAlias | crate::defs::DefKind::Trait(_)
                    )
                }) {
                    // A type alias or a trait used as a value.
                    self.error(
                        Code::UndefinedVariable,
                        format!("'{name}' is not a value"),
                        span,
                    );
                    Type::Error
                } else {
                    let msg = format_undefined_variable_message(name, env, "");
                    self.error_help(Code::UndefinedVariable, msg, span);
                    self.fresh_var()
                }
            }

            ExprKind::FieldAccess(obj, field, _) => {
                self.last_field_access_was_method = false;
                let field = *field;
                // What the resolver said the access names: nothing (it
                // reported why, or the module failed to load), a variant
                // (`m.Red`, `Shape.Red`, `m.Shape.Red`), whose constructor
                // is bound as `Enum.Variant`, or a member of a module
                // (`m.f`, `m.Pt`), bound as `m.f`.
                if expr.res == Some(crate::defs::Res::Error) {
                    expr.ty = Some(Type::Error);
                    return Type::Error;
                }
                let names_member = self.res_variant_enum(expr.res).is_some()
                    || matches!(obj.res, Some(crate::defs::Res::Module(_)));
                if names_member {
                    let ty = match self.def_scheme(expr.res, env) {
                        Some(scheme) => {
                            let ty = self.instantiate(&scheme);
                            self.apply(&ty)
                        }
                        // A member the resolver found that has no
                        // scheme: a type alias or a trait used as a
                        // value.
                        None => {
                            let module = match &obj.kind {
                                ExprKind::Ident(m) => format!("{m}."),
                                _ => String::new(),
                            };
                            self.error(
                                Code::UndefinedVariable,
                                format!("'{module}{field}' is not a value"),
                                span,
                            );
                            Type::Error
                        }
                    };
                    expr.ty = Some(ty.clone());
                    return ty;
                }
                // `Type.method`: a method of a type, called through it
                // (`Shape.describe`, `m.Shape.describe`, `Int.display`).
                let type_ref = match (&obj.kind, obj.res) {
                    (ExprKind::Ident(name), None) => self.named_type(None, *name),
                    (ExprKind::Ident(_) | ExprKind::FieldAccess(..), res) => self.res_type(res),
                    _ => None,
                };
                if let Some(ty) = type_ref {
                    let key = (canonical_head(&self.tables.resolver, ty), field);
                    if let Some(entry) = self.tables.method_table.get(&key).cloned() {
                        if let Some(trait_name) = entry.trait_name
                            && self.private_owner(trait_name).is_some()
                        {
                            self.private_method(trait_name, field, span);
                            expr.ty = Some(Type::Error);
                            return Type::Error;
                        }
                        if self.ambiguous_method_call(key.0, field, span) {
                            expr.ty = Some(Type::Error);
                            return Type::Error;
                        }
                        self.method_trait = self.entry_trait(&entry, field);
                        let scheme = Self::method_scheme(&entry);
                        let ty = self.instantiate(&scheme);
                        let ty = self.apply(&ty);
                        expr.ty = Some(ty.clone());
                        return ty;
                    }
                    // A type with no such method, whose name is no value
                    // either (`Option.compare`, `time.Weekday.nope`): the
                    // method is what is missing.
                    if matches!(obj.kind, ExprKind::FieldAccess(..))
                        || self.def_scheme(obj.res, env).is_none()
                    {
                        self.no_type_method(&resolve(ty.name), field, span);
                        expr.ty = Some(Type::Error);
                        return Type::Error;
                    }
                }

                // Could be record.field — infer the object type
                let obj_ty = self.infer_expr(obj, env);
                let obj_ty = self.apply(&obj_ty);
                // Phase B: canonicalise before dispatch so a Range
                // receiver (from `1..n`) lands in the List arm. The
                // dedicated `Type::List(_) | Type::Range(_)` arm
                // below is therefore reduced to `Type::List(_)`; no
                // separate Range redirect to the List method table
                // is needed because the Range form has been collapsed
                // away upstream of this match.
                let obj_ty = crate::types::canonical::canonicalize(&self.tables.resolver, &obj_ty);

                // Field / method access
                //
                // `TypeOf(inner)` values (descriptors produced by `type a`
                // parameters or bare type names like `Int`) dispatch to
                // trait methods on `inner`'s type. The descriptor is a
                // type carrier, not a value argument — the downstream
                // Call arm therefore sees `is_method_call = false`, so
                // arg unification runs with no offset and the fn body's
                // parameter slots line up one-for-one with the user's
                // explicit arguments.
                if let Type::Generic(gname, gargs) = &obj_ty
                    && gname.is_builtin(crate::defs::TYPE_OF)
                    && gargs.len() == 1
                {
                    let resolved = self.resolve_type_descriptor_method(&gargs[0], field, span);
                    if let Some(ty) = resolved {
                        self.last_field_access_was_method = false;
                        expr.ty = Some(ty.clone());
                        return ty;
                    }
                    // A concrete type with no such method (a type
                    // variable's case is reported above).
                    let inner = self.apply(&gargs[0]);
                    if !matches!(inner, Type::Var(_) | Type::Error) {
                        self.no_type_method(&format!("{inner}"), field, span);
                    }
                    return Type::Error;
                }
                match &obj_ty {
                    Type::AnonRecord { fields, tail } => {
                        if let Some(ft) = fields.get(&field) {
                            ft.clone()
                        } else if let RowTail::Var(_) = tail {
                            // Open row: extend the row variable with this
                            // new field. Bind the existing row var to
                            // `{field: fresh, ...new_row}` and surface the
                            // fresh field type.
                            use std::collections::BTreeMap;
                            let result_ty = self.fresh_var();
                            let new_row = self.fresh_tyvar_id();
                            let mut new_fields = BTreeMap::new();
                            new_fields.insert(field, result_ty.clone());
                            let extended = Type::AnonRecord {
                                fields: new_fields,
                                tail: RowTail::Var(new_row),
                            };
                            self.unify(&obj_ty, &extended, span);
                            result_ty
                        } else {
                            // ERR-GAP (round 81 F2): mirror the sibling
                            // Record / Generic field-access sites and
                            // append a did-you-mean hint.
                            let candidates: Vec<(Symbol, Type)> =
                                fields.iter().map(|(k, v)| (*k, v.clone())).collect();
                            let base = format!("anon record has no field '{field}'");
                            self.error_help(
                                Code::NoSuchField,
                                format_record_field_suggestion(base, field, &candidates),
                                span,
                            );
                            Type::Error
                        }
                    }
                    Type::Record(rec_name, fields) => {
                        // Direct field access first
                        if let Some((_, ft)) = fields.iter().find(|(n, _)| *n == field) {
                            ft.clone()
                        } else if let Some(entry) =
                            self.tables.method_table.get(&(*rec_name, field)).cloned()
                        {
                            let instantiated =
                                self.dispatch_method_entry(&entry, field, &obj_ty, span);
                            let resolved = self.apply(&instantiated);
                            expr.ty = Some(resolved.clone());
                            return resolved;
                        } else if let Some(msg) =
                            self.method_auto_derive_violation(*rec_name, field)
                        {
                            // Round 93: the field-aware auto-derive gate
                            // removed this type's provisional `.equal()` /
                            // `.compare()` / `.hash()` entry — name the
                            // offending field instead of a generic
                            // "no field or method".
                            self.error(Code::NotDerivable, msg, span);
                            Type::Error
                        } else {
                            // GAP (round 26 L5): append a did-you-mean
                            // hint when a near edit-distance field
                            // exists on this record.
                            let base =
                                format!("record {rec_name} has no field or method '{field}'");
                            self.error_help(
                                Code::NoSuchField,
                                format_record_field_suggestion(base, field, fields),
                                span,
                            );
                            Type::Error
                        }
                    }
                    Type::Generic(type_name, type_args) => {
                        // Check record field definitions, substituting type parameters
                        if let Some(rec_info) = self.tables.records.get(type_name).cloned()
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
                                self.tables.record_param_var_ids.get(type_name).cloned()
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
                        if let Some(entry) =
                            self.tables.method_table.get(&(*type_name, field)).cloned()
                        {
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
                        // Round 93: the field-aware auto-derive gate
                        // removed this type's provisional `.equal()` /
                        // `.compare()` / `.hash()` entry — name the
                        // offending field instead of a generic
                        // "unknown method".
                        if let Some(msg) = self.method_auto_derive_violation(*type_name, field) {
                            self.error(Code::NotDerivable, msg, span);
                            expr.ty = Some(Type::Error);
                            return Type::Error;
                        }
                        // GAP (round 35 F7): thread did-you-mean suggestion
                        // through the Generic/named-record field-access
                        // path so `u.nam` on `type User { name, age }`
                        // prints `did you mean 'name'?`.
                        let shown = self.show_type(&Type::Generic(*type_name, vec![]));
                        let base = format!("unknown field or method '{field}' on type {shown}");
                        let msg = if let Some(rec_info) = self.tables.records.get(type_name) {
                            format_record_field_suggestion(base, field, &rec_info.fields)
                        } else {
                            (base, None)
                        };
                        self.error_help(Code::UnknownField, msg, span);
                        Type::Error
                    }
                    // Primitive types — check method table for trait methods.
                    // Channel and Fn are not
                    // auto-derived but user-defined trait impls register entries
                    // under the canonical names "Channel" / "Fn" via
                    // `type_name_for_impl` (see `src/typechecker/mod.rs:2081`,
                    // `src/typechecker/mod.rs:2092`), so dispatch must route those
                    // receivers through the same `method_table` lookup. The
                    // `"Fn"` key matches `canonical_name(Type::Fun)`,
                    // `head_of_canon`, and `dispatch_type_for_value`
                    // — round 71 follow-up unified all four sites on `"Fn"`.
                    Type::Int
                    | Type::Float
                    | Type::Bool
                    | Type::String
                    | Type::Unit
                    | Type::Channel(_)
                    | Type::Fun(_, _) => {
                        // `Unit` is the key of `()` (it matches
                        // canonical_name(Type::Unit) and
                        // dispatch_type_for_value(Value::Unit)), `Fn` of a
                        // function.
                        let type_name = head_of(&obj_ty).expect("a primitive head has a type");
                        if let Some(entry) =
                            self.tables.method_table.get(&(type_name, field)).cloned()
                        {
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
                        // A method of an impl the resolver rejected: reported there.
                        if self.unresolved_impl_methods.contains(&field) {
                            return Type::Error;
                        }
                        let display = format!("type {type_name}");
                        self.error_help(
                            Code::UnknownMethod,
                            format_unknown_method_message(
                                field,
                                &display,
                                &self.tables.method_table,
                                type_name,
                            ),
                            span,
                        );
                        Type::Error
                    }
                    // Collection types. Phase B: Range receivers were
                    // canonicalised to List upstream of this match, so
                    // the dispatch-time redirect (formerly
                    // `Type::List(_) | Type::Range(_)`) is reduced to a
                    // single List arm. Range no longer reaches here.
                    //
                    // Round 77 BLOAT D3: the four container arms (List,
                    // Tuple, Map, Set) are line-for-line identical
                    // except for the literal type-name string. They
                    // collapse onto a single arm that asks
                    // `type_name_for_impl` for the canonical name.
                    // Adding a fifth container head only requires
                    // extending the match pattern below — not cloning
                    // a fourth body. The unknown-method error wording
                    // matches the pre-fix form (`unknown method 'X' on
                    // List` — without the `type` prefix used by the
                    // primitive arm above), preserving observable
                    // behavior across all four heads.
                    t @ (Type::List(_) | Type::Tuple(_) | Type::Map(_, _) | Type::Set(_)) => {
                        let type_name = self
                            .type_name_for_impl(t)
                            .expect("container head has canonical name");
                        if let Some(entry) =
                            self.tables.method_table.get(&(type_name, field)).cloned()
                        {
                            let instantiated =
                                self.dispatch_method_entry(&entry, field, &obj_ty, span);
                            let resolved = self.apply(&instantiated);
                            expr.ty = Some(resolved.clone());
                            return resolved;
                        }
                        // GAP (round 92): `t.0` parses as a FieldAccess
                        // with an all-numeric "method" name (the parser
                        // keeps a dedicated tuple-index arm), but tuple
                        // indexing is intentionally unsupported (round 69
                        // decision — docs-only). The generic "unknown
                        // method '0' on Tuple" wording misled users into
                        // thinking they typo'd a method name. Emit a
                        // targeted diagnostic pointing at destructuring
                        // instead. Numeric "methods" can only arise from
                        // tuple-index syntax, so this never shadows a real
                        // method lookup.
                        if matches!(t, Type::Tuple(_)) {
                            let field_str = resolve(field);
                            if !field_str.is_empty()
                                && field_str.bytes().all(|b| b.is_ascii_digit())
                            {
                                self.error(
                                    Code::UnsupportedOperation,
                                    format!(
                                        "tuple indexing ('t.{field_str}') is not supported; \
                                         destructure instead: 'let (a, b) = t'"
                                    ),
                                    span,
                                );
                                return Type::Error;
                            }
                        }
                        // A method of an impl the resolver rejected: reported there.
                        if self.unresolved_impl_methods.contains(&field) {
                            return Type::Error;
                        }
                        let display = resolve(type_name.name).to_string();
                        self.error_help(
                            Code::UnknownMethod,
                            format_unknown_method_message(
                                field,
                                &display,
                                &self.tables.method_table,
                                type_name,
                            ),
                            span,
                        );
                        Type::Error
                    }
                    Type::Var(v) => {
                        // Check if this type variable has trait constraints
                        if let Some(trait_names) = self.active_constraints.get(v).cloned() {
                            // Collect all traits that provide this method
                            let mut matches: Vec<(TraitKey, Type)> = Vec::new();
                            for trait_name in &trait_names {
                                if let Some(trait_info) =
                                    self.tables.traits.get(trait_name).cloned()
                                    && let Some((_, method_ty)) =
                                        trait_info.methods.iter().find(|(n, _)| *n == field)
                                {
                                    matches.push((*trait_name, method_ty.clone()));
                                }
                            }
                            if let Some(t) = self.forced_trait {
                                matches.retain(|(n, _)| *n == t);
                            }
                            if matches.len() > 1 {
                                let trait_list = matches
                                    .iter()
                                    .map(|(name, _)| self.show_trait(*name))
                                    .collect::<Vec<_>>()
                                    .join(", ");
                                self.error(Code::AmbiguousMethod,
                                    format!(
                                        "ambiguous method '{field}': provided by multiple traits ({trait_list})"
                                    ),
                                    span,
                                );
                                Type::Error
                            } else if let Some((trait_name, method_ty)) = matches.first() {
                                self.last_field_access_was_method = true;
                                self.method_trait = Some(*trait_name);
                                // Instantiate with fresh TyVars rather than
                                // returning the trait declaration's template
                                // type directly. TraitInfo.methods stores
                                // bare Type values whose TyVars were allocated
                                // once at register_trait_decl time and shared
                                // across all call sites. Without instantiation,
                                // unification at the downstream Call arm binds
                                // those shared template TyVars in self.tables.vars.subst,
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
                                self.error(Code::UnknownMethod,
                                    format!(
                                        "no method '{field}' found in trait constraints ({traits_str})"
                                    ),
                                    span,
                                );
                                Type::Error
                            }
                        } else {
                            // B3 (row polymorphism): unconstrained type
                            // variable + field access — if the field name
                            // is not a known method (registered impl OR
                            // declared on any trait), generate an open
                            // anon-record constraint so
                            // `fn first_name(p) { p.name }` infers
                            // `p: {name: a, ...r} -> a`. When the field
                            // is a method name, fall back to the legacy
                            // deferred-check path so trait dispatch keeps
                            // working unchanged.
                            // A method only another module's private trait
                            // provides cannot be called here, whatever the
                            // receiver turns out to be.
                            if let Some(trait_name) = self.only_private_provider(field) {
                                self.private_method(trait_name, field, span);
                                expr.ty = Some(Type::Error);
                                return Type::Error;
                            }
                            // A call that stays polymorphic names the one
                            // trait the module sees with a method of the
                            // name, when there is one: the VM looks the
                            // method up in that trait's impls.
                            let mut seen = self
                                .tables
                                .traits
                                .iter()
                                .filter(|(_, info)| info.methods.iter().any(|(n, _)| *n == field))
                                .map(|(t, _)| *t)
                                .filter(|t| self.sees_trait(*t));
                            if let (Some(t), None) = (seen.next(), seen.next()) {
                                self.method_trait = Some(t);
                            }
                            let result_ty = self.fresh_var();
                            let is_known_impl_method =
                                self.tables.method_table.keys().any(|(_, m)| *m == field);
                            let is_declared_trait_method = self
                                .tables
                                .traits
                                .values()
                                .any(|info| info.methods.iter().any(|(n, _)| *n == field));
                            if !is_known_impl_method && !is_declared_trait_method {
                                let row_var = self.fresh_tyvar_id();
                                use std::collections::BTreeMap;
                                let mut fmap = BTreeMap::new();
                                fmap.insert(field, result_ty.clone());
                                let row_ty = Type::AnonRecord {
                                    fields: fmap,
                                    tail: RowTail::Var(row_var),
                                };
                                self.unify(&obj_ty, &row_ty, span);
                            }
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
                            Code::UnknownField,
                            format!(
                                "unknown field or method '{field}' on type {}",
                                self.show_type(&obj_ty)
                            ),
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
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Mod => {
                        let op_str = match op {
                            BinOp::Add => "'+'",
                            BinOp::Sub => "'-'",
                            BinOp::Mul => "'*'",
                            BinOp::Mod => "'%'",
                            _ => unreachable!(),
                        };
                        let resolved_l = self.apply(&lt);
                        let resolved_r = self.apply(&rt);
                        match (&resolved_l, &resolved_r) {
                            // An operand already in error (e.g. the left
                            // side of `a + "b" + "c"`) was reported once;
                            // stay quiet. `Type::Error` keeps an ascribed
                            // let from re-reporting the result.
                            (Type::Error, Type::String) | (Type::String, Type::Error) => {
                                Type::Error
                            }
                            (Type::String, _) | (_, Type::String) => {
                                self.error(
                                    Code::UnsupportedOperation,
                                    arith_operand_message(op_str, &Type::String),
                                    span,
                                );
                                Type::Error
                            }
                            _ => {
                                // Round 100: `unify_binop_operands` emits at
                                // most ONE correctly-directed diagnostic —
                                // the left operand establishes the
                                // expectation, and a lone out-of-domain
                                // operand gets the operator-domain message
                                // instead of a misdirected mismatch (see its
                                // doc comment). F1 (round 67): the
                                // operand-domain check below is skipped when
                                // it errored (the second domain message
                                // would be noise). Also return `Type::Error`
                                // on unify failure so an outer ascribed-let
                                // (`let n: Int = s - 1`) hits the
                                // cascade-suppression branch in `unify`
                                // (`mod.rs:1387`) and doesn't re-emit
                                // (G2, round 60).
                                let unify_errored = self.unify_binop_operands(
                                    &lt,
                                    &rt,
                                    lhs_span,
                                    rhs_span,
                                    is_valid_arith_operand,
                                    |t| arith_operand_message(op_str, t),
                                );
                                if !unify_errored {
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
                                        _ if !is_valid_arith_operand(&resolved) => {
                                            self.error(
                                                Code::UnsupportedOperation,
                                                arith_operand_message(op_str, &resolved),
                                                span,
                                            );
                                        }
                                        _ => {}
                                    }
                                }
                                if unify_errored { Type::Error } else { lt }
                            }
                        }
                    }
                    BinOp::Div => {
                        // Round 100 (+ F1 round 72): mirror the
                        // `+`/`-`/`*`/`%` arm — `unify_binop_operands`
                        // emits at most one correctly-directed
                        // diagnostic, so the operand-domain check
                        // below is skipped when it errored (the
                        // second message would be redundant noise).
                        // Also return `Type::Error` on unify
                        // failure so an outer ascribed-let
                        // (`let n: Int = b / 1`) hits the
                        // cascade-suppression branch in `unify`
                        // (`mod.rs:1387`).
                        let unify_errored = self.unify_binop_operands(
                            &lt,
                            &rt,
                            lhs_span,
                            rhs_span,
                            is_valid_arith_operand,
                            |t| arith_operand_message("'/'", t),
                        );
                        if !unify_errored {
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
                                _ if !is_valid_arith_operand(&resolved) => {
                                    self.error(
                                        Code::UnsupportedOperation,
                                        arith_operand_message("'/'", &resolved),
                                        span,
                                    );
                                }
                                _ => {}
                            }
                        }
                        if unify_errored { Type::Error } else { lt }
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
                        // Round 100 (+ F1 round 72): `unify_binop_operands`
                        // emits at most one correctly-directed diagnostic,
                        // so the operand-domain check below is skipped when
                        // it errored (the second message would be redundant
                        // noise — mirrors the Add/Sub/Div arms). For Eq/Neq
                        // this is defensive: today the domain check passes
                        // for most cases (e.g. Bool is a valid equality
                        // operand) so the dual diagnostic doesn't surface,
                        // but apply uniformly to close the latent door.
                        let unify_errored = self.unify_binop_operands(
                            &lt,
                            &rt,
                            lhs_span,
                            rhs_span,
                            |t| is_valid_compare_operand(t, is_equality),
                            |t| {
                                let domain = if is_equality {
                                    "a comparable type"
                                } else {
                                    "Int, Float, String, List, Range, Record, or Variant"
                                };
                                format!("operator {op_str} requires {domain}, got '{t}'")
                            },
                        );
                        if !unify_errored {
                            // B3: enforce comparison operand domain. The VM's
                            // compare() (src/vm/arithmetic.rs) only supports
                            // Int/Float/String/List/Range/Record/Variant
                            // for ordering. Equality additionally supports
                            // Tuple/Map/Set/Bool/Unit/Channel and closed-row
                            // AnonRecord via Value's PartialEq.
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
                                        "Int, Float, String, List, Range, Record, or Variant"
                                    };
                                    self.error(
                                        Code::UnsupportedOperation,
                                        format!(
                                            "operator {op_str} requires {domain}, got '{resolved}'"
                                        ),
                                        span,
                                    );
                                }
                                _ => {
                                    // Round 93: nominal record / enum operands
                                    // pass the shape check above, but the
                                    // field-aware auto-derive gate may have
                                    // proven the type cannot support the
                                    // Value-level operation (e.g. a record
                                    // wrapping a `Fn(..)` field — closure
                                    // ordering is Arc-pointer-address
                                    // nondeterministic). Reject statically
                                    // with the precise reason.
                                    if let Some(msg) =
                                        self.operand_builtin_trait_violation(&resolved, is_equality)
                                    {
                                        self.error(Code::NotDerivable, msg, span);
                                    }
                                }
                            }
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
                            Type::Int | Type::Float => {}
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
                                    Code::UnsupportedOperation,
                                    format!("unary '-' requires Int or Float, got '{}'", resolved),
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
                        // Same signature fact the Call arm reads: may the
                        // call leave out the callee's last argument?
                        let optional_last_param =
                            self.callee_declares_optional_last_param(callee, env);

                        // If callee is a named function, use instantiate_with_constraints
                        let (callee_ty, where_constraints) = if callee_fn_name.is_some() {
                            if let Some(scheme) = self.callee_scheme(callee, env) {
                                let (ty, constraints) = self.instantiate_with_constraints(&scheme);
                                let applied = self.apply(&ty);
                                // Round-101 GAP fix: same stash as the
                                // `ExprKind::Call` arm's named-callee
                                // shortcut — without it, LSP hover on the
                                // callee of a piped call (`add` in
                                // `1 |> add(2)`) has no type and falls
                                // back to an enclosing expression's type.
                                callee.ty = Some(applied.clone());
                                (applied, constraints)
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
                                // Arity check — the piped value counts as
                                // the first argument; the rule is the one
                                // the Call arm applies.
                                if !call_arity_matches(
                                    params.len(),
                                    optional_last_param,
                                    all_arg_types.len(),
                                ) {
                                    self.error(
                                        Code::ArityMismatch,
                                        format!(
                                            "function expects {}, got {}",
                                            accepted_arity_text(params.len(), optional_last_param),
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
                            let bound_args = self
                                .trait_arg_bindings
                                .get(&(*tyvar, *trait_name))
                                .cloned()
                                .unwrap_or_default();
                            if self.type_name_for_impl(&resolved).is_some() {
                                // Recursively walk the matched impl's where
                                // clauses against the resolved type's args.
                                self.verify_trait_obligation(
                                    *trait_name,
                                    &bound_args,
                                    &resolved,
                                    span,
                                );
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
                                        bound_trait_args: bound_args,
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
                    let optional_last_param = self.callee_declares_optional_last_param(rhs, env);
                    let fn_type = self.infer_expr(rhs, env);
                    let fn_type = self.apply(&fn_type);

                    match &fn_type {
                        Type::Fun(params, ret) => {
                            // B6: `a |> f` is the call `f(a)`: it supplies
                            // one argument, under the arity rule of every
                            // call. Piping into a function that needs more
                            // without an explicit call forgets the
                            // remaining args.
                            if !call_arity_matches(params.len(), optional_last_param, 1) {
                                let n = params.len();
                                self.error(Code::ArityMismatch,
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
                        Type::Error => Type::Error,
                        _ => {
                            self.error(
                                Code::TypeMismatch,
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
                // Range(T) is a nominal alias over List(T): unifies
                // bidirectionally with List(T) (see unify in
                // src/typechecker/mod.rs), runtime rep is still Vec<Value>.
                // The nominal distinction lets `let r: Range(Int) = 1..10`
                // typecheck cleanly without silently widening annotations.
                Type::Range(Box::new(Type::Int))
            }

            ExprKind::QuestionMark(inner) => {
                let inner_ty = self.infer_expr(inner, env);
                let inner_ty = self.apply(&inner_ty);

                // Round 93: remember the `?` site so a later body/return
                // unify failure can point back here (see
                // `note_qmark_requirement_on_ret_mismatch`).
                self.current_qmark_spans.push(span);

                // ? operator on Result(a,e) returns a, propagates Err(e)
                // ? operator on Option(a) returns a, propagates None
                match &inner_ty {
                    Type::Generic(name, args) if name.is_builtin("Result") && args.len() == 2 => {
                        if let Some(expected_ret) = self.current_return_type.clone() {
                            let err_ty = args[1].clone();
                            let fresh_ok = self.fresh_var();
                            let expected_result = Type::builtin("Result", vec![fresh_ok, err_ty]);
                            self.unify(&expected_ret, &expected_result, span);
                        } else {
                            self.error(Code::InvalidQuestion,
                                "? operator can only be used inside a function that returns Result or Option".to_string(),
                                span,
                            );
                        }
                        args[0].clone()
                    }
                    Type::Generic(name, args) if name.is_builtin("Option") && args.len() == 1 => {
                        if let Some(expected_ret) = self.current_return_type.clone() {
                            let fresh_inner = self.fresh_var();
                            let expected_option = Type::option(fresh_inner);
                            self.unify(&expected_ret, &expected_option, span);
                        } else {
                            self.error(Code::InvalidQuestion,
                                "? operator can only be used inside a function that returns Result or Option".to_string(),
                                span,
                            );
                        }
                        args[0].clone()
                    }
                    // An operand already reported (or from a module that
                    // failed to load): nothing more to say.
                    Type::Error => Type::Error,
                    Type::Var(_) => {
                        // BROKEN (round 93): this arm used to stay lenient
                        // and DROP the obligation entirely, which made
                        // `{ x -> x? + 1 }` piped through list.map
                        // typecheck while the VM propagated the Err value
                        // into a List(Int) — a statically-clean program
                        // crashing at runtime. Defer the check instead:
                        // once all bodies are inferred, the inner type has
                        // either resolved (validate exactly like the
                        // concrete arms above, constraining the enclosing
                        // fn/lambda return type) or is genuinely
                        // polymorphic (stay lenient — same rationale as
                        // `pending_numeric_checks`). See
                        // `finalize_deferred_checks`.
                        let result_ty = self.fresh_var();
                        self.pending_question_marks.push((
                            inner_ty.clone(),
                            result_ty.clone(),
                            self.current_return_type.clone(),
                            span,
                        ));
                        result_ty
                    }
                    _ => {
                        self.error(
                            Code::InvalidQuestion,
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
                // B2: annotation arity errors should carry the annotation's
                // own span, not a zero-span sentinel.
                let prev_type_span = self.current_type_anno_span.replace(type_expr.span);
                let declared = self.resolve_type_expr(type_expr, &mut HashMap::new());
                self.current_type_anno_span = prev_type_span;
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
                // Whether the named callee's signature lets the call leave
                // out the last argument. Read before the callee is
                // inferred, which needs the callee mutably.
                let callee_optional_last_param =
                    self.callee_declares_optional_last_param(callee, env);
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
                // Round 64 item 6A: extract qualified module-call name
                // (`mod.fn`) so the where-clause-aware lookup below
                // also fires for cross-module calls. Without this, the
                // FieldAccess arm's `instantiate` call discards the
                // imported fn's `where` constraints, so the obligation
                // never reaches `verify_trait_obligation` at the call
                // site.
                let (callee_ty, where_constraints) = if callee_fn_name.is_some() {
                    if let Some(scheme) = self.callee_scheme(callee, env) {
                        let (ty, constraints) = self.instantiate_with_constraints(&scheme);
                        let applied = self.apply(&ty);
                        // Round-101 GAP fix: this named-callee shortcut
                        // bypasses `infer_expr` on the callee Ident, so
                        // (unlike every other expression) it carried no
                        // stashed `expr.ty`. LSP hover on the callee then
                        // fell back to the enclosing Call's RESULT type
                        // (`add` in `add(1, 2)` hovered as `Int`, `println`
                        // as `()`), inconsistent with qualified callees
                        // like `list.sum`. Mirror the qualified-call branch
                        // below: stash the instantiated fn type on the
                        // callee; `resolve_all_types` resolves it to the
                        // call-site instantiation after inference.
                        callee.ty = Some(applied.clone());
                        (applied, constraints)
                    } else {
                        let ty = self.infer_expr(callee, env);
                        (self.apply(&ty), vec![])
                    }
                } else if self.callee_module_is_in_scope(callee, env)
                    && let Some(scheme) = self.callee_scheme(callee, env)
                {
                    let (ty, constraints) = self.instantiate_with_constraints(&scheme);
                    // Mirror the FieldAccess side-effect: pre-set the
                    // callee's expr.ty to the instantiated type so any
                    // downstream consumer (LSP type-at-cursor, etc.)
                    // sees the same type the Call arm consumes here.
                    callee.ty = Some(self.apply(&ty));
                    (self.apply(&ty), constraints)
                } else {
                    let ty = self.infer_expr(callee, env);
                    (self.apply(&ty), vec![])
                };

                // Read the method-dispatch flag BEFORE inferring args
                // (which may trigger nested FieldAccess and overwrite it).
                let is_method_call = self.last_field_access_was_method;

                let arg_types: Vec<Type> =
                    args.iter_mut().map(|a| self.infer_expr(a, env)).collect();

                // Round 64 item 6B: detect a recursive call to the
                // enclosing fn so we can (1) attach a polymorphic-
                // recursion hint if the unify below fails AND the
                // enclosing fn is not fully annotated, and (2) flag
                // the enclosing fn as "recursive" so the narrowing
                // pass in `check_program` knows to lock its scheme
                // when it's also fully annotated.
                let is_recursive_call = match (callee_fn_name, self.current_fn_name) {
                    (Some(c), Some(cur)) => c == cur,
                    _ => false,
                };
                if is_recursive_call && let Some(cur) = self.current_fn_name {
                    self.recursive_fn_names.insert(cur);
                }
                let recursion_hint_span = span;
                let pre_call_error_count = self.errors.len();

                let result_ty = match &callee_ty {
                    Type::Fun(params, ret) => {
                        // Unify argument types with parameter types. For a
                        // method call the implicit `self` is already bound
                        // by `dispatch_method_entry` against the receiver,
                        // so the caller's arguments line up with
                        // `params[1..]` rather than `params[0..]`. Without
                        // this offset, `x.pick(Todo)` unifies Todo's type
                        // against the self slot and produces confusing
                        // diagnostics whenever self's type differs from
                        // the first explicit parameter's type.
                        let param_offset = if is_method_call { 1 } else { 0 };
                        let remaining_params = params.len().saturating_sub(param_offset);
                        let min_len = remaining_params.min(arg_types.len());
                        for i in 0..min_len {
                            self.unify(&arg_types[i], &params[i + param_offset], arg_spans[i]);
                        }
                        // Check arity. A method call (the flag is set by
                        // `dispatch_method_entry`) supplies `self`
                        // implicitly, and a method has no optional
                        // parameter; any other call supplies exactly
                        // its written arguments.
                        let implicit_self = usize::from(is_method_call);
                        let optional_last_param = callee_optional_last_param && !is_method_call;
                        if !call_arity_matches(
                            params.len(),
                            optional_last_param,
                            arg_types.len() + implicit_self,
                        ) {
                            let what = match callee_fn_name {
                                Some(name) => format!("`{name}`"),
                                None => "function".to_string(),
                            };
                            self.error(
                                Code::ArityMismatch,
                                format!(
                                    "{what} expects {}, got {}",
                                    accepted_arity_text(params.len(), optional_last_param),
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
                        // Short-circuit cascades when the callee is an
                        // unresolved tyvar — the mismatch branch above
                        // already turned it into a Fun; if we got here
                        // with something else, report the concrete type.
                        let rendered = match &callee_ty {
                            Type::Var(_) => "an expression of unknown type".to_string(),
                            t => format!("`{t}`"),
                        };
                        self.error(
                            Code::TypeMismatch,
                            format!("{rendered} is not callable"),
                            span,
                        );
                        self.fresh_var()
                    }
                };

                // Check where clause constraints using instantiated TyVars
                for (tyvar, trait_name) in &where_constraints {
                    let resolved = self.apply(&Type::Var(*tyvar));
                    let bound_args = self
                        .trait_arg_bindings
                        .get(&(*tyvar, *trait_name))
                        .cloned()
                        .unwrap_or_default();
                    if self.type_name_for_impl(&resolved).is_some() {
                        // Recursively walk the matched impl's where clauses
                        // against the resolved type's arguments.
                        self.verify_trait_obligation(*trait_name, &bound_args, &resolved, span);
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
                                bound_trait_args: bound_args,
                            });
                        }
                    }
                }

                // Round 64 item 6B: if this Call recursed into the
                // enclosing fn (callee == current_fn_name) and produced
                // a fresh diagnostic, AND the enclosing fn isn't fully
                // annotated, attach a help note pointing the user at
                // the polymorphic-recursion escape hatch. The note is
                // only emitted once per recursive call site, after the
                // mismatch error is in place.
                if is_recursive_call
                    && self.errors.len() > pre_call_error_count
                    && let Some(cur) = self.current_fn_name
                    && !self.fully_annotated_fn_names.contains(&cur)
                {
                    self.errors.push(
                        Diagnostic::warning(
                            Code::PolymorphicRecursion,
                            recursion_hint_span,
                            format!(
                                "'{}' is recursing with arguments of a different type \
                                 than its inferred signature",
                                resolve(cur)
                            ),
                        )
                        .with_help("add explicit type annotations to enable polymorphic recursion"),
                    );
                }

                result_ty
            }

            ExprKind::Lambda { params, body } => {
                let mut local_env = env.child();
                // Soundness: lambda param lists are a single conjunctive
                // scope too — `|a, a| ...` must be rejected the same way
                // `fn f(a, a)` is.
                self.check_fn_params_duplicate_bindings(params);
                let param_types: Vec<Type> = params
                    .iter()
                    .map(|p| {
                        let ty = if let Some(te) = &p.ty {
                            // B2: annotation arity errors carry the
                            // annotation's own span.
                            let prev_type_span = self.current_type_anno_span.replace(te.span);
                            let resolved = self.resolve_type_expr(te, &mut HashMap::new());
                            self.current_type_anno_span = prev_type_span;
                            resolved
                        } else {
                            self.fresh_var()
                        };
                        self.bind_irrefutable_pattern(
                            &p.pattern,
                            &ty,
                            &mut local_env,
                            span,
                            BindingSite::ClosureParam,
                        );
                        ty
                    })
                    .collect();

                // BROKEN (round 93): a lambda is its own `?`/`return`
                // boundary — the VM's Op::QuestionMark pops exactly ONE
                // frame (the lambda's), so `?` inside a lambda must
                // validate against the LAMBDA's return type, not the
                // enclosing named fn's. Establish a fresh return-type
                // context for the body, mirroring check_fn_body_with_name.
                // Without this, `{ x -> x? + 1 }` was checked against the
                // outer fn's return type and a Variant escaped into a
                // List(Int) at runtime.
                let lambda_ret = self.fresh_var();
                let prev_return_type = self.current_return_type.replace(lambda_ret.clone());
                let prev_qmark_spans = std::mem::take(&mut self.current_qmark_spans);

                let body_type = self.infer_expr(body, &mut local_env);
                let ret_unify_err_count = self.errors.len();
                self.unify(&body_type, &lambda_ret, body.span);
                self.retarget_ok_wrap_fixes(ret_unify_err_count, body);
                self.note_qmark_requirement_on_ret_mismatch(ret_unify_err_count, &lambda_ret);

                self.current_return_type = prev_return_type;
                self.current_qmark_spans = prev_qmark_spans;

                // `lambda_ret` rather than `body_type`: identical when the
                // unify above succeeded, and on failure it carries any
                // `?`-imposed Result/Option constraint, which is the type
                // the VM actually returns.
                Type::Fun(param_types, Box::new(lambda_ret))
            }

            ExprKind::RecordCreate { name, fields, .. } => {
                let name = *name;
                // GAP (round 35 F4): duplicate fields in a record literal
                // (e.g. `User { name: "a", name: "b" }`) used to slip past
                // the typechecker because downstream processing went through
                // a HashSet that silently deduped them. Mirror the record
                // type declaration's duplicate-field check: walk once and
                // emit a diagnostic per duplicate.
                {
                    let mut seen: std::collections::HashSet<Symbol> =
                        std::collections::HashSet::new();
                    for (field_name, _) in fields.iter() {
                        if !seen.insert(*field_name) {
                            self.error(
                                Code::DuplicateRecordField,
                                format!(
                                    "duplicate field '{}' in record literal for '{}'",
                                    field_name, name
                                ),
                                span,
                            );
                        }
                    }
                }
                // The type identity is the bare name: `util.Pt { x: 1 }`
                // builds a `Pt`.
                let looked = self.named_record(expr.res, name, span, false);
                if let Some((rec_ty, rec_info, param_ids)) = looked {
                    // For parameterized record types, create fresh type variables
                    // for each type parameter and substitute them into field types.
                    // This prevents different instantiations from sharing the same
                    // template variables (e.g., Box { value: 42 } and Box { value: "hi" }).
                    let instantiated_fields =
                        self.instantiate_record_fields(&rec_info, param_ids.as_deref());

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
                            Code::MissingField,
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
                            self.error_help(
                                Code::UnknownField,
                                format_record_field_suggestion(base, *field_name, &rec_info.fields),
                                span,
                            );
                        }
                    }

                    Type::Record(rec_ty, instantiated_fields)
                } else {
                    // G2: Unknown record type (reported by `named_record`
                    // or the resolver) — this used to silently synthesize
                    // an anonymous record. We still walk the field
                    // expressions so nested errors are reported, but return
                    // Type::Error to prevent downstream cascades.
                    for (_, e) in fields.iter_mut() {
                        let _ = self.infer_expr(e, env);
                    }
                    Type::Error
                }
            }

            ExprKind::RecordUpdate { expr: base, fields } => {
                let base_span = base.span;
                let base_ty = self.infer_expr(base, env);
                let resolved = self.apply(&base_ty);
                // GAP (round 35 F4): duplicate fields in a record-update
                // expression (e.g. `r.{ age: 1, age: 2 }`) used to be
                // silently deduped downstream. Emit a diagnostic per
                // duplicate so the typo is surfaced at compile time.
                {
                    let mut seen: std::collections::HashSet<Symbol> =
                        std::collections::HashSet::new();
                    for (field_name, _) in fields.iter() {
                        if !seen.insert(*field_name) {
                            self.error(
                                Code::DuplicateRecordField,
                                format!("duplicate field '{}' in record update", field_name),
                                span,
                            );
                        }
                    }
                }
                // Three cases:
                //  1. Concrete `Type::Record(name, fields)` — validate directly.
                //  2. `Type::Generic(name, args)` resolving to a declared
                //     record (happens when the base is a param annotated
                //     with a user-defined record type). BROKEN-1.
                //  3. Anything else — compile-time reject. BROKEN-2.
                let mut handled = false;
                // Anon record (row-poly) update: validate each field
                // against the row's known fields when present; for open
                // rows, missing fields are added to the row variable.
                if let Type::AnonRecord {
                    fields: af,
                    tail: at,
                } = &resolved
                {
                    let af = af.clone();
                    let at = at.clone();
                    for (field_name, field_expr) in &mut *fields {
                        let ft = self.infer_expr(field_expr, env);
                        if let Some(declared_ty) = af.get(field_name) {
                            self.unify(&ft, declared_ty, span);
                        } else if let RowTail::Var(_) = at {
                            // Open row: extend by unifying with a
                            // record carrying this new field.
                            use std::collections::BTreeMap;
                            let new_row = self.fresh_tyvar_id();
                            let mut new_fields = BTreeMap::new();
                            new_fields.insert(*field_name, ft.clone());
                            let extended = Type::AnonRecord {
                                fields: new_fields,
                                tail: RowTail::Var(new_row),
                            };
                            self.unify(&resolved, &extended, span);
                        } else {
                            // ERR-GAP (round 81 F3): mirror the sibling
                            // Record / Generic record-update sites and
                            // append a did-you-mean hint when the typo
                            // is close to one of the closed row's fields.
                            let candidates: Vec<(Symbol, Type)> =
                                af.iter().map(|(k, v)| (*k, v.clone())).collect();
                            let base =
                                format!("unknown field '{field_name}' in closed anon record");
                            self.error_help(
                                Code::UnknownField,
                                format_record_field_suggestion(base, *field_name, &candidates),
                                span,
                            );
                        }
                    }
                    handled = true;
                }
                if let Type::Record(rec_name, rec_fields) = &resolved {
                    let declared: std::collections::HashMap<Symbol, Type> =
                        rec_fields.iter().map(|(n, t)| (*n, t.clone())).collect();
                    for (field_name, field_expr) in &mut *fields {
                        let ft = self.infer_expr(field_expr, env);
                        if let Some(declared_ty) = declared.get(field_name) {
                            self.unify(&ft, declared_ty, span);
                        } else {
                            // GAP (round 26 L5): did-you-mean on record-update.
                            let base = format!("unknown field '{field_name}' in {rec_name}");
                            self.error_help(
                                Code::UnknownField,
                                format_record_field_suggestion(base, *field_name, rec_fields),
                                span,
                            );
                        }
                    }
                    handled = true;
                } else if let Type::Generic(type_name, type_args) = &resolved
                    && let Some(rec_info) = self.tables.records.get(type_name).cloned()
                {
                    let instantiated_fields: Vec<(Symbol, Type)> = if let Some(param_var_ids) =
                        self.tables.record_param_var_ids.get(type_name).cloned()
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
                            let base = format!("unknown field '{field_name}' in {type_name}");
                            self.error_help(
                                Code::UnknownField,
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
                        self.tables
                            .records
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
                                self.error(Code::UnknownField,
                                    format!(
                                        "unknown field '{field_name}' — not declared on any record type in scope"
                                    ),
                                    span,
                                );
                            }
                        }
                    }
                    if !matches!(resolved, Type::Error | Type::Var(_) | Type::Never) {
                        self.error(Code::TypeMismatch,
                            format!(
                                "record update requires a record base, but '{resolved}' is not a record type"
                            ),
                            base_span,
                        );
                    }
                }
                base_ty
            }

            ExprKind::AnonRecord { spread, fields } => {
                use std::collections::BTreeMap;
                // Phase F: Extend operator. With a spread, infer
                // base's type, then merge the listed `fields` on top.
                // Reject duplicate field names in the literal portion.
                {
                    let mut seen: std::collections::HashSet<Symbol> =
                        std::collections::HashSet::new();
                    for (fn_name, _) in fields.iter() {
                        if !seen.insert(*fn_name) {
                            self.error(
                                Code::DuplicateRecordField,
                                format!("duplicate field '{}' in anon record literal", fn_name),
                                span,
                            );
                        }
                    }
                }
                // Type each new field expression.
                let new_field_tys: Vec<(Symbol, Type)> = fields
                    .iter_mut()
                    .map(|(n, e)| (*n, self.infer_expr(e, env)))
                    .collect();
                let mut field_map: BTreeMap<Symbol, Type> = BTreeMap::new();
                for (n, t) in &new_field_tys {
                    field_map.insert(*n, t.clone());
                }
                if let Some(base_expr) = spread {
                    let base_ty = self.infer_expr(base_expr, env);
                    let base_ty = self.apply(&base_ty);
                    let base_canon =
                        crate::types::canonical::canonicalize(&self.tables.resolver, &base_ty);
                    // Determine base's known fields and tail.
                    let (base_fields, base_tail): (BTreeMap<Symbol, Type>, RowTail) =
                        match &base_canon {
                            Type::AnonRecord { fields, tail } => (fields.clone(), tail.clone()),
                            Type::Record(_, fs) => {
                                let mut m = BTreeMap::new();
                                for (n, t) in fs {
                                    m.insert(*n, t.clone());
                                }
                                (m, RowTail::Closed)
                            }
                            Type::Generic(name, args) if self.tables.records.contains_key(name) => {
                                let rec_info = self.tables.records.get(name).cloned().unwrap();
                                let inst: Vec<(Symbol, Type)> = if let Some(param_var_ids) =
                                    self.tables.record_param_var_ids.get(name).cloned()
                                {
                                    let mapping: HashMap<TyVar, Type> =
                                        if args.len() == param_var_ids.len() {
                                            param_var_ids
                                                .iter()
                                                .zip(args.iter())
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
                                let mut m = BTreeMap::new();
                                for (n, t) in &inst {
                                    m.insert(*n, t.clone());
                                }
                                (m, RowTail::Closed)
                            }
                            _ => {
                                if !matches!(base_canon, Type::Error | Type::Var(_) | Type::Never) {
                                    self.error(Code::TypeMismatch,
                                        format!(
                                            "spread requires a record base, but '{base_canon}' is not a record type"
                                        ),
                                        base_expr.span,
                                    );
                                }
                                (BTreeMap::new(), RowTail::Closed)
                            }
                        };
                    // Reject extending an existing field (decision: no
                    // override; user must restrict first, which v1 doesn't
                    // support).
                    for (n, _) in &new_field_tys {
                        if base_fields.contains_key(n) {
                            self.error(Code::DuplicateRecordField,
                                format!(
                                    "cannot extend record with existing field '{n}'; v1 row polymorphism does not support override"
                                ),
                                span,
                            );
                        }
                    }
                    // Result fields = base ∪ new (new wins on collision —
                    // already errored, but keep last write semantically
                    // benign for cascade).
                    let mut merged: BTreeMap<Symbol, Type> = base_fields.clone();
                    for (n, t) in &new_field_tys {
                        merged.insert(*n, t.clone());
                    }
                    Type::AnonRecord {
                        fields: merged,
                        tail: base_tail,
                    }
                } else {
                    // Closed anon record literal.
                    Type::AnonRecord {
                        fields: field_map,
                        tail: RowTail::Closed,
                    }
                }
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

                        // GAP (round 93): every arm checks its pattern
                        // against the SAME scrutinee, so a wrong-scrutinee
                        // match (`match 42 { Ok(v) -> ..., Err(e) -> ... }`)
                        // used to print the identical "expected Result(_, _),
                        // got Int" once per arm, plus a non-exhaustive
                        // cascade. Dedup identical (message, span, severity)
                        // diagnostics across the arm loop — genuinely
                        // distinct per-arm errors still each report.
                        let mut seen_arm_diags: std::collections::HashSet<(
                            std::string::String,
                            crate::source::FileId,
                            u32,
                            bool,
                        )> = std::collections::HashSet::new();
                        let mut any_pattern_mismatch = false;
                        // A match diverges when it has arms and every
                        // arm diverges. `unify` leaves `result_ty`
                        // unbound against `Never`, so the arms are
                        // tracked here to type such a match `Never`.
                        let mut every_arm_diverges = !arms.is_empty();
                        for arm in arms.iter_mut() {
                            let mut arm_env = env.child();
                            // Soundness: `match e { (x, x) -> x }` used to
                            // typecheck silently, binding the second `x` on
                            // top of the first. Reject duplicate binders
                            // in the arm pattern before check_pattern walks
                            // it and defines them in `arm_env`.
                            self.check_pattern_duplicate_bindings(&arm.pattern);
                            // A name in the pattern the resolver reported:
                            // what the arm covers is not known.
                            if names_unresolved(&arm.pattern) {
                                any_pattern_mismatch = true;
                            }
                            let pat_err_count = self.errors.len();
                            self.check_pattern(
                                &arm.pattern,
                                &scrutinee_ty,
                                &mut arm_env,
                                scrutinee_span,
                            );
                            if self.errors.len() > pat_err_count {
                                let new_diags: Vec<Diagnostic> =
                                    self.errors.drain(pat_err_count..).collect();
                                for d in new_diags {
                                    if matches!(d.severity, Severity::Error) {
                                        any_pattern_mismatch = true;
                                    }
                                    let key = (
                                        d.message.clone(),
                                        d.span.file,
                                        d.span.start,
                                        matches!(d.severity, Severity::Error),
                                    );
                                    if seen_arm_diags.insert(key) {
                                        // Re-inserting a pre-built Diagnostic drained
                                        // above for dedup — not a new emission site,
                                        // so `self.error(...)` does not apply. This
                                        // is the sanctioned exception counted by the
                                        // round-83 STYLE lock (threshold 1).
                                        self.errors.push(d);
                                    }
                                }
                            }

                            if let Some(ref mut guard) = arm.guard {
                                let guard_span = guard.span;
                                let guard_ty = self.infer_expr(guard, &mut arm_env);
                                self.unify(&guard_ty, &Type::Bool, guard_span);
                            }

                            let body_span = arm.body.span;
                            let arm_ty = self.infer_expr(&mut arm.body, &mut arm_env);
                            every_arm_diverges &= matches!(arm_ty, Type::Never);
                            self.unify(&result_ty, &arm_ty, body_span);
                        }

                        // Check exhaustiveness after pattern checking, so the
                        // scrutinee type is fully resolved through unification.
                        // Skipped when the scrutinee type already failed to
                        // match the arms — exhaustiveness over a broken match
                        // is a cascade, not new information (round 93).
                        // So is a scrutinee of a type already reported.
                        let resolved_scrutinee_ty = self.apply(&scrutinee_ty);
                        if !any_pattern_mismatch && resolved_scrutinee_ty != Type::Error {
                            self.check_exhaustiveness(arms, &resolved_scrutinee_ty, scrutinee_span);
                        }

                        if every_arm_diverges {
                            Type::Never
                        } else {
                            result_ty
                        }
                    }
                    None => {
                        // Guardless match: each arm's guard is a boolean
                        // condition. When no condition holds there is no
                        // value, so the last arm must be the `_` default,
                        // and there is exactly one: arms after a `_` never
                        // run.
                        let defaults: Vec<usize> = arms
                            .iter()
                            .enumerate()
                            .filter(|(_, arm)| arm.guard.is_none())
                            .map(|(i, _)| i)
                            .collect();
                        match defaults.first() {
                            None => self.error(
                                Code::NonExhaustive,
                                "a `match` without a scrutinee must end with a `_ -> ...` arm \
                                 for when no condition is true"
                                    .to_string(),
                                span,
                            ),
                            Some(&first) => {
                                if arms[first + 1..].iter().any(|arm| arm.guard.is_some()) {
                                    self.error(
                                        Code::UnreachablePattern,
                                        "the `_` arm must be last: it always matches, so the \
                                         arms after it never run"
                                            .to_string(),
                                        arms[first].pattern.span,
                                    );
                                }
                                for &i in &defaults[1..] {
                                    self.error(
                                        Code::UnreachablePattern,
                                        "unreachable `_` arm: an earlier `_` arm always matches"
                                            .to_string(),
                                        arms[i].pattern.span,
                                    );
                                }
                            }
                        }
                        let result_ty = self.fresh_var();
                        // Same rule as the scrutinee form: arms that all
                        // diverge make the match diverge.
                        let mut every_arm_diverges = !arms.is_empty();

                        for arm in arms.iter_mut() {
                            let mut arm_env = env.child();

                            if let Some(ref mut guard) = arm.guard {
                                let guard_span = guard.span;
                                let guard_ty = self.infer_expr(guard, &mut arm_env);
                                self.unify(&guard_ty, &Type::Bool, guard_span);
                            }

                            let body_span = arm.body.span;
                            let arm_ty = self.infer_expr(&mut arm.body, &mut arm_env);
                            every_arm_diverges &= matches!(arm_ty, Type::Never);
                            self.unify(&result_ty, &arm_ty, body_span);
                        }

                        if every_arm_diverges {
                            Type::Never
                        } else {
                            result_ty
                        }
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
                for (name, _, value) in bindings.iter_mut() {
                    let ty = self.infer_expr(value, env);
                    binding_types.push(ty.clone());
                    loop_env.define(*name, Scheme::mono(ty));
                }
                self.check_recur_tail_positions(body, RecurPos::Tail);
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
                            Code::ArityMismatch,
                            format!(
                                "loop has {} {}, but `loop(...)` supplies {} {}",
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
                    self.error(
                        Code::InvalidControlFlow,
                        "`loop(...)` can only appear inside a `loop` body — it restarts \
                         the enclosing loop with new binding values"
                            .to_string(),
                        span,
                    );
                }
                // `loop(...)` jumps back to the top of the loop and never
                // produces a value here.
                Type::Never
            }
        };
        let resolved = self.apply(&ty);
        expr.ty = Some(resolved.clone());
        resolved
    }

    /// Reject a `loop(...)` call that is not in tail position of its
    /// loop. `loop(...)` restarts the loop, so nothing may use its result:
    /// it must be the last expression of the loop body, or of a block,
    /// match arm or `when ... else` body that is itself in tail position.
    /// `pos` says where `expr` stands. A nested `loop` starts its own
    /// tail context, checked when that loop is inferred, so only its
    /// binding initialisers are walked here.
    fn check_recur_tail_positions(&mut self, expr: &Expr, pos: RecurPos) {
        let inner = pos.inner();
        match &expr.kind {
            ExprKind::Recur(args) => {
                match pos {
                    RecurPos::Tail => {}
                    RecurPos::NotTail => self.error(
                        Code::InvalidControlFlow,
                        "`loop(...)` must be in tail position: it restarts the loop, so \
                         nothing can use its result — make it the last expression of \
                         the loop body, or of a block, match arm or `when ... else` body \
                         in tail position"
                            .to_string(),
                        expr.span,
                    ),
                    RecurPos::InClosure => self.error(
                        Code::InvalidControlFlow,
                        "`loop(...)` inside a closure is not the loop's tail: the closure \
                         runs when it is called, not as the loop body, so it cannot \
                         restart the loop — return a value from the closure and call \
                         `loop(...)` in the loop body"
                            .to_string(),
                        expr.span,
                    ),
                }
                for arg in args {
                    self.check_recur_tail_positions(arg, inner);
                }
            }
            ExprKind::Loop { bindings, .. } => {
                for (_, _, value) in bindings {
                    self.check_recur_tail_positions(value, inner);
                }
            }
            ExprKind::Block(stmts) => {
                let last = stmts.len().saturating_sub(1);
                for (i, stmt) in stmts.iter().enumerate() {
                    match stmt {
                        Stmt::Let { value, .. } => self.check_recur_tail_positions(value, inner),
                        Stmt::When {
                            expr: value,
                            else_body,
                            ..
                        } => {
                            self.check_recur_tail_positions(value, inner);
                            self.check_recur_tail_positions(else_body, pos);
                        }
                        Stmt::WhenBool {
                            condition,
                            else_body,
                        } => {
                            self.check_recur_tail_positions(condition, inner);
                            self.check_recur_tail_positions(else_body, pos);
                        }
                        Stmt::Expr(e) => {
                            self.check_recur_tail_positions(e, if i == last { pos } else { inner })
                        }
                    }
                }
            }
            ExprKind::Match {
                expr: scrutinee,
                arms,
            } => {
                if let Some(scrutinee) = scrutinee {
                    self.check_recur_tail_positions(scrutinee, inner);
                }
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        self.check_recur_tail_positions(guard, inner);
                    }
                    self.check_recur_tail_positions(&arm.body, pos);
                }
            }
            ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::StringLit(..)
            | ExprKind::Ident(_)
            | ExprKind::Unit
            | ExprKind::Return(None) => {}
            ExprKind::StringInterp(parts) => {
                for part in parts {
                    if let StringPart::Expr(e) = part {
                        self.check_recur_tail_positions(e, inner);
                    }
                }
            }
            ExprKind::List(elems) => {
                for elem in elems {
                    match elem {
                        ListElem::Single(e) | ListElem::Spread(e) => {
                            self.check_recur_tail_positions(e, inner)
                        }
                    }
                }
            }
            ExprKind::Map(entries) => {
                for (k, v) in entries {
                    self.check_recur_tail_positions(k, inner);
                    self.check_recur_tail_positions(v, inner);
                }
            }
            ExprKind::SetLit(elems) | ExprKind::Tuple(elems) => {
                for e in elems {
                    self.check_recur_tail_positions(e, inner);
                }
            }
            ExprKind::FieldAccess(e, _, _)
            | ExprKind::Unary(_, e)
            | ExprKind::QuestionMark(e)
            | ExprKind::Ascription(e, _)
            | ExprKind::Return(Some(e)) => self.check_recur_tail_positions(e, inner),
            ExprKind::Binary(l, _, r) | ExprKind::Pipe(l, r) | ExprKind::Range(l, r) => {
                self.check_recur_tail_positions(l, inner);
                self.check_recur_tail_positions(r, inner);
            }
            ExprKind::Call(callee, args) => {
                self.check_recur_tail_positions(callee, inner);
                for arg in args {
                    self.check_recur_tail_positions(arg, inner);
                }
            }
            ExprKind::Lambda { body, .. } => {
                self.check_recur_tail_positions(body, RecurPos::InClosure)
            }
            ExprKind::RecordCreate { fields, .. } => {
                for (_, e) in fields {
                    self.check_recur_tail_positions(e, inner);
                }
            }
            ExprKind::RecordUpdate { expr: base, fields } => {
                self.check_recur_tail_positions(base, inner);
                for (_, e) in fields {
                    self.check_recur_tail_positions(e, inner);
                }
            }
            ExprKind::AnonRecord { spread, fields } => {
                if let Some(base) = spread {
                    self.check_recur_tail_positions(base, inner);
                }
                for (_, e) in fields {
                    self.check_recur_tail_positions(e, inner);
                }
            }
        }
    }

    // ── Statement type inference ────────────────────────────────────

    fn infer_stmt(&mut self, stmt: &mut Stmt, env: &mut TypeEnv) -> Type {
        match stmt {
            Stmt::Let { pattern, ty, value } => {
                let value_span = value.span;
                let is_value = is_syntactic_value(&value.kind);
                let mut val_ty = self.infer_expr(value, env);

                if let Some(te) = &ty {
                    // B2: populate the arity-error span hint with the
                    // annotation's own span so the duplicate span-less
                    // diagnostic in `let x: Box(Int) = ...` goes away.
                    let prev_type_span = self.current_type_anno_span.replace(te.span);
                    let declared = self.resolve_type_expr(te, &mut HashMap::new());
                    self.current_type_anno_span = prev_type_span;
                    self.unify(&val_ty, &declared, value_span);
                    // A value of unknown type (from a module that failed
                    // to load) takes the declared type: `let y: Int = x`
                    // makes `y` an Int.
                    if matches!(self.apply(&val_ty), Type::Error) {
                        val_ty = declared;
                    }
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
                        // Soundness: reject duplicate binding names within
                        // the let pattern. `let (a, a) = (1, 2)` used to
                        // silently shadow the first `a`.
                        self.check_pattern_duplicate_bindings(pattern);
                        // A `let` has no failure branch: the pattern
                        // must match every value of the bound type.
                        self.bind_irrefutable_pattern(
                            pattern,
                            &val_ty,
                            env,
                            value_span,
                            BindingSite::Let,
                        );
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
                        Code::InvalidControlFlow,
                        "'when let' else body must diverge — end it with 'return', 'panic' or, \
                         inside a loop, 'loop(...)'"
                            .to_string(),
                        else_body.span,
                    );
                }

                // Bind the pattern in the current scope (type narrowing).
                // bind_pattern handles all pattern kinds including constructors
                // (enum lookup, param substitution, recursive sub-pattern binding).
                //
                // Soundness: reject duplicate binders before defining so
                // `when let (a, a) = expr` doesn't silently shadow.
                self.check_pattern_duplicate_bindings(pattern);
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
                        Code::InvalidControlFlow,
                        "'when' else body must diverge — end it with 'return', 'panic' or, \
                         inside a loop, 'loop(...)'"
                            .to_string(),
                        else_body.span,
                    );
                }

                Type::Unit
            }

            Stmt::Expr(expr) => self.infer_expr(expr, env),
        }
    }

    /// LATENT (round 88): walk a supertrait-arg `TypeExpr` and emit a
    /// diagnostic at every `Named(sym)` that refers to a *parametric*
    /// user-defined type (enum, record, or alias with arity > 0) used
    /// without its arguments — e.g. `trait Sub: Super(Box)` where
    /// `type Box(a) { ... }`. `resolve_supertrait_arg` falls through
    /// such names to `Type::Generic(sym, [])`, which then never matches
    /// any downstream impl's actual args, so the obligation silently
    /// goes unsatisfied. With this check the user sees a clean arity
    /// diagnostic at the supertrait declaration's span instead.
    ///
    /// Names that map to a trait param (`info.params`), builtin
    /// primitives, or any name we don't recognise are left to the
    /// existing handling — the latter are diagnosed elsewhere as
    /// "unknown type" at the type-expr resolve site.
    ///
    /// Skips emission when the same message+span is already in
    /// `self.errors` so multiple constrained fn-body passes (one per
    /// fn whose param is bound on the trait) don't pile on duplicates.
    pub(super) fn check_supertrait_arg_parametric_arity(
        &mut self,
        te: &TypeExpr,
        trait_info: &TraitInfo,
    ) {
        match &te.kind {
            TypeExprKind::Named { name: sym, .. } => {
                // Trait params are substituted, not type references.
                if trait_info.params.iter().any(|p| p == sym) {
                    return;
                }
                // Unknown name — leave to other diagnostics.
                let Some(ty) = self.named_type(te.res, *sym) else {
                    return;
                };
                let arity = if let Some(info) = self.tables.enums.get(&ty) {
                    info.params.len()
                } else if let Some(ids) = self.tables.record_param_var_ids.get(&ty) {
                    ids.len()
                } else if let Some(info) = self.tables.resolver.lookup_alias(ty) {
                    info.params.len()
                } else {
                    // Builtins like Int/Float/String are 0-arity by design.
                    return;
                };
                if arity == 0 {
                    return;
                }
                let msg = format!(
                    "type '{}' expects {} {}, got 0 in supertrait bound",
                    resolve(*sym),
                    arity,
                    plural(arity, "type argument", "type arguments"),
                );
                let already = self
                    .errors
                    .iter()
                    .any(|e| e.message == msg && e.span == te.span);
                if !already {
                    self.error(Code::ArityMismatch, msg, te.span);
                }
            }
            TypeExprKind::Generic { args, .. } => {
                for a in args {
                    self.check_supertrait_arg_parametric_arity(a, trait_info);
                }
            }
            TypeExprKind::Tuple(elems) => {
                for e in elems {
                    self.check_supertrait_arg_parametric_arity(e, trait_info);
                }
            }
            TypeExprKind::Function(params, ret) => {
                for p in params {
                    self.check_supertrait_arg_parametric_arity(p, trait_info);
                }
                self.check_supertrait_arg_parametric_arity(ret, trait_info);
            }
            TypeExprKind::SelfType
            | TypeExprKind::AssocProj { .. }
            | TypeExprKind::AnonRecord { .. } => {
                // These either aren't meaningful as supertrait args
                // (handled elsewhere) or carry no bare-name parametric
                // reference at the top level. Nothing to validate here.
            }
        }
    }

    /// Round 100: unify the operand types of a binary operator with a
    /// correctly-DIRECTED diagnostic.
    ///
    /// `unify(t1, t2)` renders "type mismatch: expected {t2}, got {t1}",
    /// and the old `unify(&lt, &rt, span)` call in the binop arms
    /// therefore cast the not-yet-read RIGHT operand as the expectation
    /// whenever the right operand was the offender (`1 + true` said
    /// "expected Bool, got Int"). The left operand is inferred first
    /// and establishes the expectation, so this helper passes it as the
    /// "expected" side and anchors the diagnostic at the right
    /// operand's span.
    ///
    /// Additionally (inverting the round-67 F1 priority): when the
    /// unification fails AND exactly one resolved operand is outside
    /// the operator's domain (`in_domain`), the generic mismatch is
    /// replaced with the operand-domain message (`domain_msg`) aimed at
    /// the offender's span — that message names the true offender
    /// regardless of side, where mismatch wording would misfire for a
    /// left-side offender (`true + 1`). `chain_hint` guidance is
    /// preserved on the replacement (`opt + 1` still explains `?` /
    /// `flat_map`). Exactly one diagnostic is emitted either way,
    /// preserving the round-67 single-diagnostic invariant
    /// (tests/lang/binop_single_diagnostic_round67_tests.rs).
    ///
    /// Returns `true` if a diagnostic was emitted; callers skip their
    /// follow-up operand-domain check and return `Type::Error` so outer
    /// ascriptions hit the cascade-suppression branch in `unify` (the
    /// round-60 G2 contract), exactly as with the old error-count
    /// snapshot.
    fn unify_binop_operands(
        &mut self,
        lt: &Type,
        rt: &Type,
        lhs_span: Span,
        rhs_span: Span,
        in_domain: impl Fn(&Type) -> bool,
        domain_msg: impl Fn(&Type) -> std::string::String,
    ) -> bool {
        let err_count_before = self.errors.len();
        self.unify(rt, lt, rhs_span);
        if self.errors.len() == err_count_before {
            return false;
        }
        // The domain predicates treat `Var` / `AssocProj` / `Error` as
        // "maybe valid", so the replacement below only fires when the
        // offender is a RESOLVED out-of-domain type.
        let resolved_l = self.apply(lt);
        let resolved_r = self.apply(rt);
        let l_bad = !in_domain(&resolved_l);
        let r_bad = !in_domain(&resolved_r);
        if l_bad != r_bad {
            let (offender, other, offender_span) = if l_bad {
                (&resolved_l, &resolved_r, lhs_span)
            } else {
                (&resolved_r, &resolved_l, rhs_span)
            };
            let mut d = Diagnostic::error(
                Code::UnsupportedOperation,
                offender_span,
                domain_msg(offender),
            );
            d.help.extend(Self::chain_hint(offender, other));
            self.errors.truncate(err_count_before);
            self.errors.push(d);
        }
        true
    }
}

/// The type a type expression written in a trait declaration names: its
/// resolution (a type's id is its definition's), else the builtin type
/// of that name.
fn written_type(te: &TypeExpr, name: Symbol) -> Option<TypeRef> {
    match te.res {
        Some(crate::defs::Res::Def(id)) => Some(TypeRef {
            id: crate::defs::TypeId(id),
            name,
        }),
        Some(_) => None,
        None => {
            let name_str = resolve(name);
            let name_str = if name_str == "()" {
                "Unit"
            } else {
                name_str.as_str()
            };
            crate::defs::builtin_type_id(name_str).map(|id| TypeRef { id, name })
        }
    }
}

/// Resolve a supertrait reference's TypeExpr argument against the
/// enclosing trait's params. `Named("a")` where `"a"` is in
/// `trait_info.params` maps to the corresponding entry in `base_args`
/// (the enclosing trait's supplied args at the call site). Nested forms
/// (`Generic`, `Tuple`) recurse. Unmapped names and concrete primitives
/// fall through to their `Type::…` counterparts.
///
/// INVARIANT (round 101): the shapes produced here must be
/// CANONICAL — identical to what `resolve_type_expr` builds for the
/// same surface syntax on the impl side — because
/// `trait_arg_compatible_canon` compares them structurally and
/// `canonicalize()` does not collapse `Generic("List", …)` onto
/// `Type::List` (etc.). Non-canonical output makes every builtin
/// container / Unit supertrait arg spuriously incompatible
/// with its own impl.
pub(super) fn resolve_supertrait_arg(
    te: &TypeExpr,
    trait_info: &TraitInfo,
    base_args: &[Type],
) -> Type {
    match &te.kind {
        TypeExprKind::Named { name: sym, .. } => {
            if let Some(idx) = trait_info.params.iter().position(|p| p == sym)
                && let Some(ty) = base_args.get(idx)
            {
                return ty.clone();
            }
            // Bare type name that isn't a trait param — interpret as a
            // concrete type reference (Int, String, or user type).
            // Round 101: mirror `resolve_type_expr` (mod.rs Named arm)
            // exactly for primitives — Unit/() previously
            // fell through to `Type::Generic`, which never compares
            // equal to the canonical `Type::Unit` the
            // impl side produces, spuriously failing
            // `trait_arg_compatible_canon` with identical Display
            // strings on both sides of the error.
            let Some(ty) = written_type(te, *sym) else {
                return Type::Error;
            };
            match builtin_type_name(ty) {
                Some("Int") => Type::Int,
                Some("Float") => Type::Float,
                Some("Bool") => Type::Bool,
                Some("String") => Type::String,
                Some("Unit") => Type::Unit,
                _ => Type::Generic(ty, Vec::new()),
            }
        }
        TypeExprKind::Generic {
            name: sym, args, ..
        } => {
            let mut resolved: Vec<Type> = args
                .iter()
                .map(|a| resolve_supertrait_arg(a, trait_info, base_args))
                .collect();
            // Round 101: builtin container heads must produce the same
            // canonical `Type::…` shapes `resolve_type_expr` builds for
            // the impl side (mod.rs Generic arm). `canonicalize()` does
            // NOT collapse `Generic("List",[T])` onto `Type::List(T)`,
            // so leaving these as `Type::Generic` made supertrait args
            // like `List(b)` incomparable with the registered impl's
            // `Type::List(Int)`. Wrong-arity forms fall through to
            // `Type::Generic` (they can't match any canonical impl
            // shape, and resolve_type_expr diagnoses the arity at the
            // declaration site).
            let Some(ty) = written_type(te, *sym) else {
                return Type::Error;
            };
            match builtin_type_name(ty).unwrap_or_default() {
                "List" if resolved.len() == 1 => Type::List(Box::new(resolved.pop().unwrap())),
                "Range" if resolved.len() == 1 => Type::Range(Box::new(resolved.pop().unwrap())),
                "Set" if resolved.len() == 1 => Type::Set(Box::new(resolved.pop().unwrap())),
                "Channel" if resolved.len() == 1 => {
                    Type::Channel(Box::new(resolved.pop().unwrap()))
                }
                "Map" if resolved.len() == 2 => {
                    let v = resolved.pop().unwrap();
                    let k = resolved.pop().unwrap();
                    Type::Map(Box::new(k), Box::new(v))
                }
                _ => Type::Generic(ty, resolved),
            }
        }
        TypeExprKind::Tuple(elems) => Type::Tuple(
            elems
                .iter()
                .map(|e| resolve_supertrait_arg(e, trait_info, base_args))
                .collect(),
        ),
        TypeExprKind::Function(params, ret) => Type::Fun(
            params
                .iter()
                .map(|p| resolve_supertrait_arg(p, trait_info, base_args))
                .collect(),
            Box::new(resolve_supertrait_arg(ret, trait_info, base_args)),
        ),
        TypeExprKind::SelfType => Type::Error, // Self isn't meaningful in a supertrait arg
        TypeExprKind::AssocProj { .. } => {
            // Associated-type projections aren't meaningful as
            // supertrait args either: a supertrait arg is a fixed
            // type expression substituted from the enclosing trait's
            // params. Returning `Type::Error` matches the SelfType
            // arm and lets the caller continue without a cascade.
            Type::Error
        }
        TypeExprKind::AnonRecord { .. } => {
            // Anon records aren't meaningful as supertrait args either —
            // supertrait params are referred to by name.
            Type::Error
        }
    }
}

/// Returns true if the given type is a valid operand for arithmetic operators.
/// Type variables and `Type::Error` are treated as "maybe valid" (caller handles
/// the Var case via deferred checks).
pub(super) fn is_valid_arith_operand(ty: &Type) -> bool {
    match ty {
        Type::Int | Type::Float | Type::Error | Type::Never => true,
        Type::Var(_) => true,
        // Round 92: an abstract associated-type projection (`<a as T>::Item`
        // with the receiver still a where-bound type variable) is "maybe
        // valid" exactly like Type::Var — the concrete type is only known
        // once a trait impl binds it, and the concrete check fires at the
        // instantiation site (or as a VM operator error) just as for Var.
        Type::AssocProj { .. } => true,
        _ => false,
    }
}

/// The operand-domain diagnostic for an arithmetic operator (`op_str` is
/// the quoted operator, e.g. `'+'`). A String operand of `+` also names
/// the way to build strings: interpolation.
pub(super) fn arith_operand_message(op_str: &str, ty: &Type) -> String {
    let msg = format!("operator {op_str} requires Int or Float, got '{ty}'");
    if op_str == "'+'" && matches!(ty, Type::String) {
        format!("{msg}; build strings with interpolation, e.g. \"{{a}}{{b}}\"")
    } else {
        msg
    }
}

/// Returns true if the given type is a valid operand for comparison operators.
/// `is_equality` widens the domain to include types supported by Value's
/// PartialEq implementation but not `Value::cmp` (Tuple, Map, Set, Bool, Unit).
/// Type variables and `Type::Error` are treated as "maybe valid".
///
/// Round 93: this is a SHAPE check only. `Record(..)` / `Generic(..)`
/// heads pass here, but nominal operands are additionally vetted by
/// `TypeChecker::operand_builtin_trait_violation` at both call sites
/// (the concrete comparison arm and the deferred pending-check pass):
/// a record / enum wrapping a field that cannot satisfy Equal/Compare
/// (e.g. `Fn(..)` — closure ordering is Arc-pointer-address
/// nondeterministic) is rejected there with a field-precise message.
/// This free function stays stateless because every other arm is
/// purely structural.
///
/// Round 97: the same applies to CONTAINER heads. `List(_)` / `Range(_)`
/// (ordering + equality) and `Tuple`/`Map`/`Set` (equality) pass this
/// shape gate, but a container whose element / component / value type is
/// `Fn`-shaped would launder into the same Arc-pointer-address ordering
/// at runtime. `operand_builtin_trait_violation` recurses into the
/// element types (via `gate_field_supports_trait`) and rejects those.
pub(super) fn is_valid_compare_operand(ty: &Type, is_equality: bool) -> bool {
    match ty {
        Type::Int
        | Type::Float
        | Type::String
        | Type::List(_)
        | Type::Range(_)
        | Type::Record(..)
        | Type::Generic(..)
        | Type::Error
        | Type::Never => true,
        Type::Var(_) => true,
        // Round 92: abstract associated-type projections are "maybe valid"
        // like Type::Var — see is_valid_arith_operand above for rationale.
        Type::AssocProj { .. } => true,
        Type::Bool | Type::Unit | Type::Tuple(_) | Type::Map(..) | Type::Set(_) if is_equality => {
            true
        }
        // TYPE-GAP (round 81 F1): closed-row anon records compile down to
        // `Value::Record` and Value's PartialEq compares them element-wise
        // (src/value/key.rs), so `==`/`!=` is well-defined for them.
        // Open rows are rejected even on equality: two open-row values may
        // differ on unobserved fields, so the answer would depend on the
        // hidden tail — surface the row variable as the reason rather than
        // silently letting one row's surplus fields decide the result.
        // Ordering still rejects AnonRecord (the VM's compare() does not
        // support it, mirroring the Tuple/Map/Set/Bool/Unit treatment).
        Type::AnonRecord { fields: _, tail } if is_equality => {
            matches!(tail, RowTail::Closed)
        }
        // TYPE-LATENT-1 (round 82): Channel handles support identity-based
        // equality at runtime (`Value::Channel(a) == Value::Channel(b)` iff
        // `a.id == b.id`, see src/value/key.rs). Without this arm the
        // typechecker rejected `ch1 == ch2` even though the VM produces a
        // well-defined Bool. Ordering is still rejected: Channel ids are
        // identity tokens, not a meaningful well-order — same shape as
        // Tuple/Map/Set/Bool/Unit/AnonRecord above.
        Type::Channel(_) if is_equality => true,
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
mod tests;
