//! Round 77 lock tests for a LATENT finding in `src/errors.rs`.
//!
//! ERR-L1: a diagnostic whose position lies past the end of the source
//! (an unexpected-EOF parse error, or a bogus offset) must still be shown
//! on a real line of the file, with that line's text, and must never
//! index past the source.

use silt::errors::SourceError;
use silt::lexer::LexError;
use silt::source::{FileId, SourceMap, SourceName, Span};

fn sources(text: &str) -> SourceMap {
    let mut map = SourceMap::new();
    map.add(SourceName::Path("test.silt".into()), text.into());
    map
}

fn lex_error_at(start: u32) -> LexError {
    LexError {
        message: "synthetic lex error for round77 lock".to_string(),
        span: Span::point(FileId::default(), start),
    }
}

/// A position way past EOF is shown on the last real line, just after
/// its last character.
#[test]
fn a_position_past_eof_is_shown_on_the_last_line() {
    let source = "let x = 1\n";
    let se = SourceError::from_lex_error(&lex_error_at(9999), &sources(source), "test.silt");
    assert_eq!((se.line, se.col), (1, 10));
    assert_eq!(se.source_line.as_deref(), Some("let x = 1"));
}

/// The happy path: an in-range position is shown where it is.
#[test]
fn an_in_range_position_is_shown_where_it_is() {
    let source = "let x = 1\nlet y = 2\n";
    let se = SourceError::from_lex_error(&lex_error_at(4), &sources(source), "test.silt");
    assert_eq!((se.line, se.col), (1, 5));
    assert_eq!(se.source_line.as_deref(), Some("let x = 1"));
}

/// Empty source is a valid (degenerate) input: line 1, column 1, and no
/// source line to show.
#[test]
fn an_empty_source_has_no_line_to_show() {
    let se = SourceError::from_lex_error(&lex_error_at(0), &sources(""), "test.silt");
    assert_eq!((se.line, se.col), (1, 1));
    assert_eq!(se.source_line, None);
}
