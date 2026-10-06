//! The scheduler: tasks on a pool of worker threads, and the threads
//! that run silt code of their own.
//!
//! Every piece of running silt code is a task to the scheduler, whoever
//! drives it:
//!
//! - a task made by `task.spawn` is run slice by slice by the workers;
//! - the program itself (`fn main`, a test, a REPL entry) and the
//!   callbacks of a stream stage or an HTTP handler are run by their
//!   own thread (`Scheduler::enter`).
//!
//! Each of them waits in the same place, the registry of parked tasks
//! ([`Parking`]): a spawned task is taken off its worker and put back
//! on the run queue when its wait ends; a thread waits where it is
//! (`Scheduler::block_thread`) and, where there are no workers, runs
//! the queued tasks while it does.
//!
//! # Deadlock
//!
//! The program is deadlocked when every task waits and nothing outside
//! can end a wait: no timer is pending and no I/O operation is in
//! flight (`Scheduler::external`). That is checked when a task parks,
//! when one ends, and when something external ends; it is exact, so it
//! is reported at once. The program's own thread gets the error.

use parking_lot::{Condvar, Mutex};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::Weak;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use crate::runtime::channel::{Channel, Waker, WakerRegistration};
use crate::runtime::completion::{IoCompletion, IoWakerRegistration};
use crate::runtime::handle::{JoinWakerRegistration, TaskHandle};
use crate::runtime::sync::{self, Arm, Parking, Resumed, TaskId, Wait};
use crate::value::Value;
use crate::vm::{BlockReason, HostIo, SelectOpKind, Vm, VmError};

#[doc(hidden)]
pub mod test_support;

/// Maximum number of live (active + blocked + queued) tasks the scheduler allows.
const MAX_TASKS: usize = 100_000;

/// Stack size of every scheduler worker thread, in bytes.
///
/// A silt call takes a frame of the VM and no native stack, however it
/// is made. What still recurses on the worker's native stack is the
/// work on a value that is nested deeply (comparing, printing or
/// dropping a list of lists of lists ...): a program can build one as
/// deep as it can recurse, and the platform default for a spawned
/// thread (2 MiB) overflows on values the main thread handles. A
/// native stack overflow aborts the whole process.
///
/// On 64-bit targets the value equals the main thread's reserve
/// (`SILT_STACK_SIZE` in `src/main.rs`, 256 MiB), so a program can
/// work on the same values inside `task.spawn` as outside of it. The
/// size is a reservation of address space: memory is committed page by
/// page as the stack grows, so a worker that never recurses deeply
/// costs what it cost before. There are `max(cores, 2)` workers; on a
/// 64-core machine they reserve 16 GiB of a 128 TiB address space.
///
/// On 32-bit targets the address space is the limit (2 to 4 GiB for
/// everything), so workers get 16 MiB there: 8 times the default, and
/// 64 workers still fit.
pub const WORKER_STACK_BYTES: usize = if cfg!(target_pointer_width = "64") {
    256 * 1024 * 1024
} else {
    16 * 1024 * 1024
};

/// Upper bound on the failed tasks kept for the report of failures that
/// nobody joined. A long-running program whose tasks keep failing must
/// not grow without bound; what goes beyond the bound is counted.
const MAX_RECORDED_FAILURES: usize = 64;

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
    task: TaskId,
    completion: Weak<IoCompletion>,
    /// A reading of the host clock.
    deadline: Duration,
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
    /// Whether its thread runs.
    started: Mutex<bool>,
}

impl WatchdogRegistry {
    fn new(interval: Duration) -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            interval,
            shutdown: AtomicBool::new(false),
            started: Mutex::new(false),
        }
    }

    fn add(
        &self,
        task: TaskId,
        completion: &Arc<IoCompletion>,
        deadline: Duration,
        source: DeadlineSource,
    ) {
        self.entries.lock().push(WatchdogEntry {
            task,
            completion: Arc::downgrade(completion),
            deadline,
            source,
        });
    }

    fn remove(&self, task: TaskId) {
        let mut entries = self.entries.lock();
        if let Some(pos) = entries.iter().position(|e| e.task == task) {
            entries.swap_remove(pos);
        }
    }

    /// Scan the registry for entries overdue at `now`, a reading of the
    /// host clock. For each one, fire an
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
    fn scan_and_fire(&self, now: Duration) -> usize {
        let to_fire: Vec<(Weak<IoCompletion>, &'static str)> = {
            let mut entries = self.entries.lock();
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
                // Phases 1-3 of the stdlib error redesign: each blocking
                // builtin registers its own `timeout_err` factory on the
                // `IoCompletion` so a deadline-cancelled op surfaces the
                // typed variant the caller's signature declares (e.g.
                // `Err(TcpTimeout)` for tcp.*, `Err(HttpTimeout)` for
                // http.*, `Err(IoUnknown(msg))` for io/fs). The watchdog
                // just calls the factory and stuffs the result into the
                // completion — it remains module-agnostic.
                let err_value = completion.build_timeout_err(msg);
                if completion.complete(err_value) {
                    fired += 1;
                }
            }
        }
        fired
    }
}

/// Watchdog worker loop. Wakes every `interval` (of real time), scans
/// registry, fires timeouts on the entries overdue on the host clock.
/// Exits cleanly on shutdown signal.
fn watchdog_loop(registry: Arc<WatchdogRegistry>, io: HostIo) {
    while !registry.shutdown.load(Ordering::SeqCst) {
        thread::sleep(registry.interval);
        if registry.shutdown.load(Ordering::SeqCst) {
            return;
        }
        // If the clock has panicked, every watched wait ends now: the
        // task runs into the clock's failure at its next builtin call.
        let now = io.monotonic();
        let now = if io.clock_failure().is_some() {
            Duration::MAX
        } else {
            now
        };
        registry.scan_and_fire(now);
    }
}

