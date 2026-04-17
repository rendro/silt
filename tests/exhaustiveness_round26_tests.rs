//! Regression tests for round-26 findings B1 and B4 in the
//! exhaustiveness checker.
//!
//! - B1: tuple first-columns of a non-scalar non-enum type (records,
//!   nested tuples, etc.) used to pretend specific-value patterns in
//!   the matrix covered the whole column. The fix generalises the
//!   round-16 "infinite scalar" witness-split to any column type whose
//!   `constructors_for_query` cannot faithfully enumerate inhabitants
//!   (everything except `Bool` and known enums).
//!
//! - B4: unit `()` patterns on a `Type::Unit` scrutinee were flagged
//!   non-exhaustive because the `is_wildcard_useful` fallthrough only
//!   treats wildcard/ident rows as covering a column. The fix adds a
//!   `Type::Unit` arm that also accepts the zero-tuple pattern — the
//!   mirror of the round-23 bind/check_pattern unification lock on the
//!   pattern side.
//!
//! Each test drives the real typechecker entry point (`typechecker::check`)
//! so the assertions exercise the same pipeline `silt check` uses.

use silt::typechecker;
use silt::types::Severity;

/// Parse `input` and return the hard (Error-severity) type-check errors.
fn hard_errors(input: &str) -> Vec<String> {
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

// ── B1: record-valued first tuple column ────────────────────────────

#[test]
fn test_b1_record_first_col_specific_value_non_exhaustive() {
    // Only arm covers `(Pair{a:0, b:0}, _)`, but `(Pair{a:1, b:2}, 99)`
    // exists. Pre-fix the checker said clean; runtime panicked with
    // "non-exhaustive match: no arm matched". Post-fix the typechecker
    // must emit a non-exhaustive diagnostic.
    let errs = hard_errors(
        r#"
type Pair { a: Int, b: Int }
fn main() {
  let t = (Pair { a: 1, b: 2 }, 99)
  match t {
    (Pair { a: 0, b: 0 }, _) -> println("zero")
  }
}
"#,
    );
    assert!(
        errs.iter().any(|m| m.contains("non-exhaustive")),
        "expected a non-exhaustive diagnostic for specific-value record \
         first-column tuple match, got: {errs:?}"
    );
}

#[test]
fn test_b1_record_first_col_with_wildcard_arm_is_exhaustive() {
    // Control: adding a catch-all `_` arm must make the match
    // exhaustive (no errors). Guards the witness-split against being
    // too aggressive.
    let errs = hard_errors(
        r#"
type Pair { a: Int, b: Int }
fn main() {
  let t = (Pair { a: 1, b: 2 }, 99)
  match t {
    (Pair { a: 0, b: 0 }, _) -> println("zero")
    _ -> println("other")
  }
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no errors with catch-all arm, got: {errs:?}"
    );
}

#[test]
fn test_b1_record_first_col_covering_record_pattern_is_exhaustive() {
    // Control: a `Pair{..}` arm covering all records in the first
    // column, combined with a wildcard second column, IS exhaustive.
    let errs = hard_errors(
        r#"
type Pair { a: Int, b: Int }
fn main() {
  let t = (Pair { a: 1, b: 2 }, 99)
  match t {
    (Pair { a, b }, _) -> println("any pair")
  }
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no errors with full-record-binding arm, got: {errs:?}"
    );
}

// ── B1: nested tuple first column ───────────────────────────────────

#[test]
fn test_b1_nested_tuple_first_col_specific_value_non_exhaustive() {
    // `match ((1,2), 99) { ((0,0), _) -> ... }` — specific nested
    // tuple value in the first column leaves `((1,2), 99)` uncovered.
    let errs = hard_errors(
        r#"
fn main() {
  let t = ((1, 2), 99)
  match t {
    ((0, 0), _) -> println("origin")
  }
}
"#,
    );
    assert!(
        errs.iter().any(|m| m.contains("non-exhaustive")),
        "expected a non-exhaustive diagnostic for specific-value nested \
         tuple first-column match, got: {errs:?}"
    );
}

#[test]
fn test_b1_nested_tuple_first_col_with_wildcard_arm_is_exhaustive() {
    // Control: a catch-all arm restores exhaustiveness.
    let errs = hard_errors(
        r#"
fn main() {
  let t = ((1, 2), 99)
  match t {
    ((0, 0), _) -> println("origin")
    _ -> println("other")
  }
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no errors with catch-all arm on nested tuple match, \
         got: {errs:?}"
    );
}

// ── B4: unit pattern on Type::Unit scrutinee ────────────────────────

#[test]
fn test_b4_unit_pattern_on_unit_type_type_checks_clean() {
    // `let u: () = ()` followed by `match u { () -> ... }` must not
    // surface a non-exhaustive error (or any other error). Pre-fix
    // the fallthrough "infinite type" arm of `is_wildcard_useful`
    // treated Unit as if it had infinitely many inhabitants and
    // demanded a wildcard.
    let errs = hard_errors(
        r#"
fn main() {
  let u: () = ()
  match u {
    () -> println("done")
  }
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected zero errors for `match unit {{ () -> ... }}`, got: {errs:?}"
    );
}

#[test]
fn test_b4_empty_match_on_unit_type_is_non_exhaustive() {
    // Control: the match must still be flagged if the arm covering
    // the sole unit inhabitant is missing entirely.
    let errs = hard_errors(
        r#"
fn main() {
  let u: () = ()
  match u {
  }
}
"#,
    );
    assert!(
        errs.iter().any(|m| m.contains("non-exhaustive")),
        "expected a non-exhaustive diagnostic for an empty match on \
         Unit, got: {errs:?}"
    );
}
