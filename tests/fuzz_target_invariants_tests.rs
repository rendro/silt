//! Regression tests for the fuzz-target invariant helpers in
//! `silt::fuzz_invariants`. These exercise the structural checks that
//! `fuzz/fuzz_targets/fuzz_lexer.rs` and `fuzz/fuzz_targets/fuzz_formatter.rs`
//! now enforce on every fuzz input.
//!
//! Each synthetic "corrupted output" below is crafted so that the OLD
//! fuzz targets (which only checked non-panic / idempotency) would have
//! silently accepted it, while the new invariants reject it.

use silt::formatter;
use silt::fuzz_invariants::{check_formatter_invariants, check_lexer_invariants};
use silt::lexer::{Lexer, Span, Token};

// --------------------------------------------------------------------
// Lexer invariants
// --------------------------------------------------------------------

#[test]
fn lexer_invariants_accept_real_tokenization() {
    let src = "let x = 1 + 2\nfn main() = x\n";
    let tokens = Lexer::new(src).tokenize().unwrap();
    check_lexer_invariants(src, &tokens).expect("real source must satisfy invariants");
}

#[test]
fn lexer_invariants_reject_missing_eof() {
    // Synthesize a token stream without the terminating Eof. The old
    // fuzz target never looked at the tokens at all; the new one
    // demands Eof as the last element.
    let src = "x";
    let tokens = vec![(Token::Ident(silt::intern::intern("x")), Span::new(1, 1))];
    let err = check_lexer_invariants(src, &tokens).unwrap_err();
    assert!(err.contains("Eof"), "unexpected error: {err}");
}

#[test]
fn lexer_invariants_reject_offset_past_source() {
    let src = "x";
    let tokens = vec![
        (
            Token::Ident(silt::intern::intern("x")),
            Span::with_offset(1, 1, 0),
        ),
        // Eof claiming an offset past the end of source — would be a
        // silent bug in a real lexer; the old fuzz driver never noticed.
        (Token::Eof, Span::with_offset(1, 99, 99)),
    ];
    let err = check_lexer_invariants(src, &tokens).unwrap_err();
    assert!(
        err.contains("beyond source length") || err.contains("Eof span offset"),
        "unexpected error: {err}"
    );
}

#[test]
fn lexer_invariants_reject_non_monotonic_offsets() {
    let src = "ab";
    let tokens = vec![
        (
            Token::Ident(silt::intern::intern("a")),
            Span::with_offset(1, 1, 1),
        ),
        (
            Token::Ident(silt::intern::intern("b")),
            // Rewound offset — a real lexer bug would look like this if
            // it accidentally reset position state between tokens.
            Span::with_offset(1, 2, 0),
        ),
        (Token::Eof, Span::with_offset(1, 3, 2)),
    ];
    let err = check_lexer_invariants(src, &tokens).unwrap_err();
    assert!(err.contains("non-monotonic"), "unexpected error: {err}");
}

#[test]
fn lexer_invariants_reject_token_after_eof() {
    let src = "x";
    let tokens = vec![
        (
            Token::Ident(silt::intern::intern("x")),
            Span::with_offset(1, 1, 0),
        ),
        (Token::Eof, Span::with_offset(1, 2, 1)),
        // Bogus extra token after Eof.
        (Token::Plus, Span::with_offset(1, 3, 1)),
    ];
    let err = check_lexer_invariants(src, &tokens).unwrap_err();
    assert!(err.contains("after Eof"), "unexpected error: {err}");
}

// --------------------------------------------------------------------
// Formatter invariants
// --------------------------------------------------------------------

#[test]
fn formatter_invariants_accept_identity() {
    let src = "let x = 1\n";
    check_formatter_invariants(src, src).expect("identity must pass");
}

#[test]
fn formatter_invariants_accept_real_formatter_output() {
    // Exercise the invariant on the real formatter's output so we catch
    // any case where the invariant is accidentally over-strict.
    let src = "let   x=1\nlet y  =   2\n";
    let formatted = formatter::format(src).expect("source must format");
    check_formatter_invariants(src, &formatted)
        .expect("real formatter output must satisfy invariants");
}