/// Result of running a task's VM for one time slice.
pub(crate) enum SliceResult {
    /// Time slice expired; task is still runnable.
    Yielded,
    /// Task completed with a value.
    Completed(Value),
    /// Task failed with an error.
    Failed(VmError),
    /// The task waits for this.
    Blocked(BlockReason),
}

/// A lightweight task scheduled on the M:N thread pool.
pub struct Task {
    pub id: usize,
    pub vm: Vm,
    pub handle: Arc<TaskHandle>,
}

/// The scheduler of one program.
pub struct Scheduler {
    inner: Arc<Inner>,
    /// The worker threads and the watchdog, started by the first task.
    workers: Mutex<Option<Vec<thread::JoinHandle<()>>>>,
}

/// Who waits in the registry of parked tasks, with what wakes it.
struct Sleeper {
    who: Who,
    /// Dropped when the wait ends, however it ends.
    bridge: Bridge,
}

// Nearly every sleeper is a task.
#[allow(clippy::large_enum_variant)]
enum Who {
    /// A spawned task: it goes back on the run queue.
    Task(Task),
    /// A thread that runs silt code of its own: it is told.
    Thread(Arc<ThreadPark>),
}

/// Where a thread that waits finds how its wait ended.
#[derive(Default)]
struct ThreadPark {
    resumed: Mutex<Option<Resumed>>,
}

/// The wait of the program's own thread, for the deadlock report.
#[derive(Clone)]
struct MainWait {
    id: TaskId,
    /// `channel receive with no counterparty`.
    what: &'static str,
}

struct Inner {
    queue: Mutex<VecDeque<Task>>,
    /// The workers wait here for a task.
    work: Condvar,
    /// The threads that wait in [`Scheduler::block_thread`] wait here,
    /// with the queue's lock.
    threads: Condvar,
    shutdown: AtomicBool,
    /// Whether worker threads run the queue.
    has_workers: AtomicBool,
    /// The tasks that have not ended: spawned tasks, and threads that
    /// are running silt code.
    live: AtomicUsize,
    /// The spawned tasks among them, for `MAX_TASKS`.
    spawned: AtomicUsize,
    /// What can end a wait from outside the tasks: timers that are
    /// pending, I/O operations in flight. See [`External`].
    external: AtomicUsize,
    parking: Parking<Sleeper>,
    /// How many parked tasks have a wait that a stream stage can end,
    /// whose thread the scheduler does not count: a receive on the
    /// stage's output, or a send on its input. While there is one, no
    /// verdict is given.
    stream_waits: AtomicUsize,
    main_wait: Mutex<Option<MainWait>>,
    /// The verdict, between the check that gave it and the program's
    /// thread that raises it.
    deadlock: Mutex<Option<VmError>>,
    /// The names of the waits of threads.
    next_thread_wait: AtomicU64,
    /// Whether the thread that runs the program knows this scheduler
    /// (`StartedSchedulers`).
    registered: AtomicBool,
    /// Always-on I/O watchdog registry. Entries are added only when an
    /// I/O block has an effective deadline (from SILT_IO_TIMEOUT or
    /// task.deadline). If neither is in effect for a given block, no
    /// entry is added and the wait is indefinite.
    watchdog: Arc<WatchdogRegistry>,
    /// Global I/O timeout from `SILT_IO_TIMEOUT`. When set, every I/O
    /// block registers with `now + global_io_timeout` as its deadline
    /// unless a tighter task.deadline is in effect.
    global_io_timeout: Option<Duration>,
    /// Tasks that ended with an error, kept for the report of failures
    /// that nobody joined. See `report_unjoined_failures`.
    failed_tasks: Mutex<FailedTasks>,
    /// The host's clock and stderr.
    io: HostIo,
}

/// The wakers a parked task has on the channels, handles and
/// completions of its wait. Each completes the cell the task is parked
/// on; the builtin that parked then runs again and takes what it
/// finds.
struct Bridge {
    inner: Arc<Inner>,
    registrations: Vec<Registration>,
    channels: Vec<Arc<Channel>>,
    /// Counted in `Inner::stream_waits`.
    stream: bool,
    /// The task's entry in the I/O watchdog.
    watched: Option<TaskId>,
}

#[allow(dead_code)] // held for their `Drop`
enum Registration {
    Channel(WakerRegistration),
    Join(JoinWakerRegistration),
    Io(IoWakerRegistration),
}

impl Bridge {
    /// The wait ended by a wake: the builtin runs again and takes what
    /// it was woken for, so nothing is passed on.
    fn woken(mut self) {
        self.channels.clear();
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.registrations.clear();
        if self.stream {
            self.inner.stream_waits.fetch_sub(1, Ordering::SeqCst);
        }
        if let Some(task) = self.watched {
            self.inner.watchdog.remove(task);
        }
        // A channel wakes one waiter for each value and each free
        // place. A task that was cancelled may have been woken for one
        // it does not take: the next waiter is woken for it.
        for channel in &self.channels {
            channel.rewake_waiters();
        }
    }
}

/// Something outside the tasks that can end a wait, for as long as it
/// lives: a pending timer, an I/O operation in flight, a thread that
/// serves the program. While one exists the program is not deadlocked.
/// It is dropped after the last thing it does for a task has returned.
pub(crate) struct External(Arc<Inner>);

impl Drop for External {
    fn drop(&mut self) {
        self.0.external.fetch_sub(1, Ordering::SeqCst);
        self.0.check_stuck();
    }
}

/// A thread that is running silt code of its own, from
/// [`Scheduler::enter`] until it is dropped.
pub(crate) struct Running(Arc<Inner>);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.live.fetch_sub(1, Ordering::SeqCst);
        self.0.check_stuck();
    }
}

