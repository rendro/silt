//! M:N scheduler mapping lightweight tasks onto a fixed-size thread pool.
//!
//! Spawned tasks run cooperatively on worker threads. Channel operations
//! park tasks instead of blocking OS threads, and wakers re-enqueue
//! them when data arrives.

use parking_lot::{Condvar, Mutex};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Weak;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::value::{IoCompletion, TaskHandle, Value, WakerRegistration};
use crate::vm::{BlockReason, SelectOpKind, Vm, VmError};

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
    live_tasks: AtomicUsize,
    /// Number of tasks currently parked (blocked on channel/join/io).
    blocked_tasks: AtomicUsize,
    /// Number of tasks currently parked on external I/O (a strict subset of
    /// `blocked_tasks`). These do not count toward deadlock detection because
    /// an external waker (I/O pool completion) will unblock them — the
    /// scheduler can't prove the graph is stuck while work is in flight.
    /// Matches Go's philosophy: netpoll-blocked goroutines are not deadlocked.
    io_blocked_tasks: AtomicUsize,
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
    /// between worker dequeue and waker registration is exactly where the
    /// false-positive deadlock fires — the task is no longer in the queue,
    /// no longer in `live > blocked` arithmetic in any visible way, but
    /// nobody has had a chance to register a waker that will eventually
    /// unblock the main thread. Holding `unsettled_tasks > 0` across that
    /// window is the whole point of this counter.
    ///
    /// `can_make_progress` short-circuits on `unsettled_tasks > 0` — any
    /// non-zero value means the scheduler has work that has not yet had a
    /// chance to either complete or register an external wake source, so
    /// declaring deadlock would be premature.
    unsettled_tasks: AtomicUsize,
    /// Reserved for cross-process deadlock state. The worker-side
    /// detector that used to flip this flag was removed because it could
    /// not distinguish "main thread is descheduled" from "main thread is
    /// stuck"; deadlock detection now happens exclusively on the main
    /// thread (see `main_thread_wait_for_*` in
    /// `src/builtins/concurrency.rs`). This flag is currently never
    /// flipped, but is retained so existing accessors / tests do not
    /// have to change shape.
    deadlock_detected: AtomicBool,
    /// Handles of tasks that are currently blocked. When deadlock is detected,
    /// all of these are completed with a deadlock error so joiners unblock.
    blocked_handles: Mutex<Vec<Arc<TaskHandle>>>,
    /// Always-on I/O watchdog registry. Entries are added only when an
    /// I/O block has an effective deadline (from SILT_IO_TIMEOUT or
    /// task.deadline). If neither is in effect for a given block, no
    /// entry is added and the wait is indefinite.
    watchdog: Arc<WatchdogRegistry>,
    /// Global I/O timeout from `SILT_IO_TIMEOUT`. When set, every I/O
    /// block registers with `now + global_io_timeout` as its deadline
    /// unless a tighter task.deadline is in effect.
    global_io_timeout: Option<Duration>,
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
                blocked_tasks: AtomicUsize::new(0),
                io_blocked_tasks: AtomicUsize::new(0),
                unsettled_tasks: AtomicUsize::new(0),
                deadlock_detected: AtomicBool::new(false),
                blocked_handles: Mutex::new(Vec::new()),
                watchdog,
                global_io_timeout,
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

    /// True if a task with `task_id` is currently registered in
    /// `blocked_handles` — i.e. parked on an internal channel/select/join
    /// edge with a waker. Returns `false` if the task is queued, running
    /// on a worker, parked on external I/O, or has already completed.
    ///
    /// Used by `main_thread_wait_for_join` to distinguish "joinee is
    /// progressing" (queued / running) from "joinee is parked itself"
    /// (recursing into the joinee's primitive would be richer information,
    /// but for the watchdog reset heuristic this binary is enough): if
    /// the joinee is NOT blocked, main's join may be moments away from
    /// completing, so reset the deadlock streak.
    pub fn is_handle_blocked(&self, task_id: usize) -> bool {
        self.inner
            .blocked_handles
            .lock()
            .iter()
            .any(|h| h.id == task_id)
    }

    /// Snapshot `(live_tasks, internal_blocked)` used by the main-thread
    /// channel watchdog to decide whether any scheduled task could still
    /// make progress. `internal_blocked` excludes I/O-blocked tasks — an
    /// external waker (I/O pool completion) will eventually unblock them
    /// and they might then reach the channel the main thread is waiting on.
    /// When `live > internal_blocked`, at least one task is either queued,
    /// running, or parked on external I/O; when `live <= internal_blocked`,
    /// no scheduled counterparty can make progress.
    ///
    /// NOTE: prefer [`Self::can_make_progress`] for the deadlock check —
    /// it also consults `unsettled_tasks`, which covers the race between
    /// `submit` (or `requeue` / yield) and the worker actually parking
    /// the task on a wakeable edge that this snapshot misses.
    pub fn progress_snapshot(&self) -> (usize, usize) {
        let blocked = self.inner.blocked_tasks.load(Ordering::SeqCst);
        let io_blocked = self.inner.io_blocked_tasks.load(Ordering::SeqCst);
        (
            self.inner.live_tasks.load(Ordering::SeqCst),
            blocked.saturating_sub(io_blocked),
        )
    }

    /// True if any scheduled task could still unblock the caller. Returns
    /// `true` when either:
    ///   * at least one live task is not parked on an internal graph edge
    ///     (so it is either running, queued, or parked on external I/O), or
    ///   * at least one task is unsettled — it has been enqueued but the
    ///     worker has not yet finished a slice that either completed it
    ///     OR parked it with a registered waker (`unsettled_tasks > 0`).
    ///     This second clause closes the dequeue → register-waker window:
    ///     the main thread's watchdog can otherwise sample the counters
    ///     after a worker has popped the task off the queue but before it
    ///     has registered the send / recv / select / join / I/O waker
    ///     that will eventually unblock the caller. Without this, the
    ///     detector fires a false positive on every fan-in shape that
    ///     races spawn → main-thread receive.
    ///
    /// A `true` return means a legitimate deadlock must not be reported.
    /// A `false` return means every live task is parked on an internal
    /// edge AND no task is mid-settle — the caller should still re-check
    /// after a small delay (TOCTOU windows exist between the three atomic
    /// loads) but may safely treat a steady `false` as a deadlock.
    pub fn can_make_progress(&self) -> bool {
        if self.snapshot_says_progress() {
            return true;
        }
        // First read says no progress. Mirror the worker-side
        // double-check: yield+sleep briefly and recheck. The reason this
        // is needed even with the `unsettled_tasks` rework is that the
        // three-atomic snapshot is not consistent — a sender that
        // unparks during the snapshot can briefly read as "no progress"
        // because the reader saw the post-decrement `unsettled` but
        // the pre-increment `live`/`blocked`. A real deadlock will
        // continue to read false on every retry; a racing-progress
        // reading flips to true within a few iterations. Total
        // worst-case latency for a real deadlock signal is the budget
        // below (~10ms), well below the caller's 100ms watchdog tick.
        const MAX_RECHECKS: usize = 5;
        const RECHECK_SLEEP: Duration = Duration::from_millis(2);
        for _ in 0..MAX_RECHECKS {
            std::thread::yield_now();
            std::thread::sleep(RECHECK_SLEEP);
            if self.snapshot_says_progress() {
                return true;
            }
        }
        false
    }

    /// Single-snapshot progress check. See [`Self::can_make_progress`].
    fn snapshot_says_progress(&self) -> bool {
        let unsettled = self.inner.unsettled_tasks.load(Ordering::SeqCst);
        if unsettled > 0 {
            return true;
        }
        let (live, internal_blocked) = self.progress_snapshot();
        live > 0 && live > internal_blocked
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
        // Order matters: bump unsettled_tasks BEFORE live_tasks so that
        // any observer sees a "definitely progressing" counter throughout
        // the submit window. If live_tasks went up first, a detector
        // racing between the two fetch_adds would briefly see
        // live == blocked + N with unsettled_tasks still zero. Bumping
        // unsettled_tasks first means the detector sees
        // `unsettled_tasks > 0` the moment it sees the increment to live,
        // and `can_make_progress` short-circuits.
        self.inner.unsettled_tasks.fetch_add(1, Ordering::SeqCst);
        self.inner.live_tasks.fetch_add(1, Ordering::SeqCst);
        let mut queue = self.inner.run_queue.lock();
        queue.push_back(task);
        self.inner.condvar.notify_one();
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
            }
            SliceResult::Failed(err) => {
                handle.complete(Err(vm.enrich_error(err)));
                inner.unsettled_tasks.fetch_sub(1, Ordering::SeqCst);
                inner.live_tasks.fetch_sub(1, Ordering::SeqCst);
            }
            SliceResult::Blocked => {
                // Take the block reason from the VM.
                let reason = vm.take_block_reason();
                let task_slot: Arc<Mutex<Option<Task>>> =
                    Arc::new(Mutex::new(Some(Task { id, vm, handle })));

                // Track whether this block is on external I/O. I/O blocks
                // bump `io_blocked_tasks` in addition to `blocked_tasks` so
                // the deadlock detector can exclude them.
                let was_io = matches!(reason, Some(BlockReason::Io(_)));
                // Snapshot whether the block had a reason before the
                // match below moves out of `reason`. Used for the final
                // settle decrement on `unsettled_tasks` after the arm
                // finishes registering wakers.
                let had_block_reason = reason.is_some();

                // Track that this task is now blocked (unless no block reason,
                // which is treated as a yield and re-enqueued immediately).
                if reason.is_some() {
                    inner.blocked_tasks.fetch_add(1, Ordering::SeqCst);
                    if was_io {
                        inner.io_blocked_tasks.fetch_add(1, Ordering::SeqCst);
                    }
                    // Store the handle so we can complete it on deadlock.
                    // SAFETY: task_slot was just created with Some(Task{..}) above.
                    let handle_for_registry = task_slot
                        .lock()
                        .as_ref()
                        .expect("task_slot just initialized")
                        .handle
                        .clone();
                    inner
                        .blocked_handles
                        .lock()
                        .push(handle_for_registry.clone());

                    // Register cancel cleanup: if the task is cancelled while
                    // blocked, take it from the slot (making the waker a no-op),
                    // decrement blocked_tasks, and remove from blocked_handles.
                    let cancel_slot = task_slot.clone();
                    let cancel_inner = inner.clone();
                    let cancel_task_id = id;
                    handle_for_registry.set_cancel_cleanup(Box::new(move || {
                        // Take the task so the waker closure becomes a no-op.
                        // If the slot is empty, the waker already fired and the
                        // task is either running or already accounted for — do
                        // not double-decrement the counters in that case.
                        if cancel_slot.lock().take().is_none() {
                            return;
                        }
                        // This blocked task is being dropped: decrement
                        // blocked_tasks AND live_tasks (the Completed/Failed
                        // path is unreachable once we drop the task, so
                        // live_tasks would otherwise leak).
                        cancel_inner.blocked_tasks.fetch_sub(1, Ordering::SeqCst);
                        if was_io {
                            cancel_inner.io_blocked_tasks.fetch_sub(1, Ordering::SeqCst);
                            cancel_inner.watchdog.remove(cancel_task_id);
                        }
                        cancel_inner.live_tasks.fetch_sub(1, Ordering::SeqCst);
                        let mut handles = cancel_inner.blocked_handles.lock();
                        if let Some(pos) = handles.iter().position(|h| h.id == cancel_task_id) {
                            handles.swap_remove(pos);
                        }
                    }));
                }

                match reason {
                    Some(BlockReason::Receive(ch)) => {
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
                            cancel_inner.blocked_tasks.fetch_sub(1, Ordering::SeqCst);
                            if was_io {
                                cancel_inner.io_blocked_tasks.fetch_sub(1, Ordering::SeqCst);
                                cancel_inner.watchdog.remove(cancel_task_id);
                            }
                            cancel_inner.live_tasks.fetch_sub(1, Ordering::SeqCst);
                            let mut handles = cancel_inner.blocked_handles.lock();
                            if let Some(pos) = handles.iter().position(|h| h.id == cancel_task_id) {
                                handles.swap_remove(pos);
                            }
                        }));
                    }
                    Some(BlockReason::Send(ch)) => {
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
                        // cancel (round-27 B3/B4).
                        let cancel_slot = task_slot.clone();
                        let cancel_inner = inner.clone();
                        let cancel_task_id = id;
                        handle_for_cancel.set_cancel_cleanup(Box::new(move || {
                            let _reg = reg;
                            if cancel_slot.lock().take().is_none() {
                                return;
                            }
                            cancel_inner.blocked_tasks.fetch_sub(1, Ordering::SeqCst);
                            if was_io {
                                cancel_inner.io_blocked_tasks.fetch_sub(1, Ordering::SeqCst);
                                cancel_inner.watchdog.remove(cancel_task_id);
                            }
                            cancel_inner.live_tasks.fetch_sub(1, Ordering::SeqCst);
                            let mut handles = cancel_inner.blocked_handles.lock();
                            if let Some(pos) = handles.iter().position(|h| h.id == cancel_task_id) {
                                handles.swap_remove(pos);
                            }
                        }));
                    }
                    Some(BlockReason::Select(ops)) => {
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
                        // with a select-aware version: same counter
                        // bookkeeping, plus sibling-waker removal via the
                        // guard vec. Select is never I/O, so `was_io` is
                        // false here.
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
                            select_inner.blocked_tasks.fetch_sub(1, Ordering::SeqCst);
                            select_inner.live_tasks.fetch_sub(1, Ordering::SeqCst);
                            let mut handles = select_inner.blocked_handles.lock();
                            if let Some(pos) = handles.iter().position(|h| h.id == select_task_id) {
                                handles.swap_remove(pos);
                            }
                            drop(handles);
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
                        let slot = task_slot.clone();
                        let inner2 = inner.clone();
                        target_handle.register_join_waker(Box::new(move || {
                            if let Some(task) = slot.lock().take() {
                                requeue(&inner2, task, false);
                            }
                        }));
                    }
                    Some(BlockReason::Io(completion)) => {
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

/// Re-enqueue a parked task on the scheduler's run queue.
///
/// `was_io` indicates whether the task was parked on external I/O (so
/// `io_blocked_tasks` needs decrementing and the watchdog registry
/// needs its entry cleared) or on an internal graph edge
/// (channel/select/join). The waker site knows which arm it's in.
fn requeue(inner: &Arc<SchedulerInner>, task: Task, was_io: bool) {
    // Bump unsettled_tasks FIRST so the deadlock detector cannot observe
    // a transient "blocked count briefly equals live" between the
    // `blocked--` below and the actual queue push. The worker that next
    // runs this task will decrement unsettled_tasks again — either when
    // the slice completes the task, when it parks with a registered
    // waker, or when the task yields again (in which case requeue runs
    // a second time and the counter stays balanced via this same
    // increment-then-future-settle dance).
    inner.unsettled_tasks.fetch_add(1, Ordering::SeqCst);
    inner.blocked_tasks.fetch_sub(1, Ordering::SeqCst);
    if was_io {
        inner.io_blocked_tasks.fetch_sub(1, Ordering::SeqCst);
        inner.watchdog.remove(task.id);
    }
    // Remove this task's handle from the blocked registry.
    {
        let task_id = task.id;
        let mut handles = inner.blocked_handles.lock();
        if let Some(pos) = handles.iter().position(|h| h.id == task_id) {
            handles.swap_remove(pos);
        }
    }
    // Clear the stale cancel-cleanup so it won't double-decrement
    // blocked_tasks when the task completes normally.
    task.handle.clear_cancel_cleanup();
    let mut queue = inner.run_queue.lock();
    queue.push_back(task);
    inner.condvar.notify_one();
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
    fn test_progress_snapshot_excludes_io_blocked_tasks() {
        // Unit-level invariant: progress_snapshot returns `(live,
        // internal_blocked)` where internal_blocked = blocked - io_blocked.
        // I/O-blocked tasks have external wakers and must not count toward
        // a deadlock-style "no progress possible" signal.
        let scheduler = Scheduler::new();
        // Simulate: 3 live tasks, 3 blocked total, 2 of which are on I/O.
        scheduler.inner.live_tasks.store(3, Ordering::SeqCst);
        scheduler.inner.blocked_tasks.store(3, Ordering::SeqCst);
        scheduler.inner.io_blocked_tasks.store(2, Ordering::SeqCst);
        let (live, internal_blocked) = scheduler.progress_snapshot();
        assert_eq!(live, 3);
        assert_eq!(
            internal_blocked, 1,
            "internal_blocked should exclude I/O tasks: 3 - 2 = 1"
        );
        // Sanity: if io_blocked == blocked, internal_blocked should be 0.
        scheduler.inner.io_blocked_tasks.store(3, Ordering::SeqCst);
        let (_, internal_blocked) = scheduler.progress_snapshot();
        assert_eq!(internal_blocked, 0);
        // Reset so the Drop path doesn't see non-zero counters.
        scheduler.inner.live_tasks.store(0, Ordering::SeqCst);
        scheduler.inner.blocked_tasks.store(0, Ordering::SeqCst);
        scheduler.inner.io_blocked_tasks.store(0, Ordering::SeqCst);
    }

    #[test]
    fn test_io_blocked_task_does_not_trigger_deadlock() {
        // Regression lock for the channel-only deadlock split: a task that
        // performs real I/O should complete successfully, and the scheduler
        // must NOT have marked the session as deadlocked during the I/O
        // park window. Tests the I/O path end-to-end through the counter
        // bookkeeping changes in requeue/cancel_cleanup.
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
