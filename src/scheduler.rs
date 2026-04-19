//! M:N scheduler mapping lightweight tasks onto a fixed-size thread pool.
//!
//! Spawned tasks run cooperatively on worker threads. Channel operations
//! park tasks instead of blocking OS threads, and wakers re-enqueue
//! them when data arrives.

use parking_lot::{Condvar, Mutex};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Weak;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::value::{IoCompletion, TaskHandle, Value, WakerRegistration};
use crate::vm::{BlockReason, SelectOpKind, Vm, VmError};

// `test_hooks` and `test_support` are public so integration tests in
// `tests/` (separate crate, no `cfg(test)`) can `use` them. The
// fire_hook! macro that calls into `test_hooks` stays feature-gated
// (`feature = "test-hooks"`) so the hot scheduler path pays zero cost
// when the feature is off. Marked `#[doc(hidden)]` to discourage
// downstream crates from depending on the test API.
#[doc(hidden)]
pub mod test_hooks;
#[doc(hidden)]
pub mod test_support;
pub mod wake_graph;

pub use wake_graph::{MainTarget, SelectEdge as WakeSelectEdge};
use wake_graph::{NodeId, ParkEdge, SelectEdge, WakeGraph};

/// Callback invoked on every wake-graph state change. Type-aliased to
/// keep `SchedulerInner::main_waiters` legible — clippy's
/// `type_complexity` lint flags the inline form. Trait-object shape
/// chosen so the watchdog can hold callbacks across `Vm::run` without
/// caring about the concrete `Fn` type.
pub type MainWaiterCallback = Arc<dyn Fn() + Send + Sync>;

/// Fire a scheduler instrumentation hook. Compiles to a no-op outside
/// `cfg(test)` / `feature = "test-hooks"`. Each call site names a
/// stable transition point so a Phase-3 test can install a hook that
/// blocks (e.g. on a barrier) at exactly the racy moment.
macro_rules! fire_hook {
    ($which:ident, $tag:expr) => {
        #[cfg(any(test, feature = "test-hooks"))]
        {
            $crate::scheduler::test_hooks::$which($tag);
        }
        #[cfg(not(any(test, feature = "test-hooks")))]
        {
            let _ = $tag;
        }
    };
}

/// Maximum number of live (active + blocked + queued) tasks the scheduler allows.
const MAX_TASKS: usize = 100_000;

/// Parse a duration string like `"30s"`, `"500ms"`, `"5m"`, `"2h"`, or
/// `"none"`/empty. Returns `None` for disabled/invalid input — the caller
/// treats `None` as "no timeout configured" (infinite wait).
fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("none") || s.eq_ignore_ascii_case("off") {
        return None;
    }
    let unit_start = s.find(|c: char| c.is_alphabetic())?;
    let (num_part, unit) = s.split_at(unit_start);
    let n: u64 = num_part.trim().parse().ok()?;
    match unit.trim().to_lowercase().as_str() {
        "ms" => Some(Duration::from_millis(n)),
        "s" | "sec" | "secs" => Some(Duration::from_secs(n)),
        "m" | "min" | "mins" => Some(Duration::from_secs(n.checked_mul(60)?)),
        "h" | "hr" | "hrs" => Some(Duration::from_secs(n.checked_mul(3600)?)),
        _ => None,
    }
}

/// Source of an I/O watchdog deadline — determines the error message
/// the watchdog fires. `Global` comes from `SILT_IO_TIMEOUT`; `Task`
/// comes from a scoped `task.deadline(dur, fn)` block.
#[derive(Clone, Copy)]
pub(crate) enum DeadlineSource {
    Global,
    Task,
}

impl DeadlineSource {
    /// User-visible message surfaced as the inner String of the `Err`
    /// variant when an I/O times out. Silt-side match arms pattern on
    /// this exact text, so it is part of the public contract.
    pub(crate) fn message(self) -> &'static str {
        match self {
            DeadlineSource::Global => "I/O timeout (SILT_IO_TIMEOUT exceeded)",
            DeadlineSource::Task => "I/O timeout (task.deadline exceeded)",
        }
    }
}

/// An entry in the I/O watchdog registry. When the watchdog thread
/// scans and finds an entry whose `deadline <= now`, it fires
/// `completion.complete(Err(...))` to unblock the task with a timeout
/// error. The `Weak` reference ensures a dropped task's completion
/// doesn't keep the watchdog holding memory.
struct WatchdogEntry {
    task_id: usize,
    completion: Weak<IoCompletion>,
    deadline: Instant,
    source: DeadlineSource,
}

/// Registry of in-flight I/O operations watched for timeout. Populated
/// whenever a task blocks on I/O with an effective deadline — either
/// from `SILT_IO_TIMEOUT` (global) or `task.deadline` (per-task scope).
pub(crate) struct WatchdogRegistry {
    entries: Mutex<Vec<WatchdogEntry>>,
    /// How frequently the watchdog thread scans the registry.
    /// Controlled by `SILT_IO_WATCHDOG_INTERVAL`, defaulted below.
    interval: Duration,
    shutdown: AtomicBool,
}

impl WatchdogRegistry {
    fn new(interval: Duration) -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            interval,
            shutdown: AtomicBool::new(false),
        }
    }

    fn add(
        &self,
        task_id: usize,
        completion: &Arc<IoCompletion>,
        deadline: Instant,
        source: DeadlineSource,
    ) {
        self.entries.lock().push(WatchdogEntry {
            task_id,
            completion: Arc::downgrade(completion),
            deadline,
            source,
        });
    }

    fn remove(&self, task_id: usize) {
        let mut entries = self.entries.lock();
        if let Some(pos) = entries.iter().position(|e| e.task_id == task_id) {
            entries.swap_remove(pos);
        }
    }

    /// Scan the registry for overdue entries. For each one, fire an
    /// `Err("...")` into the completion (no-op if the real I/O already
    /// wrote a result — `IoCompletion::complete` is first-writer-wins).
    /// Returns the number of timeouts fired for test introspection.
    ///
    /// The firing happens *outside* the `entries` lock: `completion.complete`
    /// drains registered wakers synchronously, and the I/O waker's requeue
    /// path calls back into `WatchdogRegistry::remove` — which re-acquires
    /// `self.entries.lock()`. Holding the lock across the completion would
    /// deadlock the watchdog thread on the same parking_lot mutex. So we
    /// drain overdue entries into a local vec under the lock, release the
    /// lock, then fire completions.
    fn scan_and_fire(&self) -> usize {
        let to_fire: Vec<(Weak<IoCompletion>, &'static str)> = {
            let mut entries = self.entries.lock();
            let now = Instant::now();
            let mut drained = Vec::new();
            entries.retain(|entry| {
                if now < entry.deadline {
                    return true; // not overdue, keep watching
                }
                drained.push((entry.completion.clone(), entry.source.message()));
                false // remove from registry regardless (won't fire again)
            });
            drained
        }; // lock released here
        let mut fired = 0;
        for (weak_completion, msg) in to_fire {
            if let Some(completion) = weak_completion.upgrade() {
                let err_value = Value::Variant("Err".into(), vec![Value::String(msg.to_string())]);
                if completion.complete(err_value) {
                    fired += 1;
                }
            }
        }
        fired
    }
}

