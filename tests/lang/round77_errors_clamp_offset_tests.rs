//! Round 77 lock tests: a diagnostic whose position lies past the end of
//! the source (an unexpected-EOF parse error, or a bogus offset) must
//! still be shown on a real line of the file, with that line's text, and
//! must never index past the source.

use silt::source::{FileId, SourceMap, SourceName, Span};

fn sources(text: &str) -> SourceMap {
    let mut map = SourceMap::new();
    map.add(SourceName::Path("test.silt".into()), text.into());
    map
}

fn at(start: u32) -> Span {
    Span::point(FileId::default(), start)
}

/// A position way past EOF is shown on the last real line, just after
/// its last character.
#[test]
fn a_position_past_eof_is_shown_on_the_last_line() {
    let p = sources("let x = 1\n").position(at(9999)).unwrap();
    assert_eq!((p.line, p.col), (1, 10));
    assert_eq!(p.line_text, "let x = 1");
}

/// The happy path: an in-range position is shown where it is.
#[test]
fn an_in_range_position_is_shown_where_it_is() {
    let p = sources("let x = 1\nlet y = 2\n").position(at(4)).unwrap();
    assert_eq!((p.line, p.col), (1, 5));
    assert_eq!(p.line_text, "let x = 1");
}

/// Empty source is a valid (degenerate) input: line 1, column 1, and an
/// empty line to show.
#[test]
fn an_empty_source_has_no_text_to_show() {
    let p = sources("").position(at(0)).unwrap();
    assert_eq!((p.line, p.col), (1, 1));
    assert_eq!(p.line_text, "");
}
