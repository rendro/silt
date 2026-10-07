//! `textDocument/typeDefinition` handler.
//!
//! Given a cursor on any expression, look up the inferred type and, if
//! the head of that type names a user-defined declaration (record,
//! enum, or a user-declared generic like `Option(a)`), jump to that
//! declaration's span. For built-in types (`Int`, `List(a)`, …) we have
//! no user-authored declaration to point at, so we return `None` and
//! LSP clients render "no type definition".

use lsp_types::request::{GotoTypeDefinitionParams, GotoTypeDefinitionResponse};

use super::Server;
use super::ast_walk::find_type_at_offset;
use super::conversions::position_to_offset;
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
        // The declaration of the type's head, by its definition.
        let target = self.type_target(doc, type_head(&ty)?)?;
        let mut locations = self.declarations_of(uri, &target);
        match locations.len() {
            0 => None,
            1 => Some(GotoTypeDefinitionResponse::Scalar(locations.remove(0))),
            _ => Some(GotoTypeDefinitionResponse::Array(locations)),
        }
    }
}

/// The type a `type` declaration declares, of which `ty` is an
/// instance. Only a nominal type has one; a structural type (tuple,
/// list, function, ...) has none.
fn type_head(ty: &Type) -> Option<crate::defs::TypeId> {
    match ty {
        Type::Generic(name, _) => Some(name.id),
        _ => None,
    }
}
