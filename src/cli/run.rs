//! `silt run [--disassemble] [<file>]` — compile and execute a silt
//! program with the bytecode VM. Also backs the bare `silt
//! <file>.silt` convenience shim.

use std::path::Path;
use std::process;
use std::sync::Arc;

use silt::errors::SourceError;
use silt::vm::Vm;

use crate::cli::help::{run_help_text, run_usage_banner};
use crate::cli::module_sources::collect_module_function_sources;
use crate::cli::package::resolve_package_entry_point;
use crate::cli::pipeline::compile_file;
use crate::cli::source_scan::{is_missing_main_error, looks_like_test_file};

/// Dispatch `silt run [--disassemble] [<file>]`.
pub(crate) fn dispatch(args: &[String]) {
    if args[2..].iter().any(|a| a == "--help" || a == "-h") {
        print!("{}", run_help_text());
        process::exit(0);
    }
    let mut disasm = false;
    let mut file: Option<String> = None;
    for arg in &args[2..] {
        if arg == "--disassemble" {
            disasm = true;
        } else if arg.starts_with('-') {
            let suggestion = match arg.as_str() {
                "--disasm" | "--disassembly" | "-d" => " (did you mean --disassemble?)",
                "--h" | "-help" => " (did you mean --help?)",
                _ => "",
            };
            eprintln!("silt run: unknown flag '{arg}'{suggestion}");
            eprintln!("Run 'silt run --help' for usage.");
            process::exit(1);
        } else if file.is_none() {
            file = Some(arg.clone());
        }
    }
    // No explicit file → look for an enclosing silt package and use
    // its `src/main.silt`. If we're not inside a package, preserve
    // the legacy "missing argument" error so non-package users
    // see a familiar message.
    let file = match file {
        Some(f) => f,
        None => match resolve_package_entry_point() {
            Ok(Some(p)) => p.to_string_lossy().into_owned(),
            Ok(None) => {
                eprintln!("Usage: {}", run_usage_banner());
                process::exit(1);
            }
            Err(()) => process::exit(1),
        },
    };
    if disasm {
        crate::cli::disasm::disasm_file(&file);
    } else {
        vm_run_file(&file);
    }
}

/// Legacy `silt <file>.silt [--help|--disassemble]` convenience shim —
/// same behavior as `silt run` with the file baked in as the first
/// argument.
pub(crate) fn dispatch_bare_file(args: &[String], file: &str) {
    let mut disasm = false;
    for extra in &args[2..] {
        if extra == "--help" || extra == "-h" {
            print!("{}", run_help_text());
            process::exit(0);
        } else if extra == "--disassemble" {
            disasm = true;
        } else if extra.starts_with('-') {
            let suggestion = match extra.as_str() {
                "--disasm" | "--disassembly" | "-d" => " (did you mean --disassemble?)",
                "--h" | "-help" => " (did you mean --help?)",
                _ => "",
            };
            eprintln!("silt run: unknown flag '{extra}'{suggestion}");
            eprintln!("Run 'silt run --help' for usage.");
            process::exit(1);
        }
    }
    if disasm {
        crate::cli::disasm::disasm_file(file);
    } else {
        vm_run_file(file);
    }
}

