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

/// Synthetic span used by every auto-derived AST node. Pointing at
/// `(0, 0, 0)` keeps any diagnostic raised against synthesized code
/// distinguishable from a real user-source location: line 0 / col 0 /
/// offset 0 is impossible for any character read by the lexer (every
/// real token starts at line 1+). Test helpers and audits can grep for
/// this sentinel to confirm a diagnostic is auto-derive-related.
fn synth_span() -> Span {
    Span {
        line: 0,
        col: 0,
        offset: 0,
    }
}

fn id_pat(name: Symbol) -> Pattern {
    Pattern::new(PatternKind::Ident(name), synth_span())
}

fn wildcard_pat() -> Pattern {
    Pattern::new(PatternKind::Wildcard, synth_span())
}

fn ctor_pat(name: Symbol, args: Vec<Pattern>) -> Pattern {
    Pattern::new(PatternKind::Constructor(name, args), synth_span())
}

fn tuple_pat(elems: Vec<Pattern>) -> Pattern {
    Pattern::new(PatternKind::Tuple(elems), synth_span())
}

fn ident_expr(name: Symbol) -> Expr {
    Expr::new(ExprKind::Ident(name), synth_span())
}

fn int_expr(n: i64) -> Expr {
    Expr::new(ExprKind::Int(n), synth_span())
}

fn bool_expr(b: bool) -> Expr {
    Expr::new(ExprKind::Bool(b), synth_span())
}

fn string_expr(s: &str) -> Expr {
    Expr::new(ExprKind::StringLit(s.to_string(), false), synth_span())
}

fn tuple_expr(elems: Vec<Expr>) -> Expr {
    Expr::new(ExprKind::Tuple(elems), synth_span())
}

/// `recv.method(args...)` — used to emit `xa.compare(xb)`,
/// `self.x.equal(other.x)`, etc. The implementation is `Call(FieldAccess)`
/// because that is what the parser produces for surface-syntax method
/// calls.
fn method_call(recv: Expr, method: Symbol, args: Vec<Expr>) -> Expr {
    let fa = Expr::new(
        ExprKind::FieldAccess(Box::new(recv), method),
        synth_span(),
    );
    Expr::new(ExprKind::Call(Box::new(fa), args), synth_span())
}

/// `recv.field` — record field access.
fn field_access(recv: Expr, field: Symbol) -> Expr {
    Expr::new(
        ExprKind::FieldAccess(Box::new(recv), field),
        synth_span(),
    )
}

/// `a + b` — used to combine display strings.
fn bin(a: Expr, op: BinOp, b: Expr) -> Expr {
    Expr::new(
        ExprKind::Binary(Box::new(a), op, Box::new(b)),
        synth_span(),
    )
}

