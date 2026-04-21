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

        // Current-file definition first; fall back to workspace-wide
        // lookup when the identifier isn't declared in this file.
        if let Some(def) = doc.definitions.get(&name) {
            return Some(GotoDefinitionResponse::Scalar(Location::new(
                uri.clone(),
                span_to_range(&def.span, &doc.source),
            )));
        }

        // Workspace fallback: search every open document's top-level
        // definitions. Multiple hits become an array response — LSP
        // clients display a picker.
        let hits = self.workspace_lookup_definition(name);
        if hits.is_empty() {
            return None;
        }
        let locations: Vec<Location> = hits
            .into_iter()
            .filter_map(|(hit_uri, span)| {
                let src = self.documents.get(&hit_uri).map(|d| d.source.as_str())?;
                Some(Location::new(hit_uri, span_to_range(&span, src)))
            })
            .collect();
        if locations.len() == 1 {
            Some(GotoDefinitionResponse::Scalar(
                locations.into_iter().next().unwrap(),
            ))
        } else {
            Some(GotoDefinitionResponse::Array(locations))
        }
    }
}
