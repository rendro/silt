//! Regression tests for the fuzz-target invariant helpers in
//! `silt::fuzz_invariants`. These exercise the structural checks that
//! `fuzz/fuzz_targets/fuzz_lexer.rs`, `fuzz/fuzz_targets/fuzz_formatter.rs`,
//! `fuzz/fuzz_targets/fuzz_parser.rs`, and
//! `fuzz/fuzz_targets/fuzz_roundtrip.rs` now enforce on every fuzz input.
//!
//! Each synthetic corrupted output below is one that a check of "does
//! not panic" alone would accept, and that the invariants reject.

use silt::ast::{Decl, ImportTarget, Program};
use silt::fuzz_invariants::{
    check_formatter_invariants, check_formatter_invariants_of, check_lexer_invariants,
    check_parser_invariants,
};
use silt::lexer::{Lexed, Lexer, Tok, Token};
use silt::parser::Parser;
use silt::source::Span;

// --------------------------------------------------------------------
// Lexer invariants
// --------------------------------------------------------------------

#[test]
fn lexer_invariants_accept_real_tokenization() {
    let src = "let x = 1 + 2\nfn main() { x }\n";
    let tokens = Lexer::new(silt::source::FileId::default(), src)
        .tokenize()
        .unwrap();
    check_lexer_invariants(src, &tokens).expect("real source must satisfy invariants");
}

/// A hand-made token stream with no trivia, as the lexer would return it.
fn lexed(tokens: Vec<(Token, Span)>) -> Lexed {
    Lexed {
        tokens: tokens
            .into_iter()
            .map(|(kind, span)| Tok {
                kind,
                span,
                newlines_before: 0,
                comments: 0..0,
            })
            .collect(),
        comments: Vec::new(),
    }
}

#[test]
fn lexer_invariants_reject_missing_eof() {
    // Synthesize a token stream without the terminating Eof. The old
    // fuzz target never looked at the tokens at all; the new one
    // demands Eof as the last element.
    let src = "x";
    let tokens = vec![(
        Token::Ident(silt::intern::intern("x")),
        Span::point(silt::source::FileId::default(), 0),
    )];
    let err = check_lexer_invariants(src, &lexed(tokens)).unwrap_err();
    assert!(err.contains("Eof"), "unexpected error: {err}");
}

#[test]
fn lexer_invariants_reject_offset_past_source() {
    let src = "x";
    let tokens = vec![
        (
            Token::Ident(silt::intern::intern("x")),
            Span::point(silt::source::FileId::default(), 0),
        ),
        // Eof claiming an offset past the end of source — would be a
        // silent bug in a real lexer; the old fuzz driver never noticed.
        (Token::Eof, Span::point(silt::source::FileId::default(), 99)),
    ];
    let err = check_lexer_invariants(src, &lexed(tokens)).unwrap_err();
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
            Span::point(silt::source::FileId::default(), 1),
        ),
        (
            Token::Ident(silt::intern::intern("b")),
            // Rewound offset — a real lexer bug would look like this if
            // it accidentally reset position state between tokens.
            Span::point(silt::source::FileId::default(), 0),
        ),
        (Token::Eof, Span::point(silt::source::FileId::default(), 2)),
    ];
    let err = check_lexer_invariants(src, &lexed(tokens)).unwrap_err();
    assert!(err.contains("non-monotonic"), "unexpected error: {err}");
}

#[test]
fn lexer_invariants_reject_token_after_eof() {
    let src = "x";
    let tokens = vec![
        (
            Token::Ident(silt::intern::intern("x")),
            Span::point(silt::source::FileId::default(), 0),
        ),
        (Token::Eof, Span::point(silt::source::FileId::default(), 1)),
        // Bogus extra token after Eof.
        (Token::Plus, Span::point(silt::source::FileId::default(), 1)),
    ];
    let err = check_lexer_invariants(src, &lexed(tokens)).unwrap_err();
    assert!(err.contains("after Eof"), "unexpected error: {err}");
}

// --------------------------------------------------------------------
// Formatter invariants
// --------------------------------------------------------------------

/// The formatter, with `tamper` applied to its result before it checks
/// it: what a defect of the printer would hand to the check.
fn tampered(
    tamper: fn(String) -> String,
) -> impl Fn(&str) -> Result<String, silt::diagnostic::Diagnostic> {
    move |text| silt::format::format_with(silt::source::FileId::default(), text, tamper)
}

const SOURCE: &str = "-- about f\nfn f(x) {\n  let y = [1, 2]  {- two -}\n  (x + 0x10) * y\n}\n";

#[test]
fn formatter_invariants_accept_the_formatter() {
    check_formatter_invariants(SOURCE).expect("the formatter's result passes");
    check_formatter_invariants("let   x=1\n\n\nlet y=   2\n").expect("messy input");
}

