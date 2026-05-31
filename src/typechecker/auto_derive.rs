//! Auto-derive synthesis for built-in traits on user-declared types.
//!
//! For every user enum or record without a manual `trait <X> for T` impl,
//! we synthesize a `TraitImpl` AST node for each of Display, Compare,
//! Equal, and Hash. The synthesized impl's method bodies are real silt
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
//!   fn compare(self: Color, other: Color) -> Int = match (self, other) {
//!     (Red, Red) -> 0
//!     (Green(xa), Green(xb)) -> xa.compare(xb)
//!     (Blue(a1, a2), Blue(b1, b2)) -> {
//!         let c1 = a1.compare(b1)
//!         match c1 { 0 -> a2.compare(b2), _ -> c1 }
//!     }
//!     _ -> {
//!         let ord_self = match self { Red -> 0, Green(_) -> 1, Blue(_, _) -> 2 }
//!         let ord_other = match other { Red -> 0, Green(_) -> 1, Blue(_, _) -> 2 }
//!         ord_self.compare(ord_other)
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
use crate::lexer::Span;

// Auto-derived AST nodes use the canonical `Span::synthetic()` shape
// (line 0 / col 0 / offset 0). This signals "compiler-synthesized" to
// the diagnostic renderer, which suppresses the source-line-with-caret
// display. The same zero-span shape is shared with exhaustiveness, CLI
// helpers, bytecode/disassemble and other internal call sites — it is
// NOT unique to auto-derive, so it cannot be used as a grep-sentinel
// to identify auto-derive diagnostics specifically.

fn id_pat(name: Symbol) -> Pattern {
    Pattern::new(PatternKind::Ident(name), Span::synthetic())
}

fn wildcard_pat() -> Pattern {
    Pattern::new(PatternKind::Wildcard, Span::synthetic())
}

fn ctor_pat(name: Symbol, args: Vec<Pattern>) -> Pattern {
    Pattern::new(PatternKind::Constructor(name, args), Span::synthetic())
}

fn tuple_pat(elems: Vec<Pattern>) -> Pattern {
    Pattern::new(PatternKind::Tuple(elems), Span::synthetic())
}

fn ident_expr(name: Symbol) -> Expr {
    Expr::new(ExprKind::Ident(name), Span::synthetic())
}

fn int_expr(n: i64) -> Expr {
    Expr::new(ExprKind::Int(n), Span::synthetic())
}

fn bool_expr(b: bool) -> Expr {
    Expr::new(ExprKind::Bool(b), Span::synthetic())
}

fn string_expr(s: &str) -> Expr {
    Expr::new(ExprKind::StringLit(s.to_string(), false), Span::synthetic())
}

fn tuple_expr(elems: Vec<Expr>) -> Expr {
    Expr::new(ExprKind::Tuple(elems), Span::synthetic())
}

/// `recv.method(args...)` — used to emit `xa.compare(xb)`,
/// `self.x.equal(other.x)`, etc. The implementation is `Call(FieldAccess)`
/// because that is what the parser produces for surface-syntax method
/// calls.
fn method_call(recv: Expr, method: Symbol, args: Vec<Expr>) -> Expr {
    let fa = Expr::new(
        ExprKind::FieldAccess(Box::new(recv), method),
        Span::synthetic(),
    );
    Expr::new(ExprKind::Call(Box::new(fa), args), Span::synthetic())
}

/// `recv.field` — record field access.
fn field_access(recv: Expr, field: Symbol) -> Expr {
    Expr::new(
        ExprKind::FieldAccess(Box::new(recv), field),
        Span::synthetic(),
    )
}

/// `a + b` — used to combine display strings.
fn bin(a: Expr, op: BinOp, b: Expr) -> Expr {
    Expr::new(
        ExprKind::Binary(Box::new(a), op, Box::new(b)),
        Span::synthetic(),
    )
}

fn match_expr(scrut: Expr, arms: Vec<MatchArm>) -> Expr {
    Expr::new(
        ExprKind::Match {
            expr: Some(Box::new(scrut)),
            arms,
        },
        Span::synthetic(),
    )
}

fn arm(pattern: Pattern, body: Expr) -> MatchArm {
    MatchArm {
        pattern,
        guard: None,
        body,
    }
}

