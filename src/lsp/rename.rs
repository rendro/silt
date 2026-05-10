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

use crate::intern::{Symbol, resolve as resolve_sym};
use crate::lexer;
use crate::module;
use crate::types::builtins as builtin_types;

use super::Server;
use super::ast_walk::find_ident_at_offset_with_source;
use super::conversions::position_to_offset;
use super::local_bindings::{find_local_binding_at_offset, nearest_local_binding_for};
use super::state::Document;

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

        // Scope-aware gate: a symbol is renameable when it resolves to a
        // user-introduced binding visible at the cursor, even if the
        // lexeme matches a builtin name (silt allows shadowing builtins
        // via let / fn / type / trait declarations). Only when no user
        // binding is in scope do we fall back to the name-based
        // `is_user_renameable` filter (rejects builtins, keywords, etc.).
        if !is_symbol_user_renameable_at_cursor(doc, name, &name_str, cursor) {
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
        // Scope-aware gate: see `prepare_rename` for the rationale. A
        // user-shadowed builtin name (e.g. `let Int = 42` followed by a
        // use of `Int`) must be renameable; a use-site of an actual
        // builtin (no shadowing in scope) must still be rejected.
        if !is_symbol_user_renameable_at_cursor(doc, name, &name_str, cursor) {
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

/// Scope-aware rename gate.
///
/// Returns true when the symbol at `cursor` (with display name `name_str`)
/// resolves to a user-introduced binding — either:
///   * the cursor sits ON a local binding identifier (fn param, `let`
///     binder inside a fn body, when-binder, …), OR
///   * a local binding for `name` is visible at this cursor (use-site of
///     a local), OR
///   * a top-level definition for `name` exists in this document
///     (Decl::Let, Decl::Fn, Decl::Type, Decl::Trait, …).
///
/// In all three cases the name maps to a user binding even if the
/// lexeme also names a builtin — silt explicitly allows shadowing
/// builtins via `let`/`fn`/`type`/`trait`, so refusing rename based on
/// the lexeme alone is wrong (DX-G1).
///
/// When none of those hold, the cursor is on a use-site of a name with
/// no user binding in scope — i.e. a real builtin reference. We then
/// fall back to [`is_user_renameable`], the historical name-based
/// filter that rejects builtin globals, builtin module names, builtin
/// constructors, and silt keywords.
fn is_symbol_user_renameable_at_cursor(
    doc: &Document,
    name: Symbol,
    name_str: &str,
    cursor: usize,
) -> bool {
    // Always reject silt keywords / empty names regardless of scope —
    // those can never be renamed (parser would reject the result).
    if name_str.is_empty() || is_silt_keyword(name_str) {
        return false;
    }

    // 1. Cursor is on a local binding identifier itself.
    if find_local_binding_at_offset(&doc.locals, cursor).is_some() {
        return true;
    }
    // 2. A local binding for this name is visible at the cursor.
    if nearest_local_binding_for(&doc.locals, name, cursor).is_some() {
        return true;
    }
    // 3. The symbol is a user-defined top-level binding in this doc
    //    (Decl::Let / Decl::Fn / Decl::Type / Decl::Trait register here
    //    via `build_definitions`). Top-level user shadowing of a builtin
    //    name (`let Int = 42` at module scope) lands in this branch.
    if doc.definitions.contains_key(&name) {
        return true;
    }

    // 4. No user binding for this name in scope — the cursor is on a
    //    use-site of a builtin module / global / constructor. Defer to
    //    the historical name-based filter so keyword/constructor
    //    rejection still fires.
    is_user_renameable(name_str)
}

/// A user-renameable identifier: not a silt keyword, not a builtin
/// module, not a reserved name. Builtin constructor variants (every
/// name yielded by `module::all_builtin_constructor_names` — `Ok`,
/// `Err`, `Some`, `None`, plus every gated variant from `io`, `json`,
/// `http`, `channel`, `postgres`, `time`, etc.) are stdlib-defined
/// and also rejected.
///
/// This is a name-only check; it intentionally has no notion of
/// "is the cursor on a binding or a use-site?". Callers that need
/// scope-aware behaviour should consult
/// [`is_symbol_user_renameable_at_cursor`] instead — that helper falls
/// back to this one only when the symbol has no user binding in scope.
///
/// `pub` so integration tests (see `tests/builtin_constructor_parity_tests.rs`)
/// can assert every gated constructor is protected from rename.
pub fn is_user_renameable(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    if is_silt_keyword(name) {
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
    !is_silt_keyword(name)
}

/// Reject every reserved word the lexer recognizes — both keyword-shaped
/// tokens (`KEYWORDS`) and reserved-word-shaped boolean literals
/// (`KEYWORD_LITERALS`). Sourced from `crate::lexer` so a future keyword
/// addition flows through automatically; guarded by
/// `tests/lexer_keyword_parity_tests.rs`.
fn is_silt_keyword(name: &str) -> bool {
    lexer::KEYWORDS.contains(&name) || lexer::KEYWORD_LITERALS.contains(&name)
}

// Builtin constructor rejection consults `module::all_builtin_constructor_names`
// so new gated variants (e.g. `IoNotFound`, `PgConnect`, `Recv`/`Send`) are
// picked up automatically. Parity-lock test in
// `tests/builtin_constructor_parity_tests.rs` guards the coupling.

/// Combined list of every reserved global identifier — built-in
/// free functions plus every name in [`builtin_types::BUILTIN_TYPES`].
/// Computed once on first access via [`OnceLock`]; the `&[&str]`
/// surface mirrors the previous hand-rolled constant so existing
/// callers keep working unchanged.
///
/// Free-function entries (`print`/`println`/`panic`) come from
/// `module::builtin_free_function_names()` so adding a new global
/// free function in the typechecker registers everywhere. Type-name
/// entries are derived from `crate::types::builtins::iter_all()` so
/// additions to that authoritative table propagate here automatically.
/// Parity lock: `tests/builtin_free_function_parity_tests.rs`.
pub(crate) fn builtin_globals() -> &'static [&'static str] {
    static GLOBALS: OnceLock<Vec<&'static str>> = OnceLock::new();
    GLOBALS
        .get_or_init(|| {
            let mut v: Vec<&'static str> = module::builtin_free_function_names().to_vec();
            v.extend(builtin_types::iter_all().map(|b| b.name));
            v
        })
        .as_slice()
}
