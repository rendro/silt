//! The differential oracle: what one program is put through, and what
//! counts as a finding.
//!
//! 1. The program is analysed as every front door analyses it. A
//!    program with an error is not run.
//! 2. It is compiled for `main`. Every function it is made of must
//!    pass the bytecode verifier again.
//! 3. A program whose code names a builtin that reaches outside the VM
//!    ([`stays_inside`]) is not run: the oracle's programs read and
//!    write nothing but their output.
//! 4. It is run twice, each run on a VM of its own whose output goes
//!    into buffers. The two runs must agree: the same output, the same
//!    failures of tasks that nobody joined, the same value of `main` or
//!    the same error.
//! 5. No run may end in a `type_confusion` error, in an internal error
//!    or in a panic, and no task of it either.
//!
//! What is compared in step 4 depends on the builtins the program's
//! code names. The runtime runs tasks on several threads and reads the
//! time of day, so the output of a program that names a function of
//! [`UNORDERED`] or of [`RANDOM`] is its own only when something says
//! so (a golden case's exact `.stdout`, [`Expect::stdout`]); without
//! that, such a program is held to step 5 alone.
//!
//! This file uses `silt` and `std` only: the fuzz target `fuzz_run`
//! includes it by path.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use silt::builtins::registry::registry;
use silt::bytecode::{Function, Instr};
use silt::diagnostic::Code;
use silt::session::{Config, Entry, LockPolicy, Program, ProjectSetup, Session};
use silt::{Buffer, Clock, HostIo, Value, Vm};

/// The builtin modules whose functions stay inside the VM, but for
/// those of [`OUTSIDE`]. The prelude (`println`) is the module `""`. A
/// module that is not listed reaches files, the environment, the
/// network, the process's arguments or its stdin.
const INSIDE_MODULES: &[&str] = &[
    "", "string", "int", "float", "list", "map", "set", "result", "option", "test", "math",
    "regex", "json", "toml", "bytes", "encoding", "crypto", "uuid", "time", "task", "channel",
    "stream",
];

/// The functions of [`INSIDE_MODULES`] that read or write a file or a
/// connection.
const OUTSIDE: &[&str] = &[
    "stream.file_chunks",
    "stream.file_lines",
    "stream.tcp_chunks",
    "stream.tcp_lines",
    "stream.write_to_file",
    "stream.write_to_tcp",
];

/// The functions of other modules that stay inside the VM.
const INSIDE: &[&str] = &["io.inspect"];

/// The builtin modules with which the order of a program's output is
/// the scheduler's or the clock's to decide.
pub const UNORDERED: &[&str] = &["task", "channel", "stream", "time"];

/// The builtins that draw from the system's random source.
pub const RANDOM: &[&str] = &["uuid.v4", "uuid.v7", "crypto.random_bytes"];

/// Whether the builtin `name` (`list.map`, `println`) touches nothing
/// outside the VM that runs it.
pub fn stays_inside(name: &str) -> bool {
    match INSIDE_MODULES.contains(&module_of(name)) {
        true => !OUTSIDE.contains(&name),
        false => INSIDE.contains(&name),
    }
}

/// The native stack of the thread a program is checked and run on: what
/// the `silt` command gives its main thread.
const STACK_BYTES: usize = 256 * 1024 * 1024;

/// How long a run may take before it counts as hung. No program of the
/// oracle's inputs comes near it; a run that does is a finding
/// ([`Kind::Hang`]), and its thread is left behind.
const WATCHDOG: Duration = Duration::from_secs(60);

/// The directory the files of an in-memory program are in.
const MEMORY_DIR: &str = "/silt-oracle";

/// The files of a program.
pub enum Source {
    /// Files that exist only in memory, each a name and its text; the
    /// first is the entry file.
    Memory(Vec<(String, String)>),
    /// A file on disk, in a directory of its own: the files beside it
    /// are its modules.
    Script(PathBuf),
    /// The entry file of a package on disk: its `silt.toml` is looked
    /// for in the file's directory and above.
    Package(PathBuf),
}

/// What is known of a program's run from elsewhere.
#[derive(Default)]
pub struct Expect {
    /// The program ends without an error and no task of it fails
    /// unjoined: a golden case's `exit: 0`.
    pub succeeds: bool,
    /// The program's exact stdout: a golden case's `.stdout` file.
    pub stdout: Option<String>,
    /// The value `main` returns, or the message of the runtime error
    /// the program stops at: a reference evaluator's.
    pub end: Option<Result<Value, String>>,
}