impl Scheduler {
    /// Create a new scheduler (does NOT start worker threads yet). Its
    /// deadlines are read on the clock of `io`, and its reports go to
    /// the stderr of `io`.
    pub fn new(io: HostIo) -> Self {
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
        let timer = sync::Timer::new(io.clone());
        let inner = Arc::new_cyclic(|inner: &Weak<Inner>| {
            let ready = inner.clone();
            Inner {
                queue: Mutex::new(VecDeque::new()),
                work: Condvar::new(),
                threads: Condvar::new(),
                shutdown: AtomicBool::new(false),
                has_workers: AtomicBool::new(false),
                live: AtomicUsize::new(0),
                spawned: AtomicUsize::new(0),
                external: AtomicUsize::new(0),
                parking: Parking::new(timer, move |sleeper, resumed| {
                    // Without a scheduler the task is dropped.
                    if let Some(inner) = ready.upgrade() {
                        inner.ready(sleeper, resumed);
                    }
                }),
                stream_waits: AtomicUsize::new(0),
                main_wait: Mutex::new(None),
                deadlock: Mutex::new(None),
                next_thread_wait: AtomicU64::new(0),
                registered: AtomicBool::new(false),
                watchdog,
                global_io_timeout,
                failed_tasks: Mutex::new(FailedTasks::default()),
                io,
            }
        });
        Scheduler {
            inner,
            workers: Mutex::new(None),
        }
    }

    /// Ensure worker threads are running.
    ///
    /// Returns an error if not a single worker thread could be started.
    /// If some but not all could be started, the scheduler runs with
    /// those.
    fn ensure_workers(&self) -> Result<(), String> {
        let mut guard = self.workers.lock();
        if guard.is_some() {
            return Ok(());
        }

        let num_workers = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .max(2); // At least 2 workers to avoid deadlocks

        let mut handles = Vec::with_capacity(num_workers);
        // Workers reserve `WORKER_STACK_BYTES`. Where the system refuses
        // a reservation that large (an address-space limit, strict
        // overcommit), the first worker falls back to the default stack,
        // and the rest follow it; the program runs.
        let mut stack_bytes = Some(WORKER_STACK_BYTES);
        while handles.len() < num_workers {
            let inner = self.inner.clone();
            let mut builder = thread::Builder::new();
            if let Some(bytes) = stack_bytes {
                builder = builder.stack_size(bytes);
            }
            let spawned = builder.spawn(move || worker_loop(inner));
            match spawned {
                Ok(handle) => handles.push(handle),
                Err(_) if handles.is_empty() && stack_bytes.is_some() => stack_bytes = None,
                Err(e) if handles.is_empty() => {
                    return Err(format!("cannot start a scheduler worker thread: {e}"));
                }
                // The workers started so far run the tasks.
                Err(_) => break,
            }
        }
        self.inner.has_workers.store(true, Ordering::SeqCst);
        *guard = Some(handles);
        drop(guard);
        self.ensure_watchdog();
        Ok(())
    }

    /// Start the thread that ends the I/O waits whose deadline has
    /// passed, if it does not run yet.
    fn ensure_watchdog(&self) {
        static NAME: &str = "silt-io-watchdog";
        let mut started = self.inner.watchdog.started.lock();
        if *started || self.inner.shutdown.load(Ordering::SeqCst) {
            return;
        }
        let registry = self.inner.watchdog.clone();
        let io = self.inner.io.clone();
        // Without the thread a deadline does not end an I/O wait; the
        // operation still ends by itself.
        *started = thread::Builder::new()
            .name(NAME.into())
            .spawn(move || watchdog_loop(registry, io))
            .is_ok();
    }

    /// Report on the host's stderr every task of this scheduler that failed and
    /// whose error no `task.join` has received and no `task.cancel` has
    /// dismissed. Each failure is reported once, so the call can be
    /// repeated. Returns the number of failures that this call reported.
    ///
    /// It runs by itself when the scheduler is dropped and when the
    /// thread that spawned the first task ends. A caller that ends the
    /// process in another way (`std::process::exit`) calls it before
    /// that.
    ///
    /// While the failures are collected (`collect_unjoined_failures`),
    /// the scheduler keeps none, and this reports nothing: the front
    /// end takes them with `take_unjoined_failures` and reports them.
    pub fn report_unjoined_failures(&self) -> usize {
        report_unjoined_failures(&self.inner)
    }

    /// Submit a runnable task to the scheduler.
    ///
    /// Returns an error if the live-task count has reached the
    /// scheduler's hard task limit.
    pub fn submit(&self, task: Task) -> Result<(), String> {
        if self.inner.shutdown.load(Ordering::SeqCst) {
            return Err("cannot spawn a task: the VM that ran the program has been dropped".into());
        }
        // A target without threads runs the tasks on the threads that
        // wait ([`Scheduler::block_thread`]).
        #[cfg(not(target_arch = "wasm32"))]
        self.ensure_workers()?;
        let current = self.inner.spawned.load(Ordering::SeqCst);
        if current >= MAX_TASKS {
            return Err(format!(
                "task limit exceeded: {} tasks running (max {MAX_TASKS})",
                current
            ));
        }
        // The thread that spawns the first task is the one that runs
        // the program. When it ends, the program has ended: see
        // `StartedSchedulers`.
        if !self.inner.registered.swap(true, Ordering::SeqCst) {
            let _ = STARTED_HERE.try_with(|started| {
                started
                    .schedulers
                    .borrow_mut()
                    .push(Arc::downgrade(&self.inner));
            });
        }
        // A cancel of the task ends its wait, if it waits.
        let inner = Arc::downgrade(&self.inner);
        let id = TaskId(task.id as u64);
        task.handle.set_cancel_cleanup(Box::new(move || {
            if let Some(inner) = inner.upgrade() {
                inner.parking.cancel(id);
            }
        }));
        self.inner.spawned.fetch_add(1, Ordering::SeqCst);
        self.inner.live.fetch_add(1, Ordering::SeqCst);
        self.inner.enqueue(task);
        Ok(())
    }

