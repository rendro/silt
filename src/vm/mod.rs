//! Stack-based bytecode VM for Silt.
//!
//! Executes compiled `Function` objects produced by the compiler.

mod arithmetic;
mod calls;
pub(crate) mod dispatch;
pub mod error;
mod io;
mod iter;
mod run;
mod runtime;
mod show;

pub use error::VmError;
pub use io::{Buffer, Clock, HostIo, Output, SystemClock};
pub(crate) use iter::{Flow, call_then, item_arg, iterate, next, stop};
pub use runtime::IoFailure;
pub use runtime::Runtime;
pub(crate) use runtime::{CallFrame, ErrFactory, Frame, IoOp, Native, Step};

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

/// Test-only: run a panicking operation on this VM's I/O pool, wait
/// for it, and return the value it completes with: `failure` of the
/// panic's message, with a "panic: " prefix. `failure` is the typed
/// error of a builtin module, so the value has the shape that module's
/// callers match on.
#[cfg(any(test, feature = "test-hooks"))]
pub fn submit_panicking_io_for_test(vm: &Vm, failure: fn(IoFailure<'_>) -> Value) -> Value {
    let op = vm.runtime.io_pool.submit(failure, || {
        panic!("synthetic IO worker panic for round-76 lock");
    });
    let wait = Wait::new(vec![Arm::Cell(op.cell.clone())]);
    let _ = vm
        .runtime
        .scheduler
        .block_thread(wait, crate::scheduler::Blocks::Thread);
    op.cell.get().cloned().expect("the operation has ended")
}

use regex::Regex;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::bytecode::{Function, Globals, VmClosure};
use crate::runtime::sync::{Arm, Fired, Wait};
use crate::scheduler::Scheduler;
use crate::typeinfo::TypeTable;
use crate::value::Value;
use runtime::{IoPool, RegexCache};

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
    pub(crate) stack: Vec<Value>,
    /// The values of the program's global slots; `None` until the
    /// definition's code has run.
    pub(crate) globals: Vec<Option<Value>>,
    /// What each global slot is, and the slot of each impl method.
    pub(crate) global_slots: Arc<Globals>,
    /// The program's types, by id: the decoders find the record types
    /// of record fields here.
    pub(crate) types: Arc<TypeTable>,

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
    /// Scoped deadline in effect for this task, as a reading of the
    /// host clock ([`Clock::monotonic`]). Set by
    /// `task.deadline(dur, fn)` for the duration of the callback; a
    /// wait for I/O ends at it ([`Vm::io`]), and I/O builtins check it
    /// at entry so a call made past the
    /// deadline returns `Err(...)` immediately without submitting to
    /// the I/O pool. Nested `task.deadline` calls use the earlier
    /// deadline (monotonic tightening); each one's frame holds the
    /// deadline to restore when its callback returns.
    pub(crate) current_deadline: Option<Duration>,

    /// Diagnostic log of callers that were elided by tail-call replacement.
    /// Each entry is `(frame_depth, caller_name, caller_span)` where
    /// `frame_depth` is the index in `self.frames` at which the TCO happened
    /// (i.e. `self.frames.len() - 1` at that moment).  `enrich_error` reads
    /// this log so tail-call chains still render a full call stack instead
    /// of showing only the innermost callee.  Entries are pruned when the
    /// frame at `frame_depth` pops, and the log is bounded per-depth by
    /// `runtime::TCO_ELIDED_CAP` to cap memory under deeply recursive
    /// tail-call loops.
    ///
    /// Lock: tests/lang/callback_frame_capture_tests.rs
    /// `test_tail_call_chain_preserves_caller_frames_in_call_stack`.
    pub(crate) tco_elided: Vec<(usize, String, crate::source::Span)>,

    // ── Caches ──────────────────────────────────────────────────
    /// Cache for compiled regex patterns (bounded, FIFO eviction —
    /// first-in first-out: the oldest 25% of entries are evicted when
    /// the cache reaches `MAX_ENTRIES`. `RegexCache::get` only pushes
    /// to its order deque on miss; a hit does not re-order, so this
    /// is pure insertion order).
    pub(crate) regex_cache: RegexCache,
}

impl Drop for Vm {
    fn drop(&mut self) {
        // The calls in progress end here, however the VM ends: a task
        // that failed, was cancelled or was still waiting when the
        // program ended. Each builtin's frame is told ([`Native::abandon`]).
        self.unwind(0, 0);
        if self.owns_runtime {
            self.runtime.shutdown();
        }
    }
}

/// Create a finite float Value, returning an error if the result is NaN or Infinity.
/// Also canonicalizes -0.0 to 0.0.
fn finite_float(f: f64, op_desc: &str) -> Result<Value, VmError> {
    if !f.is_finite() {
        return Err(VmError::new(format!("float overflow: {op_desc}")));
    }
    Ok(Value::Float(if f == 0.0 { 0.0 } else { f }))
}