/// One input of the oracle.
pub struct Input {
    /// What the input is called in reports and in the skip file.
    pub name: String,
    pub source: Source,
    pub expect: Expect,
}

/// Why a program was not run.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum NotRun {
    /// Its entry file could not be read.
    Unreadable,
    /// Its analysis has an error.
    CheckError,
    /// It could not be compiled for `main` (it has none, or it is over
    /// a limit of the bytecode).
    NotCompiled,
    /// Its code names a builtin of this module, which reaches outside
    /// the VM.
    Outside(String),
}

impl fmt::Display for NotRun {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NotRun::Unreadable => write!(f, "unreadable"),
            NotRun::CheckError => write!(f, "check error"),
            NotRun::NotCompiled => write!(f, "not compiled for main"),
            NotRun::Outside(module) => write!(f, "uses {module}"),
        }
    }
}

/// What kind of finding. The names ([`Kind::name`]) are those of the
/// skip file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// The analysis, the compiler or a run panicked.
    Panic,
    /// The compiler reported a bug of its own.
    CompilerBug,
    /// A compiled function did not pass the verifier.
    Verifier,
    /// A run ended in a `type_confusion` error.
    TypeConfusion,
    /// A run ended in an internal error.
    InternalError,
    /// A run did not end.
    Hang,
    /// A run ends the process it is in (an overflow of the native
    /// stack). The oracle cannot see this from inside: an input that
    /// does it is known by its line in the skip file, and is run
    /// through the `silt` command instead (`sweep.rs`).
    Abort,
    /// The two runs disagree.
    Differs,
    /// A run is not what the input's [`Expect`] says.
    Expectation,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Panic => "panic",
            Kind::CompilerBug => "compiler-bug",
            Kind::Verifier => "verifier",
            Kind::TypeConfusion => "type-confusion",
            Kind::InternalError => "internal-error",
            Kind::Hang => "hang",
            Kind::Abort => "abort",
            Kind::Differs => "differs",
            Kind::Expectation => "expectation",
        }
    }
}

/// A defect the oracle found in a program that checks clean.
#[derive(Debug, Clone)]
pub struct Finding {
    pub kind: Kind,
    pub detail: String,
}

impl Finding {
    pub fn new(kind: Kind, detail: impl Into<String>) -> Finding {
        Finding {
            kind,
            detail: detail.into(),
        }
    }
}

/// How much of two runs was compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Compared {
    /// Output, reports and the end of the run.
    Everything,
    /// Nothing: the program names a builtin of [`UNORDERED`] or
    /// [`RANDOM`] and nothing says what its output is.
    Invariants,
}

/// What the oracle makes of an input.
#[derive(Debug, Clone)]
pub enum Verdict {
    NotRun(NotRun),
    Passed(Compared),
    Finding(Finding),
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
enum End {
    /// `main` returned this value; `failed` when it is an `Err(..)`.
    Value {
        shown: String,
        failed: bool,
    },
    /// A runtime error.
    Error {
        message: String,
        type_confusion: bool,
        /// Everything of the error: place and call stack too.
        whole: String,
    },
    Panic(String),
    Hang,
}

/// The error of a task that failed and that nobody joined.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct TaskFailure {
    message: String,
    type_confusion: bool,
}

/// What a run left behind.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Run {
    stdout: String,
    /// What the runtime wrote for the program on the host's stderr.
    stderr: String,
    /// The tasks that failed unjoined, sorted: the order in which they
    /// fail is the scheduler's.
    failures: Vec<TaskFailure>,
    end: End,
}

/// The failures of tasks are collected for the whole process
/// (`silt::scheduler::collect_unjoined_failures`), so that the oracle
/// has each as an error and not as a line of text. They are taken
/// between runs only ([`Gate`]): a failure that is taken while its
/// program runs can no longer be joined by it. What is taken is sorted
/// into `FAILED` by the owner tag each run gives its tasks.
static FAILED: Mutex<BTreeMap<u64, Vec<TaskFailure>>> = Mutex::new(BTreeMap::new());
static NEXT_OWNER: AtomicU64 = AtomicU64::new(1);
static GATE: Gate = Gate {
    state: Mutex::new(GateState {
        running: 0,
        waiting: 0,
        hung: false,
    }),
    changed: Condvar::new(),
};

