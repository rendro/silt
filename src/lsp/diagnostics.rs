//! Document analysis and diagnostics publishing.
//!
//! `didOpen` and `didChange` store the new text and schedule an analysis
//! ([`DEBOUNCE`] later, or before the next request, whichever comes
//! first). The analysis gives every open document to its project's
//! session as an overlay, analyses each open document as an entry, and
//! publishes the diagnostics of every file of the analysed graphs whose
//! diagnostics changed: an imported module's errors appear in the
//! imported file, and a file that no longer has any is cleared.
//!
//! The features read what the analysis leaves in each open [`Document`]:
//! the session's checked module (the AST with its types), its top-level
//! definitions and its local bindings.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lsp_server::Message;
use lsp_types::notification::{Notification as _, PublishDiagnostics};
use lsp_types::{Diagnostic, DiagnosticSeverity, Position, PublishDiagnosticsParams, Range, Uri};

use crate::diagnostic::Phase;
use crate::session::{ModuleId, parse_text};
use crate::source::{FileId, SourceFile, SourceMap, SourceName};

use super::Server;
use super::definitions::build_definitions;
use super::local_bindings::collect_local_bindings;
use super::panic_message;
use super::project::{Project, path_key, project_dir};
use super::state::{Document, ModuleRef};

/// How long after an edit the analysis runs, so that a burst of edits is
/// analysed once.
pub(super) const DEBOUNCE: Duration = Duration::from_millis(150);

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

/// The note published on an open document of a project whose packages
/// cannot be resolved: the errors are in `silt.toml`, and the document's
/// imports cannot be resolved until they are fixed.
fn broken_manifest_note() -> Diagnostic {
    Diagnostic {
        range: Range::new(Position::new(0, 0), Position::new(0, 0)),
        severity: Some(DiagnosticSeverity::INFORMATION),
        source: Some("silt".to_string()),
        message: "this file is not checked: the project's silt.toml has errors (see silt.toml)"
            .to_string(),
        ..Diagnostic::default()
    }
}

/// The text of the file at `path` and the document made of it, indexed
/// as a workspace file (not open): its declarations as parsed.
pub(super) fn indexed_document(path: PathBuf, text: Arc<str>) -> Document {
    let mut sources = SourceMap::new();
    let file = sources.add(SourceName::Path(path.clone()), text.clone());
    let (program, _) = parse_text(file, &text);
    let definitions = program
        .as_ref()
        .map(|p| build_definitions(p, None))
        .unwrap_or_default();
    let locals = program
        .as_ref()
        .map(|p| collect_local_bindings(p, &text, None))
        .unwrap_or_default();
    Document {
        source: SourceFile::new(SourceName::Path(path.clone()), text),
        key: path_key(&path),
        path,
        open: false,
        module: None,
        program: program.map(Arc::new),
        definitions,
        locals,
    }
}

/// The path a document's URI names.
pub(super) fn uri_to_path(uri: &Uri) -> PathBuf {
    super::file_uri_to_path(uri.as_str()).unwrap_or_else(|| PathBuf::from(uri.path().as_str()))
}

impl Server {
    /// Store `text` as the text of the open document `uri` and schedule
    /// its analysis.
    pub(super) fn update_document(&mut self, uri: Uri, text: String) {
        let path = uri_to_path(&uri);
        let source = SourceFile::new(SourceName::Overlay(path.clone()), text.into());
        let doc = self
            .documents
            .entry(uri.clone())
            .or_insert_with(|| Document {
                source: SourceFile::new(SourceName::Overlay(path.clone()), "".into()),
                key: path_key(&path),
                path: path.clone(),
                open: true,
                module: None,
                program: None,
                definitions: HashMap::new(),
                locals: Vec::new(),
            });
        doc.source = source;
        doc.open = true;
        self.pending.insert(uri);
        self.deadline
            .get_or_insert_with(|| Instant::now() + DEBOUNCE);
    }

    /// The editor closed `uri`: the file on disk is its text again. A
    /// workspace file stays indexed.
    pub(super) fn close_document(&mut self, uri: Uri) {
        let Some(doc) = self.documents.remove(&uri) else {
            return;
        };
        match std::fs::read_to_string(&doc.path) {
            Ok(text) => {
                self.documents
                    .insert(uri.clone(), indexed_document(doc.path, text.into()));
            }
            // A file deleted while it was open: the sessions that read it
            // read it again, and find it gone.
            Err(_) => self.disk_events.push(doc.path),
        }
        self.pending.insert(uri);
        self.deadline
            .get_or_insert_with(|| Instant::now() + DEBOUNCE);
    }

