//! Auto-derive synthesis for built-in traits on user-declared types.
//!
//! For every user enum or record we synthesize a `TraitImpl` AST node
//! for each of Compare, Equal and Hash (sealed: they cannot be written
//! by hand), and for Display unless the type has a manual
//! `trait Display for T` impl. The synthesized impl's method bodies are real silt
//! AST (match expressions, let bindings, calls) so they flow through the
//! typechecker's body-check pass and the compiler's TraitImpl emit path
//! exactly the same as a user-written impl. The result is that
//! `Op::CallMethod`'s qualified-global lookup at runtime finds e.g.
//! `Color.compare` directly, and never falls through to
//! `dispatch_trait_method` for user-defined enum/record receivers.
//!
//! Replaces the prior typecheck-only stamp pattern (`trait_impl_set`
//! insertion + `method_table` registration with `is_auto_derived: true`,
//! no body) which was load-bearing on hand-rolled VM dispatch arms in
//! `src/vm/dispatch.rs` and required round-after-round sync between
//! "typechecker accepts" and "VM can run".
//!
//! ## Scope
//!
//! Covers **both non-generic and generic** user enums and records. For
//! a generic type `type Box(a) { Foo(a) }`, the synthesized impl is
//! shaped as
//!
//! ```silt
//! trait Compare for Box(a) where a: Compare {
//!     fn compare(self: Box(a), other: Box(a)) -> Int = ...
//! }
//! ```
//!
//! — i.e. each generic param gets a where-clause bound to whichever
//! trait we're synthesizing. The body is identical to the non-generic
//! case because the recursive `.compare()` / `.equal()` / `.hash()` /
//! `.display()` calls on field bindings flow through the trait dispatch
//! using the where-bound, not a structural inspection of the field type.
//!
//! ## Body shapes
//!
//! For `type Color { Red, Green(Int), Blue(Int, String) }`:
//!
//! ```silt
//! trait Compare for Color {
//!   fn compare(self: Color, other: Color) -> Int {
//!     match (self, other) {
//!       (Red, Red) -> 0
//!       (Green(xa), Green(xb)) -> xa.compare(xb)
//!       (Blue(a1, a2), Blue(b1, b2)) -> {
//!         let c1 = a1.compare(b1)
//!         match c1 { 0 -> a2.compare(b2), _ -> c1 }
//!       }
//!       _ -> {
//!         let ord_self = match self { Red -> 0, Green(_) -> 1, Blue(_, _) -> 2 }
//!         let ord_other = match other { Red -> 0, Green(_) -> 1, Blue(_, _) -> 2 }
//!         ord_self.compare(ord_other)
//!       }
//!     }
//!   }
//! }
//! ```
//!
//! Equal and Hash mirror the same nested-match shape; Display per-
//! variant renders `Tag` for nullary variants and `Tag(arg1, arg2, ...)`
//! for n-ary variants, recursing into `.display()` on each arg.
//!
//! For records, lex-comparing fields in declaration order with
//! `if cx != 0 { cx } else { ... }`-style chaining (encoded as nested
//! `match cx { 0 -> ..., _ -> cx }`).

use crate::ast::*;
use crate::intern::{Symbol, intern};
use crate::source::Span;

/// The synthesis of the derived impls of one type. Every node it makes
/// takes `span`, the span of the type's declaration, so a diagnostic or a
/// runtime error inside a derived method points at the type.
pub(super) struct Derive {
    pub(super) span: Span,
    /// The type the impls are for. A variant pattern is written with it
    /// (`Shape.Circle(r)`): two enums may have variants of one name.
    pub(super) ty: Symbol,
}

impl Derive {
    fn id_pat(&self, name: Symbol) -> Pattern {
        Pattern::new(PatternKind::Ident(name), self.span)
    }

    fn wildcard_pat(&self) -> Pattern {
        Pattern::new(PatternKind::Wildcard, self.span)
    }

    fn ctor_pat(&self, name: Symbol, args: Vec<Pattern>) -> Pattern {
        Pattern::new(
            PatternKind::Constructor {
                qualifier: vec![Qualifier {
                    name: self.ty,
                    span: self.span,
                }],
                name,
                name_span: self.span,
                args,
            },
            self.span,
        )
    }

    fn tuple_pat(&self, elems: Vec<Pattern>) -> Pattern {
        Pattern::new(PatternKind::Tuple(elems), self.span)
    }

    fn ident_expr(&self, name: Symbol) -> Expr {
        Expr::new(ExprKind::Ident(name), self.span)
    }

    fn int_expr(&self, n: i64) -> Expr {
        Expr::new(ExprKind::Int(n), self.span)
    }

    fn bool_expr(&self, b: bool) -> Expr {
        Expr::new(ExprKind::Bool(b), self.span)
    }

    fn string_expr(&self, s: &str) -> Expr {
        Expr::new(ExprKind::StringLit(s.to_string(), false), self.span)
    }

    fn tuple_expr(&self, elems: Vec<Expr>) -> Expr {
        Expr::new(ExprKind::Tuple(elems), self.span)
    }

    /// `recv.method(args...)` — used to emit `xa.compare(xb)`,
    /// `self.x.equal(other.x)`, etc. The implementation is `Call(FieldAccess)`
    /// because that is what the parser produces for surface-syntax method
    /// calls.
    fn method_call(&self, recv: Expr, method: Symbol, args: Vec<Expr>) -> Expr {
        let fa = Expr::new(
            ExprKind::FieldAccess(Box::new(recv), method, self.span),
            self.span,
        );
        Expr::new(ExprKind::Call(Box::new(fa), args), self.span)
    }

    /// `recv.field` — record field access.
    fn field_access(&self, recv: Expr, field: Symbol) -> Expr {
        Expr::new(
            ExprKind::FieldAccess(Box::new(recv), field, self.span),
            self.span,
        )
    }

