//! Regression tests for round-89 BROKEN finding in the exhaustiveness
//! checker.
//!
//! The enumerable-enum first-column specialization in
//! `is_tuple_useful_recursive` performed only a NAME-only match on the
//! first column and dropped the first column's sub-pattern when building
//! the specialized rest matrix. As a result a *refining* sub-pattern like
//! `Yes(true)` collapsed to the same rest-row as `Yes(a)`, hiding the
//! `(Yes(false), _)` witness and wrongly certifying the match exhaustive.
//! `silt check` passed; `silt run` then panicked "non-exhaustive match:
//! no arm matched" on valid input.
//!
//! The fix performs full Maranget specialization in that branch: the
//! first column's sub-patterns become new leading columns, so a refining
//! sub-pattern is no longer treated as covering the whole constructor.
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

// ── Repro 1: refining sub-pattern in the first tuple column ──────────

#[test]
fn test_refining_first_col_subpattern_non_exhaustive() {
    // `(Yes(false), 99)` is uncovered: the only `Yes` arm refines its
    // payload to `true`. Pre-fix the checker dropped the `true`
    // sub-pattern and declared the match exhaustive; runtime then
    // panicked. Post-fix the typechecker must flag it non-exhaustive.
    let errs = hard_errors(
        r#"
type Maybe { Nope, Yes(Bool) }
fn f(x: Maybe, y: Int) -> Int {
  match (x, y) {
    (Nope, _) -> 0
    (Yes(true), _) -> 1
  }
}
fn main() { print(f(Yes(false), 99)) }
"#,
    );
    assert!(
        errs.iter().any(|m| m.contains("non-exhaustive")),
        "expected a non-exhaustive diagnostic, got: {errs:?}"
    );
}

#[test]
fn test_refining_first_col_subpattern_exhaustive_with_missing_arm() {
    // Adding `(Yes(false), _)` makes the match genuinely exhaustive: the
    // checker must NOT emit a non-exhaustive (or any) error.
    let errs = hard_errors(
        r#"
type Maybe { Nope, Yes(Bool) }
fn f(x: Maybe, y: Int) -> Int {
  match (x, y) {
    (Nope, _) -> 0
    (Yes(true), _) -> 1
    (Yes(false), _) -> 2
  }
}
fn main() { print(f(Yes(false), 99)) }
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no errors on a genuinely-exhaustive match, got: {errs:?}"
    );
}

// ── Repro 2: refinement in BOTH columns leaves a nested witness ──────

#[test]
fn test_refining_both_cols_non_exhaustive() {
    // Arms `(Nope,_) | (Yes(a),Nope) | (Yes(true),Yes(b))` leave
    // `(Yes(false), Yes(_))` uncovered. Pre-fix the name-only first-col
    // match hid this; post-fix it must surface as non-exhaustive.
    let errs = hard_errors(
        r#"
type Maybe { Nope, Yes(Bool) }
fn f(x: Maybe, y: Maybe) -> Int {
  match (x, y) {
    (Nope, _) -> 0
    (Yes(a), Nope) -> 1
    (Yes(true), Yes(b)) -> 2
  }
}
fn main() { print(f(Yes(false), Yes(true))) }
"#,
    );
    assert!(
        errs.iter().any(|m| m.contains("non-exhaustive")),
        "expected a non-exhaustive diagnostic, got: {errs:?}"
    );
}

#[test]
fn test_refining_both_cols_exhaustive_when_completed() {
    // Adding `(Yes(false), Yes(b))` closes the gap from repro 2.
    let errs = hard_errors(
        r#"
type Maybe { Nope, Yes(Bool) }
fn f(x: Maybe, y: Maybe) -> Int {
  match (x, y) {
    (Nope, _) -> 0
    (Yes(a), Nope) -> 1
    (Yes(true), Yes(b)) -> 2
    (Yes(false), Yes(b)) -> 3
  }
}
fn main() { print(f(Yes(false), Yes(true))) }
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no errors on a genuinely-exhaustive match, got: {errs:?}"
    );
}