fn block_expr(stmts: Vec<Stmt>) -> Expr {
    Expr::new(ExprKind::Block(stmts), Span::synthetic())
}

fn let_stmt(name: Symbol, value: Expr) -> Stmt {
    Stmt::Let {
        pattern: id_pat(name),
        ty: None,
        value,
    }
}

fn named_te(name: Symbol) -> TypeExpr {
    TypeExpr::new(TypeExprKind::Named(name), Span::synthetic())
}

/// Build a `TypeExpr` for the (possibly generic) type being derived.
/// `Box(a)` → `Generic(Box, [Named(a)])`; non-generic `Color` →
/// `Named(Color)`.
fn type_te(name: Symbol, params: &[Symbol]) -> TypeExpr {
    if params.is_empty() {
        named_te(name)
    } else {
        let args: Vec<TypeExpr> = params.iter().map(|p| named_te(*p)).collect();
        TypeExpr::new(TypeExprKind::Generic(name, args), Span::synthetic())
    }
}

/// Build a `Param { kind: Data, pattern: Ident(name), ty: Some(<ty>) }`.
fn param(name: Symbol, ty: TypeExpr) -> Param {
    Param {
        kind: ParamKind::Data,
        pattern: id_pat(name),
        ty: Some(ty),
    }
}

/// Wrap a single expression in an FnDecl with the given name, params,
/// return type and body. All other fields are defaults.
fn fn_decl(name: Symbol, params: Vec<Param>, return_type: Option<TypeExpr>, body: Expr) -> FnDecl {
    FnDecl {
        name,
        params,
        return_type,
        where_clauses: Vec::new(),
        body,
        is_pub: false,
        span: Span::synthetic(),
        // Synthesized: no source identifier — fall back to the
        // synthetic span so callers don't crash on a missing field.
        name_span: Span::synthetic(),
        is_recovery_stub: false,
        is_signature_only: false,
        doc: None,
        // Auto-derived methods (Display / Compare / Equal / Hash on
        // user records and enums) carry the gradual-rollout `TOP`
        // default so they don't fail enforcement when callers happen
        // to declare a tighter set on the calling fn. Not user-
        // annotated.
        declared_effects: crate::types::effects::EffectSet::TOP,
        is_annotated: false,
        inferred_effects: None,
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
/// - `is_auto_derived = true` (so user impls can override).
fn trait_impl(
    trait_name: Symbol,
    type_name: Symbol,
    params: &[Symbol],
    methods: Vec<FnDecl>,
) -> TraitImpl {
    let target_type_args: Vec<TypeExpr> = params.iter().map(|p| named_te(*p)).collect();
    let target_param_names: Vec<Symbol> = params.to_vec();
    let where_clauses: Vec<WhereClause> = params
        .iter()
        .map(|p| WhereClause {
            type_param: *p,
            trait_name,
            trait_args: Vec::new(),
            trait_name_span: Span::synthetic(),
        })
        .collect();
    TraitImpl {
        trait_name,
        trait_name_span: Span::synthetic(),
        trait_args: Vec::new(),
        target_type: type_name,
        target_type_span: Span::synthetic(),
        target_type_args,
        target_param_names,
        where_clauses,
        methods,
        assoc_type_bindings: Vec::new(),
        span: Span::synthetic(),
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
//      (see tests/empty_type_match_tests.rs).
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
fn empty_match_body(self_sym: Symbol) -> Expr {
    Expr::new(
        ExprKind::Match {
            expr: Some(Box::new(ident_expr(self_sym))),
            arms: Vec::new(),
        },
        Span::synthetic(),
    )
}

/// Scaffold for binop-shaped enum derives (Compare, Equal).
///
/// Builds `fn <method>(self: T, other: T) -> <ret_ty> = ...` where the
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
    let self_te = type_te(type_name, type_params);
    let make_method = |body: Expr| {
        fn_decl(
            method_name,
            vec![
                param(self_sym, self_te.clone()),
                param(other_sym, self_te.clone()),
            ],
            Some(ret_ty.clone()),
            body,
        )
    };
    if variants.is_empty() {
        let method = make_method(empty_match_body(self_sym));
        return trait_impl(trait_name, type_name, type_params, vec![method]);
    }
    let scrut = tuple_expr(vec![ident_expr(self_sym), ident_expr(other_sym)]);
    let mut arms: Vec<MatchArm> = Vec::new();
    for variant in variants {
        let arity = variant.fields.len();
        let a_names: Vec<Symbol> = (0..arity)
            .map(|i| intern(&format!("{name_prefix_a}{i}__")))
            .collect();
        let b_names: Vec<Symbol> = (0..arity)
            .map(|i| intern(&format!("{name_prefix_b}{i}__")))
            .collect();
        let a_pat = ctor_pat(variant.name, a_names.iter().map(|n| id_pat(*n)).collect());
        let b_pat = ctor_pat(variant.name, b_names.iter().map(|n| id_pat(*n)).collect());
        let pat = tuple_pat(vec![a_pat, b_pat]);
        arms.push(arm(pat, same_tag_body(variant, &a_names, &b_names)));
    }
    if variants.len() > 1
        && let Some(catch) = catch_all_body(self_sym, other_sym, variants)
    {
        arms.push(arm(wildcard_pat(), catch));
    }
    let body = match_expr(scrut, arms);
    let method = make_method(body);
    trait_impl(trait_name, type_name, type_params, vec![method])
}

/// Scaffold for unop-shaped enum derives (Hash, Display).
///
/// Builds `fn <method>(self: T) -> <ret_ty> = ...` where the body is:
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
    let self_te = type_te(type_name, type_params);
    let make_method = |body: Expr| {
        fn_decl(
            method_name,
            vec![param(self_sym, self_te.clone())],
            Some(ret_ty.clone()),
            body,
        )
    };
    if variants.is_empty() {
        let method = make_method(empty_match_body(self_sym));
        return trait_impl(trait_name, type_name, type_params, vec![method]);
    }
    let arms: Vec<MatchArm> = variants
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let arity = v.fields.len();
            let arg_names: Vec<Symbol> = (0..arity)
                .map(|j| intern(&format!("{name_prefix}{j}__")))
                .collect();
            let pat = ctor_pat(v.name, arg_names.iter().map(|n| id_pat(*n)).collect());
            arm(pat, per_variant_body(i, v, &arg_names))
        })
        .collect();
    let body = match_expr(ident_expr(self_sym), arms);
    let method = make_method(body);
    trait_impl(trait_name, type_name, type_params, vec![method])
}

