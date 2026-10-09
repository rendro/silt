//! `textDocument/references` handler.

use lsp_types::Location;

use super::Server;
use super::conversions::position_to_offset;

impl Server {
    pub(super) fn references(
        &mut self,
        params: lsp_types::ReferenceParams,
    ) -> Option<Vec<Location>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let doc = self.documents.get(uri)?;
        let cursor = position_to_offset(&doc.source, &pos);
        let (_, target) = self.target_at(uri, cursor)?;
        Some(self.references_to(uri, &target, params.context.include_declaration))
    }
}