impl Vm {
    /// If the current task.deadline has already elapsed, build an `Err`
    /// Value via the caller's factory; otherwise return `None`. I/O
    /// builtins call this at entry so a call made past the deadline
    /// short-circuits into a clean `Err` without submitting to the
    /// I/O pool. The factory determines which typed error variant the
    /// caller's signature expects (io uses `IoUnknown`, tcp uses
    /// `TcpTimeout`, etc.).
    pub(crate) fn deadline_exceeded_with(&self, timeout_err: ErrFactory) -> Option<Value> {
        let deadline = self.current_deadline?;
        if self.runtime.io.monotonic() >= deadline {
            Some(timeout_err(IoFailure::Timeout(
                crate::scheduler::DeadlineSource::Task.message(),
            )))
        } else {
            None
        }
    }

    /// The step of the I/O builtin `name` that runs the blocking
    /// operation `op` on the I/O pool and waits for its value, which
    /// is the builtin's: already `Ok(_)` or `Err(_)`.
    ///
    /// The wait ends at the earlier of the task deadline in effect
    /// and `SILT_IO_TIMEOUT`, with `timeout_err` of the reason: the
    /// typed error the builtin's signature declares. With the task
    /// deadline already past, nothing is run.
    pub(crate) fn io(
        &mut self,
        name: &'static str,
        timeout_err: ErrFactory,
        op: impl FnOnce() -> Value + Send + 'static,
    ) -> Result<Step, VmError> {
        if let Some(err) = self.deadline_exceeded_with(timeout_err) {
            return Ok(Step::Done(err));
        }
        self.io_started(name, timeout_err, op)
    }

    /// [`Vm::io`] for a caller that has looked at the task deadline
    /// itself ([`Vm::deadline_exceeded_with`]) and done something
    /// between that and the operation.
    pub(crate) fn io_started(
        &mut self,
        name: &'static str,
        timeout_err: ErrFactory,
        op: impl FnOnce() -> Value + Send + 'static,
    ) -> Result<Step, VmError> {
        let op = self.runtime.io_pool.submit(timeout_err, op);
        self.io_wait(name, timeout_err, op)
    }

    /// [`Vm::io_started`] for an operation that can be made to return:
    /// `stop` is called if the task stops waiting while the operation
    /// runs (its deadline passed, it was cancelled, it was dropped at
    /// the end of the program), so that the thread of the operation
    /// ends.
    #[cfg(feature = "tcp")]
    pub(crate) fn io_stoppable(
        &mut self,
        name: &'static str,
        timeout_err: ErrFactory,
        stop: impl FnOnce() + Send + 'static,
        op: impl FnOnce() -> Value + Send + 'static,
    ) -> Result<Step, VmError> {
        let op = self.runtime.io_pool.submit(timeout_err, op).stop_with(stop);
        self.io_wait(name, timeout_err, op)
    }

