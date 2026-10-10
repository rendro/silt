//! Invariant checks shared between fuzz targets and regression tests.
//!
//! The fuzz targets under `fuzz/fuzz_targets/` historically only asserted
//! "must not panic" or "must be idempotent", which lets a large class of
//! real bugs slip through (dropped tokens, corrupted spans, deleted
//! comments). This module factors the cheap-to-evaluate structural
//! invariants into helper functions so both the fuzzers and the test
//! suite can exercise them on the same inputs.
//!
//! All functions return `Result<(), String>` with a human-readable
//! description on failure; the fuzz targets `unwrap()` that result to
//! trigger a libFuzzer-visible panic, while the regression tests match on
//! the `Err` to verify that synthetic corruption is detected.
//!
//! This module has no external dependencies beyond `crate::lexer`,
//! `crate::parser`, and `crate::format`, and is deliberately kept
//! side-effect-free so it stays safe to call from `no_main` fuzz drivers.

use crate::ast::{Decl, Program};
use crate::diagnostic::{Code, Diagnostic};
use crate::lexer::{Lexed, Tok, Token};
use crate::source::Span;

/// Verify structural invariants on what `Lexer::tokenize` makes of any
/// text, with or without lex errors.
///
/// Current checks:
///
/// 1. Every span ends no earlier than it starts and no later than
///    `source.len()` (tokens cannot point past the end of the input).
/// 2. Span starts are monotonically non-decreasing across the token
///    stream (the lexer never rewinds).
/// 3. A token does not start before the previous token ends: tokens do
///    not overlap.
/// 4. Exactly one `Eof` token is emitted, and it is the final token.
/// 5. The final `Eof` span offset equals the source length in bytes
///    (the lexer consumed everything).
/// 6. The comment ranges of the tokens follow each other and leave no
///    comment out, and each comment lies between the token before it
///    and the token that carries it.
/// 7. A `Token::Error` has an error: the lexer recorded at least one,
///    and every error it recorded lies in the source.
/// 8. At most `MAX_SYNTAX_ERRORS` errors are kept; when more are
///    counted, that many are kept and the tokens end in one
///    `Token::Error` for the rest of the text.
pub fn check_lexer_invariants(source: &str, lexed: &Lexed) -> Result<(), String> {
    let tokens = &lexed.tokens;
    if tokens.is_empty() {
        return Err("token stream is empty (expected at least Eof)".into());
    }

    let src_len = source.len();
    let mut prev: Option<&Span> = None;
    let mut seen_eof = false;

    for (
        idx,
        Tok {
            kind: tok, span, ..
        },
    ) in tokens.iter().enumerate()
    {
        if seen_eof {
            return Err(format!("token {tok:?} at index {idx} emitted after Eof"));
        }

        if span.end < span.start || span.end as usize > src_len {
            return Err(format!(
                "token {tok:?} at index {idx} has span {}..{} beyond source length {}",
                span.start, span.end, src_len
            ));
        }

        if let Some(p) = prev {
            if span.start < p.start {
                return Err(format!(
                    "token {tok:?} at index {idx} has non-monotonic offset {} < {}",
                    span.start, p.start
                ));
            }
            if span.start < p.end {
                return Err(format!(
                    "token {tok:?} at index {idx} starts at {} inside the previous token (ends at {})",
                    span.start, p.end
                ));
            }
        }

        if matches!(tok, Token::Eof) {
            seen_eof = true;
            if span.start as usize != src_len {
                return Err(format!(
                    "Eof span offset {} != source length {}",
                    span.start, src_len
                ));
            }
        }

        prev = Some(span);
    }

    if !seen_eof {
        return Err("token stream ended without emitting Eof".into());
    }

    let mut next_comment = 0;
    let mut floor = 0;
    for (idx, tok) in tokens.iter().enumerate() {
        if tok.comments.start != next_comment || tok.comments.end < tok.comments.start {
            return Err(format!(
                "token {:?} at index {idx} has comments {:?}, expected a range from {next_comment}",
                tok.kind, tok.comments
            ));
        }
        let Some(comments) = lexed
            .comments
            .get(tok.comments.start as usize..tok.comments.end as usize)
        else {
            return Err(format!(
                "token {:?} at index {idx} has comments {:?} of {}",
                tok.kind,
                tok.comments,
                lexed.comments.len()
            ));
        };
        for comment in comments {
            if comment.span.start < floor || comment.span.end < comment.span.start {
                return Err(format!(
                    "comment at {}..{} starts before offset {floor}",
                    comment.span.start, comment.span.end
                ));
            }
            floor = comment.span.end;
        }
        if tok.span.start < floor {
            return Err(format!(
                "token {:?} at index {idx} starts at {} inside a comment (ends at {floor})",
                tok.kind, tok.span.start
            ));
        }
        floor = tok.span.end;
        next_comment = tok.comments.end;
    }
    if next_comment as usize != lexed.comments.len() {
        return Err(format!(
            "{} comments, of which the tokens carry {next_comment}",
            lexed.comments.len()
        ));
    }

    if lexed.errors.is_empty() && tokens.iter().any(|tok| tok.kind == Token::Error) {
        return Err("an Error token without a lex error".into());
    }
    if lexed.errors.len() > crate::lexer::MAX_SYNTAX_ERRORS {
        return Err(format!("{} lex errors kept", lexed.errors.len()));
    }
    if lexed.more_errors > 0 {
        let rest = tokens.len().checked_sub(2).map(|i| &tokens[i]);
        if lexed.errors.len() != crate::lexer::MAX_SYNTAX_ERRORS
            || !rest.is_some_and(|tok| tok.kind == Token::Error && tok.span.end as usize == src_len)
        {
            return Err(format!(
                "{} errors counted behind {} kept, and the tokens do not end in one Error \
                 token for the rest",
                lexed.more_errors,
                lexed.errors.len()
            ));
        }
    }
    for error in &lexed.errors {
        if error.span.end < error.span.start || error.span.end as usize > src_len {
            return Err(format!(
                "lex error at {}..{} beyond source length {src_len}",
                error.span.start, error.span.end
            ));
        }
    }

    Ok(())
}

