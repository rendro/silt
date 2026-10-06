use super::*;
use crate::ast::Selection;

/// Snapshot of a user type's resolved body, used only by the auto-
/// derive synthesis pass (`synthesize_auto_derive_impls`). Captures the
/// EnumInfo/RecordInfo data in an owned form so the synthesis loop can
/// iterate without holding a borrow on `self.tables.enums` / `self.tables.records`.
pub(super) enum TypeBodyKind {
    Enum(Vec<VariantInfo>),
    Record(Vec<(Symbol, Type)>),
}

impl TypeChecker {
    /// Auto-derive `Display`, `Compare`, `Equal`, `Hash` impls for every
    /// user-declared enum or record (`Display` only when the type has no
    /// manual `trait Display for T` impl; the other three are sealed, so
    /// they are always derived). Pushes synthesized [`TraitImpl`] AST nodes
    /// onto `decls` so they flow through the same registration pipeline
    /// (`register_trait_impl`) and the compiler's `Decl::TraitImpl`
    /// emission path as user-written impls.
    ///
    /// Skipped for:
    /// - `Display` on types with a manual `Display` impl (synthesized
    ///   impls carry `is_auto_derived: true`, and the coherence check in
    ///   `register_trait_impl` lets a user `Display` impl override one).
    /// - **Generic** user types (`type Box(a) { Foo(a) }`,
    ///   `type Pair(a, b) { x: a, y: b }`). These keep the prior
    ///   typecheck-stamp + `dispatch_trait_method` behaviour. Synthesis
    ///   of generic-typed bodies requires impl-level
    ///   `where a: Compare`-style clauses on every recursive call site,
    ///   plus correct propagation through `target_type_args` /
    ///   `target_param_names` / `where_clauses`. That work is mechanical
    ///   but invasive; gated as a follow-up. See round-62 audit notes
    ///   for the deferred-work entry.
    /// - The `Alias` body kind (handled separately by
    ///   `register_type_alias`).
    pub(super) fn synthesize_auto_derive_impls(&mut self, decls: &mut Vec<Decl>) {
        let display_sym = TraitKey::builtin("Display");
        // Scan for user-written `Display` impls so we can skip synthesis
        // for the types the user already covered. Use the canonical
        // target-type symbol so an impl on an alias skips synthesis on
        // every type under the same canonical name.
        let mut user_display_impls: std::collections::HashSet<TypeRef> =
            std::collections::HashSet::new();
        for decl in decls.iter() {
            if let Decl::TraitImpl(ti) = decl
                && !ti.is_auto_derived
                && self.named_trait(ti.trait_res, ti.trait_name) == Some(display_sym)
            {
                if let Some(target) = self.impl_target(ti) {
                    user_display_impls.insert(target);
                }
            }
        }
        // And for the types whose written `Display` impl an import brought
        // in (a module of the package, or an earlier REPL cell): deriving
        // one again would clash with it.
        let display_method = intern("display");
        for ((type_name, method), entry) in &self.tables.method_table {
            if *method == display_method
                && !entry.is_auto_derived
                && entry.trait_name == Some(display_sym)
            {
                user_display_impls.insert(*type_name);
            }
        }

        self.display_written = user_display_impls
            .iter()
            .map(|ty| canonical_head(&self.tables.resolver, *ty))
            .collect();

        let compare_sym = TraitKey::builtin("Compare");
        let equal_sym = TraitKey::builtin("Equal");
        let hash_sym = TraitKey::builtin("Hash");

        // Pre-collect type names + body kind so we can check field types
        // against `trait_impl_set` without holding a borrow on
        // `program.decls` while we mutate `self.tables.trait_impl_set` below.
        // (We don't mutate trait_impl_set here, but we do need to call
        // `type_name_for_impl` which takes `&self`.)
        //
        // Both non-generic and generic types are collected. For generic
        // types (`td.params` non-empty) the synth helpers emit a
        // where-clause `where p: <Trait>` for each `p` in `td.params`,
        // so generic-param fields trivially satisfy the trait being
        // synthesized.
        // Each task carries the span its synthesized nodes take: the type
        // declaration's, or `Span::BUILTIN` for a builtin type.
        let mut tasks: Vec<(TypeRef, Vec<Symbol>, TypeBodyKind, Span)> = Vec::new();
        // The types the module declares, which the walk over the
        // builtin types below skips.
        let mut user_decl_type_names: std::collections::HashSet<TypeRef> =
            std::collections::HashSet::new();
        for decl in decls.iter() {
            if let Decl::Type(td) = decl {
                match &td.body {
                    TypeBody::Enum(_) => {
                        let ty = self.own_type(td.name);
                        user_decl_type_names.insert(ty);
                        if let Some(info) = self.tables.enums.get(&ty) {
                            tasks.push((
                                ty,
                                td.params.clone(),
                                TypeBodyKind::Enum(info.variants.clone()),
                                td.span,
                            ));
                        }
                    }
                    TypeBody::Record(_) => {
                        let ty = self.own_type(td.name);
                        user_decl_type_names.insert(ty);
                        if let Some(info) = self.tables.records.get(&ty) {
                            tasks.push((
                                ty,
                                td.params.clone(),
                                TypeBodyKind::Record(info.fields.clone()),
                                td.span,
                            ));
                        }
                    }
                    TypeBody::Alias(_) => {}
                }
            }
        }
        // ── Round 93: field-aware eligibility gate ──────────────────
        // `register_type_decl` pre-stamps Equal/Compare/Hash/Display
        // for EVERY user type. Pre-round-93 the stamp stood even when
        // a field could never satisfy the trait — synthesis was merely
        // SKIPPED, so `==` / `<` / `.compare()` / `.hash()` on e.g. a
        // record wrapping a `Fn(..)` field typechecked and laundered
        // into nondeterministic Value-level fallbacks (closure
        // ordering = Arc pointer address under ASLR). Here we compute
        // honest, recursive, field-aware eligibility and UN-stamp the
        // ineligible pairs (recording a precise reason for
        // diagnostics). Equal/Compare/Hash are sealed (no hand-written
        // impl can exist), so one computation serves both the method
        // calls and the `==` / `<` operand checks in inference.rs.
        // Display is exempt: the runtime display fallback is total and
        // deterministic for every Value shape.
        self.enforce_auto_derive_field_gate(&user_decl_type_names);

        // Built-in enums and records are entered into
        // `self.tables.enums` / `self.tables.records` by the builtin
        // environment, from the builtin registry's type declarations,
        // without ever appearing as a top-level `Decl::Type`. Walk
        // both maps to give them the same synth treatment as user
        // types: every built-in `(trait, type)` pair pre-stamped in
        // `trait_impl_set` (see `register_builtin_trait_impls`)
        // receives a synthesized impl method, so `Op::CallMethod`'s
        // method lookup resolves at runtime without falling through to
        // `dispatch_trait_method`.
        //
        // This is the second half of the round-62 work: round 62
        // covered every user enum / record (generic + non-generic);
        // this round extends coverage to built-in enums / records.
        // After this pass, the Variant / Record arms in
        // `dispatch_trait_method` and the corresponding entries in
        // the hash allowlist are unreachable and can be deleted.
        //
        // Iteration order: enums then records, sorted by name within
        // each map, so the synthesized AST is deterministic.
        //
        // Only the builtin types are derived here, besides the module's
        // own: another module's types were derived when it was checked,
        // and their impls are in the session's tables. The builtin types'
        // impls are derived once, with the builtin environment (see
        // `BuiltinEnv::build`), and are not derived again.
        let builtin_derived = &self.tables.builtin_derived;
        let underived_builtin = |ty: &TypeRef| -> bool {
            builtin_type_name(*ty).is_some() && !builtin_derived.contains(ty)
        };
        let mut builtin_enum_names: Vec<TypeRef> = self
            .tables
            .enums
            .iter()
            .filter(|(n, _)| !user_decl_type_names.contains(*n) && underived_builtin(n))
            .map(|(n, _)| *n)
            .collect();
        builtin_enum_names.sort_by_key(|t| resolve(t.name));
        for type_name in builtin_enum_names {
            if let Some(info) = self.tables.enums.get(&type_name) {
                tasks.push((
                    type_name,
                    info.params.clone(),
                    TypeBodyKind::Enum(info.variants.clone()),
                    Span::BUILTIN,
                ));
            }
        }
        let mut builtin_record_names: Vec<TypeRef> = self
            .tables
            .records
            .iter()
            .filter(|(n, _)| !user_decl_type_names.contains(*n) && underived_builtin(n))
            .map(|(n, _)| *n)
            .collect();
        builtin_record_names.sort_by_key(|t| resolve(t.name));
        for type_name in builtin_record_names {
            if let Some(info) = self.tables.records.get(&type_name) {
                // Built-in records are non-generic; the params vec is
                // empty. (Generic built-in records would need their
                // param Symbol names tracked alongside `record_param_var_ids`
                // — extend this branch the day a generic built-in
                // record appears.)
                let params = self
                    .tables
                    .record_param_var_ids
                    .get(&type_name)
                    .map(|ids| {
                        // No symbolic param names are tracked for
                        // built-in records; synth helpers only need a
                        // count + uniqueness, so synthesize fresh
                        // placeholder Symbols. Today this branch is
                        // unreachable because every built-in record
                        // is non-generic.
                        (0..ids.len())
                            .map(|i| intern(&format!("__builtin_rec_param_{i}__")))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                tasks.push((
                    type_name,
                    params,
                    TypeBodyKind::Record(info.fields.clone()),
                    Span::BUILTIN,
                ));
            }
        }

        let mut synthesized: Vec<Decl> = Vec::new();
        for (type_name, type_params, body, decl_span) in tasks {
            let derive = auto_derive::Derive {
                span: decl_span,
                ty: type_name,
                variants: self.variant_resolutions(type_name),
            };
            // Helper closures to scope the synthesis decisions per-trait.
            let key = canonical_head(&self.tables.resolver, type_name);

            // Resolve this type's field types in the form they appear in
            // EnumInfo/RecordInfo (already-resolved Types). Use them to
            // decide which traits to synthesize: a trait whose method
            // body would call `.compare()` / `.hash()` on a field that
            // doesn't satisfy that trait would fail the body-check pass.
            // Skipping synthesis for those cases falls back to the
            // typecheck-stamp behaviour from `register_type_decl`, which
            // is intentionally permissive (the call site only fails if
            // the user actually invokes the method).
            //
            // Recursive same-type references are accepted via
            // self_supports = true: e.g. `type Tree { Leaf, Node(Tree, Tree) }`
            // can derive Compare because the recursive `.compare()` calls
            // resolve to the same synthesized method we're emitting.
            //
            // For generic types, every generic param is treated as
            // supporting the trait we're synthesizing — the synthesized
            // impl carries `where p: <Trait>` for each param, so the
            // trait obligation on the recursive `.compare()` etc. will
            // be satisfied at the impl-instantiation site by the user
            // (or rejected there with a localised error). A field that
            // mentions a generic param via `Type::Var` resolves through
            // the param_var_ids in EnumInfo/RecordInfo.
            let param_var_ids: std::collections::HashSet<TyVar> = match &body {
                TypeBodyKind::Enum(_) => self
                    .tables
                    .enums
                    .get(&type_name)
                    .map(|info| info.param_var_ids.iter().copied().collect())
                    .unwrap_or_default(),
                TypeBodyKind::Record(_) => self
                    .tables
                    .record_param_var_ids
                    .get(&type_name)
                    .map(|ids| ids.iter().copied().collect())
                    .unwrap_or_default(),
            };
            let supports = |_trait_sym: TraitKey, ty: &Type| -> bool {
                // Self-references — same nominal head as the type we're
                // synthesizing for — are always allowed (the recursive
                // body calls the same method we register).
                if let Some(name) = self.type_name_for_impl(ty)
                    && name == type_name
                {
                    return true;
                }
                // Generic-param references resolve to a TyVar whose id
                // appears in `param_var_ids`; treat them as supporting
                // the trait being synthesized (the where-clause covers
                // the obligation at the impl-instantiation site).
                if let Type::Var(v) = self.apply(ty)
                    && param_var_ids.contains(&v)
                {
                    return true;
                }
                self.field_type_supports_trait(_trait_sym, ty)
            };

            let (mut compare_ok, mut equal_ok, mut hash_ok, mut display_ok) =
                (true, true, true, true);
            match &body {
                TypeBodyKind::Enum(variants) => {
                    for v in variants {
                        for field_ty in &v.field_types {
                            compare_ok &= supports(compare_sym, field_ty);
                            equal_ok &= supports(equal_sym, field_ty);
                            hash_ok &= supports(hash_sym, field_ty);
                            display_ok &= supports(display_sym, field_ty);
                        }
                    }
                }
                TypeBodyKind::Record(fields) => {
                    for (_, field_ty) in fields {
                        compare_ok &= supports(compare_sym, field_ty);
                        equal_ok &= supports(equal_sym, field_ty);
                        hash_ok &= supports(hash_sym, field_ty);
                        display_ok &= supports(display_sym, field_ty);
                    }
                }
            }

            // Per-trait policy gate. A `(trait, type)` pair receives
            // a synthesized impl ONLY if it is already present in
            // `trait_impl_set`. For user-declared types,
            // `register_type_decl` pre-stamps all four built-in traits
            // unconditionally (line ~2314), so this gate is a no-op
            // for the user-type path. For built-in types,
            // `register_builtin_trait_impls` stamps only the
            // policy-permitted traits (e.g. Compare is excluded for
            // Option/Result/Tuple/Map/Set, see `non_ordering_traits`),
            // and the gate honours that exclusion — synth would
            // otherwise produce a `Compare:Option` impl that breaks
            // `tests/cli/trait_init_parity_tests.rs`.
            let policy_allows = |trait_sym: TraitKey| -> bool {
                self.tables.trait_impl_set.contains(&(trait_sym, key))
            };

            match body {
                TypeBodyKind::Enum(variants) => {
                    // Convert resolved VariantInfo back into AST EnumVariant
                    // shape (the auto_derive helpers operate on AST forms).
                    // Only .name and .fields.len() matter — the synthesis
                    // just emits .compare() / .hash() / .display() /
                    // .equal() calls on positionally-named bound vars, so
                    // the field TypeExprs are placeholders.
                    let ast_variants: Vec<EnumVariant> = variants
                        .iter()
                        .map(|v| EnumVariant {
                            name: v.name,
                            name_span: decl_span,
                            // Synthesize `Wildcard` placeholder TypeExprs;
                            // the auto_derive helpers only count them.
                            fields: v
                                .field_types
                                .iter()
                                .map(|_| {
                                    TypeExpr::new(
                                        TypeExprKind::Named {
                                            module: None,
                                            name: intern("__synth_placeholder__"),
                                            name_span: decl_span,
                                        },
                                        decl_span,
                                    )
                                })
                                .collect(),
                        })
                        .collect();
                    if display_ok
                        && policy_allows(display_sym)
                        && !user_display_impls.contains(&key)
                    {
                        synthesized.push(Decl::TraitImpl(derive.synth_display_impl_for_enum(
                            type_name.name,
                            &type_params,
                            &ast_variants,
                        )));
                    }
                    if compare_ok && policy_allows(compare_sym) {
                        synthesized.push(Decl::TraitImpl(derive.synth_compare_impl_for_enum(
                            type_name.name,
                            &type_params,
                            &ast_variants,
                        )));
                    }
                    if equal_ok && policy_allows(equal_sym) {
                        synthesized.push(Decl::TraitImpl(derive.synth_equal_impl_for_enum(
                            type_name.name,
                            &type_params,
                            &ast_variants,
                        )));
                    }
                    if hash_ok && policy_allows(hash_sym) {
                        synthesized.push(Decl::TraitImpl(derive.synth_hash_impl_for_enum(
                            type_name.name,
                            &type_params,
                            &ast_variants,
                        )));
                    }
                }
                TypeBodyKind::Record(fields) => {
                    let ast_fields: Vec<RecordField> = fields
                        .iter()
                        .map(|(name, _)| RecordField {
                            name: *name,
                            name_span: decl_span,
                            ty: TypeExpr::new(
                                TypeExprKind::Named {
                                    module: None,
                                    name: intern("__synth_placeholder__"),
                                    name_span: decl_span,
                                },
                                decl_span,
                            ),
                        })
                        .collect();
                    if display_ok
                        && policy_allows(display_sym)
                        && !user_display_impls.contains(&key)
                    {
                        synthesized.push(Decl::TraitImpl(derive.synth_display_impl_for_record(
                            type_name.name,
                            &type_params,
                            &ast_fields,
                        )));
                    }
                    if compare_ok && policy_allows(compare_sym) {
                        synthesized.push(Decl::TraitImpl(derive.synth_compare_impl_for_record(
                            type_name.name,
                            &type_params,
                            &ast_fields,
                        )));
                    }
                    if equal_ok && policy_allows(equal_sym) {
                        synthesized.push(Decl::TraitImpl(derive.synth_equal_impl_for_record(
                            type_name.name,
                            &type_params,
                            &ast_fields,
                        )));
                    }
                    if hash_ok && policy_allows(hash_sym) {
                        synthesized.push(Decl::TraitImpl(derive.synth_hash_impl_for_record(
                            type_name.name,
                            &type_params,
                            &ast_fields,
                        )));
                    }
                }
            }
        }
        decls.extend(synthesized);
    }
}

/// Synthesize `TraitDecl` AST nodes for the five built-in traits.
///
/// Round 62 (item 3 of type-design improvements) — built-in trait
/// registration was previously a separate code path that hand-rolled
/// `TraitInfo` entries directly, bypassing the duplicate-method check,
/// supertrait/where-clause processing, and any future feature added to
/// `register_trait_decl`. This function returns the same `TraitDecl`
/// shape a parser would produce for hand-written user code, so the
/// unified `register_trait_decl_inner` pipeline handles built-ins and
/// user traits identically.
///
/// The five built-ins:
/// - `Display`: `fn display(self) -> String` (signature only).
/// - `Compare`: `fn compare(self, other: Self) -> Int` (signature only).
/// - `Equal`:   `fn equal(self, other: Self) -> Bool` (signature only).
/// - `Hash`:    `fn hash(self) -> Int` (signature only).
/// - `Error: Display { fn message(self) -> String { self.display() } }`.
///   Carries a real default body, which an impl that omits `message`
///   runs (`builtin_default_methods`). No program declares the trait,
///   so no check resolves the body: the call is written resolved, to
///   `Display`'s method.
pub(super) fn builtin_trait_decls() -> Vec<TraitDecl> {
    let dummy_span = Span::BUILTIN;
    let self_sym = intern("self");
    let other_sym = intern("other");

    fn self_param(self_sym: Symbol, span: Span) -> Param {
        Param {
            kind: ParamKind::Data,
            pattern: Pattern::new(PatternKind::Ident(self_sym), span),
            ty: None,
        }
    }
    fn other_param(other_sym: Symbol, span: Span) -> Param {
        // `other: Self`
        Param {
            kind: ParamKind::Data,
            pattern: Pattern::new(PatternKind::Ident(other_sym), span),
            ty: Some(TypeExpr::new(TypeExprKind::SelfType, span)),
        }
    }
    fn unit_body(span: Span) -> Expr {
        Expr::new(ExprKind::Unit, span)
    }
    fn named_ret(name: &str, span: Span) -> Option<TypeExpr> {
        Some(TypeExpr::new(
            TypeExprKind::Named {
                module: None,
                name: intern(name),
                name_span: span,
            },
            span,
        ))
    }
    fn sig_only_method(name: &str, params: Vec<Param>, ret: &str, span: Span) -> FnDecl {
        FnDecl {
            name: intern(name),
            params,
            return_type: named_ret(ret, span),
            where_clauses: Vec::new(),
            body: unit_body(span),
            is_pub: true,
            span,
            // Synthesized built-in: no source identifier — mirror `span`.
            name_span: span,
            is_recovery_stub: false,
            is_signature_only: true,
            doc: None,
        }
    }

    // Error.message default body: `self.display()`
    let error_default_body = {
        let self_ident = Expr::new(ExprKind::Ident(self_sym), dummy_span);
        let mut field_access = Expr::new(
            ExprKind::FieldAccess(Box::new(self_ident), intern("display"), dummy_span),
            dummy_span,
        );
        field_access.res =
            crate::defs::builtin_trait_id("Display").map(|t| crate::defs::Res::Def(t.0));
        field_access.sel =
            crate::defs::builtin_trait_id("Display").map(|tr| Selection::Dynamic { tr });
        Expr::new(
            ExprKind::Call(Box::new(field_access), Vec::new()),
            dummy_span,
        )
    };
    let error_message_fn = FnDecl {
        name: intern("message"),
        params: vec![self_param(self_sym, dummy_span)],
        return_type: named_ret("String", dummy_span),
        where_clauses: Vec::new(),
        body: error_default_body,
        is_pub: true,
        span: dummy_span,
        // Synthesized built-in `Error.message`: no source ident — mirror span.
        name_span: dummy_span,
        is_recovery_stub: false,
        is_signature_only: false,
        doc: None,
    };

    vec![
        // trait Display { fn display(self) -> String }
        TraitDecl {
            name: intern("Display"),
            name_span: dummy_span,
            params: Vec::new(),
            supertraits: Vec::new(),
            param_where_clauses: Vec::new(),
            methods: vec![sig_only_method(
                "display",
                vec![self_param(self_sym, dummy_span)],
                "String",
                dummy_span,
            )],
            assoc_types: Vec::new(),
            is_pub: true,
            span: dummy_span,
            doc: None,
        },
        // trait Compare: Equal { fn compare(self, other: Self) -> Int }
        // What is ordered can be compared for equality.
        TraitDecl {
            name: intern("Compare"),
            name_span: dummy_span,
            params: Vec::new(),
            supertraits: vec![TraitRef {
                module: None,
                name: intern("Equal"),
                args: Vec::new(),
                span: dummy_span,
                res: None,
            }],
            param_where_clauses: Vec::new(),
            methods: vec![sig_only_method(
                "compare",
                vec![
                    self_param(self_sym, dummy_span),
                    other_param(other_sym, dummy_span),
                ],
                "Int",
                dummy_span,
            )],
            assoc_types: Vec::new(),
            is_pub: true,
            span: dummy_span,
            doc: None,
        },
        // trait Equal { fn equal(self, other: Self) -> Bool }
        TraitDecl {
            name: intern("Equal"),
            name_span: dummy_span,
            params: Vec::new(),
            supertraits: Vec::new(),
            param_where_clauses: Vec::new(),
            methods: vec![sig_only_method(
                "equal",
                vec![
                    self_param(self_sym, dummy_span),
                    other_param(other_sym, dummy_span),
                ],
                "Bool",
                dummy_span,
            )],
            assoc_types: Vec::new(),
            is_pub: true,
            span: dummy_span,
            doc: None,
        },
        // trait Number {}: the types arithmetic is on, Int and Float.
        TraitDecl {
            name: intern("Number"),
            name_span: dummy_span,
            params: Vec::new(),
            supertraits: Vec::new(),
            param_where_clauses: Vec::new(),
            methods: Vec::new(),
            assoc_types: Vec::new(),
            is_pub: true,
            span: dummy_span,
            doc: None,
        },
        // trait Hash { fn hash(self) -> Int }
        TraitDecl {
            name: intern("Hash"),
            name_span: dummy_span,
            params: Vec::new(),
            supertraits: Vec::new(),
            param_where_clauses: Vec::new(),
            methods: vec![sig_only_method(
                "hash",
                vec![self_param(self_sym, dummy_span)],
                "Int",
                dummy_span,
            )],
            assoc_types: Vec::new(),
            is_pub: true,
            span: dummy_span,
            doc: None,
        },
        // trait Error: Display { fn message(self) -> String { self.display() } }
        TraitDecl {
            name: intern("Error"),
            name_span: dummy_span,
            params: Vec::new(),
            supertraits: vec![TraitRef {
                module: None,
                name: intern("Display"),
                args: Vec::new(),
                span: dummy_span,
                res: None,
            }],
            param_where_clauses: Vec::new(),
            methods: vec![error_message_fn],
            assoc_types: Vec::new(),
            is_pub: true,
            span: dummy_span,
            doc: None,
        },
    ]
}

/// Register built-in trait declarations (Display/Compare/Equal/Hash/Error)
/// and their auto-derived impls for primitives and builtin containers.
///
/// This is the single source of truth for derive policy. The builtin
/// environment every check starts from is built with it, so no two
/// checks diverge on which types implement which traits.
///
/// Round 62 (item 3 of type-design improvements): the trait-decl
/// registration step now flows through the same code path as user
/// `trait X { ... }` declarations. See `builtin_trait_decls` for the
/// synthesized AST nodes and `register_trait_decl_inner` for the
/// shared body. The auto-derive policy below remains imperative — it's
/// policy (which types pre-stamp Display/Compare/Equal/Hash impls), not
/// declaration.
///
/// Derive policy:
/// - `Int`, `Float`, `Bool`, `String`, `()`, `List` get all four
///   built-in traits (Equal, Compare, Hash, Display).
/// - `Tuple`, `Map`, `Set` get Equal/Hash/Display only — the VM's
///   `compare()` (src/vm/arithmetic.rs) does not support ordering for
///   these, so registering Compare would type-check code that then
///   panics at runtime.
/// - `Option`, `Result` get Equal/Hash/Display only. They wrap generic
///   parameters; the auto-derived methods are stored as polymorphic
///   templates and instantiated at each call site. Compare is
///   excluded because ordering on Variants is limited to same-name
///   variants at runtime.
pub(super) fn register_builtin_trait_impls(checker: &mut TypeChecker) {
    // ── Register built-in trait declarations through the unified
    //    register_trait_decl_inner pipeline. Same code path user
    //    `trait X { fn ... }` declarations take, minus the
    //    BUILTIN_TRAIT_NAMES redefinition check (which is keyed off
    //    user input and lives on register_trait_decl_user). ─────────
    for td in builtin_trait_decls() {
        checker.register_trait_decl_inner(&td);
    }

    // ── Register auto-derived impls ─────────────────────────────
    // Error is intentionally excluded from auto-derive: user code and
    // stdlib must `trait Error for XyzError { ... }` explicitly. Only
    // Equal/Compare/Hash/Display are auto-derived for the built-in
    // types below.
    let all_auto_traits: &[&str] = BUILTIN_AUTO_DERIVED_TRAIT_NAMES;
    let non_ordering_traits: &[&str] = &["Equal", "Hash", "Display"];

    // Primitives + List: all four auto-derived traits.
    register_auto_derived_impls_for(
        checker,
        // Round 75 TYPE-3 LATENT: canonical key for the unit type is
        // "Unit" (matches canonical_name(Type::Unit) and
        // dispatch_type_for_value(Value::Unit)); an impl target `()`
        // names it too.
        &["Int", "Float", "Bool", "String", "Unit"],
        all_auto_traits,
    );
    register_auto_derived_impls_for(checker, &["List"], all_auto_traits);
    // Arithmetic is on Int and Float.
    for ty in ["Int", "Float"] {
        checker
            .tables
            .trait_impl_set
            .insert((TraitKey::builtin("Number"), TypeRef::builtin(ty)));
    }
    // A tuple has what its parts have; a map and a set have no order.
    register_auto_derived_impls_for(checker, &["Tuple"], all_auto_traits);
    register_auto_derived_impls_for(checker, &["Map", "Set"], non_ordering_traits);
    // A channel is equal to itself only (`ch1 == ch2`).
    register_auto_derived_impls_for(checker, &["Channel"], &["Equal"]);
    // ── Built-in enums + records that flow through synth ────────────
    //
    // Each stamp below tells `synthesize_auto_derive_impls` which
    // (trait, type) pairs may receive a synthesized impl method, and
    // makes `field_type_supports_trait` true for fields of these types
    // (e.g. `Option(DateTime)` on `FileStat`). The synth pass finds the
    // types by a walk over `self.tables.enums` / `self.tables.records`
    // and emits the `TraitImpl` AST a user-declared type receives.

    // Each type the builtin registry declares derives what the registry
    // says: all four traits, unless it names fewer (`Option` and
    // `Result`, generic wrappers: no `Compare`; `http.Response` holds a
    // `Map`: no `Compare`; `channel.ChannelOp` holds a channel: none).
    // The stamps of a module's error enum add `Equal`/`Compare`/`Hash`
    // to the `Error` and `Display` the builtin environment entered for
    // it (insert is idempotent).
    for (_, ty) in crate::builtins::registry::registry().types() {
        register_auto_derived_impls_for(checker, &[ty.name], ty.derives);
    }

    // Bytes: Display here (`Value::Bytes` prints as a short hex preview
    // and its length), Equal and Hash below. No Compare: bytes are not
    // an ordered key type (`bytes.to_hex` first).
    register_auto_derived_impls_for(checker, &["Bytes"], &["Display"]);

    // `==` on a value of an opaque type is the value's own equality (a
    // handle's identity, the bytes' content).
    let registry = crate::builtins::registry::registry();
    let opaque = registry
        .modules
        .iter()
        .flat_map(|module| module.opaque.iter())
        .map(|(name, _)| *name)
        .chain(["Bytes"]);
    for name in opaque {
        register_auto_derived_impls_for(checker, &[name], &["Equal"]);
    }
    // Bytes are a map key and a set element by their content.
    register_auto_derived_impls_for(checker, &["Bytes"], &["Hash"]);

    // TcpListener / TcpStream are registered in BUILTIN_TYPES so the
    // trait-impl-target gate gives an orphan-rule rejection (rather
    // than "type not declared") if a user tries to add their own
    // impls, but no built-in trait impls are stamped — they remain
    // unprintable opaque resources.
}

/// Register auto-derived trait impls and method-table entries for a
/// group of types against a set of trait names. Shared helper used by
/// `register_builtin_trait_impls` and the `time` builtin module so
/// the set of derived methods stays consistent.
///
/// The four built-in trait methods are always considered. A method is
/// registered only when its parent trait appears in `trait_names`:
/// - `display` ← Display
/// - `equal`   ← Equal
/// - `compare` ← Compare
/// - `hash`    ← Hash
pub(super) fn register_auto_derived_impls_for(
    checker: &mut TypeChecker,
    type_names: &[&str],
    trait_names: &[&str],
) {
    let dummy_span = Span::BUILTIN;
    let has_display = trait_names.contains(&"Display");
    let has_equal = trait_names.contains(&"Equal");
    let has_compare = trait_names.contains(&"Compare");
    let has_hash = trait_names.contains(&"Hash");
    for type_name in type_names {
        for trait_name in trait_names {
            checker
                .tables
                .trait_impl_set
                .insert((TraitKey::builtin(trait_name), TypeRef::builtin(type_name)));
        }
        // Build method entries only for traits in `trait_names`.
        let mut methods: Vec<(&str, Type)> = Vec::with_capacity(4);
        if has_display {
            methods.push((
                "display",
                Type::Fun(vec![checker.fresh_var()], Box::new(Type::String)),
            ));
        }
        if has_equal {
            methods.push((
                "equal",
                Type::Fun(
                    vec![checker.fresh_var(), checker.fresh_var()],
                    Box::new(Type::Bool),
                ),
            ));
        }
        if has_compare {
            methods.push((
                "compare",
                Type::Fun(
                    vec![checker.fresh_var(), checker.fresh_var()],
                    Box::new(Type::Int),
                ),
            ));
        }
        if has_hash {
            methods.push((
                "hash",
                Type::Fun(vec![checker.fresh_var()], Box::new(Type::Int)),
            ));
        }
        for (method_name, method_type) in &methods {
            checker.tables.method_table.insert(
                (TypeRef::builtin(type_name), intern(method_name)),
                MethodEntry {
                    method_type: method_type.clone(),
                    span: dummy_span,
                    is_auto_derived: true,
                    trait_name: None,
                    preds: Vec::new(),
                },
            );
        }
    }
}