/// Run a file using the bytecode VM (default path).
pub(crate) fn vm_run_file(path: &str) {
    silt::intern::reset();
    let (functions, source) = compile_file(path);

    // Build a name → (module_file, source) map so runtime errors from
    // imported modules are rendered against the correct file.  See
    // `collect_module_function_sources` for the rationale.
    let module_sources = collect_module_function_sources(path, &source);

    let Some(script) = functions.into_iter().next() else {
        eprintln!("{path}: internal error: empty function list");
        process::exit(1);
    };
    let script = Arc::new(script);

    // Run via VM
    let mut vm = Vm::new();
    if let Err(e) = vm.run(script) {
        if let Some(span) = e.span {
            // F13 (audit round 17) + G1 (audit round 21): normalize
            // frame and error-header paths so they all use the same
            // style the user typed on the command line.  Moved above
            // the SourceError construction so the `-->` line also
            // benefits from normalization, not just the call-stack
            // frames.
            //
            // Lock: tests/cli_test_rendering_tests.rs
            // `test_cross_module_call_stack_uses_consistent_path_style`
            // `test_run_module_error_paths_consistently_normalized`.
            let user_path_is_absolute = Path::new(path).is_absolute();
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

            // Determine which source text & file path to render against.
            // Prefer the innermost non-synthetic frame's function name,
            // falling back to the main file when the frame isn't from an
            // imported module.
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
            eprintln!("{source_err}");
            // Print call stack if there are user frames beyond the error site.
            // Drop synthetic entry-point frames (<script>, <call:...>) by name
            // rather than by span — a zero-spanned frame inside an otherwise
            // good stack shouldn't cause the whole stack to be discarded.
            // Keep <module:...> frames for module-aware path resolution.
            let meaningful: Vec<_> = e
                .call_stack
                .iter()
                .filter(|(name, _)| !name.starts_with('<') || name.starts_with("<module:"))
                .collect();
            // Only show the stack if it adds information beyond the error
            // site the user already sees above. A single-frame "stack"
            // would just restate that location, which is noisy.
            let any_real_span = meaningful.iter().any(|(_, s)| s.line > 0);
            if meaningful.len() >= 2 && any_real_span {
                eprintln!("\ncall stack:");
                let head = 10;
                let tail = 5;
                let print_frame = |name: &str, frame_span: &silt::lexer::Span| {
                    // Each frame uses its own function's source file for
                    // file labels — this matters when the call crosses a
                    // module boundary.
                    let frame_path: String = match module_sources.get(name) {
                        Some((p, _)) => normalize_path(p),
                        None => normalize_path(Path::new(path)),
                    };
                    if frame_span.line > 0 {
                        eprintln!(
                            "  -> {}  at {}:{}:{}",
                            name, frame_path, frame_span.line, frame_span.col
                        );
                    } else {
                        eprintln!("  -> {name}  at {frame_path}:<unknown location>");
                    }
                };
                if meaningful.len() <= head + tail {
                    for (name, frame_span) in &meaningful {
                        print_frame(name, frame_span);
                    }
                } else {
                    for (name, frame_span) in &meaningful[..head] {
                        print_frame(name, frame_span);
                    }
                    let omitted = meaningful.len() - head - tail;
                    eprintln!("  ... ({omitted} more frames)");
                    for (name, frame_span) in &meaningful[meaningful.len() - tail..] {
                        print_frame(name, frame_span);
                    }
                }
            }
        } else if is_missing_main_error(&e) {
            // Round-24 B-fix: wrap the missing-main message in a real
            // SourceError so it renders with the canonical
            // `error[compile]:` header consistent with every other
            // file-level diagnostic. Previously this was a plain
            // `eprintln!` with no header / no `-->` locator — the only
            // diagnostic in the codebase that broke the rustc-style
            // shape. Lock: tests/empty_program_diagnostic_tests.rs.
            //
            // We use Span::new(0, 0) because there's no source location
            // for "the file has no main()" — the Display impl omits the
            // `-->` line when span.line == 0 but still emits the header.
            //
            // Detect test-only files so we can nudge the user toward
            // `silt test` instead of the generic "add a main()" error.
            // The body line below the header is rendered as a `= note:`
            // continuation, matching the multi-line message convention.
            let msg = if looks_like_test_file(&source) {
                format!(
                    "program has no main() function\nThis looks like a test file — run it with 'silt test {path}' instead."
                )
            } else {
                "program has no main() function\nadd one as the entry point".to_string()
            };
            let source_err =
                SourceError::compile_error_at(msg, silt::lexer::Span::new(0, 0), &source, path);
            eprintln!("{source_err}");
        } else {
            // Span-less runtime error: `VmError::Display` starts with the
            // bare "VM error:" prefix, which leaks that internal label to
            // users. Round-36: funnel through `SourceError::runtime_at`
            // with a zero span so output renders with the canonical
            // `error[runtime]:` header and never contains "VM error:".
            let source_err =
                SourceError::runtime_at(&e.message, silt::lexer::Span::new(0, 0), &source, path);
            eprintln!("{source_err}");
        }
        process::exit(1);
    }
}