/// The tokens that a declaration is made of: all but `Eof` and the
/// parentheses.
fn significant_token_count(tokens: &[Tok]) -> usize {
    tokens
        .iter()
        .filter(|tok| !matches!(tok.kind, Token::Eof | Token::LParen | Token::RParen))
        .count()
}

/// Extract the declaration's top-level span. Used by
/// [`check_parser_invariants`] to validate that every `Decl` points into
/// the source buffer the parser was given (not past the end).
fn decl_span(decl: &Decl) -> Span {
    match decl {
        Decl::Fn(f) => f.span,
        Decl::Type(t) => t.span,
        Decl::Trait(t) => t.span,
        Decl::TraitImpl(i) => i.span,
        Decl::Import(_, span) => *span,
        Decl::Let { span, .. } => *span,
    }
}

/// Verify structural invariants on a successful `Parser::parse_program`
/// result. The caller must have already lexed `source` into `tokens`
/// and produced `program` by calling `Parser::new(tokens, source).parse_program()`.
///
/// Current checks:
///
/// 1. Every top-level `Decl`'s span ends at or before `source.len()`. A
///    formatter or parser that silently corrupts spans would otherwise
///    slip past the other invariants — the fuzzer can't see AST fields
///    directly, but it can see a panic on this assertion.
/// 2. If the source contains any "significant" token (excluding
///    `Eof`, `LParen`, `RParen`), then `program.decls` must
///    be non-empty. A parser bug that silently drops every top-level
///    construct would otherwise produce an empty-but-Ok program.
///    Conversely, empty/whitespace-only source must yield zero decls.
/// 3. The number of decls is bounded by the number of tokens — trivially
///    true for a correct parser, but catches pathological duplication
///    bugs (accidental push-in-a-loop).
pub fn check_parser_invariants(
    source: &str,
    lexed: &Lexed,
    program: &Program,
) -> Result<(), String> {
    let src_len = source.len();
    for (idx, decl) in program.decls.iter().enumerate() {
        let span = decl_span(decl);
        if span.end as usize > src_len {
            return Err(format!(
                "decl at index {idx} has span end {} beyond source length {}",
                span.end, src_len
            ));
        }
    }

    let tokens = &lexed.tokens;
    let sig = significant_token_count(tokens);
    if sig == 0 && !program.decls.is_empty() {
        return Err(format!(
            "empty-of-tokens source produced {} decls",
            program.decls.len()
        ));
    }
    if sig > 0 && program.decls.is_empty() {
        return Err(format!(
            "source has {sig} significant tokens but program has zero decls"
        ));
    }

    if program.decls.len() > tokens.len() {
        return Err(format!(
            "decl count {} exceeds token count {}",
            program.decls.len(),
            tokens.len()
        ));
    }

    Ok(())
}

