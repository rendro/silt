//! Phase A effect-set inference.
//!
//! Bottom-up traversal of an expression tree that computes the
//! `EffectSet` performed by a function body. Used by the typechecker to
//! populate `TypeChecker::fn_body_effects` after Hindley-Milner inference
//! has finished. The pass is read-only — it inspects the AST and the
//! environment but does not mutate either, so it can run after the
//! existing inference pipeline without disturbing it.
//!
//! Phase A semantics:
//!   - A reference to a name with a known `Scheme` contributes that
//!     scheme's `effects` field.
//!   - A name not found in the environment contributes `EffectSet::TOP`
//!     (the conservative default — we don't know what it does).
//!   - A lambda's body's effects do NOT propagate to the enclosing
//!     scope. Only invoking the lambda would, but Phase A doesn't track
//!     lambda values across binding sites — every lambda construction
//!     contributes `EffectSet::EMPTY` to the surrounding effect set.
//!   - Pattern match arms produce the union of their bodies' effects.
//!
//! Annotation enforcement (does the body's inferred set fit inside the
//! declared set?) lands in Phase B/D — this module only computes.

use std::collections::HashMap;

use crate::ast::{Expr, ExprKind, ListElem, MatchArm, PatternKind, Stmt, StringPart};
use crate::types::effects::EffectSet;

use super::{Symbol, TypeEnv};

/// Per-walk side-channel of let-bound function aliases. Block traversal
/// extends this map with `let alias = doit` style bindings so that
/// subsequent `alias()` calls can resolve to `doit`'s declared
/// `Scheme::effects` instead of falling through to the conservative
/// higher-order `EffectSet::TOP`. The typechecker fix in
/// `inference::propagate_alias_effects` writes the same effect set onto
/// the alias's `Scheme::effects` field; this map mirrors that into the
/// effects-walker world, where the let-binding env is dropped before
/// `infer_expr_effects` runs over `f.body`.
///
/// Round 94 (module-shadowing): also carries the set of names REBOUND
/// by `let` statements, match-arm patterns and `loop` bindings inside
/// the walked body. The walker's `env` only holds the enclosing fn's
/// parameters plus top-level names, so without this set a body-local
/// rebinding of a name that collides with an imported module
/// (`let other = ...; other.double(2)`) would let `call_effects`
/// resolve the dotted callee through the MODULE's qualified scheme —
/// charging the wrong fn's effects in whichever direction it differs.
#[derive(Clone, Default)]
struct AliasMap {
    aliases: HashMap<Symbol, EffectSet>,
    rebound: std::collections::HashSet<Symbol>,
}

impl AliasMap {
    fn new() -> Self {
        Self::default()
    }

    fn get(&self, name: &Symbol) -> Option<&EffectSet> {
        self.aliases.get(name)
    }

    fn insert(&mut self, name: Symbol, eff: EffectSet) {
        self.aliases.insert(name, eff);
    }

    fn remove(&mut self, name: &Symbol) {
        self.aliases.remove(name);
    }

    /// Record that `pattern`'s binders rebind (shadow) their names for
    /// the remainder of the walk.
    fn mark_pattern_rebound(&mut self, pattern: &crate::ast::Pattern) {
        for name in super::collect_pattern_vars(pattern) {
            self.rebound.insert(name);
        }
    }

    /// Is `name` rebound by a body-local binding (and therefore not a
    /// module qualifier)?
    fn is_rebound(&self, name: Symbol) -> bool {
        self.rebound.contains(&name)
    }
}

/// Compute the effect set performed by `expr`. Walks the AST bottom-up
/// and unions the effects of every sub-expression. Function calls
/// contribute the callee's `Scheme::effects` from `env`; unknown names
/// contribute `EffectSet::TOP` (conservative default).
pub(super) fn infer_expr_effects(expr: &Expr, env: &TypeEnv) -> EffectSet {
    let aliases = AliasMap::new();
    walk_expr(expr, env, &aliases)
}

