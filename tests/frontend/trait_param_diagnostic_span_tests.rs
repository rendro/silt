//! Round-52 deferred item 2: `TypeExpr` now carries a `Span`, so the
//! three trait-header parameter diagnostics in `parse_trait_or_impl`
//! anchor their caret at the offending argument's own span rather than
//! at the outer `trait` keyword (which was always col 1).
//!
//! These tests pin each diagnostic's column to the real column of the
//! offending token in the source. A regression that dropped the span
//! back to the outer opener would collapse all three carets to col 1
//! and these tests would fail.
//!
//! Fixtures are single-line source strings so column math is
//! unambiguous: we compute the expected column by scanning for the
//! offending substring in the fixture.

use silt::lexer::Lexer;
use silt::parser::{ParseError, Parser};

/// Run just the parser and collect the first error (or panic if parse
/// unexpectedly succeeds). The three trait-header checks fire from
/// `parse_trait_or_impl` and bail on the first mismatch, so a single
/// `parse_program()` is sufficient to surface them.
fn first_parse_error(src: &str) -> ParseError {
    let tokens = Lexer::new(src).tokenize().expect("lex");
    Parser::new(tokens)
        .parse_program()
        .err()
        .expect("expected parse error")
}

/// Compute the 1-based column of the first occurrence of `needle` on
/// the first line of `src`. Mirrors the lexer's column convention
/// (1-based). Panics if not found so a typo in the test fixture
/// surfaces immediately.
fn col_of(src: &str, needle: &str) -> usize {
    let line = src.lines().next().expect("empty source");
    let idx = line
        .find(needle)
        .unwrap_or_else(|| panic!("fixture missing needle {needle:?} on first line: {line:?}"));
    idx + 1
}

/// Fixture 1: `trait Foo(Int) { ... }` — the argument `Int` is
/// uppercase and therefore not a lowercase type variable. The
/// diagnostic MUST point at `Int` (col 11), not at `trait` (col 1).
#[test]
fn trait_param_uppercase_points_at_arg() {
    let src = "trait Foo(Int) { fn bar() }";
    let err = first_parse_error(src);
    let expected_col = col_of(src, "Int");
    assert!(
        err.message.contains("must be a lowercase type variable"),
        "expected lowercase-type-var message, got: {}",
        err.message,
    );
    assert_eq!(
        err.span.col, expected_col,
        "expected caret at col {} (the `Int`), got col {} — span anchor regressed to the outer `trait` keyword? message={}",
        expected_col, err.span.col, err.message,
    );
    assert_eq!(err.span.line, 1);
}

/// Fixture 2: `trait Foo(A, A) { ... }` — duplicate type variable
/// 'A'. Even though neither `A` is lowercase (so the uppercase check
/// would also fire), the order of checks in `parse_trait_or_impl`
/// emits the uppercase diagnostic first for the first `A`. This test
/// pins the uppercase fail on the FIRST `A` — the span must point at
/// col 11, not col 1.
#[test]
fn trait_param_first_uppercase_points_at_first_arg() {
    let src = "trait Foo(A, A) { fn bar() }";
    let err = first_parse_error(src);
    // The first `A` fails the uppercase check (duplicate check runs
    // later in the per-arg loop, so it only fires if the arg passes
    // the uppercase gate). We assert the caret is at the first `A`.
    let expected_col = col_of(src, "A");
    assert!(
        err.message.contains("must be a lowercase type variable"),
        "expected lowercase-type-var message, got: {}",
        err.message,
    );
    assert_eq!(
        err.span.col, expected_col,
        "expected caret at col {} (the first `A`), got col {} — span anchor regressed",
        expected_col, err.span.col,
    );
}

/// Fixture 3: `trait Foo(a, a) { ... }` — duplicate type variable
/// 'a'. Both args are lowercase so they pass the uppercase gate; the
/// duplicate check then fires on the SECOND `a`. The span must point
/// at the second `a` (col 14), not at col 1.
#[test]
fn trait_param_duplicate_points_at_second_arg() {
    let src = "trait Foo(a, a) { fn bar() }";
    let err = first_parse_error(src);
    // Compute the 1-based column of the SECOND `a` (the trait-param
    // `a`, not the `a` inside the `trait` keyword). Anchor to the
    // `(` so we start the scan past `trait Foo`.
    let line = src.lines().next().unwrap();
    let lparen = line.find('(').unwrap();
    let first_after_paren = lparen + 1 + line[lparen + 1..].find('a').unwrap();
    let second_after_paren =
        first_after_paren + 1 + line[first_after_paren + 1..].find('a').unwrap();
    let expected_col = second_after_paren + 1;
    assert!(
        err.message.contains("duplicate type variable 'a'"),
        "expected duplicate-type-var message, got: {}",
        err.message,
    );
    assert_eq!(
        err.span.col, expected_col,
        "expected caret at col {} (the second `a`), got col {} — span anchor regressed",
        expected_col, err.span.col,
    );
    // Sanity: the caret MUST NOT be at col 1 (the outer `trait`
    // keyword). This is the pre-round-52 regression we're guarding
    // against.
    assert_ne!(
        err.span.col, 1,
        "caret regressed to col 1 (the outer `trait` keyword)",
    );
}

/// Fixture 4: `trait Foo(Bar) { ... }` — capitalized identifier that
/// is not a type variable. The diagnostic span must point at `Bar`
/// (col 11), not at the outer `trait`.
#[test]
fn trait_param_capitalized_nonvar_points_at_arg() {
    let src = "trait Foo(Bar) { fn bar() }";
    let err = first_parse_error(src);
    let expected_col = col_of(src, "Bar");
    assert!(
        err.message.contains("must be a lowercase type variable"),
        "expected lowercase-type-var message, got: {}",
        err.message,
    );
    assert_eq!(
        err.span.col, expected_col,
        "expected caret at col {} (the `Bar`), got col {}",
        expected_col, err.span.col,
    );
}

/// Positive control: well-formed trait decl parses cleanly. Pins
/// that the new per-arg span machinery didn't spuriously reject the
/// happy path.
#[test]
fn trait_param_well_formed_parses_cleanly() {
    let src = "trait Foo(a, b) { fn bar() }";
    let tokens = Lexer::new(src).tokenize().expect("lex");
    let result = Parser::new(tokens).parse_program();
    assert!(
        result.is_ok(),
        "expected clean parse for well-formed trait decl, got error: {:?}",
        result.err(),
    );
}
