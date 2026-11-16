//! `silt run [<file>]` — compile and execute a silt program with the
//! bytecode VM.

use std::path::Path;
use std::process;

use crate::diagnostic::{Diagnostic, render_human};
use crate::session::{Entry, LockPolicy};
use crate::source::SourceMap;
use crate::vm::{Exit, Vm, VmError};

use crate::cli::help::{asks_for_help, run_help_text, run_usage_banner};
use crate::cli::package::{
    EntryPointKind, find_project_root, resolve_package_entry_point, resolve_package_entry_point_in,
};
use crate::cli::paths::{
    FILE_OR_PACKAGE, PathArg, ProgramFiles, door_diagnostics, open_entry_or_exit, path_arg,
    refuse_path,
};

/// Dispatch `silt run [<file>] [-- <program-args>...]`.
pub(crate) fn dispatch(args: &[String]) {
    if asks_for_help(&args[2..]) {
        print!("{}", run_help_text());
        process::exit(0);
    }
    let (file, program_args) = arguments(args);
    // Publish the forwarded args before compile/run so `io.args()`
    // sees them. Always set (even when empty) so a stale snapshot
    // from a prior in-process invocation doesn't leak through.
    crate::cli::refuse_bad_settings("run");
    crate::builtins::io::set_program_args(program_args);
    // No explicit file → look for an enclosing silt package and use
    // its `src/main.silt`. Outside a package, the usage line says that
    // a file is needed.
    // The path given is what the five commands that take one make of
    // it (`paths::path_arg`). No path: the package's `src/main.silt`;
    // outside a package, the usage line says that a file is needed.
    let file = match file.as_deref().map(path_arg) {
        // A package's directory runs its `main`, as `silt run` in it does.
        Some(PathArg::Package(dir)) => {
            match resolve_package_entry_point_in(Path::new(&dir), EntryPointKind::RequireMain) {
                Ok(Some(p)) => p.to_string_lossy().into_owned(),
                // (Not found: it is read as the directory it is.)
                Ok(None) => dir,
                Err(()) => process::exit(1),
            }
        }
        Some(PathArg::Dir(dir)) => refuse_path("run", &dir, FILE_OR_PACKAGE),
        // Any other file is no program: most likely it is meant for the
        // program, as its argument.
        Some(PathArg::NotSilt(f)) => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
            let in_package = matches!(find_project_root(&cwd), Ok(Some(_)));
            // (In a package, `silt run` with no file runs the package's
            // program: the argument goes behind `--` alone.)
            let (to, program) = if in_package {
                ("the package's program (src/main.silt)", "")
            } else {
                ("a program", " <file>.silt")
            };
            let shown = crate::source::shown_path(&f);
            match crate::source::pasteable(&f) {
                true => eprintln!(
                    "error: silt run: {shown} is not a .silt file; to give it to {to}, write it \
                     after `--`: `silt run{program} -- {f}`"
                ),
                false => eprintln!(
                    "error: silt run: {shown} is not a .silt file; to give it to {to}, write it \
                     after `--`"
                ),
            }
            process::exit(1);
        }
        Some(PathArg::File(f)) => f,
        None => match resolve_package_entry_point() {
            Ok(Some(p)) => p.to_string_lossy().into_owned(),
            Ok(None) => {
                crate::cli::no_file("run");
                eprintln!("Usage: {}", run_usage_banner());
                process::exit(1);
            }
            Err(()) => process::exit(1),
        },
    };
    vm_run_file(&file);
}

