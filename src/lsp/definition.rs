//! `textDocument/definition` handler.

use lsp_types::{GotoDefinitionResponse, Location, Range};

use super::Server;
use super::ast_walk::find_field_access_at;
use super::conversions::position_to_offset;
use super::modules::module_qualifier_at;

impl Server {
    /// Where the name under the cursor is declared: a local's binder, a
    /// definition's name in its file, a method's declaration in the impl
    /// or the trait (see `workspace.rs`), a record field's declaration,
    /// the file of the module a qualifier names. Nothing for one of
    /// silt's own names.
    pub(super) fn goto_definition(
        &self,
        params: lsp_types::GotoDefinitionParams,
    ) -> Option<GotoDefinitionResponse> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let doc = self.documents.get(uri)?;
        let cursor = position_to_offset(&doc.source, &pos);
        // A method name at a call: the method the checker selected.
        if let Some(location) = self.method_declaration_at(uri, cursor) {
            return Some(GotoDefinitionResponse::Scalar(location));
        }
        // A field of a record pattern, also a shorthand binder
        // (`Pt { x } ->`): the field's declaration.
        if let Some(location) = self.pattern_field_declaration_at(uri, cursor) {
            return Some(GotoDefinitionResponse::Scalar(location));
        }
        if let Some((_, target)) = self.target_at(uri, cursor) {
            let mut locations = self.declarations_of(uri, &target);
            return match locations.len() {
                0 => None,
                1 => Some(GotoDefinitionResponse::Scalar(locations.remove(0))),
                _ => Some(GotoDefinitionResponse::Array(locations)),
            };
        }
        let program = doc.program.as_ref()?;
        // A field of a record value: its declaration in the record type.
        if let Some((receiver, field)) = find_field_access_at(program, cursor)
            && let crate::types::Type::Generic(ty, _) = receiver
            && let Some(location) = self.field_declaration(doc, ty.id, field)
        {
            return Some(GotoDefinitionResponse::Scalar(location));
        }
        // The module before a dot (`helpers` in `helpers.div`, and in a
        // type, `helpers.Pt`): its file.
        let module = module_qualifier_at(doc, cursor)?;
        let view = self.imported_module(doc, module)?;
        Some(GotoDefinitionResponse::Scalar(Location::new(
            view.uri,
            Range::default(),
        )))
    }
}
