//! `textDocument/typeDefinition` handler.
//!
//! Given a cursor on any expression, look up the inferred type and, if
//! the head of that type names a user-defined declaration (record,
//! enum, or a user-declared generic like `Option(a)`), jump to that
//! declaration's span. For built-in types (`Int`, `List(a)`, …) we have
//! no user-authored declaration to point at, so we return `None` and
//! LSP clients render "no type definition".

use lsp_types::Location;
use lsp_types::request::{GotoTypeDefinitionParams, GotoTypeDefinitionResponse};

use super::Server;
use super::ast_walk::find_type_at_offset;
use super::conversions::{position_to_offset, span_to_range};
use crate::intern::Symbol;
use crate::types::Type;

impl Server {
    // ── Go to type definition ──────────────────────────────────────

    pub(super) fn type_definition(
        &self,
        params: GotoTypeDefinitionParams,
    ) -> Option<GotoTypeDefinitionResponse> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let doc = self.documents.get(uri)?;
        let program = doc.program.as_ref()?;

        let cursor = position_to_offset(&doc.source, &pos);
        let ty = find_type_at_offset(program, cursor)?;
        let name = type_head_name(&ty)?;

        // Look up the type's declaration anywhere in the workspace.
        // The definitions map is populated from top-level decls, so
        // records, enums, and traits all live there.
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
        if locations.is_empty() {
            return None;
        }
        if locations.len() == 1 {
            Some(GotoTypeDefinitionResponse::Scalar(
                locations.into_iter().next().unwrap(),
            ))
        } else {
            Some(GotoTypeDefinitionResponse::Array(locations))
        }
    }
}

/// Extract the "head" name of a type — the identifier that a user's
/// `type` declaration would bind. Only nominal types (records and
/// generics) have a head name; structural types (tuple, list, fn, …)
/// and primitives return `None`.
fn type_head_name(ty: &Type) -> Option<Symbol> {
    match ty {
        Type::Record(name, _) | Type::Generic(name, _) => Some(*name),
        _ => None,
    }
}
