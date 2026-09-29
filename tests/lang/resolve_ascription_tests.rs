//! Round-52 regression-locking tests for `check_unresolved_in_expr` in
//! `src/typechecker/resolve.rs`.
//!
//! Finding: `ExprKind::Ascription` was silently dropped by the catch-all
//! `_ => {}` arm in `check_unresolved_in_expr`, so let-bindings with
//! unresolved bare type variables nested inside an ascription expression
//! (e.g. a block used as the inner expression of `expr as Type`) would
//! never be flagged. The sibling walker `resolve_expr_types` correctly
//! descends into Ascription — the omission was a quiet invariant break.
//!
//! This test locks the fix by ensuring the unresolved-tyvar diagnostic
//! fires for a problematic let-binding hidden inside an ascription.

use silt::typechecker;
use silt::types::{Severity, TypeError};

fn type_errors(input: &str) -> Vec<TypeError> {
    let tokens = silt::lexer::Lexer::new(input)
        .tokenize()
        .expect("lexer error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    typechecker::check(&mut program)
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .collect()
}

fn error_messages(input: &str) -> Vec<String> {
    type_errors(input).into_iter().map(|e| e.message).collect()
}

/// Behavioural regression lock: a `let` binding whose value has a bare
/// unresolved `Type::Var` is hidden inside the inner block of an
/// ascription expression (`({ ... }) as Int`). The bound name is never
/// referenced later, so the unresolved-tyvar diagnostic MUST fire.
///
/// Before the fix, `check_unresolved_in_expr` fell through `_ => {}`
/// for `ExprKind::Ascription` and silently ignored the inner block's
/// problematic let. After the fix, the inner block is visited and the
/// unresolved-type error is emitted.
#[test]
fn test_ascription_inner_block_unresolved_let_reported() {
    let errs = error_messages(
        r#"
fn default() -> a { panic("no value") }
fn main() {
  let y = ({
    let x = default()
    42
  }) as Int
  y
}
"#,
    );
    assert!(
        errs.iter().any(|m| m.contains("cannot infer the type")),
        "expected an unresolved-type diagnostic for the inner `let x = default()` \
         buried inside the ascription, but got: {errs:?}"
    );
}

/// Secondary behavioural check: the problematic let lives inside an
/// ascription whose outer context is a match-arm body. Same invariant:
/// the unresolved-tyvar diagnostic must still fire through the
/// ascription layer.
#[test]
fn test_ascription_in_match_arm_unresolved_let_reported() {
    let errs = error_messages(
        r#"
fn default() -> a { panic("no value") }
fn main() {
  let r = match 1 {
    _ -> ({
      let x = default()
      42
    }) as Int
  }
  r
}
"#,
    );
    assert!(
        errs.iter().any(|m| m.contains("cannot infer the type")),
        "expected an unresolved-type diagnostic for the inner `let x = default()` \
         buried inside a match-arm-body ascription, but got: {errs:?}"
    );
}