fn walk_expr(expr: &Expr, env: &TypeEnv, aliases: &AliasMap) -> EffectSet {
    match &expr.kind {
        // Pure leaves — literals carry no effects.
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Bool(_)
        | ExprKind::StringLit(..)
        | ExprKind::Unit => EffectSet::EMPTY,

        // A bare identifier reads a binding — reading the binding itself
        // is pure. Effects from a value-of-fn-type are surfaced only at
        // a Call site (see ExprKind::Call below).
        ExprKind::Ident(_) => EffectSet::EMPTY,

        ExprKind::StringInterp(parts) => {
            let mut acc = EffectSet::EMPTY;
            for part in parts {
                if let StringPart::Expr(e) = part {
                    acc = acc.union(walk_expr(e, env, aliases));
                }
            }
            acc
        }

        ExprKind::List(elems) => {
            let mut acc = EffectSet::EMPTY;
            for e in elems {
                let inner = match e {
                    ListElem::Single(x) | ListElem::Spread(x) => x,
                };
                acc = acc.union(walk_expr(inner, env, aliases));
            }
            acc
        }

        ExprKind::Map(entries) => {
            let mut acc = EffectSet::EMPTY;
            for (k, v) in entries {
                acc = acc.union(walk_expr(k, env, aliases));
                acc = acc.union(walk_expr(v, env, aliases));
            }
            acc
        }

        ExprKind::SetLit(elems) | ExprKind::Tuple(elems) => {
            let mut acc = EffectSet::EMPTY;
            for e in elems {
                acc = acc.union(walk_expr(e, env, aliases));
            }
            acc
        }

        ExprKind::FieldAccess(obj, _) => walk_expr(obj, env, aliases),

        ExprKind::Binary(l, _, r) => walk_expr(l, env, aliases).union(walk_expr(r, env, aliases)),
        ExprKind::Unary(_, inner)
        | ExprKind::QuestionMark(inner)
        | ExprKind::Ascription(inner, _) => walk_expr(inner, env, aliases),
        // A pipe is a call in disguise: `a |> f(b)` desugars to `f(a, b)`
        // (see the typechecker's ExprKind::Pipe arm in inference.rs). When
        // the RHS is already a Call, walking it lands in the Call arm
        // below, which charges the callee's declared effects. Any OTHER
        // RHS shape (bare ident `|> println`, dotted path
        // `|> io.read_file`, lambda, ...) IS the callee of the desugared
        // call, so it must be charged through the same `call_effects`
        // path the Call arm uses — otherwise `"hi" |> println` would
        // contribute EMPTY (Ident arm) while `println("hi")` contributes
        // io, and `--strict-effects` would miss every effect performed
        // via a pipe (round 93 soundness gap).
        ExprKind::Pipe(l, r) => {
            let lhs = walk_expr(l, env, aliases);
            let rhs = match &r.kind {
                ExprKind::Call(callee, args) => call_effects(callee, args, env, aliases),
                _ => call_effects(r, &[], env, aliases),
            };
            lhs.union(rhs)
        }
        ExprKind::Range(l, r) => walk_expr(l, env, aliases).union(walk_expr(r, env, aliases)),
        ExprKind::FloatElse(l, r) => walk_expr(l, env, aliases).union(walk_expr(r, env, aliases)),

        ExprKind::Call(callee, args) => call_effects(callee, args, env, aliases),

        // Lambda construction is pure — building a function value does
        // not perform the function's effects. Effects only happen when
        // the lambda is *called*. Phase A doesn't track lambda values
        // through let-bindings, so a lambda's body's effects don't leak
        // upward; downstream calls of the lambda would be Higher-Order
        // and hit the TOP branch in Call above. That's the documented
        // Phase A limitation.
        ExprKind::Lambda { .. } => EffectSet::EMPTY,

        ExprKind::RecordCreate { fields, .. } => {
            let mut acc = EffectSet::EMPTY;
            for (_, e) in fields {
                acc = acc.union(walk_expr(e, env, aliases));
            }
            acc
        }
        ExprKind::RecordUpdate { expr, fields } => {
            let mut acc = walk_expr(expr, env, aliases);
            for (_, e) in fields {
                acc = acc.union(walk_expr(e, env, aliases));
            }
            acc
        }
        ExprKind::AnonRecord { spread, fields } => {
            let mut acc = EffectSet::EMPTY;
            if let Some(sp) = spread {
                acc = acc.union(walk_expr(sp, env, aliases));
            }
            for (_, e) in fields {
                acc = acc.union(walk_expr(e, env, aliases));
            }
            acc
        }

        ExprKind::Match { expr, arms } => {
            let mut acc = match expr {
                Some(e) => walk_expr(e, env, aliases),
                None => EffectSet::EMPTY,
            };
            for arm in arms {
                acc = acc.union(arm_effects(arm, env, aliases));
            }
            acc
        }

        ExprKind::Return(inner) => match inner {
            Some(e) => walk_expr(e, env, aliases),
            None => EffectSet::EMPTY,
        },

        ExprKind::Block(stmts) => {
            // Round 64 BROKEN: thread an alias map through statements
            // so that `let alias = doit; alias()` resolves the alias
            // call to `doit`'s declared `Scheme::effects` instead of
            // falling through to the higher-order TOP default. The
            // typechecker side (`inference::propagate_alias_effects`)
            // writes the same effects onto the alias's `Scheme` field,
            // but that env is dropped before the effects walker sees
            // the body — so we recreate the binding here.
            let mut local = aliases.clone();
            let mut acc = EffectSet::EMPTY;
            for s in stmts {
                acc = acc.union(stmt_effects(s, env, &mut local));
            }
            acc
        }

        ExprKind::Loop { bindings, body } => {
            let mut acc = EffectSet::EMPTY;
            for (_, e) in bindings {
                acc = acc.union(walk_expr(e, env, aliases));
            }
            // Round 94: loop binders rebind their names inside the body
            // (same rationale as `arm_effects`).
            let mut local = aliases.clone();
            for (name, _) in bindings {
                local.rebound.insert(*name);
            }
            acc.union(walk_expr(body, env, &local))
        }
        ExprKind::Recur(args) => {
            let mut acc = EffectSet::EMPTY;
            for a in args {
                acc = acc.union(walk_expr(a, env, aliases));
            }
            acc
        }
    }
}

