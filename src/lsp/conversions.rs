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

/// The byte offset of the character that hover asks about at `pos`: the
/// one at `pos`, or the last character of a name that `pos` stands
/// directly behind (`yy|` at the end of a line or before a space), as
/// definition and the other requests take a name's end. `None` where
/// there is no such character: past the last line, past the end of its
/// line, at the end of a line that ends in no name.
pub(super) fn char_offset_at(file: &SourceFile, pos: &Position) -> Option<usize> {
    let (line_start, line) = file.lsp_line(pos.line)?;
    if pos.character as usize > utf16_len(line) {
        return None;
    }
    let line_end = line_start + line.len();
    let offset = position_to_offset(file, pos);
    let name_char = |c: char| c.is_alphanumeric() || c == '_';
    let at = file.text[offset..line_end].chars().next();
    if at.is_some_and(name_char) {
        return Some(offset);
    }
    match file.text[line_start..offset].chars().next_back() {
        Some(before) if name_char(before) => Some(offset - before.len_utf8()),
        _ => at.map(|_| offset),
    }
}

/// `text` as a silt code block of Markdown. Its fence is a run of
/// backticks longer than any in the text (at least three, the CommonMark
/// way), so a line of the text that is only ``` (a declaration with a
/// block comment over lines) cannot close it early.
pub(super) fn code_block(text: &str) -> String {
    let longest = (text.split(|c| c != '`')).map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    format!("{fence}silt\n{text}\n{fence}")
}

/// Markdown for the editor (a hover, the documentation of a completion
/// or a signature): `value` with each control character, invisible or
/// direction character shown as the line of source under a diagnostic
/// shows it (`diagnostic::visible_line`: ESC as a picture), so a doc
/// comment cannot hand the editor a raw ESC. Its line ends (`\n`, `\r`)
/// stay line ends.
pub(super) fn markdown(value: String) -> lsp_types::MarkupContent {
    let mut shown = String::with_capacity(value.len());
    let mut rest = value.as_str();
    while let Some(at) = rest.find(['\n', '\r']) {
        shown.push_str(&crate::diagnostic::visible_line(&rest[..at]));
        shown.push_str(&rest[at..at + 1]);
        rest = &rest[at + 1..];
    }
    shown.push_str(&crate::diagnostic::visible_line(rest));
    lsp_types::MarkupContent {
        kind: lsp_types::MarkupKind::Markdown,
        value: shown,
    }
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

    #[test]
    fn a_code_block_s_fence_is_longer_than_any_backtick_run_in_it() {
        assert_eq!(code_block("x: Int"), "```silt\nx: Int\n```");
        assert_eq!(
            code_block("type T {\n  {-\n```\n  -}\n}"),
            "````silt\ntype T {\n  {-\n```\n  -}\n}\n````"
        );
        assert!(code_block("a ````` b").starts_with("``````silt"));
    }

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
