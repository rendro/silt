//! `textDocument/references` handler.

use lsp_types::Location;

use super::Server;
use super::ast_walk::find_ident_at_offset;
use super::conversions::position_to_offset;

impl Server {
    pub(super) fn references(
        &self,
        params: lsp_types::ReferenceParams,
    ) -> Option<Vec<Location>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let doc = self.documents.get(uri)?;
        let program = doc.program.as_ref()?;

        let cursor = position_to_offset(&doc.source, &pos);
        let name = find_ident_at_offset(program, cursor)?;

        let include_definition = params.context.include_declaration;
        let locations = self.workspace_find_references(name, include_definition);
        Some(locations)
    }
}
