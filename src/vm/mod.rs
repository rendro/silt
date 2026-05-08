//! Stack-based bytecode VM for Silt.
//!
//! Executes compiled `Function` objects produced by the compiler.

mod arithmetic;
pub(crate) mod dispatch;
pub mod error;
mod execute;
mod runtime;

pub use error::VmError;
pub(crate) use execute::BuiltinIterKind;
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
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use crate::builtins::data::FieldType;
use crate::bytecode::{Function, VmClosure};
use crate::scheduler::Scheduler;
use crate::types::canonical::dispatch_name_for_value;
use crate::value::{FromValue, IntoValue, IoCompletion, Value};
use runtime::{IoPool, RegexCache, TimerManager};

// ── VM ────────────────────────────────────────────────────────────

pub struct Vm {
    pub(crate) runtime: Arc<Runtime>,
    pub(crate) frames: Vec<CallFrame>,
    pub(crate) stack: Vec<Value>,
    pub(crate) globals: HashMap<String, Value>,
    /// Maps record type names to their field definitions (name, type) for json.parse.
    pub(crate) record_types: HashMap<String, Vec<(String, FieldType)>>,

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
    /// Scoped wall-clock deadline in effect for this task. Set by
    /// `task.deadline(dur, fn)` for the duration of the callback; the
    /// scheduler's I/O watchdog consults this when the task parks on
    /// I/O, and I/O builtins check it at entry so a call made past the
    /// deadline returns `Err(...)` immediately without submitting to
    /// the I/O pool. Nested `task.deadline` calls use the earlier
    /// deadline (monotonic tightening).
    pub(crate) current_deadline: Option<Instant>,
    /// LIFO stack of outer deadlines, pushed by each task.deadline call
    /// on its first entry and popped on non-yield return. Lets nested
    /// synchronous `task.deadline` scopes correctly restore the outer
    /// deadline when an inner scope exits. Across yields, the stack is
    /// preserved (not touched on yield return), so the first-entry
    /// check `suspended_invoke.is_none()` distinguishes fresh entry
    /// from a resume.
    pub(crate) deadline_stack: Vec<Option<Instant>>,
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
    /// Lock: tests/callback_frame_capture_tests.rs
    /// `test_tail_call_chain_preserves_caller_frames_in_call_stack`.
    pub(crate) tco_elided: Vec<(usize, String, crate::lexer::Span)>,

    // ── Caches ──────────────────────────────────────────────────
    /// Cache for compiled regex patterns (bounded, FIFO eviction —
    /// first-in first-out: the oldest 25% of entries are evicted when
    /// the cache reaches `MAX_ENTRIES`. `RegexCache::get` only pushes
    /// to its order deque on miss; a hit does not re-order, so this
    /// is pure insertion order).
    pub(crate) regex_cache: RegexCache,
}

