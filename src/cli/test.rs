//! `silt test [--filter <pat>] [path]` — discover, compile, and run
//! `test_*` functions.

use std::fs;
use std::path::Path;
use std::process;
use std::sync::Arc;

use silt::compiler::Compiler;
use silt::errors::SourceError;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::vm::Vm;

use crate::cli::help::test_usage_banner;
use crate::cli::module_sources::collect_module_function_sources;
use crate::cli::package::package_setup_for_file;
use crate::cli::pipeline::is_unknown_module_warning;

/// Dispatch `silt test [--filter <pat>] [path]`.
pub(crate) fn dispatch(args: &[String]) {
    let mut file: Option<String> = None;
    let mut filter: Option<String> = None;
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
        } else if args[i] == "--help" || args[i] == "-h" {
            println!("Usage: {}", test_usage_banner());
            println!();
            println!("Options:");
            println!("  --filter <pat>   Only run tests whose name contains <pat>");
            println!("  --watch, -w      Re-run on file changes");
            println!();
            println!("Auto-discovery: when no file is given, recursively runs tests");
            println!("from files matching *_test.silt or *.test.silt.");
            process::exit(0);
        } else if args[i].starts_with('-') {
            // Unknown flag — don't silently treat as a filename.
            let suggestion = match args[i].as_str() {
                "--filters" | "-filter" | "-f" => " (did you mean --filter?)",
                "--h" | "-help" => " (did you mean --help?)",
                _ => "",
            };
            eprintln!("silt test: unknown flag '{}'{}", args[i], suggestion);
            eprintln!("Run 'silt test --help' for usage.");
            process::exit(1);
        } else {
            file = Some(args[i].clone());
            i += 1;
        }
    }
    run_tests(file.as_deref(), filter);
}

fn find_test_files(dir: &Path) -> Vec<String> {
    let mut results = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return results;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            results.extend(find_test_files(&path));
        } else {
            let name = path.to_string_lossy().to_string();
            if name.ends_with("_test.silt") || name.ends_with(".test.silt") {
                results.push(name);
            }
        }
    }
    results.sort();
    results
}

