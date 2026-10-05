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

pub use error::VmError;
pub use io::{Buffer, Clock, HostIo, Output, SystemClock};
pub(crate) use iter::BuiltinIterKind;
pub use runtime::Runtime;
pub(crate) use runtime::{BlockReason, BuiltinAcc, CallFrame, SelectOpKind, SuspendedBuiltin};

/// Test-only: report the worker count of the I/O pool attached to this
/// VM. Used by the `SILT_IO_POOL_SIZE` env-knob integration tests to
/// verify the env var actually shaped pool construction (no silent
/// regression to the old hardcoded `min(cores, 4)` form).
///
/// Gated on `cfg(any(test, feature = "test-hooks"))` so it does not
/// appear in the release surface area. Returns the pool worker count.
#[cfg(any(test, feature = "test-hooks"))]
pub fn io_pool_worker_count(vm: &Vm) -> usize {
    vm.runtime.io_pool.worker_count()
}

/// Test-only: return [`runtime::resolve_io_pool_size`] without
/// requiring the caller to construct a full `Vm`. Lets integration
/// tests assert the env-var → resolved-size mapping directly. Same
/// gating as [`io_pool_worker_count`].
#[cfg(any(test, feature = "test-hooks"))]
pub fn resolve_io_pool_size() -> usize {
    runtime::resolve_io_pool_size()
}

/// Test-only: the cap [`runtime::resolve_io_pool_size`] clamps at when
/// the env var is set to an absurdly large value. Re-exported so the
/// integration test asserts the same constant the resolver uses.
#[cfg(any(test, feature = "test-hooks"))]
pub fn io_pool_size_cap() -> usize {
    runtime::IO_POOL_SIZE_CAP
}

/// Test-only: the unset-env default for the I/O pool worker count.
/// Re-exported so the integration test compares against the same
/// definition the production resolver falls back to.
#[cfg(any(test, feature = "test-hooks"))]
pub fn default_io_pool_size() -> usize {
    runtime::default_io_pool_size()
}

/// Test-only: submit a panicking closure to this VM's I/O pool with a
/// caller-supplied `IoCompletion` (whose `timeout_err` factory shapes
/// the resulting `Err` Value), block until the completion fires, and
/// return the resulting `Value`. Used by the round-76 lock test for
/// the IoPool worker-panic recovery path: pre-fix, a worker panic
/// produced an untyped `Err(String)` that bypassed every typed match
/// arm; post-fix it routes through `IoCompletion::build_timeout_err`
/// with a "panic: " prefix so the result is the same typed shape the
/// scheduler watchdog produces on a deadline cancel.
#[cfg(any(test, feature = "test-hooks"))]
pub fn submit_panicking_io_for_test(vm: &Vm, completion: Arc<IoCompletion>) -> Value {
    let c = vm.runtime.io_pool.submit_with(completion, || {
        panic!("synthetic IO worker panic for round-76 lock");
    });
    c.wait()
}

use regex::Regex;
use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::bytecode::{Function, Globals, VmClosure};
use crate::runtime::completion::IoCompletion;
use crate::scheduler::Scheduler;
use crate::typeinfo::TypeTable;
use crate::value::Value;
use runtime::{IoPool, RegexCache, TimerManager};

// ── Native stack budget ───────────────────────────────────────────
//
// A plain silt call pushes a VM frame and stays in the interpreter loop
// that is already running. A method call, and a function passed to a
// builtin (`list.map`, `result.map_ok`, ...), instead run a nested
// interpreter loop on the host stack (`Vm::invoke_callable`,
// `Vm::resume_suspended_invoke`). Recursion through either therefore
// consumes host stack, and running out of host stack aborts the whole
// process. The VM bounds it by counting the interpreter loops nested on
// the current thread and refusing to start one more than the thread's
// stack can hold.

/// Host stack, in bytes, that one nested interpreter loop is assumed to
/// need, together with the builtin that started it.
///
/// Unoptimised build: measured, by recursing until the 256 MiB main
/// thread overflowed. A level entered through a method call costs about
/// 95 KiB, a level entered through a `list.*` callback (the most
/// expensive builtin measured) about 152 KiB; nothing is inlined and
/// every local of the opcode dispatch gets its own stack slot. The value
/// is 1.68 times the most expensive level, so at the limit the nested
/// loops fill at most about 60% of the stack. The rest is left for the
/// frames below the first loop and for the native work of the innermost
/// call.
///
/// Optimised build: measured from the resident size of the thread's
/// stack at two depths. A level entered through a method call costs about
/// 4.0 KiB, through `set.map` about 6.2 KiB, through `list.unfold` about
/// 6.3 KiB, and through `list.fold`, `list.map`, `list.sort_by`, string
/// interpolation or a pattern match (the most expensive measured) about
/// 7.5 KiB, the same on the main thread, in a task and in a stream stage.
/// The value, 12 KiB, is 1.6 times the most expensive level: at the limit
/// of 21845 levels on a 256 MiB stack the nested loops fill about 62% of
/// it, and the stack would overflow only at about 34,900 levels.
///
/// The margin is guarded by `tests/lang/wave2_vm_tests.rs`, which recurses
/// through the most expensive shapes to exactly the limit, on the main
/// thread and in a task, and requires a normal result. If a compiler or
/// platform change makes a level more expensive than this value allows
/// for, that test aborts instead of passing; measure again and raise the
/// value.
///
/// The build kind is read off `debug_assertions`, which is on in the
/// `dev` and `test` profiles and off in `release` and `bench`.
const NATIVE_STACK_BYTES_PER_LEVEL: usize = if cfg!(debug_assertions) {
    256 * 1024
} else {
    12 * 1024
};

