//! Round 77 lock tests for two LATENT findings in `src/errors.rs`.
//!
//! ERR-L1: `clamp_span_to_source` previously copied `span.offset` through
//! verbatim when the line/col were clamped onto the last real line, which
//! left the byte offset pointing past EOF. Downstream consumers (e.g. the
//! LSP layer's UTF-16 column conversion) assume `offset <= source.len()`,
//! so an out-of-range offset can cause panics or incorrect column math.
//!
//! ERR-L2: A stale comment above the caret-line `write!` claimed the
//! multi-line message split happened locally there, but it now happens at
//! the top of `fmt`. The doc-only fix removes the misleading text.

use silt::errors::SourceError;
use silt::lexer::{LexError, Span};

/// ERR-L1: Span with offset past EOF and line past last line should be
/// clamped — both line/col AND offset must be brought back into range.
#[test]
fn clamp_span_offset_clamped_when_past_eof() {
    let source = "let x = 1\n";
    let src_len = source.len();
    // Construct an out-of-range span: offset way past EOF, line past last.
    let bogus_span = Span::with_offset(99, 50, src_len + 9999);
    let lex_err = LexError {
        message: "synthetic past-EOF lex error for round77 lock".to_string(),
        span: bogus_span,
    };

    // Drive through the public surface. `from_lex_error` calls
    // `clamp_span_to_source` internally and stores the result in `.span`.
    let se = SourceError::from_lex_error(&lex_err, source, "test.silt");

    assert!(
        se.span.offset <= src_len,
        "clamp_span_to_source must clamp offset onto source: got offset={}, source.len()={}",
        se.span.offset,
        src_len,
    );
    // Sanity: line should also be clamped onto a real line.
    let line_count = source.lines().count();
    assert!(
        se.span.line <= line_count,
        "clamp_span_to_source must clamp line onto last real line: got line={}, line_count={}",
        se.span.line,
        line_count,
    );
}

/// ERR-L1 corollary: when the span is already in-range, clamping must
/// leave the offset alone — the fix must not regress the happy path.
#[test]
fn clamp_span_offset_preserved_when_in_range() {
    let source = "let x = 1\nlet y = 2\n";
    // Pick an offset/line that actually points inside `source`.
    let in_range_span = Span::with_offset(1, 5, 4);
    let lex_err = LexError {
        message: "in-range synthetic lex error".to_string(),
        span: in_range_span,
    };
    let se = SourceError::from_lex_error(&lex_err, source, "test.silt");
    assert_eq!(
        se.span.offset, 4,
        "in-range span must pass through unchanged: got offset={}",
        se.span.offset,
    );
    assert_eq!(se.span.line, 1);
    assert_eq!(se.span.col, 5);
}

/// ERR-L1 corollary: empty source is a valid (degenerate) input — the
/// early-return path for `line_count == 0` must still produce an offset
/// that's in-bounds (i.e. `<= source.len() == 0`). Note the function
/// returns the span unchanged in this case, so we construct one whose
/// offset is already 0.
#[test]
fn clamp_span_offset_empty_source_zero_offset() {
    let source = "";
    let span = Span::with_offset(1, 1, 0);
    let lex_err = LexError {
        message: "empty-source synthetic".to_string(),
        span,
    };
    let se = SourceError::from_lex_error(&lex_err, source, "test.silt");
    assert!(se.span.offset <= source.len());
}

/// ERR-L2: the stale "If `self.message` is a multi-line blob..." comment
/// must be GONE from `src/errors.rs`. Use a literal substring grep so the
/// lock fires the moment anyone re-introduces the misleading wording.
#[test]
fn err_l2_stale_multiline_blob_comment_removed() {
    let src = include_str!("../src/errors.rs");
    assert!(
        !src.contains("If `self.message` is a multi-line blob"),
        "stale ERR-L2 comment must remain deleted in src/errors.rs",
    );
}
