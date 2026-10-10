//! `silt check [--format json] <file>` — analyse and compile a program
//! without executing it, reporting diagnostics.

use std::process;

use silt::diagnostic::{Diagnostic, SourceView};
use silt::session::{Entry, LockPolicy};

use crate::cli::help::check_usage_banner;
use crate::cli::package::{EntryPointKind, resolve_package_entry_point_for};
use crate::cli::paths::{ProgramFiles, door_diagnostics, open_entry_or_exit};

/// Output format for `silt check` — human-readable by default, or
/// machine-readable JSON when `--format json` is passed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum OutputFormat {
    Human,
    Json,
}

/// Dispatch `silt check [--format json] <file>`.
pub(crate) fn dispatch(args: &[String]) {
    let mut file: Option<String> = None;
    let mut format = OutputFormat::Human;
    let mut i = 2;
    while i < args.len() {
        if args[i] == "--" {
            // Round-74: positionals after `--` are forwarded to the
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
        } else if args[i] == "--help" || args[i] == "-h" {
            println!("Usage: {}", check_usage_banner());
            println!();
            println!("Options:");
            println!("  --format json       Emit diagnostics as JSON");
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
            // Round-74: to bake program args into a `silt check`
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
    let path = match file {
        Some(p) => p,
        // Round 93: `silt check` accepts a lib-only package
        // (`src/lib.silt` with no `src/main.silt`) — the required shape
        // for dependencies. `silt run`/`silt disasm` keep requiring main.
        None => match resolve_package_entry_point_for(EntryPointKind::AllowLib) {
            Ok(Some(p)) => p.to_string_lossy().into_owned(),
            Ok(None) => {
                eprintln!("Usage: {}", check_usage_banner());
                process::exit(1);
            }
            Err(()) => process::exit(1),
        },
    };
    check_file(&path, format);
}

pub(crate) fn check_file(path: &str, format: OutputFormat) {
    silt::intern::reset();
    // `silt check` checks the file as a module: the session's analysis
    // and the compile step, as the language server does. A module needs
    // no `main`; `silt run` asks for one. A `main` the file binds must be
    // able to start, and so must its test functions.
    let (mut session, file) = open_entry_or_exit(path, LockPolicy::Update);
    let compiled = session.compile(file, Entry::Tests { filter: None });
    let errors = door_diagnostics(&mut session, file, &compiled);

    let files = ProgramFiles::new(path, session.sources());
    if format == OutputFormat::Json {
        print_json_errors(&files, &errors);
    } else {
        // F14 (audit round 17): separate diagnostics with blank lines.
        silt::diagnostic::eprint_all(&files, &errors);
    }

    if errors.iter().any(Diagnostic::is_error) {
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
