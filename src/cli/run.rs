//! `silt run [--disassemble] [<file>]` — compile and execute a silt
//! program with the bytecode VM. Also backs the bare `silt
//! <file>.silt` convenience shim.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;

use silt::errors::SourceError;
use silt::vm::{Vm, VmError};

use crate::cli::help::{run_help_text, run_usage_banner};
use crate::cli::module_sources::collect_module_function_sources;
use crate::cli::package::resolve_package_entry_point;
use crate::cli::pipeline::{CompiledFile, compile_file, resolve_strict_effects};
use crate::cli::source_scan::{missing_main_error, program_has_main};

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

/// The payload of `value` when it is the `Err(..)` of a `Result`, rendered
/// for a diagnostic; `None` for every other value.
///
/// A `main` or a test function that returns `Err(..)` has failed. This is
/// the one place that says what "returned `Err`" means, for `silt run`
/// and `silt test` alike.
pub(crate) fn returned_err(value: &silt::Value) -> Option<String> {
    let silt::Value::Variant(tag, fields) = value else {
        return None;
    };
    if tag.as_str() != "Err" {
        return None;
    }
    // Result's Err carries exactly one payload; render it via the VM's
    // Display machinery (stdlib error variants print their `.message()`,
    // strings print bare). Fall back to the whole variant for defensive
    // completeness.
    Some(match fields.as_slice() {
        [single] => single.to_string(),
        _ => value.to_string(),
    })
}

/// Run a file using the bytecode VM (default path).
pub(crate) fn vm_run_file(path: &str, strict_effects: bool) {
    silt::intern::reset();
    let CompiledFile {
        functions,
        source,
        program,
    } = compile_file(path, true, strict_effects);

    // The script ends with a call of the global `main`. Whether there is
    // one is known from the declarations, so a program without it is
    // rejected here, before any of it runs, with the diagnostic
    // `silt check` gives. A test file gets a pointer to `silt test`.
    if !program_has_main(&program) {
        eprintln!("{}", missing_main_error(&program, &source, path, true));
        process::exit(1);
    }

    // Build a name → (module_file, source) map so runtime errors from
    // imported modules are rendered against the correct file.  See
    // `collect_module_function_sources` for the rationale.
    let module_sources = collect_module_function_sources(path, &source);

    let Some(script) = functions.into_iter().next() else {
        eprintln!("{path}: internal error: empty function list");
        process::exit(1);
    };
    let script = Arc::new(script);

    // Run via VM. The failures of tasks that nobody joins are taken and
    // reported here, against the program's files, instead of by the
    // scheduler.
    silt::scheduler::collect_unjoined_failures();
    let mut vm = Vm::new();
    let run_result = vm.run(script);
    // The program has ended. The tasks that failed by now and that
    // nobody joined or cancelled are reported, and make the run fail. A
    // task that is still running is not a failure; if it fails later,
    // nobody takes its failure and it is not reported.
    let tasks_failed = report_task_failures(path, &source, &module_sources);
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
    // `silt test` applies the same rule to a test function, through the
    // same `returned_err`; the REPL has its own `vm.run` handling.
    if let Ok(value) = &run_result
        && let Some(payload) = returned_err(value)
    {
        let source_err = SourceError::runtime_at(
            format!("main returned Err: {payload}"),
            silt::lexer::Span::new(0, 0),
            &source,
            path,
        );
        eprintln!("{source_err}");
        process::exit(1);
    }
    if let Err(e) = run_result {
        eprintln!(
            "{}",
            render_runtime_error(&e, path, &source, &module_sources)
        );
        process::exit(1);
    }
    if tasks_failed {
        process::exit(1);
    }
}

