//! Round 100 GAP regression lock — binary-operator type-mismatch
//! diagnostic DIRECTION.
//!
//! All four binop arms used to call `unify(&lt, &rt, span)` while
//! `unify(t1, t2)` renders "type mismatch: expected {t2}, got {t1}" —
//! so the not-yet-read RIGHT operand was always cast as the
//! expectation. `1 + true` said "expected Bool, got Int" (caret on the
//! `1`): nothing expects Bool; the Bool is the offender. `true + 1`
//! only rendered correctly because the offender happened to sit on the
//! left.
//!
//! The fix (`TypeChecker::unify_binop_operands`,
//! src/typechecker/inference.rs):
//!   * the LEFT operand is inferred first and establishes the
//!     expectation — on mismatch the right operand is the "got" side,
//!     anchored at its own span;
//!   * when exactly one resolved operand is outside the operator's
//!     domain, the operand-domain message (which names the true
//!     offender regardless of side) replaces the generic mismatch,
//!     aimed at the offender's span — inverting the round-67 F1
//!     priority while preserving its single-diagnostic invariant
//!     (tests/binop_single_diagnostic_round67_tests.rs).
//!
//! Each test also re-asserts the round-67 invariant: exactly ONE of
//! (type mismatch | operator-domain) per bad binop.

use silt::lexer::{Lexer, Span};
use silt::parser::Parser;
use silt::typechecker;
use silt::types::Severity;

fn type_errors(input: &str) -> Vec<(String, Span)> {
    let tokens = Lexer::new(input).tokenize().expect("lexer");
    let mut program = Parser::new(tokens).parse_program().expect("parse");
    typechecker::check(&mut program)
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| (e.message, e.span))
        .collect()
}

/// Round-67 invariant: exactly one of (mismatch | operator-domain).
fn assert_single_binop_diagnostic(errs: &[(String, Span)], op_label: &str) {
    let mismatches = errs
        .iter()
        .filter(|(m, _)| m.contains("type mismatch:"))
        .count();
    let domain = errs
        .iter()
        .filter(|(m, _)| m.contains(&format!("operator {op_label} requires")))
        .count();
    assert_eq!(
        mismatches + domain,
        1,
        "round-67 single-diagnostic invariant violated for {op_label}: \
         mismatch={mismatches}, domain={domain}, all errors:\n{errs:?}"
    );
}

// ── Right-side offender, out of operator domain ─────────────────────
//
// Pre-fix: "type mismatch: expected Bool, got Int" — inverted. Post-fix
// the operand-domain message names the Bool offender, caret on `true`.

#[test]
fn test_int_plus_bool_names_the_bool_offender() {
    let errs = type_errors(
        r#"
fn main() {
  let x = 1 + true
  print(x)
}
"#,
    );
    assert_single_binop_diagnostic(&errs, "'+'");
    assert!(
        !errs
            .iter()
            .any(|(m, _)| m.contains("expected Bool, got Int")),
        "inverted-direction mismatch resurfaced (nothing expects Bool in `1 + true`):\n{errs:?}"
    );
    let (msg, span) = errs
        .iter()
        .find(|(m, _)| {
            m.contains("operator '+' requires Int, Float, ExtFloat, or String, got 'Bool'")
        })
        .expect("expected the operand-domain diagnostic naming the Bool offender");
    // Caret must land on the offending RIGHT operand (`true`, col 15 of
    // `  let x = 1 + true`), not on the `1` (col 11) as pre-fix.
    assert_eq!(span.line, 3, "caret should be on the binop line: {msg}");
    assert!(
        span.col >= 13,
        "caret should land on the right operand `true` (col >= 13), got col {}: {msg}",
        span.col
    );
}

// ── Left-side offender, out of operator domain ──────────────────────
//
// Pre-fix this rendered "expected Int, got Bool" — directionally fine
// by accident. Post-fix the operand-domain message names the offender
// explicitly, caret on `true` (the LEFT operand this time).

#[test]
fn test_bool_plus_int_names_the_bool_offender() {
    let errs = type_errors(
        r#"
fn main() {
  let x = true + 1
  print(x)
}
"#,
    );
    assert_single_binop_diagnostic(&errs, "'+'");
    let (msg, span) = errs
        .iter()
        .find(|(m, _)| {
            m.contains("operator '+' requires Int, Float, ExtFloat, or String, got 'Bool'")
        })
        .expect("expected the operand-domain diagnostic naming the Bool offender");
    // Caret must land on the offending LEFT operand (`true`, col 11).
    assert_eq!(span.line, 3, "caret should be on the binop line: {msg}");
    assert!(
        span.col <= 12,
        "caret should land on the left operand `true` (col <= 12), got col {}: {msg}",
        span.col
    );
}

// ── Both operands in-domain: first-seen operand sets the expectation ─