fn match_expr(scrut: Expr, arms: Vec<MatchArm>) -> Expr {
    Expr::new(
        ExprKind::Match {
            expr: Some(Box::new(scrut)),
            arms,
        },
        synth_span(),
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
    Expr::new(ExprKind::Block(stmts), synth_span())
}

fn let_stmt(name: Symbol, value: Expr) -> Stmt {
    Stmt::Let {
        pattern: id_pat(name),
        ty: None,
        value,
    }
}

fn named_te(name: Symbol) -> TypeExpr {
    TypeExpr::new(TypeExprKind::Named(name), synth_span())
}

/// Build a `TypeExpr` for the (possibly generic) type being derived.
/// `Box(a)` → `Generic(Box, [Named(a)])`; non-generic `Color` →
/// `Named(Color)`.
fn type_te(name: Symbol, params: &[Symbol]) -> TypeExpr {
    if params.is_empty() {
        named_te(name)
    } else {
        let args: Vec<TypeExpr> = params.iter().map(|p| named_te(*p)).collect();
        TypeExpr::new(TypeExprKind::Generic(name, args), synth_span())
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
fn fn_decl(
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
        span: synth_span(),
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
        .map(|p| (*p, trait_name, Vec::<TypeExpr>::new()))
        .collect();
    TraitImpl {
        trait_name,
        trait_args: Vec::new(),
        target_type: type_name,
        target_type_args,
        target_param_names,
        where_clauses,
        methods,
        span: synth_span(),
        is_auto_derived: true,
    }
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
    let self_sym = intern("self");
    let other_sym = intern("other");
    let self_te = type_te(type_name, type_params);
    // Uninhabited enum: `match self { }` is vacuously exhaustive — no
    // value of the type can reach this point, so the body never runs.
    // The exhaustiveness checker has a documented short-circuit for
    // uninhabited scrutinees (see tests/empty_type_match_tests.rs).
    if variants.is_empty() {
        let body = Expr::new(
            ExprKind::Match {
                expr: Some(Box::new(ident_expr(self_sym))),
                arms: Vec::new(),
            },
            synth_span(),
        );
        let method = fn_decl(
            intern("compare"),
            vec![
                param(self_sym, self_te.clone()),
                param(other_sym, self_te),
            ],
            Some(named_te(intern("Int"))),
            body,
        );
        return trait_impl(intern("Compare"), type_name, type_params, vec![method]);
    }
    let scrut = tuple_expr(vec![ident_expr(self_sym), ident_expr(other_sym)]);
    let mut arms: Vec<MatchArm> = Vec::new();

    // Same-tag arms.
    for variant in variants {
        let arity = variant.fields.len();
        if arity == 0 {
            // (V, V) -> 0
            let pat = tuple_pat(vec![
                ctor_pat(variant.name, vec![]),
                ctor_pat(variant.name, vec![]),
            ]);
            arms.push(arm(pat, int_expr(0)));
        } else {
            // (V(a1, a2, ..), V(b1, b2, ..)) -> lex compare
            let a_names: Vec<Symbol> = (0..arity)
                .map(|i| intern(&format!("__d_a{i}__")))
                .collect();
            let b_names: Vec<Symbol> = (0..arity)
                .map(|i| intern(&format!("__d_b{i}__")))
                .collect();
            let a_pat = ctor_pat(variant.name, a_names.iter().map(|n| id_pat(*n)).collect());
            let b_pat = ctor_pat(variant.name, b_names.iter().map(|n| id_pat(*n)).collect());
            let pat = tuple_pat(vec![a_pat, b_pat]);
            // Build chain: c0 = a0.compare(b0); match c0 { 0 -> c1 = ..., _ -> c0 }
            let body = build_lex_compare_chain(&a_names, &b_names);
            arms.push(arm(pat, body));
        }
    }

    // Catch-all arm: ordinal compare.
    if variants.len() > 1 {
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
        arms.push(arm(wildcard_pat(), block_expr(stmts)));
    }

    let body = match_expr(scrut, arms);
    let method = fn_decl(
        intern("compare"),
        vec![
            param(self_sym, self_te.clone()),
            param(other_sym, self_te),
        ],
        Some(named_te(intern("Int"))),
        body,
    );
    trait_impl(intern("Compare"), type_name, type_params, vec![method])
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
            arm(
                Pattern::new(PatternKind::Int(0), synth_span()),
                inner,
            ),
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
    let self_sym = intern("self");
    let other_sym = intern("other");
    let self_te = type_te(type_name, type_params);
    if variants.is_empty() {
        let body = Expr::new(
            ExprKind::Match {
                expr: Some(Box::new(ident_expr(self_sym))),
                arms: Vec::new(),
            },
            synth_span(),
        );
        let method = fn_decl(
            intern("equal"),
            vec![
                param(self_sym, self_te.clone()),
                param(other_sym, self_te),
            ],
            Some(named_te(intern("Bool"))),
            body,
        );
        return trait_impl(intern("Equal"), type_name, type_params, vec![method]);
    }
    let scrut = tuple_expr(vec![ident_expr(self_sym), ident_expr(other_sym)]);
    let mut arms: Vec<MatchArm> = Vec::new();

    for variant in variants {
        let arity = variant.fields.len();
        if arity == 0 {
            arms.push(arm(
                tuple_pat(vec![
                    ctor_pat(variant.name, vec![]),
                    ctor_pat(variant.name, vec![]),
                ]),
                bool_expr(true),
            ));
        } else {
            let a_names: Vec<Symbol> = (0..arity)
                .map(|i| intern(&format!("__d_ea{i}__")))
                .collect();
            let b_names: Vec<Symbol> = (0..arity)
                .map(|i| intern(&format!("__d_eb{i}__")))
                .collect();
            let pat = tuple_pat(vec![
                ctor_pat(variant.name, a_names.iter().map(|n| id_pat(*n)).collect()),
                ctor_pat(variant.name, b_names.iter().map(|n| id_pat(*n)).collect()),
            ]);
            // a0.equal(b0) && a1.equal(b1) && ... — left-fold
            let mut chain: Expr = method_call(
                ident_expr(a_names[0]),
                intern("equal"),
                vec![ident_expr(b_names[0])],
            );
            for i in 1..arity {
                let next = method_call(
                    ident_expr(a_names[i]),
                    intern("equal"),
                    vec![ident_expr(b_names[i])],
                );
                chain = bin(chain, BinOp::And, next);
            }
            arms.push(arm(pat, chain));
        }
    }

    // Catch-all: false. Only emit when there's more than one variant
    // (otherwise the same-tag arm above is total, and a wildcard arm
    // would be unreachable and trigger the typechecker's
    // unreachable-pattern guard).
    if variants.len() > 1 {
        arms.push(arm(wildcard_pat(), bool_expr(false)));
    }

    let body = match_expr(scrut, arms);
    let method = fn_decl(
        intern("equal"),
        vec![
            param(self_sym, self_te.clone()),
            param(other_sym, self_te),
        ],
        Some(named_te(intern("Bool"))),
        body,
    );
    trait_impl(intern("Equal"), type_name, type_params, vec![method])
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
    let self_sym = intern("self");
    let self_te = type_te(type_name, type_params);
    if variants.is_empty() {
        // `match self { }` — uninhabited scrutinee.
        let body = Expr::new(
            ExprKind::Match {
                expr: Some(Box::new(ident_expr(self_sym))),
                arms: Vec::new(),
            },
            synth_span(),
        );
        let method = fn_decl(
            intern("hash"),
            vec![param(self_sym, self_te)],
            Some(named_te(intern("Int"))),
            body,
        );
        return trait_impl(intern("Hash"), type_name, type_params, vec![method]);
    }
    let arms: Vec<MatchArm> = variants
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let arity = v.fields.len();
            if arity == 0 {
                // Just the tag hash — `i.hash()` for ordinal i.
                arm(ctor_pat(v.name, vec![]), method_call(int_expr(i as i64), intern("hash"), vec![]))
            } else {
                let arg_names: Vec<Symbol> = (0..arity)
                    .map(|j| intern(&format!("__d_h{j}__")))
                    .collect();
                let pat = ctor_pat(v.name, arg_names.iter().map(|n| id_pat(*n)).collect());
                // tag = i; combine = tag.hash(); for each arg: combine = combine_hash(combine, arg.hash())
                let mut combine: Expr = method_call(int_expr(i as i64), intern("hash"), vec![]);
                for arg_name in &arg_names {
                    let arg_hash = method_call(
                        ident_expr(*arg_name),
                        intern("hash"),
                        vec![],
                    );
                    combine = combine_hash_expr(combine, arg_hash);
                }
                arm(pat, combine)
            }
        })
        .collect();
    let body = match_expr(ident_expr(self_sym), arms);
    let method = fn_decl(
        intern("hash"),
        vec![param(self_sym, self_te)],
        Some(named_te(intern("Int"))),
        body,
    );
    trait_impl(intern("Hash"), type_name, type_params, vec![method])
}

/// Combine two hashes into a structural hash. Silt's surface-level
/// arithmetic is checked (overflow → runtime error), so a
/// straightforward `a * 31 + b` FNV-style combine blows up immediately
/// when `a` or `b` is the i64-magnitude output of `Int.hash()` (which
/// itself is `DefaultHasher::finish() as i64`). To stay in pure silt
/// surface syntax without bypassing the overflow check, each side is
/// reduced modulo a large prime first:
///
///   ((a mod P) * 31 + (b mod P)) mod (i64-safe bound)
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

pub(super) fn synth_display_impl_for_enum(
    type_name: Symbol,
    type_params: &[Symbol],
    variants: &[EnumVariant],
) -> TraitImpl {
    let self_sym = intern("self");
    let self_te = type_te(type_name, type_params);
    if variants.is_empty() {
        let body = Expr::new(
            ExprKind::Match {
                expr: Some(Box::new(ident_expr(self_sym))),
                arms: Vec::new(),
            },
            synth_span(),
        );
        let method = fn_decl(
            intern("display"),
            vec![param(self_sym, self_te)],
            Some(named_te(intern("String"))),
            body,
        );
        return trait_impl(intern("Display"), type_name, type_params, vec![method]);
    }
    let arms: Vec<MatchArm> = variants
        .iter()
        .map(|v| {
            let arity = v.fields.len();
            let tag_name = crate::intern::resolve(v.name);
            if arity == 0 {
                arm(ctor_pat(v.name, vec![]), string_expr(&tag_name))
            } else {
                let arg_names: Vec<Symbol> = (0..arity)
                    .map(|j| intern(&format!("__d_d{j}__")))
                    .collect();
                let pat = ctor_pat(v.name, arg_names.iter().map(|n| id_pat(*n)).collect());
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
                acc = bin(acc, BinOp::Add, string_expr(")"));
                arm(pat, acc)
            }
        })
        .collect();
    let body = match_expr(ident_expr(self_sym), arms);
    let method = fn_decl(
        intern("display"),
        vec![param(self_sym, self_te)],
        Some(named_te(intern("String"))),
        body,
    );
    trait_impl(intern("Display"), type_name, type_params, vec![method])
}

// ── Compare on record ────────────────────────────────────────────────

pub(super) fn synth_compare_impl_for_record(
    type_name: Symbol,
    type_params: &[Symbol],
    fields: &[RecordField],
) -> TraitImpl {
    let self_sym = intern("self");
    let other_sym = intern("other");
    let self_te = type_te(type_name, type_params);
    let body = if fields.is_empty() {
        // No fields → all instances are equal under Compare.
        int_expr(0)
    } else {
        build_record_lex_compare(self_sym, other_sym, fields)
    };
    let method = fn_decl(
        intern("compare"),
        vec![
            param(self_sym, self_te.clone()),
            param(other_sym, self_te),
        ],
        Some(named_te(intern("Int"))),
        body,
    );
    trait_impl(intern("Compare"), type_name, type_params, vec![method])
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
                Pattern::new(PatternKind::Int(0), synth_span()),
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

pub(super) fn synth_equal_impl_for_record(
    type_name: Symbol,
    type_params: &[Symbol],
    fields: &[RecordField],
) -> TraitImpl {
    let self_sym = intern("self");
    let other_sym = intern("other");
    let self_te = type_te(type_name, type_params);
    let body = if fields.is_empty() {
        bool_expr(true)
    } else {
        // self.f0.equal(other.f0) && self.f1.equal(other.f1) && ...
        let mut chain: Expr = method_call(
            field_access(ident_expr(self_sym), fields[0].name),
            intern("equal"),
            vec![field_access(ident_expr(other_sym), fields[0].name)],
        );
        for f in &fields[1..] {
            let next = method_call(
                field_access(ident_expr(self_sym), f.name),
                intern("equal"),
                vec![field_access(ident_expr(other_sym), f.name)],
            );
            chain = bin(chain, BinOp::And, next);
        }
        chain
    };
    let method = fn_decl(
        intern("equal"),
        vec![
            param(self_sym, self_te.clone()),
            param(other_sym, self_te),
        ],
        Some(named_te(intern("Bool"))),
        body,
    );
    trait_impl(intern("Equal"), type_name, type_params, vec![method])
}

// ── Hash on record ───────────────────────────────────────────────────

pub(super) fn synth_hash_impl_for_record(
    type_name: Symbol,
    type_params: &[Symbol],
    fields: &[RecordField],
) -> TraitImpl {
    let self_sym = intern("self");
    let self_te = type_te(type_name, type_params);
    let body = if fields.is_empty() {
        int_expr(0)
    } else {
        // h0.combine(h1).combine(h2)... where h0 = self.f0.hash()
        let mut combine: Expr = method_call(
            field_access(ident_expr(self_sym), fields[0].name),
            intern("hash"),
            vec![],
        );
        for f in &fields[1..] {
            let next = method_call(
                field_access(ident_expr(self_sym), f.name),
                intern("hash"),
                vec![],
            );
            combine = combine_hash_expr(combine, next);
        }
        combine
    };
    let method = fn_decl(
        intern("hash"),
        vec![param(self_sym, self_te)],
        Some(named_te(intern("Int"))),
        body,
    );
    trait_impl(intern("Hash"), type_name, type_params, vec![method])
}

// ── Display on record ────────────────────────────────────────────────

pub(super) fn synth_display_impl_for_record(
    type_name: Symbol,
    type_params: &[Symbol],
    fields: &[RecordField],
) -> TraitImpl {
    let self_sym = intern("self");
    let self_te = type_te(type_name, type_params);
    let name_str = crate::intern::resolve(type_name);
    let body = if fields.is_empty() {
        // "Name {}"
        string_expr(&format!("{name_str} {{}}"))
    } else {
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
        acc = bin(acc, BinOp::Add, string_expr(" }"));
        acc
    };
    let method = fn_decl(
        intern("display"),
        vec![param(self_sym, self_te)],
        Some(named_te(intern("String"))),
        body,
    );
    trait_impl(intern("Display"), type_name, type_params, vec![method])
}