/// What lets runs go on side by side and the failures be taken with no
/// run in progress.
struct Gate {
    state: Mutex<GateState>,
    changed: Condvar,
}

struct GateState {
    /// The runs in progress.
    running: usize,
    /// The threads that wait to take the failures: no run starts while
    /// there is one.
    waiting: usize,
    /// A run has not ended and never will: it is not waited for.
    hung: bool,
}

impl Gate {
    fn state(&self) -> MutexGuard<'_, GateState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A run starts.
    fn enter(&self) {
        let mut state = self.state();
        while state.waiting > 0 {
            state = self.changed.wait(state).unwrap_or_else(|p| p.into_inner());
        }
        state.running += 1;
    }

    /// A run has ended, its tasks too.
    fn leave(&self) {
        self.state().running -= 1;
        self.changed.notify_all();
    }

    /// A run was given up.
    fn hung(&self) {
        self.state().hung = true;
        self.changed.notify_all();
    }

    /// `take`, called while no run is in progress.
    fn alone<T>(&self, take: impl FnOnce() -> T) -> T {
        let mut state = self.state();
        state.waiting += 1;
        while state.running > 0 && !state.hung {
            state = self.changed.wait(state).unwrap_or_else(|p| p.into_inner());
        }
        let taken = take();
        state.waiting -= 1;
        drop(state);
        self.changed.notify_all();
        taken
    }
}

/// The failures of the tasks of the run with the tag `owner`, which has
/// ended.
fn failures_of(owner: u64) -> Vec<TaskFailure> {
    let taken = GATE.alone(silt::scheduler::take_unjoined_failures);
    let mut failed = FAILED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for failure in taken.failures {
        failed.entry(failure.owner).or_default().push(TaskFailure {
            message: failure.error.message.clone(),
            type_confusion: failure.error.type_confusion,
        });
    }
    for (owner, count) in taken.not_kept {
        failed.entry(owner).or_default().push(TaskFailure {
            message: format!("{count} more tasks failed"),
            type_confusion: false,
        });
    }
    let mut failures = failed.remove(&owner).unwrap_or_default();
    failures.sort();
    failures
}

/// A clock that stands still: the time a program reads when it names
/// no builtin of [`UNORDERED`] (`math.random`'s seed, for one).
struct Still;

impl Clock for Still {
    fn now(&self) -> Duration {
        // 2026-01-01T12:00:00Z
        Duration::from_secs(1_767_268_800)
    }

    fn monotonic(&self) -> Duration {
        Duration::ZERO
    }

    fn sleep(&self, _duration: Duration) {}
}

/// Put `input` through the oracle. The work is done on a thread with
/// the native stack of the `silt` command's main thread.
pub fn examine(input: &Input) -> Verdict {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(STACK_BYTES)
            .spawn_scoped(scope, || examine_here(input))
            .expect("a thread for the oracle")
            .join()
            .unwrap_or_else(|panic| Verdict::Finding(Finding::new(Kind::Panic, panic_text(&panic))))
    })
}

fn examine_here(input: &Input) -> Verdict {
    let prepared = catch_unwind(AssertUnwindSafe(|| prepare(&input.source)));
    let (program, builtins) = match prepared {
        Ok(Ok(prepared)) => prepared,
        Ok(Err(verdict)) => return verdict,
        Err(panic) => {
            let detail = format!("check or compile panicked: {}", panic_text(&panic));
            return Verdict::Finding(Finding::new(Kind::Panic, detail));
        }
    };
    let unordered = builtins
        .iter()
        .any(|name| UNORDERED.contains(&module_of(name)));
    let random = builtins.iter().any(|name| RANDOM.contains(&name.as_str()));
    let compared = match unordered || random {
        true if input.expect.stdout.is_none() => Compared::Invariants,
        _ => Compared::Everything,
    };

    let program = Arc::new(program);
    let first = run(&program, unordered);
    let second = run(&program, unordered);
    for (which, ran) in [("first", &first), ("second", &second)] {
        if let Some(finding) = broken(ran) {
            let detail = format!("{which} run: {}", finding.detail);
            return Verdict::Finding(Finding::new(finding.kind, detail));
        }
    }
    if let Some(finding) = unexpected(&input.expect, &first, &second) {
        return Verdict::Finding(finding);
    }
    if compared == Compared::Everything
        && let Some(detail) = difference(&first, &second)
    {
        return Verdict::Finding(Finding::new(Kind::Differs, detail));
    }
    Verdict::Passed(compared)
}

