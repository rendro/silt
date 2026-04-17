//! `textDocument/formatting` handler.

use lsp_types::{Position, Range, TextEdit};

use super::Server;
use super::conversions::utf16_len;

impl Server {
    // ── Formatting ────────────────────────────────────────────────

    pub(super) fn format(
        &self,
        params: lsp_types::DocumentFormattingParams,
    ) -> Option<Vec<TextEdit>> {
        let uri = &params.text_document.uri;
        let doc = self.documents.get(uri)?;
        let formatted = crate::formatter::format(&doc.source).ok()?;

        if formatted == doc.source {
            return Some(vec![]);
        }

        // Replace the entire document. `Position.character` is defined in
        // UTF-16 code units by the LSP spec, and `Position.line` must be a
        // valid 0-based line index — NOT one past the last line. Compute
        // both from the raw source so we stay correct for multibyte input.
        //
        // Three cases:
        //   1. Empty source        → (0, 0)..(0, 0)
        //   2. Trailing newline(s) → end at (line_after_last_newline, 0)
        //   3. No trailing newline → end at (last_line_idx, utf16_len(last))
        let end_position = {
            let src = doc.source.as_str();
            if src.is_empty() {
                Position::new(0, 0)
            } else if src.ends_with('\n') {
                // Count newlines to determine how many lines are fully
                // terminated. The "virtual" line that follows the final
                // `\n` starts at column 0.
                let newline_count = src.bytes().filter(|b| *b == b'\n').count() as u32;
                Position::new(newline_count, 0)
            } else {
                // No trailing newline — the final line is indexed by the
                // number of newlines seen so far, and its end column is
                // the UTF-16 length of its content.
                let newline_count = src.bytes().filter(|b| *b == b'\n').count() as u32;
                let last_line = src.rsplit('\n').next().unwrap_or("");
                Position::new(newline_count, utf16_len(last_line) as u32)
            }
        };
        Some(vec![TextEdit {
            range: Range::new(Position::new(0, 0), end_position),
            new_text: formatted,
        }])
    }
}
