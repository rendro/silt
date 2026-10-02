//! `silt test [--filter <pat>] [path]` — discover, compile, and run
//! `test_*` functions.

use std::collections::BTreeMap;
use std::path::Path;
use std::process;
use std::sync::Arc;

use silt::diagnostic::{Code, Diagnostic, render_human};
use silt::scheduler::UnjoinedFailures;
use silt::session::{Entry, EntryPoint, LockPolicy, TestKind, test_functions};
use silt::source::{FileId, SourceMap, Span};
use silt::vm::Vm;

use crate::cli::help::test_usage_banner;
use crate::cli::paths::{ProgramFiles, door_diagnostics, find_silt_files, open_entry};
use crate::cli::run::{render_runtime_error, returned_err};

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
            println!("  --filter <pat>      Only run tests whose name contains <pat>");
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
    run_tests(file.as_deref(), filter);
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

/// Print what the session found in `path`, in the order and the form
/// `silt check` prints it, warnings included. `compiled` says whether the
/// file compiled. Returns `true` when the file failed to compile; a line
/// that says so is printed last.
fn report_diagnostics(
    path: &str,
    sources: &SourceMap,
    diagnostics: &[Diagnostic],
    compiled: bool,
) -> bool {
    silt::diagnostic::eprint_all(&ProgramFiles::new(path, sources), diagnostics);
    if compiled && !diagnostics.iter().any(Diagnostic::is_error) {
        return false;
    }
    let mut kinds: Vec<&str> = Vec::new();
    for diagnostic in diagnostics.iter().filter(|d| d.is_error()) {
        let kind = diagnostic.phase().word();
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    if kinds.is_empty() {
        eprintln!("{path}: failed to compile — errors (see above)");
        return true;
    }
    let kinds: Vec<String> = kinds.iter().map(|kind| format!("{kind} errors")).collect();
    eprintln!(
        "{path}: failed to compile — {} (see above)",
        kinds.join(" and ")
    );
    true
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

    // Files that `--filter` did not rule out. Without a filter, all of them.
    let mut files_considered: usize = 0;
    let mut total = 0;
    let mut skipped = 0;
    let mut counts = Counts::default();
    // The failures of spawned tasks are taken from the scheduler and
    // charged to the test that spawned the task.
    silt::scheduler::collect_unjoined_failures();
    let mut owners = TaskOwners::default();

    for path in &paths {
        let (mut session, file) = match open_entry(path, LockPolicy::Update) {
            Ok(opened) => opened,
            Err(e) => {
                // An unreadable file cannot be asked for its tests, so
                // `--filter` does not rule it out: the error is reported.
                files_considered += 1;
                eprintln!("{path}: failed to read — {e}");
                counts.file_errors += 1;
                continue;
            }
        };

        // With a filter, a file without a test whose name it selects is
        // left alone: it is not analysed, and nothing is reported for it.
        // A file that does not lex cannot be asked for its tests, so it
        // is kept and its error reported.
        if let Some(pattern) = filter.as_deref()
            && let Some(ast) = &session.graph().module(session.module_of(file)).ast
            && !test_functions(ast)
                .iter()
                .any(|(name, _)| name.contains(pattern))
        {
            continue;
        }
        files_considered += 1;

        // The session analyses the file as `silt check` does, so a test
        // file gets the diagnostics `silt check` gives it: the type
        // errors of the modules it imports, the compiler's warnings, the
        // static checks against declared dependencies. It is compiled for
        // its tests: the declarations, without a call of `main`, and the
        // tests that the filter selects, in source order.
        let compiled = session.compile(
            file,
            Entry::Tests {
                filter: filter.clone(),
            },
        );
        let diagnostics = door_diagnostics(&mut session, file, &compiled);
        let failed_to_compile =
            report_diagnostics(path, session.sources(), &diagnostics, compiled.is_ok());
        let program = match compiled {
            Ok(program) if !failed_to_compile => program,
            _ => {
                counts.file_errors += 1;
                continue;
            }
        };
        let EntryPoint::Tests(tests) = program.entry else {
            unreachable!("a program compiled for its tests has tests as its entry point");
        };
        let sources = session.into_sources();
        let Some(first) = program.functions.into_iter().next() else {
            eprintln!("{path}: internal error: no functions compiled");
            counts.file_errors += 1;
            continue;
        };
        // Tasks that the file's top-level code spawns are the file's.
        let file_index = owners.add_file(TestFile {
            path: path.clone(),
            sources,
            entry: file,
        });
        let setup_owner = owners.add_owner(file_index, None);
        silt::scheduler::set_task_owner(setup_owner);

        let script = Arc::new(first);
        let mut vm = Vm::new();
        if let Err(e) = vm.run(script) {
            owners.mark_failed(setup_owner);
            // G2 (audit round 21): frame and error-header paths follow
            // the style of the path the user typed, as under `silt run`.
            //
            // Lock: tests/cli/cli_test_rendering_tests.rs
            // `test_test_setup_error_paths_normalized`.
            eprintln!("{path}: setup error:");
            eprintln!(
                "{}",
                render_runtime_error(&e, path, &owners.files[file_index].sources)
            );
            counts.file_errors += 1;
            continue;
        }

        // Run each selected test function
        for test in &tests {
            let name = &test.name;
            total += 1;
            if test.kind == TestKind::Skip {
                eprintln!("  SKIP {path}::{name}");
                skipped += 1;
                continue;
            }
            // The tasks that this test spawns, and the tasks that those
            // spawn in turn, are the test's: their failures fail it.
            let owner = owners.add_owner(file_index, Some(name.clone()));
            silt::scheduler::set_task_owner(owner);
            let caller = silt::bytecode::call_global_script(name);
            let outcome = vm.run(Arc::new(caller));
            // The failures of spawned tasks that have happened by now.
            // Those of this test's tasks are reported under its result
            // line; those of earlier tests are reported here.
            let task_failures = owners.charge_task_failures(Some(owner), &mut counts);
            let test_failed = match outcome {
                Ok(value) => match returned_err(&value) {
                    // `Ok(..)`, Unit and every other value: the test ran
                    // to its end.
                    None if task_failures.is_empty() => {
                        eprintln!("  PASS {path}::{name}");
                        false
                    }
                    // The test ran to its end, but a task it spawned
                    // failed and was neither joined nor cancelled.
                    None => {
                        eprintln!("  FAIL {path}::{name}");
                        true
                    }
                    // `Err(..)`: the test gave up, typically at a `?`, and
                    // the assertions after that point never ran. Same
                    // rule as for `main` under `silt run`; the error is
                    // at the test's name.
                    Some(payload) => {
                        eprintln!("  FAIL {path}::{name}");
                        let d = Diagnostic::error(
                            Code::MainReturnedErr,
                            test.span,
                            format!("{name} returned Err: {payload}"),
                        );
                        let files = ProgramFiles::new(path, &owners.files[file_index].sources);
                        eprint_indented(&render_human(&files, &d));
                        true
                    }
                },
                Err(e) => {
                    eprintln!("  FAIL {path}::{name}");
                    // The location is in the file the error's span
                    // names, mirroring `silt run`; the test file itself
                    // is named as typed.
                    // Indented under the FAIL header, with the call stack
                    // when the error crosses two or more meaningful
                    // frames, as under `silt run`.
                    eprint_indented(&render_runtime_error(
                        &e,
                        path,
                        &owners.files[file_index].sources,
                    ));
                    true
                }
            };
            for report in &task_failures {
                eprint_indented(report);
            }
            if test_failed {
                owners.mark_failed(owner);
                counts.failed += 1;
            } else {
                counts.passed += 1;
            }
        }
    }

    // `--filter` ruled every file out: say so instead of printing a
    // summary of nothing.
    if files_considered == 0 {
        println!("no matching test files found");
        return;
    }

    // The last test has returned. The tasks that have failed by now are
    // charged to the tests that spawned them, before the summary. A task
    // that is still running is not a failure; if it fails later, its
    // failure is not reported.
    let _ = owners.charge_task_failures(None, &mut counts);

    let Counts {
        passed,
        failed,
        file_errors,
    } = counts;
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

/// The tallies of a `silt test` run.
#[derive(Default)]
struct Counts {
    passed: usize,
    failed: usize,
    /// Files that failed to lex, parse, type-check or compile, or whose
    /// top-level code failed. These are tracked separately from the
    /// per-test failure counter so that `X tests: Y passed, Z failed`
    /// still reflects what actually ran: a file that does not compile
    /// may contain dozens of tests that could not even be counted.
    file_errors: usize,
}

/// A test file whose code has run, kept to render the failures of the
/// tasks that its tests spawned.
struct TestFile {
    path: String,
    /// The text of the file and of every module file it imports.
    sources: SourceMap,
    /// The file itself.
    entry: FileId,
}

/// What spawned a task: a test, or the top-level code of a file.
struct TaskOwner {
    /// Index of the file in `TaskOwners::files`.
    file: usize,
    /// The test's name; `None` for the file's top-level code.
    test: Option<String>,
    /// True once the test (or the file) has been counted as failed.
    failed: bool,
}

/// Who spawned the tasks of a `silt test` run, so that the failure of a
/// task can be charged to the test that spawned it. The owner tag of an
/// owner, as the scheduler carries it (`silt::scheduler::set_task_owner`),
/// is its index in `owners` plus one; 0 is no owner.
#[derive(Default)]
struct TaskOwners {
    files: Vec<TestFile>,
    owners: Vec<TaskOwner>,
}

impl TaskOwners {
    /// Keep a file, and return its index.
    fn add_file(&mut self, file: TestFile) -> usize {
        self.files.push(file);
        self.files.len() - 1
    }

    /// A new owner in the file at `file`: the test `test`, or the file's
    /// top-level code. Returns its owner tag.
    fn add_owner(&mut self, file: usize, test: Option<String>) -> u64 {
        self.owners.push(TaskOwner {
            file,
            test,
            failed: false,
        });
        self.owners.len() as u64
    }

    /// Note that the owner tagged `tag` has been counted as failed.
    fn mark_failed(&mut self, tag: u64) {
        if let Some(index) = owner_index(tag)
            && let Some(owner) = self.owners.get_mut(index)
        {
            owner.failed = true;
        }
    }

    /// Take the failures of spawned tasks that have happened so far, each
    /// rendered against the file of the test that spawned the task.
    ///
    /// The reports of the owner tagged `current`, the test that has just
    /// run, are returned: the caller prints them under the test's result
    /// line, and counts the test as failed. Every other owner has been
    /// counted already. It is reported here, as failed, with the reports
    /// of its tasks; a test that was counted as passed is counted as
    /// failed instead.
    fn charge_task_failures(&mut self, current: Option<u64>, counts: &mut Counts) -> Vec<String> {
        let taken = silt::scheduler::take_unjoined_failures();
        // The reports per owner tag, in the order in which the tasks
        // failed.
        let mut reports: BTreeMap<u64, Vec<String>> = BTreeMap::new();
        for failure in &taken.failures {
            let error = failure.report_error();
            let rendered = match self.file_of(failure.owner) {
                Some(file) => render_runtime_error(&error, &file.path, &file.sources),
                None => error.to_string(),
            };
            reports.entry(failure.owner).or_default().push(rendered);
        }
        for &(owner, count) in &taken.not_kept {
            // Which tasks they were is not known: the report is about
            // the whole file, at its start.
            let message = UnjoinedFailures::not_kept_message(count);
            let rendered = match self.file_of(owner) {
                Some(file) => render_human(
                    &ProgramFiles::new(&file.path, &file.sources),
                    &Diagnostic::error(
                        Code::UnjoinedTaskFailure,
                        Span::point(file.entry, 0),
                        message,
                    ),
                ),
                None => render_human(
                    &SourceMap::new(),
                    &Diagnostic::error(Code::UnjoinedTaskFailure, Span::BUILTIN, message),
                ),
            };
            reports.entry(owner).or_default().push(rendered);
        }
        let mut current_reports = Vec::new();
        for (tag, owner_reports) in reports {
            if Some(tag) == current {
                current_reports = owner_reports;
            } else {
                self.report_late_failures(tag, &owner_reports, counts);
            }
        }
        current_reports
    }

    /// The file of the owner tagged `tag`.
    fn file_of(&self, tag: u64) -> Option<&TestFile> {
        let owner = self.owners.get(owner_index(tag)?)?;
        self.files.get(owner.file)
    }

    /// Report the failures of tasks spawned by the owner tagged `tag`,
    /// which has been counted already, and count it as failed.
    fn report_late_failures(&mut self, tag: u64, reports: &[String], counts: &mut Counts) {
        let owner = match owner_index(tag) {
            Some(index) => self.owners.get_mut(index),
            None => None,
        };
        match owner {
            Some(owner) => {
                let path = self.files[owner.file].path.as_str();
                match &owner.test {
                    Some(name) => {
                        eprintln!(
                            "  FAIL {path}::{name} (a task it spawned failed after the test had returned)"
                        );
                        if !owner.failed {
                            counts.passed = counts.passed.saturating_sub(1);
                            counts.failed += 1;
                        }
                    }
                    None => {
                        eprintln!(
                            "  FAIL {path} (a task spawned by the file's top-level code failed)"
                        );
                        if !owner.failed {
                            counts.file_errors += 1;
                        }
                    }
                }
                owner.failed = true;
            }
            // Every task of the run is spawned under a tag of this run;
            // this is a failure that nothing can be charged with.
            None => {
                eprintln!("  FAIL a task that no test can be named for failed");
                counts.file_errors += 1;
            }
        }
        for report in reports {
            eprint_indented(report);
        }
    }
}

/// The index in `TaskOwners::owners` of the owner tagged `tag`.
fn owner_index(tag: u64) -> Option<usize> {
    usize::try_from(tag.checked_sub(1)?).ok()
}

/// Print a rendered diagnostic on stderr, indented under a test's result
/// line.
fn eprint_indented(text: &str) {
    for line in text.lines() {
        if line.is_empty() {
            eprintln!();
        } else {
            eprintln!("    {line}");
        }
    }
}
