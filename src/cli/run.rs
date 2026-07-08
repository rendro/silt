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
use crate::cli::pipeline::{compile_file_with_options, resolve_strict_effects};
use crate::cli::source_scan::{is_missing_main_error, looks_like_test_file};

/// Dispatch `silt run [--disassemble] [--strict-effects] [<file>] [-- <program-args>...]`.
pub(crate) fn dispatch(args: &[String]) {
    if args[2..].iter().any(|a| a == "--help" || a == "-h") {
        print!("{}", run_help_text());
        process::exit(0);
    }
    let mut disasm = false;
    let mut file: Option<String> = None;
    let mut strict_effects: Option<bool> = None;
    // Round-74: positionals after `--` are forwarded to the running
    // program (surfaced via `io.args()`), not interpreted as silt CLI
    // flags / files. This restores the ability to pass user args to
    // scripts (e.g. `silt run text_stats.silt -- input.txt`) which
    // round-72's bare-extra-positional rejection had broken — there
    // was no other channel to reach `io.args()`.
    let mut program_args: Vec<String> = Vec::new();
    let mut iter = args[2..].iter();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            // Everything past the separator is verbatim program args.
            for rest in iter.by_ref() {
                program_args.push(rest.clone());
            }
            break;
        } else if arg == "--disassemble" {
            disasm = true;
        } else if arg == "--strict-effects" {
            strict_effects = Some(true);
        } else if arg.starts_with('-') {
            let suggestion = match arg.as_str() {
                "--disasm" | "--disassembly" | "-d" => " (did you mean --disassemble?)",
                "--h" | "-help" => " (did you mean --help?)",
                "--strict-effect" | "--strict_effects" => " (did you mean --strict-effects?)",
                _ => "",
            };
            eprintln!("silt run: unknown flag '{arg}'{suggestion}");
            eprintln!("Run 'silt run --help' for usage.");
            process::exit(1);
        } else if file.is_none() {
            file = Some(arg.clone());
        } else {
            // Reject extra positionals — `silt run` takes at most one
            // file. Pre-fix the loop silently dropped subsequent
            // positionals (only the first won), which made
            // `silt run a.silt b.silt` look like it had run both files.
            // Mirror the rejection pattern used by `silt update`,
            // `silt repl`, `silt lsp`, and `silt add`.
            //
            // Round-74: to forward args to the running program, use
            // `--` as a separator (`silt run a.silt -- foo bar`).
            eprintln!("silt run: unexpected extra argument '{arg}'");
            eprintln!(
                "If '{arg}' is meant for the program, separate it with '--' (e.g. 'silt run <file>.silt -- {arg}')."
            );
            eprintln!("Run 'silt run --help' for usage.");
            process::exit(1);
        }
    }
    // Publish the forwarded args before compile/run so `io.args()`
    // sees them. Always set (even when empty) so a stale snapshot
    // from a prior in-process invocation doesn't leak through.
    silt::builtins::io::set_program_args(program_args);
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
    let strict = resolve_strict_effects(&file, strict_effects);
    if disasm {
        crate::cli::disasm::disasm_file(&file);
    } else {
        vm_run_file(&file, strict);
    }
}

/// Legacy `silt <file>.silt [--help|--disassemble|--strict-effects]`
/// convenience shim — same behavior as `silt run` with the file baked
/// in as the first argument.
pub(crate) fn dispatch_bare_file(args: &[String], file: &str) {
    let mut disasm = false;
    let mut strict_effects: Option<bool> = None;
    // Round-74: same `--` forwarding as the explicit `silt run` form.
    let mut program_args: Vec<String> = Vec::new();
    let mut iter = args[2..].iter();
    while let Some(extra) = iter.next() {
        if extra == "--" {
            for rest in iter.by_ref() {
                program_args.push(rest.clone());
            }
            break;
        } else if extra == "--help" || extra == "-h" {
            print!("{}", run_help_text());
            process::exit(0);
        } else if extra == "--disassemble" {
            disasm = true;
        } else if extra == "--strict-effects" {
            strict_effects = Some(true);
        } else if extra.starts_with('-') {
            let suggestion = match extra.as_str() {
                "--disasm" | "--disassembly" | "-d" => " (did you mean --disassemble?)",
                "--h" | "-help" => " (did you mean --help?)",
                "--strict-effect" | "--strict_effects" => " (did you mean --strict-effects?)",
                _ => "",
            };
            eprintln!("silt run: unknown flag '{extra}'{suggestion}");
            eprintln!("Run 'silt run --help' for usage.");
            process::exit(1);
        } else {
            // Reject extra positionals on the bare-file shim — the
            // file is already pinned by the dispatcher to args[1], so
            // any non-flag positional here is a user mistake. Pre-fix
            // the loop ignored these silently, so `silt foo.silt
            // bar.silt` looked like it had run both files. Mirror the
            // rejection pattern used by `silt update`, `silt repl`,
            // `silt lsp`, and `silt add`.
            //
            // Round-74: to forward args to the running program, use
            // `--` as a separator (`silt foo.silt -- arg1 arg2`).
            eprintln!("silt run: unexpected extra argument '{extra}'");
            eprintln!(
                "If '{extra}' is meant for the program, separate it with '--' (e.g. 'silt {file} -- {extra}')."
            );
            eprintln!("Run 'silt run --help' for usage.");
            process::exit(1);
        }
    }
    silt::builtins::io::set_program_args(program_args);
    let strict = resolve_strict_effects(file, strict_effects);
    if disasm {
        crate::cli::disasm::disasm_file(file);
    } else {
        vm_run_file(file, strict);
    }
}