    /// The calling thread starts to run silt code of its own: it counts
    /// as a task until the guard is dropped.
    pub(crate) fn enter(&self) -> Running {
        self.inner.live.fetch_add(1, Ordering::SeqCst);
        Running(self.inner.clone())
    }

    /// See [`External`].
    pub(crate) fn external(&self) -> External {
        self.inner.external.fetch_add(1, Ordering::SeqCst);
        External(self.inner.clone())
    }

    /// The calling thread, which runs silt code of its own
    /// ([`Scheduler::enter`]), waits for `reason`: it returns when the
    /// builtin that parked is to run again. `deadline` is the task
    /// deadline in effect. `main` says that the thread is the
    /// program's: it gets the error when the program is deadlocked.
    pub(crate) fn block_thread(
        &self,
        reason: BlockReason,
        deadline: Option<Duration>,
        main: bool,
    ) -> Result<(), VmError> {
        let inner = &self.inner;
        if matches!(reason, BlockReason::Io(_)) {
            self.ensure_watchdog();
        }
        // The names of spawned tasks count up from 0.
        let id = TaskId(u64::MAX - inner.next_thread_wait.fetch_add(1, Ordering::Relaxed));
        if main {
            let what = match &reason {
                BlockReason::Receive(_) => "channel receive with no counterparty",
                BlockReason::Send(_) => "channel send with no counterparty",
                BlockReason::Select(_) => "channel select with no counterparty",
                BlockReason::Join(_) | BlockReason::Io(_) => "task.join with no progress possible",
            };
            *inner.main_wait.lock() = Some(MainWait { id, what });
        }
        let (bridge, wait) = inner.bridge(id, &reason, deadline);
        let park = Arc::new(ThreadPark::default());
        let sleeper = Sleeper {
            who: Who::Thread(park.clone()),
            bridge,
        };
        let resumed = match inner.parking.park(id, sleeper, wait, || false) {
            Some((sleeper, resumed)) => {
                if let Resumed::Fired(_) = resumed {
                    sleeper.bridge.woken();
                }
                resumed
            }
            None => {
                inner.check_stuck();
                inner.wait_thread(&park)
            }
        };
        if main {
            *inner.main_wait.lock() = None;
        }
        // The verdict is the program's thread's.
        let verdict = if main {
            inner.deadlock.lock().take()
        } else {
            None
        };
        match resumed {
            Resumed::Fired(_) => Ok(()),
            Resumed::Cancelled => Err(match verdict {
                Some(verdict) => {
                    // A task that failed is the usual reason why the
                    // counterparty of the wait is missing: it is
                    // reported before the verdict.
                    let _ = report_unjoined_failures(inner);
                    verdict
                }
                None => VmError::new("the VM that ran the program has been dropped".into()),
            }),
        }
    }
}

impl Inner {
    /// Put a task on the run queue.
    fn enqueue(&self, task: Task) {
        let mut queue = self.queue.lock();
        if self.shutdown.load(Ordering::SeqCst) {
            // It never runs: dropped with the queue unlocked.
            drop(queue);
            self.end_task(task);
            return;
        }
        queue.push_back(task);
        if self.has_workers.load(Ordering::SeqCst) {
            self.work.notify_one();
        } else {
            self.threads.notify_all();
        }
    }

    /// A task has ended: it completed, failed, or was cancelled.
    fn end_task(&self, task: Task) {
        // Its frames are abandoned before it stops counting.
        drop(task);
        self.spawned.fetch_sub(1, Ordering::SeqCst);
        self.live.fetch_sub(1, Ordering::SeqCst);
        self.check_stuck();
    }

    /// The wait of `sleeper` has ended.
    fn ready(&self, sleeper: Sleeper, resumed: Resumed) {
        let Sleeper { who, bridge } = sleeper;
        match resumed {
            Resumed::Fired(_) => bridge.woken(),
            Resumed::Cancelled => drop(bridge),
        }
        match who {
            Who::Task(task) => match resumed {
                Resumed::Fired(_) => self.enqueue(task),
                Resumed::Cancelled => self.end_task(task),
            },
            Who::Thread(park) => {
                *park.resumed.lock() = Some(resumed);
                // With the queue's lock, the thread is either before
                // its look at `resumed` or waiting.
                let _queue = self.queue.lock();
                self.threads.notify_all();
            }
        }
    }

