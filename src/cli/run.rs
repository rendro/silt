//! `silt run [--disassemble] [<file>]` — compile and execute a silt
//! program with the bytecode VM. Also backs the bare `silt
//! <file>.silt` convenience shim.

use std::process;

use silt::ast::{Decl, Program};
use silt::diagnostic::{Code, Diagnostic, render_human};
use silt::intern::resolve;
use silt::session::{Entry, LockPolicy};
use silt::source::{FileId, SourceMap, Span};
use silt::vm::{Vm, VmError};

use crate::cli::help::{run_help_text, run_usage_banner};
use crate::cli::package::resolve_package_entry_point;
use crate::cli::paths::{ProgramFiles, door_diagnostics, open_entry_or_exit};

/// Dispatch `silt run [--disassemble] [<file>] [-- <program-args>...]`.
pub(crate) fn dispatch(args: &[String]) {
    if args[2..].iter().any(|a| a == "--help" || a == "-h") {
        print!("{}", run_help_text());
        process::exit(0);
    }
    let mut disasm = false;
    let mut file: Option<String> = None;
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
    if disasm {
        crate::cli::disasm::disasm_file(&file);
    } else {
        vm_run_file(&file);
    }
}

/// Legacy `silt <file>.silt [--help|--disassemble]`
/// convenience shim — same behavior as `silt run` with the file baked
/// in as the first argument.
pub(crate) fn dispatch_bare_file(args: &[String], file: &str) {
    let mut disasm = false;
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
        } else if extra.starts_with('-') {
            let suggestion = match extra.as_str() {
                "--disasm" | "--disassembly" | "-d" => " (did you mean --disassemble?)",
                "--h" | "-help" => " (did you mean --help?)",
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
    if disasm {
        crate::cli::disasm::disasm_file(file);
    } else {
        vm_run_file(file);
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
    if !tag.is(silt::typeinfo::bv::ERR) {
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

/// The span of the name of the top-level function `name` of `program`.
pub(crate) fn fn_name_span(program: &Program, name: &str) -> Option<Span> {
    program.decls.iter().find_map(|decl| match decl {
        Decl::Fn(f) if resolve(f.name) == name => Some(f.name_span),
        _ => None,
    })
}

/// Run a file using the bytecode VM (default path): the session analyses
/// it and compiles it for `main`, and the VM runs what it compiled.
pub(crate) fn vm_run_file(path: &str) {
    silt::intern::reset();
    let (mut session, file) = open_entry_or_exit(path, LockPolicy::Update);
    let compiled = session.compile(file, Entry::Main);
    let diagnostics = door_diagnostics(&mut session, file, &compiled);
    // F14 (audit round 17): print diagnostics with a blank line between
    // consecutive errors so multi-error output doesn't form a solid wall
    // of text. Matches rustc/gcc convention.
    // Lock: tests/cli/cli_test_rendering_tests.rs
    // `test_multiple_errors_render_with_blank_separator`.
    silt::diagnostic::eprint_all(&ProgramFiles::new(path, session.sources()), &diagnostics);
    let program = match compiled {
        Ok(program) if !diagnostics.iter().any(Diagnostic::is_error) => program,
        _ => process::exit(1),
    };
    // Where a `main` that returns `Err` is reported: at its name, or at
    // the start of the entry file when no function is named `main`.
    let main_span = session
        .module_analysis(session.module_of(file))
        .and_then(|analysis| fn_name_span(&analysis.ast, "main"))
        .unwrap_or(Span::point(file, 0));
    let sources = session.into_sources();

    // Run via VM. The failures of tasks that nobody joins are taken and
    // reported here, against the program's files, instead of by the
    // scheduler.
    silt::scheduler::collect_unjoined_failures();
    let mut vm = Vm::new(silt::HostIo::process());
    let run_result = vm.run_program(&program);
    // The program has ended. The tasks that failed by now and that
    // nobody joined or cancelled are reported, and make the run fail. A
    // task that is still running is not a failure; if it fails later,
    // nobody takes its failure and it is not reported.
    let tasks_failed = report_task_failures(path, &sources, file);
    // Round-93: a `fn main() -> Result(..)` that evaluates to `Err(..)`
    // is a failed program — surface it. Previously the Ok value of
    // `vm.run` (main's return value) was discarded wholesale, so
    // `fn main() -> Result(Int, String) { Err("boom") }` (or a `?`
    // propagating an Err out of main) exited 0 with no diagnostic and
    // CI/shell callers saw success on failure. Render through the
    // canonical `error[runtime]:` header, at `main`'s name, and exit 1,
    // matching the exit code every other runtime error uses.
    // `Ok(..)` and non-Result returns (Unit, Int, ...) are unchanged.
    // `silt test` applies the same rule to a test function, through the
    // same `returned_err`; the REPL has its own `vm.run` handling.
    if let Ok(value) = &run_result
        && let Some(payload) = returned_err(value)
    {
        let d = Diagnostic::error(
            Code::MainReturnedErr,
            main_span,
            format!("main returned Err: {payload}"),
        );
        eprintln!("{}", render_human(&ProgramFiles::new(path, &sources), &d));
        process::exit(1);
    }
    if let Err(e) = run_result {
        eprintln!("{}", render_runtime_error(&e, path, &sources));
        process::exit(1);
    }
    if tasks_failed {
        process::exit(1);
    }
}

/// Report on stderr the failures of tasks that nobody joined or
/// cancelled and that have happened so far, each rendered like any other
/// runtime error of the program at `path`, whose entry file is `entry`.
/// Returns true if there was one.
fn report_task_failures(path: &str, sources: &SourceMap, entry: FileId) -> bool {
    let failures = silt::scheduler::take_unjoined_failures();
    for failure in &failures.failures {
        eprintln!(
            "{}",
            render_runtime_error(&failure.report_error(), path, sources)
        );
    }
    let not_kept: usize = failures.not_kept.iter().map(|(_, count)| count).sum();
    if not_kept > 0 {
        // Which tasks they were is not known: the report is about the
        // whole program, at the start of its entry file.
        let d = Diagnostic::error(
            Code::UnjoinedTaskFailure,
            Span::point(entry, 0),
            silt::scheduler::UnjoinedFailures::not_kept_message(not_kept),
        );
        eprintln!("{}", render_human(&ProgramFiles::new(path, sources), &d));
    }
    !failures.is_empty()
}

/// Render a runtime error of the program at `path`, the way `silt run`
/// shows it: the header, the location with its source line, in the file
/// the error's span names, then the call stack when it has more than one
/// meaningful frame. `silt test` renders the failures of tasks with it as
/// well.
pub(crate) fn render_runtime_error(e: &VmError, path: &str, sources: &SourceMap) -> String {
    render_human(&ProgramFiles::new(path, sources), &e.to_diagnostic())
}
