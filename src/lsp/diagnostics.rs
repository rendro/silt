//! Diagnostics publishing and document (re)analysis.
//!
//! `update_document` drives the lexer → parser → typechecker pipeline,
//! converts errors to LSP `Diagnostic` values, and re-publishes them for
//! the client.

use std::collections::HashMap;

use lsp_server::Message;
use lsp_types::notification::{Notification as _, PublishDiagnostics};
use lsp_types::{Diagnostic, DiagnosticSeverity, PublishDiagnosticsParams, Uri};

use crate::lexer::{Lexer, Span};
use crate::parser::Parser;
use crate::typechecker;

use super::Server;
use super::conversions::span_to_range;
use super::definitions::build_definitions;
use super::local_bindings::collect_local_bindings;
use super::state::Document;

// ── Diagnostics helper ─────────────────────────────────────────────

pub(super) fn make_diagnostic(
    message: &str,
    span: &Span,
    severity: DiagnosticSeverity,
    source: &str,
) -> Diagnostic {
    Diagnostic {
        range: span_to_range(span, source),
        severity: Some(severity),
        message: message.to_string(),
        ..Diagnostic::default()
    }
}

impl Server {
    // ── Document analysis ──────────────────────────────────────────

    pub(super) fn update_document(&mut self, uri: Uri, source: String) {
        let mut diagnostics = Vec::new();

        let tokens = match Lexer::new(&source).tokenize() {
            Ok(t) => t,
            Err(e) => {
                diagnostics.push(make_diagnostic(
                    &e.message,
                    &e.span,
                    DiagnosticSeverity::ERROR,
                    &source,
                ));
                self.documents.insert(
                    uri.clone(),
                    Document {
                        source,
                        program: None,
                        definitions: HashMap::new(),
                        locals: Vec::new(),
                    },
                );
                self.diagnostics_cache
                    .insert(uri.clone(), diagnostics.clone());
                self.publish_diagnostics(uri, diagnostics);
                return;
            }
        };

        let (mut program, parse_errors) =
            Parser::new_with_source(tokens, &source).parse_program_recovering();

        for e in &parse_errors {
            diagnostics.push(make_diagnostic(
                &e.message,
                &e.span,
                DiagnosticSeverity::ERROR,
                &source,
            ));
        }

        let type_errors = typechecker::check(&mut program);
        // GAP #8: drop the "unknown module" warning for user-module imports
        // and the follow-on "undefined" errors for names they bring in. The
        // type checker has no filesystem access, so every legitimate
        // `import <user_module>` would otherwise surface as a warning in the
        // editor, plus noise for every imported name. The compiler resolves
        // those at link time — if the name truly is missing a hard error
        // will surface there — so we suppress them here the same way the
        // CLI does.
        let has_user_import_warning = type_errors
            .iter()
            .any(is_unknown_module_warning_te);
        for e in &type_errors {
            if is_unknown_module_warning_te(e) {
                continue;
            }
            if has_user_import_warning && is_user_import_resolvable_error_te(e) {
                continue;
            }
            let severity = match e.severity {
                typechecker::Severity::Error => DiagnosticSeverity::ERROR,
                typechecker::Severity::Warning => DiagnosticSeverity::WARNING,
            };
            diagnostics.push(make_diagnostic(&e.message, &e.span, severity, &source));
        }

        let definitions = build_definitions(&program);
        let locals = collect_local_bindings(&program, &source);

        self.documents.insert(
            uri.clone(),
            Document {
                source,
                program: Some(program),
                definitions,
                locals,
            },
        );

        // Cache diagnostics for the pull-model handler
        // (`textDocument/diagnostic`). We store a clone before
        // publishing so the cache and the push always match.
        self.diagnostics_cache
            .insert(uri.clone(), diagnostics.clone());

        self.publish_diagnostics(uri, diagnostics);
    }

    pub(super) fn publish_diagnostics(&self, uri: Uri, diagnostics: Vec<Diagnostic>) {
        let params = PublishDiagnosticsParams::new(uri, diagnostics, None);
        let notif = lsp_server::Notification::new(PublishDiagnostics::METHOD.to_string(), params);
        self.connection
            .sender
            .send(Message::Notification(notif))
            .ok();
    }
}

// mirrors is_user_import_resolvable_error in src/cli/pipeline.rs.
// The CLI helper operates on SourceError (post-wrapping); this version
// operates on the typechecker's native TypeError so the LSP can filter
// before converting to lsp_types::Diagnostic. Keep the two in sync; a
// future LATENT dedupe round can lift them into a shared module.
fn is_unknown_module_warning_te(err: &typechecker::TypeError) -> bool {
    err.severity == typechecker::Severity::Warning && err.message.contains("unknown module")
}

// mirrors is_user_import_resolvable_error in src/cli/pipeline.rs.
// Deliberately omits a `starts_with("type ")` clause: the CLI filter
// dropped that pattern because it swallowed real type-mismatch errors
// alongside user-module follow-ons (GAP #7). Trait-impl cascades flow
// through the narrow `"does not implement"` substring instead.
fn is_user_import_resolvable_error_te(err: &typechecker::TypeError) -> bool {
    err.severity == typechecker::Severity::Error
        && (err.message.starts_with("undefined variable")
            || err.message.starts_with("undefined constructor")
            || err.message.starts_with("undefined type")
            || err.message.starts_with("unknown field")
            || err.message.contains("does not implement"))
}
