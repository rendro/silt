//! `textDocument/formatting` handler.

use lsp_server::Message;
use lsp_types::notification::{Notification as _, ShowMessage};
use lsp_types::{Position, Range, TextEdit};

use super::Server;
use super::conversions::utf16_len;
use crate::diagnostic::Code;
use crate::source::FileId;

impl Server {
    // ── Formatting ────────────────────────────────────────────────

    pub(super) fn format(
        &self,
        params: lsp_types::DocumentFormattingParams,
    ) -> Option<Vec<TextEdit>> {
        let uri = &params.text_document.uri;
        let doc = self.documents.get(uri)?;
        // No edits for a document that does not parse (its diagnostics
        // say why), and none for one whose formatted text the formatter
        // refused: the editor's buffer stays as it is, and the refusal
        // is shown as a message.
        let formatted = match crate::format::format(FileId::default(), &doc.source.text) {
            Ok(formatted) => formatted,
            Err(refusal) if refusal.code == Code::FormatRefused => {
                self.show_warning(&refusal.message);
                return None;
            }
            Err(_) => return None,
        };

        if *formatted == *doc.source.text {
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
            let src: &str = &doc.source.text;
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

    /// A `window/showMessage` warning: what the user is told when a
    /// request cannot do its work.
    fn show_warning(&self, message: &str) {
        let params = lsp_types::ShowMessageParams {
            typ: lsp_types::MessageType::WARNING,
            message: message.to_string(),
        };
        let notification = lsp_server::Notification::new(ShowMessage::METHOD.to_string(), params);
        self.connection
            .sender
            .send(Message::Notification(notification))
            .ok();
    }
}