    /// The client reports that the files `changes` changed on disk. An
    /// open document's text is the editor's, so only the others count:
    /// a workspace file is indexed again (or dropped), and each session
    /// that read one is made again at the next analysis, which is
    /// scheduled.
    pub(super) fn files_changed_on_disk(&mut self, changes: Vec<lsp_types::FileEvent>) {
        for change in changes {
            if self.documents.get(&change.uri).is_some_and(|doc| doc.open) {
                continue;
            }
            let path = uri_to_path(&change.uri);
            let is_silt = path.extension().is_some_and(|ext| ext == "silt");
            match std::fs::read_to_string(&path) {
                Ok(text) if is_silt && change.typ != lsp_types::FileChangeType::DELETED => {
                    self.documents
                        .insert(change.uri, indexed_document(path.clone(), text.into()));
                }
                _ => {
                    self.documents.remove(&change.uri);
                }
            }
            self.disk_events.push(path);
        }
        if !self.disk_events.is_empty() {
            self.deadline
                .get_or_insert_with(|| Instant::now() + DEBOUNCE);
        }
    }

    /// Compare the stamps of the files the sessions read from disk and
    /// the client does not watch; a change schedules an analysis that
    /// makes the session again. A request runs this, so a file created
    /// or changed by another program is seen without an edit.
    pub(super) fn check_disk(&mut self) {
        for project in self.projects.values() {
            self.disk_events.extend(project.changed_files());
        }
    }

    /// Run the scheduled analysis now, if there is one.
    pub(super) fn analyse_pending(&mut self) {
        self.analyse_pending_with(Self::analyse);
    }

