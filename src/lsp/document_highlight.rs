//! `textDocument/documentHighlight` — highlight every occurrence of
//! the identifier under the cursor, scoped to the current document.

use lsp_types::{DocumentHighlight, DocumentHighlightKind};

use super::Server;
use super::ast_walk::find_ident_at_offset;
use super::conversions::position_to_offset;

impl Server {
    pub(super) fn document_highlight(
        &self,
        params: lsp_types::DocumentHighlightParams,
    ) -> Option<Vec<DocumentHighlight>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let doc = self.documents.get(uri)?;
        let program = doc.program.as_ref()?;
        let cursor = position_to_offset(&doc.source, &pos);
        let name = find_ident_at_offset(program, cursor)?;

        // Reuse the workspace references walker but filter to current
        // document. Kind: TEXT — we don't distinguish read vs write.
        let locations = self.workspace_find_references(name, true);
        let highlights: Vec<DocumentHighlight> = locations
            .into_iter()
            .filter(|loc| loc.uri == *uri)
            .map(|loc| DocumentHighlight {
                range: loc.range,
                kind: Some(DocumentHighlightKind::TEXT),
            })
            .collect();
        if highlights.is_empty() {
            None
        } else {
            Some(highlights)
        }
    }
}
