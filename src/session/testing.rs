//! Helpers for Rust tests: check or run a program given as text, through
//! the same session every front door uses.
//!
//! A program is one file, or several files of one directory that exists
//! only in memory (`files`): no disk is read, so a test needs no
//! temporary directory.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::diagnostic::Diagnostic;
use crate::value::Value;
use crate::vm::Vm;

use super::{Config, Entry, HostModule, LockPolicy, ProjectSetup, Session};

/// The directory the in-memory files of a test program are in.
const TEST_DIR: &str = "/silt-test";

/// A session over the in-memory `files` (name, text), with the first one
/// opened as the entry file, and the host modules `host`.
fn session(files: &[(&str, &str)], host: Vec<HostModule>) -> (Session, crate::source::FileId) {
    let dir = PathBuf::from(TEST_DIR);
    let mut session = Session::new(Config {
        project: ProjectSetup::Script(dir.clone()),
        lock: LockPolicy::ReadOnly,
        host,
    });
    let mut entry = None;
    for (name, text) in files {
        let file = session.set_overlay(&dir.join(name), (*text).to_string());
        entry.get_or_insert(file);
    }
    (session, entry.expect("a program has at least one file"))
}

/// The static diagnostics of `source`: its analysis and, when that has
/// no error, what compiling its declarations finds. `main` is not
/// required.
pub fn check_str(source: &str) -> Vec<Diagnostic> {
    check_files(&[("main.silt", source)])
}

/// [`check_str`] for a program of several files; the first is the entry.
pub fn check_files(files: &[(&str, &str)]) -> Vec<Diagnostic> {
    check_with_host(files, Vec::new())
}

/// [`check_files`] with the host modules `host`.
pub fn check_with_host(files: &[(&str, &str)], host: Vec<HostModule>) -> Vec<Diagnostic> {
    let (mut session, entry) = session(files, host);
    let mut diagnostics = session.analyze(entry).diagnostics.clone();
    if session.analyze(entry).has_errors() {
        return diagnostics;
    }
    match session.compile(entry, Entry::Tests { filter: None }) {
        Ok(program) => diagnostics.extend(program.warnings),
        Err(errors) => diagnostics.extend(errors),
    }
    diagnostics
}

/// Compile `source` as a program that starts at `main`, run it, and give
/// `main`'s value. `Err` holds the message of the first static error,
/// or of the runtime error.
pub fn run_str(source: &str) -> Result<Value, String> {
    run_files(&[("main.silt", source)])
}

/// [`run_str`] for a program of several files; the first is the entry.
pub fn run_files(files: &[(&str, &str)]) -> Result<Value, String> {
    run_with_host(files, Vec::new())
}

/// [`run_files`] with the host modules `host`.
pub fn run_with_host(files: &[(&str, &str)], host: Vec<HostModule>) -> Result<Value, String> {
    let (mut session, entry) = session(files, host);
    let analysis = session.analyze(entry);
    if let Some(error) = analysis.diagnostics.iter().find(|d| d.is_error()) {
        return Err(error.message.clone());
    }
    let program = session.compile(entry, Entry::Main).map_err(|errors| {
        errors
            .first()
            .map(|d| d.message.clone())
            .unwrap_or_default()
    })?;
    let script = program
        .functions
        .into_iter()
        .next()
        .expect("a compiled program has a script");
    let mut vm = Vm::new();
    vm.run(Arc::new(script)).map_err(|e| e.to_string())
}

/// The path of the in-memory file `name` of a test program, for a test
/// that edits it through [`Session::set_overlay`].
pub fn test_path(name: &str) -> PathBuf {
    Path::new(TEST_DIR).join(name)
}

/// A session over the in-memory `files`, the first opened as the entry,
/// for a test that drives the session itself.
pub fn session_with(files: &[(&str, &str)]) -> (Session, crate::source::FileId) {
    session(files, Vec::new())
}
