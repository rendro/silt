//! `textDocument/definition` handler.

use lsp_types::{GotoDefinitionResponse, Location};

use super::Server;
use super::ast_walk::find_ident_at_offset;
use super::conversions::{binding_range, position_to_offset, span_to_range};
use super::local_bindings::{find_local_binding_at_offset, nearest_local_binding_for};

impl Server {
    // ── Go to definition ───────────────────────────────────────────

    pub(super) fn goto_definition(
        &self,
        params: lsp_types::GotoDefinitionParams,
    ) -> Option<GotoDefinitionResponse> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let doc = self.documents.get(uri)?;
        let program = doc.program.as_ref()?;

        let cursor = position_to_offset(&doc.source, &pos);

        // If the cursor is already ON a binding site, jump to itself. This
        // gives editors a sensible answer and keeps goto-def idempotent.
        if let Some(binding) = find_local_binding_at_offset(&doc.locals, cursor)
            && let Some(range) =
                binding_range(&doc.source, binding.binding_offset, binding.binding_len)
        {
            return Some(GotoDefinitionResponse::Scalar(Location::new(
                uri.clone(),
                range,
            )));
        }

        let name = find_ident_at_offset(program, cursor)?;

        // Prefer local bindings in scope at the cursor position.
        if let Some(binding) = nearest_local_binding_for(&doc.locals, name, cursor) {
            return Some(GotoDefinitionResponse::Scalar(Location::new(
                uri.clone(),
                binding_range(&doc.source, binding.binding_offset, binding.binding_len)?,
            )));
        }

        let def = doc.definitions.get(&name)?;

        Some(GotoDefinitionResponse::Scalar(Location::new(
            uri.clone(),
            span_to_range(&def.span, &doc.source),
        )))
    }
}