impl Default for Vm {
    fn default() -> Self {
        Self::new()
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
        if Instant::now() >= deadline {
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

    pub fn new() -> Self {
        let mut vm = Vm {
            runtime: Arc::new(Runtime {
                foreign_fns: HashMap::new(),
                scheduler: parking_lot::Mutex::new(None),
                timer: TimerManager::new(),
                io_pool: IoPool::new(runtime::resolve_io_pool_size()),
            }),
            frames: Vec::new(),
            stack: Vec::new(),
            globals: HashMap::new(),
            record_types: HashMap::new(),
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
        };
        vm.register_builtins();
        vm
    }

    // ── Foreign function registration ───────────────────────────

    /// Register a foreign function callable from Silt.
    ///
    /// The function receives `&[Value]` and returns `Result<Value, VmError>`.
    /// Use `FromValue` / `IntoValue` traits for type-safe marshalling.
    ///
    /// # Panics
    /// Panics inside `func` are caught by the dispatcher via
    /// `std::panic::catch_unwind` and converted into a `VmError` whose
    /// message includes the panic payload when it is a `&str` or `String`.
    /// The scheduler worker thread survives and other tasks continue to
    /// run. Returning `Err(VmError)` for error conditions is still
    /// strongly preferred — panics are only caught as a safety net.
    ///
    /// # Errors
    /// Returns an error if the VM's runtime has already been shared (e.g. via
    /// task spawning). All foreign functions must be registered before running
    /// any Silt code that spawns tasks.
    pub fn register_fn(
        &mut self,
        name: impl Into<String>,
        func: impl Fn(&[Value]) -> Result<Value, VmError> + Send + Sync + 'static,
    ) -> Result<(), VmError> {
        let name = name.into();
        let runtime = Arc::get_mut(&mut self.runtime).ok_or_else(|| {
            VmError::new(format!(
                "cannot register function '{}': VM runtime has already been shared \
                 (register all foreign functions before spawning tasks)",
                name
            ))
        })?;
        runtime.foreign_fns.insert(name.clone(), Arc::new(func));
        self.globals.insert(name.clone(), Value::BuiltinFn(name));
        Ok(())
    }

    /// Register a 0-argument foreign function with automatic marshalling.
    pub fn register_fn0<R: IntoValue>(
        &mut self,
        name: impl Into<String>,
        func: impl Fn() -> R + Send + Sync + 'static,
    ) -> Result<(), VmError> {
        let n = name.into();
        let n2 = n.clone();
        self.register_fn(n, move |args: &[Value]| {
            if !args.is_empty() {
                return Err(VmError::new(format!(
                    "{n2} expects 0 arguments, got {}",
                    args.len()
                )));
            }
            Ok(func().into_value())
        })
    }

    /// Register a 1-argument foreign function with automatic marshalling.
    pub fn register_fn1<A: FromValue, R: IntoValue>(
        &mut self,
        name: impl Into<String>,
        func: impl Fn(A) -> R + Send + Sync + 'static,
    ) -> Result<(), VmError> {
        let n = name.into();
        let n2 = n.clone();
        self.register_fn(n, move |args: &[Value]| {
            if args.len() != 1 {
                return Err(VmError::new(format!(
                    "{n2} expects 1 argument, got {}",
                    args.len()
                )));
            }
            let a = A::from_value(&args[0]).map_err(|e| VmError::new(format!("{n2}: {e}")))?;
            Ok(func(a).into_value())
        })
    }

    /// Register a 2-argument foreign function with automatic marshalling.
    pub fn register_fn2<A: FromValue, B: FromValue, R: IntoValue>(
        &mut self,
        name: impl Into<String>,
        func: impl Fn(A, B) -> R + Send + Sync + 'static,
    ) -> Result<(), VmError> {
        let n = name.into();
        let n2 = n.clone();
        self.register_fn(n, move |args: &[Value]| {
            if args.len() != 2 {
                return Err(VmError::new(format!(
                    "{n2} expects 2 arguments, got {}",
                    args.len()
                )));
            }
            let a =
                A::from_value(&args[0]).map_err(|e| VmError::new(format!("{n2}: arg 1: {e}")))?;
            let b =
                B::from_value(&args[1]).map_err(|e| VmError::new(format!("{n2}: arg 2: {e}")))?;
            Ok(func(a, b).into_value())
        })
    }

    /// Create a child VM that shares runtime state (variant types, foreign functions)
    /// via Arc and clones per-task state (globals, record types cache).
    /// Used for thread-per-task spawning.
    pub(crate) fn spawn_child(&self) -> Self {
        Vm {
            runtime: self.runtime.clone(), // Arc clone = cheap
            frames: Vec::new(),
            stack: Vec::new(),
            globals: self.globals.clone(),
            record_types: self.record_types.clone(),
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
            let sched = Arc::new(Scheduler::new());
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
    /// `tests/repl_frame_leak_tests.rs` for the regression lock.
    pub fn run(&mut self, script: Arc<Function>) -> Result<Value, VmError> {
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
                // call stack as phantom `-> main at <declaration>`
                // entries.
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

    fn pop(&mut self) -> Result<Value, VmError> {
        self.stack.pop().ok_or_else(|| {
            let ip = self.frames.last().map(|f| f.ip).unwrap_or(0);
            VmError::new(format!("internal VM error: stack underflow at ip={ip}"))
        })
    }

    fn peek(&self) -> Result<&Value, VmError> {
        self.stack.last().ok_or_else(|| {
            let ip = self.frames.last().map(|f| f.ip).unwrap_or(0);
            VmError::new(format!("internal VM error: stack underflow at ip={ip}"))
        })
    }

    // ── Bytecode reading ──────────────────────────────────────────

    fn read_byte(&mut self) -> Result<u8, VmError> {
        let frame = self.frames.last().ok_or_else(|| {
            VmError::new("internal VM error: no call frame while reading bytecode".to_string())
        })?;
        let ip = frame.ip;
        let byte = *frame.closure.function.chunk.code.get(ip).ok_or_else(|| {
            VmError::new(format!(
                "internal VM error: bytecode out of bounds at ip={ip}"
            ))
        })?;
        self.frames
            .last_mut()
            .ok_or_else(|| {
                VmError::new("internal VM error: no call frame while reading bytecode".to_string())
            })?
            .ip = ip + 1;
        Ok(byte)
    }

    fn read_u8(&mut self) -> Result<u8, VmError> {
        self.read_byte()
    }

    fn read_u16(&mut self) -> Result<u16, VmError> {
        let lo = self.read_byte()? as u16;
        let hi = self.read_byte()? as u16;
        Ok(lo | (hi << 8))
    }

    fn read_constant(&self, index: usize) -> Result<Value, VmError> {
        let frame = self.current_frame()?;
        frame
            .closure
            .function
            .chunk
            .constants
            .get(index)
            .cloned()
            .ok_or_else(|| {
                VmError::new(format!(
                    "internal VM error: constant index {index} out of bounds"
                ))
            })
    }

    fn read_constant_string(&self, index: usize) -> Result<String, VmError> {
        let val = self.read_constant(index)?;
        match val {
            Value::String(s) => Ok(s),
            other => Err(VmError::new(format!(
                "expected string constant at index {index}, got {}",
                self.type_name(&other)
            ))),
        }
    }

    // ── Frame access ──────────────────────────────────────────────

    fn current_frame(&self) -> Result<&CallFrame, VmError> {
        self.frames
            .last()
            .ok_or_else(|| VmError::new("internal VM error: no call frame".to_string()))
    }

    fn current_frame_mut(&mut self) -> Result<&mut CallFrame, VmError> {
        self.frames
            .last_mut()
            .ok_or_else(|| VmError::new("internal VM error: no call frame".to_string()))
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
            let span = frame.closure.function.chunk.span_at(ip);
            if span.line > 0 {
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
            let func_name = frame.closure.function.name.clone();
            let ip = frame.ip.saturating_sub(1);
            let span = frame.closure.function.chunk.span_at(ip);
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
            Value::ExtFloat(f) => f.to_string(),
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
    /// Do NOT use this for method-dispatch keys — use
    /// `value_type_name_for_dispatch` (which routes through the
    /// canonical name oracle in `crate::types::canonical`) instead.
    pub fn type_name(&self, val: &Value) -> &'static str {
        match val {
            Value::Int(_) => "Int",
            Value::Float(_) => "Float",
            Value::ExtFloat(_) => "ExtFloat",
            Value::Bool(_) => "Bool",
            Value::String(_) => "String",
            Value::List(_) => "List",
            // `stringify!` is used here instead of the bare string
            // literal so the architectural lock test
            // (tests/canonical_type_arch_lock_tests.rs) can grep for
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
            // `dispatch_name_for_value`. Round 71 follow-up unified
            // `Function` / `Fun` / `Fn` on `"Fn"`.
            Value::VmClosure(_) => "Fn",
            Value::BuiltinFn(_) => "BuiltinFn",
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
    ///   - `Variant(tag, _)` → resolves to the parent enum type via the
    ///     `__type_of__<tag>` global registered by the compiler, falling
    ///     back to the bare tag.
    ///   - `VariantConstructor(name, _)` → ``"VariantConstructor `name`"``
    ///     (TitleCase, no "a " article).
    ///   - `TypeDescriptor(name)` / `PrimitiveDescriptor(name)` →
    ///     ``"TypeDescriptor `name`"`` / ``"PrimitiveDescriptor `name`"``.
    ///
    /// All other variants delegate to `type_name` so the two paths
    /// produce byte-identical output. The `pub` visibility is required
    /// by `tests/round75_kind_naming_canonical_tests.rs`, which pins
    /// the alignment matrix.
    pub fn user_facing_type_name(&self, val: &Value) -> String {
        match val {
            // Variants that carry semantic content into the user-facing
            // diagnostic. Each is a deliberate alias documented above.
            Value::Record(name, _) => name.clone(),
            Value::Variant(tag, _) => {
                let key = format!("__type_of__{tag}");
                if let Some(Value::String(type_name)) = self.globals.get(&key) {
                    type_name.clone()
                } else {
                    tag.clone()
                }
            }
            Value::VariantConstructor(name, _) => {
                format!("VariantConstructor `{name}`")
            }
            Value::TypeDescriptor(name) => {
                format!("TypeDescriptor `{name}`")
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

    /// Get the type name for method dispatch. For variants, looks up the parent type.
    ///
    /// This name is used to build the qualified global lookup key
    /// `"<TypeName>.<method>"` in `Op::CallMethod`. Returning the
    /// canonical `type_name` for every variant is load-bearing: if the
    /// name disagrees with what the compiler registers (e.g. returning
    /// `"Unknown"` for `ExtFloat`), the qualified-global miss falls
    /// through to `dispatch_trait_method` with a stringly-typed type
    /// name that no fallback arm matches — producing spurious
    /// `"no method '<m>' for type 'Unknown'"` errors. Keep this in sync
    /// with `type_name` above and `user_facing_type_name`.
    ///
    /// Phase C of the canonical-type-equality refactor centralises the
    /// shape-only mapping in `crate::types::canonical::dispatch_name_for_value`
    /// (the single source of truth for value-shape -> dispatch-name
    /// reduction, including the `Range -> List` collapse). This method
    /// remains a thin wrapper because the `Value::Variant` case needs
    /// the VM's `__type_of__<tag>` globals lookup, which the canonical
    /// module — by design — does not have access to.
    fn value_type_name_for_dispatch(&self, val: &Value) -> String {
        if let Some(name) = dispatch_name_for_value(val) {
            return name;
        }
        // The shape-only canonical helper returned None; the only
        // variant that lands here is `Value::Variant`, whose dispatch
        // name comes from the VM-side `__type_of__<tag>` global table.
        match val {
            Value::Variant(tag, _) => {
                let key = format!("__type_of__{tag}");
                if let Some(Value::String(type_name)) = self.globals.get(&key) {
                    type_name.clone()
                } else {
                    tag.clone() // fallback: use the tag itself
                }
            }
            // Unreachable: dispatch_name_for_value returns Some(_) for
            // every Value variant except Variant. Defensive: if a new
            // Value variant is added without updating dispatch_name_for_value,
            // the unit tests in src/types/canonical.rs catch the omission;
            // this match arm exists only so the compiler can prove
            // exhaustiveness.
            other => format!("{other:?}"),
        }
    }
}

#[cfg(test)]
mod tests;