#[test]
fn test_int_plus_string_expectation_is_left_operand() {
    // Both Int and String are valid `+` operands (addition /
    // concatenation), so this stays a mismatch — but directed
    // left-to-right: `1` establishes Int, `"hi"` is the "got" side.
    let errs = type_errors(
        r#"
fn main() {
  let x = 1 + "hi"
  print(x)
}
"#,
    );
    assert_single_binop_diagnostic(&errs, "'+'");
    assert!(
        errs.iter()
            .any(|(m, _)| m.contains("type mismatch: expected Int, got String")),
        "expected 'type mismatch: expected Int, got String' for `1 + \"hi\"`, got:\n{errs:?}"
    );
    assert!(
        !errs
            .iter()
            .any(|(m, _)| m.contains("expected String, got Int")),
        "inverted-direction mismatch resurfaced for `1 + \"hi\"`:\n{errs:?}"
    );
}

#[test]
fn test_int_lt_string_expectation_is_left_operand() {
    // Both Int and String are orderable, so `<` reports a mismatch —
    // directed left-to-right.
    let errs = type_errors(
        r#"
fn main() {
  let x = (1 < "a")
  print(x)
}
"#,
    );
    assert_single_binop_diagnostic(&errs, "'<'");
    assert!(
        errs.iter()
            .any(|(m, _)| m.contains("type mismatch: expected Int, got String")),
        "expected 'type mismatch: expected Int, got String' for `1 < \"a\"`, got:\n{errs:?}"
    );
    assert!(
        !errs
            .iter()
            .any(|(m, _)| m.contains("expected String, got Int")),
        "inverted-direction mismatch resurfaced for `1 < \"a\"`:\n{errs:?}"
    );
}

#[test]
fn test_int_eq_string_expectation_is_left_operand() {
    // Both Int and String support equality, so `==` reports a mismatch
    // — directed left-to-right.
    let errs = type_errors(
        r#"
fn main() {
  let x = (1 == "a")
  print(x)
}
"#,
    );
    assert_single_binop_diagnostic(&errs, "'=='");
    assert!(
        errs.iter()
            .any(|(m, _)| m.contains("type mismatch: expected Int, got String")),
        "expected 'type mismatch: expected Int, got String' for `1 == \"a\"`, got:\n{errs:?}"
    );
    assert!(
        !errs
            .iter()
            .any(|(m, _)| m.contains("expected String, got Int")),
        "inverted-direction mismatch resurfaced for `1 == \"a\"`:\n{errs:?}"
    );
}

// ── Right-side offender under an ordering comparison ────────────────

#[test]
fn test_int_lt_bool_names_the_bool_offender() {
    // Bool is NOT orderable, so the domain message must name it —
    // pre-fix this said "expected Bool, got Int".
    let errs = type_errors(
        r#"
fn main() {
  let x = (1 < true)
  print(x)
}
"#,
    );
    assert_single_binop_diagnostic(&errs, "'<'");
    assert!(
        !errs
            .iter()
            .any(|(m, _)| m.contains("expected Bool, got Int")),
        "inverted-direction mismatch resurfaced for `1 < true`:\n{errs:?}"
    );
    assert!(
        errs.iter()
            .any(|(m, _)| m.contains("operator '<' requires") && m.contains("got 'Bool'")),
        "expected the ordering-domain diagnostic naming the Bool offender, got:\n{errs:?}"
    );
}

// ── Wrapper offender keeps the chain hint on the domain message ─────

#[test]
fn test_option_offender_domain_message_keeps_chain_hint() {
    // `Option(Int) + Int`: the Option is the lone out-of-domain
    // operand, so the domain message wins — but the `?` / `flat_map`
    // guidance from `chain_hint` must survive the replacement.
    let errs = type_errors(
        r#"
import list

fn main() {
  let xs: List(Int) = [1, 2, 3]
  let head = list.head(xs)
  let y = head + 1
  print(y)
}
"#,
    );
    let (msg, _) = errs
        .iter()
        .find(|(m, _)| m.contains("operator '+' requires") && m.contains("Option"))
        .expect("expected the operand-domain diagnostic naming the Option offender");
    assert!(
        msg.contains("help: to chain through an `Option`"),
        "chain hint must be preserved on the domain replacement, got: {msg}"
    );
}

// ── Source lock: the misdirected call form must not return ──────────

#[test]
fn source_lock_no_misdirected_binop_unify_call() {
    let src = include_str!("../src/typechecker/inference.rs");
    assert!(
        !src.contains("self.unify(&lt, &rt, span)"),
        "binop arms must route operand unification through \
         `unify_binop_operands` (left operand establishes the \
         expectation), not the old direction-inverting \
         `self.unify(&lt, &rt, span)` call"
    );
    assert!(
        src.contains("fn unify_binop_operands"),
        "`unify_binop_operands` helper disappeared from \
         src/typechecker/inference.rs — if it was renamed, re-aim this \
         lock and tests/binop_single_diagnostic_round67_tests.rs"
    );
}
