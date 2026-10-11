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
//!    into buffers: once in slices of 2000 steps and once in slices of
//!    one step, where the program is stopped and resumed after every
//!    instruction and every step of a builtin that calls back into it
//!    (`Vm::set_time_slice`). The two runs must agree: the same output,
//!    the same failures of tasks that nobody joined, the same value of
//!    `main` or the same error.
//! 5. No run may end in a `type_confusion` error, in an internal error
//!    or in a panic, and no task of it either; and every run ends. A
//!    panic counts on whatever thread it happens: one on a thread of
//!    the scheduler takes that thread and leaves the program waiting,
//!    so the oracle records every panic of the process ([`Panics`]) and
//!    judges a run by the panics recorded while it ran.
//!
//! Each run has a step budget (`Vm::set_step_budget`). A program that
//! uses it up in both runs is cut short: it was held to step 5 as far
//! as it ran, and nothing of it is compared ([`Verdict::Cut`]). One
//! that ends at one slice and not at the other is a finding
//! ([`Kind::OneSlice`]): a fault that keeps a program from going on
//! after a slice boundary looks exactly so. (The run that was cut is
//! first repeated with a larger budget, [`Steps::again`]: a program
//! may need a little more at one slice than at the other.)
//!
//! The order of judgement: what broke step 5 in whatever ran; then a
//! program that ends at one slice only; then one that is cut short at
//! both; then a difference of the two runs; then what is known of the
//! program from elsewhere ([`Expect`]).
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
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Once};
use std::time::{Duration, Instant};

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

/// The builtins that wait for the clock: a run of a program that names
/// one may take as long as the program likes.
pub const TIMED: &[&str] = &[
    "time.sleep",
    "channel.timeout",
    "channel.recv_timeout",
    "task.deadline",
    "task.spawn_until",
];

/// The slices of the two runs.
const SLICES: [usize; 2] = [2000, 1];

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

/// How long a run may take before it counts as hung. A run does a
/// bounded number of steps (its budget), so one that is still there
/// then waits for something that does not come, or sits in a builtin:
/// a finding ([`Kind::Hang`]), unless it is the clock the program
/// waits for ([`Cut::Waiting`]). The run's thread is left behind.
///
/// The time is two minutes and what the steps take on a machine that
/// does 200,000 of them in a second, a tenth of what a debug build does
/// at slice 1.
fn watchdog(steps: u64) -> Duration {
    Duration::from_secs(120 + steps / 200_000)
}

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

/// The step budgets of an input's runs.
#[derive(Debug, Clone, Copy)]
pub struct Steps {
    /// The budget of each of the two runs.
    pub each: u64,
    /// The budget with which a run that was cut short is repeated when
    /// the other one ended; no repeat unless it is larger than `each`.
    pub again: u64,
}

/// What is known of a program's run from elsewhere.
#[derive(Default)]
pub struct Expect {
    /// The program is one that `silt run` runs, and it ends without an
    /// error and with no task that fails unjoined: a golden case's
    /// `cmd: run` and `exit: 0`. So it also checks clean and compiles:
    /// an oracle that says otherwise disagrees with the command.
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
    /// Whether a program that uses tasks or the clock reads the
    /// system's clock, as a golden case does, whose timing is part of
    /// what it shows. Otherwise it reads a clock that leaps
    /// ([`Leaping`]), and its sleeps and timeouts take no time.
    pub real_time: bool,
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
    /// The program ends at one slice and is cut short at the other.
    OneSlice,
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
            Kind::OneSlice => "one-slice",
            Kind::Differs => "differs",
            Kind::Expectation => "expectation",
        }
    }
}

/// A defect the oracle found in a program that checks clean.
#[derive(Debug, Clone)]
pub struct Finding {
    pub kind: Kind,
    /// What the finding is about, in words that do not depend on the
    /// program's values: which part of the runs differs, which
    /// expectation does not hold. Two findings of a kind are the same
    /// finding when this is the same (a shrinker keeps to it).
    pub what: String,
    pub detail: String,
}

impl Finding {
    /// A finding that is about what `detail` says in its first line.
    pub fn new(kind: Kind, detail: impl Into<String>) -> Finding {
        let detail = detail.into();
        let what = detail.lines().next().unwrap_or_default().to_string();
        Finding { kind, what, detail }
    }