    /// [`Server::analyse_pending`] with `analyse` as the analysis.
    ///
    /// A panic inside the analysis is caught and logged to stderr. The
    /// sessions are dropped (the next analysis starts from fresh ones),
    /// and each changed open document is kept without analysis results,
    /// with one diagnostic that says so: the client has applied the edit
    /// already, so answering its next requests from the previous analysis
    /// would hand it positions for a text it no longer has.
    pub(super) fn analyse_pending_with(&mut self, analyse: fn(&mut Self, &HashSet<Uri>)) {
        self.deadline = None;
        if self.pending.is_empty() && self.disk_events.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut self.pending);
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| analyse(self, &pending)));
        if let Err(payload) = outcome {
            eprintln!(
                "silt-lsp: internal error while analysing the open documents: {}; they are \
                 kept without analysis results and the server keeps running",
                panic_message(payload)
            );
            self.projects.clear();
            for uri in &pending {
                if let Some(doc) = self.documents.get_mut(uri).filter(|d| d.open) {
                    doc.module = None;
                    doc.program = None;
                    doc.definitions.clear();
                    doc.locals.clear();
                    let diagnostics = vec![analysis_failed_diagnostic()];
                    self.published.insert(uri.clone(), diagnostics.clone());
                    self.publish_diagnostics(uri.clone(), diagnostics);
                }
            }
        }
    }

    /// Analyse every open document, after the documents `pending` changed
    /// (or closed), and publish what changed.
    fn analyse(&mut self, pending: &HashSet<Uri>) {
        // The texts the sessions get: every open document's, and the disk
        // text of a document just closed.
        let mut texts: Vec<(PathBuf, Arc<str>)> = Vec::new();
        let mut by_project: BTreeMap<PathBuf, Vec<Uri>> = BTreeMap::new();
        for (uri, doc) in &self.documents {
            if doc.open {
                texts.push((doc.path.clone(), doc.source.text.clone()));
                by_project
                    .entry(project_dir(&doc.path))
                    .or_default()
                    .push(uri.clone());
            } else if pending.contains(uri) {
                texts.push((doc.path.clone(), doc.source.text.clone()));
            }
        }
        self.projects.retain(|dir, _| by_project.contains_key(dir));
        let disk_events = std::mem::take(&mut self.disk_events);
        let open_keys: HashSet<PathBuf> = self
            .documents
            .values()
            .filter(|doc| doc.open)
            .map(|doc| doc.key.clone())
            .collect();
        // The files whose stamps are recorded: those not open, and not
        // under the workspace folder the client watches.
        let watched_root = self.root.clone().filter(|_| self.watching);
        let skip = |path: &std::path::Path| {
            let key = path_key(path);
            open_keys.contains(&key) || watched_root.as_ref().is_some_and(|r| key.starts_with(r))
        };

        // The URI of each file: an open document's when it has one (a
        // workspace file can be indexed under another spelling of the
        // same file, through a symbolic link).
        let mut uris_by_key: HashMap<PathBuf, Uri> = HashMap::new();
        for (uri, doc) in self.documents.iter().filter(|(_, doc)| !doc.open) {
            uris_by_key.insert(doc.key.clone(), uri.clone());
        }
        for (uri, doc) in self.documents.iter().filter(|(_, doc)| doc.open) {
            uris_by_key.insert(doc.key.clone(), uri.clone());
        }
        let mut diagnostics: HashMap<Uri, Vec<Diagnostic>> = HashMap::new();
        let mut analysed: Vec<(Uri, ModuleRef)> = Vec::new();
        for (dir, uris) in &by_project {
            // A session is made again when the project's manifest or
            // lockfile changed, or a file it read from disk changed: as
            // the client reports it, or, for a file the client does not
            // watch, as the file's stamp shows.
            let stale = self.projects.get(dir).is_none_or(|p| {
                p.is_stale(dir)
                    || !p.changed_files().is_empty()
                    || disk_events.iter().any(|path| p.has_module(path))
            });
            if stale {
                self.projects.insert(dir.clone(), Project::new(dir));
            }
            let project = self.projects.get_mut(dir).expect("inserted above");
            let entries: Vec<(Uri, PathBuf)> = uris
                .iter()
                .map(|uri| (uri.clone(), self.documents[uri].path.clone()))
                .collect();
            let modules = analyse_project(project, &entries, &texts);
            project.record_disk(skip);
            let packages_failed = project.session.packages().is_err();
            for (uri, id, mut found) in modules {
                if packages_failed {
                    // The cascade an unresolved project causes is not
                    // shown; its errors are in the manifest.
                    found.retain(|d| d.phase() == Phase::Package);
                    diagnostics
                        .entry(uri.clone())
                        .or_default()
                        .push(broken_manifest_note());
                }
                let sources = project.session.sources();
                let uri_of = |f: FileId| uri_of(&uris_by_key, sources, f);
                for d in &found {
                    let Some(uri) = uri_of(d.span.file) else {
                        continue;
                    };
                    let converted = crate::diagnostic::to_lsp(sources, d, &uri_of);
                    let list = diagnostics.entry(uri).or_default();
                    if !list.contains(&converted) {
                        list.push(converted);
                    }
                }
                analysed.push((
                    uri,
                    ModuleRef {
                        project: dir.clone(),
                        id,
                    },
                ));
            }
        }

        for (uri, module) in analysed {
            self.store_analysis(&uri, module);
            diagnostics.entry(uri).or_default();
        }

        // Publish each file whose diagnostics changed, each changed open
        // document, and clear each file that has none any more.
        let previous = std::mem::take(&mut self.published);
        for uri in previous.keys() {
            if !diagnostics.contains_key(uri) {
                self.publish_diagnostics(uri.clone(), Vec::new());
            }
        }
        let mut uris: Vec<&Uri> = diagnostics.keys().collect();
        uris.sort_by_key(|u| u.as_str());
        for uri in uris {
            let list = &diagnostics[uri];
            if previous.get(uri) != Some(list) || pending.contains(uri) {
                self.publish_diagnostics(uri.clone(), list.clone());
            }
        }
        self.published = diagnostics;
    }

    /// Leave in the open document `uri` what the features read: the
    /// checked module `module`, its definitions and its local bindings.
    fn store_analysis(&mut self, uri: &Uri, module: ModuleRef) {
        let project = &self.projects[&module.project];
        let parsed = project.session.graph().module(module.id).ast.is_some();
        let checked = project
            .session
            .module_analysis(module.id)
            .filter(|_| parsed);
        let doc = self.documents.get_mut(uri).expect("an analysed document");
        match checked {
            Some(checked) => {
                doc.definitions = build_definitions(&checked.ast, Some(&checked.top_level));
                doc.locals = collect_local_bindings(
                    &checked.ast,
                    &doc.source.text,
                    Some(&checked.top_level),
                );
                doc.program = Some(checked.ast.clone());
            }
            None => {
                doc.program = None;
                doc.definitions.clear();
                doc.locals.clear();
            }
        }
        doc.module = Some(module);
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

/// The URI of the file `file` of `sources`: the document's, when one
/// names the same file (`uris_by_key`, by path key).
fn uri_of(uris_by_key: &HashMap<PathBuf, Uri>, sources: &SourceMap, file: FileId) -> Option<Uri> {
    let path = match &sources.get(file)?.path {
        SourceName::Path(p) | SourceName::Overlay(p) | SourceName::Manifest(p) => p,
        _ => return None,
    };
    uris_by_key
        .get(&path_key(path))
        .cloned()
        .or_else(|| super::path_to_file_uri(path))
}

/// Give `project`'s session the texts it needs and analyse each of its
/// open documents (`entries`, with their paths) as an entry. Returns each
/// entry's module and its static diagnostics: the analysis's and, when
/// that has no error, what compiling finds.
///
/// The session gets the text of each open document of the project, and
/// of each other file in `texts` (every open document's, and the disk
/// text of each document just closed) that its graph reads, so an edit
/// in another package's file is seen by the packages that import it.
fn analyse_project(
    project: &mut Project,
    entries: &[(Uri, PathBuf)],
    texts: &[(PathBuf, Arc<str>)],
) -> Vec<(Uri, ModuleId, Vec<crate::diagnostic::Diagnostic>)> {
    let mut changed: Vec<ModuleId> = Vec::new();
    let mut modules: Vec<(Uri, ModuleId)> = Vec::new();
    for (uri, path) in entries {
        let text = &texts
            .iter()
            .find(|(p, _)| p == path)
            .expect("an open document has a text")
            .1;
        let id = match project.set_text(path, text) {
            Some(id) => {
                changed.push(id);
                id
            }
            None => project.module_at(path),
        };
        modules.push((uri.clone(), id));
    }
    // Each other file in `texts` that a graph reads gets its text, and
    // the analyses run again. Each round gives one more file its text, so
    // this ends.
    loop {
        for (_, id) in &modules {
            let file = project.file(*id);
            project.session.analyze(file);
        }
        let mut more = false;
        for (path, text) in texts {
            if project.has_module(path)
                && let Some(id) = project.set_text(path, text)
            {
                changed.push(id);
                more = true;
            }
        }
        if !more {
            break;
        }
    }
    project.forget_compiled(&changed);
    modules
        .into_iter()
        .map(|(uri, id)| {
            let file = project.file(id);
            let analysis = project.session.analyze(file).clone();
            let mut found = analysis.diagnostics;
            if !found.iter().any(crate::diagnostic::Diagnostic::is_error) {
                found.extend(
                    project
                        .compile_errors(id, &analysis.modules)
                        .iter()
                        .cloned(),
                );
            }
            (uri, id, found)
        })
        .collect()
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
        server.analyse_pending();

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
        server.analyse_pending();

        server.update_document(uri.clone(), "fn main() { 2 }".to_string());
        server.analyse_pending_with(|_, _| panic!("deliberate panic in a test"));

        let doc = server.documents.get(&uri).expect("document is stored");
        assert_eq!(&*doc.source.text, "fn main() { 2 }");
        assert!(doc.program.is_none());
        let cached = server.published.get(&uri).expect("diagnostics are cached");
        assert_eq!(cached.len(), 1);
        assert!(cached[0].message.starts_with("internal error"));
        let published = published_diagnostics(&client);
        assert_eq!(published.len(), 2);
        assert_eq!(published[1].len(), 1);
        assert_eq!(published[1][0].message, cached[0].message);

        server.update_document(uri.clone(), "fn main() { 3 }".to_string());
        server.analyse_pending();
        let doc = server.documents.get(&uri).expect("document is stored");
        assert_eq!(&*doc.source.text, "fn main() { 3 }");
        assert!(doc.program.is_some());
    }
}
