//! `textDocument/definition` handler.

use lsp_types::{GotoDefinitionResponse, Location};

use super::Server;
use super::ast_walk::find_ident_at_offset;
use super::conversions::{offsets_to_range, position_to_offset, span_to_range};
use super::local_bindings::{find_local_binding_at_offset, nearest_local_binding_for};
use super::modules::qualified_access_at;
use super::state::LocalBinding;
use crate::source::SourceFile;

/// The range of a local binding's identifier.
fn binding_location(source: &SourceFile, binding: &LocalBinding) -> lsp_types::Range {
    let start = binding.binding_offset;
    offsets_to_range(source, start, start + binding.binding_len)
}

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
        if let Some(binding) = find_local_binding_at_offset(&doc.locals, cursor) {
            return Some(GotoDefinitionResponse::Scalar(Location::new(
                uri.clone(),
                binding_location(&doc.source, binding),
            )));
        }

        // A member of an imported module, `m.f`: its definition in that
        // module's file, unless a local binding shadows `m`.
        if let Some((module, member)) = qualified_access_at(program, cursor)
            && nearest_local_binding_for(&doc.locals, module, cursor).is_none()
            && let Some(view) = self.imported_module(doc, module)
            && let Some(def) = view.definitions.get(&member)
        {
            return Some(GotoDefinitionResponse::Scalar(Location::new(
                view.uri.clone(),
                span_to_range(&def.span, view.source),
            )));
        }

        // Source-aware so cursor on `fn`/`type` decl names resolves
        // (round-63 B2 — match rename/hover behaviour).
        let name = find_ident_at_offset(program, cursor)?;

        // Prefer local bindings in scope at the cursor position.
        if let Some(binding) = nearest_local_binding_for(&doc.locals, name, cursor) {
            return Some(GotoDefinitionResponse::Scalar(Location::new(
                uri.clone(),
                binding_location(&doc.source, binding),
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

        // An item of `import m.{ f }`: its definition in `m`'s file.
        if let Some(view) = self.item_module(doc, name)
            && let Some(def) = view.definitions.get(&name)
        {
            return Some(GotoDefinitionResponse::Scalar(Location::new(
                view.uri.clone(),
                span_to_range(&def.span, view.source),
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
                let src = &self.documents.get(&hit_uri)?.source;
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