fn arm_effects(arm: &MatchArm, env: &TypeEnv, aliases: &AliasMap) -> EffectSet {
    // Round 94: arm-pattern binders rebind their names for the guard
    // and body — `match x { other -> other.double(2) }` must not
    // resolve `other.double` through a same-named imported module.
    let mut local = aliases.clone();
    local.mark_pattern_rebound(&arm.pattern);
    let mut acc = EffectSet::EMPTY;
    if let Some(g) = &arm.guard {
        acc = acc.union(walk_expr(g, env, &local));
    }
    acc.union(walk_expr(&arm.body, env, &local))
}

fn stmt_effects(stmt: &Stmt, env: &TypeEnv, aliases: &mut AliasMap) -> EffectSet {
    match stmt {
        Stmt::Let { pattern, value, .. } => {
            // Round 64 BROKEN: an aliasing let (`let alias = doit`,
            // `let alias = mod.func`) extends the alias map so a later
            // `alias()` Call in the same Block resolves to the source's
            // declared effects. Mirrors `inference::propagate_alias_effects`
            // for the effects walker, which doesn't share the
            // typechecker's TypeEnv mutations across passes.
            let val_effects = walk_expr(value, env, aliases);
            // Round 94: resolve the potential alias source BEFORE
            // marking this let's binders as rebound — the value sits
            // outside the new binding's scope and must see the outer
            // meaning of any name it mentions. (Shadowing-aware: a
            // dotted source whose base is itself shadowed is not a
            // module path — see `resolvable_callee_name`.)
            let src = resolvable_callee_name(value, env, aliases);
            // Every binder introduced by this let rebinds its name for
            // the rest of the block — a later `name.fn(...)` must not
            // resolve through a same-named imported module.
            aliases.mark_pattern_rebound(pattern);
            if let PatternKind::Ident(name) = &pattern.kind {
                if let Some(src) = src {
                    // The value is a simple aliasing reference. Look up
                    // the source's effects through the same path the
                    // Call site uses (env + previous aliases).
                    let src_eff = scheme_effects_of(src, env, aliases);
                    aliases.insert(*name, src_eff);
                } else {
                    // Soundness fix: the value is NOT a simple aliasing
                    // name (e.g. a Call returning a fn). A prior aliasing
                    // `let name = pure_fn` would have cached
                    // `name -> EMPTY` in `aliases`; this shadowing rebind
                    // must invalidate that stale entry, or a later
                    // `name()` call would consult the stale EMPTY via
                    // `scheme_effects_of` (which checks `aliases` first)
                    // and UNDER-approximate effects — letting an
                    // under-annotated fn slip past `--strict-effects`.
                    // Removing the entry makes `scheme_effects_of` fall
                    // through to `env.lookup`, yielding the real/TOP
                    // effects (the conservative over-approximating
                    // direction the Phase-A invariant requires).
                    aliases.remove(name);
                }
            }
            val_effects
        }
        Stmt::When {
            pattern,
            expr,
            else_body,
        } => {
            // Round 97 BROKEN (soundness): `when let pat = expr else { .. }`
            // binds `pat`'s names into the enclosing block scope, exactly
            // like `Stmt::Let`. The arm previously ignored the pattern, so a
            // binder that shadows a same-named imported module was still
            // treated as a live module qualifier — a later `name.fn(...)`
            // resolved through the MODULE's qualified scheme and charged the
            // module fn's declared effects instead of conservative TOP,
            // letting a real effectful field-fn slip past --strict-effects.
            // Mirror the `Stmt::Let` arm: resolve any aliasing source first
            // (the scrutinee sits outside the new binding's scope), then mark
            // the binders rebound and invalidate stale alias entries.
            let scrutinee_effects = walk_expr(expr, env, aliases);
            let src = resolvable_callee_name(expr, env, aliases);
            aliases.mark_pattern_rebound(pattern);
            if let PatternKind::Ident(name) = &pattern.kind {
                if let Some(src) = src {
                    let src_eff = scheme_effects_of(src, env, aliases);
                    aliases.insert(*name, src_eff);
                } else {
                    aliases.remove(name);
                }
            }
            scrutinee_effects.union(walk_expr(else_body, env, aliases))
        }
        Stmt::WhenBool {
            condition,
            else_body,
        } => walk_expr(condition, env, aliases).union(walk_expr(else_body, env, aliases)),
        Stmt::Expr(e) => walk_expr(e, env, aliases),
    }
}

