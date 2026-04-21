//! `textDocument/codeAction` handler and pluggable quick-fix catalog.
//!
//! The handler walks each diagnostic the client sent in
//! `params.context.diagnostics`, runs every registered [`QuickFix`]'s matcher
//! against it, and collects one `CodeAction` per successful match. Each
//! quick-fix produces a list of [`TextEdit`]s which we wrap into a
//! [`WorkspaceEdit`] targeting the document that owns the diagnostic.
//!
//! ── Starter catalog ─────────────────────────────────────────────────
//!
//! Three diagnostic-driven quick-fixes ship with the initial framework:
//!
//! 1. **Add import for `{module}`** — triggered by the compiler diagnostic
//!    `module 'X' is not imported; add \`import X\` at the top of the file`.
//!    Inserts `import X\n` at line 0. We keep the insertion trivial; smarter
//!    placement (after shebang/license blocks) is a follow-up once silt
//!    actually supports those.
//!
//! 2. **Change `(a -> b)` to `Fn(a) -> b`** — triggered by the parse error
//!    `expected identifier, found ->`, which fires on the old arrow
//!    function-type syntax. We narrow the trigger further by peeking at the
//!    source around the diagnostic range: the surrounding parens + the
//!    inline `->` must actually be present, otherwise we skip (the parse
//!    error can come from other misuses that wouldn't be fixed by the
//!    rewrite). Because silt's error span points at `->`, we expand it
//!    outward to cover the enclosing `(..)` before emitting the edit.
//!
//! 3. **Wrap expression in `Ok(...)`** — triggered when typechecker emits
//!    `type mismatch: expected Result(a, e), got X`. Silt has no `if/else`,
//!    so the spec's 3rd candidate ("Convert if..else to match") doesn't
//!    apply; this Result-wrap is the idiomatic replacement.

use std::collections::HashMap;

use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams, CodeActionResponse,
    Diagnostic, Position, Range, TextEdit, WorkspaceEdit,
};

use super::Server;
use super::conversions::position_to_offset;
use super::state::Document;

// ── QuickFix trait & catalog ─────────────────────────────────────────

/// One diagnostic-or-range → edit mapping.
pub(super) trait QuickFix {
    fn title(&self) -> &str;
    fn diagnostic_matcher(&self, diag: &Diagnostic) -> bool;
    fn build_edits(
        &self,
        doc: &Document,
        params: &CodeActionParams,
        diag: &Diagnostic,
    ) -> Option<Vec<TextEdit>>;
}

fn quickfixes() -> Vec<Box<dyn QuickFix>> {
    vec![
        Box::new(AddImport),
        Box::new(FixArrowFnType),
        Box::new(WrapInOk),
    ]
}

// ── Handler ──────────────────────────────────────────────────────────

