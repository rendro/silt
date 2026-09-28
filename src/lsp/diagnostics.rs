//! Diagnostics publishing and document (re)analysis.
//!
//! `update_document` drives the lexer → parser → typechecker pipeline,
//! converts errors to LSP `Diagnostic` values, and re-publishes them for
//! the client.

use std::collections::HashMap;
use std::panic::{self, AssertUnwindSafe};

use lsp_server::Message;
use lsp_types::notification::{Notification as _, PublishDiagnostics};
use lsp_types::{Diagnostic, DiagnosticSeverity, Position, PublishDiagnosticsParams, Range, Uri};

use crate::ast::Program;
use crate::intern::Symbol;
use crate::lexer::{Lexer, Span};
use crate::parser::Parser;
use crate::typechecker;

use super::Server;
use super::conversions::span_to_range;
use super::definitions::build_definitions;
use super::local_bindings::collect_local_bindings;
use super::panic_message;
use super::state::{DefInfo, Document, LocalBinding};

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

// ── Document analysis ──────────────────────────────────────────────

/// What one analysis pass over a document yields.
struct Analysis {
    /// `None` when the text does not lex.
    program: Option<Program>,
    definitions: HashMap<Symbol, DefInfo>,
    locals: Vec<LocalBinding>,
    diagnostics: Vec<Diagnostic>,
}

impl Analysis {
    /// The result for a text that yields no program: every feature that
    /// needs the syntax tree is off for the document until its next edit.
    fn without_program(diagnostics: Vec<Diagnostic>) -> Self {
        Analysis {
            program: None,
            definitions: HashMap::new(),
            locals: Vec::new(),
            diagnostics,
        }
    }
}

/// The one diagnostic published for a document whose analysis panicked.
fn analysis_failed_diagnostic() -> Diagnostic {
    Diagnostic {
        range: Range::new(Position::new(0, 0), Position::new(0, 1)),
        severity: Some(DiagnosticSeverity::ERROR),
        message: "internal error: silt could not analyse this file. Diagnostics, hover and \
                  navigation are unavailable for it until the next edit. Please report this \
                  as a bug."
            .to_string(),
        ..Diagnostic::default()
    }
}

/// Lex, parse and typecheck `source`. Reads nothing but its arguments.
fn analyse(source: &str, strict_effects: bool) -> Analysis {
    let mut diagnostics = Vec::new();

    let tokens = match Lexer::new(source).tokenize() {
        Ok(t) => t,
        Err(e) => {
            diagnostics.push(make_diagnostic(
                &e.message,
                &e.span,
                DiagnosticSeverity::ERROR,
                source,
            ));
            return Analysis::without_program(diagnostics);
        }
    };

    let (mut program, parse_errors) =
        Parser::new_with_source(tokens, source).parse_program_recovering();

    for e in &parse_errors {
        diagnostics.push(make_diagnostic(
            &e.message,
            &e.span,
            DiagnosticSeverity::ERROR,
            source,
        ));
    }

    // Phase D: when the workspace's manifest enables strict-effects
    // mode (`[lints] strict-effects = true`), surface the
    // strict-mode diagnostics in the editor too. The flag was
    // captured at workspace preload time in `lsp::run`.
    let type_errors = if strict_effects {
        // No package context for an LSP-pull typecheck: the
        // server doesn't have the producing-package symbol on
        // hand for ad-hoc typechecks during editing. We also
        // don't have cached cross-module exports plumbed through
        // here yet — that's a follow-up. For now, route through
        // the options entry so the strict flag flows; package=None
        // matches the legacy `typechecker::check()` behaviour.
        let (errs, _exports) = typechecker::check_with_package_and_imports_options(
            &mut program,
            None,
            std::collections::HashMap::new(),
            true,
        );
        errs
    } else {
        typechecker::check(&mut program)
    };
    // GAP #8: drop the "unknown module" warning for user-module imports
    // and the follow-on "undefined" errors for names they bring in. The
    // type checker has no filesystem access, so every legitimate
    // `import <user_module>` would otherwise surface as a warning in the
    // editor, plus noise for every imported name. The compiler resolves
    // those at link time — if the name truly is missing a hard error
    // will surface there — so we suppress them here the same way the
    // CLI does.
    let has_user_import_warning = type_errors.iter().any(is_unknown_module_warning_te);
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
        diagnostics.push(make_diagnostic(&e.message, &e.span, severity, source));
    }

    let definitions = build_definitions(&program);
    let locals = collect_local_bindings(&program, source);

    Analysis {
        program: Some(program),
        definitions,
        locals,
        diagnostics,
    }
}

impl Server {
    pub(super) fn update_document(&mut self, uri: Uri, source: String) {
        self.update_document_with(uri, source, analyse);
    }