fn run_tests(file: Option<&str>, filter: Option<String>) {
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

    // When a filter is provided, skip files that can't possibly contain matching tests.
    // We do a quick text scan for `fn test_` / `fn skip_test_` names rather than a full parse.
    let paths: Vec<String> = if let Some(ref filter) = filter {
        paths
            .into_iter()
            .filter(|path| {
                let source = match fs::read_to_string(path) {
                    Ok(s) => s,
                    Err(_) => return true, // keep the file so the error is reported later
                };
                // Scan for function names like `fn test_...` or `fn skip_test_...`
                // (including `pub fn` variants).
                for line in source.lines() {
                    let trimmed = line.trim_start();
                    let rest = if let Some(r) = trimmed.strip_prefix("pub fn ") {
                        Some(r)
                    } else {
                        trimmed.strip_prefix("fn ")
                    };
                    if let Some(rest) = rest {
                        let name: String = rest
                            .chars()
                            .take_while(|c| c.is_alphanumeric() || *c == '_')
                            .collect();
                        if (name.starts_with("test_") || name.starts_with("skip_test_"))
                            && name.contains(filter.as_str())
                        {
                            return true;
                        }
                    }
                }
                false
            })
            .collect()
    } else {
        paths
    };

    if paths.is_empty() {
        println!("no matching test files found");
        return;
    }

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
                eprintln!("{path}: failed to read — {e}");
                file_errors += 1;
                continue;
            }
        };

        let tokens = match Lexer::new(&source).tokenize() {
            Ok(t) => t,
            Err(e) => {
                let source_err = SourceError::from_lex_error(&e, &source, path.as_str());
                eprintln!("{path}: failed to compile — {source_err}");
                file_errors += 1;
                continue;
            }
        };

        let (mut program, parse_errors) = Parser::new(tokens).parse_program_recovering();
        if !parse_errors.is_empty() {
            eprintln!("{path}: failed to compile — parse errors:");
            for (i, e) in parse_errors.iter().enumerate() {
                if i > 0 {
                    eprintln!();
                }
                let source_err = SourceError::from_parse_error(e, &source, path.as_str());
                eprintln!("{source_err}");
            }
            file_errors += 1;
            continue;
        }

        // Type-check before compiling so type errors fail the test.
        // Drop "unknown module" warnings for imports the compiler resolves
        // later (see `reportable_type_errors` / `is_unknown_module_warning`):
        // every test file that imports a sibling module would otherwise
        // flood test output with a spurious warning even on clean runs.
        // Matches `silt run`'s behavior exactly. Real missing modules are
        // still caught by the compiler's own "cannot load module" error
        // in the block below.
        //
        // Resolve the local package up front so the typechecker can
        // enforce the trait-orphan rule for `silt test` (round 63 item
        // 5). The compile path below reuses the same setup.
        let (local_pkg, package_roots) = package_setup_for_file(path.as_str(), true);
        // Round 64 item 6A: pre-compile-typecheck imports through the
        // compiler so the entrypoint typecheck can see sibling
        // modules' exports (cross-module let-generalization). The
        // compiler is reused below for `compile_declarations` so the
        // pre-typecheck typecheck doubles as the import-load
        // (compile_file_module's caching avoids re-reading sources).
        let mut compiler = Compiler::with_package_roots(local_pkg, package_roots);
        compiler.pre_typecheck_imports(&program);
        let exports = compiler.module_exports_snapshot();
        let (type_errors, _entry_exports) = typechecker::check_with_package_and_imports(
            &mut program,
            Some(local_pkg),
            exports,
        );
        let mut has_type_error = false;
        let mut printed_type_errors: usize = 0;
        for te in &type_errors {
            let source_err = SourceError::from_type_error(te, &source, path);
            if is_unknown_module_warning(&source_err) {
                continue;
            }
            if printed_type_errors > 0 {
                eprintln!();
            }
            eprintln!("{source_err}");
            printed_type_errors += 1;
            if te.severity == typechecker::Severity::Error {
                has_type_error = true;
            }
        }
        if has_type_error {
            eprintln!("{path}: failed to compile — type errors (see above)");
            file_errors += 1;
            continue;
        }

        // Compile all declarations (without calling main).  Package
        // setup mirrors `silt run`: prefer the nearest enclosing
        // `silt.toml` so cross-file `import foo` resolves consistently
        // regardless of which file `silt test` was pointed at, and
        // auto-update the lockfile when the manifest has new deps.
        // (The compiler instance is the one constructed above —
        // pre_typecheck_imports populated its module_exports cache
        // already; the compile pass below reuses that work.)
        let functions = match compiler.compile_declarations(&program) {
            Ok(f) => f,
            Err(e) => {
                let source_err = SourceError::from_compile_error(&e, &source, path);
                eprintln!("{path}: failed to compile — {source_err}");
                // Round-52: when the primary error originated from a
                // broken imported module, the recovery parser will have
                // collected additional module parse errors that the CLI
                // should surface too so users can fix them all in one go.
                for extra in compiler.module_parse_errors() {
                    let extra_err = SourceError::from_compile_error(extra, &source, path);
                    eprintln!("{path}: failed to compile — {extra_err}");
                }
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
        let user_path_is_absolute = Path::new(path.as_str()).is_absolute();
        let cwd = std::env::current_dir().ok();
        let normalize_path = |candidate: &Path| -> String {
            if user_path_is_absolute {
                if candidate.is_absolute() {
                    candidate.display().to_string()
                } else if let Some(ref cwd) = cwd {
                    cwd.join(candidate).display().to_string()
                } else {
                    candidate.display().to_string()
                }
            } else {
                if let Some(ref cwd) = cwd {
                    match candidate.strip_prefix(cwd) {
                        Ok(rel) => rel.display().to_string(),
                        Err(_) => candidate.display().to_string(),
                    }
                } else {
                    candidate.display().to_string()
                }
            }
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
                // Span-less runtime error: avoid leaking the bare
                // "VM error:" prefix from `VmError::Display`. Funnel
                // through `SourceError::runtime_at` with a zero span so
                // the output renders with the canonical `error[runtime]:`
                // header, matching every other diagnostic.
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

        // Run each test function
        for decl in &program.decls {
            if let silt::ast::Decl::Fn(f) = decl {
                let name = silt::intern::resolve(f.name);
                if name.starts_with("skip_test_") {
                    if let Some(ref filter) = filter
                        && !name.contains(filter.as_str())
                    {
                        continue;
                    }
                    total += 1;
                    eprintln!("  SKIP {path}::{name}");
                    skipped += 1;
                    continue;
                }
                if name.starts_with("test_") {
                    if let Some(ref filter) = filter
                        && !name.contains(filter.as_str())
                    {
                        continue;
                    }
                    total += 1;
                    let caller = silt::bytecode::call_global_script(&name);
                    match vm.run(Arc::new(caller)) {
                        Ok(_) => {
                            eprintln!("  PASS {path}::{name}");
                            passed += 1;
                        }
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
                                let source_err = SourceError::runtime_at(
                                    &e.message, span, err_source, &err_path,
                                );
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
                                        let frame_path: String =
                                            match module_sources.get(frame_name) {
                                                Some((p, _)) => normalize_path(p),
                                                None => path.to_string(),
                                            };
                                        if frame_span.line > 0 {
                                            format!(
                                                "{}:{}:{}",
                                                frame_path, frame_span.line, frame_span.col
                                            )
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
                                // Span-less runtime error: avoid leaking
                                // the bare "VM error:" prefix from
                                // `VmError::Display`. Render via
                                // `SourceError::runtime_at` with a zero
                                // span and indent to match the FAIL
                                // header's alignment.
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
                }
            }
        }
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
