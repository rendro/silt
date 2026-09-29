//! Regression tests for round-52 deferred item 3: when an unclosed `[`,
//! `(`, or `{` is followed on the next line by a top-level `fn NAME(...)`
//! declaration, the parser previously blamed the `fn NAME` site
//! (e.g. "expected (, found b") instead of the unclosed opener.
//!
//! Fix: `parse_expr_in_delim` now pre-checks for `fn IDENT`, which is
//! unambiguously a top-level fn decl (anon-fn expressions must be
//! `fn(...)`). In that case we surface the unclosed-delimiter error
//! pointing at the opener span rather than letting parse_fn_expr
//! consume the `fn` and trip on the trailing identifier.
//!
//! These tests lock:
//!   1. unclosed `[` + `fn NAME` → list-literal error mentioning the
//!      opener line,
//!   2. unclosed `[` + EOF (existing behavior — positive lock that the
//!      pre-existing path still fires),
//!   3. positive: a list of anonymous fns `[fn() = 1, fn() = 2]` still
//!      parses cleanly,
//!   4. positive: `[1, 2, fn() = 3]` still parses cleanly,
//!   5. unclosed `(` + `fn NAME` → tuple/call-arg error mentions opener,
//!   6. unclosed `{` (record-literal-style hashed opener `#{`) + `fn NAME`
//!      → map literal error mentions opener.

use silt::lexer::Lexer;
use silt::parser::Parser;

/// Parse non-recovering and return the single error (panics on success
/// when we expected failure).
fn parse_err(input: &str) -> String {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    match Parser::new(tokens).parse_program() {
        Ok(_) => panic!("expected parse error, got success"),
        Err(e) => format!("{}: {}", e.span, e.message),
    }
}

/// Parse and assert success.
fn parse_ok(input: &str) {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    if let Err(e) = Parser::new(tokens).parse_program() {
        panic!(
            "expected clean parse, got error: {} at {}",
            e.message, e.span
        );
    }
}

#[test]
fn unclosed_list_then_fn_decl_blames_opener() {
    // Opener `[` is at line 1, col 10 (after `fn a() = `). The follow-up
    // `fn b() = 99` on the next line used to surface as
    // "expected (, found b" at 2:4. With the fix we point back at the
    // list opener.
    let src = "fn a() = [1, 2,\nfn b() = 99\n";
    let err = parse_err(src);
    assert!(
        err.contains("list literal"),
        "expected list-literal opener mention, got: {err}"
    );
    assert!(
        err.contains("starting at line 1"),
        "expected opener line reference, got: {err}"
    );
    // The old diagnostic pointed at `b` (2:4) with "expected (, found b".
    // Lock that we no longer produce that text.
    assert!(
        !err.contains("expected (, found b"),
        "regressed to pre-fix diagnostic: {err}"
    );
}

#[test]
fn unclosed_list_at_eof_still_reports_opener() {
    // Positive lock on the existing clean-EOF case — the fix must not
    // change this path.
    let src = "fn a() = [1, 2,\n";
    let err = parse_err(src);
    assert!(
        err.contains("list literal"),
        "expected list-literal mention, got: {err}"
    );
    assert!(
        err.contains("starting at line 1"),
        "expected opener line reference, got: {err}"
    );
}

#[test]
fn list_of_anon_fns_parses_cleanly() {
    // Positive lock: genuine anon-fn-in-list shape must still parse.
    // (Anon-fn expression shape is `fn(...) { body }`.)
    let src = "fn a() = [fn() { 1 }, fn() { 2 }]\n";
    parse_ok(src);
}

#[test]
fn list_with_trailing_anon_fn_single_line_parses_cleanly() {
    // Positive lock: single-line list ending with an anon-fn element.
    let src = "fn a() = [1, 2, fn() { 3 }]\n";
    parse_ok(src);
}

#[test]
fn unclosed_paren_then_fn_decl_blames_opener() {
    // Unclosed tuple / call-arg paren followed by a top-level fn decl.
    // Previously parse_fn_expr would consume `fn` and fail on the ident.
    let src = "fn a() = (1, 2,\nfn b() = 99\n";
    let err = parse_err(src);
    assert!(
        err.contains("starting at line 1"),
        "expected opener line reference, got: {err}"
    );
    assert!(
        !err.contains("expected (, found b"),
        "regressed to pre-fix diagnostic: {err}"
    );
}

#[test]
fn unclosed_map_then_fn_decl_blames_opener() {
    // `#{` map literal opener followed by a top-level fn decl after a
    // trailing comma should blame the map opener.
    let src = "fn a() = #{1: 2,\nfn b() = 99\n";
    let err = parse_err(src);
    assert!(
        err.contains("map literal"),
        "expected map-literal mention, got: {err}"
    );
    assert!(
        err.contains("starting at line 1"),
        "expected opener line reference, got: {err}"
    );
}