    /// Register what wakes a task that waits for `reason`, and give
    /// the wait that those wakers end.
    fn bridge(
        self: &Arc<Self>,
        id: TaskId,
        reason: &BlockReason,
        task_deadline: Option<Duration>,
    ) -> (Bridge, Wait) {
        let cell = sync::Cell::<()>::new();
        let waker = || -> Waker {
            let (cell, inner) = (cell.clone(), self.clone());
            Box::new(move || {
                let _ = cell.complete((), &inner.parking);
            })
        };
        let mut bridge = Bridge {
            inner: self.clone(),
            registrations: Vec::new(),
            channels: Vec::new(),
            stream: false,
            watched: None,
        };
        let stream = Cell::new(false);
        let receive = |bridge: &mut Bridge, channel: &Arc<Channel>| {
            if crate::builtins::concurrency::is_stream_fed(channel) {
                stream.set(true);
            }
            let registration = channel.register_recv_waker_guard(waker());
            bridge
                .registrations
                .push(Registration::Channel(registration));
            bridge.channels.push(channel.clone());
        };
        let send = |bridge: &mut Bridge, channel: &Arc<Channel>| {
            if channel.is_read_by_stream() {
                stream.set(true);
            }
            let registration = channel.register_send_waker_guard(waker());
            bridge
                .registrations
                .push(Registration::Channel(registration));
            bridge.channels.push(channel.clone());
        };
        match reason {
            BlockReason::Receive(channel) => receive(&mut bridge, channel),
            BlockReason::Send(channel) => send(&mut bridge, channel),
            BlockReason::Select(ops) => {
                for (channel, kind) in ops {
                    match kind {
                        SelectOpKind::Receive => receive(&mut bridge, channel),
                        SelectOpKind::Send => send(&mut bridge, channel),
                    }
                }
            }
            BlockReason::Join(handle) => {
                let registration = handle.register_join_waker_guard(waker());
                bridge.registrations.push(Registration::Join(registration));
            }
            BlockReason::Io(completion) => {
                // The I/O wait ends at the earlier of the global and
                // the task's deadline.
                let now = self.io.monotonic();
                let global = self
                    .global_io_timeout
                    .and_then(|t| now.checked_add(t))
                    .map(|d| (d, DeadlineSource::Global));
                let task = task_deadline.map(|d| (d, DeadlineSource::Task));
                let effective = match (global, task) {
                    (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
                    (Some(x), None) | (None, Some(x)) => Some(x),
                    (None, None) => None,
                };
                if let Some((deadline, source)) = effective {
                    self.watchdog.add(id, completion, deadline, source);
                    bridge.watched = Some(id);
                }
                let registration = completion.register_waker_guard(waker());
                bridge.registrations.push(Registration::Io(registration));
            }
        }
        if stream.get() {
            bridge.stream = true;
            self.stream_waits.fetch_add(1, Ordering::SeqCst);
        }
        (bridge, Wait::new(vec![Arm::Cell(cell)]))
    }

    /// The calling thread waits until its wait has ended. Where no
    /// worker runs the queue, it does.
    fn wait_thread(self: &Arc<Self>, park: &ThreadPark) -> Resumed {
        let mut queue = self.queue.lock();
        loop {
            if let Some(resumed) = park.resumed.lock().take() {
                return resumed;
            }
            if !self.has_workers.load(Ordering::SeqCst)
                && let Some(task) = queue.pop_front()
            {
                drop(queue);
                self.run_slice(task);
                queue = self.queue.lock();
                continue;
            }
            self.threads.wait(&mut queue);
        }
    }

    /// Run a task for one slice, and do what its end asks for.
    fn run_slice(self: &Arc<Self>, task: Task) {
        let Task { id, mut vm, handle } = task;
        // A task that was cancelled runs no further.
        if handle.is_finished() {
            self.end_task(Task { id, vm, handle });
            return;
        }
        // The tasks that this slice spawns belong to the owner of this
        // task. See `set_task_owner`.
        let outer = RUNNING_TASK_OWNER.with(|owner| owner.replace(Some(handle.owner())));
        let result = vm.execute_slice(time_slice());
        RUNNING_TASK_OWNER.with(|owner| owner.set(outer));

        match result {
            SliceResult::Yielded => self.enqueue(Task { id, vm, handle }),
            SliceResult::Completed(value) => {
                handle.complete(Ok(value));
                self.end_task(Task { id, vm, handle });
            }
            SliceResult::Failed(error) => {
                // A failure that no join receives is reported when the
                // program ends, so it is recorded here. `fail` returns
                // false if the task had been cancelled before: its
                // handle keeps the cancellation as its result then.
                if handle.fail(vm.enrich_error(error)) {
                    record_failed_task(self, &handle);
                }
                self.end_task(Task { id, vm, handle });
            }
            SliceResult::Blocked(reason) => {
                let pid = TaskId(id as u64);
                let (bridge, wait) = self.bridge(pid, &reason, vm.current_deadline);
                let cancelled = handle.clone();
                let sleeper = Sleeper {
                    who: Who::Task(Task { id, vm, handle }),
                    bridge,
                };
                let back = self
                    .parking
                    .park(pid, sleeper, wait, || cancelled.is_finished());
                match back {
                    Some((sleeper, resumed)) => self.ready(sleeper, resumed),
                    None => self.check_stuck(),
                }
            }
        }
    }

    /// Give the verdict if the program is deadlocked: its own thread
    /// waits, and so does every other task, with nothing pending
    /// outside.
    fn check_stuck(&self) {
        if self.external.load(Ordering::SeqCst) > 0 || self.stream_waits.load(Ordering::SeqCst) > 0
        {
            return;
        }
        // Without the program's thread waiting there is nobody to tell.
        let Some(main) = self.main_wait.lock().clone() else {
            return;
        };
        let stuck = self.parking.stuck(
            || self.live.load(Ordering::SeqCst),
            || self.external.load(Ordering::SeqCst) + self.stream_waits.load(Ordering::SeqCst),
        );
        let Some(stuck) = stuck else {
            return;
        };
        if !stuck.iter().any(|stuck| stuck.task == main.id) {
            return;
        }
        *self.deadlock.lock() = Some(VmError::new(format!(
            "deadlock on main thread: {}",
            main.what
        )));
        self.parking.cancel(main.id);
    }
}

/// How many steps a task runs before it gives way (`SILT_TIME_SLICE`).
fn time_slice() -> usize {
    static SLICE: OnceLock<usize> = OnceLock::new();
    *SLICE.get_or_init(|| {
        std::env::var("SILT_TIME_SLICE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2000)
            .max(1)
    })
}

impl Scheduler {
    /// End the scheduler with the program that used it: report the
    /// failures that nobody joined (nobody can join a task of it any
    /// more), tell the workers and the watchdog to end, and drop the
    /// tasks that wait for a worker or for anything else. A task
    /// submitted later is refused.
    ///
    /// The threads are not waited for: each ends when the slice it is
    /// running returns.
    pub(crate) fn shutdown(&self) {
        let inner = &self.inner;
        let _ = report_unjoined_failures(inner);
        inner.shutdown.store(true, Ordering::SeqCst);
        inner.watchdog.shutdown.store(true, Ordering::SeqCst);
        // Detach the workers.
        drop(self.workers.lock().take());
        // The tasks are dropped after the queue's lock is released.
        let waiting: Vec<Task> = inner.queue.lock().drain(..).collect();
        inner.work.notify_all();
        for task in waiting {
            inner.end_task(task);
        }
        // The parked tasks never run again either: each comes off
        // every queue, and its frames are abandoned. A thread that
        // waits is told that the program is gone.
        for (_, sleeper) in inner.parking.shutdown() {
            inner.ready(sleeper, Resumed::Cancelled);
        }
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        // Reached with its workers still attached only when no VM was
        // dropped first (`shutdown`).
        let workers = self.workers.lock().take();
        self.shutdown();
        if let Some(workers) = workers {
            // A worker may run a task whose end drops the last
            // reference to the runtime, and with it this scheduler: a
            // thread cannot join itself.
            let me = thread::current().id();
            for w in workers {
                if w.thread().id() == me {
                    std::mem::forget(w);
                } else {
                    let _ = w.join();
                }
            }
        }
    }
}

/// A worker: take a task off the queue, run it for a slice, again.
fn worker_loop(inner: Arc<Inner>) {
    loop {
        let task = {
            let mut queue = inner.queue.lock();
            loop {
                if inner.shutdown.load(Ordering::SeqCst) {
                    return;
                }
                if let Some(task) = queue.pop_front() {
                    break task;
                }
                inner.work.wait(&mut queue);
            }
        };
        inner.run_slice(task);
    }
}

/// Failed tasks kept for the report of failures that nobody joined, up
/// to `MAX_RECORDED_FAILURES`. Each scheduler has one; while the
/// failures are collected (`collect_unjoined_failures`), the one of the
/// process is used instead.
#[derive(Default)]
struct FailedTasks {
    /// Handles of tasks that ended with an error. Whether a join has
    /// received the error since, or a cancel has dismissed it, is read
    /// from the handle at report time.
    handles: Vec<Arc<TaskHandle>>,
    /// Per owner tag, the number of failed tasks that were not recorded
    /// because `handles` was full of failures that nobody had joined.
    not_recorded: BTreeMap<u64, usize>,
}

impl FailedTasks {
    /// Keep the handle of a task that ended with an error. Returns the
    /// handles that were let go to make room (failures that have been
    /// joined or cancelled since they were kept); the caller drops them
    /// after it has released the lock on the record.
    fn record(&mut self, handle: &Arc<TaskHandle>) -> Vec<Arc<TaskHandle>> {
        let mut handled_since = Vec::new();
        if self.handles.len() >= MAX_RECORDED_FAILURES {
            let (unhandled, handled): (Vec<_>, Vec<_>) = std::mem::take(&mut self.handles)
                .into_iter()
                .partition(|h| h.has_unjoined_failure());
            self.handles = unhandled;
            handled_since = handled;
        }
        if self.handles.len() >= MAX_RECORDED_FAILURES {
            *self.not_recorded.entry(handle.owner()).or_insert(0) += 1;
        } else {
            self.handles.push(handle.clone());
        }
        handled_since
    }

    /// Empty the record, and return what it held.
    fn take(&mut self) -> (Vec<Arc<TaskHandle>>, BTreeMap<u64, usize>) {
        (
            std::mem::take(&mut self.handles),
            std::mem::take(&mut self.not_recorded),
        )
    }
}

/// Keep the handle of a task that ended with an error, for the report
/// of failures that nobody joined: in the record of the process while
/// the failures are collected, in the scheduler's own record otherwise.
fn record_failed_task(inner: &Inner, handle: &Arc<TaskHandle>) {
    let handled_since = if COLLECTING.load(Ordering::SeqCst) {
        collected_failures().lock().record(handle)
    } else {
        inner.failed_tasks.lock().record(handle)
    };
    // Dropped after the lock has been released.
    drop(handled_since);
}

/// Report on stderr every recorded task of this scheduler that failed,
/// whose error no join has received and that no cancel has dismissed.
/// A failure is reported once: the record is emptied and each handle
/// gives its error out once. Returns the number of failures that this
/// call reported, those that were counted but not kept included.
///
/// The report has no file name and no source line: the scheduler knows
/// neither, so it renders through a source map with no files. A front end
/// that knows the program's files collects the failures instead
/// (`collect_unjoined_failures`) and renders them itself; the record of
/// the scheduler stays empty then.
fn report_unjoined_failures(inner: &Inner) -> usize {
    let (handles, not_recorded) = inner.failed_tasks.lock().take();
    // The scheduler knows no files: the diagnostics show no place.
    let no_files = crate::source::SourceMap::new();
    let mut reported = 0;
    let mut report = String::new();
    for handle in &handles {
        let Some(error) = handle.take_unjoined_failure() else {
            continue;
        };
        reported += 1;
        let failure = UnjoinedFailure {
            task_id: handle.id,
            owner: 0,
            error,
        };
        let d = failure.report_error().to_diagnostic();
        report.push_str(&crate::diagnostic::render_human(&no_files, &d));
        report.push('\n');
    }
    let not_recorded: usize = not_recorded.values().sum();
    if not_recorded > 0 {
        reported += not_recorded;
        let d = crate::diagnostic::Diagnostic::error(
            crate::diagnostic::Code::UnjoinedTaskFailure,
            crate::source::Span::BUILTIN,
            UnjoinedFailures::not_kept_message(not_recorded),
        );
        report.push_str(&crate::diagnostic::render_human(&no_files, &d));
        report.push('\n');
    }
    if !report.is_empty() {
        inner.io.err(&report);
    }
    reported
}

/// The schedulers whose workers a thread has started.
///
/// The thread that spawns the first task of a program is the thread
/// that runs the program. When that thread ends, the program has ended,
/// and the failures that nobody joined are reported. Tasks that are
/// still parked keep the scheduler itself alive, so its `Drop` alone
/// would not be reached in a program that leaves a task behind.
///
/// This is a thread-local with a destructor. It runs when the thread
/// returns. Whether it runs when the thread calls
/// `std::process::exit` depends on the platform; a caller that ends
/// the process that way calls `Scheduler::report_unjoined_failures`
/// first.
struct StartedSchedulers {
    schedulers: RefCell<Vec<Weak<Inner>>>,
}

impl Drop for StartedSchedulers {
    fn drop(&mut self) {
        for scheduler in self.schedulers.get_mut().drain(..) {
            if let Some(inner) = scheduler.upgrade() {
                let _ = report_unjoined_failures(&inner);
            }
        }
    }
}

thread_local! {
    static STARTED_HERE: StartedSchedulers = const {
        StartedSchedulers {
            schedulers: RefCell::new(Vec::new()),
        }
    };
}

// ── Who spawned a task, and the failures that nobody joined ─────────

/// The owner tag of the tasks spawned outside any task. See
/// `set_task_owner`.
static PROGRAM_TASK_OWNER: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// On a worker thread, while it runs a slice of a task: the owner
    /// tag of that task.
    static RUNNING_TASK_OWNER: Cell<Option<u64>> = const { Cell::new(None) };
}

/// Set the owner tag of the tasks spawned from now on outside any task:
/// by the thread that runs the program, or by a thread that runs a
/// stream stage or an HTTP handler. A task spawned by a task gets the
/// owner of the task that spawns it, so one tag covers every task that
/// descends from the tasks spawned under it.
///
/// A report of a task's failure carries the owner tag
/// (`UnjoinedFailure::owner`). `silt test` sets one tag per test, and
/// so charges the failure of a task to the test that spawned it. 0, the
/// default, means no owner.
pub fn set_task_owner(owner: u64) {
    PROGRAM_TASK_OWNER.store(owner, Ordering::SeqCst);
}

/// The owner tag of a task spawned here and now. See `set_task_owner`.
pub(crate) fn current_task_owner() -> u64 {
    RUNNING_TASK_OWNER
        .try_with(|owner| owner.get())
        .ok()
        .flatten()
        .unwrap_or_else(|| PROGRAM_TASK_OWNER.load(Ordering::SeqCst))
}

/// What the report of a failure that nobody joined advises.
const UNJOINED_FAILURE_HELP: &str =
    "join the task with task.join to handle its error, or cancel it with task.cancel";

/// The first line of the report of a failure that nobody joined.
fn unjoined_failure_headline(task_id: usize, message: &str) -> String {
    format!("task <handle:{task_id}> failed and was never joined: {message}")
}

/// Set once a front end collects the failures that nobody joined. See
/// `collect_unjoined_failures`.
static COLLECTING: AtomicBool = AtomicBool::new(false);

/// The record of failed tasks of the whole process, used instead of the
/// schedulers' own while `COLLECTING` is set.
fn collected_failures() -> &'static Mutex<FailedTasks> {
    static COLLECTED: OnceLock<Mutex<FailedTasks>> = OnceLock::new();
    COLLECTED.get_or_init(|| Mutex::new(FailedTasks::default()))
}

