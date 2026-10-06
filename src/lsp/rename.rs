//! `textDocument/rename` and `textDocument/prepareRename` handlers.
//!
//! A rename edits every place that names what the name under the cursor
//! names (see `workspace.rs`): the declaration, each use, each item of
//! an `import m.{ item }`. A function of another module with the same
//! name, or a local that shadows it, is something else and is left as
//! it is.
//!
//! What cannot be renamed: one of silt's own definitions (`Some`,
//! `list.map`, `Int`), and anything the resolver gave no meaning (a
//! field, a method, a keyword). The new name must be an identifier, and
//! of the same kind as the old one: a type, a variant or a trait starts
//! with an upper-case letter, anything else does not.
//!
//! The files edited are the open documents, the modules they import,
//! and the workspace files that import the definition's module.

use std::collections::HashMap;

use lsp_server::{ErrorCode, Response};
use lsp_types::{PrepareRenameResponse, TextEdit, Uri, WorkspaceEdit};

use crate::lexer;

use super::Server;
use super::conversions::{position_to_offset, span_to_range};

impl Server {
    /// `textDocument/prepareRename`: the range of the name under the
    /// cursor, if it can be renamed.
    pub(super) fn prepare_rename(
        &self,
        params: lsp_types::TextDocumentPositionParams,
    ) -> Option<PrepareRenameResponse> {
        let uri = &params.text_document.uri;
        let doc = self.documents.get(uri)?;
        let cursor = position_to_offset(&doc.source, &params.position);
        let (span, target) = self.target_at(uri, cursor)?;
        target
            .is_renameable()
            .then(|| PrepareRenameResponse::Range(span_to_range(&span, &doc.source)))
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
        let old_name = &doc.source.text[span.start as usize..span.end as usize];
        if !target.is_renameable() {
            return Err(invalid(format!(
                "`{old_name}` is a builtin and cannot be renamed"
            )));
        }
        // An upper-case first letter is what makes a name a type, a
        // variant or a trait.
        let upper = |name: &str| name.starts_with(|c: char| c.is_ascii_uppercase());
        if upper(old_name) != upper(&new_name) {
            let kind = if upper(old_name) {
                "starts with an upper-case letter, as the name of a type, a variant or a trait does"
            } else {
                "does not start with an upper-case letter, which only the name of a type, a \
                 variant or a trait does"
            };
            return Err(invalid(format!(
                "`{old_name}` {kind}: `{new_name}` would be a name of another kind"
            )));
        }

        let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();
        for loc in self.references_to(uri, &target, true) {
            changes.entry(loc.uri).or_default().push(TextEdit {
                range: loc.range,
                new_text: new_name.clone(),
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

/// Basic identifier shape check. Matches silt's lexer: starts with an
/// ASCII letter or `_`, followed by any mix of alphanumerics and `_`.
///
/// The lexer's identifier-start set is ASCII-only (`'a'..='z' | 'A'..='Z'
/// | '_'` at `lexer.rs`), so the first-char check must use
/// `is_ascii_alphabetic` — `char::is_alphabetic` would accept Unicode
/// letters (`é`, `名`, …) that the lexer rejects, letting rename rewrite
/// source into something that no longer lexes on the next `silt run`.
pub fn is_valid_silt_ident(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphabetic() && first != '_' {
        return false;
    }
    for c in chars {
        if !c.is_alphanumeric() && c != '_' {
            return false;
        }
    }
    !is_silt_keyword(name)
}

/// Reject every reserved word the lexer recognizes — both keyword-shaped
/// tokens (`KEYWORDS`) and reserved-word-shaped boolean literals
/// (`KEYWORD_LITERALS`). Sourced from `crate::lexer` so a future keyword
/// addition flows through automatically; guarded by
/// `tests/meta/lexer_keyword_parity_tests.rs`.
fn is_silt_keyword(name: &str) -> bool {
    lexer::KEYWORDS.contains(&name) || lexer::KEYWORD_LITERALS.contains(&name)
}