    /// A finding about `what`.
    fn about(kind: Kind, what: &str, detail: String) -> Finding {
        Finding {
            kind,
            what: what.to_string(),
            detail,
        }
    }

    /// The finding with the text `before` in front of its detail.
    fn at(mut self, before: &str) -> Finding {
        self.detail = format!("{before}: {}", self.detail);
        self
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

/// Why the runs of a program were not compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Cut {
    /// A run used its step budget up.
    OutOfSteps,
    /// A run was still waiting for the clock when the watchdog looked:
    /// the program names a builtin of [`TIMED`], and nothing says that
    /// it ends.
    Waiting,
}

impl fmt::Display for Cut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Cut::OutOfSteps => write!(f, "out of steps"),
            Cut::Waiting => write!(f, "waiting for the clock"),
        }
    }
}

/// What the oracle makes of an input.
#[derive(Debug, Clone)]
pub enum Verdict {
    NotRun(NotRun),
    /// The runs were held to step 5 as far as they went, and not
    /// compared.
    Cut(Cut),
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
        out_of_steps: bool,
        /// Everything of the error: place and call stack too.
        whole: String,
    },
    Panic(String),
    Hang,
}

impl End {
    /// The end in a few words, for a report.
    fn shown(&self) -> String {
        match self {
            End::Value { shown, .. } => format!("main returned {shown}"),
            End::Error { message, .. } => format!("runtime error: {message}"),
            End::Panic(text) => format!("panic: {text}"),
            End::Hang => "no end".to_string(),
        }
    }
}

/// The error of a task that failed and that nobody joined.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct TaskFailure {
    message: String,
    type_confusion: bool,
    out_of_steps: bool,
}

/// The panics of the process, as the panic hook records them.
///
/// A panic belongs to a run when it happened on the run's own thread,
/// or on a thread without a name while the run was in progress: the
/// threads of the runtime (the scheduler's workers, the timer, the I/O
/// pool) have none, and nothing says which VM one of them serves. So
/// when two runs are in progress side by side, a panic on such a thread
/// is laid to both; a suite in which that happens has failed anyway. A
/// panic on any other named thread (a test's own) is no run's.
#[derive(Default)]
struct Panics {
    /// The name of the thread and what the panic said, in order.
    recorded: Mutex<Vec<(Option<String>, String)>>,
}

static PANICS: Panics = Panics {
    recorded: Mutex::new(Vec::new()),
};

impl Panics {
    fn recorded(&self) -> MutexGuard<'_, Vec<(Option<String>, String)>> {
        self.recorded.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn record(&self, thread: Option<&str>, text: String) {
        self.recorded().push((thread.map(str::to_string), text));
    }

    /// How many panics there have been: what a run remembers when it
    /// starts.
    fn mark(&self) -> usize {
        self.recorded().len()
    }

    /// The panics since `mark` that belong to the run whose thread is
    /// named `run`.
    fn of_run(&self, mark: usize, run: &str) -> Vec<String> {
        let recorded = self.recorded();
        let mine = recorded
            .iter()
            .skip(mark)
            .filter(|(thread, _)| match thread {
                None => true,
                Some(name) => name == run,
            });
        mine.map(|(thread, text)| match thread {
            Some(_) => text.clone(),
            None => format!("on a thread of the runtime: {text}"),
        })
        .collect()
    }
}

/// Have every panic of the process recorded in [`PANICS`], and then
/// handled as before (printed; a fuzz target's hook ends the process).
fn record_panics() {
    static HOOKED: Once = Once::new();
    HOOKED.call_once(|| {
        let before = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let said = match info.payload().downcast_ref::<&'static str>() {
                Some(text) => (*text).to_string(),
                None => match info.payload().downcast_ref::<String>() {
                    Some(text) => text.clone(),
                    None => "(a panic without a message)".to_string(),
                },
            };
            let text = match info.location() {
                Some(at) => format!("{said} ({}:{})", at.file(), at.line()),
                None => said,
            };
            PANICS.record(std::thread::current().name(), text);
            before(info);
        }));
    });
}

