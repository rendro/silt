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
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::types::{RowTail, Severity, Type};

fn typecheck(input: &str) -> (silt::ast::Program, Vec<silt::types::TypeError>) {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let errors = typechecker::check(&mut program);
    (program, errors)
}

fn type_error_messages(errors: &[silt::types::TypeError]) -> Vec<String> {
    errors
        .iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message.clone())
        .collect()
}

// ── F2: AnonRecord usage in a later let must suppress the
//        "cannot infer" cascade ─────────────────────────────────────

#[test]
fn f2_anon_record_field_use_suppresses_cannot_infer_cascade() {
    // The exact repro from the audit. `nullary()` returns an empty
    // list (List(?a) at inference time). Without the F2 fix,
    // `expr_references_name` did not recurse into
    // `ExprKind::AnonRecord`, so `x`'s use inside `{ val: x }` was
    // invisible to `check_unresolved_let_types`, which then fired the
    // misleading "cannot infer the type of `x`" diagnostic.
    let source = r#"
fn nullary() = []
fn main() {
  let x = nullary()
  let r = { val: x }
  ()
}
"#;
    let (_program, errors) = typecheck(source);
    let messages = type_error_messages(&errors);
    assert!(
        !messages.iter().any(|m| m.contains("cannot infer")),
        "AnonRecord field use must suppress 'cannot infer' cascade for `x` (round 67 F2); \
         got errors: {messages:?}"
    );
}

#[test]
fn f2_anon_record_spread_use_suppresses_cannot_infer_cascade() {
    // Same idea, but the use site is a *spread* head — exercises
    // the `spread` arm that the fix recurses into. Without the F2
    // fix, `r` is an unresolved bare Var (because `r2`'s spread is
    // invisible) and a "cannot infer" error fires for `r`.
    let source = r#"
fn make() = ({ val: 1 })
fn main() {
  let r = make()
  let r2 = { ...r, extra: 2 }
  ()
}
"#;
    let (_program, errors) = typecheck(source);
    let messages = type_error_messages(&errors);
    assert!(
        !messages.iter().any(|m| m.contains("cannot infer")),
        "AnonRecord spread head must keep the spread base reachable to the \
         unresolved-let walker (round 67 F2); got errors: {messages:?}"
    );
}

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
fn nullary() = []
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

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "row tail var bound to non-record concrete type")]
fn f10_row_tail_var_bound_to_non_record_panics_in_debug() {
    // Construct a contrived AnonRecord whose RowTail::Var is mapped
    // (in the substitution mapping passed to substitute_vars) to a
    // non-Record, non-Var concrete type — Int in this case. The
    // unifier should never produce this state; if a future change
    // drifts the invariant, the debug_assert!() must catch it
    // immediately instead of silently dropping the binding.
    let row_var: silt::types::TyVar = 42;
    let ty = Type::AnonRecord {
        fields: std::collections::BTreeMap::new(),
        tail: RowTail::Var(row_var),
    };
    let mut mapping: HashMap<silt::types::TyVar, Type> = HashMap::new();
    mapping.insert(row_var, Type::Int);

    // Should panic via debug_assert! in debug builds.
    let _ = silt::types::substitute_vars(&ty, &mapping);
}
