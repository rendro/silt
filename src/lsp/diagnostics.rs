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
                self.publish_diagnostics(uri, diagnostics);
                return;
            }
        };

        let (mut program, parse_errors) = Parser::new(tokens).parse_program_recovering();

        for e in &parse_errors {
            diagnostics.push(make_diagnostic(
                &e.message,
                &e.span,
                DiagnosticSeverity::ERROR,
                &source,
            ));
        }

        let type_errors = typechecker::check(&mut program);
        for e in &type_errors {
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