    /// `a + b` — used to combine display strings.
    fn bin(&self, a: Expr, op: BinOp, b: Expr) -> Expr {
        Expr::new(ExprKind::Binary(Box::new(a), op, Box::new(b)), self.span)
    }

    fn match_expr(&self, scrut: Expr, arms: Vec<MatchArm>) -> Expr {
        Expr::new(
            ExprKind::Match {
                expr: Some(Box::new(scrut)),
                arms,
            },
            self.span,
        )
    }

    fn arm(&self, pattern: Pattern, body: Expr) -> MatchArm {
        MatchArm {
            pattern,
            guard: None,
            body,
        }
    }

    fn block_expr(&self, stmts: Vec<Stmt>) -> Expr {
        Expr::new(ExprKind::Block(stmts), self.span)
    }

    fn let_stmt(&self, name: Symbol, value: Expr) -> Stmt {
        Stmt::Let {
            pattern: self.id_pat(name),
            ty: None,
            value,
        }
    }

    // ── Depth-bounded combinators (round 92) ─────────────────────────────
    //
    // The original derive bodies were built as *left-leaning* chains: a
    // fold of `Binary`/`Block` nodes nesting one level per field/variant
    // arg. The typechecker's and compiler's expression walkers recurse per
    // AST level, so an N-field record synthesized an O(N)-deep expression
    // and a ~70-field record overflowed the 8 MiB silt-main stack on
    // `silt check` (debug-build frames are tens of KiB). The helpers below
    // produce the *same observable semantics* with O(log N) (balanced
    // trees) or O(1) (statement sequences) expression depth, so derive
    // depth no longer scales with record/variant width at all.

    /// Fold a non-empty list of expressions into a **balanced** binary
    /// tree with `combine`, preserving the left-to-right order of the
    /// leaves. Reduction is pairwise per round: `[e0, e1, e2, e3, e4]` →
    /// `[c(e0,e1), c(e2,e3), e4]` → … — depth is ⌈log2 N⌉ instead of the
    /// N-1 of a left fold.
    ///
    /// Semantics caveat for callers: this changes the *grouping* of
    /// `combine` applications, so it is only valid when `combine` is
    /// associative AND its evaluation order over the leaves matches the
    /// left fold's (true for `&&` and the compare
    /// first-non-zero combinator used below — each evaluates its left
    /// operand fully before deciding whether to evaluate the right one,
    /// so leaves still run strictly left-to-right with identical
    /// short-circuiting).
    fn balanced_fold(
        &self,
        mut items: Vec<Expr>,
        combine: &mut dyn FnMut(Expr, Expr) -> Expr,
    ) -> Expr {
        debug_assert!(!items.is_empty(), "balanced_fold requires >= 1 item");
        while items.len() > 1 {
            let mut next: Vec<Expr> = Vec::with_capacity(items.len().div_ceil(2));
            let mut iter = items.into_iter();
            while let Some(a) = iter.next() {
                match iter.next() {
                    Some(b) => next.push(combine(a, b)),
                    None => next.push(a),
                }
            }
            items = next;
        }
        items.pop().expect("balanced_fold invariant: one item left")
    }

    /// Join display pieces into one string with interpolation. Literal
    /// pieces become literal segments (adjacent ones merged) and the
    /// `.display()` calls become interpolated segments, which run strictly
    /// left to right. An interpolation holds at most 255 segments (the
    /// compiler's `StringConcat` count is a `u8`), so a wider join nests
    /// interpolations of at most 255 segments each: depth O(log N).
    fn concat_all(&self, pieces: Vec<Expr>) -> Expr {
        let mut parts: Vec<StringPart> = Vec::with_capacity(pieces.len());
        for piece in pieces {
            match piece.kind {
                ExprKind::StringLit(lit, _) => match parts.last_mut() {
                    Some(StringPart::Literal(prev)) => prev.push_str(&lit),
                    _ => parts.push(StringPart::Literal(lit)),
                },
                _ => parts.push(StringPart::Expr(piece)),
            }
        }
        const MAX_SEGMENTS: usize = u8::MAX as usize;
        while parts.len() > MAX_SEGMENTS {
            let mut chunks = Vec::with_capacity(parts.len().div_ceil(MAX_SEGMENTS));
            let mut iter = parts.into_iter().peekable();
            while iter.peek().is_some() {
                let chunk: Vec<StringPart> = iter.by_ref().take(MAX_SEGMENTS).collect();
                chunks.push(StringPart::Expr(self.interp_expr(chunk)));
            }
            parts = chunks;
        }
        self.interp_expr(parts)
    }

    fn interp_expr(&self, parts: Vec<StringPart>) -> Expr {
        Expr::new(ExprKind::StringInterp(parts), self.span)
    }

    /// Balanced `&&`-join. `&&` is associative, and the balanced grouping
    /// preserves the left fold's observable behavior exactly: operands
    /// still evaluate strictly left-to-right, and evaluation still stops
    /// at the first `false` (a false left subtree short-circuits its whole
    /// right sibling). Used by the Equal derives.
    fn and_all(&self, pieces: Vec<Expr>) -> Expr {
        self.balanced_fold(pieces, &mut |a, b| self.bin(a, BinOp::And, b))
    }

