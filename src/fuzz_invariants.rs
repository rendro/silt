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
//! `crate::parser`, and `crate::formatter`, and is deliberately kept
//! side-effect-free so it stays safe to call from `no_main` fuzz drivers.

use crate::ast::{Decl, Program};
use crate::lexer::{Lexed, Lexer, Tok, Token};
use crate::parser::Parser;
use crate::source::Span;

/// Verify structural invariants on a successful `Lexer::tokenize` result.
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
///    comment out, a newline token has none, and each comment lies
///    between the token before it and the token that carries it.
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
        if tok.kind == Token::Newline {
            if !tok.comments.is_empty() || tok.newlines_before != 0 {
                return Err(format!("newline token at index {idx} carries trivia"));
            }
            continue;
        }
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

    Ok(())
}

/// Count "significant" tokens, skipping `Newline`, `Eof`, the
/// round-paren delimiters `(` and `)`, and any `,` that immediately
/// precedes a closer `)`, `]`, or `}` (a trailing comma).
///
/// silt's parser requires explicit commas between elements in every
/// list-style construct, but the formatter legitimately drops a
/// trailing comma when it reflows a multi-line collection to a single
/// line and re-adds one when it reflows a single-line collection to
/// multi-line. Counting trailing commas as "significant" makes that
/// pure-layout normalization a fuzzer false positive, so they are
/// excluded here. Inter-element commas remain counted — dropping one
/// of those changes program structure and must still trip the check.
///
/// Likewise the formatter inserts disambiguation parens around sub-
/// expressions whose precedence is non-obvious (e.g. `B?-F` → `(B?) - F`).
/// Those paren pairs are balanced and harmless — verified separately
/// by `delimiter_balance`, which asserts net open and close counts
/// match exactly — so excluding them from the "every token must
/// survive" count avoids false positives without losing coverage of
/// dropped parens.
///
/// Braces and brackets remain counted (the formatter never inserts
/// `{`, `}`, `[`, or `]` tokens the source didn't already have), and
/// non-trailing commas remain counted.
fn significant_token_count(tokens: &[Tok]) -> usize {
    tokens
        .iter()
        .enumerate()
        .filter(|(i, Tok { kind: t, .. })| {
            if matches!(
                t,
                Token::Newline | Token::Eof | Token::LParen | Token::RParen
            ) {
                return false;
            }
            if matches!(t, Token::Comma) {
                // Skip whitespace-equivalent tokens (`Newline`) when
                // looking for the next non-trivial token.
                let mut j = i + 1;
                while j < tokens.len() && matches!(tokens[j].kind, Token::Newline) {
                    j += 1;
                }
                if j < tokens.len()
                    && matches!(
                        tokens[j].kind,
                        Token::RParen | Token::RBracket | Token::RBrace
                    )
                {
                    return false;
                }
            }
            true
        })
        .count()
}

/// Compute net delimiter balances `(paren, brace, bracket)`. A balanced
/// program has all zeros; mismatched open/close produces non-zero or
/// negative counts (we saturate at 0 on underflow since this runs on
/// fuzz inputs where the lexer is tolerant).
fn delimiter_balance(tokens: &[Tok]) -> (i64, i64, i64) {
    let mut p = 0i64;
    let mut b = 0i64;
    let mut k = 0i64;
    for tok in tokens {
        match tok.kind {
            Token::LParen => p += 1,
            Token::RParen => p -= 1,
            Token::LBrace | Token::HashBrace => b += 1,
            Token::RBrace => b -= 1,
            Token::LBracket | Token::HashBracket => k += 1,
            Token::RBracket => k -= 1,
            _ => {}
        }
    }
    (p, b, k)
}

/// Rough comment count using textual scanning. Line comments start with
/// `--` (to end of line); block comments start with `{-` and nest.
///
/// String-aware: the scanner skips over the content of `"..."` and
/// `"""..."""` string literals so `--` markers appearing inside string
/// content do not count as comment markers. This is required because
/// the formatter legitimately reflows multi-line `"..."` content to
/// single-line `\n`-escaped form (and vice versa), which collapses the
/// number of physical lines `--` markers appear on. Without string
/// awareness, two scans of input vs output disagree on a "comment"
/// count that has nothing to do with real comments. The same scheme
/// is applied to both inputs so the comparison stays apples-to-apples.
///
/// Block-comment-aware: a `--` inside an open `{- ... -}` block is also
/// not a line comment, so block depth is tracked while scanning.
fn comment_marker_count(source: &str) -> (usize, usize) {
    let bytes = source.as_bytes();
    let mut line_comments = 0usize;
    let mut block_open = 0usize;
    let mut i = 0;
    let mut block_depth: usize = 0;
    while i < bytes.len() {
        // Inside a `{- ... -}` block: only `{-` deepens, `-}` closes;
        // `--` and string quotes are inert.
        if block_depth > 0 {
            if i + 1 < bytes.len() && bytes[i] == b'{' && bytes[i + 1] == b'-' {
                block_open += 1;
                block_depth += 1;
                i += 2;
                continue;
            }
            if i + 1 < bytes.len() && bytes[i] == b'-' && bytes[i + 1] == b'}' {
                block_depth -= 1;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }

        // Triple-quoted string `"""..."""` — content is opaque.
        if i + 2 < bytes.len() && &bytes[i..i + 3] == b"\"\"\"" {
            i += 3;
            while i + 2 < bytes.len() && &bytes[i..i + 3] != b"\"\"\"" {
                i += 1;
            }
            if i + 2 < bytes.len() {
                i += 3;
            } else {
                // Unterminated triple — consume rest.
                i = bytes.len();
            }
            continue;
        }

        // Regular string `"..."` — content is opaque (including embedded
        // raw newlines, which the silt lexer tolerates and the formatter
        // re-emits as `\n` escapes). Honour `\"` escapes.
        if bytes[i] == b'"' {
            i += 1;
            while i < bytes.len() && bytes[i] != b'"' {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                    continue;
                }
                i += 1;
            }
            if i < bytes.len() {
                i += 1;
            }
            continue;
        }

        // `{-` block comment opener (outside any string).
        if i + 1 < bytes.len() && bytes[i] == b'{' && bytes[i + 1] == b'-' {
            block_open += 1;
            block_depth += 1;
            i += 2;
            continue;
        }

        // `--` line comment (outside any string / block comment).
        if i + 1 < bytes.len() && bytes[i] == b'-' && bytes[i + 1] == b'-' {
            line_comments += 1;
            // Skip to end of line so we don't double-count `----`.
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }

        i += 1;
    }
    (line_comments, block_open)
}

