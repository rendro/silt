//! `silt check [--format json] [--deny-warnings] <file>` — analyse and
//! compile a program without executing it, reporting diagnostics.

use std::path::Path;
use std::process;

use silt::diagnostic::{Diagnostic, SourceView};
use silt::session::{Entry, LockPolicy, Session};

use crate::cli::help::check_usage_banner;
use crate::cli::package::{EntryPointKind, resolve_package_entry_point_for};
use crate::cli::paths::{ProgramFiles, door_diagnostics, open_entry, package_test_files};

/// Output format for `silt check` — human-readable by default, or
/// machine-readable JSON when `--format json` is passed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum OutputFormat {
    Human,
    Json,
}

/// Dispatch `silt check [--format json] [--deny-warnings] <file>`.
pub(crate) fn dispatch(args: &[String]) {
    let mut file: Option<String> = None;
    let mut format = OutputFormat::Human;
    let mut deny_warnings = false;
    let mut i = 2;
    while i < args.len() {
        if args[i] == "--" {
            // `silt check` runs no program, so it takes no program
            // arguments: `--` is an argument too many.
            eprintln!("error: silt check: unexpected extra argument '--'");
            eprintln!("Run 'silt check --help' for usage.");
            process::exit(1);
        } else if args[i] == "--format" || args[i].starts_with("--format=") {
            if format == OutputFormat::Json {
                crate::cli::once_only("check", "--format");
            }
            if let Some(value) = args[i].strip_prefix("--format=") {
                // GNU-style `--format=json` form, to match `silt add
                // --path=...` and every other subcommand that accepts an
                // `=`-joined value.
                if value == "json" {
                    format = OutputFormat::Json;
                    i += 1;
                } else {
                    eprintln!("error: silt check: --format requires 'json'");
                    process::exit(1);
                }
            } else if i + 1 < args.len() && args[i + 1] == "json" {
                format = OutputFormat::Json;
                i += 2;
            } else {
                eprintln!("error: silt check: --format requires 'json'");
                process::exit(1);
            }
        } else if args[i] == "--deny-warnings" {
            if deny_warnings {
                crate::cli::once_only("check", "--deny-warnings");
            }
            deny_warnings = true;
            i += 1;
        } else if args[i] == "--help" || args[i] == "-h" {
            println!("Usage: {}", check_usage_banner());
            println!();
            println!("Options:");
            println!("  --format json       Emit diagnostics as JSON");
            println!("  --deny-warnings     Exit with 1 when there is a warning");
            println!("  --watch, -w         Re-run on file changes");
            process::exit(0);
        } else if args[i].starts_with('-') {
            // Unknown flag — don't silently treat as a filename.
            let suggestion = match args[i].as_str() {
                "--formats" | "-format" | "-f" => " (did you mean --format?)",
                "--h" | "-help" => " (did you mean --help?)",
                _ => "",
            };
            eprintln!(
                "error: silt check: unknown flag '{}'{}",
                args[i], suggestion
            );
            eprintln!("Run 'silt check --help' for usage.");
            process::exit(1);
        } else if file.is_none() {
            file = Some(args[i].clone());
            i += 1;
        } else {
            // Reject extra positionals: `silt check` takes at most one
            // file.
            eprintln!("error: silt check: unexpected extra argument '{}'", args[i]);
            eprintln!("Run 'silt check --help' for usage.");
            process::exit(1);
        }
    }
    if format == OutputFormat::Json {
        crate::cli::package::print_manifest_errors_as_json();
    }
    let (path, modules, tests) = match file {
        Some(p) => (p, Vec::new(), Vec::new()),
        None => match package_files() {
            Ok(Some(files)) => (files.entry, files.modules, files.tests),
            Ok(None) => {
                crate::cli::no_file("check");
                eprintln!("Usage: {}", check_usage_banner());
                process::exit(1);
            }
            Err(()) => process::exit(1),
        },
    };
    check_files(&path, &modules, &tests, format, deny_warnings);
}

