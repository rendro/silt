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
use crate::lexer::Lexer;
use crate::parser::Parser;
use crate::session::{Config, Entry, LockPolicy, ProjectSetup, Session};
use crate::source::{SourceFile, SourceMap, SourceName};
use crate::typechecker;

use super::Server;
use super::definitions::build_definitions;
use super::local_bindings::collect_local_bindings;
use super::panic_message;
use super::state::{DefInfo, Document, LocalBinding};

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

/// The static diagnostics of `source`, the text of the document `uri`,
/// as every front door reports them: from a session over the document's
/// project, with the document's text as an overlay. Only those in the
/// document itself are published for it.
fn session_diagnostics(source: &SourceFile, uri: &Uri) -> Vec<Diagnostic> {
    let path = match &source.path {
        SourceName::Overlay(path) | SourceName::Path(path) => path.clone(),
        _ => std::path::PathBuf::from("untitled.silt"),
    };
    let project = match path.parent() {
        Some(dir) if dir.is_dir() => ProjectSetup::Discover(dir.to_path_buf()),
        _ => ProjectSetup::None,
    };
    let mut session = Session::new(Config {
        project,
        lock: LockPolicy::ReadOnly,
        host: Vec::new(),
    });
    let file = session.set_overlay(&path, source.text.to_string());
    let mut diagnostics = session.analyze(file).diagnostics.clone();
    if let Err(errors) = session.compile(file, Entry::Tests { filter: None }) {
        diagnostics.extend(errors);
    }
    // The document's file, however the session registered it: a file can
    // be registered once as the document and once by a package check.
    let in_document = |d: &crate::diagnostic::Diagnostic| {
        d.span.file == file
            || session
                .sources()
                .get(d.span.file)
                .is_some_and(|f| match &f.path {
                    SourceName::Path(p) | SourceName::Overlay(p) => *p == path,
                    _ => false,
                })
    };
    diagnostics
        .iter()
        .filter(|d| in_document(d))
        .map(|d| {
            crate::diagnostic::to_lsp(session.sources(), d, &|f| {
                (f == file || f == d.span.file).then(|| uri.clone())
            })
        })
        .collect()
}

/// Analyse `source`, the text of the document `uri`: its diagnostics from
/// the session, and the syntax tree the editor features read.
fn analyse(source: &SourceFile, uri: &Uri) -> Analysis {
    let diagnostics = session_diagnostics(source, uri);
    let mut sources = SourceMap::new();
    let file = sources.add(source.path.clone(), source.text.clone());
    let Ok(tokens) = Lexer::new(file, &source.text).tokenize() else {
        return Analysis::without_program(diagnostics);
    };
    let (mut program, _) = Parser::new(tokens, &source.text)
        .with_docs()
        .parse_program_recovering();
    // The features read the types the checker fills in.
    let _ = typechecker::check(&mut program);

    let definitions = build_definitions(&program);
    let locals = collect_local_bindings(&program, &source.text);

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
        analyse: fn(&SourceFile, &Uri) -> Analysis,
    ) {
        let source = SourceFile::new(
            SourceName::Overlay(uri.path().as_str().into()),
            source.into(),
        );
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| analyse(&source, &uri)));
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
        assert_eq!(&*doc.source.text, source);
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
        assert_eq!(&*doc.source.text, "fn main() { 2 }");
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
        assert_eq!(&*doc.source.text, "fn main() { 3 }");
        assert!(doc.program.is_some());
    }
}