/// How long a run in which a panic was recorded is given to end by
/// itself before it is given up: a panic that a builtin caught is an
/// error of the program a moment later; one that took a thread of the
/// scheduler leaves the program waiting for ever.
const AFTER_A_PANIC: Duration = Duration::from_secs(2);

/// What a run left behind.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Run {
    stdout: String,
    /// What the runtime wrote for the program on the host's stderr.
    stderr: String,
    /// The tasks that failed unjoined, sorted: the order in which they
    /// fail is the scheduler's.
    failures: Vec<TaskFailure>,
    /// The panics recorded while it ran ([`Panics::of_run`]).
    panics: Vec<String>,
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
            out_of_steps: failure.error.out_of_steps,
        });
    }
    for (owner, count) in taken.not_kept {
        failed.entry(owner).or_default().push(TaskFailure {
            message: format!("{count} more tasks failed"),
            type_confusion: false,
            out_of_steps: false,
        });
    }
    let mut failures = failed.remove(&owner).unwrap_or_default();
    failures.sort();
    failures
}

/// 2026-01-01T12:00:00Z, the time of day of the oracle's own clocks.
const NOON: Duration = Duration::from_secs(1_767_268_800);

/// A clock that leaps: each reading is a second after the one before,
/// and a sleep is over at once. The runtime's threads look at the clock
/// every millisecond while a deadline is pending on it, so a task's
/// sleep and a timeout end at their next look, however long they are.
/// It never goes back, which is all a program may count on.
#[derive(Default)]
struct Leaping(Mutex<Duration>);

impl Leaping {
    fn passed(&self, more: Duration) -> Duration {
        let mut passed = self.0.lock().unwrap_or_else(|p| p.into_inner());
        *passed = passed.saturating_add(more);
        *passed
    }
}

impl Clock for Leaping {
    fn now(&self) -> Duration {
        NOON.saturating_add(self.passed(Duration::ZERO))
    }

    fn monotonic(&self) -> Duration {
        self.passed(Duration::from_secs(1))
    }

    fn sleep(&self, duration: Duration) {
        self.passed(duration);
    }
}

/// A clock that stands still: the time a program reads when it names
/// no builtin of [`UNORDERED`] (`math.random`'s seed, for one).
struct Still;

impl Clock for Still {
    fn now(&self) -> Duration {
        NOON
    }

    fn monotonic(&self) -> Duration {
        Duration::ZERO
    }

    fn sleep(&self, _duration: Duration) {}
}

/// Put `input` through the oracle, its runs with the budgets `steps`.
/// The work is done on a thread with the native stack of the `silt`
/// command's main thread.
pub fn examine(input: &Input, steps: Steps) -> Verdict {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("oracle-examine".into())
            .stack_size(STACK_BYTES)
            .spawn_scoped(scope, || examine_here(input, steps))
            .expect("a thread for the oracle")
            .join()
            .unwrap_or_else(|panic| Verdict::Finding(Finding::new(Kind::Panic, panic_text(&panic))))
    })
}

