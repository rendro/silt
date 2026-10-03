//! Span → LSP range conversion.
//!
//! LSP positions count characters in **UTF-16 code units** (per the spec,
//! and what nearly every client uses as the default encoding). A span is
//! a byte range; the document's [`SourceFile`] turns a byte offset into a
//! position (`SourceFile::lsp_position`) and back
//! (`SourceFile::offset_of_lsp_position`).

use lsp_types::{Position, Range};

use crate::source::{SourceFile, Span};

/// The LSP range of `span`. An empty span (a lexer error points at the
/// character it rejects, a parse error at the end of the file) covers the
/// character it is at, so an editor has something to underline; at the
/// end of the file, one column.
pub(super) fn span_to_range(span: &Span, file: &SourceFile) -> Range {
    let start = file.lsp_position(span.start);
    let end = if span.end > span.start {
        file.lsp_position(span.end)
    } else {
        let width = file
            .text
            .get(span.start as usize..)
            .and_then(|rest| rest.chars().next())
            .map_or(1, char::len_utf16);
        Position::new(start.line, start.character + width as u32)
    };
    Range::new(start, end)
}

/// The LSP range of the bytes `start..end` of `file`.
pub(super) fn offsets_to_range(file: &SourceFile, start: usize, end: usize) -> Range {
    Range::new(
        file.lsp_position(start as u32),
        file.lsp_position(end as u32),
    )
}

/// The byte offset of an LSP position in `file`: always a char boundary,
/// never past the end, so callers may slice `file.text` with it.
pub(super) fn position_to_offset(file: &SourceFile, pos: &Position) -> usize {
    file.offset_of_lsp_position(*pos) as usize
}

/// The byte offset of the character at `pos`, or `None` when there is no
/// character there: `pos` is at or past the end of its line, or past the
/// last line. What hover asks about is a character.
pub(super) fn char_offset_at(file: &SourceFile, pos: &Position) -> Option<usize> {
    let line = file.line_text(pos.line + 1)?;
    let line = line.strip_suffix('\r').unwrap_or(line);
    let line_end = file.line_start(pos.line + 1) as usize + line.len();
    let offset = position_to_offset(file, pos);
    (offset < line_end).then_some(offset)
}

/// Return the UTF-16 code-unit length of a string (what LSP positions count).
pub(super) fn utf16_len(s: &str) -> usize {
    s.chars().map(|c| c.len_utf16()).sum()
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{FileId, SourceName};

    fn file(text: &str) -> SourceFile {
        SourceFile::new(SourceName::Builtin, text.into())
    }

    fn span(start: u32, end: u32) -> Span {
        Span {
            file: FileId::default(),
            start,
            end,
        }
    }

    #[test]
    fn a_range_runs_from_the_start_to_the_end_of_the_span() {
        let f = file("let abc = 1\nlet y = abc");
        assert_eq!(
            span_to_range(&span(4, 7), &f),
            Range::new(Position::new(0, 4), Position::new(0, 7))
        );
        // A span over a line break ends on the next line.
        assert_eq!(
            span_to_range(&span(8, 15), &f),
            Range::new(Position::new(0, 8), Position::new(1, 3))
        );
    }

    #[test]
    fn columns_count_utf16_units() {
        // `😀` is four bytes and two UTF-16 units: `x` after it is at
        // column 2 and ends at column 3.
        let f = file("😀x");
        assert_eq!(
            span_to_range(&span(4, 5), &f),
            Range::new(Position::new(0, 2), Position::new(0, 3))
        );
    }

    #[test]
    fn an_empty_span_covers_the_character_it_points_at() {
        // A lexer error at a rejected character: the range is that whole
        // character, never a slice inside it.
        let f = file("fn main() {\n  println(“hello”)\n}\n");
        let at = f.text.find('“').unwrap() as u32;
        assert_eq!(
            span_to_range(&span(at, at), &f),
            Range::new(Position::new(1, 10), Position::new(1, 11))
        );
        let astral = file("x 😀");
        assert_eq!(
            span_to_range(&span(2, 2), &astral),
            Range::new(Position::new(0, 2), Position::new(0, 4))
        );
        // At the end of the file: one column.
        let empty = file("");
        assert_eq!(
            span_to_range(&span(0, 0), &empty),
            Range::new(Position::new(0, 0), Position::new(0, 1))
        );
    }

    #[test]
    fn no_offset_makes_a_range_panic() {
        let f = file("a😀“b\nλ");
        for start in 0..=f.text.len() as u32 + 3 {
            for end in start..=f.text.len() as u32 + 3 {
                let _ = span_to_range(&span(start, end), &f);
            }
        }
    }

    #[test]
    fn positions_and_offsets_round_trip() {
        let f = file("ab\r\nλx\n");
        for offset in [0usize, 1, 2, 4, 6, 7] {
            let pos = f.lsp_position(offset as u32);
            assert_eq!(position_to_offset(&f, &pos), offset, "{pos:?}");
        }
        // A column past the end of a CRLF line is the end of the line,
        // before the `\r`.
        assert_eq!(position_to_offset(&f, &Position::new(0, 9)), 2);
    }
}
