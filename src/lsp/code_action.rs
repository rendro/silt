//! `textDocument/codeAction`: the quick fixes of the diagnostics the
//! client hands back.
//!
//! Every quick fix comes from the diagnostic itself: silt attaches its
//! fixes (`Diagnostic::fixes`) when it reports the problem, and the LSP
//! diagnostic carries them in `data` (see `diagnostic::to_lsp`). This
//! handler only turns them into code actions; it never reads a message.

use std::collections::HashMap;

use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams, CodeActionResponse,
    Diagnostic, Range, TextEdit, WorkspaceEdit,
};
use serde::Deserialize;

use super::Server;

/// One quick fix as `diagnostic::to_lsp` stores it in `data`.
#[derive(Deserialize)]
struct StoredFix {
    title: String,
    edits: Vec<StoredEdit>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredEdit {
    range: Range,
    new_text: String,
}

/// The quick fixes stored in `diag`.
fn stored_fixes(diag: &Diagnostic) -> Vec<StoredFix> {
    diag.data
        .clone()
        .and_then(|data| serde_json::from_value(data).ok())
        .unwrap_or_default()
}

impl Server {
    pub(super) fn code_action(&self, params: CodeActionParams) -> Option<CodeActionResponse> {
        let uri = params.text_document.uri.clone();
        self.documents.get(&uri)?;
        let mut out: Vec<CodeActionOrCommand> = Vec::new();
        for diag in &params.context.diagnostics {
            for fix in stored_fixes(diag) {
                let edits: Vec<TextEdit> = fix
                    .edits
                    .into_iter()
                    .map(|e| TextEdit {
                        range: e.range,
                        new_text: e.new_text,
                    })
                    .collect();
                if edits.is_empty() {
                    continue;
                }
                let mut changes = HashMap::new();
                changes.insert(uri.clone(), edits);
                out.push(CodeActionOrCommand::CodeAction(CodeAction {
                    title: fix.title,
                    kind: Some(CodeActionKind::QUICKFIX),
                    diagnostics: Some(vec![diag.clone()]),
                    edit: Some(WorkspaceEdit {
                        changes: Some(changes),
                        document_changes: None,
                        change_annotations: None,
                    }),
                    command: None,
                    is_preferred: Some(true),
                    disabled: None,
                    data: None,
                }));
            }
        }
        Some(out)
    }
}
