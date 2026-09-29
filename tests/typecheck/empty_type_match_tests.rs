//! Regression tests for the BROKEN finding in the exhaustiveness checker
//! where `match x { }` on an uninhabited scrutinee type (an enum with
//! zero variants — the "bottom-eliminator" idiom) was wrongly reported
//! as non-exhaustive.
//!
//! The fix (see `check_exhaustiveness` in
//! src/typechecker/exhaustiveness.rs) short-circuits empty matches on an
//! uninhabited type and returns success: no value of that type can ever
//! reach the match, so no pattern is missing.

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

// ── Positive: empty match on an empty enum is exhaustive ─────────────

#[test]
fn test_empty_enum_empty_match_is_exhaustive() {
    // `type Absurd { }` has zero inhabitants, so `match x { }` on it is
    // vacuously exhaustive (bottom-eliminator). Pre-fix the typechecker
    // raised `non-exhaustive match: not all patterns are covered`; post-
    // fix it must accept the match cleanly.
    let errs = hard_errors(
        r#"
type Absurd { }
fn elim(x: Absurd) -> Int { match x { } }
"#,
    );
    assert!(
        errs.is_empty(),
        "expected zero errors for empty match on uninhabited enum, got: {errs:?}"
    );
}

// ── Regression guard: empty match on a non-empty enum still errors ───

#[test]
fn test_nonempty_enum_empty_match_still_non_exhaustive() {
    // Control — the uninhabited short-circuit must NOT accidentally swallow
    // the legitimate non-exhaustive case where the enum has inhabitants
    // but the match has no arms.
    let errs = hard_errors(
        r#"
type NonEmpty { A, B }
fn elim(x: NonEmpty) -> Int { match x { } }
fn main() { let _ = elim(A) }
"#,
    );
    assert!(
        errs.iter().any(|m| m.contains("non-exhaustive")),
        "expected a non-exhaustive diagnostic for empty match on a \
         non-empty enum, got: {errs:?}"
    );
}

// ── Bonus: bottom-eliminator exercised through the full pipeline ─────

#[test]
fn test_bottom_eliminator_function_body_type_checks() {
    // An empty-enum argument used as a bottom-eliminator inside a
    // non-trivial function body must still type-check. This exercises
    // parse + typecheck + exhaustiveness end-to-end.
    let errs = hard_errors(
        r#"
type Absurd { }
fn absurd(x: Absurd) -> Int { match x { } }
fn id(n: Int) -> Int { n }
fn main() { let _ = id(42) }
"#,
    );
    assert!(
        errs.is_empty(),
        "expected zero errors for bottom-eliminator pipeline, got: {errs:?}"
    );
}

// ── Regression guard: a non-empty match on the empty enum is still ok ──

#[test]
fn test_empty_enum_with_wildcard_arm_is_exhaustive() {
    // Control — if the user does write an arm on the empty enum (a
    // wildcard), the match is exhaustive by the normal rules too.
    let errs = hard_errors(
        r#"
type Absurd { }
fn elim(x: Absurd) -> Int { match x { _ -> 0 } }
"#,
    );
    assert!(
        errs.is_empty(),
        "expected zero errors for wildcard arm on empty enum, got: {errs:?}"
    );
}