    /// Combine two compare results in the "first non-zero wins" monoid:
    ///
    /// ```silt
    /// { let __d_bcK__ = <left>
    ///   match __d_bcK__ { 0 -> <right>, _ -> __d_bcK__ } }
    /// ```
    ///
    /// This combinator is **associative** — `(a ⊕ b) ⊕ c` and `a ⊕ (b ⊕ c)`
    /// both yield the first non-zero of `a, b, c` (or 0) — which is exactly
    /// the lexicographic-compare contract, so a *balanced* fold of the
    /// per-field compares is semantically identical to the old
    /// left-leaning `match`-nest: the first field pair (in declaration
    /// order) whose compare is non-zero decides the result. Short-circuit
    /// order is also preserved: the right subtree is evaluated only when
    /// the left subtree's combined result is 0, i.e. only when every
    /// earlier field compared equal. `counter` supplies fresh binding
    /// names per combine node (`prefix` distinguishes the enum/record
    /// call sites for greppability).
    fn compare_combine(&self, prefix: &str, counter: &mut usize, left: Expr, right: Expr) -> Expr {
        let c_sym = intern(&format!("{prefix}{counter}__"));
        *counter += 1;
        let arms = vec![
            self.arm(Pattern::new(PatternKind::Int(0), self.span), right),
            self.arm(self.wildcard_pat(), self.ident_expr(c_sym)),
        ];
        self.block_expr(vec![
            self.let_stmt(c_sym, left),
            Stmt::Expr(self.match_expr(self.ident_expr(c_sym), arms)),
        ])
    }

    /// Sequence the hash combine as a flat statement list:
    ///
    /// ```silt
    /// { let __d_hc0__ = <leaf0>
    ///   let __d_hc1__ = combine(__d_hc0__, <leaf1>)
    ///   ...
    ///   __d_hc{n-1}__ }
    /// ```
    ///
    /// The hash combine `(a mod P) * 31 + (b mod P)` is **not**
    /// associative, so unlike Compare/Equal/Display it cannot be folded
    /// into a balanced tree without changing the numeric output (which
    /// tests lock). A statement sequence computes the exact same
    /// left-fold arithmetic — same values, same leaf evaluation order —
    /// while keeping expression depth O(1): block statements are walked
    /// iteratively, not recursively, by both the typechecker and the
    /// compiler.
    fn hash_combine_block(&self, prefix: &str, leaves: Vec<Expr>) -> Expr {
        debug_assert!(!leaves.is_empty(), "hash_combine_block requires >= 1 leaf");
        if leaves.len() == 1 {
            return leaves.into_iter().next().expect("len checked above");
        }
        let mut stmts: Vec<Stmt> = Vec::with_capacity(leaves.len() + 1);
        let mut prev: Option<Symbol> = None;
        for (i, leaf) in leaves.into_iter().enumerate() {
            let sym = intern(&format!("{prefix}{i}__"));
            let value = match prev {
                None => leaf,
                Some(p) => self.combine_hash_expr(self.ident_expr(p), leaf),
            };
            stmts.push(self.let_stmt(sym, value));
            prev = Some(sym);
        }
        stmts.push(Stmt::Expr(
            self.ident_expr(prev.expect("loop ran at least once")),
        ));
        self.block_expr(stmts)
    }

    fn named_te(&self, name: Symbol) -> TypeExpr {
        TypeExpr::new(
            TypeExprKind::Named {
                module: None,
                name,
                name_span: self.span,
            },
            self.span,
        )
    }

    /// Build a `TypeExpr` for the (possibly generic) type being derived.
    /// `Box(a)` → `Generic(Box, [Named(a)])`; non-generic `Color` →
    /// `Named(Color)`.
    fn type_te(&self, name: Symbol, params: &[Symbol]) -> TypeExpr {
        if params.is_empty() {
            self.named_te(name)
        } else {
            let args: Vec<TypeExpr> = params.iter().map(|p| self.named_te(*p)).collect();
            TypeExpr::new(
                TypeExprKind::Generic {
                    module: None,
                    name,
                    name_span: self.span,
                    args,
                },
                self.span,
            )
        }
    }

    /// Build a `Param { kind: Data, pattern: Ident(name), ty: Some(<ty>) }`.
    fn param(&self, name: Symbol, ty: TypeExpr) -> Param {
        Param {
            kind: ParamKind::Data,
            pattern: self.id_pat(name),
            ty: Some(ty),
        }
    }

    /// Wrap a single expression in an FnDecl with the given name, params,
    /// return type and body. All other fields are defaults.
    fn fn_decl(
        &self,
        name: Symbol,
        params: Vec<Param>,
        return_type: Option<TypeExpr>,
        body: Expr,
    ) -> FnDecl {
        FnDecl {
            name,
            params,
            return_type,
            where_clauses: Vec::new(),
            body,
            is_pub: false,
            span: self.span,
            // Synthesized: no source identifier — fall back to the
            // synthetic span so callers don't crash on a missing field.
            name_span: self.span,
            is_recovery_stub: false,
            is_signature_only: false,
            doc: None,
        }
    }

    /// Wrap synthesized FnDecls in a TraitImpl. For non-generic types
    /// (`params` empty), `target_type_args`, `target_param_names`, and
    /// `where_clauses` are all empty. For generic types
    /// (`params` non-empty), e.g. `type Box(a)`:
    /// - `target_type_args = [TypeExpr::Named("a"), ...]`
    /// - `target_param_names = ["a", ...]`
    /// - `where_clauses = [("a", trait_name, []), ...]` (each param bound
    ///   to the trait being synthesized — `where a: Compare` for Compare's
    ///   impl, etc.). Phantom params (params not used in any field) still
    ///   receive the bound for consistency: this matches Rust's auto-derive
    ///   behaviour and avoids a special case.
    /// - `is_auto_derived = true` (so a user `Display` impl can override it).
    fn trait_impl(
        &self,
        trait_name: Symbol,
        type_name: Symbol,
        params: &[Symbol],
        methods: Vec<FnDecl>,
    ) -> TraitImpl {
        let target_type_args: Vec<TypeExpr> = params.iter().map(|p| self.named_te(*p)).collect();
        let target_param_names: Vec<Symbol> = params.to_vec();
        let where_clauses: Vec<WhereClause> = params
            .iter()
            .map(|p| WhereClause {
                type_param: *p,
                trait_module: None,
                trait_name,
                trait_args: Vec::new(),
                trait_name_span: self.span,
                trait_res: None,
            })
            .collect();
        TraitImpl {
            trait_module: None,
            trait_name,
            trait_res: None,
            trait_name_span: self.span,
            trait_args: Vec::new(),
            target_module: None,
            target_type: type_name,
            target_res: None,
            target_type_span: self.span,
            target_type_args,
            target_param_names,
            where_clauses,
            methods,
            assoc_type_bindings: Vec::new(),
            span: self.span,
            is_auto_derived: true,
        }
    }