/// Report on stderr the failures of tasks that nobody joined or
/// cancelled and that have happened so far, each rendered like any other
/// runtime error of the program at `path`. Returns true if there was
/// one.
fn report_task_failures(
    path: &str,
    source: &str,
    module_sources: &HashMap<String, (PathBuf, String)>,
) -> bool {
    let failures = silt::scheduler::take_unjoined_failures();
    for failure in &failures.failures {
        eprintln!(
            "{}",
            render_runtime_error(&failure.report_error(), path, source, module_sources)
        );
    }
    let not_kept: usize = failures.not_kept.iter().map(|(_, count)| count).sum();
    if not_kept > 0 {
        let source_err = SourceError::runtime_at(
            silt::scheduler::UnjoinedFailures::not_kept_message(not_kept),
            silt::lexer::Span::new(0, 0),
            source,
            path,
        );
        eprintln!("{source_err}");
    }
    !failures.is_empty()
}

/// Render a runtime error of the program at `path`, the way `silt run`
/// shows it: the header, the location with its source line, in the file
/// of the innermost frame (the program's own file, or the imported module
/// the frame belongs to), then the call stack when it has more than one
/// meaningful frame. `module_sources` maps function names to the file
/// and source of the module they come from
/// (`collect_module_function_sources`). `silt test` renders the failures
/// of tasks with it as well.
pub(crate) fn render_runtime_error(
    e: &VmError,
    path: &str,
    source: &str,
    module_sources: &HashMap<String, (PathBuf, String)>,
) -> String {
    let Some(span) = e.span else {
        // Span-less runtime error: funnel through
        // `SourceError::runtime_at` with a zero span so the output
        // carries the file path and the ANSI color gating every other
        // diagnostic gets — a bare `VmError` Display is plain text
        // with no file to point at. (Round-36 originally added this
        // to route around a legacy internal Display prefix; that
        // Display has since been canonicalized to the
        // `error[runtime]:` header itself — see src/vm/error.rs — so
        // the prefix concern is historical.)
        return SourceError::runtime_at(&e.message, silt::lexer::Span::new(0, 0), source, path)
            .to_string();
    };
    // F13 (audit round 17) + G1 (audit round 21): normalize
    // frame and error-header paths so they all use the same
    // style the user typed on the command line, the `-->` line
    // included.
    //
    // Lock: tests/cli/cli_test_rendering_tests.rs
    // `test_cross_module_call_stack_uses_consistent_path_style`
    // `test_run_module_error_paths_consistently_normalized`.
    //
    // Round-101: the normalization body lives in the shared
    // `crate::cli::paths::display_path_for` helper — `silt test`
    // (src/cli/test.rs) builds the same closure from it, so the
    // two subcommands can never drift. Lock:
    // tests/meta/round101_display_path_helper_lock_tests.rs.
    let user_path_is_absolute = Path::new(path).is_absolute();
    let cwd = std::env::current_dir().ok();
    let normalize_path = |candidate: &Path| -> String {
        crate::cli::paths::display_path_for(user_path_is_absolute, cwd.as_deref(), candidate)
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
    let (err_source, err_path): (&str, String) = match innermost_fn_name
        .and_then(|n| module_sources.get(n))
    {
        Some((module_path, module_source)) => (module_source.as_str(), normalize_path(module_path)),
        None => (source, normalize_path(Path::new(path))),
    };
    let mut rendered = SourceError::runtime_at(&e.message, span, err_source, &err_path).to_string();
    // The call stack, if there are user frames beyond the error site.
    // Synthetic entry-point frames (<script>, <call:...>) are dropped
    // by name rather than by span — a zero-spanned frame inside an
    // otherwise good stack shouldn't cause the whole stack to be
    // discarded. <module:...> frames are kept for module-aware path
    // resolution.
    //
    // Round-73 G1: filter and truncation live in the shared
    // `render_call_stack` helper so `silt run` and `silt test` can
    // never drift again.
    let stack_lines = silt::vm::error::render_call_stack(&e.call_stack, |name, frame_span| {
        // Each frame uses its own function's source file for file
        // labels — this matters when the call crosses a module
        // boundary.
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
        rendered.push_str("\n\ncall stack:");
        for line in stack_lines {
            rendered.push('\n');
            rendered.push_str(&line);
        }
    }
    rendered
}
