//! `textDocument/rename` and `textDocument/prepareRename` handlers.
//!
//! Rename uses the workspace references index: every identifier
//! reference to the target symbol (plus its binding/definition site)
//! becomes a TextEdit in the response WorkspaceEdit.
//!
//! Safety guards:
//!   * prepareRename rejects cursors not sitting on a user-renameable
//!     identifier (builtins, keywords, non-ident positions).
//!   * Rename rejects when the new name is not a valid silt
//!     identifier (empty, starts with a digit, contains spaces).
//!
//! Limitations documented:
//!   * Only identifiers in open documents are edited. Files the editor
//!     has not opened will not be updated.
//!   * Shadowed bindings with the same name are not distinguished —
//!     every occurrence of the symbol is renamed. This is the correct
//!     behaviour for top-level renames; for inner-scope renames it
//!     over-reaches and the user must undo.

use std::collections::HashMap;
use std::sync::OnceLock;

use lsp_server::{ErrorCode, Response};
use lsp_types::{PrepareRenameResponse, Range, TextEdit, Uri, WorkspaceEdit};

use crate::intern::resolve as resolve_sym;
use crate::module;
use crate::types::builtins as builtin_types;

use super::Server;
use super::ast_walk::find_ident_at_offset_with_source;
use super::conversions::position_to_offset;

impl Server {
    /// `textDocument/prepareRename` — tell the client whether a rename
    /// can start at this cursor. Returns the identifier's range so the
    /// editor can prefill the input box.
    pub(super) fn prepare_rename(
        &self,
        params: lsp_types::TextDocumentPositionParams,
    ) -> Option<PrepareRenameResponse> {
        let uri = &params.text_document.uri;
        let pos = params.position;
        let doc = self.documents.get(uri)?;
        let program = doc.program.as_ref()?;

        let cursor = position_to_offset(&doc.source, &pos);
        let name = find_ident_at_offset_with_source(program, cursor, Some(&doc.source))?;
        let name_str = resolve_sym(name);

        if !is_user_renameable(&name_str) {
            return None;
        }

        // Hand back the identifier's range so the client can preselect
        // the old name in its rename input box. Approximate by using
        // the workspace references list — the definition site is the
        // authoritative range when available.
        let range = self
            .workspace_find_references(name, true)
            .into_iter()
            .find(|loc| loc.uri == *uri)
            .map(|loc| loc.range)
            .unwrap_or(Range {
                start: pos,
                end: pos,
            });
        Some(PrepareRenameResponse::Range(range))
    }

    /// `textDocument/rename` — build a WorkspaceEdit covering every
    /// reference to the target symbol across open documents.
    pub(super) fn rename(
        &self,
        params: lsp_types::RenameParams,
        request_id: lsp_server::RequestId,
    ) -> Result<Option<WorkspaceEdit>, Response> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let new_name = params.new_name.clone();

        if !is_valid_silt_ident(&new_name) {
            return Err(Response::new_err(
                request_id,
                ErrorCode::InvalidParams as i32,
                format!("`{new_name}` is not a valid silt identifier"),
            ));
        }

        let Some(doc) = self.documents.get(uri) else {
            return Ok(None);
        };
        let Some(program) = &doc.program else {
            return Ok(None);
        };
        let cursor = position_to_offset(&doc.source, &pos);
        let Some(name) = find_ident_at_offset_with_source(program, cursor, Some(&doc.source))
        else {
            return Ok(None);
        };
        let name_str = resolve_sym(name);
        if !is_user_renameable(&name_str) {
            return Err(Response::new_err(
                request_id,
                ErrorCode::InvalidParams as i32,
                format!("`{name_str}` is a builtin and cannot be renamed"),
            ));
        }

        // Aggregate every reference (including the definition) into a
        // per-URI edit list.
        let locations = self.workspace_find_references(name, true);
        let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();
        for loc in locations {
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

/// A user-renameable identifier: not a silt keyword, not a builtin
/// module, not a reserved name. Builtin constructor variants (every
/// name yielded by `module::all_builtin_constructor_names` — `Ok`,
/// `Err`, `Some`, `None`, plus every gated variant from `io`, `json`,
/// `http`, `channel`, `postgres`, `time`, etc.) are stdlib-defined
/// and also rejected.
///
/// `pub` so integration tests (see `tests/builtin_constructor_parity_tests.rs`)
/// can assert every gated constructor is protected from rename.
pub fn is_user_renameable(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    if SILT_KEYWORDS.contains(&name) {
        return false;
    }
    if module::is_builtin_module(name) {
        return false;
    }
    if module::all_builtin_constructor_names().any(|c| c == name) {
        return false;
    }
    if builtin_globals().contains(&name) {
        return false;
    }
    true
}

/// Basic identifier shape check. Matches silt's lexer: starts with a
/// letter or `_`, followed by any mix of alphanumerics and `_`.
fn is_valid_silt_ident(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_alphabetic() && first != '_' {
        return false;
    }
    for c in chars {
        if !c.is_alphanumeric() && c != '_' {
            return false;
        }
    }
    !SILT_KEYWORDS.contains(&name)
}

const SILT_KEYWORDS: &[&str] = &[
    "as", "else", "fn", "import", "let", "loop", "match", "mod", "pub", "return", "trait", "type",
    "when", "where", "true", "false",
];

// Builtin constructor rejection consults `module::all_builtin_constructor_names`
// so new gated variants (e.g. `IoNotFound`, `PgConnect`, `Recv`/`Send`) are
// picked up automatically. Parity-lock test in
// `tests/builtin_constructor_parity_tests.rs` guards the coupling.

/// Built-in print/panic free functions that user code cannot rename.
/// Type names (`Int`, `List`, `Map`, ...) are sourced separately from
/// the authoritative table at `crate::types::builtins`, so adding a
/// new built-in type does not require touching this file.
const BUILTIN_FUNCTIONS: &[&str] = &["println", "print", "panic"];

/// Combined list of every reserved global identifier — built-in
/// functions plus every name in [`builtin_types::BUILTIN_TYPES`].
/// Computed once on first access via [`OnceLock`]; the `&[&str]`
/// surface mirrors the previous hand-rolled constant so existing
/// callers keep working unchanged. Type-name entries are derived
/// from `crate::types::builtins::iter_all()` so additions to that
/// authoritative table propagate here automatically.
pub(crate) fn builtin_globals() -> &'static [&'static str] {
    static GLOBALS: OnceLock<Vec<&'static str>> = OnceLock::new();
    GLOBALS
        .get_or_init(|| {
            let mut v: Vec<&'static str> = BUILTIN_FUNCTIONS.to_vec();
            v.extend(builtin_types::iter_all().map(|b| b.name));
            v
        })
        .as_slice()
}
