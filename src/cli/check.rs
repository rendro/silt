//! `silt check [--format json] <file>` — run the full compile pipeline
//! without executing, reporting diagnostics.

use std::process;

use silt::errors::SourceError;

use crate::cli::help::check_usage_banner;
use crate::cli::package::resolve_package_entry_point;
use crate::cli::pipeline::{
    reportable_type_errors, resolve_strict_effects, run_compile_pipeline_with_options,
};
use crate::cli::source_scan::{looks_like_library_module, looks_like_test_file, program_has_main};

/// Output format for `silt check` — human-readable by default, or
/// machine-readable JSON when `--format json` is passed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum OutputFormat {
    Human,
    Json,
}

/// Dispatch `silt check [--format json] [--strict-effects] <file>`.
pub(crate) fn dispatch(args: &[String]) {
    let mut file: Option<String> = None;
    let mut format = OutputFormat::Human;
    // Phase D effect-rows flag. `None` means "not supplied on the
    // CLI"; the manifest's `[lints] strict-effects` field then
    // decides. `Some(true)` forces strict mode regardless of
    // manifest. `Some(false)` would force off (we don't expose a
    // negative form yet — adding `--no-strict-effects` is a future
    // ergonomic improvement). See `cli::pipeline::resolve_strict_effects`.
    let mut strict_effects: Option<bool> = None;
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
        } else if args[i] == "--strict-effects" {
            strict_effects = Some(true);
            i += 1;
        } else if args[i] == "--help" || args[i] == "-h" {
            println!("Usage: {}", check_usage_banner());
            println!();
            println!("Options:");
            println!("  --format json       Emit diagnostics as JSON");
            println!("  --strict-effects    Treat unannotated fns as pure (Phase D)");
            println!("  --watch, -w         Re-run on file changes");
            process::exit(0);
        } else if args[i].starts_with('-') {
            // Unknown flag — don't silently treat as a filename.
            let suggestion = match args[i].as_str() {
                "--formats" | "-format" | "-f" => " (did you mean --format?)",
                "--h" | "-help" => " (did you mean --help?)",
                "--strict-effect" | "--strict_effects" => " (did you mean --strict-effects?)",
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
    let strict = resolve_strict_effects(&path, strict_effects);
    check_file(&path, format, strict);
}

pub(crate) fn check_file(path: &str, format: OutputFormat, strict_effects: bool) {
    silt::intern::reset();
    // `silt check` must match `silt run` diagnostics exactly, minus
    // execution. That means (a) running the compile step so the compiler
    // surfaces real module-resolution errors, and (b) filtering out the
    // type checker's "unknown module" warnings — which the compiler
    // resolves later — so we don't cry wolf on every valid file-backed
    // import. Previously this path skipped compile entirely AND emitted
    // every warning, which produced spurious "unknown module" warnings
    // on programs that `silt run` handles cleanly.
    let result = run_compile_pipeline_with_options(path, false, true, true, strict_effects);

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
            // add a `hints` array extracted from the remaining lines.
            //
            // Round-77 fix (GAP ERR-1): the human renderer (`SourceError::
            // Display` at `src/errors.rs:366-389`) prepends `= note:` to
            // ANY unprefixed first body line, so structural notes like
            // `add one as the entry point` (emitted alongside
            // `program has no main() function`) showed in stderr but
            // were silently dropped from JSON output. We now mirror the
            // renderer: an unprefixed first body line is emitted as a
            // `note:` hint, and subsequent unprefixed lines (continuation
            // of an existing note/help) are appended verbatim — matching
            // the human form's `       <line>` continuation indent.
            let mut lines = e.message.lines();
            let head = lines.next().unwrap_or(&e.message);
            let mut hints: Vec<String> = Vec::new();
            let mut first_body_line = true;
            for ln in lines {
                let t = ln.trim_start();
                if t.starts_with("help:") || t.starts_with("note:") {
                    // Already prefixed — keep as-is (after trimming
                    // leading whitespace, matching the prior behavior).
                    hints.push(t.to_string());
                    first_body_line = false;
                } else if first_body_line {
                    // Unprefixed first body line: human renderer treats
                    // this as `= note: <line>`. Mirror that here so JSON
                    // consumers see the same hint.
                    hints.push(format!("note: {t}"));
                    first_body_line = false;
                } else {
                    // Unprefixed continuation of a prior hint: append to
                    // the most recent hint so multi-line notes round-trip
                    // as a single logical entry (matching the renderer's
                    // 7-space continuation indent under `= note:`).
                    if let Some(last) = hints.last_mut() {
                        last.push('\n');
                        last.push_str(t);
                    } else {
                        // No prior hint to attach to — fall back to a
                        // bare `note:` so the line isn't dropped.
                        hints.push(format!("note: {t}"));
                    }
                }
            }
            // Round-86 fix (B1): JSON output is machine-readable; it must
            // never carry ANSI escape sequences regardless of upstream
            // signal (TTY, FORCE_COLOR, etc). `format_module_source_error`
            // at `src/compiler/mod.rs:218-239` routes its inner snippet
            // through `active_colors()` and bakes ANSI codes into the
            // message string under FORCE_COLOR=1 or a real TTY — which
            // then leak into `head` and `hints` here, corrupting output
            // for editor plugins / LSP / CI. Strip at the boundary: this
            // is the only place JSON crosses out of the process, so a
            // single defensive sweep keeps the contract simple and
            // covers any future color sources we forget about.
            let head_clean = strip_ansi(head);
            let hints_clean: Vec<String> = hints.iter().map(|h| strip_ansi(h)).collect();
            serde_json::json!({
                "file": e.file.as_deref().unwrap_or("<unknown>"),
                "line": e.span.line,
                "col": e.span.col,
                "message": head_clean,
                "hints": hints_clean,
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

/// Strip ANSI SGR escape sequences (`ESC [ ... m`) from `s`.
///
/// Round-86 fix (B1): JSON-formatted diagnostics must be plain text so
/// editor plugins / LSP / CI consumers can parse them safely. Upstream
/// renderers like `format_module_source_error`
/// (`src/compiler/mod.rs:218-239`) honor FORCE_COLOR / TTY and bake
/// ANSI codes into the message string; we sweep them out here at the
/// JSON boundary to keep that surface contract simple.
///
/// Recognizes the CSI-SGR form we actually emit: `\x1b[` followed by
/// any run of digits and `;`, terminated by `m`. Anything that isn't a
/// well-formed SGR is passed through unchanged so unrelated `\x1b`
/// bytes (if any ever leak in) are still visible to the consumer
/// rather than silently absorbed.
fn strip_ansi(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            // Scan params: digits and `;` until terminator `m`.
            let mut j = i + 2;
            while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b';') {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'm' {
                // Consume the entire `\x1b[...m` sequence.
                i = j + 1;
                continue;
            }
            // Malformed / non-SGR — emit the ESC byte literally and
            // advance one. The next iteration will pick up `[` etc as
            // normal text.
            out.push(bytes[i] as char);
            i += 1;
        } else {
            // Push the byte as a char. All our color sequences operate
            // on plain ASCII surrounding text, so direct byte-as-char
            // is safe for ASCII; for any non-ASCII bytes we'd need to
            // reconstruct the UTF-8 boundary. Since `s` is `&str`
            // (already valid UTF-8) and we only skip whole ASCII SGR
            // sequences, the surviving bytes still form valid UTF-8.
            // Push the raw byte through `str::from_utf8` of a slice to
            // preserve multi-byte chars.
            // Find the next UTF-8 char boundary and copy that slice.
            let ch_len = utf8_char_len(bytes[i]);
            let end = (i + ch_len).min(bytes.len());
            // Safety: `s` is valid UTF-8 and `i` is on a char boundary
            // (we only ever advance by full `\x1b[...m` (ASCII) or by
            // `ch_len`). So `&s[i..end]` is a valid str slice.
            out.push_str(&s[i..end]);
            i = end;
        }
    }
    out
}

/// Length in bytes of the UTF-8 char starting at `b` (the leading byte).
/// Falls back to 1 for any byte that doesn't look like a valid leader,
/// so a malformed input degrades gracefully rather than panicking.
fn utf8_char_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b < 0xC0 {
        1 // continuation byte; treat as 1 to avoid stalling
    } else if b < 0xE0 {
        2
    } else if b < 0xF0 {
        3
    } else {
        4
    }
}
