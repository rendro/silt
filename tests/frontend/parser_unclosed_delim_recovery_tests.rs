//! Positive locks for round-52 deferred item 3: the parser's
//! `fn IDENT` pre-check inside delimiters (which makes an unclosed
//! opener followed by a top-level `fn NAME` blame the opener) must not
//! reject anonymous `fn() { ... }` elements. The negative cases live in
//! `tests/golden/frontend/parser/parser_unclosed_delim_recovery__*.silt`.
//! These two stay in Rust until stage 4 removes `fn` lambdas.

use silt::lexer::Lexer;
use silt::parser::Parser;

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