fn examine_here(input: &Input, steps: Steps) -> Verdict {
    let finding = |kind, detail: String| Verdict::Finding(Finding::new(kind, detail));
    let prepared = catch_unwind(AssertUnwindSafe(|| prepare(&input.source)));
    let (program, builtins) = match prepared {
        Ok(Ok(prepared)) => prepared,
        // A program that the command runs is one the oracle's session
        // checks and compiles too.
        Ok(Err((Verdict::NotRun(why), said)))
            if input.expect.succeeds && !matches!(why, NotRun::Outside(_)) =>
        {
            let detail = format!("the command runs the program, and the oracle has: {why}: {said}");
            return finding(Kind::Expectation, detail);
        }
        Ok(Err((verdict, _))) => return verdict,
        Err(panic) => {
            let detail = format!("check or compile panicked: {}", panic_text(&panic));
            return finding(Kind::Panic, detail);
        }
    };
    let names = |set: &[&str]| builtins.iter().any(|name| set.contains(&name.as_str()));
    let unordered = builtins
        .iter()
        .any(|name| UNORDERED.contains(&module_of(name)));
    let compared = match unordered || names(RANDOM) {
        true if input.expect.stdout.is_none() => Compared::Invariants,
        _ => Compared::Everything,
    };
    let time = match (unordered, input.real_time) {
        (false, _) => Time::Still,
        (true, true) => Time::System,
        (true, false) => Time::Leaping,
    };
    // A run that waits for the clock when the watchdog looks is the
    // program's doing, unless the program is known to end.
    let may_wait = names(TIMED) && !input.expect.succeeds;
    // How `ran` was cut short, when it was; `Err` when it broke step 5.
    let judge = |slice: usize, ran: &Run| -> Result<Option<Cut>, Verdict> {
        let broke = |broken: Finding| {
            let broken = broken.at(&format!("the run at slice {slice}"));
            Err(Verdict::Finding(broken))
        };
        match broken(ran) {
            Some(broken) => broke(broken),
            None if ran.end == End::Hang && !may_wait => broke(Finding::new(
                Kind::Hang,
                "no end within the watchdog's time",
            )),
            None if ran.end == End::Hang => Ok(Some(Cut::Waiting)),
            None if ran.out_of_steps() => Ok(Some(Cut::OutOfSteps)),
            None => Ok(None),
        }
    };

    let program = Arc::new(program);
    let mut runs = Vec::new();
    let mut cuts = Vec::new();
    for slice in SLICES {
        let ran = run(&program, time, slice, steps.each);
        match judge(slice, &ran) {
            Ok(cut) => cuts.push(cut),
            Err(verdict) => return verdict,
        }
        runs.push(ran);
    }
    // One run ended and the other did not: once more, with more steps.
    if let Some(short) = cuts.iter().position(Option::is_some)
        && cuts.iter().any(Option::is_none)
    {
        let mut tried = steps.each;
        if cuts[short] == Some(Cut::OutOfSteps) && steps.again > steps.each {
            tried = steps.again;
            let again = run(&program, time, SLICES[short], steps.again);
            match judge(SLICES[short], &again) {
                Ok(cut) => cuts[short] = cut,
                Err(verdict) => return verdict,
            }
            runs[short] = again;
        }
        if let Some(cut) = cuts[short] {
            let what = format!("ends at slice {} only", SLICES[1 - short]);
            let detail = format!(
                "the program ends at slice {} ({}) and not at slice {}: {cut} ({tried} steps)",
                SLICES[1 - short],
                runs[1 - short].end.shown(),
                SLICES[short],
            );
            return Verdict::Finding(Finding::about(Kind::OneSlice, &what, detail));
        }
    }
    if let Some(cut) = cuts[0] {
        // Cut short at both slices. A program that is known to end well
        // and whose `main` ended in a fault of its own, while a task
        // used the budget up, is judged by the fault.
        let fault = runs.iter().find(|run| match &run.end {
            End::Error { out_of_steps, .. } => !out_of_steps,
            End::Value { failed, .. } => *failed,
            End::Panic(_) | End::Hang => false,
        });
        return match fault {
            Some(run) if input.expect.succeeds => {
                let fault = Finding::about(Kind::Expectation, "success", run.end.shown());
                Verdict::Finding(fault)
            }
            _ => Verdict::Cut(cut),
        };
    }
    if compared == Compared::Everything
        && let Some((what, detail)) = difference(&runs[0], &runs[1])
    {
        let differs = Finding::about(Kind::Differs, what, detail);
        let slices = format!("slice {} against slice {}", SLICES[0], SLICES[1]);
        return Verdict::Finding(differs.at(&slices));
    }
    if let Some(finding) = unexpected(&input.expect, &runs) {
        return Verdict::Finding(finding);
    }
    Verdict::Passed(compared)
}

