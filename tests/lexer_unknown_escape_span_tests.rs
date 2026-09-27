//! Regression lock: the unknown-escape-sequence lexer error must anchor
//! its span AT the backslash, not after the escape sequence.
//!
//! Before the fix, `scan_string`'s `Some('\\')` arm consumed both the
//! backslash and the escape character before building the `LexError`,
//! then used `span: self.span()` — the lexer's *current* position, i.e.
//! the character after the sequence. The rendered caret landed two
//! columns past the offender (on the following character, or on the
//! closing quote when the escape was last in the string).
//!
//! The fix captures `esc_span = self.span()` before the first
//! `advance_char()`, so the caret lands on `\` — matching how rustc
//! anchors bad escapes, and matching every sibling lexer error in the
//! file (unterminated string, unterminated escape, block comment),
//! which all anchor at a captured start span.

use silt::lexer::{Lexer, Span};

/// Lex `input`, expect an unknown-escape error, return its span.
fn unknown_escape_span(input: &str) -> Span {
    match Lexer::new(input).tokenize() {
        Err(e) => {
            assert!(
                e.message.contains("unknown escape sequence"),
                "expected unknown-escape error, got: {}",
                e.message
            );
            e.span
        }
        Ok(_) => panic!("expected lexer error for {input:?}, got success"),
    }
}

/// Audit repro: escape mid-string, mid-file. The backslash of `\q` sits
/// at line 2, col 14 (offset 25). Pre-fix the span pointed at the `c`
/// after the escape (col 16, offset 27).
#[test]
fn unknown_escape_span_anchors_on_backslash_mid_string() {
    let source = "fn main() {\n  let s = \"ab\\qcd\"\n  println(s)\n}\n";
    let span = unknown_escape_span(source);
    assert_eq!(
        (span.line, span.col, span.offset),
        (2, 14, 25),
        "unknown-escape span must anchor on the backslash of \\q \
         (line 2, col 14, offset 25), got {}:{} offset {}",
        span.line,
        span.col,
        span.offset
    );
}

/// Escape as the last content of the string: pre-fix the caret landed
/// on the closing quote (col 5); it must land on the backslash (col 3).
#[test]
fn unknown_escape_span_anchors_on_backslash_at_end_of_string() {
    let source = "\"a\\q\"";
    let span = unknown_escape_span(source);
    assert_eq!(
        (span.line, span.col, span.offset),
        (1, 3, 2),
        "unknown-escape span must anchor on the backslash, not the \
         closing quote; got {}:{} offset {}",
        span.line,
        span.col,
        span.offset
    );
}
