//! The scheduler: tasks on a pool of worker threads, and the threads
//! that run silt code of their own.
//!
//! Every piece of running silt code is a task to the scheduler, whoever
//! drives it:
//!
//! - a task made by `task.spawn` is run slice by slice by the workers;
//! - the program itself (`fn main`, a test, a REPL entry), a stream
//!   stage and an HTTP handler are run by their own thread
//!   (`Scheduler::enter`).
//!
//! Each of them waits in the same place, the registry of parked tasks
//! ([`Parking`]), for the same thing, a [`Wait`]: a spawned task is
//! taken off its worker and put back on the run queue when its wait
//! ends; a thread waits where it is (`Scheduler::block_thread`) and,
//! where there are no workers, runs the queued tasks while it does.
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

use crate::runtime::handle::TaskHandle;
use crate::runtime::sync::{self, Channel, Fired, Parking, Resumed, Source, Stuck, TaskId, Wait};
use crate::value::Value;
use crate::vm::{HostIo, Vm, VmError};

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

/// Which deadline ended a wait for I/O: it is named in the error.
/// `Global` comes from `SILT_IO_TIMEOUT`; `Task` comes from a scoped
/// `task.deadline(dur, fn)` block.
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

/// Result of running a task's VM for one time slice.
pub(crate) enum SliceResult {
    /// Time slice expired; task is still runnable.
    Yielded,
    /// Task completed with a value.
    Completed(Value),
    /// Task failed with an error.
    Failed(VmError),
    /// The task waits for this.
    Blocked(Wait),
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
    /// The worker threads, started by the first task.
    workers: Mutex<Option<Vec<thread::JoinHandle<()>>>>,
}

/// Who waits in the registry of parked tasks.
// Nearly every sleeper is a task.
#[allow(clippy::large_enum_variant)]
enum Sleeper {
    /// A spawned task: it goes back on the run queue.
    Task(Task),
    /// A thread that runs silt code of its own: it is told.
    Thread(Arc<ThreadPark>),
}

/// Where a thread that waits finds how its wait ended, and sleeps
/// until it has.
struct ThreadPark {
    state: Mutex<ThreadState>,
    wake: Condvar,
    /// The spawned task whose slice the thread runs, if it is a
    /// worker's.
    task: Option<usize>,
    /// Who the code on the thread belongs to (`set_task_owner`).
    owner: u64,
}

#[derive(Default)]
struct ThreadState {
    resumed: Option<Resumed>,
    /// There may be something to do for the thread while it waits: a
    /// task to run where there are no workers, a deadline to keep
    /// where there is no timer thread.
    poked: bool,
}

/// What the scheduler says of a parked task in a deadlock report.
struct Who {
    /// The handle's number, for a spawned task; `None` for a thread
    /// that is no worker's (a stream stage, an HTTP handler).
    task: Option<usize>,
    owner: u64,
}

struct Inner {
    queue: Mutex<RunQueue>,
    /// The length of the queue, for a look without its lock.
    queued: AtomicUsize,
    /// How many workers look for a task without sleeping.
    spinning: AtomicUsize,
    /// The workers wait here for a task.
    work: Condvar,
    /// The threads that wait in [`Scheduler::block_thread`].
    waiting_threads: Mutex<Vec<Arc<ThreadPark>>>,
    shutdown: AtomicBool,
    /// Whether worker threads run the queue.
    has_workers: AtomicBool,
    /// The tasks that have not ended: spawned tasks, and threads that
    /// are running silt code.
    live: AtomicUsize,
    /// The spawned tasks among them, for `MAX_TASKS`.
    spawned: AtomicUsize,
    /// What can end a wait from outside the tasks, besides the timer:
    /// I/O operations in flight. See [`External`].
    external: AtomicUsize,
    parking: Parking<Sleeper>,
    /// The wait of the program's own thread, while it waits: who gets
    /// the verdict.
    main_wait: Mutex<Option<TaskId>>,
    /// The verdict, between the check that gave it and the program's
    /// thread that raises it.
    deadlock: Mutex<Option<VmError>>,
    /// The names of the waits of threads.
    next_thread_wait: AtomicU64,
    /// Whether the thread that runs the program knows this scheduler
    /// (`StartedSchedulers`).
    registered: AtomicBool,
    /// The thread that fires the timer waits here for the next
    /// deadline, or for an earlier one to be armed.
    timer_lock: Mutex<TimerThread>,
    timer_wake: Condvar,
    /// Global I/O timeout from `SILT_IO_TIMEOUT`. When set, every wait
    /// for I/O ends after it, unless a tighter task.deadline is in
    /// effect.
    global_io_timeout: Option<Duration>,
    /// Tasks that ended with an error, kept for the report of failures
    /// that nobody joined. See `report_unjoined_failures`.
    failed_tasks: Mutex<FailedTasks>,
    /// The host's clock and stderr.
    io: HostIo,
}