/// Steps 1 to 3: the compiled program and the builtins its code names,
/// or why there is nothing to run, with what the session said.
fn prepare(source: &Source) -> Result<(Program, BTreeSet<String>), (Verdict, String)> {
    let not_run = |why, said: String| Err((Verdict::NotRun(why), said));
    let finding = |kind, detail: String| {
        let verdict = Verdict::Finding(Finding::new(kind, detail));
        Err((verdict, String::new()))
    };
    let first = |diagnostics: &[silt::diagnostic::Diagnostic]| {
        let error = diagnostics.iter().find(|d| d.is_error());
        error.map(|d| d.message.clone()).unwrap_or_default()
    };

    let (mut session, entry) = match open(source) {
        Some(opened) => opened,
        None => return not_run(NotRun::Unreadable, String::new()),
    };
    let analysis = session.analyze(entry);
    if analysis.has_errors() {
        return not_run(NotRun::CheckError, first(&analysis.diagnostics));
    }

    let program = match session.compile(entry, Entry::Main) {
        Ok(program) => program,
        Err(errors) => {
            let bug = errors
                .iter()
                .find(|d| d.code == Code::CompilerBug || d.message.starts_with("internal"));
            return match bug {
                Some(bug) => finding(Kind::CompilerBug, bug.message.clone()),
                None => not_run(NotRun::NotCompiled, first(&errors)),
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
        return not_run(NotRun::Outside(module_of(name).to_string()), name.clone());
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

/// The clock of a run.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Time {
    /// [`Still`]: the program names no builtin of [`UNORDERED`].
    Still,
    /// The system's.
    System,
    /// [`Leaping`].
    Leaping,
}

/// Run `program` as `silt run` does, on a thread and a VM of its own,
/// in slices of `slice` steps and for at most `steps` steps: `main`,
/// then its tasks to their end, or, when `main` failed, no further (the
/// command ends its process there). A program on another clock than
/// [`Time::Still`] may have tasks, whose failures are taken when it has
/// ended.
fn run(program: &Arc<Program>, time: Time, slice: usize, steps: u64) -> Run {
    record_panics();
    let (stdout, stderr) = (Buffer::new(), Buffer::new());
    let io = HostIo::new(stdout.clone(), stderr.clone());
    let io = match time {
        Time::Still => io.clock(Still),
        Time::System => io,
        Time::Leaping => io.clock(Leaping::default()),
    };
    let tasks = time != Time::Still;
    let program = program.clone();
    let owner = NEXT_OWNER.fetch_add(1, Ordering::SeqCst);
    let name = format!("oracle-run-{owner}");
    let mark = PANICS.mark();
    let (ended, end) = mpsc::sync_channel(1);
    let thread = std::thread::Builder::new()
        .name(name.clone())
        .stack_size(STACK_BYTES)
        .spawn(move || {
            silt::scheduler::collect_unjoined_failures();
            if tasks {
                GATE.enter();
            }
            let end = catch_unwind(AssertUnwindSafe(|| {
                let mut vm = Vm::new(io);
                vm.set_task_owner(owner);
                vm.set_time_slice(slice);
                vm.set_step_budget(steps);
                let end = match vm.run_program(&program) {
                    Ok(value) => End::Value {
                        shown: format!("{value:?}"),
                        failed: is_err(&value),
                    },
                    Err(error) => End::Error {
                        message: error.message.clone(),
                        type_confusion: error.type_confusion,
                        out_of_steps: error.out_of_steps,
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
            let failures = match tasks {
                true => {
                    GATE.leave();
                    failures_of(owner)
                }
                false => Vec::new(),
            };
            let _ = ended.send((end, failures));
        })
        .expect("a thread for the run");
    // The run is waited for in short looks, so that one in which a
    // thread of the runtime has panicked is not waited out.
    let started = Instant::now();
    let mut panicked = None;
    let given_up = |end: End| {
        if tasks {
            GATE.hung();
        }
        (end, Vec::new())
    };
    let (end, failures) = loop {
        match end.recv_timeout(Duration::from_millis(20)) {
            Ok(ended) => {
                let _ = thread.join();
                break ended;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let end = End::Panic("the run's thread ended without a result".into());
                break (end, Vec::new());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if panicked.is_none() && !PANICS.of_run(mark, &name).is_empty() {
            panicked = Some(Instant::now());
        }
        if panicked.is_some_and(|at| at.elapsed() > AFTER_A_PANIC) {
            break given_up(End::Hang);
        }
        if started.elapsed() > watchdog(steps) {
            break given_up(End::Hang);
        }
    };
    Run {
        stdout: stdout.contents(),
        stderr: stderr.contents(),
        failures,
        panics: PANICS.of_run(mark, &name),
        end,
    }
}

impl Run {
    /// Whether the run, or a task of it, was ended by the step budget.
    fn out_of_steps(&self) -> bool {
        let main = matches!(
            self.end,
            End::Error {
                out_of_steps: true,
                ..
            }
        );
        main || self.failures.iter().any(|failure| failure.out_of_steps)
    }
}

/// Whether `value` is the `Err(..)` of a `Result`: a `main` that
/// returns one has failed.
fn is_err(value: &Value) -> bool {
    matches!(value, Value::Variant(variant) if variant.is(silt::typeinfo::bv::ERR))
}

/// Step 5, but for a run that did not end, which the caller judges:
/// what no run may end in.
fn broken(run: &Run) -> Option<Finding> {
    let finding = |kind, detail: &str| Some(Finding::new(kind, detail));
    // A panic first: what else the run shows (that it did not end, as
    // a rule) follows from it.
    if let Some(panic) = run.panics.first() {
        return finding(Kind::Panic, panic);
    }
    match &run.end {
        End::Panic(text) => return finding(Kind::Panic, text),
        End::Error {
            message,
            type_confusion: true,
            ..
        } => return finding(Kind::TypeConfusion, message),
        End::Error { message, .. } if message.starts_with("internal") => {
            return finding(Kind::InternalError, message);
        }
        End::Error { .. } | End::Value { .. } | End::Hang => {}
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

/// What of the runs is not as `expect` says.
fn unexpected(expect: &Expect, runs: &[Run]) -> Option<Finding> {
    for (slice, run) in SLICES.iter().zip(runs) {
        let wrong = |what: &str, detail: String| {
            let finding = Finding::about(Kind::Expectation, what, detail);
            Some(finding.at(&format!("the run at slice {slice}")))
        };
        if expect.succeeds {
            match &run.end {
                End::Value { failed: false, .. } => {}
                End::Value { .. } | End::Error { .. } => return wrong("success", run.end.shown()),
                End::Panic(_) | End::Hang => {}
            }
            if let Some(failure) = run.failures.first() {
                let detail = format!("a task failed and nobody joined it: {}", failure.message);
                return wrong("success", detail);
            }
            if !run.stderr.is_empty() {
                return wrong("success", format!("a report on stderr:\n{}", run.stderr));
            }
        }
        // What of the end is not the expected one.
        let what = match (&expect.end, &run.end) {
            (Some(Ok(value)), End::Value { shown, .. }) => {
                (*shown != format!("{value:?}")).then_some("main's value")
            }
            (
                Some(Err(message)),
                End::Error {
                    message: actual, ..
                },
            ) => (message != actual).then_some("the runtime error"),
            (Some(Ok(_)), End::Error { .. }) => Some("a runtime error for a value"),
            (Some(Err(_)), End::Value { .. }) => Some("a value for a runtime error"),
            (None, _) | (_, End::Panic(_) | End::Hang) => None,
        };
        if let Some(what) = what {
            let expected = match &expect.end {
                Some(Ok(value)) => format!("main returns {value:?}"),
                Some(Err(message)) => format!("runtime error: {message}"),
                None => unreachable!("an end is expected"),
            };
            return wrong(what, format!("{}; expected: {expected}", run.end.shown()));
        }
        if let Some(stdout) = &expect.stdout
            && *stdout != run.stdout
        {
            let at = first_difference(stdout, &run.stdout);
            return wrong("stdout", format!("stdout is not the expected one:\n{at}"));
        }
    }
    None
}

/// Which part of the two runs differs, and how; `None` when they
/// agree.
fn difference(first: &Run, second: &Run) -> Option<(&'static str, String)> {
    if first.stdout != second.stdout {
        let at = first_difference(&first.stdout, &second.stdout);
        return Some(("stdout", format!("stdout differs:\n{at}")));
    }
    if first.stderr != second.stderr {
        let at = first_difference(&first.stderr, &second.stderr);
        return Some(("stderr", format!("stderr differs:\n{at}")));
    }
    if first.failures != second.failures {
        let detail = format!(
            "the tasks that failed unjoined differ:\n  {:?}\n  {:?}",
            first.failures, second.failures
        );
        return Some(("the unjoined failures", detail));
    }
    if first.end != second.end {
        let detail = format!(
            "the runs end differently:\n  {:?}\n  {:?}",
            first.end, second.end
        );
        return Some(("the end", detail));
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
    use super::{End, Kind, PANICS, Panics, Run, TaskFailure, broken, difference, record_panics};

    fn run(stdout: &str, stderr: &str, value: &str) -> Run {
        Run {
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            failures: Vec::new(),
            panics: Vec::new(),
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
        let (what, stdout) = difference(&base, &run("a\nc\n", "", "1")).unwrap();
        assert_eq!(what, "stdout");
        assert!(
            stdout.contains("stdout differs") && stdout.contains("line 2"),
            "{stdout}"
        );
        let (_, shorter) = difference(&base, &run("a\n", "", "1")).unwrap();
        assert!(shorter.contains("(no line)"), "{shorter}");
        let (_, newline) = difference(&base, &run("a\nb", "", "1")).unwrap();
        assert!(newline.contains("the line ends differ"), "{newline}");
        let (what, _) = difference(&base, &run("a\nb\n", "task 1 failed\n", "1")).unwrap();
        assert_eq!(what, "stderr");
        let mut with_failure = base.clone();
        with_failure.failures.push(TaskFailure {
            message: "division by zero".to_string(),
            type_confusion: false,
            out_of_steps: false,
        });
        let (what, _) = difference(&base, &with_failure).unwrap();
        assert_eq!(what, "the unjoined failures");
        let (what, _) = difference(&base, &run("a\nb\n", "", "2")).unwrap();
        assert_eq!(what, "the end");
        let mut failed = base.clone();
        failed.end = End::Error {
            message: "division by zero".to_string(),
            type_confusion: false,
            out_of_steps: false,
            whole: String::new(),
        };
        assert!(difference(&base, &failed).is_some());
    }

    /// A panic belongs to the run on whose thread it happened, and to
    /// every run in progress when the thread has no name; a panic on
    /// another named thread is no run's.
    #[test]
    fn a_panic_is_laid_to_the_runs_it_may_be_of() {
        let panics = Panics::default();
        panics.record(Some("oracle-run-1"), "before the mark".into());
        let mark = panics.mark();
        panics.record(Some("oracle-run-7"), "in the run".into());
        panics.record(Some("oracle-run-8"), "in another run".into());
        panics.record(Some("selfcheck::a_test"), "in a test".into());
        panics.record(None, "in a worker".into());
        assert_eq!(
            panics.of_run(mark, "oracle-run-7"),
            ["in the run", "on a thread of the runtime: in a worker"]
        );
        assert_eq!(
            panics.of_run(mark, "oracle-run-1"),
            ["on a thread of the runtime: in a worker"]
        );
        assert!(panics.of_run(panics.mark(), "oracle-run-7").is_empty());
    }

    /// A run with a recorded panic is a `panic` finding, whatever its
    /// end: one that ended well, and one that did not end.
    #[test]
    fn a_run_with_a_recorded_panic_is_judged_by_it() {
        let mut ended = run("out\n", "", "1");
        assert!(broken(&ended).is_none());
        ended.panics.push("on a thread of the runtime: boom".into());
        let finding = broken(&ended).unwrap();
        assert_eq!(finding.kind, Kind::Panic);
        assert!(finding.detail.contains("boom"), "{}", finding.detail);
        let mut hung = ended.clone();
        hung.end = End::Hang;
        assert_eq!(broken(&hung).unwrap().kind, Kind::Panic);
        // (Without a panic, a run that did not end is the caller's to
        // judge: it may be the clock the program waits for.)
        hung.panics.clear();
        assert!(broken(&hung).is_none());
    }

    /// The hook records a panic of any thread of the process, with the
    /// thread's name and the place. (The thread here has a name that no
    /// run has, so no run of a test beside this one is judged by it.)
    #[test]
    fn the_hook_records_a_panic_on_any_thread() {
        record_panics();
        let mark = PANICS.mark();
        let thread = std::thread::Builder::new()
            .name("oracle-hook-test".into())
            .spawn(|| panic!("a panic for the hook"))
            .unwrap();
        assert!(thread.join().is_err());
        let recorded = PANICS.of_run(mark, "oracle-hook-test");
        assert_eq!(recorded.len(), 1, "{recorded:?}");
        assert!(
            recorded[0].starts_with("a panic for the hook (tests"),
            "{recorded:?}"
        );
    }
}
