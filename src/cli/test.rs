//! `silt test [--filter <pat>] [path]` — discover, compile, and run
//! `test_*` functions.

use std::fs;
use std::path::Path;
use std::process;
use std::sync::Arc;

use silt::errors::{ErrorKind, SourceError};
use silt::vm::Vm;

use crate::cli::help::test_usage_banner;
use crate::cli::module_sources::collect_module_function_sources;
use crate::cli::paths::find_silt_files;
use crate::cli::pipeline::{
    CompilePipelineResult, Emit, analyse_parsed_entry_file, parse_entry_file,
    pipeline_has_real_hard_errors, reportable_diagnostics, resolve_strict_effects,
};
use crate::cli::run::returned_err;
use crate::cli::source_scan::{TestKind, test_functions};

/// Dispatch `silt test [--filter <pat>] [--strict-effects] [path]`.
pub(crate) fn dispatch(args: &[String]) {
    let mut file: Option<String> = None;
    let mut filter: Option<String> = None;
    let mut strict_effects: Option<bool> = None;
    let mut i = 2;
    while i < args.len() {
        if args[i] == "--filter" {
            if i + 1 < args.len() {
                filter = Some(args[i + 1].clone());
                i += 2;
            } else {
                eprintln!("--filter requires a pattern");
                process::exit(1);
            }
        } else if let Some(value) = args[i].strip_prefix("--filter=") {
            // GNU-style `--filter=pat` form, to match `silt add --path=...`
            // and every other subcommand that accepts an `=`-joined value.
            // An empty value (`--filter=`) is a usage error — treat it
            // the same as `--filter` with no following argument.
            if value.is_empty() {
                eprintln!("--filter requires a pattern");
                process::exit(1);
            }
            filter = Some(value.to_string());
            i += 1;
        } else if args[i] == "--strict-effects" {
            strict_effects = Some(true);
            i += 1;
        } else if args[i] == "--help" || args[i] == "-h" {
            println!("Usage: {}", test_usage_banner());
            println!();
            println!("Options:");
            println!("  --filter <pat>      Only run tests whose name contains <pat>");
            println!("  --strict-effects    Treat unannotated fns as pure (Phase D)");
            println!("  --watch, -w         Re-run on file changes");
            println!();
            println!("Auto-discovery: when no file is given, recursively runs tests");
            println!("from files matching *_test.silt or *.test.silt.");
            process::exit(0);
        } else if args[i].starts_with('-') {
            // Unknown flag — don't silently treat as a filename.
            let suggestion = match args[i].as_str() {
                "--filters" | "-filter" | "-f" => " (did you mean --filter?)",
                "--h" | "-help" => " (did you mean --help?)",
                "--strict-effect" | "--strict_effects" => " (did you mean --strict-effects?)",
                _ => "",
            };
            eprintln!("silt test: unknown flag '{}'{}", args[i], suggestion);
            eprintln!("Run 'silt test --help' for usage.");
            process::exit(1);
        } else if file.is_none() {
            file = Some(args[i].clone());
            i += 1;
        } else {
            // Reject extra positionals — `silt test` takes at most one
            // path (a single file or a directory to scan). Pre-fix the
            // assign was unconditional and last-wins, so
            // `silt test a_test.silt b_test.silt` silently ran only
            // `b_test.silt` and reported green while `a_test.silt` never
            // ran — a CI hazard (a skipped test reads as passing).
            // Mirror the rejection pattern in `silt check`/`silt run`.
            eprintln!("silt test: unexpected extra argument '{}'", args[i]);
            eprintln!("silt test takes at most one path (a file or a directory to scan).");
            eprintln!("Run 'silt test --help' for usage.");
            process::exit(1);
        }
    }
    run_tests(file.as_deref(), filter, strict_effects);
}

/// Walk `dir` for files named `*_test.silt` or `*.test.silt`. Delegates
/// the recursive scan to `cli::paths::find_silt_files` so the
/// skip-list policy (`target/`, `.git/`, `node_modules/`,
/// `fuzz/corpus/…`) and the sort order live in exactly one place.
/// Pre-round-87, this function was a byte-identical clone of
/// `find_silt_files` *without* the skip filter, which meant `silt test`
/// would happily try to execute `.silt` files that had been deposited
/// under `target/` or vendored under `node_modules/`. Test-file suffix
/// filtering happens here so the canonical scanner stays
/// suffix-agnostic.
fn find_test_files(dir: &Path) -> Vec<String> {
    find_silt_files(dir)
        .into_iter()
        .filter(|name| name.ends_with("_test.silt") || name.ends_with(".test.silt"))
        .collect()
}