/// A task that failed, whose error no join received and that no cancel
/// dismissed. Given out by `take_unjoined_failures`.
#[derive(Debug, Clone)]
pub struct UnjoinedFailure {
    /// The id of the task, as `<handle:N>` shows it.
    pub task_id: usize,
    /// The owner tag the task was spawned under. See `set_task_owner`.
    pub owner: u64,
    /// The error the task ended with, as the task raised it.
    pub error: VmError,
}

impl UnjoinedFailure {
    /// The error to report: the task's error, with a message that names
    /// the task and a help line that says what to do. Span and call
    /// stack are the task's.
    pub fn report_error(&self) -> VmError {
        let mut error = self.error.clone();
        error.message = unjoined_failure_headline(self.task_id, &self.error.message);
        error.with_help(UNJOINED_FAILURE_HELP)
    }
}

/// The failures that nobody joined, as `take_unjoined_failures` gives
/// them out.
#[derive(Debug, Default)]
pub struct UnjoinedFailures {
    /// The failures whose errors were kept, in the order in which the
    /// tasks failed.
    pub failures: Vec<UnjoinedFailure>,
    /// Per owner tag, the number of failed tasks whose errors were not
    /// kept: at most `MAX_RECORDED_FAILURES` unjoined failures are kept
    /// at a time.
    pub not_kept: Vec<(u64, usize)>,
}