/// Check invariants that a correct formatter must uphold between its
/// input and output.
///
/// Current checks:
///
/// 1. The formatted output must lex successfully (if it doesn't, the
///    formatter produced garbage).
/// 2. The count of non-whitespace tokens must match the original.
///    Whitespace (Newline) is allowed to differ because the formatter
///    legitimately reshapes blank lines and indentation.
/// 3. Delimiter balances `(p, b, k)` must match exactly.
/// 4. Comment marker counts (`--` line comments, `{-` block openers)
///    must match exactly.
/// 5. If the original program parses, the formatted output must parse
///    too. A formatter is allowed to reject un-parseable input, but it
///    must never turn a valid program into an invalid one.
pub fn check_formatter_invariants(original: &str, formatted: &str) -> Result<(), String> {
    let orig_tokens = Lexer::new(crate::source::FileId::default(), original)
        .tokenize()
        .map_err(|e| format!("original failed to lex: {}", e.message))?;
    let fmt_tokens = Lexer::new(crate::source::FileId::default(), formatted)
        .tokenize()
        .map_err(|e| format!("formatted output failed to lex: {}", e.message))?;

    let orig_sig = significant_token_count(&orig_tokens.tokens);
    let fmt_sig = significant_token_count(&fmt_tokens.tokens);
    if orig_sig != fmt_sig {
        return Err(format!(
            "significant token count changed: {orig_sig} -> {fmt_sig}"
        ));
    }

    let orig_bal = delimiter_balance(&orig_tokens.tokens);
    let fmt_bal = delimiter_balance(&fmt_tokens.tokens);
    if orig_bal != fmt_bal {
        return Err(format!(
            "delimiter balance changed: {orig_bal:?} -> {fmt_bal:?}"
        ));
    }

    let orig_comments = comment_marker_count(original);
    let fmt_comments = comment_marker_count(formatted);
    if orig_comments != fmt_comments {
        return Err(format!(
            "comment marker count changed: {orig_comments:?} -> {fmt_comments:?}"
        ));
    }

    // Parse-preservation: if the original parses, the formatted output must.
    if Parser::new(orig_tokens, original).parse_program().is_ok()
        && Parser::new(fmt_tokens, formatted).parse_program().is_err()
    {
        return Err("original parsed but formatted output did not".into());
    }

    Ok(())
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
///    `Newline`, `Eof`, `LParen`, `RParen`), then `program.decls` must
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

/// Verify that the formatter is idempotent: `format(format(src))` must
/// equal `format(src)` byte-for-byte. This is a well-known property of
/// a well-behaved formatter and is complementary to the structural
/// checks in [`check_formatter_invariants`]: that function compares
/// original-vs-formatted shape, this one locks down the fixed-point
/// property.
///
/// Returns Ok if `src` fails to format (the formatter may legitimately
/// reject invalid programs); callers who require parseability should
/// gate on that separately before calling this.
pub fn check_format_idempotent(source: &str) -> Result<(), String> {
    let first = match crate::formatter::format(source) {
        Ok(s) => s,
        Err(_) => return Ok(()),
    };
    let second = match crate::formatter::format(&first) {
        Ok(s) => s,
        Err(e) => return Err(format!("second format pass failed: {e:?}")),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexer_invariants_accept_well_formed_source() {
        let src = "let x = 1\nlet y = 2\n";
        let tokens = Lexer::new(crate::source::FileId::default(), src)
            .tokenize()
            .unwrap();
        check_lexer_invariants(src, &tokens).unwrap();
    }

    #[test]
    fn formatter_invariants_accept_identity() {
        let src = "let x = 1\n";
        check_formatter_invariants(src, src).unwrap();
    }

    #[test]
    fn formatter_invariants_detect_dropped_token() {
        let original = "let x = (1 + 2)\n";
        // Corrupted "formatter output" — the trailing paren was deleted.
        let corrupted = "let x = (1 + 2\n";
        assert!(check_formatter_invariants(original, corrupted).is_err());
    }

    #[test]
    fn formatter_invariants_detect_dropped_comment() {
        let original = "-- a comment\nlet x = 1\n";
        let corrupted = "let x = 1\n";
        assert!(check_formatter_invariants(original, corrupted).is_err());
    }
}
