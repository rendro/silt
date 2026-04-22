//! `silt check [--format json] <file>` — run the full compile pipeline
//! without executing, reporting diagnostics.

use std::process;

use silt::errors::SourceError;

use crate::cli::help::check_usage_banner;
use crate::cli::package::resolve_package_entry_point;
use crate::cli::pipeline::{reportable_type_errors, run_compile_pipeline};
use crate::cli::source_scan::{looks_like_library_module, looks_like_test_file, program_has_main};

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
        if args[i] == "--format" {
            if i + 1 < args.len() && args[i + 1] == "json" {
                format = OutputFormat::Json;
                i += 2;
            } else {
                eprintln!("--format requires 'json'");
                process::exit(1);
            }
        } else if args[i] == "--help" || args[i] == "-h" {
            println!("Usage: {}", check_usage_banner());
            println!();
            println!("Options:");
            println!("  --format json   Emit diagnostics as JSON");
            println!("  --watch, -w     Re-run on file changes");
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
        } else {
            file = Some(args[i].clone());
            i += 1;
        }
    }
    let path = match file {
        Some(p) => p,
        None => match resolve_package_entry_point() {
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
    // `silt check` must match `silt run` diagnostics exactly, minus
    // execution. That means (a) running the compile step so the compiler
    // surfaces real module-resolution errors, and (b) filtering out the
    // type checker's "unknown module" warnings — which the compiler
    // resolves later — so we don't cry wolf on every valid file-backed
    // import. Previously this path skipped compile entirely AND emitted
    // every warning, which produced spurious "unknown module" warnings
    // on programs that `silt run` handles cleanly.
    let result = run_compile_pipeline(path, false, true, true);

    // Filter per-entry: drop the "unknown module" warnings the compiler
    // will resolve, but keep every other diagnostic so real errors still
    // surface. See `reportable_type_errors` for the rationale.
    let reportable_types = reportable_type_errors(&result);
    let mut errors: Vec<&SourceError> = result
        .parse_errors
        .iter()
        .chain(reportable_types.iter().copied())
        .chain(result.compile_errors.iter())
        .chain(result.compile_warnings.iter())
        .collect();

    // Round-24 B-fix: if compilation succeeded but the program defines no
    // `main` function AND the file doesn't look like a library module
    // (`pub fn ...`) or a test file (`fn test_...`), surface the same
    // canonical missing-main diagnostic that `silt run` emits — exit 1
    // with `error[compile]: program has no main() function`. Without
    // this, an empty / no-main "script" file would pass `silt check`
    // cleanly and then fail at `silt run`, which is off-spec.
    //
    // We deliberately exclude library modules (identified by any
    // `pub fn`) and test files (identified by `fn test_*` / `test.*`)
    // because those files legitimately never define `main` and are
    // consumed by importers / by `silt test` respectively. The
    // `silt run` path still flags both with its own nudge — `check`
    // is the "does this file compile standalone" answer, and neither
    // a library nor a test file should be invoked standalone.
    //
    // Lock: tests/empty_program_diagnostic_tests.rs and
    // tests/examples_check.rs (every_example_type_checks_and_has_no_warnings).
    let missing_main_err: Option<SourceError> = if errors.is_empty()
        && result.functions.is_some()
        && !program_has_main(&result.source)
        && !looks_like_library_module(&result.source)
        && !looks_like_test_file(&result.source)
    {
        let msg = "program has no main() function\nadd one as the entry point".to_string();
        Some(SourceError::compile_error_at(
            msg,
            silt::lexer::Span::new(0, 0),
            &result.source,
            path,
        ))
    } else {
        None
    };
    if let Some(ref err) = missing_main_err {
        errors.push(err);
    }

    if format == OutputFormat::Json {
        print_json_errors(&errors);
    } else {
        // F14 (audit round 17): separate diagnostics with blank lines.
        silt::errors::eprintln_errors_with_separator(&errors);
    }

    // A hard error is real only if it's a parse/compile error or a
    // non-suppressed type error with severity Error — same gate as
    // `compile_file`. We deliberately do NOT rely on
    // `result.has_hard_errors`, which counts the suppressed warnings'
    // peers but we re-check here for clarity.
    let has_real_type_error = reportable_types.iter().any(|e| !e.is_warning);
    let has_real_hard_errors = !result.parse_errors.is_empty()
        || !result.compile_errors.is_empty()
        || has_real_type_error
        || missing_main_err.is_some();
    if has_real_hard_errors {
        process::exit(1);
    }
}

fn print_json_errors(errors: &[&SourceError]) {
    let json_errors: Vec<serde_json::Value> = errors
        .iter()
        .map(|e| {
            // Round-36 fix: the human renderer emits `= help:` / `= note:`
            // continuation lines below the caret for any `\nhelp: ...` or
            // `\nnote: ...` suffix a diagnostic tacks onto its message
            // (see `src/typechecker/inference.rs` — did-you-mean hints are
            // appended as `\nhelp: did you mean ...?`). The JSON emitter
            // used to keep the first line as the `message` and drop the
            // rest, which meant `--format json` consumers (editors,
            // LSP front-ends, CI scripts) never saw the hints. We now
            // preserve the first line as `message` (backward compat) and
            // add a `hints` array extracted from the remaining lines,
            // filtered to diagnostic hint prefixes (`help:`, `note:`).
            let mut lines = e.message.lines();
            let head = lines.next().unwrap_or(&e.message);
            let hints: Vec<String> = lines
                .filter_map(|ln| {
                    let t = ln.trim_start();
                    if t.starts_with("help:") || t.starts_with("note:") {
                        Some(t.to_string())
                    } else {
                        None
                    }
                })
                .collect();
            serde_json::json!({
                "file": e.file.as_deref().unwrap_or("<unknown>"),
                "line": e.span.line,
                "col": e.span.col,
                "message": head,
                "hints": hints,
                "severity": if e.is_warning { "warning" } else { "error" },
                "kind": e.kind.to_string(),
            })
        })
        .collect();
    match serde_json::to_string(&json_errors) {
        Ok(json) => println!("{json}"),
        Err(e) => {
            eprintln!("internal error: failed to serialize diagnostics: {e}");
            process::exit(1);
        }
    }
}