/// The file and the program's arguments of `silt run <args>` (`args[0]`
/// is the binary, `args[1]` the command). A usage error (an unknown
/// flag, a second file) ends the process with 1, before anything is
/// read: also before `silt run --watch` watches.
pub(crate) fn arguments(args: &[String]) -> (Option<String>, Vec<String>) {
    let mut file: Option<String> = None;
    // Positionals after `--` are forwarded to the running
    // program (surfaced via `io.args()`), not interpreted as silt CLI
    // flags / files. This restores the ability to pass user args to
    // scripts (e.g. `silt run text_stats.silt -- input.txt`) which
    // the rejection of bare extra positionals had broken — there
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
        } else if arg.starts_with('-') {
            let suggestion = match arg.as_str() {
                "--h" | "-help" => " (did you mean --help?)",
                _ => "",
            };
            eprintln!(
                "error: silt run: unknown flag '{}'{suggestion}",
                crate::git::escape_for_display(arg)
            );
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
            // To forward args to the running program, use
            // `--` as a separator (`silt run a.silt -- foo bar`).
            let shown = crate::git::escape_for_display(arg);
            eprintln!("error: silt run: unexpected extra argument '{shown}'");
            match crate::source::pasteable(arg) {
                true => eprintln!(
                    "If '{shown}' is meant for the program, separate it with '--' (e.g. 'silt run <file>.silt -- {arg}')."
                ),
                false => eprintln!("If '{shown}' is meant for the program, separate it with '--'."),
            }
            eprintln!("Run 'silt run --help' for usage.");
            process::exit(1);
        }
    }
    (file, program_args)
}

/// Run a file using the bytecode VM (default path): the session analyses
/// it and compiles it for `main`, and the VM runs what it compiled.
pub(crate) fn vm_run_file(path: &str) {
    let (mut session, file) = open_entry_or_exit(path, LockPolicy::Update);
    let compiled = session.compile(file, Entry::Main);
    let diagnostics = door_diagnostics(&mut session, file, &compiled);
    // F14: print diagnostics with a blank line between
    // consecutive errors so multi-error output doesn't form a solid wall
    // of text. Matches rustc/gcc convention.
    // Lock: tests/cli/cli_test_rendering_tests.rs
    // `test_multiple_errors_render_with_blank_separator`.
    crate::diagnostic::eprint_all(&ProgramFiles::new(path, session.sources()), &diagnostics);
    let program = match compiled {
        Ok(program) if !diagnostics.iter().any(Diagnostic::is_error) => program,
        _ => process::exit(1),
    };
    // The warnings stand apart from what the program writes.
    if !diagnostics.is_empty() {
        eprintln!();
    }
    let sources = session.into_sources();

    // Run via VM. The failures of tasks that nobody joins are taken and
    // reported here, against the program's files, instead of by the
    // scheduler. From here on the program may have threads, and the
    // process ends through `exit_with_status`: a thread of the runtime
    // that has panicked may be ending it with the internal error.
    crate::scheduler::collect_unjoined_failures();
    let mut vm = Vm::new(crate::HostIo::process());
    // `process.exit` ends the process where it is called, from `main`
    // or from a task: nothing below runs then.
    vm.set_exit(Exit::EndsProcess);
    #[cfg(feature = "test-hooks")]
    panic_where_a_test_says(&vm);
    // (A build with `test-hooks`: tests/cli/end_race_tests.rs.)
    #[cfg(feature = "test-hooks")]
    races_where_a_test_says();
    let run_result = vm.run_program(&program);
    // `main` has returned; the program ends when its tasks can do no
    // more. After an error of `main`, raised or returned as `Err`,
    // nothing is waited for: the program has failed.
    if run_result.is_ok() {
        vm.settle();
    }
    // The program has ended. What failed is rendered first: the tasks
    // that failed and that nobody joined or cancelled, and `main`'s
    // error or `Err`. (The `Display` of `main`'s `Err` ran in
    // `run_program`: a `process.exit` there ended the process.) A
    // `process.exit` in a task that still runs ends the process with
    // its code, and nothing is reported. Then the failure claims the end
    // (`claim_program_failure`): from here an exit is ignored, and the
    // report is written whole, and the run fails.
    let (task_texts, tasks_failed) = task_failure_texts(path, &sources);
    // `main`'s error: raised, with its full diagnostic, or returned as
    // `Err(e)`, which is the program's message to its user: the one line
    // `error: <e>`, with no place of silt's (see `Vm::run_program`).
    // A `Display` impl of the `Err` that failed is told in a note under
    // the line.
    let main_text = (run_result.as_ref().err()).map(|e| {
        e.render_in(
            &ProgramFiles::new(path, &sources),
            crate::diagnostic::active_colors(),
        )
    });
    let failed = tasks_failed || main_text.is_some();
    if failed {
        crate::scheduler::claim_program_failure();
    }
    // (Also when there is nothing to say: a process that is ending
    // with the internal error goes no further than here, and nothing of
    // the program's is reported then. See `crate::scheduler::report`.)
    let mut report = String::new();
    for text in task_texts.iter().chain(&main_text) {
        report += text;
        report.push('\n');
    }
    crate::scheduler::report_on_stderr(&report);
    if failed {
        crate::scheduler::exit_with_status(1);
    }
}