impl Server {
    pub(super) fn code_action(&self, params: CodeActionParams) -> Option<CodeActionResponse> {
        let uri = params.text_document.uri.clone();
        let doc = self.documents.get(&uri)?;
        let fixes = quickfixes();
        let mut out: Vec<CodeActionOrCommand> = Vec::new();

        for diag in &params.context.diagnostics {
            for qf in &fixes {
                if !qf.diagnostic_matcher(diag) {
                    continue;
                }
                let Some(edits) = qf.build_edits(doc, &params, diag) else {
                    continue;
                };
                if edits.is_empty() {
                    continue;
                }
                let mut changes = HashMap::new();
                changes.insert(uri.clone(), edits);
                out.push(CodeActionOrCommand::CodeAction(CodeAction {
                    title: {
                        // Allow a fix to materialise a data-dependent title
                        // (e.g. include the module name) by overriding via
                        // `.title()` — but every starter fix yields a title
                        // that already reads naturally as a sentence.
                        let title = qf.title();
                        // Add-import parameterises on the module name.
                        if title == AddImport.title()
                            && let Some(module) = import_module_from_message(&diag.message)
                        {
                            format!("Add import for `{module}`")
                        } else {
                            title.to_string()
                        }
                    },
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

// ── Fix #1: Add import for `{module}` ────────────────────────────────

struct AddImport;

impl QuickFix for AddImport {
    fn title(&self) -> &str {
        // Replaced with a module-specific title in the handler.
        "Add import"
    }

    fn diagnostic_matcher(&self, diag: &Diagnostic) -> bool {
        import_module_from_message(&diag.message).is_some()
    }

    fn build_edits(
        &self,
        doc: &Document,
        _params: &CodeActionParams,
        diag: &Diagnostic,
    ) -> Option<Vec<TextEdit>> {
        let module = import_module_from_message(&diag.message)?;
        // Don't duplicate an existing import.
        let needle = format!("import {module}");
        if doc
            .source
            .lines()
            .any(|line| line.trim() == needle || line.trim_start().starts_with(&(needle.clone() + " ")))
        {
            return None;
        }
        // Simplest safe placement: line 0, column 0.
        let at = Position::new(0, 0);
        Some(vec![TextEdit {
            range: Range::new(at, at),
            new_text: format!("import {module}\n"),
        }])
    }
}

/// Extract the module name X from the compiler's
/// `module 'X' is not imported; add \`import X\` at the top of the file`
/// diagnostic message. Returns None if the message doesn't match.
fn import_module_from_message(msg: &str) -> Option<String> {
    // Be lenient about punctuation — we want to match both single quotes and
    // backticks — but insist on the two anchor phrases so we don't match
    // arbitrary diagnostics that happen to contain "import".
    if !msg.contains("is not imported") {
        return None;
    }
    // Prefer the name inside the first pair of single quotes.
    let start = msg.find('\'')?;
    let rest = &msg[start + 1..];
    let end = rest.find('\'')?;
    let name = rest[..end].trim();
    if name.is_empty() {
        return None;
    }
    Some(name.to_string())
}

// ── Fix #2: Change `(a -> b)` to `Fn(a) -> b` ────────────────────────

struct FixArrowFnType;

impl QuickFix for FixArrowFnType {
    fn title(&self) -> &str {
        "Change `(a -> b)` to `Fn(a) -> b`"
    }

    fn diagnostic_matcher(&self, diag: &Diagnostic) -> bool {
        diag.message.contains("expected identifier, found ->")
    }

    fn build_edits(
        &self,
        doc: &Document,
        _params: &CodeActionParams,
        diag: &Diagnostic,
    ) -> Option<Vec<TextEdit>> {
        // Expand the diagnostic range outward to the enclosing `(` and `)`
        // on the same line, verifying we really see a `(A -> B)` shape.
        let src = &doc.source;
        let arrow_off = position_to_offset(src, &diag.range.start);
        let bytes = src.as_bytes();
        if arrow_off >= bytes.len() {
            return None;
        }

        // Find the most recent `(` before the arrow on the same line.
        let line_start = src[..arrow_off].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let open = src[line_start..arrow_off].rfind('(')? + line_start;

        // Find the matching `)` after the arrow on the same line (simple
        // same-line balance; nested parens inside A or B would need a real
        // parser — if we hit one, bail).
        let after_arrow = arrow_off + 2; // skip past "->"
        let line_end = src[after_arrow..]
            .find('\n')
            .map(|i| after_arrow + i)
            .unwrap_or(bytes.len());
        let segment = &src[after_arrow..line_end];
        // Bail if we'd cross a nested `(` before our closing `)`.
        let close_rel = segment.find(')')?;
        if segment[..close_rel].contains('(') {
            return None;
        }
        let close = after_arrow + close_rel;

        let a = src[open + 1..arrow_off].trim();
        let b = src[arrow_off + 2..close].trim();
        if a.is_empty() || b.is_empty() {
            return None;
        }

        let replacement = format!("Fn({a}) -> {b}");
        let range = Range::new(
            offset_to_position(src, open),
            offset_to_position(src, close + 1),
        );
        Some(vec![TextEdit {
            range,
            new_text: replacement,
        }])
    }
}

// ── Fix #3: Wrap expression in `Ok(...)` ─────────────────────────────

struct WrapInOk;

impl QuickFix for WrapInOk {
    fn title(&self) -> &str {
        "Wrap expression in `Ok(...)`"
    }

    fn diagnostic_matcher(&self, diag: &Diagnostic) -> bool {
        let m = &diag.message;
        m.contains("type mismatch")
            && m.contains("expected Result")
            && !m.contains("got Result")
    }

    fn build_edits(
        &self,
        doc: &Document,
        _params: &CodeActionParams,
        diag: &Diagnostic,
    ) -> Option<Vec<TextEdit>> {
        let src = &doc.source;
        let start = position_to_offset(src, &diag.range.start);
        let end = position_to_offset(src, &diag.range.end);
        if end <= start || end > src.len() {
            return None;
        }
        let snippet = src.get(start..end)?.trim();
        if snippet.is_empty() {
            return None;
        }
        let new_text = format!("Ok({snippet})");
        Some(vec![TextEdit {
            range: diag.range,
            new_text,
        }])
    }
}

// ── Helpers ──────────────────────────────────────────────────────────

/// Byte offset → LSP `Position` (UTF-16). Only used within this module;
/// mirrors the direction `position_to_offset` handles, but the reverse.
fn offset_to_position(src: &str, offset: usize) -> Position {
    let offset = offset.min(src.len());
    let line_start = src[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line = src[..line_start].bytes().filter(|b| *b == b'\n').count() as u32;
    let character: u32 = src[line_start..offset]
        .chars()
        .map(|c| c.len_utf16() as u32)
        .sum();
    Position::new(line, character)
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_module_parser_recognises_compiler_message() {
        let m = "module 'list' is not imported; add `import list` at the top of the file";
        assert_eq!(import_module_from_message(m).as_deref(), Some("list"));
    }

    #[test]
    fn import_module_parser_rejects_unrelated() {
        assert!(import_module_from_message("type mismatch").is_none());
        assert!(import_module_from_message("'foo' something else").is_none());
    }

    #[test]
    fn offset_to_position_handles_multiline() {
        let src = "ab\ncde\nfg";
        assert_eq!(offset_to_position(src, 0), Position::new(0, 0));
        assert_eq!(offset_to_position(src, 3), Position::new(1, 0));
        assert_eq!(offset_to_position(src, 5), Position::new(1, 2));
        assert_eq!(offset_to_position(src, 7), Position::new(2, 0));
    }
}
