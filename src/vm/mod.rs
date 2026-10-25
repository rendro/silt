//! Stack-based bytecode VM for Silt.
//!
//! Executes compiled `Function` objects produced by the compiler.

mod arithmetic;
mod calls;
pub(crate) mod decode;
mod dicts;
pub(crate) mod dispatch;
mod encode;
pub mod error;
mod io;
mod io_op;
mod iter;
mod run;
mod runtime;
mod show;

use std::fmt::Write as _;

use show::write_plain;

pub use error::VmError;
pub use io::{Buffer, Clock, HostIo, Output, SystemClock};
pub(crate) use io::{NO_PANIC_TEXT, panic_message};
pub use io_op::IoFailure;
pub(crate) use io_op::{ErrFactory, IoOp, Leave, Limit, Wanted};
pub(crate) use iter::{Flow, call_then, item_arg, iterate, next, stop};
pub use runtime::Runtime;
pub(crate) use runtime::{CallFrame, Frame, Native, Step};

/// Test-only: what a test is told of the writes of a connection that
/// take their turn, and how it holds one of them in its turn.
#[cfg(any(test, feature = "test-hooks"))]
pub use crate::runtime::handle::{WriteMoment, WriteWatching, watch_writes};

#[cfg(any(test, feature = "test-hooks"))]
thread_local! {
    static SET_AGAIN: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Test-only: how often a VM of this thread has set a global slot that
/// had a value (`Vm::set_global`): a program run again on one VM does
/// it, a REPL session never.
#[cfg(any(test, feature = "test-hooks"))]
pub fn slots_set_again() -> u64 {
    SET_AGAIN.with(std::cell::Cell::get)
}

/// Note that a slot that had a value was set ([`slots_set_again`]).
fn count_set_again() {
    #[cfg(any(test, feature = "test-hooks"))]
    SET_AGAIN.with(|count| count.set(count.get() + 1));
}

/// Test-only: how many threads the I/O pool of this VM has: those
/// that run an operation somebody waits for, and those that wait for
/// work.
#[cfg(any(test, feature = "test-hooks"))]
pub fn io_pool_threads(vm: &Vm) -> usize {
    vm.runtime.io_pool.threads()
}

/// Test-only: how many threads of the I/O pool of this VM exist. It
/// equals [`io_pool_threads`] unless a thread still runs an operation
/// that nobody waits for: such a thread has not ended.
#[cfg(any(test, feature = "test-hooks"))]
pub fn io_pool_live_threads(vm: &Vm) -> usize {
    vm.runtime.io_pool.live_threads()
}

/// Test-only: a place in the runtime's own code where a test makes it
/// panic ([`panic_for_test`]).
#[cfg(any(test, feature = "test-hooks"))]
pub use crate::scheduler::PanicAt;

/// Test-only: the runtime of this VM panics once at `at`, in its own
/// code, outside every catch of a builtin's or a host function's
/// panic: what no silt program can make it do. It is how the tests
/// see what a panic of the runtime does to a program. (One place,
/// [`PanicAt::Builtin`], is inside such a catch: for what a panic of a
/// builtin's own code does.)
#[cfg(any(test, feature = "test-hooks"))]
pub fn panic_for_test(vm: &Vm, at: PanicAt) {
    match at {
        PanicAt::Io => vm.runtime.io_pool.panic_for_test(),
        at => vm.runtime.scheduler.panic_for_test(at),
    }
}

/// Test-only: run a panicking operation on this VM's I/O pool, wait
/// for it, and return the value it completes with: `failure` of the
/// panic's message, with a "panic: " prefix. `failure` is the typed
/// error of a builtin module, so the value has the shape that module's
/// callers match on.
#[cfg(any(test, feature = "test-hooks"))]
pub fn submit_panicking_io_for_test(vm: &Vm, failure: fn(IoFailure<'_>) -> Value) -> Value {
    let op = vm
        .runtime
        .io_pool
        .submit(failure, Vec::new(), io_op::Leave::RunsOut, |_| {
            panic!("synthetic IO worker panic for a test");
        });
    let wait = op.wait(vm, Limit::None);
    let _ = vm
        .runtime
        .scheduler
        .block_thread(wait, crate::scheduler::Blocks::Thread);
    op.cell.get().cloned().expect("the operation has ended")
}

use regex::Regex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use crate::bytecode::{Function, Globals, VmClosure};
use crate::runtime::sync::Fired;
use crate::scheduler::Scheduler;
use crate::value::Value;
use io_op::IoPool;
use runtime::{RegexCache, TailCalls};

// ── VM ────────────────────────────────────────────────────────────

pub struct Vm {
    pub(crate) runtime: Arc<Runtime>,
    /// True for the VM made by [`Vm::new`], false for the VMs of its
    /// tasks: when that VM is dropped, the runtime's threads end.
    owns_runtime: bool,
    /// The calls in progress, the innermost last: functions, and
    /// builtins that call functions or wait ([`Native`]).
    pub(crate) frames: Vec<Frame>,
    /// How many of the frames are builtins'.
    native_frames: usize,
    /// The value the builtin's frame on top is resumed with, when a
    /// slice ended between the value and the frame.
    pending_input: Option<Value>,
    /// The second part of the instruction before the `ip` of the
    /// function's frame on top, when a slice ended between the two
    /// parts of an instruction that fuses two (`run::Half`).
    half: Option<run::Half>,
    pub(crate) stack: Vec<Value>,
    /// The arguments of the builtin being called, off the stack for the
    /// time of the call ([`Vm::with_args`]). Between calls it is empty
    /// and kept for the next one; a VM that waits gives it up.
    builtin_args: Vec<Value>,
    /// The values of the program's global slots and what each slot is,
    /// as this VM has them. The VMs of the program's tasks have the same
    /// slots and tables, not a copy of them: a task starts with nothing
    /// to copy, however many definitions the program has.
    pub(crate) view: runtime::View,
    /// This VM's own copies of the closures of the global slots it has
    /// read: what it calls in their place ([`runtime::OwnClosures`]).
    pub(crate) own: runtime::OwnClosures,

    // ── Concurrency state ────────────────────────────────────────
    next_channel_id: Arc<AtomicU64>,
    next_task_id: Arc<AtomicU64>,

    // ── M:N scheduler state ─────────────────────────────────────
    /// How the wait of the builtin's frame on top ended, between the
    /// end of the wait and the frame's resumption ([`Step::Park`]).
    pub(crate) woken: Option<Fired>,
    /// True for the VM of a task made by `task.spawn`.
    pub(crate) spawned: bool,
    /// The cancel flag of the task this is the VM of; `None` for the
    /// VM that runs the program itself. Set when the task is started
    /// (`Scheduler::submit`).
    pub(crate) cancelled: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// When the task this is the VM of is over (`TaskHandle::deadline`):
    /// the VM of a function that `task.deadline` runs has one. A
    /// `task.deadline` inside it gives its function no later time.
    pub(crate) deadline: Option<std::time::Duration>,
    /// The count of the steps the task has run past that time
    /// (`TaskHandle::late`): the function of a `task.deadline` inside
    /// it that has the same time goes on with it.
    pub(crate) late: Option<Arc<std::sync::atomic::AtomicUsize>>,

    /// The callers that tail calls replaced, for an error's call stack
    /// ([`TailCalls`]).
    pub(crate) tco_elided: TailCalls,

    // ── Caches ──────────────────────────────────────────────────
    /// Cache for compiled regex patterns (bounded, FIFO eviction —
    /// first-in first-out: the oldest 25% of entries are evicted when
    /// the cache reaches `MAX_ENTRIES`. `RegexCache::get` only pushes
    /// to its order deque on miss; a hit does not re-order, so this
    /// is pure insertion order).
    pub(crate) regex_cache: RegexCache,

    // ── What the loop does not read ─────────────────────────────
    // (Declared last: the fields above are where they were before
    // these were added, which the loop's speed was found to hang on.)
    /// Which of the views the program's VM has published `view` is
    /// ([`runtime::Newest::count`]).
    view_count: u64,
    /// The table the VMs of this VM's tasks look their methods up in:
    /// for the VM of a task its own; for the VM that runs a REPL entry
    /// what stands of the program whatever becomes of the entry.
    settled: Arc<Globals>,
    /// Set by the scheduler when a slice of the task begins past its
    /// deadline (`TaskHandle::set_over`): the task takes nothing new
    /// from then on ([`Vm::takes_nothing_new`]). `None` for a VM whose
    /// task has no deadline.
    pub(crate) over: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Where `Interpolate` writes the string it makes (src/vm/run.rs),
    /// kept from one to the next: the string itself is then the one
    /// allocation an interpolation makes.
    text: String,
}

/// How many steps the program's own thread runs before it looks
/// whether a thread of the runtime has panicked ([`Vm::own_slice`]):
/// about a million, some milliseconds.
const OWN_SLICE: usize = 1 << 20;

impl Drop for Vm {
    fn drop(&mut self) {
        // The calls in progress end here, however the VM ends: a task
        // that failed, was cancelled or was still waiting when the
        // program ended. Each builtin's frame is told ([`Native::abandon`]).
        let end = |vm: &mut Vm| {
            vm.unwind(0, 0);
            if vm.owns_runtime {
                vm.runtime.shutdown();
            }
        };
        if !self.runtime.scheduler.is_broken() && !std::thread::panicking() {
            return end(self);
        }
        // A thread of the runtime has panicked, and that panic is on
        // its way to the embedder. This runs over state that may be
        // half updated: a second panic here, while the embedder's
        // thread unwinds, would abort its process, so it is dropped.
        // The same on the thread that is unwinding from the first
        // panic, which drops the VM of the task it was running.
        let _ = crate::scheduler::catch_as_error(std::panic::AssertUnwindSafe(|| {
            #[cfg(any(test, feature = "test-hooks"))]
            self.runtime.scheduler.panic_while_unwinding_for_test();
            end(self)
        }));
    }
}

impl Vm {
    /// A VM whose programs write to the output of `io` and read its
    /// clock.
    ///
    /// Dropping it ends the program: the threads that served it (the
    /// scheduler's, the timer's, the I/O workers) end, tasks that are
    /// still running or waiting never run again, and the tasks that
    /// failed since the last report and that nobody joined are reported
    /// on the stderr of `io`.
    pub fn new(io: HostIo) -> Self {
        let scheduler = Arc::new(Scheduler::new(io.clone()));
        Vm {
            runtime: Arc::new(Runtime {
                io_pool: IoPool::new(scheduler.clone()),
                scheduler,
                io,
                time_slice: Default::default(),
                steps_left: AtomicU64::new(u64::MAX),
                newest: Default::default(),
                types: Default::default(),
                refusals: Default::default(),
                rng: parking_lot::Mutex::new(None),
                uuid_v7: std::sync::Mutex::new(uuid::ContextV7::new()),
            }),
            owns_runtime: true,
            frames: Vec::new(),
            pending_input: None,
            half: None,
            native_frames: 0,
            stack: Vec::new(),
            builtin_args: Vec::new(),
            view: Default::default(),
            own: Default::default(),
            view_count: 0,
            settled: Default::default(),
            next_channel_id: Arc::new(AtomicU64::new(0)),
            next_task_id: Arc::new(AtomicU64::new(0)),
            woken: None,
            spawned: false,
            cancelled: None,
            deadline: None,
            late: None,
            over: None,
            text: String::new(),
            regex_cache: RegexCache::new(),
            tco_elided: TailCalls::default(),
        }
    }

    /// Run the program in slices of `steps` steps (at least 1): a task
    /// gives way to the other tasks after that many, instead of the
    /// scheduler's own 2000, and the thread that runs the program's own
    /// code (`fn main`, a test) stops and goes on after that many too,
    /// where it otherwise does so about every million steps
    /// (`Vm::own_slice`).
    ///
    /// What a program computes does not depend on the slice. An embedder
    /// sets one to see that: with `steps` 1 the program is stopped and
    /// resumed after every instruction and after every step of a builtin
    /// that calls back into it. It holds for every program the VM runs
    /// from then on, and for the tasks it has.
    pub fn set_time_slice(&mut self, steps: usize) {
        self.runtime
            .time_slice
            .store(steps.max(1), Ordering::Relaxed);
    }

    /// The slice an embedder set ([`Vm::set_time_slice`]).
    pub(crate) fn time_slice(&self) -> Option<usize> {
        match self.runtime.time_slice.load(Ordering::Relaxed) {
            0 => None,
            steps => Some(steps),
        }
    }

    /// Let the program run `steps` more steps, those of its tasks
    /// included. When they are used up, whatever of the program still
    /// runs ends with an error whose [`VmError::out_of_steps`] is set
    /// (`main`, and each task: a join of one gives the error on): a
    /// program that does not end by itself ends there. A deadlock of
    /// the program's own code from then on has it set too: the task it
    /// waited for was ended. A fault of the program's own (a division
    /// by zero, a `panic`) does not, whatever its tasks did to the
    /// budget.
    ///
    /// A step is an instruction, or a step of a builtin that calls back
    /// into the program. They are counted where a slice has run its
    /// full length, and no slice starts once they are used up: so each
    /// thread of the program may run up to one slice more than the
    /// budget (1,999 steps at the scheduler's slice of 2,000, with
    /// which a budget of 1 runs a short program to its end), the count
    /// is exact at slice 1, and a budget of 0 runs nothing. With a
    /// budget the thread that runs the program's own code is sliced as
    /// the tasks are (see [`Vm::set_time_slice`]). The start of a wait counts as
    /// one step, so tasks that hand a value to and fro for ever end
    /// too; a wait that never ends is not ended by the budget.
    pub fn set_step_budget(&mut self, steps: u64) {
        let steps = steps.min(u64::MAX - 1);
        self.runtime.steps_left.store(steps, Ordering::Relaxed);
    }

    /// How many steps the thread that runs the program's own code runs
    /// at a time: [`OWN_SLICE`], unless an embedder set a slice or a
    /// step budget. The thread gives way to nobody between two such
    /// runs; it looks whether a thread of the runtime has panicked
    /// (`Vm::run_thread`), which a thread that computes and never
    /// waits would not learn otherwise.
    pub(crate) fn own_slice(&self) -> usize {
        match self.time_slice() {
            Some(steps) => steps,
            None if self.runtime.steps_left.load(Ordering::Relaxed) != u64::MAX => {
                crate::scheduler::time_slice()
            }
            None => OWN_SLICE,
        }
    }

    /// Report on the host's stderr the tasks that have failed so far
    /// and that nobody joined or cancelled. Nothing while a front end
    /// collects them (`scheduler::collect_unjoined_failures`).
    fn report_unjoined_failures(&self) {
        self.runtime.scheduler.report_unjoined_failures();
    }

    /// Run a compiled program: take in its tables, then run its script.
    /// The value is the script's: `main`'s for a program compiled for
    /// `Entry::Main`. This is the one way to start a [`Program`]; the
    /// test functions of a program compiled for its tests are called
    /// with [`Vm::call_test`] afterwards. A REPL session runs each entry's program
    /// on one `Vm`.
    ///
    /// [`Program`]: crate::session::Program
    pub fn run_program(&mut self, program: &crate::session::Program) -> Result<Value, VmError> {
        self.load(program);
        let script =
            program.functions.first().cloned().ok_or_else(|| {
                VmError::new("internal VM error: a program without a script".into())
            })?;
        let result = self.run(Arc::new(script));
        self.report_unjoined_failures();
        result
    }

    /// Wait until the program has ended. [`Vm::run_program`] and
    /// [`Vm::call_test`] return when the program's own code (`fn main`,
    /// the test function) has returned; the tasks it spawned may still
    /// run. The program has ended when none of them can go on: each
    /// has ended or waits, and no timer and no I/O operation of theirs
    /// is pending. The tasks that still wait then are dropped, and the
    /// failures that nobody joined are final.
    ///
    /// `silt run` and `silt test` call this before they give the
    /// program's result, so a task that fails after `main` has
    /// returned still makes the run fail, and a task that never ends
    /// (an endless loop, a sleep) keeps the program from ending. An
    /// embedder that wants neither does not call it, and drops the VM
    /// to end the program where it is.
    ///
    /// The tasks waited for are those of the current owner
    /// ([`Vm::set_task_owner`]).
    pub fn settle(&mut self) {
        self.runtime.scheduler.settle();
        self.report_unjoined_failures();
    }

    /// Set the owner tag of the tasks that this VM's own code spawns
    /// from now on (`fn main`, a test function; not the code of a
    /// task). A task spawned by a task gets the owner of the task that
    /// spawns it, so one tag covers every task that descends from the
    /// tasks spawned under it. 0, the default, means no owner.
    ///
    /// The owner is whose tasks the VM asks about: a deadlock is one
    /// of the owner's tasks, [`Vm::settle`] waits for them and no
    /// others, and the report of a task's failure carries the tag
    /// ([`crate::scheduler::UnjoinedFailure::owner`]). `silt test`
    /// sets one tag per test, and so judges each test by its own
    /// tasks. The tag is the VM's: two VMs do not see each other's.
    pub fn set_task_owner(&mut self, owner: u64) {
        self.runtime.scheduler.set_owner(owner, 0);
    }

    /// [`Vm::set_task_owner`], for code that runs while the tasks of
    /// `outer` are still there and may work for it: a test, and the
    /// tasks that the top-level code of its file left waiting. Those
    /// count with the owner's when the VM waits: it is not deadlocked
    /// while one of them can go on, and [`Vm::settle`] waits for them
    /// too. They are not dropped with the owner's, and their failures
    /// stay theirs.
    pub fn set_task_owner_within(&mut self, owner: u64, outer: u64) {
        self.runtime.scheduler.set_owner(owner, outer);
    }

    /// End the program whose own code has failed: stop its tasks
    /// where they are, and return when all have ended. One that waits
    /// is dropped; one that runs ends with the slice it is in. Nothing
    /// is waited for, as after an error of `main` under `silt run`,
    /// where the process ends; `silt test` calls this after a test
    /// that failed, since the VM goes on to the next test.
    pub fn stop_tasks(&mut self) {
        self.runtime.scheduler.stop_tasks();
        self.report_unjoined_failures();
    }

    /// Wait as [`Vm::settle`] does, and drop nothing: the tasks that
    /// wait stay, for code that this VM runs next and that may wake
    /// them. `silt test` calls this after a file's top-level code, the
    /// tasks of which may serve the file's tests
    /// ([`Vm::set_task_owner_within`]).
    pub fn wait_until_idle(&mut self) {
        self.runtime.scheduler.wait_until_idle();
        self.report_unjoined_failures();
    }

    /// Call the test function `test` of the program this VM ran with
    /// [`Vm::run_program`] (compiled for `Entry::Tests`), with no
    /// arguments, and give its value.
    pub fn call_test(&mut self, test: &crate::session::TestFn) -> Result<Value, VmError> {
        let result = self.run(Arc::new(crate::bytecode::call_global_script(
            test.slot, &test.name,
        )));
        self.report_unjoined_failures();
        result
    }

    /// Take in a program about to run: the descriptions its values'
    /// types carry, which the decoders look up by id, and its global
    /// slots and impl methods. A REPL entry's program has the slots of
    /// the entries before it too, whose values stay.
    pub(crate) fn load(&mut self, program: &crate::session::Program) {
        self.runtime.types.lock().extend(&program.types);
        // (A description taken in anew may differ from the one a decoder
        // was decided by.)
        self.runtime.refusals.write().clear();
        self.view.table = program.globals.clone();
        self.view.entry = program.globals.clone();
        self.settled = program.settled.clone();
        // More slots than the VM has room for: twice the room, with
        // the values it has, so that a session's entries copy each
        // value a bounded number of times however many they are. (A
        // program that is run once gets the slots it has.)
        let slots = program.globals.len();
        if slots > self.view.slots.len() {
            let room = slots.max(2 * self.view.slots.len());
            crate::layers::count_copied(self.view.slots.len());
            let unset = std::iter::repeat_with(OnceLock::new);
            self.view.slots = (self.view.slots.iter().cloned())
                .chain(unset)
                .take(room)
                .collect();
        }
        let runtime = self.runtime.clone();
        self.publish(&mut runtime.newest.view.lock());
    }

    /// Make this VM's slots and tables the newest of the program, for
    /// the VMs of its tasks: `newest` is the runtime's view.
    fn publish(&mut self, newest: &mut runtime::View) {
        *newest = self.view_of_a_task();
        self.view_count = self.runtime.newest.count.fetch_add(1, Ordering::Relaxed) + 1;
    }

    /// What the VM of a task of this VM has of the program.
    fn view_of_a_task(&self) -> runtime::View {
        runtime::View {
            slots: self.view.slots.clone(),
            table: self.settled.clone(),
            entry: self.view.entry.clone(),
        }
    }

    /// Whether the program has slots or tables that this VM does not
    /// have yet: if so the VM has them now, and the lookup that failed
    /// is to be made again. Called where a lookup has failed, and
    /// nowhere else: a global slot that is not there or not set
    /// ([`runtime::View::global_of_newest`]), a method that no impl of
    /// the VM's table has for the receiver's type, a record or variant
    /// that no `Display` impl of the table is for.
    ///
    /// The VM of a task started in an earlier REPL entry comes here
    /// when it runs code or meets a value of a later entry (a closure
    /// it was sent through a channel): it then sees what the session's
    /// own code sees.
    ///
    /// Looking only after a failure misses nothing, because what a VM
    /// has found once never changes: a session only ever ADDS to the
    /// slots and to the table the VMs of its tasks keep.
    /// - A slot is set once. A definition that is made again gets a
    ///   slot of its own: an entry's definitions are new ones; and what
    ///   an entry that FAILED when it ran had defined (the entry's own
    ///   impls, a module it imported) is defined in new slots when it
    ///   is entered again, because the session takes the failed entry's
    ///   definitions back (`Globals::without_definitions_since`). The
    ///   failed entry's slots keep their values for the code of that
    ///   entry that is still about (a closure it sent away), and no
    ///   later code names them.
    /// - The table a task's VM keeps ([`runtime::View::table`]) holds
    ///   what entries that ran to their end defined, never what the
    ///   entry being run defines: that it finds in
    ///   [`runtime::View::entry`], which it asks only after this
    ///   function, each time. So an entry that fails leaves nothing in
    ///   a table that a VM keeps.
    /// - A second impl of a trait for a type is refused when it is
    ///   checked (`duplicate implementation of trait`), in a later
    ///   entry as in one file.
    ///
    /// The REPL tests `a_second_impl_in_a_later_entry_is_refused` and
    /// `a_task_reads_what_an_import_that_failed_once_defines` hold the
    /// last two, `a_session_sets_no_slot_twice` the first.
    ///
    /// What a task learns of a later entry it learns from a value the
    /// session's code handed it after the entry's program was taken in
    /// (`load`), and the hand-over (a channel's lock, a join) orders
    /// the two: the count read here is then the new one.
    pub(crate) fn look_again(&mut self) -> bool {
        (self.view).renew(&mut self.view_count, &self.runtime.newest)
    }

    /// The value of the global slot `slot`, if it is set.
    pub(crate) fn global(&self, slot: u16) -> Option<&Value> {
        self.view.global(slot)
    }

    /// Set the global slot `slot`. `false` if the VM has no such slot.
    pub(crate) fn set_global(&mut self, slot: u16, value: Value) -> bool {
        let Some(unset) = self.view.slots.get(usize::from(slot)) else {
            return false;
        };
        // A slot is set once in the life of a program, and every VM of
        // the program sees it. One that has a value has it from a
        // program this VM ran before, whose slots the program run now
        // was compiled without knowing of: an embedder runs a program
        // again on one VM, or another program after it. This VM then
        // goes on with slots of its own, and a task of the earlier run
        // keeps the ones it has. (A REPL session never comes here: each
        // of its entries is compiled onto the slots of all before it,
        // and none is given out twice; `slots_set_again` counts.)
        if let Err(value) = unset.set(value) {
            count_set_again();
            self.own.forget(slot);
            let again = OnceLock::from(value);
            let runtime = self.runtime.clone();
            let mut newest = runtime.newest.view.lock();
            // (The slots the runtime has for the tasks are these: with
            // no task holding them, they are set again where they are.)
            newest.slots = Arc::from([]);
            match Arc::get_mut(&mut self.view.slots) {
                Some(slots) => slots[usize::from(slot)] = again,
                None => {
                    let mut slots: Vec<OnceLock<Value>> = self.view.slots.iter().cloned().collect();
                    slots[usize::from(slot)] = again;
                    self.view.slots = slots.into();
                }
            }
            self.publish(&mut newest);
        }
        true
    }

    /// Create a child VM that shares runtime state (the scheduler, timers, the I/O pool)
    /// and the program's (its global slots, its types) via Arc.
    /// Used for thread-per-task spawning.
    pub(crate) fn spawn_child(&self) -> Self {
        Vm {
            runtime: self.runtime.clone(), // Arc clone = cheap
            owns_runtime: false,
            frames: Vec::new(),
            pending_input: None,
            half: None,
            native_frames: 0,
            stack: Vec::new(),
            builtin_args: Vec::new(),
            view: self.view_of_a_task(),
            own: Default::default(),
            view_count: self.view_count,
            settled: self.settled.clone(),
            next_channel_id: self.next_channel_id.clone(),
            next_task_id: self.next_task_id.clone(),
            woken: None,
            spawned: false,
            cancelled: None,
            deadline: None,
            late: None,
            over: None,
            text: String::new(),
            regex_cache: RegexCache::new(),
            tco_elided: TailCalls::default(),
        }
    }

    /// The scheduler of the program.
    pub(crate) fn scheduler(&self) -> &Arc<Scheduler> {
        &self.runtime.scheduler
    }

    /// Whether this is the VM that runs the program itself (`fn main`,
    /// a test, a REPL entry), not one of a task, a stream stage or a
    /// handler: its thread gets the error when the program is
    /// deadlocked.
    pub(crate) fn is_program(&self) -> bool {
        self.owns_runtime
    }

    /// Allocate a new unique channel ID.
    pub(crate) fn next_channel_id(&mut self) -> usize {
        self.next_channel_id.fetch_add(1, Ordering::Relaxed) as usize
    }

    /// Allocate a new unique task ID.
    pub(crate) fn next_task_id(&mut self) -> usize {
        self.next_task_id.fetch_add(1, Ordering::Relaxed) as usize
    }

    /// Allocate a new unique tcp handle ID. Shares the task-id counter
    /// because IDs are only compared within their own `Value` variant
    /// (a TcpStream can never compare equal to a Handle), and avoiding
    /// a third atomic keeps the Vm struct small.
    #[cfg(feature = "tcp")]
    pub(crate) fn next_tcp_id(&mut self) -> usize {
        self.next_task_id.fetch_add(1, Ordering::Relaxed) as usize
    }

    /// Load a compiled top-level function and execute it.
    ///
    /// On the error path, the VM's frame/stack/tco-elided state is
    /// restored to the depths recorded at entry so subsequent calls
    /// (e.g. successive REPL evaluations sharing the same persistent VM)
    /// don't render phantom call-stack frames from prior entries. See
    /// `tests/cli/repl_frame_leak_tests.rs` for the regression lock.
    pub(crate) fn run(&mut self, script: Arc<Function>) -> Result<Value, VmError> {
        let floor = self.frames.len();
        let stack_floor = self.stack.len();
        assert_eq!(script.upvalue_count(), 0, "a script captures nothing");
        let closure = Arc::new(VmClosure::new(script, vec![]));
        self.frames.push(Frame::Code(CallFrame {
            closure,
            ip: 0,
            base_slot: 0,
        }));
        let run = self.run_thread(floor, |vm| vm.run_frames(floor, vm.own_slice()));
        self.finish_run(run, floor, stack_floor)
    }

    /// Make this VM the one of a task that calls `closure` with no
    /// arguments: the scheduler runs it slice by slice
    /// ([`Vm::execute_slice`]).
    pub(crate) fn start_task(&mut self, closure: Arc<VmClosure>) {
        self.stack = vec![Value::Unit];
        // (A closure with dictionaries bound is called with them.)
        dicts::with_bound(&mut self.stack, &closure, 1, 0);
        self.frames = vec![Frame::Code(CallFrame {
            closure,
            ip: 0,
            base_slot: 1,
        })];
    }

    // ── Stack operations ──────────────────────────────────────────

    pub(crate) fn push(&mut self, value: Value) {
        self.stack.push(value);
    }

    // ── Frame access ──────────────────────────────────────────────

    /// The frame of the instruction being run.
    #[inline(always)]
    fn frame(&self) -> &CallFrame {
        match self.frames.last() {
            Some(Frame::Code(frame)) => frame,
            _ => unreachable!("an instruction runs in a function's frame"),
        }
    }

    #[inline(always)]
    fn frame_mut(&mut self) -> &mut CallFrame {
        match self.frames.last_mut() {
            Some(Frame::Code(frame)) => frame,
            _ => unreachable!("an instruction runs in a function's frame"),
        }
    }

    // ── Error enrichment ─────────────────────────────────────────

    /// Enrich a VmError with the current instruction's source span and the
    /// call stack derived from the VM's frame list.
    ///
    /// Tail-call replaced callers are interleaved from `self.tco_elided` so
    /// the rendered call stack still shows every logical caller even after
    /// `Op::TailCall` overwrote the physical frame slot in place. Without
    /// this merge, a chain like `main -> middle -> helper/*boom*/` (where
    /// `middle` and `main` both tail-called) would render a single-frame
    /// stack that drops both intermediate names.
    pub(crate) fn enrich_error(&self, mut err: VmError) -> VmError {
        /// Whether the function `name`, at the place `span`, is code silt
        /// compiled for a builtin trait (`Error.message`'s default): code
        /// with no place in the program that is no synthetic frame
        /// (`<script>`, which the call stack's rendering leaves out).
        fn silts_own(name: &str, span: crate::source::Span) -> bool {
            !span.is_in_source() && !name.starts_with('<')
        }
        if err.span.is_some() {
            return err;
        }
        // Capture span from current frame's IP position.
        // A builtin's error is its caller's: the innermost function. So
        // is the error of code silt compiled for a builtin trait's
        // default method (`Error.message`), which has no place in the
        // program: the error is the call's.
        let place = |frame: &crate::vm::runtime::CallFrame| {
            (frame.closure.function.chunk()).span_at(frame.ip.saturating_sub(1))
        };
        let innermost = self.frames.iter().rev().find_map(|frame| match frame {
            Frame::Code(frame) if !silts_own(frame.closure.function.name(), place(frame)) => {
                Some(frame)
            }
            Frame::Code(_) | Frame::Native(_) => None,
        });
        if let Some(frame) = innermost {
            let span = place(frame);
            if span.is_in_source() {
                err.span = Some(span);
            }
        }
        // Build call stack from all frames, innermost first (matches the
        // existing rendering contract in vm/error.rs::render_call_stack).
        // For each physical frame at depth `d`, emit (a) the physical
        // frame's own (name, ip-span), then (b) any `tco_elided` entries
        // logged at that same depth — newest caller first so the chain
        // reads "callee -> most-recent-tco-caller -> ... -> oldest-caller".
        let mut stack = Vec::new();
        // The log is in the order of the depths, so its entries are
        // met from the end as the frames are.
        let mut elided = self.tco_elided.newest_first().peekable();
        for (depth, frame) in self.frames.iter().enumerate().rev() {
            let Frame::Code(frame) = frame else {
                continue;
            };
            let func_name = frame.closure.function.name().to_string();
            let span = place(frame);
            // (Silt's own code is no frame of the program's.)
            if !silts_own(&func_name, span) {
                stack.push((func_name, span));
            }
            // Newer (later-pushed) entries for this depth are more recent
            // callers, so they come first: the chain stays callee-first.
            // A caller's place is the tail call: the instruction before
            // the one it had come to.
            while let Some((_, caller, ip)) = elided.next_if(|(d, _, _)| *d >= depth) {
                let span = caller.chunk().span_at(ip.saturating_sub(1));
                stack.push((caller.name().to_string(), span));
            }
        }
        err.call_stack = stack;
        err
    }

    // ── Truthiness ────────────────────────────────────────────────

    pub(crate) fn is_truthy(&self, val: &Value) -> bool {
        match val {
            Value::Bool(b) => *b,
            Value::Unit => false,
            _ => true,
        }
    }

    fn is_falsy(&self, val: &Value) -> bool {
        !self.is_truthy(val)
    }

    // ── Value display ─────────────────────────────────────────────

    pub(crate) fn display_value(&self, val: &Value) -> String {
        match val {
            Value::String(s) => s.to_string(),
            _ => {
                let mut text = String::new();
                if !write_plain(&mut text, val) {
                    let _ = write!(text, "{val}");
                }
                text
            }
        }
    }

    pub(crate) fn get_regex<'a>(
        cache: &'a mut RegexCache,
        pattern: &str,
    ) -> Result<&'a Regex, regex::Error> {
        cache.get(pattern)
    }

    /// Whether a runtime value's type has a Display impl — the single
    /// runtime-side oracle for the string-interpolation Display gate
    /// (`Op::DisplayValue`, src/vm/run.rs).
    ///
    /// This mirrors the typechecker's compile-time gate: the typechecker
    /// reduces a *concrete* operand to its canonical name
    /// (`type_name_for_impl` -> `crate::types::canonical::canonicalize`)
    /// and rejects interpolation when that name is absent from the Display
    /// `trait_impl_set`. Every printable built-in and every record and
    /// variant has Display by its structure
    /// (src/typechecker/builtin_traits.rs); the values that are
    /// deliberately left out are the first-class no-Display types
    /// enumerated below:
    ///
    ///   - function-shaped values (`Fn` — closures, builtins, variant
    ///     constructors): no Display impl;
    ///   - `Channel` / `Handle`: opaque concurrency primitives;
    ///   - `TcpListener` / `TcpStream`: opaque network resources, left
    ///     explicitly unprintable (src/typechecker/mod.rs ~7878);
    ///   - `TypeDescriptor` / `PrimitiveDescriptor`: reflective handles
    ///     with no surface Display.
    ///
    /// For a *polymorphic* operand the operand type is still a type
    /// variable at the interpolation site, so the compile-time gate is
    /// skipped (`type_name_for_impl` returns `None`); `Op::DisplayValue`
    /// consults this predicate at the execution site to reject the same
    /// set rather than silently rendering a debug string. Locked by
    /// tests/typecheck/round95_interp_display_runtime_tests.rs.
    pub fn value_implements_display(val: &Value) -> bool {
        !matches!(
            val,
            Value::VmClosure(_)
                | Value::BuiltinFn(_)
                | Value::HostFn(_)
                | Value::VariantConstructor(..)
                | Value::Channel(_)
                | Value::Handle(_)
                | Value::TcpListener(_)
                | Value::TcpStream(_)
                | Value::TypeDescriptor(_)
                | Value::PrimitiveDescriptor(_)
        )
    }

    /// Human-readable type name for error messages. Renders descriptor
    /// and function-shaped values in surface-syntax terms rather than
    /// leaking internal `Value` variant names.
    ///
    /// It is the value's kind ([`Value::kind`]) except for the
    /// **deliberate aliases** that carry semantic content into the
    /// diagnostic:
    ///   - `Record(name, _)` → the record's own type name
    ///   - `Variant(tag, _)` → the name of the variant's enum type.
    ///   - `VariantConstructor(tag)` → ``"VariantConstructor `name`"``
    ///     (TitleCase, no "a " article).
    ///   - `TypeDescriptor(name)` / `PrimitiveDescriptor(name)` →
    ///     ``"TypeDescriptor `name`"`` / ``"PrimitiveDescriptor `name`"``.
    ///
    /// The `pub` visibility is required by
    /// `tests/typecheck/round75_kind_naming_canonical_tests.rs`, which pins
    /// the alignment matrix.
    pub fn user_facing_type_name(&self, val: &Value) -> String {
        match val {
            // Variants that carry semantic content into the user-facing
            // diagnostic. Each is a deliberate alias documented above.
            Value::Record(record) if record.ty().is_anon() => "an anonymous record".to_string(),
            Value::Record(record) => record.ty().name.clone(),
            Value::Variant(variant) => variant.ty().name.clone(),
            Value::VariantConstructor(tag) => {
                format!("VariantConstructor `{tag}`")
            }
            Value::TypeDescriptor(ty) => {
                format!("TypeDescriptor `{}`", ty.name)
            }
            Value::PrimitiveDescriptor(name) => {
                format!("PrimitiveDescriptor `{name}`")
            }
            // All other variants: the kind.
            _ => val.kind().to_string(),
        }
    }
}

#[cfg(test)]
mod tests;
