//! Regression tests for the round-81 TYPE-LATENT-2 finding in
//! `src/typechecker/exhaustiveness.rs`:
//!
//! `check_exhaustiveness` previously called `self.apply(...)` (resolve
//! tyvars) on the scrutinee but did NOT call
//! `crate::types::canonical::canonicalize`, so user-declared aliases of
//! empty enums (e.g. `type Void81 {} type Aliased = Void81`) failed to
//! reach the bottom-eliminator short-circuit: `is_uninhabited` looked up
//! `Type::Generic("Aliased", _)` in `self.enums`, found nothing (aliases
//! live elsewhere), and the `arms.is_empty()` short-circuit at
//! `exhaustiveness.rs:63-65` did not fire. Result: spurious
//! "non-exhaustive match" error on `match x { }` where `x: Aliased`.
//!
//! Fix: canonicalise the scrutinee type once, after `apply`, at the
//! entry of `check_exhaustiveness`. Other peer sites
//! (`type_name_for_impl`, `type_args_of`, the `unify` entry) already
//! follow this recipe.
//!
//! These tests pin the fix in three directions:
//!   1. Alias of empty enum + empty match → exhaustive (the bug).
//!   2. Direct empty enum + empty match → still exhaustive (no
//!      regression on the canonical form).
//!   3. Alias of NON-empty enum + empty match → still non-exhaustive
//!      (the fix did not over-fire and accept legitimate gaps).

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

// ── Primary case: alias of an empty enum, empty match is exhaustive ──

#[test]
fn test_alias_of_empty_enum_empty_match_is_exhaustive() {
    // The TYPE-LATENT-2 case: `Aliased` aliases `Void81`. After `apply`
    // the scrutinee is `Type::Generic("Aliased", [])`, which does not
    // appear in `self.enums` (aliases are stored separately). Without
    // canonicalisation, `is_uninhabited` returns false, the
    // `arms.is_empty()` short-circuit doesn't fire, and the usefulness
    // algorithm's `matrix.is_empty() -> wildcard useful` answer
    // produces a spurious non-exhaustive error.
    let errs = hard_errors(
        r#"
type Void81 { }
type Aliased = Void81
fn elim(x: Aliased) -> Int { match x { } }
"#,
    );
    assert!(
        errs.is_empty(),
        "expected zero errors for empty match on an alias of an empty \
         enum (`type Aliased = Void81; match x {{ }}` where `x: Aliased`), \
         got: {errs:?}"
    );
}

// ── Sanity: the canonical form (no alias) still works ────────────────

#[test]
fn test_direct_empty_enum_empty_match_still_exhaustive() {
    // Companion to `tests/empty_type_match_tests.rs::
    // test_empty_enum_empty_match_is_exhaustive`. Replicated here so
    // that this round-81 test file confirms the canonicalisation
    // change did not regress the already-fixed direct case.
    let errs = hard_errors(
        r#"
type Absurd { }
fn elim(x: Absurd) -> Int { match x { } }
"#,
    );
    assert!(
        errs.is_empty(),
        "expected zero errors for empty match on a directly named empty \
         enum (`type Absurd {{ }}; match x {{ }}`), got: {errs:?}"
    );
}

// ── Negative: alias of a NON-empty enum still demands arms ───────────

#[test]
fn test_alias_of_nonempty_enum_empty_match_still_non_exhaustive() {
    // Control — canonicalising the scrutinee must NOT accidentally
    // certify a legitimately non-exhaustive match. `Color` has
    // inhabitants, so the alias `C2 = Color` does too, and an empty
    // match on `C2` must still be flagged non-exhaustive. This
    // confirms the round-81 fix only widens the "uninhabited"
    // recognition — it does not weaken the general check.
    let errs = hard_errors(
        r#"
type Color { Red, Blue }
type C2 = Color
fn pick(x: C2) -> Int { match x { } }
fn main() { let _ = pick(Red) }
"#,
    );
    assert!(
        errs.iter().any(|m| m.contains("non-exhaustive")),
        "expected a non-exhaustive diagnostic for empty match on an \
         alias of a non-empty enum (`type C2 = Color; match x {{ }}` \
         where `x: C2`), got: {errs:?}"
    );
}