/// Print what the pipeline found in `path`, in the order and the form
/// `silt check` prints it, warnings included. Returns `true` when the
/// file failed to compile; a line that says so is printed last.
fn report_diagnostics(path: &str, result: &CompilePipelineResult) -> bool {
    let diagnostics = reportable_diagnostics(result);
    silt::errors::eprintln_errors_with_separator(&diagnostics);
    if !pipeline_has_real_hard_errors(result) && result.functions.is_some() {
        return false;
    }
    let mut kinds: Vec<&str> = Vec::new();
    for diagnostic in diagnostics.iter().filter(|d| !d.is_warning) {
        let kind = match diagnostic.kind {
            ErrorKind::Lex => "lex errors",
            ErrorKind::Parse => "parse errors",
            ErrorKind::Type => "type errors",
            ErrorKind::Compile => "compile errors",
            ErrorKind::Runtime => "runtime errors",
        };
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    if kinds.is_empty() {
        kinds.push("errors");
    }
    eprintln!(
        "{path}: failed to compile — {} (see above)",
        kinds.join(" and ")
    );
    true
}

fn run_tests(file: Option<&str>, filter: Option<String>, strict_effects_cli: Option<bool>) {
    silt::intern::reset();
    let paths: Vec<String> = if let Some(f) = file {
        let p = Path::new(f);
        if p.is_dir() {
            // silt test dir/ — find all test files in directory recursively
            find_test_files(p)
        } else {
            // silt test file.silt — single file
            vec![f.to_string()]
        }
    } else {
        // silt test — find all test files in current directory recursively
        find_test_files(Path::new("."))
    };

    if paths.is_empty() {
        println!("no test files found");
        return;
    }

    // Files that `--filter` did not rule out. Without a filter, all of them.
    let mut files_considered: usize = 0;
    let mut total = 0;
    let mut passed = 0;
    let mut failed = 0;
    let mut skipped = 0;
    // Count files that failed to lex / parse / type-check / compile.
    // These are tracked separately from the per-test failure counter so
    // that `X tests: Y passed, Z failed` still reflects what actually
    // ran. Previously a single file compile error was booked as one
    // "failed test", which was misleading — that file may have contained
    // dozens of tests we couldn't even count.
    let mut file_errors: usize = 0;

    for path in &paths {
        let source = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                // An unreadable file cannot be asked for its tests, so
                // `--filter` does not rule it out: the error is reported.
                files_considered += 1;
                eprintln!("{path}: failed to read — {e}");
                file_errors += 1;
                continue;
            }
        };

        let parsed = parse_entry_file(path.as_str(), source);

        // The tests of this file that `--filter` selects, in source
        // order. They are read from the parsed declarations, the same
        // ones that are compiled and run below, so what is selected and
        // what is run cannot differ.
        let tests: Vec<(String, TestKind)> = match &parsed.program {
            Some(program) => test_functions(program)
                .into_iter()
                .filter(|(name, _)| {
                    filter
                        .as_deref()
                        .is_none_or(|pattern| name.contains(pattern))
                })
                .collect(),
            None => Vec::new(),
        };
        // With a filter, a file without a selected test is left alone: it
        // is not compiled, and nothing is reported for it. A file that
        // does not lex cannot be asked for its tests either, so it is
        // kept and its error reported.
        if filter.is_some() && parsed.program.is_some() && tests.is_empty() {
            continue;
        }
        files_considered += 1;

        // Phase D: each test file may live in a different package
        // (autodiscovery walks the cwd recursively); resolve the
        // strict-effects flag per-file so a per-package
        // `[lints] strict-effects = true` honours its own boundary.
        // CLI flag (Some) wins over per-file manifest discovery.
        let strict_effects = resolve_strict_effects(path.as_str(), strict_effects_cli);

        // Typecheck and compile through the pipeline that `silt check`
        // and `silt run` use, with the options of `silt check`, so a test
        // file gets the diagnostics `silt check` gives it: the type
        // errors of the modules it imports, the compiler's warnings, the
        // static checks against declared dependencies. The one
        // difference is what is emitted: the declarations, without a
        // call of `main`.
        let result = analyse_parsed_entry_file(
            path.as_str(),
            parsed,
            Emit::Declarations,
            true,
            true,
            strict_effects,
        );
        let failed_to_compile = report_diagnostics(path.as_str(), &result);
        let (source, functions) = match (failed_to_compile, result.functions) {
            (false, Some(functions)) => (result.source, functions),
            _ => {
                file_errors += 1;
                continue;
            }
        };

        // Run the setup script to register all globals in the VM
        let Some(first) = functions.into_iter().next() else {
            eprintln!("{path}: internal error: no functions compiled");
            file_errors += 1;
            continue;
        };
        // Build module_sources BEFORE running the script so setup errors
        // from imported modules can render against the correct source file.
        let module_sources = collect_module_function_sources(path, &source);

        // G2 (audit round 21): normalize frame and error-header paths
        // for both setup errors and per-test errors.  Moved above the
        // vm.run() call so setup-error rendering can also benefit.
        //
        // Lock: tests/cli_test_rendering_tests.rs
        // `test_test_setup_error_paths_normalized`.
        //
        // Round-101: the normalization body lives in the shared
        // `crate::cli::paths::display_path_for` helper — `silt run`
        // (src/cli/run.rs) builds the same closure from it, so the two
        // subcommands can never drift. Lock:
        // tests/round101_display_path_helper_lock_tests.rs.
        let user_path_is_absolute = Path::new(path.as_str()).is_absolute();
        let cwd = std::env::current_dir().ok();
        let normalize_path = |candidate: &Path| -> String {
            crate::cli::paths::display_path_for(user_path_is_absolute, cwd.as_deref(), candidate)
        };

        let script = Arc::new(first);
        let mut vm = Vm::new();
        if let Err(e) = vm.run(script) {
            if let Some(span) = e.span {
                // Find the innermost frame that identifies a source file:
                // either a user function or a <module:X> init frame.
                let innermost_fn_name: Option<&str> = e
                    .call_stack
                    .iter()
                    .find(|(n, _)| !n.starts_with('<') || n.starts_with("<module:"))
                    .map(|(n, _)| n.as_str());
                let (err_source, err_path): (&str, String) =
                    match innermost_fn_name.and_then(|n| module_sources.get(n)) {
                        Some((module_path, module_source)) => {
                            (module_source.as_str(), normalize_path(module_path))
                        }
                        None => (source.as_str(), normalize_path(Path::new(path))),
                    };
                let source_err = SourceError::runtime_at(&e.message, span, err_source, &err_path);
                eprintln!("{path}: setup error:");
                eprintln!("{source_err}");
                let stack_lines =
                    silt::vm::error::render_call_stack(&e.call_stack, |frame_name, frame_span| {
                        let frame_path: String = match module_sources.get(frame_name) {
                            Some((p, _)) => normalize_path(p),
                            None => normalize_path(Path::new(path)),
                        };
                        if frame_span.line > 0 {
                            format!("{}:{}:{}", frame_path, frame_span.line, frame_span.col)
                        } else {
                            format!("{frame_path}:<unknown location>")
                        }
                    });
                if !stack_lines.is_empty() {
                    eprintln!("\ncall stack:");
                    for line in stack_lines {
                        eprintln!("{line}");
                    }
                }
            } else {
                // Span-less runtime error: funnel through
                // `SourceError::runtime_at` with a zero span so the
                // output carries the file path and color gating that a
                // bare `VmError` Display (plain text, no file) cannot
                // provide. The legacy internal Display prefix this once
                // guarded against is gone — `VmError::Display` now emits
                // the canonical `error[runtime]:` header itself (see
                // src/vm/error.rs).
                let source_err = SourceError::runtime_at(
                    &e.message,
                    silt::lexer::Span::new(0, 0),
                    &source,
                    path.as_str(),
                );
                eprintln!("{path}: setup error:");
                eprintln!("{source_err}");
            }
            file_errors += 1;
            continue;
        }

        // Run each selected test function
        for (name, kind) in &tests {
            total += 1;
            if *kind == TestKind::Skip {
                eprintln!("  SKIP {path}::{name}");
                skipped += 1;
                continue;
            }
            let caller = silt::bytecode::call_global_script(name);
            match vm.run(Arc::new(caller)) {
                Ok(value) => match returned_err(&value) {
                    // `Ok(..)`, Unit and every other value: the test ran
                    // to its end.
                    None => {
                        eprintln!("  PASS {path}::{name}");
                        passed += 1;
                    }
                    // `Err(..)`: the test gave up, typically at a `?`, and
                    // the assertions after that point never ran. Same
                    // rule as for `main` under `silt run`. There is no
                    // single source location for "the result was Err",
                    // hence the zero span.
                    Some(payload) => {
                        eprintln!("  FAIL {path}::{name}");
                        let source_err = SourceError::runtime_at(
                            format!("{name} returned Err: {payload}"),
                            silt::lexer::Span::new(0, 0),
                            &source,
                            path.as_str(),
                        );
                        let formatted = format!("{source_err}");
                        for line in formatted.lines() {
                            eprintln!("    {line}");
                        }
                        failed += 1;
                    }
                },
                Err(e) => {
                    eprintln!("  FAIL {path}::{name}");
                    if let Some(span) = e.span {
                        // Determine which source text & file path
                        // to render against, mirroring `silt run`.
                        let innermost_fn_name: Option<&str> = e
                            .call_stack
                            .iter()
                            .find(|(n, _)| !n.starts_with('<') || n.starts_with("<module:"))
                            .map(|(n, _)| n.as_str());
                        let (err_source, err_path): (&str, String) =
                            match innermost_fn_name.and_then(|n| module_sources.get(n)) {
                                Some((module_path, module_source)) => {
                                    (module_source.as_str(), normalize_path(module_path))
                                }
                                None => (source.as_str(), path.to_string()),
                            };
                        let source_err =
                            SourceError::runtime_at(&e.message, span, err_source, &err_path);
                        // Indent every line of the formatted error
                        // so multi-line SourceErrors stay aligned
                        // with the FAIL header.
                        let formatted = format!("{source_err}");
                        for line in formatted.lines() {
                            eprintln!("    {line}");
                        }
                        // Mirror `silt run`: render a call stack
                        // when the error crosses ≥2 meaningful
                        // frames. Without this, a test that fails
                        // deep inside a helper chain only prints
                        // the innermost site, leaving the user
                        // without any trail back to the test
                        // function that invoked it.
                        let stack_lines = silt::vm::error::render_call_stack(
                            &e.call_stack,
                            |frame_name, frame_span| {
                                // Use module path if the frame
                                // belongs to an imported module,
                                // then normalize to match user's
                                // path style (relative/absolute).
                                let frame_path: String = match module_sources.get(frame_name) {
                                    Some((p, _)) => normalize_path(p),
                                    None => path.to_string(),
                                };
                                if frame_span.line > 0 {
                                    format!("{}:{}:{}", frame_path, frame_span.line, frame_span.col)
                                } else {
                                    format!("{frame_path}:<unknown location>")
                                }
                            },
                        );
                        if !stack_lines.is_empty() {
                            eprintln!("\n    call stack:");
                            for line in stack_lines {
                                eprintln!("    {line}");
                            }
                        }
                    } else {
                        // Span-less runtime error: render via
                        // `SourceError::runtime_at` with a zero
                        // span (adding the file path and color
                        // gating a bare `VmError` Display lacks)
                        // and indent to match the FAIL header's
                        // alignment.
                        let source_err = SourceError::runtime_at(
                            &e.message,
                            silt::lexer::Span::new(0, 0),
                            &source,
                            path.as_str(),
                        );
                        let formatted = format!("{source_err}");
                        for line in formatted.lines() {
                            eprintln!("    {line}");
                        }
                    }
                    failed += 1;
                }
            }
            // Failures of tasks this test spawned and never joined are
            // reported under the test's result line.
            vm.report_unjoined_task_failures();
        }
    }

    // `--filter` ruled every file out: say so instead of printing a
    // summary of nothing.
    if files_considered == 0 {
        println!("no matching test files found");
        return;
    }

    let test_word = if total == 1 { "test" } else { "tests" };
    if file_errors > 0 {
        eprintln!(
            "\n{total} {test_word}: {passed} passed, {failed} failed, {skipped} skipped ({file_errors} file{} failed to compile)",
            if file_errors == 1 { "" } else { "s" }
        );
    } else {
        eprintln!("\n{total} {test_word}: {passed} passed, {failed} failed, {skipped} skipped");
    }
    if total == 0 && file_errors == 0 {
        eprintln!(
            "hint: test functions must be named 'fn test_*'; test files should end with '_test.silt'"
        );
    }
    if failed > 0 || file_errors > 0 {
        process::exit(1);
    }
}
