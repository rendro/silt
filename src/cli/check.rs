//! `silt check [--format json] [--deny-warnings] <file>` — analyse and
//! compile a program without executing it, reporting diagnostics.

use std::path::Path;
use std::process;

use silt::diagnostic::{Diagnostic, SourceView};
use silt::session::{Entry, LockPolicy};

use crate::cli::help::check_usage_banner;
use crate::cli::package::{EntryPointKind, resolve_package_entry_point_for};
use crate::cli::paths::{ProgramFiles, door_diagnostics, open_entry_or_exit, package_test_files};

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
            // Positionals after `--` are forwarded to the
            // program as `io.args()`. `silt check` doesn't execute, so
            // these are effectively ignored — but we accept and silently
            // skip them so that `silt check` is a transparent drop-in
            // for `silt run` in CI scripts that pre-bake program args.
            // Stop CLI flag parsing here.
            break;
        } else if args[i] == "--format" {
            if i + 1 < args.len() && args[i + 1] == "json" {
                format = OutputFormat::Json;
                i += 2;
            } else {
                eprintln!("--format requires 'json'");
                process::exit(1);
            }
        } else if let Some(value) = args[i].strip_prefix("--format=") {
            // GNU-style `--format=json` form, to match `silt add --path=...`
            // and every other subcommand that accepts an `=`-joined value.
            if value == "json" {
                format = OutputFormat::Json;
                i += 1;
            } else {
                eprintln!("--format requires 'json'");
                process::exit(1);
            }
        } else if args[i] == "--deny-warnings" {
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
            eprintln!("silt check: unknown flag '{}'{}", args[i], suggestion);
            eprintln!("Run 'silt check --help' for usage.");
            process::exit(1);
        } else if file.is_none() {
            file = Some(args[i].clone());
            i += 1;
        } else {
            // Reject extra positionals — `silt check` takes at most
            // one file. Pre-fix the assign was unconditional and
            // last-wins, so `silt check a.silt b.silt` silently
            // checked only `b.silt` while the user thought both ran.
            // Mirror the rejection pattern used by `silt update`,
            // `silt repl`, `silt lsp`, and `silt add`.
            //
            // To bake program args into a `silt check`
            // invocation (e.g. for CI parity with `silt run`), use
            // `--` as a separator.
            eprintln!("silt check: unexpected extra argument '{}'", args[i]);
            eprintln!(
                "If '{}' is meant for the program, separate it with '--' (e.g. 'silt check <file>.silt -- {}').",
                args[i], args[i]
            );
            eprintln!("Run 'silt check --help' for usage.");
            process::exit(1);
        }
    }
    if format == OutputFormat::Json {
        crate::cli::package::print_manifest_errors_as_json();
    }
    let (path, tests) = match file {
        Some(p) => (p, Vec::new()),
        // `silt check` accepts a lib-only package
        // (`src/lib.silt` with no `src/main.silt`) — the required shape
        // for dependencies. `silt run`/`silt disasm` keep requiring main.
        // The package's test files are checked with it, so that one
        // command sees every warning of the package.
        None => match resolve_package_entry_point_for(EntryPointKind::AllowLib) {
            Ok(Some(p)) => {
                let root = p.parent().and_then(Path::parent).unwrap_or(Path::new("."));
                (p.to_string_lossy().into_owned(), package_test_files(root))
            }
            Ok(None) => {
                eprintln!("Usage: {}", check_usage_banner());
                process::exit(1);
            }
            Err(()) => process::exit(1),
        },
    };
    check_files(&path, &tests, format, deny_warnings);
}

/// Check the entry file `path` and the test files `tests` in one
/// session, and report what was found, each diagnostic once.
fn check_files(path: &str, tests: &[String], format: OutputFormat, deny_warnings: bool) {
    silt::intern::reset();
    // `silt check` checks each file as a module: the session's analysis
    // and the compile step, as the language server does. A module needs
    // no `main`; `silt run` asks for one. A `main` the file binds must be
    // able to start, and so must its test functions.
    let (mut session, file) = open_entry_or_exit(path, LockPolicy::Update);
    let mut unreadable = false;
    let mut files = vec![file];
    for test in tests {
        match session.open(Path::new(test)) {
            Ok(file) => files.push(file),
            Err(e) => {
                eprintln!(
                    "error reading {test}: {}",
                    silt::diagnostic::io_error_text(&e)
                );
                unreadable = true;
            }
        }
    }
    // Each once, in the order found (a set beside the list: a program
    // can have many thousands of warnings).
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let mut seen: std::collections::HashSet<Diagnostic> = std::collections::HashSet::new();
    for file in files {
        let compiled = session.compile(file, Entry::Tests { filter: None });
        for d in door_diagnostics(&mut session, file, &compiled) {
            if seen.insert(d.clone()) {
                diagnostics.push(d);
            }
        }
    }

    let files = ProgramFiles::new(path, session.sources());
    if format == OutputFormat::Json {
        print_json_errors(&files, &diagnostics);
    } else {
        // F14: separate diagnostics with blank lines.
        silt::diagnostic::eprint_all(&files, &diagnostics);
    }

    if unreadable || diagnostics.iter().any(Diagnostic::is_error) {
        process::exit(1);
    }
    let warnings = diagnostics.iter().filter(|d| d.is_warning()).count();
    if deny_warnings && warnings > 0 {
        let noun = if warnings == 1 { "warning" } else { "warnings" };
        eprintln!("silt check: {warnings} {noun}, denied by --deny-warnings");
        process::exit(1);
    }
}

/// `errors` as one JSON array on stdout (see `diagnostic::render_json`).
/// Nothing in it is colored: the renderer writes no escape sequences.
fn print_json_errors(files: &dyn SourceView, errors: &[Diagnostic]) {
    let json_errors: Vec<serde_json::Value> = errors
        .iter()
        .map(|d| silt::diagnostic::render_json(files, d))
        .collect();
    match serde_json::to_string(&json_errors) {
        Ok(json) => println!("{json}"),
        Err(e) => {
            eprintln!("internal error: failed to serialize diagnostics: {e}");
            process::exit(1);
        }
    }
}
