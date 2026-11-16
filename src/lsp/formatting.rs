//! `textDocument/formatting` handler.

use lsp_server::Message;
use lsp_types::notification::{Notification as _, ShowMessage};
use lsp_types::{Position, Range, TextEdit};

use super::Server;
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

        // Replace the entire document: from its start to the position of
        // the end of its text, with lines counted as the protocol counts
        // them (a lone `\r` ends one too).
        let end_position = doc.source.lsp_position(doc.source.text.len() as u32);
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