/// Watchdog worker loop. Wakes every `interval`, scans registry, fires
/// timeouts on overdue entries. Exits cleanly on shutdown signal.
fn watchdog_loop(registry: Arc<WatchdogRegistry>) {
    while !registry.shutdown.load(Ordering::SeqCst) {
        thread::sleep(registry.interval);
        if registry.shutdown.load(Ordering::SeqCst) {
            return;
        }
        registry.scan_and_fire();
    }
}

/// Result of running a task's VM for one time slice.
pub enum SliceResult {
    /// Time slice expired; task is still runnable.
    Yielded,
    /// Task completed with a value.
    Completed(Value),
    /// Task failed with an error.
    Failed(VmError),
    /// Task is blocked on a channel/join operation.
    /// The block_reason on the VM describes what it's waiting for.
    Blocked,
}

/// A lightweight task scheduled on the M:N thread pool.
pub struct Task {
    pub id: usize,
    pub vm: Vm,
    pub handle: Arc<TaskHandle>,
}

/// Shared state for the M:N scheduler.
pub struct Scheduler {
    inner: Arc<SchedulerInner>,
    /// Worker thread handles, created lazily on first submit.
    workers: Mutex<Option<Vec<thread::JoinHandle<()>>>>,
}

struct SchedulerInner {
    run_queue: Mutex<VecDeque<Task>>,
    condvar: Condvar,
    shutdown: AtomicBool,
    /// Number of tasks that haven't yet completed (active + blocked + queued).
    /// Used by the wake graph as the "fuel" set: any live task NOT
    /// currently parked on a graph edge is universal fuel — the
    /// detector cannot fire while one exists. Also enforces `MAX_TASKS`.
    live_tasks: AtomicUsize,
    /// Number of tasks that are "in flight" but not yet settled. A task is
    /// unsettled from the moment it is enqueued (submit / requeue / yield)
    /// until it has either:
    ///   * (a) finished executing for the current step (Completed, Failed,
    ///     terminal error), OR
    ///   * (b) parked successfully — the worker has run a slice that
    ///     blocked the task and registered a waker on its blocking edge
    ///     (channel send/recv, select, join, I/O completion). Once a
    ///     waker is registered, an external event will requeue the task
    ///     and `unsettled_tasks` will be re-incremented at that point.
    ///
    /// CRITICAL: `pop_front` does NOT decrement this counter. The window
    /// between worker dequeue and waker registration is exactly the
    /// region where, pre-Phase-3, the polling watchdog could observe
    /// "no live runnable task" even though one was about to register.
    /// Wake-graph BFS now consults `live_tasks` membership directly,
    /// but `unsettled_tasks` is still tracked: the wake graph's
    /// `live_tasks` mirror is updated under its own mutex on submit /
    /// complete, so the in-flight counter pulses `signal_progress` to
    /// keep main's local condvar woken across the dequeue → register
    /// window.
    unsettled_tasks: AtomicUsize,
    /// Reserved for cross-process deadlock state. The worker-side
    /// detector that used to flip this flag was removed because it could
    /// not distinguish "main thread is descheduled" from "main thread is
    /// stuck"; deadlock detection now happens exclusively on the main
    /// thread via the wake graph (see `main_thread_wait_for_*` in
    /// `src/builtins/concurrency.rs`). This flag is currently never
    /// flipped, but is retained so existing accessors / tests do not
    /// have to change shape.
    deadlock_detected: AtomicBool,
    /// Always-on I/O watchdog registry. Entries are added only when an
    /// I/O block has an effective deadline (from SILT_IO_TIMEOUT or
    /// task.deadline). If neither is in effect for a given block, no
    /// entry is added and the wait is indefinite.
    watchdog: Arc<WatchdogRegistry>,
    /// Global I/O timeout from `SILT_IO_TIMEOUT`. When set, every I/O
    /// block registers with `now + global_io_timeout` as its deadline
    /// unless a tighter task.deadline is in effect.
    global_io_timeout: Option<Duration>,
    /// Phase 3 wake graph: per-task park edges + reverse listener
    /// indices, used by [`Scheduler::is_main_starved`] for
    /// event-driven deadlock detection. Mutated under its own internal
    /// `Mutex` at every park / wake / spawn / complete site so the
    /// graph stays consistent with the three counter atomics.
    wake_graph: WakeGraph,
    /// Per-main-thread-waiter callbacks fired on every graph mutation
    /// (submit, requeue, complete, on_park, on_wake). The main-thread
    /// `wait_for_*` loops in `src/builtins/concurrency.rs` install a
    /// callback that pokes their local condvar so any state change
    /// flips them out of `wait_for` immediately — no 100ms polling.
    /// Stored as `(id, callback)` so the waiter can deregister on
    /// drop without traversing the entire vec by closure identity.
    /// Behind a `Mutex` because installs / removes are rare (one per
    /// main-thread block) but signal_progress fires often (every
    /// task transition).
    main_waiters: Mutex<Vec<(u64, MainWaiterCallback)>>,
    /// Monotonic id source for `main_waiters` entries. Used by the
    /// `MainWaiterGuard` Drop to find its own entry on deregister.
    next_main_waiter_id: AtomicU64,
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl Scheduler {
    /// Create a new scheduler (does NOT start worker threads yet).
    pub fn new() -> Self {
        let global_io_timeout = std::env::var("SILT_IO_TIMEOUT")
            .ok()
            .and_then(|s| parse_duration(&s));
        // Watchdog scan interval: env override, else a reasonable default.
        // When SILT_IO_TIMEOUT is set, scale to timeout/4 (capped at 1s).
        // Without SILT_IO_TIMEOUT, task.deadline is the only consumer —
        // default to 100ms so sub-second deadlines fire promptly.
        // Floored at 10ms to avoid pathological busy-scanning.
        let interval = std::env::var("SILT_IO_WATCHDOG_INTERVAL")
            .ok()
            .and_then(|s| parse_duration(&s))
            .unwrap_or_else(|| {
                global_io_timeout
                    .map(|t| (t / 4).min(Duration::from_secs(1)))
                    .unwrap_or(Duration::from_millis(100))
            })
            .max(Duration::from_millis(10));
        let watchdog = Arc::new(WatchdogRegistry::new(interval));
        Scheduler {
            inner: Arc::new(SchedulerInner {
                run_queue: Mutex::new(VecDeque::new()),
                condvar: Condvar::new(),
                shutdown: AtomicBool::new(false),
                live_tasks: AtomicUsize::new(0),
                unsettled_tasks: AtomicUsize::new(0),
                deadlock_detected: AtomicBool::new(false),
                watchdog,
                global_io_timeout,
                wake_graph: WakeGraph::new(),
                main_waiters: Mutex::new(Vec::new()),
                next_main_waiter_id: AtomicU64::new(0),
            }),
            workers: Mutex::new(None),
        }
    }

    /// Ensure worker threads are running.
    fn ensure_workers(&self) {
        let mut guard = self.workers.lock();
        if guard.is_some() {
            return;
        }

        let num_workers = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .max(2); // At least 2 workers to avoid deadlocks

        // Capacity: num_workers + 1 for the watchdog thread.
        let mut handles = Vec::with_capacity(num_workers + 1);
        for _ in 0..num_workers {
            let inner = self.inner.clone();
            handles.push(thread::spawn(move || {
                worker_loop(inner);
            }));
        }
        // Always start the watchdog thread. The registry is empty
        // unless something (SILT_IO_TIMEOUT or task.deadline) supplies
        // a deadline on I/O block — the scan loop is a cheap
        // `thread::sleep(interval)` in steady state.
        let registry = self.inner.watchdog.clone();
        handles.push(thread::spawn(move || {
            watchdog_loop(registry);
        }));
        *guard = Some(handles);
    }

    /// Returns true if a deadlock has been detected.
    pub fn deadlock_detected(&self) -> bool {
        self.inner.deadlock_detected.load(Ordering::SeqCst)
    }

    /// Register the main thread with the wake graph. Called once by
    /// `main_thread_wait_for_*` the first time the main thread parks
    /// on any primitive, so the graph knows there is a main-side
    /// caller worth proving deadlock for. Without this, the graph's
    /// BFS short-circuits to `false` (no main → nobody to declare
    /// deadlock for).
    pub fn register_main_present(&self) {
        self.inner.wake_graph.register_main_present();
    }

    /// True iff the wake graph can prove that no scheduled task could
    /// ever drive `target` forward. The watchdog should fire
    /// `deadlock` immediately on a `true` return — Phase 4 deleted
    /// the polling fallback, so this is now the SOLE deadlock signal
    /// the main-thread waiters consult.
    pub fn is_main_starved(&self, target: &MainTarget) -> bool {
        self.inner.wake_graph.is_main_starved(target)
    }

    /// Park the main thread on `target`. Adds an edge from
    /// `NodeId::MAIN` into the wake graph so other tasks' BFS sees
    /// main as a destination. Paired with `unpark_main` when the wait
    /// loop returns.
    pub fn park_main(&self, target: &MainTarget) {
        let edge = match target {
            MainTarget::Recv(ch) => ParkEdge::Recv(ch.clone()),
            MainTarget::Send(ch) => ParkEdge::Send(ch.clone()),
            MainTarget::Join(h) => ParkEdge::Join(*h),
            MainTarget::Select(edges) => ParkEdge::Select(edges.clone()),
        };
        self.inner.wake_graph.on_park(NodeId::MAIN, edge);
    }

    /// Unpark the main thread from whatever edge it was on.
    pub fn unpark_main(&self) {
        self.inner.wake_graph.on_wake(NodeId::MAIN);
    }

    /// Install a callback fired by every wake-graph state change
    /// (`signal_progress` is called at every submit / requeue /
    /// complete / on_park / on_wake site). The callback is invoked
    /// synchronously on whichever thread caused the state change, so
    /// it must be cheap and non-blocking — typically a
    /// `condvar.notify_one()` poke that flips the waiter out of
    /// `wait_for`.
    ///
    /// Returns a `MainWaiterGuard` whose `Drop` deregisters the
    /// callback. The watcher MUST keep this guard alive across the
    /// entire wait loop and drop it on exit — otherwise a stale
    /// callback fires into freed memory on the next graph mutation.
    pub fn install_main_waiter(self: &Arc<Self>, callback: MainWaiterCallback) -> MainWaiterGuard {
        let id = self
            .inner
            .next_main_waiter_id
            .fetch_add(1, Ordering::Relaxed);
        self.inner.main_waiters.lock().push((id, callback));
        MainWaiterGuard {
            scheduler: self.clone(),
            id,
        }
    }

    /// Submit a runnable task to the scheduler.
    ///
    /// Returns an error if the live-task count has reached [`MAX_TASKS`].
    pub fn submit(&self, task: Task) -> Result<(), String> {
        self.ensure_workers();
        let current = self.inner.live_tasks.load(Ordering::SeqCst);
        if current >= MAX_TASKS {
            return Err(format!(
                "task limit exceeded: {} tasks running (max {MAX_TASKS})",
                current
            ));
        }
        // Bump unsettled_tasks BEFORE live_tasks so any observer sees
        // a "definitely progressing" counter throughout the submit
        // window. The wake graph's BFS treats any live task absent
        // from `edges` as universal fuel; the unsettled counter
        // additionally pulses `signal_progress` so main waiters
        // re-check immediately when a fresh task enters the queue.
        self.inner.unsettled_tasks.fetch_add(1, Ordering::SeqCst);
        self.inner.live_tasks.fetch_add(1, Ordering::SeqCst);
        // Wake graph: register the task as live (runnable, no edge
        // yet) BEFORE pushing it on the queue so a racing main-thread
        // BFS that fires after the queue push sees the live entry.
        self.inner.wake_graph.on_spawn(task.id);
        fire_hook!(on_submit, "submit_after_counters");
        let mut queue = self.inner.run_queue.lock();
        queue.push_back(task);
        self.inner.condvar.notify_one();
        // A new fuel node is now in the graph — any main-thread
        // watcher should re-check.
        signal_progress(&self.inner);
        Ok(())
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        self.inner.shutdown.store(true, Ordering::SeqCst);
        self.inner.watchdog.shutdown.store(true, Ordering::SeqCst);
        self.inner.condvar.notify_all();
        if let Some(workers) = self.workers.lock().take() {
            // The runtime that owns this `Arc<Scheduler>` is itself
            // owned by a `Vm`. A worker thread may run a task whose
            // completion drops the LAST `Arc<Runtime>` (e.g. main has
            // already returned and dropped its own Vm, so the only
            // remaining ref was the worker's currently-running task).
            // In that case `Scheduler::drop` runs ON a worker thread.
            // `JoinHandle::join` on an already-finished thread is fine,
            // but joining the CURRENT thread panics with EDEADLK
            // (`std::sys::thread::unix::Thread::join` line 127:
            // `assert!(ret == 0, "failed to join thread: ...")`). To
            // avoid that we forget any handle whose thread id matches
            // ours — the OS will clean up the (already-exited) thread.
            let me = thread::current().id();
            for w in workers {
                if w.thread().id() == me {
                    // Don't join self — the join would deadlock and
                    // the std-side assertion would abort the process.
                    // We're the last code that will ever run on this
                    // thread anyway (we're inside `drop`, which is
                    // called by the worker_loop's task drop after the
                    // worker has returned from its inner loop body).
                    std::mem::forget(w);
                } else {
                    let _ = w.join();
                }
            }
        }
    }
}

/// The worker loop: dequeue tasks, run them for a time slice, handle results.
fn worker_loop(inner: Arc<SchedulerInner>) {
    let time_slice: usize = std::env::var("SILT_TIME_SLICE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2000);

    loop {
        // Dequeue a task.
        let task = {
            let mut queue = inner.run_queue.lock();
            loop {
                if inner.shutdown.load(Ordering::SeqCst) {
                    return;
                }
                if let Some(task) = queue.pop_front() {
                    // Do NOT decrement `unsettled_tasks` here. The whole
                    // point of the counter is to cover the dequeue →
                    // register-waker window: between this `pop_front` and
                    // the moment the worker has either completed the task
                    // or parked it on a wakeable edge, no other counter
                    // can prove the task is still going to make progress.
                    // The two decrement sites are below in the
                    // Completed / Failed / Blocked-with-waker arms.
                    fire_hook!(on_dequeue, "pop_front");
                    break task;
                }
                // Wake periodically to re-check the queue. The actual
                // deadlock decision is made by the main-thread watchdog in
                // `src/builtins/concurrency.rs::main_thread_wait_for_*`,
                // which can distinguish a real deadlock from "main is just
                // descheduled / busy in VM bytecode" — something a worker
                // thread cannot prove. A worker that times out here simply
                // resumes waiting for the condvar.
                let _ = inner.condvar.wait_for(&mut queue, Duration::from_secs(1));
            }
        };

        let Task { id, mut vm, handle } = task;

        let result = vm.execute_slice(time_slice);

        match result {
            SliceResult::Yielded => {
                // Task still runnable — put it back. The task is still
                // unsettled (it has not parked on a wakeable edge and has
                // not completed), so `unsettled_tasks` is unchanged.
                let mut queue = inner.run_queue.lock();
                queue.push_back(Task { id, vm, handle });
                inner.condvar.notify_one();
            }
            SliceResult::Completed(val) => {
                handle.complete(Ok(val));
                // Terminal step for this task: settle it before
                // decrementing live so an observer that sees `live--`
                // also sees `unsettled--`.
                inner.unsettled_tasks.fetch_sub(1, Ordering::SeqCst);
                inner.live_tasks.fetch_sub(1, Ordering::SeqCst);
                // Wake graph: drop the node + any edge so subsequent
                // BFS does not see a phantom fuel node, and signal
                // any main-thread waiter to re-check (the just-
                // completed task may have been the last fuel).
                inner.wake_graph.on_complete(id);
                signal_progress(&inner);
            }
            SliceResult::Failed(err) => {
                handle.complete(Err(vm.enrich_error(err)));
                inner.unsettled_tasks.fetch_sub(1, Ordering::SeqCst);
                inner.live_tasks.fetch_sub(1, Ordering::SeqCst);
                inner.wake_graph.on_complete(id);
                signal_progress(&inner);
            }
            SliceResult::Blocked => {
                // Take the block reason from the VM.
                let reason = vm.take_block_reason();
                let task_slot: Arc<Mutex<Option<Task>>> =
                    Arc::new(Mutex::new(Some(Task { id, vm, handle })));

                // Track whether this block is on external I/O. The wake
                // graph models I/O parks as `ParkEdge::Io` (always-fuel),
                // but the I/O *watchdog* (SILT_IO_TIMEOUT) still needs
                // to know whether the requeue cleared an entry from
                // `WatchdogRegistry`.
                let was_io = matches!(reason, Some(BlockReason::Io(_)));
                // Snapshot whether the block had a reason before the
                // match below moves out of `reason`. Used for the final
                // settle decrement on `unsettled_tasks` after the arm
                // finishes registering wakers.
                let had_block_reason = reason.is_some();

                // Track that this task is now blocked (unless no block reason,
                // which is treated as a yield and re-enqueued immediately).
                if reason.is_some() {
                    // SAFETY: task_slot was just created with Some(Task{..}) above.
                    let handle_for_registry = task_slot
                        .lock()
                        .as_ref()
                        .expect("task_slot just initialized")
                        .handle
                        .clone();

                    // Register cancel cleanup: if the task is cancelled while
                    // blocked, take it from the slot (making the waker a no-op)
                    // and tear down the wake-graph node so the BFS doesn't
                    // see a phantom parked task.
                    let cancel_slot = task_slot.clone();
                    let cancel_inner = inner.clone();
                    let cancel_task_id = id;
                    handle_for_registry.set_cancel_cleanup(Box::new(move || {
                        // Take the task so the waker closure becomes a no-op.
                        // If the slot is empty, the waker already fired and
                        // the task is either running or already accounted
                        // for — do not double-decrement the counters here.
                        if cancel_slot.lock().take().is_none() {
                            return;
                        }
                        // This blocked task is being dropped: decrement
                        // live_tasks (the Completed/Failed path is
                        // unreachable once we drop the task, so live_tasks
                        // would otherwise leak).
                        if was_io {
                            cancel_inner.watchdog.remove(cancel_task_id);
                        }
                        cancel_inner.live_tasks.fetch_sub(1, Ordering::SeqCst);
                        // Wake graph: cancelled-while-blocked is the
                        // moral equivalent of `on_complete` — the task
                        // is gone forever; drop the edge and the live
                        // entry so subsequent BFS does not see a phantom
                        // fuel node, and pulse main waiters to re-check.
                        cancel_inner.wake_graph.on_complete(cancel_task_id);
                        signal_progress(&cancel_inner);
                    }));
                }

                // Wake graph: commit the parked edge for THIS task
                // BEFORE registering the channel waker. If the waker
                // fires inline (rendezvous-handshake-already-pending
                // case), `requeue` will call `wake_graph.on_wake(node)`
                // and clear the edge again — net zero. A racing main
                // BFS that catches the transient edge between commit
                // and inline-fire-requeue sees a parked node with no
                // fuel reachable from it and may report starved; the
                // watchdog re-checks before firing, so the transient
                // does not cause a false positive (the second check
                // is post-requeue and the node is gone).
                let park_edge_for_graph = match &reason {
                    Some(BlockReason::Receive(ch)) => Some(ParkEdge::Recv(ch.clone())),
                    Some(BlockReason::Send(ch)) => Some(ParkEdge::Send(ch.clone())),
                    Some(BlockReason::Select(ops)) => Some(ParkEdge::Select(
                        ops.iter()
                            .map(|(ch, kind)| match kind {
                                SelectOpKind::Receive => SelectEdge::Recv(ch.clone()),
                                SelectOpKind::Send => SelectEdge::Send(ch.clone()),
                            })
                            .collect(),
                    )),
                    Some(BlockReason::Join(h)) => Some(ParkEdge::Join(h.id)),
                    Some(BlockReason::Io(_)) => Some(ParkEdge::Io),
                    None => None,
                };
                if let Some(edge) = park_edge_for_graph {
                    inner.wake_graph.on_park(NodeId::Task(id), edge);
                    // A new edge could be a Send on a channel that
                    // main is waiting to recv from — pulse so main's
                    // BFS sees the new fuel.
                    signal_progress(&inner);
                }

                match reason {
                    Some(BlockReason::Receive(ch)) => {
                        fire_hook!(on_park, "blocked_arm_entry_recv");
                        // Capture the handle BEFORE registering the waker.
                        // `register_recv_waker_guard` may synchronously invoke
                        // the waker closure if a peer is already parked at the
                        // rendezvous (e.g. a sender already waiting). That
                        // closure takes `task_slot`, leaving it `None` — so
                        // cloning the handle afterwards would panic on
                        // `expect("task_slot just initialized")`. The slot was
                        // initialized above this `match` and nothing mutates
                        // it between there and here.
                        let handle_for_cancel = task_slot
                            .lock()
                            .as_ref()
                            .expect("task_slot just initialized")
                            .handle
                            .clone();
                        let slot = task_slot.clone();
                        let inner2 = inner.clone();
                        let reg = ch.register_recv_waker_guard(Box::new(move || {
                            if let Some(task) = slot.lock().take() {
                                requeue(&inner2, task, false);
                            }
                        }));
                        // Re-install the cancel cleanup so it ALSO owns
                        // the `WakerRegistration` guard. The guard's
                        // Drop deregisters the recv waker from the
                        // channel on any path that drops the closure
                        // (cancel → `complete` fires it, then closure
                        // drops; or normal wake → `requeue` calls
                        // `clear_cancel_cleanup`, closure drops).
                        // Without this, round-27 B1/B2 leak the waker
                        // into `recv_wakers` and permanently inflate
                        // `waiting_receivers`: a later unrelated
                        // `try_send` sees a phantom receiver (B1), or
                        // a real receiver behind the dead waker in the
                        // FIFO never wakes (B2).
                        //
                        // Phase 3 / round 31: only install the new
                        // cleanup if `task_slot` is still `Some`. If
                        // `register_*_waker_guard` inline-fired (the
                        // common rendezvous case), the closure took the
                        // task out of `task_slot` and `requeue` already
                        // cleared the prior cleanup AND reset the wake-
                        // graph edge. A subsequent `set_cancel_cleanup`
                        // here would race with a concurrent worker that
                        // has already picked the requeued task up,
                        // entered a NEW Blocked arm, and installed ITS
                        // arm-specific cleanup. Replacing that newer
                        // cleanup drops a `WakerRegistration` whose
                        // entry is still live in the channel — the drop
                        // calls `remove_*_waker`, deregistering the
                        // newer arm's waker. Result: a parked task with
                        // no waker, the wake-graph still listing it as
                        // a Send/Recv listener, and the deadlock
                        // detector firing a real-looking false positive.
                        //
                        // Closing the check+set under the same
                        // `task_slot` lock keeps the protocol race-free:
                        // if the slot is empty when checked, the
                        // inline-fire (or a concurrent wake) already
                        // owns the task and we must not touch the
                        // cleanup; if the slot is `Some` while we hold
                        // the lock, no waker can fire mid-set (the
                        // waker closure also needs `slot.lock()` to
                        // proceed), so our `set_cancel_cleanup` cannot
                        // clobber a newer arm's cleanup. We do the
                        // `set_cancel_cleanup` while still holding the
                        // lock; the handle's `cancel_cleanup` Mutex is
                        // a different lock, so no deadlock.
                        let slot_guard = task_slot.lock();
                        if slot_guard.is_some() {
                            let cancel_slot = task_slot.clone();
                            let cancel_inner = inner.clone();
                            let cancel_task_id = id;
                            handle_for_cancel.set_cancel_cleanup(Box::new(move || {
                                // Move the guard into the body so its Drop
                                // runs at the end of this scope (cancel
                                // path) or when the closure itself is
                                // dropped (normal wake path).
                                let _reg = reg;
                                if cancel_slot.lock().take().is_none() {
                                    return;
                                }
                                if was_io {
                                    cancel_inner.watchdog.remove(cancel_task_id);
                                }
                                cancel_inner.live_tasks.fetch_sub(1, Ordering::SeqCst);
                                cancel_inner.wake_graph.on_complete(cancel_task_id);
                                signal_progress(&cancel_inner);
                            }));
                            drop(slot_guard);
                        } else {
                            drop(slot_guard);
                            // Inline-fire already requeued the task and
                            // dropped the prior cleanup; `reg` here
                            // refers to a drained entry whose Drop is a
                            // no-op deregister.
                            drop(reg);
                        }
                    }
                    Some(BlockReason::Send(ch)) => {
                        fire_hook!(on_park, "blocked_arm_entry_send");
                        // Capture the handle BEFORE registering the waker.
                        // `register_send_waker_guard` may synchronously invoke
                        // the waker closure if a peer is already parked at
                        // the rendezvous (e.g. a receiver already waiting).
                        // That closure takes `task_slot`, leaving it `None` —
                        // so cloning the handle afterwards would panic on
                        // `expect("task_slot just initialized")`. The slot
                        // was initialized above this `match` and nothing
                        // mutates it between there and here.
                        let handle_for_cancel = task_slot
                            .lock()
                            .as_ref()
                            .expect("task_slot just initialized")
                            .handle
                            .clone();
                        let slot = task_slot.clone();
                        let inner2 = inner.clone();
                        let reg = ch.register_send_waker_guard(Box::new(move || {
                            if let Some(task) = slot.lock().take() {
                                requeue(&inner2, task, false);
                            }
                        }));
                        // See the Receive arm: cancel cleanup owns the
                        // guard so Drop deregisters the send waker on
                        // cancel (round-27 B3/B4). Phase 3 / round 31:
                        // hold the `task_slot` lock across the
                        // is_some-check + set_cancel_cleanup so an
                        // inline-fire-then-new-arm sequence cannot
                        // clobber a concurrent worker's NEW arm
                        // cleanup. See the matching explanation in the
                        // Receive arm above.
                        let slot_guard = task_slot.lock();
                        if slot_guard.is_some() {
                            let cancel_slot = task_slot.clone();
                            let cancel_inner = inner.clone();
                            let cancel_task_id = id;
                            handle_for_cancel.set_cancel_cleanup(Box::new(move || {
                                let _reg = reg;
                                if cancel_slot.lock().take().is_none() {
                                    return;
                                }
                                if was_io {
                                    cancel_inner.watchdog.remove(cancel_task_id);
                                }
                                cancel_inner.live_tasks.fetch_sub(1, Ordering::SeqCst);
                                cancel_inner.wake_graph.on_complete(cancel_task_id);
                                signal_progress(&cancel_inner);
                            }));
                            drop(slot_guard);
                        } else {
                            drop(slot_guard);
                            drop(reg);
                        }
                    }
                    Some(BlockReason::Select(ops)) => {
                        fire_hook!(on_park, "blocked_arm_entry_select");
                        // Register waker on ALL channels. First waker to fire
                        // wakes the task AND deregisters the other siblings'
                        // wakers — this prevents a leaked `waiting_receivers`
                        // increment on the non-firing channels. Without this
                        // cleanup a rendezvous sender on a sibling channel
                        // would later see `waiting_receivers > 0` in
                        // `try_send`, place a value into the handoff slot,
                        // and return `Sent` with no real receiver waiting —
                        // a broken rendezvous handshake.
                        let cancelled = Arc::new(AtomicBool::new(false));
                        // Shared vec of registration guards. Whoever wins
                        // (winning waker OR cancel) drains the vec; dropping
                        // the drained guards deregisters every still-pending
                        // sibling from its channel. The winning waker's own
                        // entry is also drained but its `Drop` is idempotent:
                        // `remove_*_waker` returns false when the entry has
                        // already been popped by `wake_recv` / `wake_send`.
                        let entries: Arc<Mutex<Vec<WakerRegistration>>> =
                            Arc::new(Mutex::new(Vec::with_capacity(ops.len())));
                        // Replace the generic cancel_cleanup (set above)
                        // with a select-aware version: same wake-graph
                        // teardown plus sibling-waker removal via the
                        // guard vec. Select is never I/O, so the
                        // watchdog-registry call is omitted.
                        let select_slot = task_slot.clone();
                        let select_inner = inner.clone();
                        let select_task_id = id;
                        let cancelled_for_cancel = cancelled.clone();
                        let entries_for_cancel = entries.clone();
                        let handle_for_select = task_slot
                            .lock()
                            .as_ref()
                            .expect("task_slot just initialized")
                            .handle
                            .clone();
                        handle_for_select.set_cancel_cleanup(Box::new(move || {
                            if select_slot.lock().take().is_none() {
                                return;
                            }
                            // Prevent any still-racing waker from acting.
                            cancelled_for_cancel.store(true, Ordering::Release);
                            select_inner.live_tasks.fetch_sub(1, Ordering::SeqCst);
                            select_inner.wake_graph.on_complete(select_task_id);
                            signal_progress(&select_inner);
                            // Drop every still-pending registration guard;
                            // each Drop calls remove_*_waker.
                            drop(std::mem::take(&mut *entries_for_cancel.lock()));
                        }));
                        for (ch, kind) in ops.iter() {
                            if cancelled.load(Ordering::Acquire) {
                                // An earlier iteration's waker fired
                                // inline during its double-check. Don't
                                // register further wakers that would
                                // leak into the channel queue.
                                break;
                            }
                            let slot = task_slot.clone();
                            let inner2 = inner.clone();
                            let cancelled2 = cancelled.clone();
                            let entries2 = entries.clone();
                            let waker = Box::new(move || {
                                if cancelled2.load(Ordering::Acquire) {
                                    return; // Another waker already fired
                                }
                                if let Some(task) = slot.lock().take() {
                                    cancelled2.store(true, Ordering::Release);
                                    // Drain and drop sibling guards —
                                    // each Drop calls remove_*_waker.
                                    // Our own entry's guard is also in
                                    // the vec, but its Drop is a no-op
                                    // because `wake_*` already popped it.
                                    drop(std::mem::take(&mut *entries2.lock()));
                                    requeue(&inner2, task, false);
                                }
                            });
                            let reg = match kind {
                                SelectOpKind::Receive => ch.register_recv_waker_guard(waker),
                                SelectOpKind::Send => ch.register_send_waker_guard(waker),
                            };
                            // If the waker fired inline during the
                            // double-check inside register_*_waker, the
                            // winning waker already drained the vec and
                            // set `cancelled = true`. In that case drop
                            // our fresh guard immediately (idempotent
                            // no-op) and stop iterating.
                            if !cancelled.load(Ordering::Acquire) {
                                entries.lock().push(reg);
                            } else {
                                drop(reg);
                                break;
                            }
                        }
                    }
                    Some(BlockReason::Join(target_handle)) => {
                        fire_hook!(on_park, "blocked_arm_entry_join");
                        let slot = task_slot.clone();
                        let inner2 = inner.clone();
                        target_handle.register_join_waker(Box::new(move || {
                            if let Some(task) = slot.lock().take() {
                                requeue(&inner2, task, false);
                            }
                        }));
                    }
                    Some(BlockReason::Io(completion)) => {
                        fire_hook!(on_park, "blocked_arm_entry_io");
                        // Register with the watchdog if anything imposes
                        // a deadline: either SILT_IO_TIMEOUT (global) or
                        // a scoped task.deadline (per-task). The earlier
                        // deadline wins. If neither applies, the I/O
                        // waits indefinitely — no registration, no
                        // scan overhead.
                        let now = Instant::now();
                        let global_deadline = inner
                            .global_io_timeout
                            .and_then(|t| now.checked_add(t))
                            .map(|d| (d, DeadlineSource::Global));
                        let task_deadline = task_slot
                            .lock()
                            .as_ref()
                            .and_then(|t| t.vm.current_deadline)
                            .map(|d| (d, DeadlineSource::Task));
                        let effective = match (global_deadline, task_deadline) {
                            (Some((a, sa)), Some((b, sb))) => {
                                if a <= b {
                                    Some((a, sa))
                                } else {
                                    Some((b, sb))
                                }
                            }
                            (Some(x), None) | (None, Some(x)) => Some(x),
                            (None, None) => None,
                        };
                        if let Some((deadline, source)) = effective {
                            inner.watchdog.add(id, &completion, deadline, source);
                        }
                        let slot = task_slot.clone();
                        let inner2 = inner.clone();
                        completion.register_waker(Box::new(move || {
                            if let Some(task) = slot.lock().take() {
                                requeue(&inner2, task, true);
                            }
                        }));
                    }
                    None => {
                        // No block reason — shouldn't happen but treat as yield.
                        // The task stays unsettled (it neither completed nor
                        // parked with a waker), so do not touch the counter.
                        if let Some(task) = task_slot.lock().take() {
                            let mut queue = inner.run_queue.lock();
                            queue.push_back(task);
                            inner.condvar.notify_one();
                        }
                    }
                }
                // Settle: the worker has either finished registering a
                // waker on this task's blocking edge (channel/select/
                // join/io) or there was no block reason and it's been
                // re-enqueued. In either case the worker is done with
                // this task for the current step. Decrement
                // `unsettled_tasks` exactly once per Blocked arm — the
                // companion increment was the `submit` / `requeue` /
                // earlier-yield that put the task on the run queue.
                //
                // If the waker fired inline during register and called
                // `requeue`, that path bumped `unsettled_tasks` BEFORE
                // pushing the task back, so the net count after this
                // decrement is correct (queue length contribution = 1).
                //
                // If the cancel cleanup ran during register and
                // destroyed the task, the task is gone and decrementing
                // `unsettled_tasks` here drops the count to its true
                // resting value (no compensating push).
                if had_block_reason {
                    inner.unsettled_tasks.fetch_sub(1, Ordering::SeqCst);
                }
                // else: no block reason re-enqueued the task without
                // going through `requeue` (which would have incremented
                // unsettled_tasks). The task stays unsettled — same
                // semantics as Yielded above — so no decrement.
            }
        }
    }
}

/// Pulse every installed `main_waiter` callback. Called at every
/// state-change site (submit, requeue, complete, on_park, on_wake) so
/// any parked main-thread waiter re-checks the graph promptly. Cheap
/// in steady state: each callback is a single `Condvar::notify_one`
/// on a private mutex, and the typical waiter count is 0 or 1.
fn signal_progress(inner: &Arc<SchedulerInner>) {
    // Snapshot the callbacks under the lock, then fire them outside
    // the lock so a callback that re-enters the scheduler (e.g.
    // future code that touches counters during notify) cannot
    // deadlock on `main_waiters`.
    let snapshot: Vec<MainWaiterCallback> = {
        let waiters = inner.main_waiters.lock();
        waiters.iter().map(|(_, cb)| cb.clone()).collect()
    };
    for cb in snapshot {
        cb();
    }
}

/// RAII guard for a callback installed via
/// `Scheduler::install_main_waiter`. Drop deregisters the callback so
/// it does not fire after the watcher's local condvar is gone.
pub struct MainWaiterGuard {
    scheduler: Arc<Scheduler>,
    id: u64,
}

impl Drop for MainWaiterGuard {
    fn drop(&mut self) {
        let mut waiters = self.scheduler.inner.main_waiters.lock();
        if let Some(pos) = waiters.iter().position(|(wid, _)| *wid == self.id) {
            waiters.swap_remove(pos);
        }
    }
}

/// Re-enqueue a parked task on the scheduler's run queue.
///
/// `was_io` indicates whether the task was parked on external I/O —
/// in that case the watchdog registry entry must be cleared. For
/// internal-graph parks (channel/select/join) the waker site already
/// knows which arm it's in.
fn requeue(inner: &Arc<SchedulerInner>, task: Task, was_io: bool) {
    fire_hook!(on_wake, "requeue_entry");
    // Bump unsettled_tasks BEFORE pushing the task on the queue so any
    // observer sees a "definitely progressing" counter throughout the
    // requeue window. The worker that next runs this task will
    // decrement unsettled_tasks again — either when the slice
    // completes the task, when it parks with a registered waker, or
    // when the task yields again (in which case requeue runs a second
    // time and the counter stays balanced).
    inner.unsettled_tasks.fetch_add(1, Ordering::SeqCst);
    if was_io {
        inner.watchdog.remove(task.id);
    }
    // Clear the stale cancel-cleanup so it won't run when the task
    // completes normally.
    task.handle.clear_cancel_cleanup();
    // Wake graph: drop the parked edge for this task. If the task was
    // parked on Recv(ch), this clears the corresponding entry in
    // ch_recv_listeners — important because a future BFS would
    // otherwise treat this task as still parked-recv and walk past it.
    inner.wake_graph.on_wake(NodeId::Task(task.id));
    let mut queue = inner.run_queue.lock();
    queue.push_back(task);
    inner.condvar.notify_one();
    // The graph just lost a parked edge — pulse main waiters so they
    // re-check their target reachability.
    drop(queue);
    signal_progress(inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytecode::VmClosure;
    use crate::compiler::Compiler;
    use crate::lexer::Lexer;
    use crate::parser::Parser;
    use crate::vm::CallFrame;

    /// Compile a Silt snippet and return a VM ready for execute_slice.
    fn make_vm(src: &str) -> Vm {
        let tokens = Lexer::new(src).tokenize().expect("lexer error");
        let mut program = Parser::new(tokens).parse_program().expect("parse error");
        let _ = crate::typechecker::check(&mut program);
        let mut compiler = Compiler::new();
        let functions = compiler.compile_program(&program).expect("compile error");
        let script = Arc::new(functions.into_iter().next().unwrap());
        let mut vm = Vm::new();
        vm.is_scheduled_task = true;
        let closure = Arc::new(VmClosure {
            function: script,
            upvalues: vec![],
        });
        vm.frames.push(CallFrame {
            closure,
            ip: 0,
            base_slot: 0,
        });
        vm
    }

    fn make_task(id: usize, src: &str) -> (Task, Arc<TaskHandle>) {
        let handle = Arc::new(TaskHandle::new(id));
        let vm = make_vm(src);
        (
            Task {
                id,
                vm,
                handle: handle.clone(),
            },
            handle,
        )
    }

    // ── Basic lifecycle ────────────────────────────────────────────

    #[test]
    fn test_submit_and_join_single_task() {
        let scheduler = Scheduler::new();
        let (task, handle) = make_task(1, "fn main() { 42 }");
        scheduler.submit(task).unwrap();
        let result = handle.join();
        assert_eq!(result.ok(), Some(Value::Int(42)));
    }

    #[test]
    fn test_submit_multiple_tasks_all_complete() {
        let scheduler = Scheduler::new();
        let mut handles = Vec::new();
        for i in 0..10 {
            let src = format!("fn main() {{ {} }}", i);
            let (task, handle) = make_task(i, &src);
            scheduler.submit(task).unwrap();
            handles.push((i as i64, handle));
        }
        for (expected, handle) in handles {
            assert_eq!(handle.join().ok(), Some(Value::Int(expected)));
        }
    }

    #[test]
    fn test_failed_task_reports_error() {
        let scheduler = Scheduler::new();
        let (task, handle) = make_task(1, "fn main() { 1 / 0 }");
        scheduler.submit(task).unwrap();
        let result = handle.join();
        assert!(result.is_err(), "expected error from division by zero");
        assert!(
            result.unwrap_err().message.contains("division"),
            "error should mention division"
        );
    }

    // ── Counter bookkeeping ────────────────────────────────────────

    #[test]
    fn test_live_tasks_counter_reaches_zero() {
        let scheduler = Scheduler::new();
        let mut handles = Vec::new();
        for i in 0..5 {
            let (task, handle) = make_task(i, "fn main() { 1 }");
            scheduler.submit(task).unwrap();
            handles.push(handle);
        }
        for h in &handles {
            let _ = h.join();
        }
        // Give workers a moment to decrement.
        std::thread::sleep(Duration::from_millis(50));
        let live = scheduler.inner.live_tasks.load(Ordering::SeqCst);
        assert_eq!(
            live, 0,
            "live_tasks should be 0 after all tasks complete, got {live}"
        );
    }

    #[test]
    fn test_failed_task_decrements_live_counter() {
        let scheduler = Scheduler::new();
        let (task, handle) = make_task(1, "fn main() { 1 / 0 }");
        scheduler.submit(task).unwrap();
        let _ = handle.join();
        std::thread::sleep(Duration::from_millis(50));
        let live = scheduler.inner.live_tasks.load(Ordering::SeqCst);
        assert_eq!(
            live, 0,
            "live_tasks should be 0 after failed task, got {live}"
        );
    }

    // ── Shutdown ───────────────────────────────────────────────────

    #[test]
    fn test_drop_joins_workers_cleanly() {
        let scheduler = Scheduler::new();
        let (task, handle) = make_task(1, "fn main() { 1 }");
        scheduler.submit(task).unwrap();
        let _ = handle.join();
        // Drop the scheduler — should not hang or panic.
        drop(scheduler);
    }

    #[test]
    fn test_drop_empty_scheduler_is_noop() {
        // No tasks submitted — drop should be immediate.
        let scheduler = Scheduler::new();
        drop(scheduler);
    }

    // ── Deadlock detection ─────────────────────────────────────────

    #[test]
    fn test_io_blocked_task_does_not_trigger_deadlock() {
        // Regression lock for the channel-only deadlock split: a task that
        // performs real I/O should complete successfully, and the scheduler
        // must NOT have marked the session as deadlocked during the I/O
        // park window. Tests the I/O path end-to-end through the wake
        // graph (which models I/O parks as `ParkEdge::Io`, always-fuel).
        let scheduler = Scheduler::new();
        // Write a fixture file (small, completes fast).
        let path = std::env::temp_dir().join("silt_sched_io_block_test.txt");
        std::fs::write(&path, "hello").unwrap();
        // Forward-slash the path before embedding — Windows `temp_dir()`
        // returns backslash paths, and the silt lexer treats `\U`, `\A`,
        // etc. as unknown escape sequences in string literals. The
        // filesystem APIs accept `/` on Windows too.
        let path_str = path.display().to_string().replace('\\', "/");
        let src = format!(
            r#"
import io
fn main() {{
  match io.read_file("{}") {{
    Ok(s) -> s
    Err(_) -> "fail"
  }}
}}
        "#,
            path_str
        );
        let (task, handle) = make_task(1, &src);
        scheduler.submit(task).unwrap();
        let result = handle.join();
        assert_eq!(result.ok(), Some(Value::String("hello".into())));
        assert!(
            !scheduler.deadlock_detected(),
            "I/O-blocked task must not trigger deadlock detection"
        );
        let _ = std::fs::remove_file(path);
    }

    // ── I/O watchdog (SILT_IO_TIMEOUT) ────────────────────────────

    #[test]
    fn test_parse_duration_variants() {
        assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration("500ms"), Some(Duration::from_millis(500)));
        assert_eq!(parse_duration("5m"), Some(Duration::from_secs(300)));
        assert_eq!(parse_duration("2h"), Some(Duration::from_secs(7200)));
        assert_eq!(parse_duration(" 30 s "), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration("100 ms"), Some(Duration::from_millis(100)));
        assert_eq!(parse_duration("none"), None);
        assert_eq!(parse_duration("OFF"), None);
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("30"), None, "bare number — no unit");
        assert_eq!(parse_duration("30x"), None, "unknown unit");
        assert_eq!(parse_duration("abc"), None);
    }

    #[test]
    fn test_watchdog_fires_timeout_on_overdue_entry() {
        let registry = WatchdogRegistry::new(Duration::from_millis(10));
        let completion = IoCompletion::new();
        // Add an entry whose deadline is already in the past.
        let past = Instant::now() - Duration::from_secs(1);
        registry.add(1, &completion, past, DeadlineSource::Global);
        let fired = registry.scan_and_fire();
        assert_eq!(fired, 1, "one timeout should fire");
        let result = completion.try_get().expect("completion should be set");
        match result {
            Value::Variant(name, fields) => {
                assert_eq!(name.as_str(), "Err");
                let Value::String(msg) = &fields[0] else {
                    panic!("expected String in Err");
                };
                assert!(msg.contains("I/O timeout"), "unexpected: {msg}");
                assert!(msg.contains("SILT_IO_TIMEOUT"), "unexpected: {msg}");
            }
            other => panic!("expected Err variant, got {other:?}"),
        }
        assert!(registry.entries.lock().is_empty());
    }

    #[test]
    fn test_watchdog_fires_with_task_source_message() {
        let registry = WatchdogRegistry::new(Duration::from_millis(10));
        let completion = IoCompletion::new();
        let past = Instant::now() - Duration::from_secs(1);
        registry.add(1, &completion, past, DeadlineSource::Task);
        registry.scan_and_fire();
        let Value::Variant(_, fields) = completion.try_get().unwrap() else {
            panic!("expected variant");
        };
        let Value::String(msg) = &fields[0] else {
            panic!("expected String");
        };
        assert!(msg.contains("task.deadline"), "unexpected: {msg}");
    }

    #[test]
    fn test_watchdog_does_not_fire_on_fresh_entry() {
        let registry = WatchdogRegistry::new(Duration::from_millis(10));
        let completion = IoCompletion::new();
        let future = Instant::now() + Duration::from_secs(60);
        registry.add(1, &completion, future, DeadlineSource::Global);
        let fired = registry.scan_and_fire();
        assert_eq!(fired, 0);
        assert!(completion.try_get().is_none());
        assert_eq!(registry.entries.lock().len(), 1);
    }

    #[test]
    fn test_watchdog_does_not_clobber_completed_io() {
        let registry = WatchdogRegistry::new(Duration::from_millis(10));
        let completion = IoCompletion::new();
        let past = Instant::now() - Duration::from_secs(1);
        registry.add(1, &completion, past, DeadlineSource::Global);
        let ok_val = Value::Variant("Ok".into(), vec![Value::String("real".into())]);
        assert!(completion.complete(ok_val));
        let fired = registry.scan_and_fire();
        assert_eq!(fired, 0, "no timeout should fire — I/O already completed");
        let result = completion.try_get().expect("should still have Ok");
        match result {
            Value::Variant(name, fields) => {
                assert_eq!(name.as_str(), "Ok");
                assert_eq!(fields[0], Value::String("real".into()));
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn test_watchdog_global_timeout_none_when_env_unset() {
        // SAFETY: env var mutation is safe in tests since Rust 1.80+
        // uses thread-local env caches, and this test does not spawn
        // concurrent env readers. Required because Scheduler::new reads
        // the env once at construction time.
        unsafe { std::env::remove_var("SILT_IO_TIMEOUT") };
        let scheduler = Scheduler::new();
        assert!(
            scheduler.inner.global_io_timeout.is_none(),
            "global_io_timeout should be None when SILT_IO_TIMEOUT unset"
        );
    }

    #[test]
    fn test_watchdog_registry_removal_on_requeue() {
        let registry = WatchdogRegistry::new(Duration::from_secs(1));
        let c1 = IoCompletion::new();
        let c2 = IoCompletion::new();
        let d = Instant::now() + Duration::from_secs(30);
        registry.add(1, &c1, d, DeadlineSource::Global);
        registry.add(2, &c2, d, DeadlineSource::Global);
        assert_eq!(registry.entries.lock().len(), 2);
        registry.remove(1);
        let entries = registry.entries.lock();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].task_id, 2);
    }

    // Note: the previous `test_deadlock_detected_flag` was removed when
    // the worker-side deadlock detector was deleted. Deadlock detection
    // now lives entirely on the main thread (see
    // `main_thread_wait_for_send` / `_receive` / `_join` in
    // `src/builtins/concurrency.rs`), so the scheduler-only API
    // exercised by that test no longer has a way to declare a deadlock —
    // it requires a main-thread VM that is parked on a primitive. The
    // analogous program-level coverage lives in
    // `tests/scheduler_deadlock_detector_tests.rs::test_real_deadlock_*`
    // and the integration tests in `tests/integration.rs`.
}