    /// Store `source` as the text of `uri`, together with what `analyse`
    /// makes of it, and publish the diagnostics.
    ///
    /// A panic inside `analyse` is caught and logged to stderr. The new
    /// text is stored all the same, without analysis results: the client
    /// has applied the edit already, so answering its next requests from
    /// the previous text would hand it positions and edits for a document
    /// it no longer has. `analyse` reads nothing but its arguments, so the
    /// panic cannot leave the server half-updated.
    fn update_document_with(
        &mut self,
        uri: Uri,
        source: String,
        analyse: fn(&str, bool) -> Analysis,
    ) {
        let strict_effects = self.strict_effects;
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| analyse(&source, strict_effects)));
        let analysis = outcome.unwrap_or_else(|payload| {
            eprintln!(
                "silt-lsp: internal error while analysing {}: {}; the document is kept \
                 without analysis results and the server keeps running",
                uri.as_str(),
                panic_message(payload)
            );
            Analysis::without_program(vec![analysis_failed_diagnostic()])
        });

        self.documents.insert(
            uri.clone(),
            Document {
                source,
                program: analysis.program,
                definitions: analysis.definitions,
                locals: analysis.locals,
            },
        );

        // Cache diagnostics for the pull-model handler
        // (`textDocument/diagnostic`). We store a clone before
        // publishing so the cache and the push always match.
        self.diagnostics_cache
            .insert(uri.clone(), analysis.diagnostics.clone());

        self.publish_diagnostics(uri, analysis.diagnostics);
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

// Both predicates delegate the message-text matching to
// `crate::diagnostic_filters` so the LSP and CLI stay in lock-step.
// The CLI helpers (`is_unknown_module_warning`,
// `is_user_import_resolvable_error` in src/cli/pipeline.rs) operate on
// `SourceError` (post-wrapping); these versions operate on the
// typechecker's native `TypeError` so the LSP can filter before
// converting to `lsp_types::Diagnostic`. Severity gating stays at the
// call site because the two error types' shapes differ.
fn is_unknown_module_warning_te(err: &typechecker::TypeError) -> bool {
    err.severity == typechecker::Severity::Warning
        && crate::diagnostic_filters::is_unknown_module_warning_message(&err.message)
}

fn is_user_import_resolvable_error_te(err: &typechecker::TypeError) -> bool {
    err.severity == typechecker::Severity::Error
        && crate::diagnostic_filters::is_user_import_resolvable_error_message(&err.message)
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_server::Connection;
    use std::str::FromStr;

    fn published_diagnostics(client: &Connection) -> Vec<Vec<Diagnostic>> {
        client
            .receiver
            .try_iter()
            .filter_map(|msg| match msg {
                Message::Notification(notif) if notif.method == PublishDiagnostics::METHOD => {
                    let params: PublishDiagnosticsParams =
                        serde_json::from_value(notif.params).expect("publishDiagnostics params");
                    Some(params.diagnostics)
                }
                _ => None,
            })
            .collect()
    }

    /// A lexer error on a multi-byte character yields a diagnostic over
    /// that character; the text is stored without a program.
    #[test]
    fn lex_error_on_non_ascii_character_is_a_diagnostic() {
        let (connection, client) = Connection::memory();
        let mut server = Server::new(connection);
        let uri = Uri::from_str("file:///test.silt").unwrap();
        let source = "fn main() {\n  println(“hello”)\n}\n";
        server.update_document(uri.clone(), source.to_string());

        let doc = server.documents.get(&uri).expect("document is stored");
        assert_eq!(doc.source, source);
        assert!(doc.program.is_none());
        let published = published_diagnostics(&client);
        assert_eq!(published.len(), 1);
        assert_eq!(published[0].len(), 1);
        let diagnostic = &published[0][0];
        assert_eq!(diagnostic.range.start, Position::new(1, 10));
        assert_eq!(diagnostic.range.end, Position::new(1, 11));
    }

    /// A panic inside the analysis must not leave the previous text in the
    /// store, must not escape, and must not stop later analyses.
    #[test]
    fn panicking_analysis_stores_the_new_text_and_reports_it() {
        let (connection, client) = Connection::memory();
        let mut server = Server::new(connection);
        let uri = Uri::from_str("file:///test.silt").unwrap();
        server.update_document(uri.clone(), "fn main() { 1 }".to_string());

        server.update_document_with(uri.clone(), "fn main() { 2 }".to_string(), |_, _| {
            panic!("deliberate panic in a test")
        });

        let doc = server.documents.get(&uri).expect("document is stored");
        assert_eq!(doc.source, "fn main() { 2 }");
        assert!(doc.program.is_none());
        let cached = server
            .diagnostics_cache
            .get(&uri)
            .expect("diagnostics are cached");
        assert_eq!(cached.len(), 1);
        assert!(cached[0].message.starts_with("internal error"));
        let published = published_diagnostics(&client);
        assert_eq!(published.len(), 2);
        assert_eq!(published[1].len(), 1);
        assert_eq!(published[1][0].message, cached[0].message);

        server.update_document(uri.clone(), "fn main() { 3 }".to_string());
        let doc = server.documents.get(&uri).expect("document is stored");
        assert_eq!(doc.source, "fn main() { 3 }");
        assert!(doc.program.is_some());
    }
}