/// Stack size assumed for a thread that never called
/// [`set_native_stack_budget`].
///
/// Native targets: 2 MiB, the size Rust gives a spawned thread by
/// default.
///
/// WebAssembly: 1 MiB. A wasm module has no threads with a stack size of
/// their own; its one stack is a region of linear memory whose size is
/// fixed when the module is linked, and rustc links every wasm target
/// with `-z stack-size=1048576` unless the embedder overrides it. The
/// playground module (built with the default) confirms it: its
/// `__stack_pointer` starts at 1048576, with the stack placed first in
/// memory. An embedder that links a different stack size calls
/// [`set_native_stack_budget`] with it before running silt code.
const DEFAULT_NATIVE_STACK_BUDGET: usize = if cfg!(target_family = "wasm") {
    1024 * 1024
} else {
    2 * 1024 * 1024
};

/// How many nested interpreter loops fit into a stack of `bytes` bytes.
/// Never less than one, so a thread can always run a program.
const fn native_depth_limit_for(bytes: usize) -> usize {
    let levels = bytes / NATIVE_STACK_BYTES_PER_LEVEL;
    if levels == 0 { 1 } else { levels }
}

thread_local! {
    /// Most interpreter loops that may be nested on this thread.
    static NATIVE_DEPTH_LIMIT: Cell<usize> =
        const { Cell::new(native_depth_limit_for(DEFAULT_NATIVE_STACK_BUDGET)) };
    /// Interpreter loops currently nested on this thread.
    static NATIVE_DEPTH: Cell<usize> = const { Cell::new(0) };
    /// Whether the thread's outermost interpreter loop is running the
    /// program itself (see [`ProgramLoop`]), which then does not count
    /// against the limit.
    static PROGRAM_LOOP_RUNNING: Cell<bool> = const { Cell::new(false) };
}

/// Tell the VM how large the stack of the CURRENT thread is, in bytes.
///
/// Call it once, at the start of every thread that is created with an
/// explicit stack size and runs silt code. The VM derives from it how
/// deep method calls and builtin callbacks may nest on this thread before
/// it reports a stack overflow as a runtime error. A thread that never
/// calls it is treated as having a 2 MiB stack (1 MiB on WebAssembly).
pub fn set_native_stack_budget(bytes: usize) {
    NATIVE_DEPTH_LIMIT.with(|limit| limit.set(native_depth_limit_for(bytes)));
}

/// Start an OS thread that runs silt callbacks outside the scheduler: a
/// stream stage or an HTTP handler. It gets the stack of a scheduler
/// worker and the matching native recursion budget, so callbacks nest as
/// deep there as in a task. If the system refuses a stack that large, the
/// thread starts on the default stack, whose budget is the default one.
pub(crate) fn spawn_callback_thread<F, T>(f: F) -> std::thread::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let bytes = crate::scheduler::WORKER_STACK_BYTES;
    // `Builder::spawn` drops its closure when it fails, so the body is
    // shared with the fallback and taken by whichever thread runs.
    let body = Arc::new(parking_lot::Mutex::new(Some(f)));
    let for_large = body.clone();
    let spawned = std::thread::Builder::new()
        .stack_size(bytes)
        .spawn(move || {
            set_native_stack_budget(bytes);
            let f = for_large.lock().take().expect("thread body runs once");
            f()
        });
    match spawned {
        Ok(handle) => handle,
        Err(_) => std::thread::spawn(move || {
            let f = body.lock().take().expect("thread body runs once");
            f()
        }),
    }
}

/// Most method calls and builtin callbacks that may be nested on the
/// current thread, which is the number the stack-overflow error names.
/// The loop that runs the program itself comes on top (see
/// [`ProgramLoop`]).
pub(crate) fn native_depth_limit() -> usize {
    NATIVE_DEPTH_LIMIT.with(|limit| limit.get())
}

/// One nested interpreter loop on the current thread. Entering counts the
/// loop; dropping the guard, on whatever path the loop is left (result,
/// error, yield or panic), uncounts it.
#[must_use = "the loop is uncounted as soon as the guard is dropped"]
pub(crate) struct NativeDepthGuard(());

