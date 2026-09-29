//! Regression tests for round-23 finding #1: unit pattern / unit type
//! asymmetry. The parser emits `PatternKind::Tuple(vec![])` for the
//! surface syntax `()` pattern. `resolve_type_expr` normalizes the
//! empty tuple type expr to `Type::Unit`. Before this fix,
//! `bind_pattern` / `check_pattern` unified the scrutinee against
//! `Type::Tuple(vec![])` instead of `Type::Unit`, producing the
//! nonsense diagnostic "type mismatch: expected (), got ()" whenever a
//! unit-returning function was destructured with `let () = f()`.
//!
//! These tests assert on the raw typechecker error messages — with the
//! fix in place, `let () = f()` type-checks clean against a
//! `Type::Unit`-returning body. Without the fix, the "type mismatch"
//! error re-surfaces at the scrutinee span.

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

#[test]
fn test_unit_pattern_on_unit_returning_call_type_checks_clean() {
    // The repro from round-23 finding #1. `work` returns Unit (empty
    // body), `let () = work()` is the canonical unit destructure — it
    // must not surface a type mismatch.
    let errs = type_errors(
        r#"
fn work() { }
fn main() {
  let () = work()
  println("done")
}
"#,
    );
    // With the fix: no errors. Without the fix: "type mismatch:
    // expected (), got ()" from the scrutinee-vs-tuple unify.
    assert!(
        errs.is_empty(),
        "expected zero errors for `let () = unit_fn()`, got: {errs:?}"
    );
}

#[test]
fn test_unit_pattern_in_match_on_unit_type_has_no_type_mismatch() {
    // `match () { () -> ... }` — check_pattern side of the same fix.
    // The `()` arm pattern must unify against Type::Unit, not a
    // zero-tuple type. Older builds produced the bogus "type mismatch:
    // expected (), got ()" at the pattern site. Exhaustiveness may
    // still flag the match as non-exhaustive (Unit isn't modeled in
    // the exhaustiveness checker), but that's a separate concern;
    // here we just lock the fact that the unify no longer emits a
    // bogus `type mismatch` at the `()` pattern.
    let errs = type_errors(
        r#"
fn main() {
  let x = ()
  match x { () -> println("hi") }
}
"#,
    );
    let bogus = errs
        .iter()
        .any(|m| m.contains("type mismatch") && m.contains("()"));
    assert!(
        !bogus,
        "expected NO 'type mismatch' diagnostic on `()` pattern vs Unit scrutinee, got: {errs:?}"
    );
}

#[test]
fn test_nonempty_tuple_mismatch_still_errors() {
    // Regression: the fix must only special-case `pats.is_empty()`.
    // Non-empty tuple patterns against unit must still error out.
    let errs = type_errors(
        r#"
fn main() {
  let (a, b) = ()
  println(a)
}
"#,
    );
    assert!(
        !errs.is_empty(),
        "expected a type error for `let (a, b) = ()`, got none"
    );
    // Narrow lock: for `let (a, b) = ()`, the scrutinee type `()`
    // resolves to `Type::Unit`, which is NOT a `Type::Tuple`, so the
    // non-tuple branch in src/typechecker/inference.rs
    // (PatternKind::Tuple) unifies Unit against a fresh tuple shape
    // `(_, _)`. That triggers the general unify diagnostic, which
    // renders as "type mismatch: expected (_, _), got ()". The old
    // OR included "tuple length mismatch" — but that message is only
    // emitted when the scrutinee is already a Tuple with a differing
    // arity (e.g. `let (a, b) = (1, 2, 3)`); against Unit the arity
    // branch never fires, so that arm was dead. Pin the pattern-shape
    // fragment `expected (_, _)` so a regression that silently
    // accepts a non-tuple scrutinee (the round-15 BROKEN behavior)
    // would fail this assertion.
    let hit_mismatch = errs.iter().any(|m| m.contains("expected (_, _)"));
    assert!(
        hit_mismatch,
        "expected `type mismatch: expected (_, _), got ()` diagnostic, got: {errs:?}"
    );
}