// ── Compare on enum ──────────────────────────────────────────────────

/// Synthesize a `trait Compare for Enum { fn compare(self: Enum, other: Enum) -> Int = ... }` impl.
///
/// Body shape: nested match on `(self, other)`.
/// - For each variant: same-tag arm computes lex compare of args (or 0
///   for nullary variants).
/// - Catch-all arm computes ordinals via per-variant match → calls
///   `ord_self.compare(ord_other)`.
pub(super) fn synth_compare_impl_for_enum(
    type_name: Symbol,
    type_params: &[Symbol],
    variants: &[EnumVariant],
) -> TraitImpl {
    synth_binop_match_enum(
        intern("Compare"),
        intern("compare"),
        named_te(intern("Int")),
        type_name,
        type_params,
        variants,
        "__d_a",
        "__d_b",
        |_variant, a_names, b_names| {
            if a_names.is_empty() {
                int_expr(0)
            } else {
                build_lex_compare_chain(a_names, b_names)
            }
        },
        |self_sym, other_sym, variants| {
            let ord_self_sym = intern("__d_ord_self__");
            let ord_other_sym = intern("__d_ord_other__");
            let ord_self_match = build_ordinal_match(ident_expr(self_sym), variants);
            let ord_other_match = build_ordinal_match(ident_expr(other_sym), variants);
            let stmts = vec![
                let_stmt(ord_self_sym, ord_self_match),
                let_stmt(ord_other_sym, ord_other_match),
                Stmt::Expr(method_call(
                    ident_expr(ord_self_sym),
                    intern("compare"),
                    vec![ident_expr(ord_other_sym)],
                )),
            ];
            Some(block_expr(stmts))
        },
    )
}

