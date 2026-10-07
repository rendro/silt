//! `textDocument/documentHighlight`: every place in the document that
//! names what the name under the cursor names.

use lsp_types::{DocumentHighlight, DocumentHighlightKind};

use super::Server;
use super::conversions::position_to_offset;

impl Server {
    pub(super) fn document_highlight(
        &self,
        params: lsp_types::DocumentHighlightParams,
    ) -> Option<Vec<DocumentHighlight>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let doc = self.documents.get(uri)?;
        let cursor = position_to_offset(&doc.source, &pos);
        let (_, target) = self.target_at(uri, cursor)?;
        // Kind TEXT: a read is not told from a write. Only this document
        // is read: an editor asks whenever the cursor rests.
        let highlights: Vec<DocumentHighlight> = self
            .places_in_document(uri, &target)
            .into_iter()
            .map(|loc| DocumentHighlight {
                range: loc.range,
                kind: Some(DocumentHighlightKind::TEXT),
            })
            .collect();
        (!highlights.is_empty()).then_some(highlights)
    }
}