/// The files of a package that `silt check` checks.
pub(crate) struct PackageFiles {
    /// `src/main.silt`, or `src/lib.silt` in a library.
    pub(crate) entry: String,
    /// Every other `.silt` file directly in `src/`, imported or not.
    pub(crate) modules: Vec<String>,
    /// The test files of the package.
    pub(crate) tests: Vec<String>,
}

/// The files of the package of the working directory, so that one
/// command sees every error and warning of the package. `Ok(None)`
/// outside a package; `Err` when the package has no entry file (said).
/// A library (`src/lib.silt` with no `src/main.silt`) is checked too:
/// that is the shape of a dependency.
pub(crate) fn package_files() -> Result<Option<PackageFiles>, ()> {
    let Some(entry) = resolve_package_entry_point_for(EntryPointKind::AllowLib)? else {
        return Ok(None);
    };
    let root = entry
        .parent()
        .and_then(Path::parent)
        .unwrap_or(Path::new("."));
    let tests = package_test_files(root);
    let modules = package_modules(root, &entry, &tests);
    Ok(Some(PackageFiles {
        entry: entry.to_string_lossy().into_owned(),
        modules,
        tests,
    }))
}

/// Whether the session has read the file at `path` (as an import of a
/// file checked before).
fn is_loaded(session: &Session, path: &Path) -> bool {
    let path = silt::source::canonical_path(path);
    session.sources().files().any(|file| match &file.path {
        silt::source::SourceName::Path(p) => silt::source::canonical_path(p) == path,
        _ => false,
    })
}

/// The modules of the package at `root` other than its entry file
/// `entry`: every `.silt` file directly in `src/`, sorted, but for the
/// test files `tests` (checked as tests).
fn package_modules(root: &Path, entry: &Path, tests: &[String]) -> Vec<String> {
    let Ok(dir) = std::fs::read_dir(root.join("src")) else {
        return Vec::new();
    };
    let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    };
    let mut modules: Vec<String> = dir
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "silt") && p.is_file())
        .filter(|p| !same(p, entry))
        .filter(|p| !tests.iter().any(|t| same(p, Path::new(t))))
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    modules.sort();
    modules
}

/// What checking a program found.
pub(crate) struct Checked {
    /// The session it was checked in: its sources name the files.
    pub(crate) session: Session,
    /// The files that could not be read, each with why.
    pub(crate) unreadable: Vec<(String, std::io::Error)>,
    /// What was found, each diagnostic once, in the order found.
    pub(crate) diagnostics: Vec<Diagnostic>,
}

impl Checked {
    /// Whether a file could not be read or an error was found.
    pub(crate) fn failed(&self) -> bool {
        !self.unreadable.is_empty() || self.diagnostics.iter().any(Diagnostic::is_error)
    }
}

/// Check the entry file `path`, the test files `tests` and the modules
/// `modules` of its package in one session. A module that the entry or
/// a test imports is checked there, as an import (its errors show where
/// it is imported); only the others are opened as files of their own.
/// `Err` when the entry file cannot be read.
pub(crate) fn check_package(
    path: &str,
    modules: &[String],
    tests: &[String],
) -> std::io::Result<Checked> {
    // `silt check` checks each file as a module: the session's analysis
    // and the compile step, as the language server does. A module needs
    // no `main`; `silt run` asks for one. A `main` the file binds must be
    // able to start, and so must its test functions.
    let (mut session, file) = open_entry(path, LockPolicy::Update)?;
    let mut unreadable = Vec::new();
    // Each once, in the order found (a set beside the list: a program
    // can have many thousands of warnings).
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let mut seen: std::collections::HashSet<Diagnostic> = std::collections::HashSet::new();
    let mut check = |session: &mut Session, file| {
        let compiled = session.compile(file, Entry::Tests { filter: None });
        for d in door_diagnostics(session, file, &compiled) {
            if seen.insert(d.clone()) {
                diagnostics.push(d);
            }
        }
    };
    check(&mut session, file);
    for test in tests {
        match session.open(Path::new(test)) {
            Ok(file) => check(&mut session, file),
            Err(e) => unreadable.push((test.clone(), e)),
        }
    }
    for module in modules {
        if is_loaded(&session, Path::new(module)) {
            continue;
        }
        match session.open(Path::new(module)) {
            Ok(file) => check(&mut session, file),
            Err(e) => unreadable.push((module.clone(), e)),
        }
    }
    Ok(Checked {
        session,
        unreadable,
        diagnostics,
    })
}