/// The tasks that wait for a worker.
#[derive(Default)]
struct RunQueue {
    tasks: VecDeque<Task>,
    /// How many workers sleep until a task comes.
    sleepers: usize,
}

/// The thread that fires the timer.
#[derive(PartialEq)]
enum TimerThread {
    /// No deadline has needed it yet.
    Idle,
    Running,
    /// It could not be started: the threads that wait fire the timer.
    Unavailable,
}

/// Something outside the tasks that can end a wait, for as long as it
/// lives: an I/O operation in flight, a thread that serves the
/// program. While one exists the program is not deadlocked. It is
/// dropped after the last thing it does for a task has returned.
pub(crate) struct External(Arc<Inner>);

impl Drop for External {
    fn drop(&mut self) {
        self.0.external.fetch_sub(1, Ordering::SeqCst);
        self.0.check_stuck();
    }
}

thread_local! {
    /// Whether the thread is counted as running silt code: it is a
    /// worker in a slice, or holds a [`Running`].
    static COUNTED: Cell<bool> = const { Cell::new(false) };
    /// The spawned task whose slice the thread is running.
    static RUNNING_TASK: Cell<Option<usize>> = const { Cell::new(None) };
}

/// A thread that is running silt code of its own, from
/// [`Scheduler::enter`] until it is dropped: it counts as a task.
pub(crate) struct Running {
    inner: Arc<Inner>,
    /// False for a guard taken on a thread that was counted already.
    counts: bool,
}