    // ── Shared scaffolds ─────────────────────────────────────────────────
    //
    // All four enum auto-derives share the same skeletal structure:
    //
    //   1. **Uninhabited fast path.** When `variants.is_empty()`, the body
    //      is the empty match `match self { }`. The exhaustiveness checker
    //      has a documented short-circuit for uninhabited scrutinees
    //      (see tests/typecheck/empty_type_match_tests.rs).
    //
    //   2. **Inhabited body.** A match expression — over the tuple
    //      `(self, other)` for binop-shaped traits (Compare, Equal) and
    //      over `self` directly for unop-shaped traits (Hash, Display).
    //      Each variant produces an arm whose body is computed by a
    //      per-trait closure. Binop traits may additionally emit a
    //      catch-all wildcard arm (only when there is more than one
    //      variant — for a single-variant enum the same-tag arm already
    //      covers every value of `(self, other)`).
    //
    //   3. **FnDecl wrap + TraitImpl wrap.** Identical for every trait,
    //      modulo method name, return type, and parameter list shape.
    //
    // The two helpers below collapse 1+2+3 into one place. The four public
    // `synth_*_impl_for_enum` entry points become small adapters that
    // supply the per-arm body computation.

    /// Build the synthetic `match self { }` body used by all four
    /// uninhabited-enum fast paths. Shared so that any future tweak to the
    /// uninhabited shape (e.g. adding a defensive panic call) lives in one
    /// place rather than four.
    fn empty_match_body(&self, self_sym: Symbol) -> Expr {
        Expr::new(
            ExprKind::Match {
                expr: Some(Box::new(self.ident_expr(self_sym))),
                arms: Vec::new(),
            },
            self.span,
        )
    }

    /// Scaffold for binop-shaped enum derives (Compare, Equal).
    ///
    /// Builds `fn <method>(self: T, other: T) -> <ret_ty> { ... }` where the
    /// body is:
    /// - `match self { }` if `variants` is empty (uninhabited fast path).
    /// - Otherwise `match (self, other) { ...same-tag arms..., (catch-all) }`.
    ///
    /// `same_tag_body` is invoked once per variant. For nullary variants
    /// the `a_names`/`b_names` slices are empty. For n-ary variants they
    /// hold the n freshly-interned identifiers used in the constructor
    /// patterns; the closure builds the arm body referring to them by name.
    ///
    /// `catch_all_body` is only consulted when `variants.len() > 1`: for a
    /// single-variant enum the same-tag arm is total over `(self, other)`
    /// and a wildcard arm would be unreachable. Returning `None` from
    /// `catch_all_body` (e.g. for a hypothetical trait that doesn't need
    /// one) skips the wildcard arm even with multiple variants — currently
    /// both Compare and Equal always supply one.
    #[allow(clippy::too_many_arguments)]
    fn synth_binop_match_enum(
        &self,
        trait_name: Symbol,
        method_name: Symbol,
        ret_ty: TypeExpr,
        type_name: Symbol,
        type_params: &[Symbol],
        variants: &[EnumVariant],
        name_prefix_a: &str,
        name_prefix_b: &str,
        same_tag_body: impl Fn(&EnumVariant, &[Symbol], &[Symbol]) -> Expr,
        catch_all_body: impl FnOnce(Symbol, Symbol, &[EnumVariant]) -> Option<Expr>,
    ) -> TraitImpl {
        let self_sym = intern("self");
        let other_sym = intern("other");
        let self_te = self.type_te(type_name, type_params);
        let make_method = |body: Expr| {
            self.fn_decl(
                method_name,
                vec![
                    self.param(self_sym, self_te.clone()),
                    self.param(other_sym, self_te.clone()),
                ],
                Some(ret_ty.clone()),
                body,
            )
        };
        if variants.is_empty() {
            let method = make_method(self.empty_match_body(self_sym));
            return self.trait_impl(trait_name, type_name, type_params, vec![method]);
        }
        let scrut = self.tuple_expr(vec![self.ident_expr(self_sym), self.ident_expr(other_sym)]);
        let mut arms: Vec<MatchArm> = Vec::new();
        for variant in variants {
            let arity = variant.fields.len();
            let a_names: Vec<Symbol> = (0..arity)
                .map(|i| intern(&format!("{name_prefix_a}{i}__")))
                .collect();
            let b_names: Vec<Symbol> = (0..arity)
                .map(|i| intern(&format!("{name_prefix_b}{i}__")))
                .collect();
            let a_pat = self.ctor_pat(
                variant.name,
                a_names.iter().map(|n| self.id_pat(*n)).collect(),
            );
            let b_pat = self.ctor_pat(
                variant.name,
                b_names.iter().map(|n| self.id_pat(*n)).collect(),
            );
            let pat = self.tuple_pat(vec![a_pat, b_pat]);
            arms.push(self.arm(pat, same_tag_body(variant, &a_names, &b_names)));
        }
        if variants.len() > 1
            && let Some(catch) = catch_all_body(self_sym, other_sym, variants)
        {
            arms.push(self.arm(self.wildcard_pat(), catch));
        }
        let body = self.match_expr(scrut, arms);
        let method = make_method(body);
        self.trait_impl(trait_name, type_name, type_params, vec![method])
    }