impl UnjoinedFailures {
    /// True if there is nothing to report.
    pub fn is_empty(&self) -> bool {
        self.failures.is_empty() && self.not_kept.is_empty()
    }

    /// The message that reports `count` failed tasks whose errors were
    /// not kept.
    pub fn not_kept_message(count: usize) -> String {
        format!(
            "{count} more task(s) failed and were never joined; their errors were not kept \
             (at most {MAX_RECORDED_FAILURES} are kept at a time)"
        )
    }
}

/// From now on, keep the failures that nobody joined for
/// `take_unjoined_failures`, instead of having each scheduler print them
/// on stderr. This is for the front end that owns the process (`silt
/// run`, `silt test`, the REPL): it decides when a failure counts and
/// renders the report with the program's files. It cannot be undone.
///
/// A failure that is not taken is never reported. A task that fails
/// after the front end's last take, one that was still running when the
/// program ended, therefore leaves no report.
pub fn collect_unjoined_failures() {
    COLLECTING.store(true, Ordering::SeqCst);
}

/// Take every task failure, of any scheduler of the process, that has
/// been recorded since the last take, whose error no join received and
/// that no cancel dismissed. Each failure is given out once. Empty
/// unless `collect_unjoined_failures` has been called.
pub fn take_unjoined_failures() -> UnjoinedFailures {
    // The lock is released at the end of this statement; the handles
    // are dropped after that.
    let (handles, not_recorded) = collected_failures().lock().take();
    let failures = handles
        .iter()
        .filter_map(|handle| {
            handle.take_unjoined_failure().map(|error| UnjoinedFailure {
                task_id: handle.id,
                owner: handle.owner(),
                error,
            })
        })
        .collect();
    UnjoinedFailures {
        failures,
        not_kept: not_recorded.into_iter().collect(),
    }
}