#[test]
fn formatter_invariants_reject_dropped_rparen() {
    // A formatter bug that drops the closing paren would still be
    // idempotent (running it again produces the same broken output),
    // so the OLD fuzz_formatter would have accepted it. The new
    // delimiter-balance invariant catches it immediately.
    let original = "let x = (1 + 2)\n";
    let corrupted = "let x = (1 + 2\n";
    let err = check_formatter_invariants(original, corrupted).unwrap_err();
    assert!(
        err.contains("delimiter balance") || err.contains("significant token count"),
        "unexpected error: {err}"
    );
}

#[test]
fn formatter_invariants_reject_dropped_comment() {
    let original = "-- important\nlet x = 1\n";
    let corrupted = "let x = 1\n";
    let err = check_formatter_invariants(original, corrupted).unwrap_err();
    assert!(
        err.contains("comment marker count") || err.contains("significant token count"),
        "unexpected error: {err}"
    );
}

#[test]
fn formatter_invariants_reject_dropped_block_comment_open() {
    let original = "{- note -}\nlet x = 1\n";
    // Simulated formatter bug: one `{-` marker silently deleted. The
    // original's block comment is skipped by the lexer entirely, but
    // the corrupted version leaks `note - }` into the token stream,
    // which the significant-token / delimiter / comment-marker checks
    // together must catch.
    let corrupted = "note -}\nlet x = 1\n";
    let err = check_formatter_invariants(original, corrupted).unwrap_err();
    assert!(
        err.contains("comment marker count")
            || err.contains("delimiter balance")
            || err.contains("significant token count"),
        "unexpected error: {err}"
    );
}

#[test]
fn formatter_invariants_reject_dropped_token() {
    // Original has 6 significant tokens (ignoring whitespace); corrupted
    // drops the `+ 2`, leaving 4.
    let original = "let x = 1 + 2\n";
    let corrupted = "let x = 1\n";
    let err = check_formatter_invariants(original, corrupted).unwrap_err();
    assert!(
        err.contains("significant token count"),
        "unexpected error: {err}"
    );
}

#[test]
fn formatter_invariants_reject_unbalanced_braces() {
    let original = "fn f() = { 1 + 2 }\n";
    // Corrupted output drops the closing brace.
    let corrupted = "fn f() = { 1 + 2 \n";
    let err = check_formatter_invariants(original, corrupted).unwrap_err();
    assert!(
        err.contains("delimiter balance") || err.contains("significant token count"),
        "unexpected error: {err}"
    );
}

#[test]
fn formatter_invariants_allow_whitespace_reshaping() {
    // The formatter is allowed to legitimately reshape whitespace and
    // blank lines; the invariant must not fire in that case.
    let original = "let   x=1\n\n\nlet y=2\n";
    let normalized = "let x = 1\n\nlet y = 2\n";
    check_formatter_invariants(original, normalized)
        .expect("whitespace reshaping must be permitted");
}

#[test]
fn formatter_invariants_reject_unparseable_output_when_original_parsed() {
    // The original is a valid program; the "formatted" output is not.
    // This catches any formatter bug that corrupts structure enough to
    // keep the token count right but break parsing.
    let original = "let x = 1\n";
    // Same token count, same balance, same comment markers — but
    // rearranged into a syntactically invalid form.
    let corrupted = "= let x 1\n";
    let result = check_formatter_invariants(original, corrupted);
    // Either the significant-token / parse check fires.
    assert!(result.is_err(), "expected invariant failure");
}

#[test]
fn formatter_invariants_allow_comma_canonicalization() {
    // silt's parser accepts comma-less parameter and element lists, and
    // the formatter canonicalizes by inserting explicit commas. The
    // invariant must not flag this as a dropped/added token. Locks the
    // deliberate exclusion of `Comma` from `significant_token_count` —
    // if a future edit re-includes commas in the count, this test fails.
    let original = "fn f(a b c) { [1 2 3] }\n";
    let formatted = "fn f(a, b, c) { [1, 2, 3] }\n";
    check_formatter_invariants(original, formatted)
        .expect("comma canonicalization must be permitted");
}