/// In a build with `test-hooks` only: force the orders of the end of a
/// program that `SILT_TEST_END_RACE` and `SILT_TEST_STOP_RACE`
/// (`task-end`, `main-returns`) name (tests/cli/end_race_tests.rs).
#[cfg(feature = "test-hooks")]
fn races_where_a_test_says() {
    use crate::scheduler::{StopRace, force_the_end_race, force_the_stop_race};
    if std::env::var_os("SILT_TEST_END_RACE").is_some() {
        force_the_end_race();
    }
    match std::env::var("SILT_TEST_STOP_RACE").as_deref() {
        Ok("task-end") => force_the_stop_race(StopRace::TaskEnd),
        Ok("main-returns") => force_the_stop_race(StopRace::MainReturns),
        _ => {}
    }
}

/// In a build with `test-hooks` only: make the runtime of `vm` panic
/// once at each place `SILT_TEST_PANIC` names (`task:<id>`, `program`,
/// `timer`, `io`, `builtin`, `builtin-of-a-task`; several with commas between), for the tests of what
/// the binary does when a thread of the runtime panics
/// (tests/cli/runtime_panic_tests.rs). `silt run` and `silt test` call
/// it for each VM they make. No other build reads the variable.
#[cfg(feature = "test-hooks")]
pub(crate) fn panic_where_a_test_says(vm: &Vm) {
    use crate::vm::PanicAt;
    let Ok(places) = std::env::var("SILT_TEST_PANIC") else {
        return;
    };
    let task = |id: &str| id.parse().ok().map(PanicAt::Task);
    for place in places.split(',') {
        let at = match place {
            "program" => Some(PanicAt::Program),
            "timer" => Some(PanicAt::Timer),
            "io" => Some(PanicAt::Io),
            "builtin" => Some(PanicAt::Builtin),
            "builtin-of-a-task" => Some(PanicAt::BuiltinOfATask),
            place => place.strip_prefix("task:").and_then(task),
        };
        match at {
            Some(at) => crate::vm::panic_for_test(vm, at),
            None => {
                eprintln!("silt: SILT_TEST_PANIC names no place: '{place}'");
                process::exit(2);
            }
        }
    }
}

/// The failures of tasks that nobody joined or cancelled and that have
/// happened so far, each rendered like any other runtime error of the
/// program at `path`, and whether there was one.
fn task_failure_texts(path: &str, sources: &SourceMap) -> (Vec<String>, bool) {
    let failures = crate::scheduler::take_unjoined_failures();
    let mut texts: Vec<String> = Vec::new();
    for failure in &failures.failures {
        texts.push(render_runtime_error(&failure.report_error(), path, sources));
    }
    for not_shown in &failures.not_shown {
        let d = not_shown.diagnostic();
        texts.push(render_human(&ProgramFiles::new(path, sources), &d));
    }
    (texts, !failures.is_empty())
}

/// Render a runtime error of the program at `path`, the way `silt run`
/// shows it: the header, the location with its source line, in the file
/// the error's span names, then the call stack when it has more than one
/// meaningful frame. `silt test` renders the failures of tasks with it as
/// well.
pub(crate) fn render_runtime_error(e: &VmError, path: &str, sources: &SourceMap) -> String {
    e.diagnostic_text(
        &ProgramFiles::new(path, sources),
        crate::diagnostic::active_colors(),
    )
}
