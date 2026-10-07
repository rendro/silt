//! `textDocument/definition` handler.

use lsp_types::GotoDefinitionResponse;

use super::Server;
use super::conversions::position_to_offset;

impl Server {
    /// Where the name under the cursor is declared: a local's binder, a
    /// definition's name in its file (see `workspace.rs`). Nothing for
    /// one of silt's own names.
    pub(super) fn goto_definition(
        &self,
        params: lsp_types::GotoDefinitionParams,
    ) -> Option<GotoDefinitionResponse> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let doc = self.documents.get(uri)?;
        let cursor = position_to_offset(&doc.source, &pos);
        let (_, target) = self.target_at(uri, cursor)?;
        let mut locations = self.declarations_of(uri, &target);
        match locations.len() {
            0 => None,
            1 => Some(GotoDefinitionResponse::Scalar(locations.remove(0))),
            _ => Some(GotoDefinitionResponse::Array(locations)),
        }
    }
}