/// Steps 1 to 3: the compiled program and the builtins its code names,
/// or why there is nothing to run.
fn prepare(source: &Source) -> Result<(Program, BTreeSet<String>), Verdict> {
    let not_run = |why| Err(Verdict::NotRun(why));
    let finding = |kind, detail: String| Err(Verdict::Finding(Finding::new(kind, detail)));

    let (mut session, entry) = match open(source) {
        Some(opened) => opened,
        None => return not_run(NotRun::Unreadable),
    };
    if session.analyze(entry).has_errors() {
        return not_run(NotRun::CheckError);
    }

    let program = match session.compile(entry, Entry::Main) {
        Ok(program) => program,
        Err(errors) => {
            let bug = errors
                .iter()
                .find(|d| d.code == Code::CompilerBug || d.message.starts_with("internal"));
            return match bug {
                Some(bug) => finding(Kind::CompilerBug, bug.message.clone()),
                None => not_run(NotRun::NotCompiled),
            };
        }
    };
    let mut builtins = BTreeSet::new();
    for function in &program.functions {
        if let Err(detail) = verify_all(function, &program, &mut builtins) {
            return finding(Kind::Verifier, detail);
        }
    }
    if let Some(name) = builtins.iter().find(|name| !stays_inside(name)) {
        return not_run(NotRun::Outside(module_of(name).to_string()));
    }
    Ok((program, builtins))
}

/// The module of the builtin `name`; `""` for one of the prelude.
fn module_of(name: &str) -> &str {
    name.split_once('.').map_or("", |(module, _)| module)
}

/// A session with the files of `source`, and its entry file.
fn open(source: &Source) -> Option<(Session, silt::source::FileId)> {
    let config = |project| Config {
        project,
        lock: LockPolicy::ReadOnly,
        host: Vec::new(),
    };
    let directory = |file: &Path| file.parent().map(Path::to_path_buf);
    match source {
        Source::Memory(files) => {
            let dir = PathBuf::from(MEMORY_DIR);
            let mut session = Session::new(config(ProjectSetup::Script(dir.clone())));
            let mut entry = None;
            for (name, text) in files {
                let file = session.set_overlay(&dir.join(name), text.clone());
                entry.get_or_insert(file);
            }
            Some((session, entry?))
        }
        Source::Script(file) => {
            let mut session = Session::new(config(ProjectSetup::Script(directory(file)?)));
            let entry = session.open(file).ok()?;
            Some((session, entry))
        }
        Source::Package(file) => {
            let mut session = Session::new(config(ProjectSetup::Discover(directory(file)?)));
            let entry = session.open(file).ok()?;
            Some((session, entry))
        }
    }
}

/// Verify `function` and the functions among its constants, and add
/// the builtins they call or hold as values to `builtins`, each by its
/// qualified name.
fn verify_all(
    function: &Function,
    program: &Program,
    builtins: &mut BTreeSet<String>,
) -> Result<(), String> {
    let wrong = |error| format!("function '{}' is malformed {error}", function.name());
    silt::bytecode::verify(function).map_err(wrong)?;
    silt::bytecode::verify::verify_globals(function, &program.globals).map_err(wrong)?;
    // (The verifier has seen that each row is there.)
    let name = |id| registry().builtin(id).map(|row| row.qualified());
    for (_, instr) in function.chunk().instrs() {
        if let Instr::CallBuiltin { builtin, .. } = instr {
            builtins.extend(name(builtin));
        }
    }
    for constant in function.chunk().constants() {
        match constant {
            Value::BuiltinFn(id) => builtins.extend(name(*id)),
            Value::VmClosure(closure) => verify_all(&closure.function, program, builtins)?,
            _ => {}
        }
    }
    Ok(())
}