#[cfg(test)]
mod tests {
    use crate::vm::{Output, Vm, VmError};
    use crate::{HostIo, Value};
    use parking_lot::Mutex;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// An output that keeps what was printed and when.
    #[derive(Clone, Default)]
    struct Timed(Arc<Mutex<Vec<(String, Instant)>>>);

    impl Output for Timed {
        fn write(&self, text: &str) -> std::io::Result<()> {
            self.0.lock().push((text.to_string(), Instant::now()));
            Ok(())
        }
    }

    impl Timed {
        /// When `line` was printed.
        fn at(&self, line: &str) -> Instant {
            let printed = self.0.lock();
            let found = printed.iter().find(|(text, _)| text.trim_end() == line);
            found.unwrap_or_else(|| panic!("`{line}` was printed")).1
        }
    }

    fn run(source: &str) -> (Result<Value, VmError>, Timed, Instant) {
        let program = crate::session::testing::compile_str(source).expect("the program compiles");
        let out = Timed::default();
        let mut vm = Vm::new(HostIo::new(out.clone(), out.clone()));
        let result = vm.run_program(&program);
        (result, out, Instant::now())
    }

    /// How long after the last event of a deadlocked program the
    /// verdict may come. It is given by the event itself (the last
    /// park, the last task's end, the last timer), so the bound is
    /// that of waking one thread.
    const VERDICT_WITHIN: Duration = Duration::from_millis(100);

    fn assert_deadlock(result: Result<Value, VmError>, what: &str) {
        let error = result.expect_err("the program is deadlocked");
        assert_eq!(error.message, format!("deadlock on main thread: {what}"));
    }

    #[test]
    fn a_deadlock_is_reported_when_the_last_task_parks() {
        let (result, out, end) = run(r#"
import channel
import task
import time
fn main() {
  let a = channel.new(0)
  let b = channel.new(0)
  let _waits = task.spawn({ ->
    time.sleep(time.ms(150))
    println("last")
    channel.receive(b)
  })
  channel.receive(a)
}
"#);
        assert_deadlock(result, "channel receive with no counterparty");
        let after = end.duration_since(out.at("last"));
        assert!(after < VERDICT_WITHIN, "the verdict came {after:?} late");
    }

    #[test]
    fn a_deadlock_is_reported_when_the_last_task_ends() {
        let (result, out, end) = run(r#"
import channel
import task
import time
fn main() {
  let ch = channel.new(0)
  let ends = task.spawn({ ->
    time.sleep(time.ms(150))
    println("last")
  })
  channel.send(ch, 1)
  task.join(ends)
}
"#);
        assert_deadlock(result, "channel send with no counterparty");
        let after = end.duration_since(out.at("last"));
        assert!(after < VERDICT_WITHIN, "the verdict came {after:?} late");
    }

    #[test]
    fn a_deadlock_is_reported_when_the_last_timer_has_fired() {
        let start = Instant::now();
        let (result, _, end) = run(r#"
import channel
import task
fn main() {
  let ch = channel.new(0)
  let timer = channel.timeout(150)
  let _waits = task.spawn({ -> channel.receive(timer) })
  let other = task.spawn({ -> channel.receive(ch) })
  task.join(other)
}
"#);
        assert_deadlock(result, "task.join with no progress possible");
        // Not before the timer: while it is pending, a wait can end.
        let took = end.duration_since(start);
        assert!(
            took >= Duration::from_millis(150),
            "the verdict came after {took:?}"
        );
    }

    #[test]
    fn a_program_without_tasks_is_deadlocked_at_once() {
        let (result, _, _) = run(r#"
import channel
fn main() {
  let a = channel.new(0)
  let b = channel.new(0)
  channel.select([channel.Recv(a), channel.Send(b, 1)])
}
"#);
        assert_deadlock(result, "channel select with no counterparty");
    }

    #[test]
    fn a_wait_that_a_task_can_end_is_no_deadlock() {
        // The task is busy for many slices while the program waits.
        let (result, _, _) = run(r#"
import channel
import task
fn spin(n, acc) { match n { 0 -> acc _ -> spin(n - 1, acc + 1) } }
fn main() {
  let ch = channel.new(0)
  let _sends = task.spawn({ -> channel.send(ch, spin(300000, 0)) })
  when let channel.Message(n) = channel.receive(ch) else { panic("closed") }
  n
}
"#);
        assert!(matches!(result, Ok(Value::Int(300000))), "got {result:?}");
    }
}