    /// Scaffold for unop-shaped enum derives (Hash, Display).
    ///
    /// Builds `fn <method>(self: T) -> <ret_ty> { ... }` where the body is:
    /// - `match self { }` if `variants` is empty (uninhabited fast path).
    /// - Otherwise `match self { ...one arm per variant... }`. The match
    ///   is exhaustive without a wildcard because every variant has its
    ///   own arm.
    ///
    /// `per_variant_body` is invoked once per variant with the variant
    /// metadata, the variant's declaration-order index (used for hash
    /// ordinals), and the freshly-interned arg-binding symbols (empty for
    /// nullary variants).
    #[allow(clippy::too_many_arguments)]
    fn synth_unop_match_enum(
        &self,
        trait_name: Symbol,
        method_name: Symbol,
        ret_ty: TypeExpr,
        type_name: Symbol,
        type_params: &[Symbol],
        variants: &[EnumVariant],
        name_prefix: &str,
        per_variant_body: impl Fn(usize, &EnumVariant, &[Symbol]) -> Expr,
    ) -> TraitImpl {
        let self_sym = intern("self");
        let self_te = self.type_te(type_name, type_params);
        let make_method = |body: Expr| {
            self.fn_decl(
                method_name,
                vec![self.param(self_sym, self_te.clone())],
                Some(ret_ty.clone()),
                body,
            )
        };
        if variants.is_empty() {
            let method = make_method(self.empty_match_body(self_sym));
            return self.trait_impl(trait_name, type_name, type_params, vec![method]);
        }
        let arms: Vec<MatchArm> = variants
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let arity = v.fields.len();
                let arg_names: Vec<Symbol> = (0..arity)
                    .map(|j| intern(&format!("{name_prefix}{j}__")))
                    .collect();
                let pat =
                    self.ctor_pat(v.name, arg_names.iter().map(|n| self.id_pat(*n)).collect());
                self.arm(pat, per_variant_body(i, v, &arg_names))
            })
            .collect();
        let body = self.match_expr(self.ident_expr(self_sym), arms);
        let method = make_method(body);
        self.trait_impl(trait_name, type_name, type_params, vec![method])
    }

    // ── Compare on enum ──────────────────────────────────────────────────

    /// Synthesize a `trait Compare for Enum { fn compare(self: Enum, other: Enum) -> Int { ... } }` impl.
    ///
    /// Body shape: nested match on `(self, other)`.
    /// - For each variant: same-tag arm computes lex compare of args (or 0
    ///   for nullary variants).
    /// - Catch-all arm computes ordinals via per-variant match → calls
    ///   `ord_self.compare(ord_other)`.
    pub(super) fn synth_compare_impl_for_enum(
        &self,
        type_name: Symbol,
        type_params: &[Symbol],
        variants: &[EnumVariant],
    ) -> TraitImpl {
        self.synth_binop_match_enum(
            intern("Compare"),
            intern("compare"),
            self.named_te(intern("Int")),
            type_name,
            type_params,
            variants,
            "__d_a",
            "__d_b",
            |_variant, a_names, b_names| {
                if a_names.is_empty() {
                    self.int_expr(0)
                } else {
                    self.build_lex_compare_chain(a_names, b_names)
                }
            },
            |self_sym, other_sym, variants| {
                let ord_self_sym = intern("__d_ord_self__");
                let ord_other_sym = intern("__d_ord_other__");
                let ord_self_match = self.build_ordinal_match(self.ident_expr(self_sym), variants);
                let ord_other_match =
                    self.build_ordinal_match(self.ident_expr(other_sym), variants);
                let stmts = vec![
                    self.let_stmt(ord_self_sym, ord_self_match),
                    self.let_stmt(ord_other_sym, ord_other_match),
                    Stmt::Expr(self.method_call(
                        self.ident_expr(ord_self_sym),
                        intern("compare"),
                        vec![self.ident_expr(ord_other_sym)],
                    )),
                ];
                Some(self.block_expr(stmts))
            },
        )
    }

    /// Lexicographic compare of n field pairs: first non-zero
    /// `a_i.compare(b_i)` (in declaration order) wins; all-zero → 0.
    /// Round 92: built as a *balanced* fold of the associative
    /// first-non-zero combinator (see [`compare_combine`] for the
    /// associativity/short-circuit argument) instead of the old
    /// left-leaning `match`-nest, so expression depth is O(log n) and a
    /// wide variant no longer overflows the typechecker's recursive walk.
    fn build_lex_compare_chain(&self, a_names: &[Symbol], b_names: &[Symbol]) -> Expr {
        let leaves: Vec<Expr> = a_names
            .iter()
            .zip(b_names)
            .map(|(a, b)| {
                self.method_call(
                    self.ident_expr(*a),
                    intern("compare"),
                    vec![self.ident_expr(*b)],
                )
            })
            .collect();
        let mut counter = 0usize;
        self.balanced_fold(leaves, &mut |l, r| {
            self.compare_combine("__d_c", &mut counter, l, r)
        })
    }

    /// `match scrut { V0 -> 0, V1(_, _) -> 1, ... }` — produces the
    /// declaration-order ordinal of each variant. Wildcard sub-patterns
    /// match each constructor's arity so the match is exhaustive.
    fn build_ordinal_match(&self, scrut: Expr, variants: &[EnumVariant]) -> Expr {
        let arms: Vec<MatchArm> = variants
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let sub_pats = (0..v.fields.len()).map(|_| self.wildcard_pat()).collect();
                self.arm(self.ctor_pat(v.name, sub_pats), self.int_expr(i as i64))
            })
            .collect();
        self.match_expr(scrut, arms)
    }

    // ── Equal on enum ────────────────────────────────────────────────────

    pub(super) fn synth_equal_impl_for_enum(
        &self,
        type_name: Symbol,
        type_params: &[Symbol],
        variants: &[EnumVariant],
    ) -> TraitImpl {
        self.synth_binop_match_enum(
            intern("Equal"),
            intern("equal"),
            self.named_te(intern("Bool")),
            type_name,
            type_params,
            variants,
            "__d_ea",
            "__d_eb",
            |_variant, a_names, b_names| {
                if a_names.is_empty() {
                    self.bool_expr(true)
                } else {
                    // a0.equal(b0) && a1.equal(b1) && ... — balanced
                    // `&&`-join (round 92): same left-to-right
                    // short-circuit semantics as the old left fold, but
                    // O(log n) deep so wide variants don't overflow the
                    // typechecker's recursive walk.
                    let leaves: Vec<Expr> = a_names
                        .iter()
                        .zip(b_names)
                        .map(|(a, b)| {
                            self.method_call(
                                self.ident_expr(*a),
                                intern("equal"),
                                vec![self.ident_expr(*b)],
                            )
                        })
                        .collect();
                    self.and_all(leaves)
                }
            },
            // Catch-all: false. Only emitted when there's more than one
            // variant (otherwise the same-tag arms are total, and the
            // wildcard would be unreachable).
            |_self_sym, _other_sym, _variants| Some(self.bool_expr(false)),
        )
    }

    // ── Hash on enum ─────────────────────────────────────────────────────

    /// Hash combine function — must mirror the bit-wise behavior of
    /// `impl Hash for Value` in src/value.rs (which uses
    /// `std::collections::hash_map::DefaultHasher` and writes
    /// `discriminant.hash` then each component). Because we don't have
    /// access to a structural hasher in surface syntax, we approximate by
    /// FNV-style mul-xor combining `tag_ordinal.hash()` with each arg's
    /// `.hash()` result.
    ///
    /// NOTE: This produces a *different* numeric value than the runtime
    /// `dispatch_trait_method` `"hash"` arm (which falls through to
    /// `Value::hash` directly). The synthesized hash is still:
    ///   - deterministic across runs,
    ///   - structural (same value → same hash),
    ///   - consistent with our synthesized `Equal`.
    ///
    /// Tests that compare against the old `Value::hash` numeric output
    /// must be updated. See tests/lang/auto_derive_synth_body_tests.rs for the
    /// new locked values.
    pub(super) fn synth_hash_impl_for_enum(
        &self,
        type_name: Symbol,
        type_params: &[Symbol],
        variants: &[EnumVariant],
    ) -> TraitImpl {
        self.synth_unop_match_enum(
            intern("Hash"),
            intern("hash"),
            self.named_te(intern("Int")),
            type_name,
            type_params,
            variants,
            "__d_h",
            |i, _v, arg_names| {
                // Leaves: tag.hash(), arg0.hash(), arg1.hash(), ...
                // Sequenced as a flat let-block (round 92) computing the
                // exact same left-fold combine arithmetic as before —
                // identical numeric output (locked by tests) — with O(1)
                // expression depth instead of O(arity).
                let mut leaves: Vec<Expr> =
                    vec![self.method_call(self.int_expr(i as i64), intern("hash"), vec![])];
                for arg_name in arg_names {
                    leaves.push(self.method_call(
                        self.ident_expr(*arg_name),
                        intern("hash"),
                        vec![],
                    ));
                }
                self.hash_combine_block("__d_hc", leaves)
            },
        )
    }

    /// Combine two hashes into a structural hash. Silt's surface-level
    /// arithmetic is checked (overflow → runtime error), so a
    /// straightforward `a * 31 + b` FNV-style combine blows up immediately
    /// when `a` or `b` is the i64-magnitude output of `Int.hash()` (which
    /// itself is `DefaultHasher::finish() as i64`). To stay in pure silt
    /// surface syntax without bypassing the overflow check, each side is
    /// reduced modulo a large prime first:
    ///
    ///   (a mod P) * 31 + (b mod P)
    ///
    /// `P = 1_000_003` (prime, small enough that `P * 31 ≈ 3.1e7` and
    /// `(P * 31) + P` fits in i64 with room to spare). This trades a small
    /// amount of entropy for guaranteed no-overflow on any pair of i64
    /// hashes; the result is still deterministic and structural for
    /// purposes of the `Hash` trait. Matches the behavioural contract:
    /// equal values produce equal hashes (paired with synthesized Equal).
    ///
    /// NOTE: this combine produces *different numeric output* than the
    /// pre-synthesis `Value::hash` direct-call path. Any test that locks
    /// a specific i64 hash value for a user record / variant must be
    /// updated to reflect the new synthesized output.
    fn combine_hash_expr(&self, a: Expr, b: Expr) -> Expr {
        let prime = || self.int_expr(1_000_003);
        let multiplier = self.int_expr(31);
        // a' = a mod P
        let a_mod = self.bin(a, BinOp::Mod, prime());
        // b' = b mod P
        let b_mod = self.bin(b, BinOp::Mod, prime());
        let mul = self.bin(a_mod, BinOp::Mul, multiplier);
        self.bin(mul, BinOp::Add, b_mod)
    }

    // ── Display on enum ──────────────────────────────────────────────────

    /// Detect whether `type_name` is one of the stdlib error enums whose
    /// `Error::message()` is the canonical user-facing rendering. The
    /// authoritative registry lives in `module.rs` (same one consulted by
    /// `vm::dispatch::render_stdlib_error_message`), so this stays in
    /// lock-step with the runtime-side `Value::Display` arm.
    ///
    /// Round-74 follow-up: prior to this gate, the synthesized
    /// `<EnumName>.display` body for stdlib error enums was the constructor
    /// form (e.g. `IoNotFound(nope)`), but `format!("{e}")` for the same
    /// variant was already routed through `Error::message()` (round-73f),
    /// so `e.display()` and `format!("{e}")` printed different text — the
    /// dual-shape bug the round-73f fix was meant to close. By delegating
    /// `display(self)` to `self.message()` for stdlib error enums, both
    /// shapes converge on the same rendering.
    fn is_stdlib_error_enum(&self, type_name: Symbol) -> bool {
        let name = crate::intern::resolve(type_name);
        crate::module::builtin_error_enum_variants_with_arity()
            .iter()
            .any(|(enum_name, _)| *enum_name == name.as_str())
    }

    pub(super) fn synth_display_impl_for_enum(
        &self,
        type_name: Symbol,
        type_params: &[Symbol],
        variants: &[EnumVariant],
    ) -> TraitImpl {
        // Round-74 fix: stdlib error enums route `display(self)` through
        // `self.message()` so `e.display()` matches `format!("{e}")` (which
        // is also routed through `message()` via the
        // `render_stdlib_error_message` arm in `value.rs::Display`). User
        // enums keep the constructor-form body.
        if self.is_stdlib_error_enum(type_name) {
            let self_sym = intern("self");
            let self_te = self.type_te(type_name, type_params);
            let body = self.method_call(self.ident_expr(self_sym), intern("message"), vec![]);
            let method = self.fn_decl(
                intern("display"),
                vec![self.param(self_sym, self_te)],
                Some(self.named_te(intern("String"))),
                body,
            );
            return self.trait_impl(intern("Display"), type_name, type_params, vec![method]);
        }
        self.synth_unop_match_enum(
            intern("Display"),
            intern("display"),
            self.named_te(intern("String")),
            type_name,
            type_params,
            variants,
            "__d_d",
            |_i, v, arg_names| {
                let tag_name = crate::intern::resolve(v.name);
                if arg_names.is_empty() {
                    self.string_expr(&tag_name)
                } else {
                    // `Tag(<a0>, <a1>, ...)` from each argument's
                    // `.display()` — one interpolation, flat regardless of
                    // arity.
                    let mut pieces: Vec<Expr> =
                        vec![self.string_expr(&tag_name), self.string_expr("(")];
                    for (i, arg_name) in arg_names.iter().enumerate() {
                        if i > 0 {
                            pieces.push(self.string_expr(", "));
                        }
                        pieces.push(self.method_call(
                            self.ident_expr(*arg_name),
                            intern("display"),
                            vec![],
                        ));
                    }
                    pieces.push(self.string_expr(")"));
                    self.concat_all(pieces)
                }
            },
        )
    }

    // ── Shared record-side scaffolds ─────────────────────────────────────
    //
    // The record-side mirror of the enum-side `synth_binop_match_enum` /
    // `synth_unop_match_enum` collapse (round 69). All four record-side
    // auto-derives share the same skeleton:
    //
    //   1. **Empty-body fast path.** When `fields.is_empty()`, the body is
    //      a per-trait literal: `0` for Compare, `true` for Equal, `0` for
    //      Hash, `"Name {}"` for Display.
    //
    //   2. **Inhabited body.** A per-trait expression built over the field
    //      list — typically a left-fold of `self.f.method(other.f)` /
    //      `self.f.method()` for Compare / Equal / Hash / Display.
    //
    //   3. **FnDecl wrap + TraitImpl wrap.** Identical for every trait,
    //      modulo method name, return type, and parameter list shape
    //      (`(self, other)` for the binop traits, `(self)` for the unop
    //      traits).
    //
    // The two helpers below collapse 1+2+3 into one place. The four public
    // `synth_*_impl_for_record` entry points become small adapters that
    // supply the empty-body literal and the per-trait body builder.

    /// Scaffold for binop-shaped record derives (Compare, Equal).
    ///
    /// Builds `fn <method>(self: T, other: T) -> <ret_ty> { ... }` where the
    /// body is `empty_body` when `fields` is empty, otherwise the result of
    /// `full_body(self_sym, other_sym, fields)`.
    #[allow(clippy::too_many_arguments)]
    fn synth_binop_record_impl(
        &self,
        trait_name: Symbol,
        method_name: Symbol,
        ret_ty: TypeExpr,
        type_name: Symbol,
        type_params: &[Symbol],
        fields: &[RecordField],
        empty_body: Expr,
        full_body: impl FnOnce(Symbol, Symbol, &[RecordField]) -> Expr,
    ) -> TraitImpl {
        let self_sym = intern("self");
        let other_sym = intern("other");
        let self_te = self.type_te(type_name, type_params);
        let body = if fields.is_empty() {
            empty_body
        } else {
            full_body(self_sym, other_sym, fields)
        };
        let method = self.fn_decl(
            method_name,
            vec![
                self.param(self_sym, self_te.clone()),
                self.param(other_sym, self_te),
            ],
            Some(ret_ty),
            body,
        );
        self.trait_impl(trait_name, type_name, type_params, vec![method])
    }

    /// Scaffold for unop-shaped record derives (Hash, Display).
    ///
    /// Builds `fn <method>(self: T) -> <ret_ty> { ... }` where the body is
    /// `empty_body` when `fields` is empty, otherwise the result of
    /// `full_body(self_sym, fields)`.
    #[allow(clippy::too_many_arguments)]
    fn synth_unop_record_impl(
        &self,
        trait_name: Symbol,
        method_name: Symbol,
        ret_ty: TypeExpr,
        type_name: Symbol,
        type_params: &[Symbol],
        fields: &[RecordField],
        empty_body: Expr,
        full_body: impl FnOnce(Symbol, &[RecordField]) -> Expr,
    ) -> TraitImpl {
        let self_sym = intern("self");
        let self_te = self.type_te(type_name, type_params);
        let body = if fields.is_empty() {
            empty_body
        } else {
            full_body(self_sym, fields)
        };
        let method = self.fn_decl(
            method_name,
            vec![self.param(self_sym, self_te)],
            Some(ret_ty),
            body,
        );
        self.trait_impl(trait_name, type_name, type_params, vec![method])
    }

    // ── Compare on record ────────────────────────────────────────────────

    pub(super) fn synth_compare_impl_for_record(
        &self,
        type_name: Symbol,
        type_params: &[Symbol],
        fields: &[RecordField],
    ) -> TraitImpl {
        self.synth_binop_record_impl(
            intern("Compare"),
            intern("compare"),
            self.named_te(intern("Int")),
            type_name,
            type_params,
            fields,
            // No fields → all instances are equal under Compare.
            self.int_expr(0),
            |a, b, fields| self.build_record_lex_compare(a, b, fields),
        )
    }

    /// Lexicographic compare of record fields in declaration order.
    /// Round 92: balanced fold of the associative first-non-zero
    /// combinator (see [`compare_combine`]) — same "first non-equal field
    /// wins" semantics and short-circuit order as the old left-leaning
    /// `match`-nest, but O(log n) deep, so a wide record (70+, 500 fields)
    /// no longer overflows the typechecker's recursive walk on `check`.
    fn build_record_lex_compare(
        &self,
        self_sym: Symbol,
        other_sym: Symbol,
        fields: &[RecordField],
    ) -> Expr {
        let leaves: Vec<Expr> = fields
            .iter()
            .map(|f| {
                self.method_call(
                    self.field_access(self.ident_expr(self_sym), f.name),
                    intern("compare"),
                    vec![self.field_access(self.ident_expr(other_sym), f.name)],
                )
            })
            .collect();
        let mut counter = 0usize;
        self.balanced_fold(leaves, &mut |l, r| {
            self.compare_combine("__d_rc", &mut counter, l, r)
        })
    }

    // ── Equal on record ──────────────────────────────────────────────────

    fn build_record_equal_chain(
        &self,
        self_sym: Symbol,
        other_sym: Symbol,
        fields: &[RecordField],
    ) -> Expr {
        // self.f0.equal(other.f0) && self.f1.equal(other.f1) && ...
        // Balanced `&&`-join (round 92): same left-to-right short-circuit
        // semantics as a left fold, O(log n) deep.
        let pair_eq = |f: &RecordField| {
            self.method_call(
                self.field_access(self.ident_expr(self_sym), f.name),
                intern("equal"),
                vec![self.field_access(self.ident_expr(other_sym), f.name)],
            )
        };
        self.and_all(fields.iter().map(pair_eq).collect())
    }

    pub(super) fn synth_equal_impl_for_record(
        &self,
        type_name: Symbol,
        type_params: &[Symbol],
        fields: &[RecordField],
    ) -> TraitImpl {
        self.synth_binop_record_impl(
            intern("Equal"),
            intern("equal"),
            self.named_te(intern("Bool")),
            type_name,
            type_params,
            fields,
            self.bool_expr(true),
            |a, b, fields| self.build_record_equal_chain(a, b, fields),
        )
    }

    // ── Hash on record ───────────────────────────────────────────────────

    fn build_record_hash_combine(&self, self_sym: Symbol, fields: &[RecordField]) -> Expr {
        // h0.combine(h1).combine(h2)... where h0 = self.f0.hash().
        // Sequenced as a flat let-block (round 92): exact same left-fold
        // combine arithmetic — identical numeric output (locked by tests)
        // — with O(1) expression depth instead of O(n).
        let field_hash = |f: &RecordField| {
            self.method_call(
                self.field_access(self.ident_expr(self_sym), f.name),
                intern("hash"),
                vec![],
            )
        };
        self.hash_combine_block("__d_rhc", fields.iter().map(field_hash).collect())
    }

    pub(super) fn synth_hash_impl_for_record(
        &self,
        type_name: Symbol,
        type_params: &[Symbol],
        fields: &[RecordField],
    ) -> TraitImpl {
        self.synth_unop_record_impl(
            intern("Hash"),
            intern("hash"),
            self.named_te(intern("Int")),
            type_name,
            type_params,
            fields,
            self.int_expr(0),
            |a, fields| self.build_record_hash_combine(a, fields),
        )
    }

    // ── Display on record ────────────────────────────────────────────────

    fn build_record_display_concat(
        &self,
        name_str: &str,
        self_sym: Symbol,
        fields: &[RecordField],
    ) -> Expr {
        // `Name { f0: <f0>, f1: <f1> }` from each field's `.display()` —
        // one interpolation, flat regardless of width.
        let mut pieces: Vec<Expr> = vec![self.string_expr(&format!("{name_str} {{ "))];
        for (i, f) in fields.iter().enumerate() {
            if i > 0 {
                pieces.push(self.string_expr(", "));
            }
            let field_str = crate::intern::resolve(f.name);
            pieces.push(self.string_expr(&format!("{field_str}: ")));
            pieces.push(self.method_call(
                self.field_access(self.ident_expr(self_sym), f.name),
                intern("display"),
                vec![],
            ));
        }
        pieces.push(self.string_expr(" }"));
        self.concat_all(pieces)
    }

    pub(super) fn synth_display_impl_for_record(
        &self,
        type_name: Symbol,
        type_params: &[Symbol],
        fields: &[RecordField],
    ) -> TraitImpl {
        let name_str = crate::intern::resolve(type_name);
        // Empty-body literal: "Name {}". Computed up front because the
        // unop-record scaffold takes a plain `Expr` for the empty case.
        let empty_body = self.string_expr(&format!("{name_str} {{}}"));
        self.synth_unop_record_impl(
            intern("Display"),
            intern("display"),
            self.named_te(intern("String")),
            type_name,
            type_params,
            fields,
            empty_body,
            |self_sym, fields| self.build_record_display_concat(&name_str, self_sym, fields),
        )
    }
}