/// Run `program` as `silt run` does, on a thread and a VM of its own:
/// `main`, then its tasks to their end, or, when `main` failed, no
/// further (the command ends its process there). `unordered` says that
/// the program names a builtin of [`UNORDERED`]: it reads the system's
/// clock instead of [`Still`], and it may have tasks, whose failures
/// are taken when it has ended.
fn run(program: &Arc<Program>, unordered: bool) -> Run {
    let (stdout, stderr) = (Buffer::new(), Buffer::new());
    let io = HostIo::new(stdout.clone(), stderr.clone());
    let io = match unordered {
        true => io,
        false => io.clock(Still),
    };
    let program = program.clone();
    let (ended, end) = mpsc::sync_channel(1);
    let thread = std::thread::Builder::new()
        .stack_size(STACK_BYTES)
        .spawn(move || {
            silt::scheduler::collect_unjoined_failures();
            let owner = NEXT_OWNER.fetch_add(1, Ordering::SeqCst);
            if unordered {
                GATE.enter();
            }
            let end = catch_unwind(AssertUnwindSafe(|| {
                let mut vm = Vm::new(io);
                vm.set_task_owner(owner);
                let end = match vm.run_program(&program) {
                    Ok(value) => End::Value {
                        shown: format!("{value:?}"),
                        failed: is_err(&value),
                    },
                    Err(error) => End::Error {
                        message: error.message.clone(),
                        type_confusion: error.type_confusion,
                        whole: format!("{error:?}"),
                    },
                };
                match end {
                    End::Value { failed: false, .. } => vm.settle(),
                    _ => vm.stop_tasks(),
                }
                end
            }));
            let end = end.unwrap_or_else(|panic| End::Panic(panic_text(&panic)));
            let failures = match unordered {
                true => {
                    GATE.leave();
                    failures_of(owner)
                }
                false => Vec::new(),
            };
            let _ = ended.send((end, failures));
        })
        .expect("a thread for the run");
    let (end, failures) = match end.recv_timeout(WATCHDOG) {
        Ok(ended) => {
            let _ = thread.join();
            ended
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            if unordered {
                GATE.hung();
            }
            (End::Hang, Vec::new())
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            let end = End::Panic("the run's thread ended without a result".into());
            (end, Vec::new())
        }
    };
    Run {
        stdout: stdout.contents(),
        stderr: stderr.contents(),
        failures,
        end,
    }
}

/// Whether `value` is the `Err(..)` of a `Result`: a `main` that
/// returns one has failed.
fn is_err(value: &Value) -> bool {
    matches!(value, Value::Variant(tag, _) if tag.is(silt::typeinfo::bv::ERR))
}

/// Step 5: what no run may end in.
fn broken(run: &Run) -> Option<Finding> {
    let finding = |kind, detail: &str| Some(Finding::new(kind, detail));
    match &run.end {
        End::Panic(text) => return finding(Kind::Panic, text),
        End::Hang => {
            let detail = format!("no end after {} s", WATCHDOG.as_secs());
            return finding(Kind::Hang, &detail);
        }
        End::Error {
            message,
            type_confusion: true,
            ..
        } => return finding(Kind::TypeConfusion, message),
        End::Error { message, .. } if message.starts_with("internal") => {
            return finding(Kind::InternalError, message);
        }
        End::Error { .. } | End::Value { .. } => {}
    }
    for failure in &run.failures {
        let detail = format!("in a task: {}", failure.message);
        if failure.type_confusion {
            return finding(Kind::TypeConfusion, &detail);
        }
        if failure.message.starts_with("internal") {
            return finding(Kind::InternalError, &detail);
        }
    }
    None
}