/// Run a file using the bytecode VM (default path).
pub(crate) fn vm_run_file(path: &str, strict_effects: bool) {
    silt::intern::reset();
    let (functions, source) = compile_file_with_options(path, true, strict_effects);

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
    let run_result = vm.run(script);
    // Round-93: a `fn main() -> Result(..)` that evaluates to `Err(..)`
    // is a failed program — surface it. Previously the Ok value of
    // `vm.run` (main's return value) was discarded wholesale, so
    // `fn main() -> Result(Int, String) { Err("boom") }` (or a `?`
    // propagating an Err out of main) exited 0 with no diagnostic and
    // CI/shell callers saw success on failure. Render through the
    // canonical `error[runtime]:` header (zero span — there is no
    // single source location for "main's result was Err") and exit 1,
    // matching the exit code every other runtime error uses.
    // `Ok(..)` and non-Result returns (Unit, Int, ...) are unchanged.
    // Only the `silt run` surface goes through here — `silt test` and
    // the REPL have their own `vm.run` handling.
    if let Ok(silt::Value::Variant(tag, fields)) = &run_result {
        if tag.as_str() == "Err" {
            // Result's Err carries exactly one payload; render it via
            // the VM's Display machinery (stdlib error variants print
            // their `.message()`, strings print bare). Fall back to
            // the whole variant for defensive completeness.
            let payload = match fields.as_slice() {
                [single] => single.to_string(),
                _ => silt::Value::Variant(tag.clone(), fields.clone()).to_string(),
            };
            let source_err = SourceError::runtime_at(
                format!("main returned Err: {payload}"),
                silt::lexer::Span::new(0, 0),
                &source,
                path,
            );
            eprintln!("{source_err}");
            process::exit(1);
        }
    }
    if let Err(e) = run_result {
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
            //
            // Round-101: the normalization body lives in the shared
            // `crate::cli::paths::display_path_for` helper — `silt test`
            // (src/cli/test.rs) builds the same closure from it, so the
            // two subcommands can never drift. Lock:
            // tests/round101_display_path_helper_lock_tests.rs.
            let user_path_is_absolute = Path::new(path).is_absolute();
            let cwd = std::env::current_dir().ok();
            let normalize_path = |candidate: &Path| -> String {
                crate::cli::paths::display_path_for(
                    user_path_is_absolute,
                    cwd.as_deref(),
                    candidate,
                )
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
            //
            // Round-73 G1: delegate filter+truncation to the shared
            // `render_call_stack` helper so `silt run` and `silt test` can
            // never drift again. The helper applies the exact same
            // `<script>` / `<call:...>` drop + `<module:...>` keep
            // policy, plus the same head/tail truncation.
            let stack_lines =
                silt::vm::error::render_call_stack(&e.call_stack, |name, frame_span| {
                    // Each frame uses its own function's source file for
                    // file labels — this matters when the call crosses a
                    // module boundary.
                    let frame_path: String = match module_sources.get(name) {
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
            // Span-less runtime error: funnel through
            // `SourceError::runtime_at` with a zero span so the output
            // carries the file path and the ANSI color gating every other
            // diagnostic gets — a bare `VmError` Display is plain text
            // with no file to point at. (Round-36 originally added this
            // to route around a legacy internal Display prefix; that
            // Display has since been canonicalized to the
            // `error[runtime]:` header itself — see src/vm/error.rs — so
            // the prefix concern is historical.)
            let source_err =
                SourceError::runtime_at(&e.message, silt::lexer::Span::new(0, 0), &source, path);
            eprintln!("{source_err}");
        }
        process::exit(1);
    }
}