    /// Wait for `op`.
    pub(crate) fn io_wait(
        &mut self,
        name: &'static str,
        timeout_err: ErrFactory,
        op: IoOp,
    ) -> Result<Step, VmError> {
        let (deadline, source) = match self.runtime.scheduler.io_deadline(self.current_deadline) {
            Some((deadline, source)) => (Some(deadline), source),
            None => (None, crate::scheduler::DeadlineSource::Task),
        };
        let wait = Wait::new(vec![Arm::Cell(op.cell.clone())]).deadline(deadline);
        // The frame owns the operation: when the wait is over, however
        // it ends, the operation has no waiter and no longer counts as
        // pending for the program (`IoOp`'s `Drop`).
        Ok(self.park(name, wait, move |_, fired| {
            // The value is taken only by a wait that it ended: one
            // that came as the deadline passed stays unheard.
            let value = match fired {
                Fired::Arm(..) => op.take(),
                _ => None,
            };
            Ok(Step::Done(value.unwrap_or_else(|| {
                timeout_err(IoFailure::Timeout(source.message()))
            })))
        }))
    }

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
                rng: parking_lot::Mutex::new(None),
                uuid_v7: std::sync::Mutex::new(uuid::ContextV7::new()),
            }),
            owns_runtime: true,
            frames: Vec::new(),
            pending_input: None,
            native_frames: 0,
            stack: Vec::new(),
            globals: Vec::new(),
            global_slots: Arc::new(Globals::default()),
            types: Arc::new(TypeTable::default()),
            next_channel_id: Arc::new(AtomicU64::new(0)),
            next_task_id: Arc::new(AtomicU64::new(0)),
            woken: None,
            spawned: false,
            cancelled: None,
            current_deadline: None,
            regex_cache: RegexCache::new(),
            tco_elided: Vec::new(),
        }
    }

    /// Run the program in slices of `steps` steps (at least 1): a task
    /// gives way to the other tasks after that many, instead of the
    /// scheduler's own 2000, and the thread that runs the program's own
    /// code (`fn main`, a test) stops and goes on after that many too,
    /// where it otherwise runs without a break.
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
    /// program that does not end by itself ends there.
    ///
    /// A step is an instruction, or a step of a builtin that calls back
    /// into the program. They are counted where a slice has run its
    /// full length, so the program is ended at the end of a slice, at
    /// most one slice of each of its threads late; with a budget the
    /// thread that runs the program's own code is sliced as the tasks
    /// are (see [`Vm::set_time_slice`]). A wait is no step: a program
    /// that waits for ever is not ended by its budget.
    pub fn set_step_budget(&mut self, steps: u64) {
        let steps = steps.min(u64::MAX - 1);
        self.runtime.steps_left.store(steps, Ordering::Relaxed);
    }

    /// How many steps the thread that runs the program's own code runs
    /// at a time: without a break, unless an embedder set a slice or a
    /// step budget.
    pub(crate) fn own_slice(&self) -> usize {
        match self.time_slice() {
            Some(steps) => steps,
            None if self.runtime.steps_left.load(Ordering::Relaxed) != u64::MAX => {
                crate::scheduler::time_slice()
            }
            None => usize::MAX,
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
        Arc::make_mut(&mut self.types).extend(&program.types);
        self.global_slots = program.globals.clone();
        self.globals.resize(self.global_slots.len(), None);
    }

    /// Create a child VM that shares runtime state (the scheduler, timers, the I/O pool)
    /// via Arc and clones per-task state (globals, the program's types).
    /// Used for thread-per-task spawning.
    pub(crate) fn spawn_child(&self) -> Self {
        Vm {
            runtime: self.runtime.clone(), // Arc clone = cheap
            owns_runtime: false,
            frames: Vec::new(),
            pending_input: None,
            native_frames: 0,
            stack: Vec::new(),
            globals: self.globals.clone(),
            global_slots: self.global_slots.clone(),
            types: self.types.clone(),
            next_channel_id: self.next_channel_id.clone(),
            next_task_id: self.next_task_id.clone(),
            woken: None,
            spawned: false,
            cancelled: None,
            current_deadline: None,
            regex_cache: RegexCache::new(),
            tco_elided: Vec::new(),
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
        let closure = Arc::new(VmClosure {
            function: script,
            upvalues: vec![],
        });
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

    // The code a VM runs has passed the bytecode verifier
    // (`crate::bytecode::verify`): an instruction finds the values it
    // takes on the stack, its slots in the frame and its constants in
    // the pool, of the kinds it needs. The accessors below do not look
    // for anything else.

    fn pop(&mut self) -> Value {
        self.stack
            .pop()
            .expect("verified code pops a value it pushed")
    }

    fn peek(&self) -> &Value {
        self.stack
            .last()
            .expect("verified code looks at a value it pushed")
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

    /// The code and constants of the function being run.
    fn chunk(&self) -> &crate::bytecode::Chunk {
        self.frame().closure.function.chunk()
    }

    /// Decode the instruction at the instruction pointer and step past
    /// it.
    #[inline(always)]
    fn fetch(&mut self) -> crate::bytecode::Instr {
        let frame = self.frame_mut();
        let (instr, next) =
            crate::bytecode::decode(frame.closure.function.chunk().code(), frame.ip)
                .expect("verified code decodes");
        frame.ip = next;
        instr
    }

    /// Drop `tco_elided` entries whose depth is `>= keep_depth`, i.e.
    /// entries that belong to frames no longer on the physical stack.
    /// Called after any frame pop / truncate / split-off so stale
    /// diagnostic state doesn't bleed across unrelated calls.
    ///
    /// The log is in the order of the depths: an entry is logged for
    /// the frame on top, and this is called whenever a frame goes. So
    /// the entries to drop are the last ones.
    pub(crate) fn prune_tco_elided(&mut self, keep_depth: usize) {
        while let Some((depth, _, _)) = self.tco_elided.last()
            && *depth >= keep_depth
        {
            self.tco_elided.pop();
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
    /// stack that drops both intermediate names. See F10 in audit round 17.
    pub(crate) fn enrich_error(&self, mut err: VmError) -> VmError {
        if err.span.is_some() {
            return err;
        }
        // Capture span from current frame's IP position.
        // A builtin's error is its caller's: the innermost function.
        let innermost = self.frames.iter().rev().find_map(|frame| match frame {
            Frame::Code(frame) => Some(frame),
            Frame::Native(_) => None,
        });
        if let Some(frame) = innermost {
            let ip = frame.ip.saturating_sub(1);
            let span = frame.closure.function.chunk().span_at(ip);
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
        let mut elided = self.tco_elided.iter().rev().peekable();
        for (depth, frame) in self.frames.iter().enumerate().rev() {
            let Frame::Code(frame) = frame else {
                continue;
            };
            let func_name = frame.closure.function.name().to_string();
            let ip = frame.ip.saturating_sub(1);
            let span = frame.closure.function.chunk().span_at(ip);
            stack.push((func_name, span));
            // Newer (later-pushed) entries for this depth are more recent
            // callers, so they come first: the chain stays callee-first.
            while let Some((_, name, caller_span)) = elided.next_if(|(d, _, _)| *d >= depth) {
                stack.push((name.clone(), *caller_span));
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
            Value::String(s) => s.clone(),
            Value::Int(n) => n.to_string(),
            Value::Bool(true) => "true".to_string(),
            Value::Bool(false) => "false".to_string(),
            Value::Float(f) => f.to_string(),
            Value::Range(lo, hi) => format!("{lo}..{hi}"),
            _ => format!("{val}"),
        }
    }

    pub(crate) fn get_regex<'a>(
        cache: &'a mut RegexCache,
        pattern: &str,
    ) -> Result<&'a Regex, VmError> {
        cache.get(pattern)
    }

    /// Whether a runtime value's type has a Display impl — the single
    /// runtime-side oracle for the string-interpolation Display gate
    /// (`Op::DisplayValue`, src/vm/run.rs) and the polymorphic
    /// `.display()` method gate (`dispatch_trait_method`'s "display"
    /// arm, src/vm/dispatch.rs).
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

    /// Whether a runtime value is, or transitively CONTAINS, a
    /// function-shaped leaf (`VmClosure` / `BuiltinFn` /
    /// `VariantConstructor`) — the single runtime-side oracle for the
    /// execution-site Compare/Equal gates, the sibling of
    /// `value_implements_display` above.
    ///
    /// The checker rejects comparing or ordering a value that holds a
    /// function, for a concrete operand and through a bound alike
    /// (`Equal` and `Compare` are decided by structure). Without a
    /// runtime backstop such
    /// functions past the typechecker. Without a runtime backstop such
    /// values fell into `Value::cmp` / `PartialEq for Value`, which order
    /// closures by `Arc::as_ptr` (src/value/key.rs) — an ASLR-nondeterministic
    /// Bool for ordering and a silent identity-equality Bool for `==`.
    ///
    /// Consulted by the container arms of `compare()`
    /// (src/vm/arithmetic.rs), the `Op::Eq` / `Op::Neq` gate
    /// (`equality_operand_violation`, src/vm/run.rs), the
    /// `"equal"` / `"compare"` / `"hash"` trait-method arms of
    /// `dispatch_trait_method` (src/vm/dispatch.rs), and the collection
    /// builtin backstop `ensure_no_fn` (src/builtins/collections.rs).
    /// Locked by tests/typecheck/container_fn_compare_runtime_gate_tests.rs; this
    /// being the ONLY definition of the walker is locked by
    /// tests/meta/value_contains_fn_dedup_lock_tests.rs.
    ///
    /// `Range` / `Bytes` and the scalar leaves can never contain a
    /// function, and Channel / Handle / TcpListener / TcpStream stay
    /// equatable-by-identity (round-96 parity), so all fall to `false`.
    pub fn value_contains_fn(val: &Value) -> bool {
        // A worklist, not recursion: values nest as deep as a program
        // builds them, and the native stack a recursive walk needs per
        // level depends on how the compiler happened to inline it.
        let mut pending = vec![val];
        while let Some(value) = pending.pop() {
            match value {
                Value::VmClosure(_)
                | Value::BuiltinFn(_)
                | Value::HostFn(_)
                | Value::VariantConstructor(..) => {
                    return true;
                }
                Value::List(items) => pending.extend(items.iter()),
                Value::Tuple(items) | Value::Variant(_, items) => pending.extend(items.iter()),
                Value::Set(items) => pending.extend(items.iter()),
                Value::Map(entries) => {
                    for (k, v) in entries.iter() {
                        pending.push(k);
                        pending.push(v);
                    }
                }
                Value::Record(_, fields) => pending.extend(fields.values()),
                _ => {}
            }
        }
        false
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
            Value::Record(ty, _) if ty.is_anon() => "an anonymous record".to_string(),
            Value::Record(ty, _) => ty.name.clone(),
            Value::Variant(tag, _) => tag.ty().name.clone(),
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
