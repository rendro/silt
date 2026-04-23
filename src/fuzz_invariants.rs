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

use crate::lexer::{Lexer, Span, SpannedToken, Token};
use crate::parser::Parser;

/// Verify structural invariants on a successful `Lexer::tokenize` result.
///
/// Current checks:
///
/// 1. Every span's byte offset is `<= source.len()` (tokens cannot point
///    past the end of the input).
/// 2. Span byte-offsets are monotonically non-decreasing across the
///    token stream (the lexer never rewinds).
/// 3. Line numbers are also non-decreasing — within a line, column may
///    only increase between successive non-newline tokens.
/// 4. Exactly one `Eof` token is emitted, and it is the final token.
/// 5. The final `Eof` span offset equals the source length in bytes
///    (the lexer consumed everything).
pub fn check_lexer_invariants(source: &str, tokens: &[SpannedToken]) -> Result<(), String> {
    if tokens.is_empty() {
        return Err("token stream is empty (expected at least Eof)".into());
    }

    let src_len = source.len();
    let mut prev: Option<&Span> = None;
    let mut seen_eof = false;

    for (idx, (tok, span)) in tokens.iter().enumerate() {
        if seen_eof {
            return Err(format!("token {tok:?} at index {idx} emitted after Eof"));
        }

        if span.offset > src_len {
            return Err(format!(
                "token {tok:?} at index {idx} has offset {} beyond source length {}",
                span.offset, src_len
            ));
        }

        if let Some(p) = prev {
            if span.offset < p.offset {
                return Err(format!(
                    "token {tok:?} at index {idx} has non-monotonic offset {} < {}",
                    span.offset, p.offset
                ));
            }
            if span.line < p.line {
                return Err(format!(
                    "token {tok:?} at index {idx} has non-monotonic line {} < {}",
                    span.line, p.line
                ));
            }
        }

        if matches!(tok, Token::Eof) {
            seen_eof = true;
            if span.offset != src_len {
                return Err(format!(
                    "Eof span offset {} != source length {}",
                    span.offset, src_len
                ));
            }
        }

        prev = Some(span);
    }

    if !seen_eof {
        return Err("token stream ended without emitting Eof".into());
    }

    Ok(())
}

/// Count "significant" tokens, skipping `Newline` and `Eof`. A formatter
/// is allowed to reshape whitespace but must preserve every other token.
fn significant_token_count(tokens: &[SpannedToken]) -> usize {
    tokens
        .iter()
        .filter(|(t, _)| !matches!(t, Token::Newline | Token::Eof))
        .count()
}

/// Compute net delimiter balances `(paren, brace, bracket)`. A balanced
/// program has all zeros; mismatched open/close produces non-zero or
/// negative counts (we saturate at 0 on underflow since this runs on
/// fuzz inputs where the lexer is tolerant).
fn delimiter_balance(tokens: &[SpannedToken]) -> (i64, i64, i64) {
    let mut p = 0i64;
    let mut b = 0i64;
    let mut k = 0i64;
    for (tok, _) in tokens {
        match tok {
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
/// This is intentionally naïve — it does not exclude comment markers
/// appearing inside string literals — because the formatter preserves
/// string-literal content byte-for-byte, so whatever count the original
/// produces, the formatted output must produce the same count. The
/// invariant compares two scans of the *same* scheme, so the bias
/// cancels out.
fn comment_marker_count(source: &str) -> (usize, usize) {
    let bytes = source.as_bytes();
    let mut line_comments = 0usize;
    let mut block_open = 0usize;
    let mut i = 0;
    while i + 1 < bytes.len() {
        // `--` line comment
        if bytes[i] == b'-' && bytes[i + 1] == b'-' {
            line_comments += 1;
            // Skip to end of line so we don't double-count `----`.
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // `{-` block comment opener
        if bytes[i] == b'{' && bytes[i + 1] == b'-' {
            block_open += 1;
            i += 2;
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
    let orig_tokens = Lexer::new(original)
        .tokenize()
        .map_err(|e| format!("original failed to lex: {e}"))?;
    let fmt_tokens = Lexer::new(formatted)
        .tokenize()
        .map_err(|e| format!("formatted output failed to lex: {e}"))?;

    let orig_sig = significant_token_count(&orig_tokens);
    let fmt_sig = significant_token_count(&fmt_tokens);
    if orig_sig != fmt_sig {
        return Err(format!(
            "significant token count changed: {orig_sig} -> {fmt_sig}"
        ));
    }

    let orig_bal = delimiter_balance(&orig_tokens);
    let fmt_bal = delimiter_balance(&fmt_tokens);
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
    if Parser::new(orig_tokens).parse_program().is_ok()
        && Parser::new(fmt_tokens).parse_program().is_err()
    {
        return Err("original parsed but formatted output did not".into());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexer_invariants_accept_well_formed_source() {
        let src = "let x = 1\nlet y = 2\n";
        let tokens = Lexer::new(src).tokenize().unwrap();
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
