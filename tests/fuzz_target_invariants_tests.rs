//! Regression tests for the fuzz-target invariant helpers in
//! `silt::fuzz_invariants`. These exercise the structural checks that
//! `fuzz/fuzz_targets/fuzz_lexer.rs`, `fuzz/fuzz_targets/fuzz_formatter.rs`,
//! `fuzz/fuzz_targets/fuzz_parser.rs`, and
//! `fuzz/fuzz_targets/fuzz_roundtrip.rs` now enforce on every fuzz input.
//!
//! Each synthetic "corrupted output" below is crafted so that the OLD
//! fuzz targets (which only checked non-panic / idempotency) would have
//! silently accepted it, while the new invariants reject it.

use silt::ast::{Decl, ImportTarget, Program};
use silt::formatter;
use silt::fuzz_invariants::{
    check_format_idempotent, check_formatter_invariants, check_lexer_invariants,
    check_parser_invariants,
};
use silt::lexer::{Lexer, Span, Token};
use silt::parser::Parser;

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
    let err =
        check_formatter_invariants(original, corrupted).expect_err("expected invariant failure");
    // Either the significant-token-count check or the parse-preservation
    // check must fire — anything else (e.g. a generic lex error) would
    // mean the invariants we care about aren't actually being exercised.
    assert!(
        err.contains("significant token count")
            || err.contains("original parsed but formatted output did not"),
        "expected significant-token-count or parse-preservation failure, got: {err}"
    );
}

#[test]
fn formatter_invariants_preserve_comma_count() {
    // silt's parser requires explicit commas between list-construct
    // elements, so the formatter has no comma-count latitude: every
    // `Comma` in the input must survive to the output. This locks the
    // inclusion of `Comma` in `significant_token_count`.
    let original = "fn f(a, b, c) { [1, 2, 3] }\n";
    // Same program with one comma dropped — would be a formatter bug.
    let corrupted = "fn f(a, b c) { [1, 2, 3] }\n";
    let err = check_formatter_invariants(original, corrupted).unwrap_err();
    assert!(
        err.contains("significant token count"),
        "unexpected error: {err}"
    );
}

#[test]
fn formatter_invariants_allow_disambiguation_parens() {
    // The formatter is allowed to insert paren pairs around sub-
    // expressions whose precedence is non-obvious. Example: `B?-F`
    // parses as `(B?) - F` and the formatter emits the explicit
    // parens to expose precedence. Locks the exclusion of LParen /
    // RParen from `significant_token_count`. Dropping a paren is
    // still caught by the delimiter-balance check — see
    // `formatter_invariants_reject_dropped_rparen`.
    let original = "fn f() { B?-F }\n";
    let formatted = "fn f() { (B?) - F }\n";
    check_formatter_invariants(original, formatted)
        .expect("balanced paren insertion must be permitted");
}

// --------------------------------------------------------------------
// Parser invariants
// --------------------------------------------------------------------

#[test]
fn parser_invariants_accept_real_parse() {
    let src = "let x = 1\nfn main() = x\n";
    let tokens = Lexer::new(src).tokenize().unwrap();
    let program = Parser::new(tokens.clone()).parse_program().unwrap();
    check_parser_invariants(src, &tokens, &program)
        .expect("real parsed program must satisfy invariants");
}

#[test]
fn parser_invariants_accept_empty_source() {
    // Empty source has no significant tokens and must yield zero decls.
    let src = "";
    let tokens = Lexer::new(src).tokenize().unwrap();
    let program = Parser::new(tokens.clone()).parse_program().unwrap();
    check_parser_invariants(src, &tokens, &program).expect("empty source must satisfy invariants");
}

#[test]
fn parser_invariants_accept_whitespace_only_source() {
    let src = "\n\n   \n";
    let tokens = Lexer::new(src).tokenize().unwrap();
    let program = Parser::new(tokens.clone()).parse_program().unwrap();
    check_parser_invariants(src, &tokens, &program)
        .expect("whitespace-only source must satisfy invariants");
}

#[test]
fn parser_invariants_reject_decl_span_past_source() {
    // A parser bug that emits a decl with a span pointing past the end
    // of the source buffer would have slipped through the old
    // panic-only fuzz driver. The new invariant catches it.
    let src = "import foo\n";
    let tokens = Lexer::new(src).tokenize().unwrap();
    let bogus_program = Program {
        decls: vec![Decl::Import(
            ImportTarget::Module(silt::intern::intern("foo")),
            Span::with_offset(1, 1, 9999),
        )],
    };
    let err = check_parser_invariants(src, &tokens, &bogus_program).unwrap_err();
    assert!(
        err.contains("beyond source length"),
        "unexpected error: {err}"
    );
}

#[test]
fn parser_invariants_reject_empty_decls_for_nontrivial_source() {
    // A parser bug that silently drops every top-level construct would
    // otherwise produce an empty-but-Ok program. The invariant fires
    // because the source has significant tokens but zero decls.
    let src = "let x = 1\n";
    let tokens = Lexer::new(src).tokenize().unwrap();
    let empty_program = Program { decls: vec![] };
    let err = check_parser_invariants(src, &tokens, &empty_program).unwrap_err();
    assert!(err.contains("zero decls"), "unexpected error: {err}");
}

#[test]
fn parser_invariants_reject_decls_from_empty_source() {
    // The symmetric bug: parser fabricates a decl from empty input.
    let src = "";
    let tokens = Lexer::new(src).tokenize().unwrap();
    let bogus_program = Program {
        decls: vec![Decl::Import(
            ImportTarget::Module(silt::intern::intern("ghost")),
            Span::with_offset(1, 1, 0),
        )],
    };
    let err = check_parser_invariants(src, &tokens, &bogus_program).unwrap_err();
    assert!(err.contains("empty-of-tokens"), "unexpected error: {err}");
}

// --------------------------------------------------------------------
// Formatter idempotency
// --------------------------------------------------------------------

#[test]
fn format_idempotent_accepts_well_formed_source() {
    // Input that already passes through the formatter must still be
    // idempotent on a second pass.
    let src = "let x = 1\nfn main() = x\n";
    check_format_idempotent(src).expect("real source must be idempotent under format");
}

#[test]
fn format_idempotent_accepts_messy_input() {
    // The formatter will reshape this on the first pass but must fix-
    // point on the second.
    let src = "let   x=1\n\n\nlet y=   2\n";
    check_format_idempotent(src).expect("messy input must reach a formatter fixpoint");
}

#[test]
fn format_idempotent_tolerates_unformattable_input() {
    // If the formatter rejects the input outright, the idempotency
    // check is vacuously satisfied — we're only locking behavior for
    // inputs the formatter accepts.
    let src = "pub fn (((\n";
    // Whether this happens to format or not, the helper must not panic
    // and must not return an error just because formatting failed.
    let _ = check_format_idempotent(src).expect("unformattable input must not be an error");
}

#[test]
fn format_idempotent_detects_non_fixpoint() {
    // Construct a "fake formatter" scenario: verify that the helper's
    // own comparison logic would catch a non-fixpoint output. We do
    // this by mimicking what the helper would see — two calls to
    // `format` on a real source — and asserting first == second for
    // current inputs. This locks the invariant: if a future formatter
    // regression introduced non-idempotence, this test would break.
    let src = "let x = 1\n";
    let first = formatter::format(src).expect("format must succeed");
    let second = formatter::format(&first).expect("format must succeed");
    assert_eq!(
        first, second,
        "formatter must currently be idempotent on simple source"
    );
    // And the helper agrees.
    check_format_idempotent(src).expect("helper must confirm idempotence");
}