/// Effects of a call: evaluating the callee + the args + the call
/// itself. The single source of truth for charging a callee's declared
/// effects — used by both the direct `ExprKind::Call` arm AND the
/// `ExprKind::Pipe` arm (a pipe desugars to a call), so the two
/// syntactic forms of the same call can never diverge.
///
/// The call itself contributes the callee's declared effects. We
/// extract a name when the callee is a simple identifier or a dotted
/// path resolved by the existing resolver shape (`fs.read_file`);
/// deeper resolution is Phase B.
fn call_effects(callee: &Expr, args: &[Expr], env: &TypeEnv, aliases: &AliasMap) -> EffectSet {
    let mut acc = walk_expr(callee, env, aliases);
    for a in args {
        acc = acc.union(walk_expr(a, env, aliases));
    }
    if let Some(name) = resolvable_callee_name(callee, env, aliases) {
        acc = acc.union(scheme_effects_of(name, env, aliases));
    } else {
        // Higher-order calls (the callee is computed): we have no
        // scheme to consult. Treat as TOP — the conservative default.
        // Phase B's annotation pass narrows this.
        acc = acc.union(EffectSet::TOP);
    }
    acc
}

/// Round 94 (module-shadowing): is `name` bound in `env` as a VALUE
/// (fn parameter, top-level fn/let)? Such a binding shadows a
/// same-named imported module, so a dotted callee `name.fn` is a call
/// through the binding's field — its effects must NOT be read off the
/// module's qualified scheme. Type-name bindings (`TypeOf(..)`
/// descriptor schemes) don't count, mirroring
/// `TypeChecker::value_binding_shadows_module`.
fn env_value_binding_shadows(env: &TypeEnv, name: Symbol) -> bool {
    match env.lookup(name) {
        Some(s) => !matches!(
            &s.ty,
            crate::types::Type::Generic(g, args)
                if crate::intern::resolve(*g) == "TypeOf" && args.len() == 1
        ),
        None => false,
    }
}