/// What of the two runs is not as `expect` says.
fn unexpected(expect: &Expect, first: &Run, second: &Run) -> Option<Finding> {
    for (which, run) in [("first", first), ("second", second)] {
        let wrong = |detail: String| {
            let detail = format!("{which} run: {detail}");
            Some(Finding::new(Kind::Expectation, detail))
        };
        if expect.succeeds {
            match &run.end {
                End::Value { failed: false, .. } => {}
                End::Value { shown, .. } => return wrong(format!("main returned {shown}")),
                End::Error { message, .. } => return wrong(format!("runtime error: {message}")),
                End::Panic(_) | End::Hang => {}
            }
            if let Some(failure) = run.failures.first() {
                return wrong(format!(
                    "a task failed and nobody joined it: {}",
                    failure.message
                ));
            }
            if !run.stderr.is_empty() {
                return wrong(format!("a report on stderr:\n{}", run.stderr));
            }
        }
        let agrees = match (&expect.end, &run.end) {
            (Some(Ok(value)), End::Value { shown, .. }) => *shown == format!("{value:?}"),
            (
                Some(Err(message)),
                End::Error {
                    message: actual, ..
                },
            ) => message == actual,
            (Some(_), End::Value { .. } | End::Error { .. }) => false,
            (None, _) | (_, End::Panic(_) | End::Hang) => true,
        };
        if !agrees {
            let actual = match &run.end {
                End::Value { shown, .. } => format!("main returned {shown}"),
                End::Error { message, .. } => format!("runtime error: {message}"),
                End::Panic(_) | End::Hang => unreachable!("judged before"),
            };
            let expected = match &expect.end {
                Some(Ok(value)) => format!("main returns {value:?}"),
                Some(Err(message)) => format!("runtime error: {message}"),
                None => unreachable!("an end is expected"),
            };
            return wrong(format!("{actual}; expected: {expected}"));
        }
        if let Some(stdout) = &expect.stdout
            && *stdout != run.stdout
        {
            return wrong(format!(
                "stdout is not the expected one:\n{}",
                first_difference(stdout, &run.stdout)
            ));
        }
    }
    None
}

/// Where the two runs differ; `None` when they agree.
fn difference(first: &Run, second: &Run) -> Option<String> {
    if first.stdout != second.stdout {
        let at = first_difference(&first.stdout, &second.stdout);
        return Some(format!("stdout differs:\n{at}"));
    }
    if first.stderr != second.stderr {
        let at = first_difference(&first.stderr, &second.stderr);
        return Some(format!("stderr differs:\n{at}"));
    }
    if first.failures != second.failures {
        return Some(format!(
            "the tasks that failed unjoined differ:\n  {:?}\n  {:?}",
            first.failures, second.failures
        ));
    }
    if first.end != second.end {
        return Some(format!(
            "the runs end differently:\n  {:?}\n  {:?}",
            first.end, second.end
        ));
    }
    None
}

/// The first line that two texts do not share, of each.
fn first_difference(a: &str, b: &str) -> String {
    let (mut left, mut right) = (a.lines(), b.lines());
    let mut line = 1;
    loop {
        match (left.next(), right.next()) {
            (Some(x), Some(y)) if x == y => line += 1,
            (None, None) => return format!("  (the line ends differ, {line} lines)"),
            (x, y) => {
                let show = |text: Option<&str>| match text {
                    Some(text) => format!("{text:?}"),
                    None => "(no line)".to_string(),
                };
                return format!("  line {line}: {}\n  line {line}: {}", show(x), show(y));
            }
        }
    }
}

/// The message a panic was raised with.
fn panic_text(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<&'static str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "(a panic without a message)".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{End, Run, TaskFailure, difference};

    fn run(stdout: &str, stderr: &str, value: &str) -> Run {
        Run {
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            failures: Vec::new(),
            end: End::Value {
                shown: value.to_string(),
                failed: false,
            },
        }
    }

    #[test]
    fn two_runs_agree_only_in_everything() {
        let base = run("a\nb\n", "", "1");
        assert_eq!(difference(&base, &base.clone()), None);
        let stdout = difference(&base, &run("a\nc\n", "", "1")).unwrap();
        assert!(
            stdout.contains("stdout differs") && stdout.contains("line 2"),
            "{stdout}"
        );
        let shorter = difference(&base, &run("a\n", "", "1")).unwrap();
        assert!(shorter.contains("(no line)"), "{shorter}");
        let newline = difference(&base, &run("a\nb", "", "1")).unwrap();
        assert!(newline.contains("the line ends differ"), "{newline}");
        let stderr = difference(&base, &run("a\nb\n", "task 1 failed\n", "1")).unwrap();
        assert!(stderr.contains("stderr differs"), "{stderr}");
        let mut with_failure = base.clone();
        with_failure.failures.push(TaskFailure {
            message: "division by zero".to_string(),
            type_confusion: false,
        });
        let failures = difference(&base, &with_failure).unwrap();
        assert!(failures.contains("failed unjoined differ"), "{failures}");
        let end = difference(&base, &run("a\nb\n", "", "2")).unwrap();
        assert!(end.contains("end differently"), "{end}");
        let mut failed = base.clone();
        failed.end = End::Error {
            message: "division by zero".to_string(),
            type_confusion: false,
            whole: String::new(),
        };
        assert!(difference(&base, &failed).is_some());
    }
}