/// Build `let c0 = a0.compare(b0); match c0 { 0 -> let c1 = a1.compare(b1); ..., _ -> c0 }`
/// for n field pairs. Recurses through positions; the innermost call is
/// `a_{n-1}.compare(b_{n-1})`.
fn build_lex_compare_chain(a_names: &[Symbol], b_names: &[Symbol]) -> Expr {
    fn chain(a_names: &[Symbol], b_names: &[Symbol], i: usize) -> Expr {
        let cur = method_call(
            ident_expr(a_names[i]),
            intern("compare"),
            vec![ident_expr(b_names[i])],
        );
        if i + 1 == a_names.len() {
            return cur;
        }
        let c_sym = intern(&format!("__d_c{i}__"));
        let inner = chain(a_names, b_names, i + 1);
        let arms = vec![
            arm(Pattern::new(PatternKind::Int(0), Span::synthetic()), inner),
            arm(wildcard_pat(), ident_expr(c_sym)),
        ];
        block_expr(vec![
            let_stmt(c_sym, cur),
            Stmt::Expr(match_expr(ident_expr(c_sym), arms)),
        ])
    }
    chain(a_names, b_names, 0)
}

/// `match scrut { V0 -> 0, V1(_, _) -> 1, ... }` — produces the
/// declaration-order ordinal of each variant. Wildcard sub-patterns
/// match each constructor's arity so the match is exhaustive.
fn build_ordinal_match(scrut: Expr, variants: &[EnumVariant]) -> Expr {
    let arms: Vec<MatchArm> = variants
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let sub_pats = (0..v.fields.len()).map(|_| wildcard_pat()).collect();
            arm(ctor_pat(v.name, sub_pats), int_expr(i as i64))
        })
        .collect();
    match_expr(scrut, arms)
}

// ── Equal on enum ────────────────────────────────────────────────────

