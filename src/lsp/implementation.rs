//! `textDocument/implementation` handler.
//!
//! Given a cursor on a trait name, return every `trait T for X` impl
//! across the workspace so the editor can offer a picker. We resolve
//! the cursor to a `Symbol` via `find_ident_at_offset`, then sweep
//! every open document's top-level decls for matching `Decl::TraitImpl`
//! entries — same O(docs × decls) pattern as `workspace_lookup_*`.

use lsp_types::Location;
use lsp_types::request::{GotoImplementationParams, GotoImplementationResponse};

use super::Server;
use super::ast_walk::find_ident_at_offset;
use super::conversions::{position_to_offset, span_to_range};
use crate::ast::Decl;

impl Server {
    // ── Go to implementation ───────────────────────────────────────

    pub(super) fn goto_implementation(
        &self,
        params: GotoImplementationParams,
    ) -> Option<GotoImplementationResponse> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let doc = self.documents.get(uri)?;
        let program = doc.program.as_ref()?;

        let cursor = position_to_offset(&doc.source, &pos);
        let name = find_ident_at_offset(program, cursor)?;

        // Walk every open document and collect every TraitImpl whose
        // `trait_name` matches the clicked symbol.
        let mut locations: Vec<Location> = Vec::new();
        for (hit_uri, hit_doc) in &self.documents {
            let Some(hit_program) = &hit_doc.program else {
                continue;
            };
            for decl in &hit_program.decls {
                if let Decl::TraitImpl(ti) = decl
                    && !ti.is_auto_derived
                    && ti.trait_name == name
                {
                    locations.push(Location::new(
                        hit_uri.clone(),
                        span_to_range(&ti.span, &hit_doc.source),
                    ));
                }
            }
        }

        if locations.is_empty() {
            return None;
        }
        if locations.len() == 1 {
            Some(GotoImplementationResponse::Scalar(
                locations.into_iter().next().unwrap(),
            ))
        } else {
            Some(GotoImplementationResponse::Array(locations))
        }
    }
}