#[test]
fn formatter_invariants_ignore_what_is_not_a_program() {
    check_formatter_invariants("pub fn (((\n").expect("no input");
    check_formatter_invariants("fn f() { \"open }").expect("no input");
}

#[test]
fn formatter_invariants_reject_a_result_that_does_not_parse() {
    let err = check_formatter_invariants_of(SOURCE, tampered(|out| out.replace("[1, 2]", "[1, 2")))
        .unwrap_err();
    assert!(err.contains("would not parse"), "unexpected error: {err}");
}

#[test]
fn formatter_invariants_reject_a_dropped_comment() {
    for tamper in [
        (|out| out.replace("-- about f\n", "")) as fn(String) -> String,
        |out| out.replace(" {- two -}", ""),
    ] {
        let err = check_formatter_invariants_of(SOURCE, tampered(tamper)).unwrap_err();
        assert!(err.contains("comment"), "unexpected error: {err}");
    }
}

#[test]
fn formatter_invariants_reject_another_program() {
    for tamper in [
        // Parentheses that grouped.
        (|out| out.replace("(x + 0x10) * y", "x + 0x10 * y")) as fn(String) -> String,
        // A dropped element.
        |out| out.replace("[1, 2]", "[1]"),
        // A literal spelled another way.
        |out| out.replace("0x10", "16"),
    ] {
        let err = check_formatter_invariants_of(SOURCE, tampered(tamper)).unwrap_err();
        assert!(err.contains("first pass"), "unexpected error: {err}");
    }
}

#[test]
fn formatter_invariants_reject_a_second_pass_that_differs() {
    let grows = |text: &str| {
        silt::format::format(silt::source::FileId::default(), text)
            .map(|out| format!("-- more\n{out}"))
    };
    let err = check_formatter_invariants_of(SOURCE, grows).unwrap_err();
    assert!(err.contains("not idempotent"), "unexpected error: {err}");
}

// --------------------------------------------------------------------
// Parser invariants
// --------------------------------------------------------------------

#[test]
fn parser_invariants_accept_real_parse() {
    let src = "let x = 1\nfn main() { x }\n";
    let tokens = Lexer::new(silt::source::FileId::default(), src)
        .tokenize()
        .unwrap();
    let program = Parser::new(tokens.clone(), src).parse_program().unwrap();
    check_parser_invariants(src, &tokens, &program)
        .expect("real parsed program must satisfy invariants");
}

#[test]
fn parser_invariants_accept_empty_source() {
    // Empty source has no significant tokens and must yield zero decls.
    let src = "";
    let tokens = Lexer::new(silt::source::FileId::default(), src)
        .tokenize()
        .unwrap();
    let program = Parser::new(tokens.clone(), src).parse_program().unwrap();
    check_parser_invariants(src, &tokens, &program).expect("empty source must satisfy invariants");
}

#[test]
fn parser_invariants_accept_whitespace_only_source() {
    let src = "\n\n   \n";
    let tokens = Lexer::new(silt::source::FileId::default(), src)
        .tokenize()
        .unwrap();
    let program = Parser::new(tokens.clone(), src).parse_program().unwrap();
    check_parser_invariants(src, &tokens, &program)
        .expect("whitespace-only source must satisfy invariants");
}

#[test]
fn parser_invariants_reject_decl_span_past_source() {
    // A parser bug that emits a decl with a span pointing past the end
    // of the source buffer would have slipped through the old
    // panic-only fuzz driver. The new invariant catches it.
    let src = "import foo\n";
    let tokens = Lexer::new(silt::source::FileId::default(), src)
        .tokenize()
        .unwrap();
    let bogus_program = Program {
        decls: vec![Decl::Import(
            ImportTarget::Module(silt::intern::intern("foo")),
            Span::point(silt::source::FileId::default(), 9999),
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
    let tokens = Lexer::new(silt::source::FileId::default(), src)
        .tokenize()
        .unwrap();
    let empty_program = Program { decls: vec![] };
    let err = check_parser_invariants(src, &tokens, &empty_program).unwrap_err();
    assert!(err.contains("zero decls"), "unexpected error: {err}");
}

#[test]
fn parser_invariants_reject_decls_from_empty_source() {
    // The symmetric bug: parser fabricates a decl from empty input.
    let src = "";
    let tokens = Lexer::new(silt::source::FileId::default(), src)
        .tokenize()
        .unwrap();
    let bogus_program = Program {
        decls: vec![Decl::Import(
            ImportTarget::Module(silt::intern::intern("ghost")),
            Span::point(silt::source::FileId::default(), 0),
        )],
    };
    let err = check_parser_invariants(src, &tokens, &bogus_program).unwrap_err();
    assert!(err.contains("empty-of-tokens"), "unexpected error: {err}");
}