impl Running {
    /// The calling thread takes over a guard that another thread took
    /// for it ([`Scheduler::enter_for_thread`]).
    pub(crate) fn adopt(&self) {
        COUNTED.with(|counted| counted.set(true));
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if self.counts {
            let _ = COUNTED.try_with(|counted| counted.set(false));
            self.inner.live.fetch_sub(1, Ordering::SeqCst);
            self.inner.check_stuck();
        }
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
        let timer = sync::Timer::new(io.clone());
        let inner = Arc::new_cyclic(|inner: &Weak<Inner>| {
            let ready = inner.clone();
            Inner {
                queue: Mutex::default(),
                queued: AtomicUsize::new(0),
                spinning: AtomicUsize::new(0),
                work: Condvar::new(),
                waiting_threads: Mutex::new(Vec::new()),
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
                main_wait: Mutex::new(None),
                deadlock: Mutex::new(None),
                next_thread_wait: AtomicU64::new(0),
                registered: AtomicBool::new(false),
                timer_lock: Mutex::new(TimerThread::Idle),
                timer_wake: Condvar::new(),
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
        Ok(())
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
        self.inner.spawned.fetch_add(1, Ordering::SeqCst);
        self.inner.live.fetch_add(1, Ordering::SeqCst);
        self.inner.enqueue(task);
        Ok(())
    }

    /// What every operation on a channel or a cell is given: it hands
    /// the tasks whose wait the operation ended back to this scheduler.
    pub(crate) fn wake(&self) -> &dyn sync::Wake {
        &self.inner.parking
    }

    /// `task.cancel` of the task `handle`: it runs no further, and if
    /// it waits, its wait ends without taking or sending anything.
    pub(crate) fn cancel(&self, handle: &TaskHandle) {
        // The flag first: a task that is about to park reads it.
        handle.cancel(self.wake());
        self.inner.parking.cancel(TaskId(handle.id as u64));
    }

    /// Close `channel` when the clock reads `deadline`.
    pub(crate) fn close_at(&self, deadline: Duration, channel: Arc<Channel>) {
        self.inner.parking.timer().close_at(deadline, channel);
        self.inner.timer_armed();
    }

    /// When a wait for I/O that starts now ends without a result: the
    /// earlier of `SILT_IO_TIMEOUT` from now and the task deadline.
    pub(crate) fn io_deadline(
        &self,
        task_deadline: Option<Duration>,
    ) -> Option<(Duration, DeadlineSource)> {
        let global = self
            .inner
            .global_io_timeout
            .and_then(|timeout| self.inner.io.deadline_after(timeout))
            .map(|deadline| (deadline, DeadlineSource::Global));
        let task = task_deadline.map(|deadline| (deadline, DeadlineSource::Task));
        match (global, task) {
            (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
            (Some(x), None) | (None, Some(x)) => Some(x),
            (None, None) => None,
        }
    }

    /// The calling thread starts to run silt code of its own: it counts
    /// as a task until the guard is dropped. Nothing changes for a
    /// thread that counts already.
    pub(crate) fn enter(&self) -> Running {
        let counts = !COUNTED.with(|counted| counted.replace(true));
        if counts {
            self.inner.live.fetch_add(1, Ordering::SeqCst);
        }
        Running {
            inner: self.inner.clone(),
            counts,
        }
    }

    /// [`Scheduler::enter`] for a thread that is about to be started:
    /// it counts from now, so that nobody finds the program deadlocked
    /// before the thread runs. The thread calls [`Running::adopt`].
    pub(crate) fn enter_for_thread(&self) -> Running {
        self.inner.live.fetch_add(1, Ordering::SeqCst);
        Running {
            inner: self.inner.clone(),
            counts: true,
        }
    }

    /// See [`External`].
    pub(crate) fn external(&self) -> External {
        self.inner.external.fetch_add(1, Ordering::SeqCst);
        External(self.inner.clone())
    }

    /// Send `value` on `channel` from a thread, which waits while there
    /// is no receiver and no room. `false` when the channel is closed,
    /// or the program has ended: the value was not sent.
    pub(crate) fn send_wait(&self, channel: &Arc<Channel>, value: Value) -> bool {
        let value = match channel.try_send(value, self.wake()) {
            sync::TrySend::Sent => return true,
            sync::TrySend::Closed(_) => return false,
            sync::TrySend::Full(value) => value,
        };
        let wait = Wait::new(vec![sync::Arm::Send(channel.clone(), value)]);
        matches!(
            self.block_thread(wait, false),
            Ok(Fired::Arm(_, sync::Outcome::Sent))
        )
    }

    /// Receive from `channel` on a thread, which waits while the
    /// channel is empty: a value, or `None` when the channel is closed
    /// and empty, or the program has ended. `main` as for
    /// [`Scheduler::block_thread`], whose error is returned.
    pub(crate) fn receive_wait(
        &self,
        channel: &Arc<Channel>,
        main: bool,
    ) -> Result<Option<Value>, VmError> {
        match channel.try_receive(self.wake()) {
            sync::TryReceive::Value(value) => return Ok(Some(value)),
            sync::TryReceive::Closed(_) => return Ok(None),
            sync::TryReceive::Empty => {}
        }
        let wait = Wait::new(vec![sync::Arm::Recv(channel.clone())]);
        Ok(match self.block_thread(wait, main)? {
            Fired::Arm(_, sync::Outcome::Received(value)) => Some(value),
            _ => None,
        })
    }

    /// The calling thread waits for `wait` and gets how it ended. It
    /// counts as a task while it waits, if it does not already (a
    /// thread after [`Scheduler::enter`], a worker in a slice). `main` says that the
    /// thread is the program's own: it gets the error when the program
    /// is deadlocked. The other error is that the program has ended.
    pub(crate) fn block_thread(&self, wait: Wait, main: bool) -> Result<Fired, VmError> {
        let inner = &self.inner;
        let _counted = self.enter();
        // The names of spawned tasks count up from 0.
        let id = TaskId(u64::MAX - inner.next_thread_wait.fetch_add(1, Ordering::Relaxed));
        if main {
            *inner.main_wait.lock() = Some(id);
        }
        let timed = wait.deadline.is_some();
        let park = Arc::new(ThreadPark {
            state: Mutex::default(),
            wake: Condvar::new(),
            task: RUNNING_TASK.with(|task| task.get()),
            owner: current_task_owner(),
        });
        let sleeper = Sleeper::Thread(park.clone());
        let resumed = match inner.parking.park(id, sleeper, wait, || false) {
            Some((_, resumed)) => resumed,
            None => {
                if timed {
                    inner.timer_armed();
                }
                inner.check_stuck();
                inner.wait_thread(&park)
            }
        };
        // The verdict is the program's thread's.
        let verdict = if main {
            *inner.main_wait.lock() = None;
            inner.deadlock.lock().take()
        } else {
            None
        };
        match resumed {
            Resumed::Fired(fired) => Ok(fired),
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
        queue.tasks.push_back(task);
        self.queued.store(queue.tasks.len(), Ordering::SeqCst);
        if self.has_workers.load(Ordering::SeqCst) {
            // A worker that looks for a task finds this one, without
            // anybody being woken.
            if queue.sleepers > 0 && self.spinning.load(Ordering::SeqCst) == 0 {
                self.work.notify_one();
            }
        } else {
            drop(queue);
            self.poke_threads();
        }
    }

    /// The next task for a worker; `None` when the scheduler has shut
    /// down. A worker that finds no task looks on for a moment before
    /// it sleeps: in a program whose tasks wake each other, the next
    /// one comes within microseconds, and putting a thread to sleep
    /// and waking it costs more than that.
    fn next_task(&self) -> Option<Task> {
        /// How many workers look on at a time.
        const LOOKING: usize = 2;
        loop {
            {
                let mut queue = self.queue.lock();
                if self.shutdown.load(Ordering::SeqCst) {
                    return None;
                }
                if let Some(task) = self.pop(&mut queue) {
                    return Some(task);
                }
            }
            if more_than_one_core() {
                let looks = self.spinning.fetch_add(1, Ordering::SeqCst) < LOOKING;
                let mut found = false;
                if looks {
                    let until = std::time::Instant::now() + SPIN;
                    while !found && std::time::Instant::now() < until {
                        found = self.queued.load(Ordering::SeqCst) > 0
                            || self.shutdown.load(Ordering::SeqCst);
                        std::hint::spin_loop();
                    }
                }
                self.spinning.fetch_sub(1, Ordering::SeqCst);
                if found {
                    continue;
                }
            }
            // A task queued since the look above is found here: who
            // queued it saw this worker looking and woke nobody.
            let mut queue = self.queue.lock();
            if self.shutdown.load(Ordering::SeqCst) {
                return None;
            }
            if let Some(task) = self.pop(&mut queue) {
                return Some(task);
            }
            queue.sleepers += 1;
            self.work.wait(&mut queue);
            queue.sleepers -= 1;
        }
    }

    /// Take the first task off the queue. If more are left and nobody
    /// is on the way to them, a sleeping worker is woken.
    fn pop(&self, queue: &mut RunQueue) -> Option<Task> {
        let task = queue.tasks.pop_front()?;
        self.queued.store(queue.tasks.len(), Ordering::SeqCst);
        if !queue.tasks.is_empty()
            && queue.sleepers > 0
            && self.spinning.load(Ordering::SeqCst) == 0
        {
            self.work.notify_one();
        }
        Some(task)
    }

    /// Tell the threads that wait that there may be something for
    /// them to do.
    fn poke_threads(&self) {
        let threads = self.waiting_threads.lock().clone();
        for thread in threads {
            thread.state.lock().poked = true;
            thread.wake.notify_one();
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
        match sleeper {
            Sleeper::Task(mut task) => match resumed {
                Resumed::Fired(fired) => {
                    task.vm.woken = Some(fired);
                    self.enqueue(task);
                }
                Resumed::Cancelled => self.end_task(task),
            },
            Sleeper::Thread(park) => {
                park.state.lock().resumed = Some(resumed);
                park.wake.notify_one();
            }
        }
    }

    /// The calling thread waits until its wait has ended. Where no
    /// worker runs the queue, it does; where no thread fires the
    /// timer, it does.
    fn wait_thread(self: &Arc<Self>, park: &Arc<ThreadPark>) -> Resumed {
        // Most waits of a thread are short: its counterpart is a task
        // that answers within microseconds. Looking for the answer for
        // that long first saves putting the thread to sleep and waking
        // it, which costs more than the wait.
        // Not while tasks wait for a worker: then the processors are
        // busy, and the answer is not next.
        let idle = || self.queued.load(Ordering::SeqCst) <= 1;
        if self.has_workers.load(Ordering::SeqCst) && more_than_one_core() && idle() {
            let until = std::time::Instant::now() + SPIN;
            loop {
                if let Some(resumed) = park.state.lock().resumed.take() {
                    return resumed;
                }
                if std::time::Instant::now() >= until {
                    break;
                }
                std::hint::spin_loop();
            }
        }
        // From here the thread can be told that there is something to
        // do for it.
        self.waiting_threads.lock().push(park.clone());
        let resumed = self.sleep_thread(park);
        self.waiting_threads
            .lock()
            .retain(|other| !Arc::ptr_eq(other, park));
        resumed
    }

    fn sleep_thread(self: &Arc<Self>, park: &ThreadPark) -> Resumed {
        loop {
            // What there is to do for a thread that waits.
            if !self.has_workers.load(Ordering::SeqCst) {
                let task = self.pop(&mut self.queue.lock());
                if let Some(task) = task {
                    self.run_slice(task);
                    if let Some(resumed) = park.state.lock().resumed.take() {
                        return resumed;
                    }
                    continue;
                }
            }
            let timer = self.parking.timer();
            let next = match *self.timer_lock.lock() {
                TimerThread::Unavailable => timer.real_wait(),
                TimerThread::Idle | TimerThread::Running => None,
            };
            if next.is_some_and(|next| next.is_zero()) {
                if timer.fire_due(&self.parking) > 0 {
                    self.check_stuck();
                }
                if let Some(resumed) = park.state.lock().resumed.take() {
                    return resumed;
                }
                continue;
            }
            // A poke since the look above is not lost: it is noted
            // under the lock that the sleep gives up.
            let mut state = park.state.lock();
            if let Some(resumed) = state.resumed.take() {
                return resumed;
            }
            if !state.poked {
                match next {
                    None => park.wake.wait(&mut state),
                    Some(next) => {
                        let _ = park.wake.wait_for(&mut state, next);
                    }
                }
            }
            state.poked = false;
            if let Some(resumed) = state.resumed.take() {
                return resumed;
            }
            drop(state);
            if next.is_some() && timer.fire_due(&self.parking) > 0 {
                self.check_stuck();
            }
        }
    }

    /// A deadline was put in the timer: the thread that fires the
    /// timer is started, or told to look again.
    fn timer_armed(self: &Arc<Self>) {
        let mut thread = self.timer_lock.lock();
        if *thread == TimerThread::Idle {
            let inner = self.clone();
            let started = thread::Builder::new()
                .name("silt-timer".into())
                .spawn(move || timer_loop(inner));
            *thread = match started {
                Ok(_) => TimerThread::Running,
                Err(_) => TimerThread::Unavailable,
            };
        }
        match *thread {
            TimerThread::Running => {
                self.timer_wake.notify_one();
            }
            // A thread that waits may wait for longer than up to the
            // new deadline.
            _ => {
                drop(thread);
                self.poke_threads();
            }
        }
    }

    /// Run a task for one slice, and do what its end asks for.
    fn run_slice(self: &Arc<Self>, task: Task) {
        /// How often in a row a task whose wait was over at once goes
        /// on without giving way.
        const GO_ON: usize = 16;
        let mut task = task;
        for _ in 0..GO_ON {
            match self.run_once(task) {
                Some(again) => task = again,
                None => return,
            }
        }
        self.enqueue(task);
    }

    /// One slice of a task. `Some` when the slice ended in a wait that
    /// was over at once: the task has its result and can go on.
    fn run_once(self: &Arc<Self>, task: Task) -> Option<Task> {
        let Task { id, mut vm, handle } = task;
        // A task that was cancelled runs no further.
        if handle.is_cancelled() {
            self.end_task(Task { id, vm, handle });
            return None;
        }
        // The tasks that this slice spawns belong to the owner of this
        // task. See `set_task_owner`.
        let outer = RUNNING_TASK_OWNER.with(|owner| owner.replace(Some(handle.owner())));
        let counted = COUNTED.with(|counted| counted.replace(true));
        let running = RUNNING_TASK.with(|task| task.replace(Some(id)));
        let result = vm.execute_slice(time_slice());
        RUNNING_TASK.with(|task| task.set(running));
        COUNTED.with(|was| was.set(counted));
        RUNNING_TASK_OWNER.with(|owner| owner.set(outer));

        match result {
            SliceResult::Yielded => self.enqueue(Task { id, vm, handle }),
            SliceResult::Completed(value) => {
                handle.complete(Ok(value), &self.parking);
                self.end_task(Task { id, vm, handle });
            }
            SliceResult::Failed(error) => {
                // A failure that no join receives is reported when the
                // program ends, so it is recorded here. `fail` returns
                // false if the task had been cancelled before: its
                // handle keeps the cancellation as its result then.
                if handle.fail(vm.enrich_error(error), &self.parking) {
                    record_failed_task(self, &handle);
                }
                self.end_task(Task { id, vm, handle });
            }
            SliceResult::Blocked(wait) => {
                let wait = wait.cancel(Some(handle.cancel_flag()));
                let timed = wait.deadline.is_some();
                let cancelled = handle.clone();
                let sleeper = Sleeper::Task(Task { id, vm, handle });
                let back = self.parking.park(TaskId(id as u64), sleeper, wait, || {
                    cancelled.is_cancelled()
                });
                match back {
                    Some((Sleeper::Task(mut task), Resumed::Fired(fired))) => {
                        task.vm.woken = Some(fired);
                        return Some(task);
                    }
                    Some((sleeper, resumed)) => self.ready(sleeper, resumed),
                    None => {
                        if timed {
                            self.timer_armed();
                        }
                        self.check_stuck();
                    }
                }
            }
        }
        None
    }

    /// Give the verdict if the program is deadlocked: its own thread
    /// waits, and so does every other task, with nothing pending
    /// outside.
    fn check_stuck(&self) {
        // A first look without a lock. A deadlock stays one, so the
        // event that completes it (this is called after each) sees it
        // here too.
        if self.external.load(Ordering::SeqCst) > 0
            || self.parking.waiting() != self.live.load(Ordering::SeqCst)
        {
            return;
        }
        // Without the program's thread waiting there is nobody to tell.
        let Some(main) = *self.main_wait.lock() else {
            return;
        };
        let stuck = self.parking.stuck(
            || self.live.load(Ordering::SeqCst),
            || self.external.load(Ordering::SeqCst),
            |sleeper| match sleeper {
                Sleeper::Task(task) => Who {
                    task: Some(task.id),
                    owner: task.handle.owner(),
                },
                Sleeper::Thread(park) => Who {
                    task: park.task,
                    owner: park.owner,
                },
            },
        );
        let Some(stuck) = stuck else {
            return;
        };
        let Some(own) = stuck.iter().find(|stuck| stuck.task == main) else {
            return;
        };
        let what = match own.on.as_slice() {
            [Source::Recv(_)] => "channel receive with no counterparty",
            [Source::Send(_)] => "channel send with no counterparty",
            [Source::Cell(_)] => "task.join with no progress possible",
            _ => "channel select with no counterparty",
        };
        let mut verdict = VmError::new(format!("deadlock on main thread: {what}"));
        for line in waits_of_the_others(&stuck, main, own.who.owner) {
            verdict = verdict.with_help(line);
        }
        *self.deadlock.lock() = Some(verdict);
        self.parking.cancel(main);
    }
}

/// What each parked task besides the program's own waits on, one line
/// each, for the deadlock report. Only the tasks of `owner` are
/// listed: in a test run, those of the test that is deadlocked, not
/// what earlier tests left parked.
fn waits_of_the_others(stuck: &[Stuck<Who>], main: TaskId, owner: u64) -> Vec<String> {
    /// How many tasks are listed.
    const LISTED: usize = 8;
    let others: Vec<&Stuck<Who>> = stuck
        .iter()
        .filter(|stuck| stuck.task != main && stuck.who.owner == owner)
        .collect();
    let mut lines: Vec<String> = others
        .iter()
        .take(LISTED)
        .map(|stuck| {
            let on: Vec<String> = stuck
                .on
                .iter()
                .map(|source| match source {
                    Source::Recv(channel) => format!("to receive from <channel:{}>", channel.id()),
                    Source::Send(channel) => format!("to send to <channel:{}>", channel.id()),
                    Source::Cell(cell) => match cell.label() {
                        Some(label) => format!("for {label} to end"),
                        None => "for a task to end".to_string(),
                    },
                })
                .collect();
            let who = match stuck.who.task {
                Some(id) => format!("task <handle:{id}>"),
                None => "a stream stage or handler".to_string(),
            };
            format!("{who} waits {}", on.join(", or "))
        })
        .collect();
    if others.len() > LISTED {
        lines.push(format!("{} more tasks wait", others.len() - LISTED));
    }
    lines
}

/// How long a thread looks for the end of its wait before it sleeps.
const SPIN: Duration = Duration::from_micros(50);

fn more_than_one_core() -> bool {
    static MORE: OnceLock<bool> = OnceLock::new();
    *MORE.get_or_init(|| thread::available_parallelism().is_ok_and(|n| n.get() > 1))
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
    /// more), tell the workers and the timer thread to end, and drop
    /// the tasks that wait for a worker or for anything else. A task
    /// submitted later is refused.
    ///
    /// The threads are not waited for: each ends when the slice it is
    /// running returns.
    pub(crate) fn shutdown(&self) {
        let inner = &self.inner;
        let _ = report_unjoined_failures(inner);
        inner.shutdown.store(true, Ordering::SeqCst);
        {
            let _timer = inner.timer_lock.lock();
            inner.timer_wake.notify_all();
        }
        // Detach the workers.
        drop(self.workers.lock().take());
        // The tasks are dropped after the queue's lock is released.
        let waiting: Vec<Task> = inner.queue.lock().tasks.drain(..).collect();
        inner.queued.store(0, Ordering::SeqCst);
        inner.work.notify_all();
        for task in waiting {
            inner.end_task(task);
        }
        // The parked tasks never run again either: each comes off
        // every queue and out of the timer, and its frames are
        // abandoned. A thread that waits is told that the program is
        // gone.
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
    while let Some(task) = inner.next_task() {
        inner.run_slice(task);
    }
}

/// The thread that fires the timer: wait for the next deadline, or for
/// an earlier one to be armed, and end the waits that are due.
fn timer_loop(inner: Arc<Inner>) {
    let timer = inner.parking.timer().clone();
    loop {
        {
            let mut thread = inner.timer_lock.lock();
            if inner.shutdown.load(Ordering::SeqCst) {
                return;
            }
            match timer.real_wait() {
                None => inner.timer_wake.wait(&mut thread),
                Some(wait) if wait.is_zero() => {}
                Some(wait) => {
                    let _ = inner.timer_wake.wait_for(&mut thread, wait);
                }
            }
        }
        if inner.shutdown.load(Ordering::SeqCst) {
            return;
        }
        // A deadline that ended nobody's wait (a channel that closed
        // unheard) may have been the last thing pending.
        if timer.fire_due(&inner.parking) > 0 {
            inner.check_stuck();
        }
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
