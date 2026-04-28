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

use crate::ast::{Expr, ExprKind, ListElem, MatchArm, Stmt, StringPart};
use crate::types::effects::EffectSet;

use super::{Symbol, TypeEnv};

/// Compute the effect set performed by `expr`. Walks the AST bottom-up
/// and unions the effects of every sub-expression. Function calls
/// contribute the callee's `Scheme::effects` from `env`; unknown names
/// contribute `EffectSet::TOP` (conservative default).
pub(super) fn infer_expr_effects(expr: &Expr, env: &TypeEnv) -> EffectSet {
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
                    acc = acc.union(infer_expr_effects(e, env));
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
                acc = acc.union(infer_expr_effects(inner, env));
            }
            acc
        }

        ExprKind::Map(entries) => {
            let mut acc = EffectSet::EMPTY;
            for (k, v) in entries {
                acc = acc.union(infer_expr_effects(k, env));
                acc = acc.union(infer_expr_effects(v, env));
            }
            acc
        }

        ExprKind::SetLit(elems) | ExprKind::Tuple(elems) => {
            let mut acc = EffectSet::EMPTY;
            for e in elems {
                acc = acc.union(infer_expr_effects(e, env));
            }
            acc
        }

        ExprKind::FieldAccess(obj, _) => infer_expr_effects(obj, env),

        ExprKind::Binary(l, _, r) => infer_expr_effects(l, env).union(infer_expr_effects(r, env)),
        ExprKind::Unary(_, inner)
        | ExprKind::QuestionMark(inner)
        | ExprKind::Ascription(inner, _) => infer_expr_effects(inner, env),
        ExprKind::Pipe(l, r) => infer_expr_effects(l, env).union(infer_expr_effects(r, env)),
        ExprKind::Range(l, r) => infer_expr_effects(l, env).union(infer_expr_effects(r, env)),
        ExprKind::FloatElse(l, r) => infer_expr_effects(l, env).union(infer_expr_effects(r, env)),

        ExprKind::Call(callee, args) => {
            // Effects of evaluating the callee + args + the call itself.
            let mut acc = infer_expr_effects(callee, env);
            for a in args {
                acc = acc.union(infer_expr_effects(a, env));
            }
            // The call itself contributes the callee's declared effects.
            // We extract a name when the callee is a simple identifier
            // or a dotted path resolved by the existing resolver shape
            // (`fs.read_file`); deeper resolution is Phase B.
            if let Some(name) = callee_name(callee) {
                acc = acc.union(scheme_effects_of(name, env));
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
                acc = acc.union(infer_expr_effects(e, env));
            }
            acc
        }
        ExprKind::RecordUpdate { expr, fields } => {
            let mut acc = infer_expr_effects(expr, env);
            for (_, e) in fields {
                acc = acc.union(infer_expr_effects(e, env));
            }
            acc
        }
        ExprKind::AnonRecord { spread, fields } => {
            let mut acc = EffectSet::EMPTY;
            if let Some(sp) = spread {
                acc = acc.union(infer_expr_effects(sp, env));
            }
            for (_, e) in fields {
                acc = acc.union(infer_expr_effects(e, env));
            }
            acc
        }

        ExprKind::Match { expr, arms } => {
            let mut acc = match expr {
                Some(e) => infer_expr_effects(e, env),
                None => EffectSet::EMPTY,
            };
            for arm in arms {
                acc = acc.union(arm_effects(arm, env));
            }
            acc
        }

        ExprKind::Return(inner) => match inner {
            Some(e) => infer_expr_effects(e, env),
            None => EffectSet::EMPTY,
        },

        ExprKind::Block(stmts) => {
            let mut acc = EffectSet::EMPTY;
            for s in stmts {
                acc = acc.union(stmt_effects(s, env));
            }
            acc
        }

        ExprKind::Loop { bindings, body } => {
            let mut acc = EffectSet::EMPTY;
            for (_, e) in bindings {
                acc = acc.union(infer_expr_effects(e, env));
            }
            acc.union(infer_expr_effects(body, env))
        }
        ExprKind::Recur(args) => {
            let mut acc = EffectSet::EMPTY;
            for a in args {
                acc = acc.union(infer_expr_effects(a, env));
            }
            acc
        }
    }
}

fn arm_effects(arm: &MatchArm, env: &TypeEnv) -> EffectSet {
    let mut acc = EffectSet::EMPTY;
    if let Some(g) = &arm.guard {
        acc = acc.union(infer_expr_effects(g, env));
    }
    acc.union(infer_expr_effects(&arm.body, env))
}

fn stmt_effects(stmt: &Stmt, env: &TypeEnv) -> EffectSet {
    match stmt {
        Stmt::Let { value, .. } => infer_expr_effects(value, env),
        Stmt::When {
            expr, else_body, ..
        } => infer_expr_effects(expr, env).union(infer_expr_effects(else_body, env)),
        Stmt::WhenBool {
            condition,
            else_body,
        } => infer_expr_effects(condition, env).union(infer_expr_effects(else_body, env)),
        Stmt::Expr(e) => infer_expr_effects(e, env),
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
fn scheme_effects_of(name: Symbol, env: &TypeEnv) -> EffectSet {
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