pub(super) fn synth_equal_impl_for_enum(
    type_name: Symbol,
    type_params: &[Symbol],
    variants: &[EnumVariant],
) -> TraitImpl {
    synth_binop_match_enum(
        intern("Equal"),
        intern("equal"),
        named_te(intern("Bool")),
        type_name,
        type_params,
        variants,
        "__d_ea",
        "__d_eb",
        |_variant, a_names, b_names| {
            if a_names.is_empty() {
                bool_expr(true)
            } else {
                // a0.equal(b0) && a1.equal(b1) && ... — left-fold
                let mut chain: Expr = method_call(
                    ident_expr(a_names[0]),
                    intern("equal"),
                    vec![ident_expr(b_names[0])],
                );
                for i in 1..a_names.len() {
                    let next = method_call(
                        ident_expr(a_names[i]),
                        intern("equal"),
                        vec![ident_expr(b_names[i])],
                    );
                    chain = bin(chain, BinOp::And, next);
                }
                chain
            }
        },
        // Catch-all: false. Only emitted when there's more than one
        // variant (otherwise the same-tag arms are total, and the
        // wildcard would be unreachable).
        |_self_sym, _other_sym, _variants| Some(bool_expr(false)),
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
/// must be updated. See tests/auto_derive_synth_body_tests.rs for the
/// new locked values.
pub(super) fn synth_hash_impl_for_enum(
    type_name: Symbol,
    type_params: &[Symbol],
    variants: &[EnumVariant],
) -> TraitImpl {
    synth_unop_match_enum(
        intern("Hash"),
        intern("hash"),
        named_te(intern("Int")),
        type_name,
        type_params,
        variants,
        "__d_h",
        |i, _v, arg_names| {
            // tag = i; combine = tag.hash(); for each arg:
            //   combine = combine_hash(combine, arg.hash())
            let mut combine: Expr = method_call(int_expr(i as i64), intern("hash"), vec![]);
            for arg_name in arg_names {
                let arg_hash = method_call(ident_expr(*arg_name), intern("hash"), vec![]);
                combine = combine_hash_expr(combine, arg_hash);
            }
            combine
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
fn combine_hash_expr(a: Expr, b: Expr) -> Expr {
    let prime = || int_expr(1_000_003);
    let multiplier = int_expr(31);
    // a' = a mod P
    let a_mod = bin(a, BinOp::Mod, prime());
    // b' = b mod P
    let b_mod = bin(b, BinOp::Mod, prime());
    let mul = bin(a_mod, BinOp::Mul, multiplier);
    bin(mul, BinOp::Add, b_mod)
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
fn is_stdlib_error_enum(type_name: Symbol) -> bool {
    let name = crate::intern::resolve(type_name);
    crate::module::builtin_error_enum_variants_with_arity()
        .iter()
        .any(|(enum_name, _)| *enum_name == name.as_str())
}

pub(super) fn synth_display_impl_for_enum(
    type_name: Symbol,
    type_params: &[Symbol],
    variants: &[EnumVariant],
) -> TraitImpl {
    // Round-74 fix: stdlib error enums route `display(self)` through
    // `self.message()` so `e.display()` matches `format!("{e}")` (which
    // is also routed through `message()` via the
    // `render_stdlib_error_message` arm in `value.rs::Display`). User
    // enums keep the constructor-form body.
    if is_stdlib_error_enum(type_name) {
        let self_sym = intern("self");
        let self_te = type_te(type_name, type_params);
        let body = method_call(ident_expr(self_sym), intern("message"), vec![]);
        let method = fn_decl(
            intern("display"),
            vec![param(self_sym, self_te)],
            Some(named_te(intern("String"))),
            body,
        );
        return trait_impl(intern("Display"), type_name, type_params, vec![method]);
    }
    synth_unop_match_enum(
        intern("Display"),
        intern("display"),
        named_te(intern("String")),
        type_name,
        type_params,
        variants,
        "__d_d",
        |_i, v, arg_names| {
            let tag_name = crate::intern::resolve(v.name);
            if arg_names.is_empty() {
                string_expr(&tag_name)
            } else {
                // "Tag(" + a0.display() + ", " + a1.display() + ... + ")"
                let mut acc = bin(string_expr(&tag_name), BinOp::Add, string_expr("("));
                for (i, arg_name) in arg_names.iter().enumerate() {
                    if i > 0 {
                        acc = bin(acc, BinOp::Add, string_expr(", "));
                    }
                    acc = bin(
                        acc,
                        BinOp::Add,
                        method_call(ident_expr(*arg_name), intern("display"), vec![]),
                    );
                }
                bin(acc, BinOp::Add, string_expr(")"))
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
/// Builds `fn <method>(self: T, other: T) -> <ret_ty> = ...` where the
/// body is `empty_body` when `fields` is empty, otherwise the result of
/// `full_body(self_sym, other_sym, fields)`.
#[allow(clippy::too_many_arguments)]
fn synth_binop_record_impl(
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
    let self_te = type_te(type_name, type_params);
    let body = if fields.is_empty() {
        empty_body
    } else {
        full_body(self_sym, other_sym, fields)
    };
    let method = fn_decl(
        method_name,
        vec![param(self_sym, self_te.clone()), param(other_sym, self_te)],
        Some(ret_ty),
        body,
    );
    trait_impl(trait_name, type_name, type_params, vec![method])
}

/// Scaffold for unop-shaped record derives (Hash, Display).
///
/// Builds `fn <method>(self: T) -> <ret_ty> = ...` where the body is
/// `empty_body` when `fields` is empty, otherwise the result of
/// `full_body(self_sym, fields)`.
#[allow(clippy::too_many_arguments)]
fn synth_unop_record_impl(
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
    let self_te = type_te(type_name, type_params);
    let body = if fields.is_empty() {
        empty_body
    } else {
        full_body(self_sym, fields)
    };
    let method = fn_decl(
        method_name,
        vec![param(self_sym, self_te)],
        Some(ret_ty),
        body,
    );
    trait_impl(trait_name, type_name, type_params, vec![method])
}

// ── Compare on record ────────────────────────────────────────────────

pub(super) fn synth_compare_impl_for_record(
    type_name: Symbol,
    type_params: &[Symbol],
    fields: &[RecordField],
) -> TraitImpl {
    synth_binop_record_impl(
        intern("Compare"),
        intern("compare"),
        named_te(intern("Int")),
        type_name,
        type_params,
        fields,
        // No fields → all instances are equal under Compare.
        int_expr(0),
        build_record_lex_compare,
    )
}

fn build_record_lex_compare(self_sym: Symbol, other_sym: Symbol, fields: &[RecordField]) -> Expr {
    fn rec(self_sym: Symbol, other_sym: Symbol, fields: &[RecordField], i: usize) -> Expr {
        let cur = method_call(
            field_access(ident_expr(self_sym), fields[i].name),
            intern("compare"),
            vec![field_access(ident_expr(other_sym), fields[i].name)],
        );
        if i + 1 == fields.len() {
            return cur;
        }
        let c_sym = intern(&format!("__d_rc{i}__"));
        let arms = vec![
            arm(
                Pattern::new(PatternKind::Int(0), Span::synthetic()),
                rec(self_sym, other_sym, fields, i + 1),
            ),
            arm(wildcard_pat(), ident_expr(c_sym)),
        ];
        block_expr(vec![
            let_stmt(c_sym, cur),
            Stmt::Expr(match_expr(ident_expr(c_sym), arms)),
        ])
    }
    rec(self_sym, other_sym, fields, 0)
}

// ── Equal on record ──────────────────────────────────────────────────

fn build_record_equal_chain(self_sym: Symbol, other_sym: Symbol, fields: &[RecordField]) -> Expr {
    // self.f0.equal(other.f0) && self.f1.equal(other.f1) && ...
    let pair_eq = |f: &RecordField| {
        method_call(
            field_access(ident_expr(self_sym), f.name),
            intern("equal"),
            vec![field_access(ident_expr(other_sym), f.name)],
        )
    };
    let mut chain: Expr = pair_eq(&fields[0]);
    for f in &fields[1..] {
        chain = bin(chain, BinOp::And, pair_eq(f));
    }
    chain
}

pub(super) fn synth_equal_impl_for_record(
    type_name: Symbol,
    type_params: &[Symbol],
    fields: &[RecordField],
) -> TraitImpl {
    synth_binop_record_impl(
        intern("Equal"),
        intern("equal"),
        named_te(intern("Bool")),
        type_name,
        type_params,
        fields,
        bool_expr(true),
        build_record_equal_chain,
    )
}

// ── Hash on record ───────────────────────────────────────────────────

fn build_record_hash_combine(self_sym: Symbol, fields: &[RecordField]) -> Expr {
    // h0.combine(h1).combine(h2)... where h0 = self.f0.hash()
    let field_hash = |f: &RecordField| {
        method_call(
            field_access(ident_expr(self_sym), f.name),
            intern("hash"),
            vec![],
        )
    };
    let mut combine: Expr = field_hash(&fields[0]);
    for f in &fields[1..] {
        combine = combine_hash_expr(combine, field_hash(f));
    }
    combine
}

pub(super) fn synth_hash_impl_for_record(
    type_name: Symbol,
    type_params: &[Symbol],
    fields: &[RecordField],
) -> TraitImpl {
    synth_unop_record_impl(
        intern("Hash"),
        intern("hash"),
        named_te(intern("Int")),
        type_name,
        type_params,
        fields,
        int_expr(0),
        build_record_hash_combine,
    )
}

// ── Display on record ────────────────────────────────────────────────

fn build_record_display_concat(name_str: &str, self_sym: Symbol, fields: &[RecordField]) -> Expr {
    // "Name { f0: " + self.f0.display() + ", f1: " + self.f1.display() + " }"
    let mut acc = string_expr(&format!("{name_str} {{ "));
    for (i, f) in fields.iter().enumerate() {
        if i > 0 {
            acc = bin(acc, BinOp::Add, string_expr(", "));
        }
        let field_str = crate::intern::resolve(f.name);
        acc = bin(acc, BinOp::Add, string_expr(&format!("{field_str}: ")));
        acc = bin(
            acc,
            BinOp::Add,
            method_call(
                field_access(ident_expr(self_sym), f.name),
                intern("display"),
                vec![],
            ),
        );
    }
    bin(acc, BinOp::Add, string_expr(" }"))
}

pub(super) fn synth_display_impl_for_record(
    type_name: Symbol,
    type_params: &[Symbol],
    fields: &[RecordField],
) -> TraitImpl {
    let name_str = crate::intern::resolve(type_name);
    // Empty-body literal: "Name {}". Computed up front because the
    // unop-record scaffold takes a plain `Expr` for the empty case.
    let empty_body = string_expr(&format!("{name_str} {{}}"));
    synth_unop_record_impl(
        intern("Display"),
        intern("display"),
        named_te(intern("String")),
        type_name,
        type_params,
        fields,
        empty_body,
        |self_sym, fields| build_record_display_concat(&name_str, self_sym, fields),
    )
}
