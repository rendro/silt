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
type AliasMap = HashMap<Symbol, EffectSet>;

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
        ExprKind::Pipe(l, r) => walk_expr(l, env, aliases).union(walk_expr(r, env, aliases)),
        ExprKind::Range(l, r) => walk_expr(l, env, aliases).union(walk_expr(r, env, aliases)),
        ExprKind::FloatElse(l, r) => walk_expr(l, env, aliases).union(walk_expr(r, env, aliases)),

        ExprKind::Call(callee, args) => {
            // Effects of evaluating the callee + args + the call itself.
            let mut acc = walk_expr(callee, env, aliases);
            for a in args {
                acc = acc.union(walk_expr(a, env, aliases));
            }
            // The call itself contributes the callee's declared effects.
            // We extract a name when the callee is a simple identifier
            // or a dotted path resolved by the existing resolver shape
            // (`fs.read_file`); deeper resolution is Phase B.
            if let Some(name) = callee_name(callee) {
                acc = acc.union(scheme_effects_of(name, env, aliases));
            } else {
                // Higher-order calls (the callee is computed): we have
                // no scheme to consult. Treat as TOP — the conservative
                // default. Phase B's annotation pass narrows this.
                acc = acc.union(EffectSet::TOP);
            }
            acc
        }

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
            acc.union(walk_expr(body, env, aliases))
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
    let mut acc = EffectSet::EMPTY;
    if let Some(g) = &arm.guard {
        acc = acc.union(walk_expr(g, env, aliases));
    }
    acc.union(walk_expr(&arm.body, env, aliases))
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
            if let PatternKind::Ident(name) = &pattern.kind
                && let Some(src) = callee_name(value)
            {
                // The value is a simple aliasing reference. Look up
                // the source's effects through the same path the
                // Call site uses (env + previous aliases).
                let src_eff = scheme_effects_of(src, env, aliases);
                aliases.insert(*name, src_eff);
            }
            val_effects
        }
        Stmt::When {
            expr, else_body, ..
        } => walk_expr(expr, env, aliases).union(walk_expr(else_body, env, aliases)),
        Stmt::WhenBool {
            condition,
            else_body,
        } => walk_expr(condition, env, aliases).union(walk_expr(else_body, env, aliases)),
        Stmt::Expr(e) => walk_expr(e, env, aliases),
    }
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
}
