//! Regression tests for Round-36 refutable-pattern soundness fix.
//!
//! BROKEN (pre-fix): literal/bool/range/pin/float-range patterns in `let`
//! binding position silently accepted non-matching values. Programs like
//!
//!   fn main() { let 5 = 10; println("silent") }
//!   fn main() { let x = 5; let ^x = 99; println("silent") }
//!   fn main() { let 1..10 = 999; println("silent") }
//!
//! all typechecked, ran to completion, and skipped the "println" silently
//! because the pattern-compile step emitted a zero check.
//!
//! Fix: extend `reject_refutable_constructor_in_let` in
//! `src/typechecker/inference.rs` to reject these pattern kinds in `let`
//! binding position with a clear "refutable pattern in `let`" diagnostic.
//!
//! Each test below was authored to FAIL against the pre-fix codebase and
//! PASS after the fix.

use silt::typechecker;
use silt::types::Severity;

fn type_errors(input: &str) -> Vec<String> {
    let tokens = silt::lexer::Lexer::new(input)
        .tokenize()
        .expect("lexer error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    let errors = typechecker::check(&mut program);
    errors
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

const ANCHOR: &str = "refutable pattern in `let`";

#[test]
fn test_refutable_let_int_literal_rejected() {
    let errs = type_errors(
        r#"
fn main() {
  let 5 = 10
  println("silent")
}
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains(ANCHOR)),
        "expected refutable-let diagnostic for `let 5 = 10`, got: {errs:?}"
    );
}

#[test]
fn test_refutable_let_float_literal_rejected() {
    let errs = type_errors(
        r#"
fn main() {
  let 1.5 = 2.5
  println("silent")
}
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains(ANCHOR)),
        "expected refutable-let diagnostic for `let 1.5 = 2.5`, got: {errs:?}"
    );
}

#[test]
fn test_refutable_let_bool_literal_rejected() {
    let errs = type_errors(
        r#"
fn main() {
  let true = false
  println("silent")
}
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains(ANCHOR)),
        "expected refutable-let diagnostic for `let true = false`, got: {errs:?}"
    );
}

#[test]
fn test_refutable_let_string_literal_rejected() {
    let errs = type_errors(
        r#"
fn main() {
  let "foo" = "bar"
  println("silent")
}
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains(ANCHOR)),
        "expected refutable-let diagnostic for `let \"foo\" = \"bar\"`, got: {errs:?}"
    );
}

#[test]
fn test_refutable_let_range_pattern_rejected() {
    let errs = type_errors(
        r#"
fn main() {
  let 1..10 = 999
  println("silent")
}
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains(ANCHOR)),
        "expected refutable-let diagnostic for `let 1..10 = 999`, got: {errs:?}"
    );
}

#[test]
fn test_refutable_let_float_range_pattern_rejected() {
    let errs = type_errors(
        r#"
fn main() {
  let 1.0..10.0 = 999.0
  println("silent")
}
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains(ANCHOR)),
        "expected refutable-let diagnostic for `let 1.0..10.0 = 999.0`, got: {errs:?}"
    );
}

#[test]
fn test_refutable_let_pin_pattern_rejected() {
    let errs = type_errors(
        r#"
fn main() {
  let x = 5
  let ^x = 99
  println("silent")
}
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains(ANCHOR)),
        "expected refutable-let diagnostic for `let ^x = 99`, got: {errs:?}"
    );
}

// ── Positive locks: irrefutable patterns must remain legal ──────────

#[test]
fn test_irrefutable_let_tuple_destructure_still_passes() {
    let errs = type_errors(
        r#"
fn main() {
  let (a, b) = (1, 2)
  println(a + b)
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no errors for irrefutable tuple destructure, got: {errs:?}"
    );
}

#[test]
fn test_irrefutable_let_wildcard_still_passes() {
    let errs = type_errors(
        r#"
fn main() {
  let _ = 42
  println("ok")
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no errors for `let _ = 42`, got: {errs:?}"
    );
}

#[test]
fn test_irrefutable_let_ident_still_passes() {
    let errs = type_errors(
        r#"
fn main() {
  let x = 42
  println(x)
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no errors for `let x = 42`, got: {errs:?}"
    );
}

#[test]
fn test_irrefutable_let_nested_tuple_wildcard_still_passes() {
    // Compound destructure of a tuple with a wildcard inside must still
    // be legal — (Wildcard, Ident) is still irrefutable for tuple types.
    let errs = type_errors(
        r#"
fn main() {
  let (_, y) = (1, 2)
  println(y)
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no errors for `let (_, y) = (1, 2)`, got: {errs:?}"
    );
}
