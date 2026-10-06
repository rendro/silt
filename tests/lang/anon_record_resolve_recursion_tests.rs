//! Regression tests for round 67 audit findings:
//!
//! - **F2 (BROKEN)**: `expr_references_name` / `decl_references_name` /
//!   `collect_sub_spans` in `src/typechecker/resolve.rs` did not recurse
//!   into `ExprKind::AnonRecord`, so a `let x = nullary()` followed by
//!   `let r = { val: x }` produced a misleading
//!   "cannot infer the type of `x`" cascade — even though `x` *is*
//!   referenced inside the anon-record literal.
//!
//! - **F9 (LATENT)**: `resolve_expr_types` in
//!   `src/typechecker/resolve.rs` did not recurse into
//!   `ExprKind::AnonRecord` field-value sub-expressions, so the
//!   per-field `expr.ty` annotations stayed as bare `Type::Var(_)`
//!   after inference; LSP hover then renders `_` for those
//!   sub-expressions instead of the inferred concrete type.
//!
//! - **F10 (LATENT)**: `substitute_vars` in `src/types/mod.rs` had a
//!   silent fall-through when a row-tail var was bound to a
//!   non-`AnonRecord` type. A `debug_assert!` guard was added so
//!   future drift in the unifier (which currently only ever binds row
//!   vars to records) is caught immediately rather than silently
//!   dropping the binding.
//!
//! Each test was written to FAIL against the pre-fix codebase and
//! PASS after the corresponding edit.

use std::collections::HashMap;

use silt::ast::{Expr, ExprKind, Stmt};
use silt::diagnostic::Severity;
use silt::types::{RowTail, Type};

fn typecheck(
    input: &str,
) -> (
    std::sync::Arc<silt::ast::Program>,
    Vec<silt::diagnostic::Diagnostic>,
) {
    silt::session::testing::analyze_str(input)
}

// F2 (the "cannot infer" cascade) is covered by the golden cases
// tests/golden/lang/records/anon_record_resolve_recursion__*.silt.

// ── F9: AnonRecord field-value sub-expression types must be
//        substituted (no leftover bare Type::Var) ─────────────────

/// Walk an expression tree and collect the resolved `expr.ty` of every
/// AnonRecord field-value sub-expression. Returns a list of
/// (field_name_debug_str, type_or_none) pairs; the caller asserts on
/// what was found.
fn collect_anon_record_field_tys(expr: &Expr, out: &mut Vec<Option<Type>>) {
    if let ExprKind::AnonRecord { spread, fields } = &expr.kind {
        if let Some(s) = spread {
            out.push(s.ty.clone());
            collect_anon_record_field_tys(s, out);
        }
        for (_, e) in fields {
            out.push(e.ty.clone());
            collect_anon_record_field_tys(e, out);
        }
    }
    // Recurse into all child exprs (only the cases that can plausibly
    // contain an AnonRecord — keep this small + robust).
    match &expr.kind {
        ExprKind::Block(stmts) => {
            for s in stmts {
                if let Stmt::Let { value, .. } | Stmt::Expr(value) = s {
                    collect_anon_record_field_tys(value, out);
                }
            }
        }
        ExprKind::Call(callee, args) => {
            collect_anon_record_field_tys(callee, out);
            for a in args {
                collect_anon_record_field_tys(a, out);
            }
        }
        ExprKind::Lambda { body, .. } => collect_anon_record_field_tys(body, out),
        ExprKind::AnonRecord { spread, fields } => {
            if let Some(s) = spread {
                collect_anon_record_field_tys(s, out);
            }
            for (_, e) in fields {
                collect_anon_record_field_tys(e, out);
            }
        }
        _ => {}
    }
}

#[test]
fn f9_anon_record_field_subexpr_types_resolved() {
    // After typechecking, every AnonRecord field-value sub-expression
    // must have its `expr.ty` substituted to a concrete type. Without
    // F9's fix, `resolve_expr_types` skipped AnonRecord and the field
    // expr tys stayed as bare `Type::Var(_)`.
    //
    // The field value is an identifier `x` whose type is only
    // resolved via a downstream use of `r`. At inference time, when
    // the AnonRecord literal is built, `x`'s type is still a fresh
    // `Type::Var`. Only after `r.val + 1` constrains `x` to `Int`
    // does the substitution mapping bind that var. The post-pass
    // `resolve_expr_types` then *must* recurse into AnonRecord field
    // exprs to apply the substitution — without the F9 fix, the
    // field expr's `expr.ty` stays as the bare `Type::Var`.
    let source = r#"
fn nullary() { [] }
fn main() {
  let x = nullary()
  let r = { val: x }
  let y: List(Int) = r.val
  ()
}
"#;
    let (program, errors) = typecheck(source);
    assert!(
        errors.iter().all(|e| e.severity != Severity::Error),
        "source should typecheck cleanly: {errors:?}"
    );

    let mut field_tys: Vec<Option<Type>> = Vec::new();
    for decl in &program.decls {
        if let silt::ast::Decl::Fn(f) = decl {
            collect_anon_record_field_tys(&f.body, &mut field_tys);
        }
    }
    assert!(
        !field_tys.is_empty(),
        "expected to find AnonRecord field-value sub-exprs in the AST"
    );
    for ty in &field_tys {
        match ty {
            Some(Type::Var(v)) => panic!(
                "AnonRecord field-value sub-expr ty stayed as bare Type::Var({v}) — \
                 resolve_expr_types must recurse into AnonRecord (round 67 F9)"
            ),
            None => panic!(
                "AnonRecord field-value sub-expr has no ty annotation — \
                 inference should have set one"
            ),
            Some(_) => {} // resolved
        }
    }
}

// ── F10: substitute_vars row-tail handling ─────────────────────────
//
// `substitute_vars` is used by generic instantiation. When a row-tail
// var `v` is mapped to:
//   - another `Type::Var(w)` (fresh, from instantiation): re-tail on
//     `w` so the freshening propagates correctly. Pre-fix, `v` was
//     silently kept, leaving instantiation with a stale bound var
//     that downstream `apply` calls then mis-resolved.
//   - an `AnonRecord`: merge its fields and propagate its tail
//     (existing arm; not exercised here).
//   - any other concrete type: a genuine invariant violation. A
//     `debug_assert!` now fires in debug builds; release retains the
//     safe fall-through.

#[test]
fn f10_row_tail_var_remapped_to_fresh_var_during_substitution() {
    // Round 67 F10: instantiation should re-tail the row variable on
    // the fresh var it was mapped to, not silently keep the original.
    let bound_var: silt::types::TyVar = 7;
    let fresh_var: silt::types::TyVar = 99;
    let ty = Type::AnonRecord {
        fields: std::collections::BTreeMap::new(),
        tail: RowTail::Var(bound_var),
    };
    let mut mapping: HashMap<silt::types::TyVar, Type> = HashMap::new();
    mapping.insert(bound_var, Type::Var(fresh_var));

    let result = silt::types::substitute_vars(&ty, &mapping);
    match result {
        Type::AnonRecord { tail, .. } => match tail {
            RowTail::Var(v) => assert_eq!(
                v, fresh_var,
                "row tail must re-tail on the fresh var the bound var was mapped to \
                 (round 67 F10); got Var({v}) but expected Var({fresh_var})"
            ),
            other => panic!("expected RowTail::Var after substitution, got {other:?}"),
        },
        other => panic!("expected AnonRecord after substitution, got {other:?}"),
    }
}