/// Shadowing-aware wrapper around `callee_name`: a dotted path whose
/// base is rebound in the walked body (`aliases.rebound`) or shadowed
/// by a value binding in `env` is NOT a module path — return None so
/// the caller falls back to the conservative higher-order TOP default,
/// exactly like any other field call on a value. (Charging the
/// same-named MODULE fn's declared effects would be wrong in both
/// directions: it can under-approximate — a soundness hole under
/// `--strict-effects` — or over-charge effects the local never has.)
fn resolvable_callee_name(callee: &Expr, env: &TypeEnv, aliases: &AliasMap) -> Option<Symbol> {
    if let ExprKind::FieldAccess(obj, _) = &callee.kind
        && let ExprKind::Ident(base) = &obj.kind
        && (aliases.is_rebound(*base) || env_value_binding_shadows(env, *base))
    {
        return None;
    }
    callee_name(callee)
}

/// Extract a callable name from a Call expression's `callee` slot, if
/// it's syntactically a name or a dotted path we recognise. Returns
/// None for higher-order callees we can't resolve at the AST level.
fn callee_name(callee: &Expr) -> Option<Symbol> {
    match &callee.kind {
        ExprKind::Ident(name) => Some(*name),
        // Dotted path `mod.func` is parsed as FieldAccess(Ident(mod), func).
        // Reconstruct the joined `mod.func` symbol so we can look up the
        // builtin scheme stored under that intern.
        ExprKind::FieldAccess(obj, field) => {
            if let ExprKind::Ident(base) = &obj.kind {
                let joined = format!(
                    "{}.{}",
                    crate::intern::resolve(*base),
                    crate::intern::resolve(*field)
                );
                Some(crate::intern::intern(&joined))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Look up a name's scheme in the env and return its effect set. If the
/// name is unknown (typo, out of scope, etc.) return `EffectSet::TOP` —
/// the conservative default. We don't emit a diagnostic here; the main
/// type checker has already complained about unknown names by the time
/// the effects pass runs.
///
/// Round 64 BROKEN: consult `aliases` first, so a let-bound alias
/// (`let alias = doit`) resolves to the source fn's declared effect
/// set rather than the env-lookup result for `alias` (a freshly
/// generalized scheme whose effects field hardcoded TOP at the
/// `let`-binding site — see `inference::propagate_alias_effects` for
/// the matching typechecker-side fix).
fn scheme_effects_of(name: Symbol, env: &TypeEnv, aliases: &AliasMap) -> EffectSet {
    if let Some(eff) = aliases.get(&name) {
        return *eff;
    }
    match env.lookup(name) {
        Some(s) => s.effects,
        None => EffectSet::TOP,
    }
}

#[cfg(test)]
mod tests {
    //! Public-API exercises live in `tests/effect_inference_phase_a_tests.rs`.
    //! This block holds only narrow unit-tests of the effect-walking helpers
    //! that don't need a real `TypeEnv` to be meaningful.
    use super::*;
    use crate::ast::{Expr, ExprKind};
    use crate::lexer::Span;

    fn ident(name: &str) -> Expr {
        Expr::new(
            ExprKind::Ident(crate::intern::intern(name)),
            Span::new(0, 0),
        )
    }

    #[test]
    fn callee_name_extracts_simple_ident() {
        let e = ident("foo");
        assert_eq!(callee_name(&e), Some(crate::intern::intern("foo")));
    }

    #[test]
    fn callee_name_extracts_dotted_path() {
        let e = Expr::new(
            ExprKind::FieldAccess(Box::new(ident("io")), crate::intern::intern("read_file")),
            Span::new(0, 0),
        );
        assert_eq!(callee_name(&e), Some(crate::intern::intern("io.read_file")));
    }

    #[test]
    fn callee_name_returns_none_for_complex_expr() {
        // A call whose callee is itself a call: `((g x) y)` — no static name.
        let inner_call = Expr::new(
            ExprKind::Call(Box::new(ident("g")), vec![ident("x")]),
            Span::new(0, 0),
        );
        assert!(callee_name(&inner_call).is_none());
    }

    use crate::ast::{PatternKind, Stmt};
    use crate::types::Scheme;
    use crate::types::effects::EffectSet;

    fn call(callee: Expr, args: Vec<Expr>) -> Expr {
        Expr::new(ExprKind::Call(Box::new(callee), args), Span::new(0, 0))
    }

    fn let_stmt(name: &str, value: Expr) -> Stmt {
        Stmt::Let {
            pattern: crate::ast::Pattern::new(
                PatternKind::Ident(crate::intern::intern(name)),
                Span::new(0, 0),
            ),
            ty: None,
            value,
        }
    }

    /// Soundness lock (latent finding, round 64 alias-map): a shadowing
    /// `let f = <call expr>` must invalidate a prior aliasing
    /// `let f = <pure_name>` cache entry. Otherwise a later `f()` consults
    /// the stale `f -> EMPTY` alias and UNDER-approximates effects.
    ///
    /// Block:
    ///   let f = pure_fn      // caches f -> EMPTY (aliasing name)
    ///   let f = make_f()     // shadowing rebind to a non-name Call value
    ///   f()                  // later call — must see make_f's real effects
    ///
    /// `f` is registered in `env` with the rebound fn's real effects (io),
    /// `pure_fn` with EMPTY. Before the fix the block infers EMPTY (stale);
    /// after the fix the alias entry is removed so `f()` resolves via
    /// `env.lookup(f)` to io.
    #[test]
    fn shadowing_rebind_invalidates_stale_pure_alias() {
        let mut env = TypeEnv::new();
        // The pure source aliased first — caches `f -> EMPTY`.
        env.define(
            crate::intern::intern("pure_fn"),
            Scheme::with_effects(crate::types::Type::Unit, EffectSet::EMPTY),
        );
        // `make_f` is irrelevant to the alias result (it's the value of a
        // Call, not a name being aliased), but give it a scheme so the
        // walk over the Call body is well-defined.
        env.define(
            crate::intern::intern("make_f"),
            Scheme::with_effects(crate::types::Type::Unit, EffectSet::EMPTY),
        );
        // After the shadowing rebind, the *runtime* binding of `f` is the
        // result of `make_f()`. The env carries `f`'s real effects (io) —
        // this stands in for the rebound fn's actual declared effects that
        // `scheme_effects_of` must reach by falling through to env.lookup.
        env.define(
            crate::intern::intern("f"),
            Scheme::with_effects(crate::types::Type::Unit, EffectSet::io()),
        );

        let block = Expr::new(
            ExprKind::Block(vec![
                // let f = pure_fn   (aliasing name -> caches f -> EMPTY)
                let_stmt("f", ident("pure_fn")),
                // let f = make_f()  (shadowing rebind to a non-name value)
                let_stmt("f", call(ident("make_f"), vec![])),
                // f()               (later call)
                Stmt::Expr(call(ident("f"), vec![])),
            ]),
            Span::new(0, 0),
        );

        let effects = infer_expr_effects(&block, &env);

        // Must NOT be the stale EMPTY. After the fix the rebound `f`'s real
        // effects (io) surface.
        assert_ne!(
            effects,
            EffectSet::EMPTY,
            "stale pure alias was consulted after shadowing rebind — effects under-approximated"
        );
        assert!(
            effects.contains(crate::types::effects::Effect::Io),
            "rebound fn's real io effect should surface; got {effects:?}"
        );
    }
}