/// What a correct formatter upholds on `source`, with `format` as the
/// formatter (`crate::format::format`, or a wrong one in a test):
///
/// 1. it does not refuse a text that lexes and parses: a refusal says
///    that the printer could not follow the tokens, or that the result
///    failed the formatter's own check (it would not parse, would be
///    another program, would spell a literal another way or would not
///    hold the same comments in the same order);
/// 2. the result is a fixed point: formatting it gives it again.
///
/// A text that is not a program is no input: `Ok`.
pub fn check_formatter_invariants_of(
    source: &str,
    format: impl Fn(&str) -> Result<String, Diagnostic>,
) -> Result<(), String> {
    let refused = |pass: &str, e: Diagnostic| format!("{pass} pass: {}", e.message);
    let first = match format(source) {
        Ok(first) => first,
        Err(e) if e.code == Code::FormatRefused => return Err(refused("first", e)),
        Err(_) => return Ok(()),
    };
    let second = match format(&first) {
        Ok(second) => second,
        Err(e) if e.code == Code::FormatRefused => return Err(refused("second", e)),
        Err(e) => return Err(format!("the result does not parse: {}", e.message)),
    };
    if first != second {
        return Err(format!(
            "formatter not idempotent: first pass {} bytes, second pass {} bytes",
            first.len(),
            second.len()
        ));
    }
    Ok(())
}

/// [`check_formatter_invariants_of`] for the formatter.
pub fn check_formatter_invariants(source: &str) -> Result<(), String> {
    check_formatter_invariants_of(source, |text| {
        crate::format::format(crate::source::FileId::default(), text)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;

    #[test]
    fn lexer_invariants_accept_well_formed_source() {
        let src = "let x = 1\nlet y = 2\n";
        let tokens = Lexer::new(crate::source::FileId::default(), src)
            .tokenize()
            .checked()
            .unwrap();
        check_lexer_invariants(src, &tokens).unwrap();
    }

    #[test]
    fn formatter_invariants_accept_the_formatter() {
        check_formatter_invariants("let   x=1\n\n\n-- two\nlet y=   2\n").unwrap();
        // Not a program: no input.
        check_formatter_invariants("pub fn (((\n").unwrap();
    }

    #[test]
    fn formatter_invariants_detect_a_refusal_and_a_second_pass_that_differs() {
        use crate::format::format_with;
        let file = crate::source::FileId::default();
        let source = "-- a comment\nlet x = (1 + 2)\n";
        let dropped_comment =
            |text: &str| format_with(file, text, |out| out.replace("-- a comment\n", ""));
        let err = check_formatter_invariants_of(source, dropped_comment).unwrap_err();
        assert!(err.contains("first pass"), "{err}");
        let grows =
            |text: &str| crate::format::format(file, text).map(|out| format!("-- more\n{out}"));
        let err = check_formatter_invariants_of(source, grows).unwrap_err();
        assert!(err.contains("not idempotent"), "{err}");
    }
}
