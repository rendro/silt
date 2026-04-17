//! Span ↔ LSP position/range conversion utilities.
//!
//! LSP positions count characters in **UTF-16 code units** (per the spec,
//! and what nearly every client uses as the default encoding), so these
//! helpers walk source text rather than using the lexer's codepoint-based
//! column counter directly.

use lsp_types::{Position, Range};

use crate::lexer::Span;

// ── Span ↔ LSP conversion ─────────────────────────────────────────

/// Convert a span to a 0-based LSP `Position`.
///
/// LSP positions count characters in **UTF-16 code units** (per the spec,
/// and what nearly every client uses as the default encoding). The lexer
/// increments `span.col` once per Unicode codepoint (src/lexer.rs:247),
/// which is NOT the same as a UTF-16 unit count for characters outside
/// the BMP (e.g. `😀` is 1 codepoint but 2 UTF-16 units).
///
/// To produce a correct position we walk the source from the start of
/// the line containing `span.offset` up to `span.offset`, summing
/// `ch.len_utf16()` for each character. This uses `span.offset` (a byte
/// offset) as the source of truth rather than the potentially-mismatched
/// codepoint `col`, which the lexer records but which the LSP protocol
/// does not consume.
pub(super) fn span_to_position(span: &Span, source: &str) -> Position {
    let line = span.line.saturating_sub(1) as u32;
    let bytes = source.as_bytes();
    let offset = span.offset.min(bytes.len());

    // Find the byte offset of the start of the line that `offset` lives in.
    // We scan backwards for the most recent '\n' before `offset`.
    let line_start = source[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);

    // Walk from `line_start` to `offset`, accumulating UTF-16 code units.
    // We must respect char boundaries: if `offset` lands mid-character
    // (shouldn't happen for well-formed spans, but be defensive) we clamp
    // at the boundary we reach just before it.
    let mut character: u32 = 0;
    let mut idx = line_start;
    while idx < offset {
        let rest = &source[idx..];
        let Some(ch) = rest.chars().next() else { break };
        let ch_len = ch.len_utf8();
        if idx + ch_len > offset {
            break;
        }
        character += ch.len_utf16() as u32;
        idx += ch_len;
    }

    Position::new(line, character)
}

/// Return the UTF-16 code-unit length of a string (what LSP positions count).
pub(super) fn utf16_len(s: &str) -> usize {
    s.chars().map(|c| c.len_utf16()).sum()
}

