//! `textDocument/rename` and `textDocument/prepareRename` handlers.
//!
//! A rename edits every place that names what the name under the cursor
//! names (see `workspace.rs`): the declaration, each use, each item of
//! an `import m.{ item }`. A function of another module with the same
//! name, or a local that shadows it, is something else and is left as
//! it is. A record field is renamed in its type, its literals, updates,
//! reads and patterns; a field of the same name of another record type
//! is another field.
//!
//! What cannot be renamed: one of silt's own definitions (`Some`,
//! `list.map`, `Int`) and their fields, a field of a type of another
//! package, `self`, and anything the resolver gave no meaning (a
//! keyword). The new name must be one identifier to the lexer, of the
//! same kind as the old one (a type, a variant or a trait starts with
//! an upper-case letter, anything else does not), and free where the
//! old one is written: a rename that would capture another name, or be
//! captured by one, is refused with the clash; so is a field name the
//! type has already.
//! A record field written without its value (`P { x }`) is written out:
//! renaming what it holds gives `P { x: new }`, since the field keeps
//! its name; renaming the field gives `P { new: x }`, since the local
//! keeps its.
//!
//! The files edited are the open documents, the modules they import,
//! and the workspace files that import the definition's module.

use std::collections::HashMap;

use lsp_server::{ErrorCode, Response};
use lsp_types::{PrepareRenameResponse, TextEdit, Uri, WorkspaceEdit};

use crate::lexer;

use super::Server;
use super::conversions::{position_to_offset, span_to_range};
use super::workspace::Target;

impl Server {
    /// `textDocument/prepareRename`: the range of the name under the
    /// cursor, if it can be renamed: not where `rename` would refuse
    /// whatever the new name is (one of silt's own names, `self`, a name
    /// that a declaration which failed to parse could hold).
    pub(super) fn prepare_rename(
        &mut self,
        params: lsp_types::TextDocumentPositionParams,
    ) -> Option<PrepareRenameResponse> {
        let uri = &params.text_document.uri;
        let doc = self.documents.get(uri)?;
        let cursor = position_to_offset(&doc.source, &params.position);
        let (span, target) = self.target_at(uri, cursor)?;
        let range = span_to_range(&span, &doc.source);
        if self.not_renameable(uri, &target).is_some()
            || self.incomplete_for_rename(uri, &target).is_some()
            || self.unknown_field_reads(&target).is_some()
        {
            return None;
        }
        Some(PrepareRenameResponse::Range(range))
    }

    /// `textDocument/rename`: one edit per place that names the target.
    pub(super) fn rename(
        &mut self,
        params: lsp_types::RenameParams,
        request_id: lsp_server::RequestId,
    ) -> Result<Option<WorkspaceEdit>, Response> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let new_name = params.new_name.clone();
        let invalid = |message: String| {
            Response::new_err(request_id.clone(), ErrorCode::InvalidParams as i32, message)
        };

        if !is_valid_silt_ident(&new_name) {
            return Err(invalid(format!(
                "`{new_name}` is not a valid silt identifier"
            )));
        }
        let Some(doc) = self.documents.get(uri) else {
            return Ok(None);
        };
        let cursor = position_to_offset(&doc.source, &pos);
        let Some((span, target)) = self.target_at(uri, cursor) else {
            return Ok(None);
        };
        let old_name = doc.source.text[span.start as usize..span.end as usize].to_string();
        if let Some(why) = self.not_renameable(uri, &target) {
            return Err(invalid(format!("`{old_name}` {why}")));
        }
        // An upper-case first letter is what makes a name a type, a
        // variant or a trait.
        let upper = |name: &str| name.starts_with(|c: char| c.is_ascii_uppercase());
        if upper(&old_name) != upper(&new_name) {
            let kind = if upper(&old_name) {
                "starts with an upper-case letter, as the name of a type, a variant or a trait does"
            } else {
                "does not start with an upper-case letter, which only the name of a type, a \
                 variant or a trait does"
            };
            return Err(invalid(format!(
                "`{old_name}` {kind}: `{new_name}` would be a name of another kind"
            )));
        }
        if new_name == old_name {
            return Ok(None);
        }
        if let Some(broken) = self.incomplete_for_rename(uri, &target) {
            return Err(invalid(format!(
                "`{old_name}` cannot be renamed now: {broken}; what the names in the declaration \
                 that holds it mean is not known, and a rename would leave them as they are"
            )));
        }
        if let Some(unknown) = self.unknown_field_reads(&target) {
            return Err(invalid(format!(
                "`{old_name}` cannot be renamed now: {unknown}; fix that first, so the rename \
                 can find every place of the field"
            )));
        }
        if let Some(clash) = self.rename_clash(uri, &target, &new_name) {
            // (A field is named after its value only: no other name
            // can capture it.)
            if let Target::Field { .. } = target {
                return Err(invalid(format!(
                    "`{old_name}` cannot be renamed to `{new_name}`: {clash}"
                )));
            }
            return Err(invalid(format!(
                "`{old_name}` cannot be renamed to `{new_name}`: {clash}, and the rename would \
                 change what a name means"
            )));
        }

        let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();
        let field = matches!(target, Target::Field { .. });
        for place in self.places_of(uri, &target, true) {
            // A field written without its value is written out: for a
            // rename of what it holds, `P { x }` becomes `P { x: new }`;
            // for a rename of the field, `P { new: x }`.
            let new_text = match (place.punned, field) {
                (true, false) => format!("{old_name}: {new_name}"),
                (true, true) => format!("{new_name}: {old_name}"),
                (false, _) => new_name.clone(),
            };
            changes
                .entry(place.location.uri)
                .or_default()
                .push(TextEdit {
                    range: place.location.range,
                    new_text,
                });
        }
        if changes.is_empty() {
            return Ok(None);
        }
        Ok(Some(WorkspaceEdit {
            changes: Some(changes),
            document_changes: None,
            change_annotations: None,
        }))
    }
}

impl Server {
    /// For a field: why not every place of it can be known now (see
    /// `field_places_unknown`). The places are looked up first.
    fn unknown_field_reads(&self, target: &Target) -> Option<String> {
        let Target::Field { record, name } = target else {
            return None;
        };
        // (`incomplete_for_rename` walked the workspace already; the
        // walk is kept until the next analysis.)
        self.field_places_unknown(record, *name)
    }
}

/// Whether `name` is one identifier to the lexer: what a rename may give
/// a name. `_`, which binds nothing, a keyword, and anything the lexer
/// reads as more or less than one name (`x9é`, `a b`, `1x`) are not.
pub fn is_valid_silt_ident(name: &str) -> bool {
    let Ok(lexed) = lexer::Lexer::new(crate::source::FileId::default(), name)
        .tokenize()
        .checked()
    else {
        return false;
    };
    let mut tokens = lexed.tokens.iter().map(|tok| &tok.kind);
    name != "_"
        && lexed.comments.is_empty()
        && matches!(
            (tokens.next(), tokens.next()),
            (Some(lexer::Token::Ident(sym)), Some(lexer::Token::Eof))
                if crate::intern::resolve(*sym) == name
        )
}
