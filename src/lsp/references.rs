//! `textDocument/references` handler.

use lsp_types::Location;

use super::Server;
use super::ast_walk::find_ident_at_offset_with_source;
use super::conversions::position_to_offset;

impl Server {
    pub(super) fn references(&self, params: lsp_types::ReferenceParams) -> Option<Vec<Location>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let doc = self.documents.get(uri)?;
        let program = doc.program.as_ref()?;

        let cursor = position_to_offset(&doc.source, &pos);
        // Use the source-aware variant so cursors on `fn`/`type` decl
        // names resolve (round-63 B2: pre-fix this path returned None
        // when cursor was on the binding-site name).
        let name = find_ident_at_offset_with_source(program, cursor, Some(&doc.source))?;

        let include_definition = params.context.include_declaration;
        let locations = self.workspace_find_references(name, include_definition);
        Some(locations)
    }
}