/// Compute the byte length of the token that begins at `offset` in `source`.
pub(super) fn token_len_at(source: &str, offset: usize) -> usize {
    let bytes = source.as_bytes();
    if offset >= bytes.len() {
        return 1;
    }
    let first = bytes[offset];
    if first.is_ascii_alphabetic() || first == b'_' {
        let mut end = offset + 1;
        while end < bytes.len() {
            let b = bytes[end];
            if b.is_ascii_alphanumeric() || b == b'_' {
                end += 1;
            } else {
                break;
            }
        }
        return end - offset;
    }
    if first.is_ascii_digit() {
        let mut end = offset + 1;
        let mut seen_dot = false;
        while end < bytes.len() {
            let b = bytes[end];
            if b.is_ascii_digit() || b == b'_' {
                end += 1;
            } else if b == b'.' && !seen_dot {
                if end + 1 < bytes.len() && bytes[end + 1].is_ascii_digit() {
                    seen_dot = true;
                    end += 1;
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        return end - offset;
    }
    if first == b'"' {
        if offset + 2 < bytes.len() && bytes[offset + 1] == b'"' && bytes[offset + 2] == b'"' {
            let mut end = offset + 3;
            while end + 2 < bytes.len() {
                if bytes[end] == b'"' && bytes[end + 1] == b'"' && bytes[end + 2] == b'"' {
                    return end + 3 - offset;
                }
                end += 1;
            }
            return bytes.len() - offset;
        }
        let mut end = offset + 1;
        let mut escape = false;
        while end < bytes.len() {
            let b = bytes[end];
            if escape {
                escape = false;
                end += 1;
                continue;
            }
            if b == b'\\' {
                escape = true;
                end += 1;
                continue;
            }
            if b == b'"' {
                return end + 1 - offset;
            }
            if b == b'\n' {
                return end - offset;
            }
            end += 1;
        }
        return end - offset;
    }
    if offset + 1 < bytes.len() {
        let two = &bytes[offset..offset + 2];
        if matches!(
            two,
            b"==" | b"!=" | b"<=" | b">=" | b"->" | b"=>" | b".." | b"::" | b"|>" | b"&&" | b"||"
        ) {
            return 2;
        }
    }
    1
}

/// Convert a span to an LSP range, using the source text to determine the
/// byte length of the token at `span.offset`. Converts both the start and
/// computed end byte offsets to line/column via the same logic, rather than
/// hard-coding `end = start + 1`, so multi-character identifiers produce a
/// correctly-sized range.
pub(super) fn span_to_range(span: &Span, source: &str) -> Range {
    let start = span_to_position(span, source);
    let len = token_len_at(source, span.offset);
    let bytes = source.as_bytes();
    let end_col = if span.offset >= bytes.len() {
        start.character + 1
    } else {
        let slice_end = (span.offset + len).min(bytes.len());
        let slice = &source[span.offset..slice_end];
        if let Some(nl) = slice.find('\n') {
            let first_line = &slice[..nl];
            start.character + utf16_len(first_line) as u32
        } else {
            start.character + utf16_len(slice) as u32
        }
    };
    let end = Position::new(start.line, end_col);
    Range::new(start, end)
}

/// Convert an LSP 0-based line/character to a byte offset into the source.
pub(super) fn position_to_offset(source: &str, pos: &Position) -> usize {
    let mut offset = 0;
    for (i, line) in source.lines().enumerate() {
        if i == pos.line as usize {
            let mut utf16_offset = 0u32;
            for (byte_idx, ch) in line.char_indices() {
                if utf16_offset >= pos.character {
                    return offset + byte_idx;
                }
                utf16_offset += ch.len_utf16() as u32;
            }
            return offset + line.len();
        }
        // Account for actual line ending: \r\n (2 bytes) or \n (1 byte).
        let line_end = offset + line.len();
        let newline_len = if source.as_bytes().get(line_end) == Some(&b'\r')
            && source.as_bytes().get(line_end + 1) == Some(&b'\n')
        {
            2
        } else {
            1
        };
        offset += line.len() + newline_len;
    }
    offset
}

/// Build an LSP range for a binding at `(offset, len)` using the source text.
pub(super) fn binding_range(source: &str, offset: usize, len: usize) -> Option<Range> {
    if offset > source.len() || !source.is_char_boundary(offset) {
        return None;
    }
    let end_byte = (offset + len).min(source.len());
    let end_byte = if source.is_char_boundary(end_byte) {
        end_byte
    } else {
        return None;
    };

    // Compute line/column for the start offset.
    let mut line = 0u32;
    let mut col = 0u32;
    let mut idx = 0usize;
    for ch in source.chars() {
        if idx == offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 0;
        } else {
            col += ch.len_utf16() as u32;
        }
        idx += ch.len_utf8();
    }
    let start = Position::new(line, col);
    let end_col = col + utf16_len(&source[offset..end_byte]) as u32;
    let end = Position::new(line, end_col);
    Some(Range::new(start, end))
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Span;

    // ── position_to_offset ────────────────────────────────────────

    #[test]
    fn test_position_to_offset_first_line() {
        let source = "let x = 42\nlet y = 10";
        let pos = Position::new(0, 4); // 'x'
        assert_eq!(position_to_offset(source, &pos), 4);
    }

    #[test]
    fn test_position_to_offset_second_line() {
        let source = "let x = 42\nlet y = 10";
        let pos = Position::new(1, 4); // 'y'
        assert_eq!(position_to_offset(source, &pos), 15);
    }

    #[test]
    fn test_position_to_offset_start() {
        let source = "hello\nworld";
        let pos = Position::new(0, 0);
        assert_eq!(position_to_offset(source, &pos), 0);
    }

    #[test]
    fn test_position_to_offset_past_end() {
        let source = "ab\ncd";
        // Line 0, col 99 — clamps to end of line
        let pos = Position::new(0, 99);
        assert_eq!(position_to_offset(source, &pos), 2);
    }

    // ── span_to_position ──────────────────────────────────────────

    #[test]
    fn test_span_to_position() {
        // Line 3, col 5 in this source points at the 'e' in "else".
        //
        //   1: let\n      (bytes 0..4)
        //   2: foo\n      (bytes 4..8)
        //   3: else\n     (bytes 8..13)  — 'e' at byte 8 (col 1), '…' unused
        //                                   index of 'e' of col 5 would be byte 12
        //                                   but we want start-of-line col 5 = 'e'
        // Simpler: use a clean ASCII source and put the 5th column on line 3.
        let source = "let\nfoo\n    x = 1"; // line 3 col 5 is 'x' at byte 12.
        let span = Span {
            line: 3,
            col: 5,
            offset: 12,
        };
        let pos = span_to_position(&span, source);
        assert_eq!(pos.line, 2); // 0-based
        assert_eq!(pos.character, 4); // 0-based
    }

    #[test]
    fn test_span_to_position_saturates() {
        // An out-of-range span (line 0, offset 0) should not panic; it should
        // yield a position at the very beginning of the document.
        let source = "anything";
        let span = Span {
            line: 0,
            col: 0,
            offset: 0,
        };
        let pos = span_to_position(&span, source);
        assert_eq!(pos.line, 0);
        assert_eq!(pos.character, 0);
    }

    // ── span_to_position UTF-16 correctness ───────────────────────

    /// Astral-plane character (emoji) earlier on the SAME line should shift
    /// subsequent `character` values by 2 UTF-16 code units per emoji, not
    /// 1 (which is what a naive codepoint-based implementation would give).
    #[test]
    fn test_span_to_position_uses_utf16_for_astral_characters() {
        // 😀 is U+1F600, 4 bytes UTF-8, 2 UTF-16 code units, 1 codepoint.
        // Source: "😀x" — 'x' starts at byte 4.
        let source = "😀x";
        // The span for 'x' should be at line 1 (1-indexed), col 2 (1-indexed
        // codepoint, matching what the lexer would produce), byte offset 4.
        let span = Span {
            line: 1,
            col: 2,
            offset: 4,
        };
        let pos = span_to_position(&span, source);
        assert_eq!(pos.line, 0, "line should be 0-based");
        // 😀 contributes 2 UTF-16 code units, so 'x' is at character 2,
        // NOT character 1 (which is what the old codepoint-based helper
        // would have returned: col=2 → character=1).
        assert_eq!(
            pos.character, 2,
            "character must be UTF-16 code units, not codepoints"
        );
    }

    /// A span on a line AFTER a line containing an emoji should NOT be
    /// shifted — UTF-16 offsets reset per line, same as codepoint offsets.
    /// This is a regression guard against an implementation that forgets
    /// to reset the column counter at newlines.
    #[test]
    fn test_span_to_position_utf16_resets_per_line() {
        // Line 1: "😀\n"  — bytes 0..5  (😀 = 4 bytes, \n = 1 byte)
        // Line 2: "xy"    — bytes 5..7
        let source = "😀\nxy";
        // 'y' on line 2 at byte offset 6, codepoint col 2.
        let span = Span {
            line: 2,
            col: 2,
            offset: 6,
        };
        let pos = span_to_position(&span, source);
        assert_eq!(pos.line, 1);
        // On line 2, 'y' is just after 'x' — 1 UTF-16 unit from the start
        // of the line. The emoji on line 1 must NOT bleed into line 2.
        assert_eq!(pos.character, 1);
    }

    /// For pure-ASCII input the new helper must return the same Position
    /// that the old codepoint-based implementation did — backwards compat.
    #[test]
    fn test_span_to_position_ascii_unchanged() {
        let source = "hello\nworld\nagain";
        // 'g' on line 3, codepoint col 3, byte offset 14.
        let span = Span {
            line: 3,
            col: 3,
            offset: 14,
        };
        let pos = span_to_position(&span, source);
        assert_eq!(pos.line, 2);
        assert_eq!(pos.character, 2);

        // Also check a span on line 1 — col 1 should always be character 0.
        let span1 = Span {
            line: 1,
            col: 1,
            offset: 0,
        };
        let pos1 = span_to_position(&span1, source);
        assert_eq!(pos1.line, 0);
        assert_eq!(pos1.character, 0);

        // 'l' (second one, offset 3) on line 1.
        let span2 = Span {
            line: 1,
            col: 4,
            offset: 3,
        };
        let pos2 = span_to_position(&span2, source);
        assert_eq!(pos2.line, 0);
        assert_eq!(pos2.character, 3);
    }

    /// An end-to-end flavour: `span_to_range` must also produce UTF-16
    /// ranges when diagnostics live after an astral character. This
    /// exercises the `make_diagnostic` → `span_to_range` → `span_to_position`
    /// pipeline that LSP clients actually see. Uses TWO emojis so that the
    /// buggy codepoint-based and the correct UTF-16-based implementations
    /// disagree on the start column (one codepoint vs two UTF-16 units per
    /// emoji → divergence grows linearly with the emoji count).
    #[test]
    fn test_span_to_range_uses_utf16_after_emoji() {
        // Source: "😀😀bad" — each 😀 is 4 UTF-8 bytes, 1 codepoint,
        // 2 UTF-16 code units. 'b' starts at byte 8.
        //   Codepoint col of 'b' = 3 (lexer advances col by 1 per char).
        //   Correct UTF-16 character = 4.
        let source = "😀😀bad";
        let span = Span {
            line: 1,
            col: 3,
            offset: 8,
        };
        let range = span_to_range(&span, source);
        // Start: two emojis × 2 UTF-16 units each = 4.
        // Buggy implementation would return col-1 = 2, which DIFFERS from 4.
        assert_eq!(
            range.start.character, 4,
            "range start must count UTF-16 units, not codepoints"
        );
        // Token "bad" is 3 UTF-16 units long, so end.character = 7.
        assert_eq!(
            range.end.character, 7,
            "range end must extend by UTF-16 length of the token"
        );
    }

    // ── position_to_offset: UTF-16 handling ──────────────────────

    #[test]
    fn test_position_to_offset_empty_source() {
        let source = "";
        let pos = Position::new(0, 0);
        assert_eq!(position_to_offset(source, &pos), 0);
    }

    #[test]
    fn test_position_to_offset_multiline() {
        let source = "abc\ndef\nghi";
        // line 2, col 1 → 'h' at offset 8
        let pos = Position::new(2, 1);
        assert_eq!(position_to_offset(source, &pos), 9);
    }

    // ── span_to_range ────────────────────────────────────────────

    #[test]
    fn test_span_to_range_empty_source() {
        // A span referring to line 3 col 5 in an empty source is nonsensical,
        // but the helper must not panic and must produce a one-column range.
        // Under the UTF-16-correct implementation, the start column is walked
        // from the source text — so for an empty source the character count
        // is 0 (there are no characters to count); the line value still comes
        // from span.line. The range has width 1 (token_len_at on an empty
        // source returns 1 by contract).
        let span = Span {
            line: 3,
            col: 5,
            offset: 0,
        };
        let range = span_to_range(&span, "");
        assert_eq!(range.start.line, 2);
        assert_eq!(range.start.character, 0);
        assert_eq!(range.end.line, 2);
        assert_eq!(range.end.character, 1);
    }

    #[test]
    fn test_span_to_range_identifier_width() {
        // Regression: a span at a multi-character identifier must produce a
        // range whose width equals the identifier's length, not just 1.
        let source = "let println = 42";
        let span = Span {
            line: 1,
            col: 5,
            offset: 4,
        };
        let range = span_to_range(&span, source);
        assert_eq!(range.start.line, 0);
        assert_eq!(range.start.character, 4);
        assert_eq!(range.end.line, 0);
        // `println` is 7 characters wide, so end column = 4 + 7 = 11.
        assert_eq!(range.end.character, 11);
    }

    #[test]
    fn test_span_to_range_multiline_source() {
        // On line 2, both line and column math must use the span's own
        // line/col — not hard-coded values — and the end should land at the
        // end of the identifier, on the same line.
        let source = "let a = 1\nlet foobar = 2";
        let span = Span {
            line: 2,
            col: 5,
            offset: 14,
        };
        let range = span_to_range(&span, source);
        assert_eq!(range.start.line, 1);
        assert_eq!(range.start.character, 4);
        assert_eq!(range.end.line, 1);
        // `foobar` is 6 characters wide.
        assert_eq!(range.end.character, 10);
    }

    #[test]
    fn test_token_len_at_identifier() {
        assert_eq!(token_len_at("println x", 0), 7);
        assert_eq!(token_len_at("let abc = 1", 4), 3);
        assert_eq!(token_len_at("foo_bar + 1", 0), 7);
    }

    #[test]
    fn test_token_len_at_number() {
        assert_eq!(token_len_at("42 + 1", 0), 2);
        assert_eq!(token_len_at("3.14", 0), 4);
        // `1..10` should stop at the `.` because it's a range, not a float.
        assert_eq!(token_len_at("1..10", 0), 1);
    }

    #[test]
    fn test_token_len_at_string() {
        assert_eq!(token_len_at(r#""hi" end"#, 0), 4);
        assert_eq!(token_len_at(r#""esc\"ape""#, 0), 10);
    }

    #[test]
    fn test_token_len_at_past_end() {
        // Out-of-bounds offset must not panic.
        assert_eq!(token_len_at("x", 99), 1);
        assert_eq!(token_len_at("", 0), 1);
    }
}