impl NativeDepthGuard {
    /// Count one more nested loop, or return `None` if the thread is at
    /// its limit already.
    pub(crate) fn enter() -> Option<Self> {
        let program_loop = PROGRAM_LOOP_RUNNING.with(|running| running.get());
        let limit = native_depth_limit() + usize::from(program_loop);
        NATIVE_DEPTH.with(|depth| {
            let current = depth.get();
            if current >= limit {
                None
            } else {
                depth.set(current + 1);
                Some(NativeDepthGuard(()))
            }
        })
    }
}

impl Drop for NativeDepthGuard {
    fn drop(&mut self) {
        // `try_with`: a guard may be dropped while the thread is being
        // torn down, when its thread-locals are no longer accessible.
        let _ = NATIVE_DEPTH.try_with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

/// The outermost interpreter loop of a thread, while it runs a program
/// (`Vm::run`), is not a method call or a callback and is not charged
/// against the limit: its host stack comes out of the part of the budget
/// that the per-level cost leaves for the frames below the first nested
/// loop. With it uncharged, a program can nest exactly
/// [`native_depth_limit`] method calls and callbacks on any thread, on the
/// main thread as in a task (whose loop, `execute_slice`, is never
/// counted), and the stack-overflow error names that number. A loop
/// started while others are already nested on the thread is charged as
/// usual.
#[must_use = "the program loop is charged again as soon as the guard is dropped"]
struct ProgramLoop(());

impl ProgramLoop {
    /// Allow one loop more than the limit on this thread, for the loop
    /// about to run the program, until the returned guard is dropped.
    /// Returns `None`, and changes nothing, if a loop is running on the
    /// thread already.
    fn start() -> Option<Self> {
        let idle = NATIVE_DEPTH.with(|depth| depth.get()) == 0;
        PROGRAM_LOOP_RUNNING.with(|running| {
            if idle && !running.get() {
                running.set(true);
                Some(ProgramLoop(()))
            } else {
                None
            }
        })
    }
}

impl Drop for ProgramLoop {
    fn drop(&mut self) {
        let _ = PROGRAM_LOOP_RUNNING.try_with(|running| running.set(false));
    }
}

// ── VM ────────────────────────────────────────────────────────────

pub struct Vm {
    pub(crate) runtime: Arc<Runtime>,
    /// True for the VM made by [`Vm::new`], false for the VMs of its
    /// tasks: when that VM is dropped, the runtime's threads end.
    owns_runtime: bool,
    pub(crate) frames: Vec<CallFrame>,
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
    /// Set by channel/task ops when they need to park this task.
    /// Consumed by execute_slice to return SliceResult::Blocked.
    pub(crate) block_reason: Option<BlockReason>,
    /// True when this VM is running as a scheduled task (not on the main thread).
    pub(crate) is_scheduled_task: bool,
    /// Pending I/O completion handle (persists across yield/re-execute).
    pub(crate) pending_io: Option<Arc<IoCompletion>>,
    /// Scoped deadline in effect for this task, as a reading of the
    /// host clock ([`Clock::monotonic`]). Set by
    /// `task.deadline(dur, fn)` for the duration of the callback; the
    /// scheduler's I/O watchdog consults this when the task parks on
    /// I/O, and I/O builtins check it at entry so a call made past the
    /// deadline returns `Err(...)` immediately without submitting to
    /// the I/O pool. Nested `task.deadline` calls use the earlier
    /// deadline (monotonic tightening).
    pub(crate) current_deadline: Option<Duration>,
    /// LIFO stack of outer deadlines, pushed by each task.deadline call
    /// on its first entry and popped on non-yield return. Lets nested
    /// synchronous `task.deadline` scopes correctly restore the outer
    /// deadline when an inner scope exits. Across yields, the stack is
    /// preserved (not touched on yield return), so the first-entry
    /// check `suspended_invoke.is_none()` distinguishes fresh entry
    /// from a resume.
    pub(crate) deadline_stack: Vec<Option<Duration>>,
    /// Saved state from an `invoke_callable` that was interrupted by a yield.
    ///
    /// Invariant: this Option is the TOP of a LIFO stack of suspended invokes.
    /// Deeper (older) suspended states live in `suspended_invoke_outer`. When
    /// a yield happens while another suspended_invoke is already parked
    /// (e.g. nested `task.deadline` + I/O), the existing top is spilled into
    /// the outer vec before the new state overwrites the slot. When the top
    /// is taken on resume, the next state is auto-promoted from the vec back
    /// into the slot so nested-resume bytecode paths still observe
    /// `.is_some()` correctly. See `Vm::push_suspended_invoke` /
    /// `Vm::take_suspended_invoke`. Audit round 26, fix B5.
    pub(crate) suspended_invoke: Option<runtime::SuspendedInvoke>,
    /// Deeper suspended-invoke states (older, further from the current
    /// resume frontier). Top of stack = last element. See the doc on
    /// `suspended_invoke` for the stack invariant.
    pub(crate) suspended_invoke_outer: Vec<runtime::SuspendedInvoke>,
    /// Saved iteration state for a higher-order builtin (e.g. `list.map`)
    /// whose callback yielded (e.g. via I/O).  On resume, the outer
    /// `CallBuiltin` re-dispatches the same builtin, which picks up its
    /// iteration state from this slot instead of restarting from index 0.
    ///
    /// Same LIFO stack discipline as `suspended_invoke`: deeper (older)
    /// states live in `suspended_builtin_outer`.
    pub(crate) suspended_builtin: Option<runtime::SuspendedBuiltin>,
    /// Deeper suspended-builtin states. Top of stack = last element.
    pub(crate) suspended_builtin_outer: Vec<runtime::SuspendedBuiltin>,

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

/// Build the task-deadline-exceeded `Err` Value that I/O builtins
/// return when the current task.deadline has already elapsed at entry.
/// Shape matches the watchdog-fired timeout so silt-side match arms
/// don't have to distinguish between "timed out at entry" and "timed
/// out while parked". Single source of truth for the message text
/// lives on `scheduler::DeadlineSource`.
///
/// Phase 1 of the stdlib error redesign: wrapped in `IoUnknown(msg)`
/// so the outer `Err` payload has the typed `IoError` shape every io/fs
/// signature now returns. Users can still substring-match on the
/// message via `e.message()`.
impl Vm {
    /// If the current task.deadline has already elapsed, build an `Err`
    /// Value via the caller's factory; otherwise return `None`. I/O
    /// builtins call this at entry so a call made past the deadline
    /// short-circuits into a clean `Err` without submitting to the
    /// I/O pool. The factory determines which typed error variant the
    /// caller's signature expects (io uses `IoUnknown`, tcp uses
    /// `TcpTimeout`, etc.).
    pub(crate) fn deadline_exceeded_with(
        &self,
        timeout_err: &(dyn Fn(&str) -> Value + Sync),
    ) -> Option<Value> {
        let deadline = self.current_deadline?;
        if self.runtime.io.monotonic() >= deadline {
            Some(timeout_err(
                crate::scheduler::DeadlineSource::Task.message(),
            ))
        } else {
            None
        }
    }

    /// Run the shared I/O builtin entry guard:
    ///   1. If a pending I/O completion exists (we're resuming after a
    ///      yield), consume it — return `Ok(Some(result))` if ready,
    ///      else re-park via yield.
    ///   2. If the current task.deadline has already elapsed (fresh
    ///      call, no pending), return `Ok(Some(Err(timeout)))`.
    ///   3. Otherwise `Ok(None)` — caller proceeds with a fresh submit
    ///      (or main-thread sync call).
    ///
    /// The `args` slice is pushed back onto the stack on re-park so the
    /// CallBuiltin opcode can re-read them when the task resumes. The
    /// re-park branch routes through
    /// [`park_on_completion`](Self::park_on_completion) so the
    /// pending_io / block_reason / args-pushback protocol lives in
    /// exactly one place.
    ///
    /// The `timeout_err` factory shapes the typed `Err` variant emitted
    /// when `current_deadline` has already elapsed at entry. Modules
    /// with non-IoError error types (tcp, http, ...) pass their own
    /// factory so a deadline-at-entry surfaces the right typed variant
    /// rather than the generic `Err(IoUnknown(_))`. Most callers should
    /// use [`submit_io_or_run`](Self::submit_io_or_run) which calls
    /// this internally; direct callers exist only when post-guard
    /// logic must run before the actual submit (e.g. tcp.read's
    /// closed-stream check).
    pub(crate) fn io_entry_guard_with(
        &mut self,
        args: &[Value],
        timeout_err: &(dyn Fn(&str) -> Value + Sync),
    ) -> Result<Option<Value>, VmError> {
        if self.is_scheduled_task
            && let Some(completion) = self.pending_io.take()
        {
            if let Some(result) = completion.try_get() {
                return Ok(Some(result));
            }
            return Err(self.park_on_completion(args, completion));
        }
        if let Some(err) = self.deadline_exceeded_with(timeout_err) {
            return Ok(Some(err));
        }
        Ok(None)
    }

    /// Park the current scheduled task with the given block reason and
    /// re-push `args` onto the stack so the CallBuiltin opcode can
    /// re-execute on resume. Returns the yield signal as a `VmError`
    /// so the caller can `return Err(...)` directly.
    ///
    /// This is the **single** place that owns the args-pushback +
    /// yield_signal sequence. Every park site — IO completions
    /// (`park_on_completion`), channel send/receive/select, task
    /// join — routes through here so the protocol cannot drift
    /// across builtins. (Round 78 extraction; the prior ~30
    /// hand-rolled sites collapse to one.)
    ///
    /// The caller is responsible for setting up whatever wakes the
    /// task: a completion handle on `pending_io`, a waker
    /// registered on a channel, a join slot on a task handle, etc.
    pub(crate) fn park_with_reason(
        &mut self,
        args: &[Value],
        reason: crate::vm::runtime::BlockReason,
    ) -> VmError {
        self.block_reason = Some(reason);
        for arg in args {
            self.push(arg.clone());
        }
        VmError::yield_signal()
    }

    /// Park the current scheduled task on an IO completion handle.
    /// Sets `pending_io` so the entry-guard's resume branch picks
    /// up this completion, then delegates to
    /// [`park_with_reason`](Self::park_with_reason) for the
    /// shared block_reason / args-pushback / yield sequence.
    ///
    /// The completion must already be wired so that something will
    /// call `completion.complete(_)` to wake the task — typically a
    /// closure submitted to `runtime.io_pool` or a deadline
    /// scheduled on `runtime.timer`.
    pub(crate) fn park_on_completion(
        &mut self,
        args: &[Value],
        completion: Arc<IoCompletion>,
    ) -> VmError {
        use crate::vm::runtime::BlockReason;
        self.pending_io = Some(completion.clone());
        self.park_with_reason(args, BlockReason::Io(completion))
    }

    /// One-shot "submit to the IO pool, park on yield, run synchronously
    /// on the main thread" helper for IO-pool-backed builtins.
    ///
    /// Encapsulates the entire entry-guard / submit / park / sync-fallback
    /// dance in a single call so every IO builtin uses the same code path
    /// and the args-pushback on re-park is the helper's responsibility,
    /// not the caller's. Adding a new IO builtin reduces to picking the
    /// right `(completion_factory, timeout_err)` pair and writing the
    /// closure body — no manual completion-state mutation, no manual
    /// args-pushback loop.
    ///
    /// `op` runs on a worker thread when called from a scheduled task,
    /// or synchronously on the main thread otherwise. It must produce
    /// the typed `Value` result already wrapped in `Ok(_)` / `Err(_)`
    /// variants.
    ///
    /// Builtins with non-IoPool parking (channel send/receive, timer
    /// sleeps, postgres listen workers) handle their own park sequence —
    /// they call [`park_on_completion`](Self::park_on_completion) for
    /// the timer-backed case and never touch this helper.
    ///
    /// Builtins that need to inject logic *between* the entry guard and
    /// the submit (e.g. tcp.read's "drain pending completion before
    /// reporting closed-stream") call [`io_entry_guard_with`] themselves
    /// for the resume gate, then call
    /// [`run_or_submit_io`](Self::run_or_submit_io) with the
    /// already-guarded `args` for the submit-or-sync half.
    pub(crate) fn submit_io_or_run<F>(
        &mut self,
        args: &[Value],
        completion: Arc<IoCompletion>,
        timeout_err: &(dyn Fn(&str) -> Value + Sync),
        op: F,
    ) -> Result<Value, VmError>
    where
        F: FnOnce() -> Value + Send + 'static,
    {
        if let Some(r) = self.io_entry_guard_with(args, timeout_err)? {
            return Ok(r);
        }
        self.run_or_submit_io(args, completion, op)
    }

    /// Submit-or-run half of [`submit_io_or_run`] without the entry
    /// guard. Use when the caller has already run
    /// [`io_entry_guard_with`] and wants to interleave additional
    /// post-guard checks (e.g. tcp.read's closed-stream check, which
    /// must happen *after* the resume gate so a pending completion
    /// wins over a racing close) before the actual submit.
    pub(crate) fn run_or_submit_io<F>(
        &mut self,
        args: &[Value],
        completion: Arc<IoCompletion>,
        op: F,
    ) -> Result<Value, VmError>
    where
        F: FnOnce() -> Value + Send + 'static,
    {
        if self.is_scheduled_task {
            let c = self.runtime.io_pool.submit_with(completion, op);
            return Err(self.park_on_completion(args, c));
        }
        Ok(op())
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
        Vm {
            runtime: Arc::new(Runtime {
                scheduler: parking_lot::Mutex::new(None),
                timer: TimerManager::new(io.clone()),
                io_pool: IoPool::new(runtime::resolve_io_pool_size(), io.clone()),
                io,
                rng: parking_lot::Mutex::new(None),
                uuid_v7: std::sync::Mutex::new(uuid::ContextV7::new()),
            }),
            owns_runtime: true,
            frames: Vec::new(),
            stack: Vec::new(),
            globals: Vec::new(),
            global_slots: Arc::new(Globals::default()),
            types: Arc::new(TypeTable::default()),
            next_channel_id: Arc::new(AtomicU64::new(0)),
            next_task_id: Arc::new(AtomicU64::new(0)),
            block_reason: None,
            is_scheduled_task: false,
            pending_io: None,
            current_deadline: None,
            deadline_stack: Vec::new(),
            suspended_invoke: None,
            suspended_invoke_outer: Vec::new(),
            suspended_builtin: None,
            suspended_builtin_outer: Vec::new(),
            regex_cache: RegexCache::new(),
            tco_elided: Vec::new(),
        }
    }

    /// Report on the host's stderr the tasks that have failed so far
    /// and that nobody joined or cancelled. Nothing while a front end
    /// collects them (`scheduler::collect_unjoined_failures`).
    fn report_unjoined_failures(&self) {
        if let Some(scheduler) = self.current_scheduler() {
            scheduler.report_unjoined_failures();
        }
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
            stack: Vec::new(),
            globals: self.globals.clone(),
            global_slots: self.global_slots.clone(),
            types: self.types.clone(),
            next_channel_id: self.next_channel_id.clone(),
            next_task_id: self.next_task_id.clone(),
            block_reason: None,
            is_scheduled_task: false,
            pending_io: None,
            current_deadline: None,
            deadline_stack: Vec::new(),
            suspended_invoke: None,
            suspended_invoke_outer: Vec::new(),
            suspended_builtin: None,
            suspended_builtin_outer: Vec::new(),
            regex_cache: RegexCache::new(),
            tco_elided: Vec::new(),
        }
    }

    /// Return a clone of the current scheduler `Arc`, if one exists.
    ///
    /// Unlike [`get_or_create_scheduler`], this does NOT create a scheduler
    /// on demand — it returns `None` when no task has been spawned yet.
    /// Used by the main-thread channel watchdog to decide whether any
    /// scheduled task could still make progress.
    pub(crate) fn current_scheduler(&self) -> Option<Arc<Scheduler>> {
        self.runtime.scheduler.lock().clone()
    }

    /// Get or create the shared scheduler.
    pub(crate) fn get_or_create_scheduler(&self) -> Arc<Scheduler> {
        let mut guard = self.runtime.scheduler.lock();
        if let Some(ref sched) = *guard {
            sched.clone()
        } else {
            let sched = Arc::new(Scheduler::new(self.runtime.io.clone()));
            *guard = Some(sched.clone());
            sched
        }
    }

    /// Take the block_reason out of this VM (consuming it).
    pub(crate) fn take_block_reason(&mut self) -> Option<BlockReason> {
        self.block_reason.take()
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
        let saved_frames_len = self.frames.len();
        let saved_stack_len = self.stack.len();
        let saved_tco_len = self.tco_elided.len();
        let closure = Arc::new(VmClosure {
            function: script,
            upvalues: vec![],
        });
        self.frames.push(CallFrame {
            closure,
            ip: 0,
            base_slot: 0,
        });
        // Held until `execute` has returned; see `ProgramLoop`.
        let _program_loop = ProgramLoop::start();
        match self.execute() {
            Ok(v) => Ok(v),
            Err(e) => {
                // Build the enriched error first — `enrich_error` walks
                // `self.frames`/`self.tco_elided` to reconstruct the
                // call stack for THIS run, which is exactly the state
                // we're about to discard.
                let enriched = self.enrich_error(e);
                // Restore VM shape to the entry snapshot. Without this
                // the call frame pushed above (and any frames the
                // unwinding error left behind from nested calls)
                // remain on the VM and leak into the next `run`'s
                // call stack as phantom frames (the REPL runs many
                // scripts on one VM).
                self.frames.truncate(saved_frames_len);
                self.stack.truncate(saved_stack_len);
                self.tco_elided.truncate(saved_tco_len);
                Err(enriched)
            }
        }
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
    fn frame(&self) -> &CallFrame {
        self.frames.last().expect("an instruction runs in a frame")
    }

    fn frame_mut(&mut self) -> &mut CallFrame {
        self.frames
            .last_mut()
            .expect("an instruction runs in a frame")
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
    pub(crate) fn prune_tco_elided(&mut self, keep_depth: usize) {
        self.tco_elided.retain(|(d, _, _)| *d < keep_depth);
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
        if err.is_yield || err.span.is_some() {
            return err;
        }
        // Capture span from current frame's IP position.
        if let Some(frame) = self.frames.last() {
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
        for (depth, frame) in self.frames.iter().enumerate().rev() {
            let func_name = frame.closure.function.name().to_string();
            let ip = frame.ip.saturating_sub(1);
            let span = frame.closure.function.chunk().span_at(ip);
            stack.push((func_name, span));
            // Newer (later-pushed) entries for this depth are more recent
            // callers, so walk in reverse to keep the callee-first order.
            for (d, name, caller_span) in self.tco_elided.iter().rev() {
                if *d == depth {
                    stack.push((name.clone(), *caller_span));
                }
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

    /// Internal-facing type name used in debug / invariant checks and
    /// arithmetic/dispatch error messages. Surfaces enum-variant names
    /// for the underlying `Value` kinds — including the Range/List
    /// distinction (a Range receiver in an unsupported arithmetic
    /// shows as "Range", not collapsed to "List", to preserve
    /// user-facing display fidelity even though the dispatch layer
    /// canonicalises Range -> List).
    ///
    /// Do NOT use this for method dispatch — use
    /// `crate::types::canonical::dispatch_type_for_value` instead.
    pub fn type_name(&self, val: &Value) -> &'static str {
        match val {
            Value::Int(_) => "Int",
            Value::Float(_) => "Float",
            Value::Bool(_) => "Bool",
            Value::String(_) => "String",
            Value::List(_) => "List",
            // `stringify!` is used here instead of the bare string
            // literal so the architectural lock test
            // (tests/meta/canonical_type_arch_lock_tests.rs) can grep for
            // dispatch-key uses of `"Range"` without false-positiving
            // on this representation-level debug helper. The expansion
            // is identical at compile time: a `&'static str` "Range".
            Value::Range(..) => stringify!(Range),
            Value::Map(_) => "Map",
            Value::Set(_) => "Set",
            Value::Tuple(_) => "Tuple",
            Value::Record(..) => "Record",
            Value::Variant(..) => "Variant",
            // Surface name matches `Type::Fun`'s Display (`Fn(...) -> R`)
            // and the canonical dispatch name returned by
            // `dispatch_type_name`. Round 71 follow-up unified
            // `Function` / `Fun` / `Fn` on `"Fn"`.
            Value::VmClosure(_) => "Fn",
            Value::BuiltinFn(_) => "BuiltinFn",
            Value::HostFn(_) => "HostFn",
            Value::VariantConstructor(..) => "VariantConstructor",
            Value::TypeDescriptor(_) => "TypeDescriptor",
            Value::PrimitiveDescriptor(_) => "PrimitiveDescriptor",
            Value::Channel(_) => "Channel",
            Value::Handle(_) => "Handle",
            Value::Bytes(_) => "Bytes",
            Value::TcpListener(_) => "TcpListener",
            Value::TcpStream(_) => "TcpStream",
            Value::Unit => "Unit",
        }
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
    /// `trait_impl_set`. The auto-derive lists (src/typechecker/mod.rs
    /// ~7787-7876) stamp Display onto every printable built-in plus user
    /// records / variants; the values that are deliberately left out are
    /// the first-class no-Display types enumerated below:
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
    /// silt does NOT statically enforce inferred trait bounds on
    /// polymorphic templates: `pending_numeric_checks`
    /// (src/typechecker/inference.rs) skips operands whose type is still
    /// a `Var`, on the documented promise that the VM catches the
    /// violation at the execution site. Round 97 made the CONCRETE
    /// container forms (`[{ x -> x }] < [{ x -> x }]`) a compile error
    /// (`operand_builtin_trait_violation` recurses into element types),
    /// but a polymorphic wrapper (`fn lt(a: x, b: x) -> Bool { a < b }`
    /// called with lists of lambdas) still launders a container of
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
    /// **Round 75 — TitleCase alignment with `type_name`.** Pre-fix this
    /// helper drifted from its sibling `type_name`: it returned lowercase
    /// surface words ("range", "tuple") and indefinite-article forms
    /// ("a function", "a channel", "a TCP listener") while `type_name`
    /// returned TitleCase ("Range", "Tuple", "Fn", "Channel",
    /// "TcpListener"). Two error-rendering paths produced different
    /// strings for the same value — exactly the dual-shape drift that
    /// "one way to do things" forbids.
    ///
    /// Post-fix the helper mirrors `type_name` exactly except for the
    /// **deliberate aliases** that carry semantic content into the
    /// diagnostic:
    ///   - `Record(name, _)` → the record's own type name
    ///   - `Variant(tag, _)` → the name of the variant's enum type.
    ///   - `VariantConstructor(tag)` → ``"VariantConstructor `name`"``
    ///     (TitleCase, no "a " article).
    ///   - `TypeDescriptor(name)` / `PrimitiveDescriptor(name)` →
    ///     ``"TypeDescriptor `name`"`` / ``"PrimitiveDescriptor `name`"``.
    ///
    /// All other variants delegate to `type_name` so the two paths
    /// produce byte-identical output. The `pub` visibility is required
    /// by `tests/typecheck/round75_kind_naming_canonical_tests.rs`, which pins
    /// the alignment matrix.
    pub fn user_facing_type_name(&self, val: &Value) -> String {
        match val {
            // Variants that carry semantic content into the user-facing
            // diagnostic. Each is a deliberate alias documented above.
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
            // All other variants delegate to `type_name` for canonical
            // TitleCase wording. Drift is impossible because the same
            // arms are read from the same source.
            _ => self.type_name(val).to_string(),
        }
    }
}

#[cfg(test)]
mod native_stack_tests {
    use super::{
        DEFAULT_NATIVE_STACK_BUDGET, NATIVE_DEPTH, NATIVE_STACK_BYTES_PER_LEVEL, NativeDepthGuard,
        ProgramLoop, native_depth_limit, native_depth_limit_for, set_native_stack_budget,
    };

    /// Run `f` on a thread of its own, so that the thread-locals it reads
    /// and changes belong to this test alone.
    fn on_fresh_thread<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        std::thread::spawn(f).join().expect("test thread panicked")
    }

    #[test]
    fn a_thread_without_a_budget_is_treated_as_2_mib() {
        let limit = on_fresh_thread(native_depth_limit);
        assert_eq!(limit, native_depth_limit_for(2 * 1024 * 1024));
        assert_eq!(limit, native_depth_limit_for(DEFAULT_NATIVE_STACK_BUDGET));
        assert!(limit >= 1);
    }

    #[test]
    fn a_budget_belongs_to_the_thread_that_set_it() {
        let (inside, outside) = on_fresh_thread(|| {
            set_native_stack_budget(64 * 1024 * 1024);
            let inside = native_depth_limit();
            let outside = on_fresh_thread(native_depth_limit);
            (inside, outside)
        });
        assert_eq!(inside, native_depth_limit_for(64 * 1024 * 1024));
        assert_eq!(outside, native_depth_limit_for(DEFAULT_NATIVE_STACK_BUDGET));
        assert!(inside > outside);
    }

    #[test]
    fn a_larger_stack_allows_proportionally_more_levels() {
        assert_eq!(native_depth_limit_for(NATIVE_STACK_BYTES_PER_LEVEL), 1);
        assert_eq!(
            native_depth_limit_for(10 * NATIVE_STACK_BYTES_PER_LEVEL),
            10
        );
        assert_eq!(
            native_depth_limit_for(10 * NATIVE_STACK_BYTES_PER_LEVEL + 1),
            10
        );
    }

    #[test]
    fn a_stack_smaller_than_one_level_still_allows_one() {
        assert_eq!(native_depth_limit_for(0), 1);
        assert_eq!(native_depth_limit_for(NATIVE_STACK_BYTES_PER_LEVEL - 1), 1);
    }

    #[test]
    fn the_guard_refuses_the_level_past_the_limit_and_frees_its_level_on_drop() {
        on_fresh_thread(|| {
            set_native_stack_budget(3 * NATIVE_STACK_BYTES_PER_LEVEL);
            let first = NativeDepthGuard::enter().expect("level 1 fits");
            let second = NativeDepthGuard::enter().expect("level 2 fits");
            let third = NativeDepthGuard::enter().expect("level 3 fits");
            assert!(
                NativeDepthGuard::enter().is_none(),
                "a fourth level must be refused"
            );
            assert_eq!(NATIVE_DEPTH.with(|depth| depth.get()), 3);
            drop(third);
            let again = NativeDepthGuard::enter().expect("the freed level can be taken again");
            drop(again);
            drop(second);
            drop(first);
            assert_eq!(NATIVE_DEPTH.with(|depth| depth.get()), 0);
        });
    }

    #[test]
    fn a_refused_level_is_not_counted() {
        on_fresh_thread(|| {
            set_native_stack_budget(NATIVE_STACK_BYTES_PER_LEVEL);
            let only = NativeDepthGuard::enter().expect("level 1 fits");
            for _ in 0..5 {
                assert!(NativeDepthGuard::enter().is_none());
            }
            assert_eq!(NATIVE_DEPTH.with(|depth| depth.get()), 1);
            drop(only);
            assert_eq!(NATIVE_DEPTH.with(|depth| depth.get()), 0);
        });
    }

    #[test]
    fn the_program_loop_comes_on_top_of_the_limit() {
        on_fresh_thread(|| {
            set_native_stack_budget(2 * NATIVE_STACK_BYTES_PER_LEVEL);
            let program = ProgramLoop::start().expect("an idle thread can start a program");
            let outer = NativeDepthGuard::enter().expect("the program loop fits");
            let first = NativeDepthGuard::enter().expect("nested level 1 fits");
            let second = NativeDepthGuard::enter().expect("nested level 2 fits");
            assert!(
                NativeDepthGuard::enter().is_none(),
                "a third nested level must be refused"
            );
            // The error names the nested levels, not the program loop.
            assert_eq!(native_depth_limit(), 2);
            drop(second);
            drop(first);
            drop(outer);
            drop(program);
            // Without a program running, the limit is the plain one.
            let a = NativeDepthGuard::enter().expect("level 1 fits");
            let b = NativeDepthGuard::enter().expect("level 2 fits");
            assert!(NativeDepthGuard::enter().is_none());
            drop(b);
            drop(a);
        });
    }

    #[test]
    fn only_the_outermost_loop_of_a_thread_is_a_program_loop() {
        on_fresh_thread(|| {
            set_native_stack_budget(2 * NATIVE_STACK_BYTES_PER_LEVEL);
            let busy = NativeDepthGuard::enter().expect("level 1 fits");
            assert!(
                ProgramLoop::start().is_none(),
                "a program started inside a running loop is charged as usual"
            );
            drop(busy);
            let program = ProgramLoop::start().expect("the thread is idle again");
            assert!(
                ProgramLoop::start().is_none(),
                "a thread has at most one program loop"
            );
            drop(program);
        });
    }
}

#[cfg(test)]
mod tests;