/// Check the entry file `path`, the test files `tests` and the modules
/// `modules` of its package, and report what was found, each diagnostic
/// once.
fn check_files(
    path: &str,
    modules: &[String],
    tests: &[String],
    format: OutputFormat,
    deny_warnings: bool,
) {
    silt::intern::reset();
    let checked = match check_package(path, modules, tests) {
        Ok(checked) => checked,
        Err(e) => {
            report_unreadable(format, &[(path.to_string(), e)]);
            process::exit(1);
        }
    };
    let Checked {
        session,
        unreadable,
        diagnostics,
    } = &checked;

    let files = ProgramFiles::new(path, session.sources());
    if format == OutputFormat::Json {
        print_json_errors(&files, unreadable, diagnostics);
    } else {
        report_unreadable(format, unreadable);
        // F14: separate diagnostics with blank lines.
        silt::diagnostic::eprint_all(&files, diagnostics);
    }

    if checked.failed() {
        process::exit(1);
    }
    let warnings = diagnostics.iter().filter(|d| d.is_warning()).count();
    if deny_warnings && warnings > 0 {
        let noun = if warnings == 1 { "warning" } else { "warnings" };
        eprintln!("error: silt check: {warnings} {noun}, denied by --deny-warnings");
        process::exit(1);
    }
}

/// The files that could not be read, each with why: "error reading
/// <file>: <why>" lines, or under `--format json` the JSON array of them
/// on stdout.
fn report_unreadable(format: OutputFormat, unreadable: &[(String, std::io::Error)]) {
    match format {
        OutputFormat::Json => print_json_errors(&silt::source::SourceMap::new(), unreadable, &[]),
        OutputFormat::Human => {
            for (path, e) in unreadable {
                eprintln!(
                    "error reading {path}: {}",
                    silt::diagnostic::io_error_text(e)
                );
            }
        }
    }
}

/// A file that could not be read as an entry of the JSON array: the
/// fields of a diagnostic (`diagnostic::render_json`), with the file as
/// its place and no position in it.
fn unreadable_json(path: &str, e: &std::io::Error) -> serde_json::Value {
    serde_json::json!({
        "code": silt::diagnostic::Code::ModuleNotFound.id(),
        "col": 0,
        "end_col": 0,
        "end_line": 0,
        "file": path,
        "help": [],
        "kind": "compile",
        "labels": [],
        "line": 0,
        "message": format!("error reading {path}: {}", silt::diagnostic::io_error_text(e)),
        "notes": [],
        "severity": "error",
    })
}

/// The files that could not be read, then `errors`, as one JSON array
/// on stdout (see `diagnostic::render_json`). Nothing in it is colored:
/// the renderer writes no escape sequences.
fn print_json_errors(
    files: &dyn SourceView,
    unreadable: &[(String, std::io::Error)],
    errors: &[Diagnostic],
) {
    let json_errors: Vec<serde_json::Value> = unreadable
        .iter()
        .map(|(path, e)| unreadable_json(path, e))
        .chain(
            errors
                .iter()
                .map(|d| silt::diagnostic::render_json(files, d)),
        )
        .collect();
    match serde_json::to_string(&json_errors) {
        Ok(json) => println!("{json}"),
        Err(e) => {
            eprintln!("internal error: failed to serialize diagnostics: {e}");
            process::exit(1);
        }
    }
}
