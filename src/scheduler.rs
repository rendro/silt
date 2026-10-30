//! The scheduler: tasks on a pool of worker threads, and the threads
//! that run silt code of their own.
//!
//! Every piece of running silt code is a task to the scheduler, whoever
//! drives it:
//!
//! - a task made by `task.spawn`, a stage of a stream and the handler
//!   of an HTTP request are run slice by slice by the workers;
//! - the program itself (`fn main`, a test, a REPL entry) is run by
//!   its own thread (`Scheduler::enter`).
//!
//! Each of them waits in the same place, the registry of parked tasks
//! ([`Parking`]), for the same thing, a [`Wait`]: a spawned task is
//! taken off its worker and put back on the run queue when its wait
//! ends; a thread waits where it is (`Scheduler::block_thread`) and,
//! where there are no workers, runs the queued tasks while it does.
//!
//! # Deadlock, and the end of a program
//!
//! Tasks are counted by owner (`Vm::set_task_owner`): the program, or one
//! test of a test run, with which the tasks of its file's top-level
//! code count (`Vm::set_task_owner_within`). The tasks of an owner have
//! come to a stop when each has ended or waits, none of the waits
//! will end by itself (it has no deadline, and it is not on a channel
//! that a timer will close), and no I/O operation that one of them
//! waits for is in flight (`Scheduler::external`). That is checked
//! when one of them parks or ends and when something external of
//! theirs ends; it is exact and stays true, so the event that
//! completes it sees it.
//!
//! - If the program's own thread is in a wait then, the program is
//!   deadlocked: the wait fails with the error.
//! - If the program's own code has returned and its thread waits for
//!   the program to end (`Scheduler::settle`), it has ended: the tasks
//!   that still wait are dropped.
//!
//! A timer that will close a channel which no task waits on wakes
//! nobody, and holds nothing up. After an error of the program's own
//! code nothing is waited for: `Scheduler::stop_tasks` ends the
//! owner's tasks where they are.
//!
//! # A panic of the runtime
//!
//! A panic inside a builtin, a host function, an I/O operation or the
//! host's clock is caught where that is called, and is the error of
//! the call. A panic anywhere else on a thread of the runtime (a
//! worker, the timer's thread, a thread of the I/O pool) is a bug of
//! the runtime itself, whose state is not to be trusted after it: it
//! ends the program. It is caught at the top of the thread
//! (`Inner::fail`), no worker takes another task, and the program's
//! own thread gives the panic on, out of the call of the VM it is in
//! or makes next (`Scheduler::give_panic_on`): where a panic of the
//! program's own code on that thread comes out. That is all the
//! library does: it never ends an embedder's process. In a process
//! that is silt's own ([`exit_on_panic`], which the `silt` command
//! calls) no such panic is caught: it ends the process where it is
//! raised, on the thread that raised it, whatever the program's
//! thread is doing.

use parking_lot::{Condvar, Mutex, ReentrantMutex};
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::panic::{AssertUnwindSafe, PanicHookInfo, UnwindSafe, catch_unwind, resume_unwind};
use std::sync::Arc;
use std::sync::Once;
use std::sync::OnceLock;
use std::sync::Weak;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use crate::runtime::handle::TaskHandle;
use crate::runtime::sync::{self, Channel, Fired, Parking, Resumed, Source, TaskId, Wait, Waiting};
use crate::value::Value;
use crate::vm::{HostIo, Vm, VmError};

#[doc(hidden)]
pub mod test_support;

/// Maximum number of live (active + blocked + queued) tasks the scheduler allows.
const MAX_TASKS: usize = 100_000;

/// Stack size of every scheduler worker thread, in bytes.
///
/// A silt call takes a frame of the VM and no native stack, however it
/// is made, and a value that is nested deeply is compared, hashed,
/// shown and dropped on none. What still goes down a value on the
/// worker's native stack is its encoding as JSON or TOML, to the bound
/// those have (`MAX_ENCODE_DEPTH` levels, src/builtins/data.rs): in a
/// debug build that takes megabytes, more than the platform default
/// for a spawned thread (2 MiB) holds. A native stack overflow aborts
/// the whole process.
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

/// What the error of an I/O operation says when the wait for it ended
/// at `SILT_IO_TIMEOUT`. (A `task.deadline` ends the function it runs,
/// not a wait of it: see `builtins::concurrency`.)
pub(crate) const IO_TIMEOUT: &str = "I/O timeout (SILT_IO_TIMEOUT exceeded)";

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
struct Task {
    id: usize,
    vm: Vm,
    handle: Arc<TaskHandle>,
    /// The tasks it is counted with: those of its owner.
    group: Arc<Group>,
    /// Whether a slice of it has run. A task that is cancelled before
    /// its first slice is dropped without running; one that has run
    /// goes on for the steps a cancelled task has
    /// ([`Inner::late_slice`]).
    ran: bool,
}

/// The tasks of one owner (`Vm::set_task_owner`): the program, or one
/// test of a test run. Whether a program is deadlocked, and whether it
/// has ended, is asked of its own tasks: what an earlier test left
/// behind does not count.
struct Group {
    owner: u64,
    /// The tasks that have not ended: spawned tasks, and threads that
    /// are running silt code.
    live: AtomicUsize,
    /// What can end a wait of one of them from outside the tasks: I/O
    /// operations in flight. See [`External`]. (The deadline of a wait
    /// is seen on the wait.)
    external: AtomicUsize,
    /// The tasks that wait or are on their way into a wait: never
    /// fewer than those that wait. A look without a lock that spares
    /// most of the exact ones.
    parking: AtomicUsize,
    /// Set while its tasks are being stopped
    /// ([`Scheduler::stop_tasks`]): none of them runs another slice
    /// or enters a wait.
    stopped: AtomicBool,
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
    /// The tasks that the code on the thread counts with: those of
    /// its owner (`Vm::set_task_owner`).
    group: Arc<Group>,
}

#[derive(Default)]
struct ThreadState {
    resumed: Option<Resumed>,
    /// There may be something to do for the thread while it waits: a
    /// task to run where there are no workers, a deadline to keep
    /// where there is no timer thread.
    poked: bool,
}

struct Inner {
    /// Which scheduler of the process this is.
    id: u64,
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
    /// The spawned tasks that have not ended, for `MAX_TASKS`.
    spawned: AtomicUsize,
    /// The tasks by owner.
    groups: Mutex<HashMap<u64, Arc<Group>>>,
    parking: Parking<Sleeper>,
    /// What the program's own thread waits for, while it waits: it is
    /// told when its tasks can do nothing more.
    main: Mutex<Option<Main>>,
    /// The verdict, between the check that gave it and the program's
    /// thread that raises it.
    deadlock: Mutex<Option<VmError>>,
    /// The names of the waits of threads.
    next_thread_wait: AtomicU64,
    /// The owner tag of the tasks spawned outside any task
    /// (`Scheduler::set_owner`).
    owner: AtomicU64,
    /// The owner tag of the tasks that are there besides those of
    /// `owner`; 0 for none.
    outer: AtomicU64,
    /// Whether the thread that runs the program knows this scheduler
    /// (`StartedSchedulers`).
    registered: AtomicBool,
    /// Whether a task or an I/O operation was ever started
    /// ([`Scheduler::begin`]): until then no thread of the runtime that
    /// may read the environment exists.
    begun: AtomicBool,
    /// The thread that fires the timer waits here for the next
    /// deadline, or for an earlier one to be armed.
    timer_lock: Mutex<TimerThread>,
    timer_wake: Condvar,
    /// Global I/O timeout from `SILT_IO_TIMEOUT`. When set, every wait
    /// for I/O ends after it.
    global_io_timeout: Option<Duration>,
    /// Tasks that ended with an error, kept for the report of failures
    /// that nobody joined. See `report_unjoined_failures`.
    failed_tasks: Mutex<FailedTasks>,
    /// The host's clock and stderr.
    io: HostIo,
    /// Set when a thread of the runtime has panicked
    /// ([`Inner::fail`]): no task runs another slice, and the
    /// program's thread gives the panic on.
    broken: AtomicBool,
    /// That panic.
    panic: Mutex<Panicked>,
    /// The code of the first `process.exit` that ended the run
    /// ([`Scheduler::exit`]).
    exit: OnceLock<u8>,
    /// Test-only: where the runtime is to panic, once at each
    /// ([`PanicAt`]).
    #[cfg(any(test, feature = "test-hooks"))]
    panic_at: Mutex<Vec<PanicAt>>,
    /// Test-only: whether the next call of a builtin is to panic
    /// ([`PanicAt::Builtin`]). (Not in the list above: every call of a
    /// builtin asks, and reads this without a lock.)
    #[cfg(any(test, feature = "test-hooks"))]
    panic_in_a_builtin: AtomicBool,
    /// Test-only: the same for the next call a worker makes
    /// ([`PanicAt::BuiltinOfATask`]).
    #[cfg(any(test, feature = "test-hooks"))]
    panic_in_a_builtin_of_a_task: AtomicBool,
}

/// The panic of a thread of the runtime ([`Inner::fail`]).
#[derive(Default)]
struct Panicked {
    /// What it was raised with, until the program's thread takes it.
    payload: Option<Box<dyn Any + Send>>,
    /// Its message, for a call of the VM that comes after that.
    text: String,
}

/// Test-only: a place in the runtime's own code, outside every catch
/// of a builtin's or a host function's panic, where a test makes the
/// runtime panic once (`silt::vm::panic_for_test`). No silt program
/// can make it panic there.
#[cfg(any(test, feature = "test-hooks"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanicAt {
    /// On the worker that is about to run a slice of the spawned task
    /// with this id (`<handle:id>`), when the task has run before: a
    /// task that waited is lost as it is woken. A test's task must
    /// certainly have such a slice. A `time.sleep` makes it so (it
    /// always ends the slice); a receive does not (it may find its
    /// value there, and the task goes on in its first slice).
    Task(usize),
    /// On the program's own thread, when a wait of its code has ended.
    Program,
    /// On the timer's thread, when a deadline is due. Every deadline
    /// of a wait goes through the timer then, one that had passed
    /// when its wait began too (`Timer::fire_every_deadline`).
    Timer,
    /// On a thread of the I/O pool, when its operation has returned
    /// and before the value is handed to the waiter.
    Io,
    /// On the thread that drops the VM, as the scheduler is shut down.
    Shutdown,
    /// On a thread that is unwinding from a panic, as it drops a VM:
    /// a second panic while the first is on its way.
    Unwinding,
    /// Inside the next call of a builtin, on the thread that makes
    /// it. This one is no panic of the runtime: it is the error of
    /// the call, as a panic of a builtin's own code would be.
    Builtin,
    /// The same inside the next call of a builtin that a worker
    /// makes: in a spawned task. (The first call of a builtin in any
    /// program is on the program's own thread.)
    BuiltinOfATask,
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
    /// It could not be started: the threads that wait and the workers
    /// without work fire the timer. (While every worker runs a task
    /// and no thread waits, nobody does: a deadline is then late by
    /// as long as that lasts.)
    Unavailable,
}

/// What the program's own thread waits for.
#[derive(Clone)]
struct Main {
    /// The tasks of the owner it runs code for.
    group: Arc<Group>,
    /// The tasks that are there besides, and count with them
    /// (`Vm::set_task_owner_within`).
    outer: Option<Arc<Group>>,
    waits: MainWaits,
}

impl Main {
    /// The thread that calls is the program's own, and `group` its
    /// owner's tasks.
    fn of_this_thread(inner: &Inner, group: Arc<Group>, waits: MainWaits) -> Main {
        let outer = inner.outer.load(Ordering::SeqCst);
        Main {
            group,
            outer: (outer != 0).then(|| inner.group_of(outer)),
            waits,
        }
    }

    /// Whether the tasks of `owner` are among those it waits on.
    fn counts(&self, owner: u64) -> bool {
        self.group.owner == owner || self.outer.as_ref().is_some_and(|g| g.owner == owner)
    }

    fn groups(&self) -> impl Iterator<Item = &Arc<Group>> {
        std::iter::once(&self.group).chain(&self.outer)
    }
}

#[derive(Clone)]
enum MainWaits {
    /// In a wait of the program's code: when no task it waits on can
    /// go on, that is a deadlock, and the wait fails.
    Blocked(TaskId),
    /// For its tasks to come to a stop, its own code having returned
    /// ([`Scheduler::settle`]).
    Settling(Arc<ThreadPark>),
    /// For its tasks to have ended, each stopped where it was
    /// ([`Scheduler::stop_tasks`]).
    Stopping(Arc<ThreadPark>),
}

/// Something outside the tasks that can end a wait, for as long as it
/// lives: an I/O operation in flight, a thread that serves the
/// program. While one exists the program is neither deadlocked nor
/// ended. It is dropped after the last thing it does for a task has
/// returned.
pub(crate) struct External(Arc<Inner>, Arc<Group>);

impl Drop for External {
    fn drop(&mut self) {
        self.1.external.fetch_sub(1, Ordering::SeqCst);
        self.0.check(&self.1);
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
    /// The tasks it counts with; `None` for a guard taken on a thread
    /// that was counted already.
    counted: Option<Arc<Group>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(group) = &self.counted {
            let _ = COUNTED.try_with(|counted| counted.set(false));
            group.live.fetch_sub(1, Ordering::SeqCst);
            self.inner.check(group);
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
            static SCHEDULERS: AtomicU64 = AtomicU64::new(0);
            Inner {
                id: SCHEDULERS.fetch_add(1, Ordering::Relaxed),
                queue: Mutex::default(),
                queued: AtomicUsize::new(0),
                spinning: AtomicUsize::new(0),
                work: Condvar::new(),
                waiting_threads: Mutex::new(Vec::new()),
                shutdown: AtomicBool::new(false),
                has_workers: AtomicBool::new(false),
                spawned: AtomicUsize::new(0),
                groups: Mutex::new(HashMap::new()),
                parking: Parking::new(timer, move |sleeper, resumed| {
                    // Without a scheduler the task is dropped.
                    if let Some(inner) = ready.upgrade() {
                        inner.ready(sleeper, resumed);
                    }
                }),
                main: Mutex::new(None),
                deadlock: Mutex::new(None),
                next_thread_wait: AtomicU64::new(0),
                owner: AtomicU64::new(0),
                outer: AtomicU64::new(0),
                registered: AtomicBool::new(false),
                begun: AtomicBool::new(false),
                timer_lock: Mutex::new(TimerThread::Idle),
                timer_wake: Condvar::new(),
                global_io_timeout,
                failed_tasks: Mutex::new(FailedTasks::default()),
                io,
                broken: AtomicBool::new(false),
                panic: Mutex::default(),
                exit: OnceLock::new(),
                #[cfg(any(test, feature = "test-hooks"))]
                panic_at: Mutex::new(Vec::new()),
                #[cfg(any(test, feature = "test-hooks"))]
                panic_in_a_builtin: AtomicBool::new(false),
                #[cfg(any(test, feature = "test-hooks"))]
                panic_in_a_builtin_of_a_task: AtomicBool::new(false),
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
    #[cfg(not(target_arch = "wasm32"))]
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
            let mut builder = thread::Builder::new().name("silt-worker".into());
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

    /// Start the task `id`, whose VM `vm` has its first frame and
    /// whose result goes to `handle`.
    ///
    /// Returns an error if the live-task count has reached the
    /// scheduler's hard task limit.
    pub(crate) fn submit(
        &self,
        id: usize,
        mut vm: Vm,
        handle: Arc<TaskHandle>,
    ) -> Result<(), String> {
        self.begin();
        vm.cancelled = Some(handle.cancel_flag());
        vm.deadline = handle.deadline();
        vm.late = handle.deadline().map(|_| handle.late());
        vm.over = handle.deadline().map(|_| handle.over_flag());
        if self.inner.shutdown.load(Ordering::SeqCst) {
            return Err("cannot spawn a task: the VM that ran the program has been dropped".into());
        }
        if self.inner.broken.load(Ordering::SeqCst) {
            return Err("cannot spawn a task: a thread of the runtime has panicked".into());
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
        let group = self.inner.group(handle.owner());
        self.inner.spawned.fetch_add(1, Ordering::SeqCst);
        group.live.fetch_add(1, Ordering::SeqCst);
        self.inner.enqueue(Task {
            id,
            vm,
            handle,
            group,
            ran: false,
        });
        Ok(())
    }

    /// What every operation on a channel or a cell is given: it hands
    /// the tasks whose wait the operation ended back to this scheduler.
    pub(crate) fn wake(&self) -> &dyn sync::Wake {
        &self.inner.parking
    }

    /// `task.cancel` of the task `handle`. What becomes of the task
    /// depends on where it is:
    ///
    /// - It waits: its wait ends without taking or sending anything,
    ///   and the task is ended there.
    /// - It has never run: it is dropped without running.
    /// - It runs, or is to run again: a wait that had ended when the
    ///   cancel came stays ended, and the value a receive of the task
    ///   was handed, or that the operation it waited for gave, is the
    ///   task's to use. The task goes on for a counted number of steps
    ///   ([`LATE_STEPS`], as a task past its deadline does) and is
    ///   ended where they are used up, or before that at the first
    ///   wait that cannot end at once (`Inner::run_once`). It takes
    ///   nothing new in them: an operation that would ends it too
    ///   (`Vm::takes_nothing_new`).
    pub(crate) fn cancel(&self, handle: &TaskHandle) {
        // The flag first: a task that is about to park reads it.
        handle.cancel(self.wake());
        self.inner.parking.cancel(TaskId(handle.id as u64));
    }

    /// Close `channel` when the clock reads `deadline`. Until then a
    /// task that waits on the channel has a wait that will end.
    pub(crate) fn close_at(&self, deadline: Duration, channel: Arc<Channel>) {
        let timer = self.inner.parking.timer();
        timer.close_at(deadline, channel);
        self.inner.timer_armed();
    }

    /// When a wait for I/O that starts now ends without a result:
    /// `SILT_IO_TIMEOUT` from now, if it is set.
    pub(crate) fn io_deadline(&self) -> Option<Duration> {
        self.inner
            .global_io_timeout
            .and_then(|timeout| self.inner.io.deadline_after(timeout))
    }

    /// The calling thread starts to run silt code of its own: it counts
    /// as a task until the guard is dropped. Nothing changes for a
    /// thread that counts already.
    pub(crate) fn enter(&self) -> Running {
        let counted = (!COUNTED.with(|counted| counted.replace(true))).then(|| {
            let group = self.inner.group(self.inner.current_owner());
            group.live.fetch_add(1, Ordering::SeqCst);
            group
        });
        Running {
            inner: self.inner.clone(),
            counted,
        }
    }

    /// The owner tag of the tasks spawned from now on outside any task
    /// (by the thread that runs the program), and, if it is not 0,
    /// that of the tasks that count with them. See
    /// [`Vm::set_task_owner`] and [`Vm::set_task_owner_within`].
    pub(crate) fn set_owner(&self, owner: u64, outer: u64) {
        self.inner.outer.store(outer, Ordering::SeqCst);
        self.inner.owner.store(owner, Ordering::SeqCst);
    }

    /// The owner tag of a task spawned here and now.
    pub(crate) fn current_owner(&self) -> u64 {
        self.inner.current_owner()
    }

    /// A task or an I/O operation starts: from now on a thread other
    /// than the program's own may run code that reads the environment
    /// (a task's builtins, a name lookup of the I/O pool). It stays so
    /// for as long as the scheduler lives: a REPL session's, a
    /// program's.
    pub(crate) fn begin(&self) {
        self.inner.begun.store(true, Ordering::SeqCst);
    }

    /// Whether a task or an I/O operation was ever started
    /// ([`Scheduler::begin`]).
    pub(crate) fn begun(&self) -> bool {
        self.inner.begun.load(Ordering::SeqCst)
    }

    /// See [`External`]. It counts for the owner of the code that asks.
    pub(crate) fn external(&self) -> External {
        let group = self.inner.group(self.inner.current_owner());
        group.external.fetch_add(1, Ordering::SeqCst);
        External(self.inner.clone(), group)
    }

    /// The program's own code has returned: wait until the program has
    /// ended. That is when none of its tasks can go on: each of them
    /// has ended or waits, and no sleep, timeout or I/O operation of theirs
    /// is pending. The tasks that still wait then are dropped; they
    /// could never be woken. Called by the thread that ran the code,
    /// for the owner it ran it for (`Vm::set_task_owner`): a test waits
    /// for its own tasks only.
    pub(crate) fn settle(&self) {
        let _ = self.wait_for(MainWaits::Settling);
        // What still waits is dropped, in one act: dropping a task can
        // wake another (a stage that is dropped closes its output, and
        // whoever reads that wakes), and the program has ended, so
        // that one runs no further either.
        self.stop_tasks();
    }

    /// The program's own code has failed: its tasks are stopped where
    /// they are. One that waits is dropped; one that runs ends with
    /// the slice it is in; none starts a wait or another slice.
    /// Returns when all have ended. What they had failed with by then
    /// stays to be reported; I/O that they started is not waited for.
    /// Called like [`Scheduler::settle`], for the same owner.
    pub(crate) fn stop_tasks(&self) {
        let group = self.inner.group(self.inner.current_owner());
        // The flag first: a task on its way into a wait reads it.
        group.stopped.store(true, Ordering::SeqCst);
        self.drop_waiting(&group);
        let _ = self.wait_for(MainWaits::Stopping);
        group.stopped.store(false, Ordering::SeqCst);
    }

    /// Drop the tasks of `group` that wait.
    fn drop_waiting(&self, group: &Group) {
        let inner = &self.inner;
        let left: Vec<TaskId> = inner.parking.inspect(|waiting| {
            let of_owner =
                |waiting: &&Waiting<'_, Sleeper>| owner_of(waiting.sleeper) == group.owner;
            waiting.iter().filter(of_owner).map(|w| w.task).collect()
        });
        for task in left {
            inner.parking.cancel(task);
        }
    }

    /// Wait until the tasks of the calling thread's owner have come to
    /// a stop, as [`Scheduler::settle`] does, and leave those that
    /// wait where they are: code that the thread runs next may wake
    /// them.
    pub(crate) fn wait_until_idle(&self) {
        let _ = self.wait_for(MainWaits::Settling);
    }

    /// The calling thread, the program's own, waits for its tasks:
    /// `waits` says for what. Gives the owner's tasks.
    fn wait_for(&self, waits: fn(Arc<ThreadPark>) -> MainWaits) -> Arc<Group> {
        let inner = &self.inner;
        let group = inner.group(inner.current_owner());
        let park = Arc::new(ThreadPark {
            state: Mutex::default(),
            wake: Condvar::new(),
            task: None,
            group: group.clone(),
        });
        let main = Main::of_this_thread(inner, group.clone(), waits(park.clone()));
        *inner.main.lock() = Some(main);
        inner.check(&group);
        inner.waiting_threads.lock().push(park.clone());
        let _ = inner.sleep_thread(&park);
        inner
            .waiting_threads
            .lock()
            .retain(|other| !Arc::ptr_eq(other, &park));
        *inner.main.lock() = None;
        // The wait is over too when a thread of the runtime panicked.
        inner.give_panic_on();
        group
    }

    /// Send `value` on `channel` from a thread, which waits while there
    /// is no receiver and no room. `false` when the channel is closed,
    /// or the program has ended: the value was not sent.
    #[cfg(feature = "postgres")]
    pub(crate) fn send_wait(&self, channel: &Arc<Channel>, value: Value) -> bool {
        let value = match channel.try_send(value, self.wake()) {
            sync::TrySend::Sent => return true,
            sync::TrySend::Closed(_) => return false,
            sync::TrySend::Full(value) => value,
        };
        let wait = Wait::new(vec![sync::Arm::Send(channel.clone(), value)]);
        matches!(
            self.block_thread(wait, Blocks::Thread),
            Ok(Fired::Arm(_, sync::Outcome::Sent))
        )
    }

    /// The calling thread waits for `wait` and gets how it ended. It
    /// counts as a task while it waits, if it does not already (a
    /// thread after [`Scheduler::enter`], a worker in a slice). `who`
    /// says whether the thread is the program's own: that one gets
    /// the error when the program is deadlocked. The other error is
    /// that the program has ended.
    pub(crate) fn block_thread(&self, wait: Wait, who: Blocks) -> Result<Fired, VmError> {
        let main = matches!(who, Blocks::Program);
        let inner = &self.inner;
        let _counted = self.enter();
        let group = inner.group(inner.current_owner());
        // The names of spawned tasks count up from 0.
        let id = TaskId(u64::MAX - inner.next_thread_wait.fetch_add(1, Ordering::Relaxed));
        if main {
            let waits = MainWaits::Blocked(id);
            *inner.main.lock() = Some(Main::of_this_thread(inner, group.clone(), waits));
        }
        let timed = wait.deadline.is_some();
        let park = Arc::new(ThreadPark {
            state: Mutex::default(),
            wake: Condvar::new(),
            task: RUNNING_TASK.with(|task| task.get()),
            group: group.clone(),
        });
        let sleeper = Sleeper::Thread(park.clone());
        // Counted as on its way into the wait until the wait has ended
        // (`ready`: not as late as when the thread has woken), or was
        // over at once.
        group.parking.fetch_add(1, Ordering::SeqCst);
        // (A thread of a program that has exited begins no wait.)
        let exited = || inner.exit.get().is_some();
        let resumed = match inner.parking.park(id, sleeper, wait, exited) {
            Some((_, resumed)) => {
                group.parking.fetch_sub(1, Ordering::SeqCst);
                resumed
            }
            None => {
                if timed {
                    inner.timer_armed();
                }
                inner.check(&group);
                inner.wait_thread(&park)
            }
        };
        // The verdict is the program's thread's, and so is the panic
        // of a thread of the runtime, which ended the wait of every
        // thread: the others get the error of a program that is gone.
        let verdict = if main {
            *inner.main.lock() = None;
            inner.give_panic_on();
            inner.deadlock.lock().take()
        } else {
            None
        };
        match resumed {
            Resumed::Fired(fired) => Ok(fired),
            // (No thread has a deadline of its task: only a spawned
            // task's wait expires.)
            Resumed::Cancelled | Resumed::Expired => Err(match verdict {
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

impl Scheduler {
    /// The program has called `process.exit(code)`, and that ends the
    /// run ([`crate::vm::Exit::EndsRun`]). The first call's code is the
    /// run's; a later one changes nothing. From here on no task runs
    /// another slice or begins a wait, every wait of the program ends,
    /// the threads that wait are told, and no failure of a task is
    /// recorded or reported: the program's own thread ends with the
    /// error of the exit (`Vm::run`).
    pub(crate) fn exit(&self, code: u8) {
        let inner = &self.inner;
        // The code first: a task or a thread on its way into a wait
        // reads it, and is refused the wait.
        if inner.exit.set(code).is_err() {
            return;
        }
        let waiting: Vec<TaskId> =
            (inner.parking).inspect(|waiting| waiting.iter().map(|w| w.task).collect());
        for task in waiting {
            inner.parking.cancel(task);
        }
        inner.poke_threads();
    }

    /// The code of the `process.exit` that ended the run, once there
    /// has been one ([`Scheduler::exit`]).
    pub(crate) fn exit_code(&self) -> Option<u8> {
        self.inner.exit.get().copied()
    }

    /// If a thread of the runtime has panicked, the panic goes on
    /// here, on the calling thread: the program's own, between two
    /// slices of its code, where no catch of a builtin's panic is
    /// around it. See [`Inner::fail`].
    pub(crate) fn give_panic_on(&self) {
        self.inner.give_panic_on();
    }

    /// Whether a thread of the runtime has panicked ([`Inner::fail`]).
    pub(crate) fn is_broken(&self) -> bool {
        self.inner.broken.load(Ordering::SeqCst)
    }

    /// What a thread of the runtime that is not the scheduler's own
    /// raises when a panic has escaped to its top.
    pub(crate) fn alarm(&self) -> Alarm {
        Alarm(Arc::downgrade(&self.inner))
    }

    /// Test-only: the runtime panics once at `at`, beside the places
    /// it was told before.
    #[cfg(any(test, feature = "test-hooks"))]
    pub(crate) fn panic_for_test(&self, at: PanicAt) {
        match at {
            PanicAt::Builtin => (self.inner.panic_in_a_builtin).store(true, Ordering::SeqCst),
            PanicAt::BuiltinOfATask => {
                (self.inner.panic_in_a_builtin_of_a_task).store(true, Ordering::SeqCst)
            }
            at => {
                // A deadline that has passed when its wait begins is
                // fired by the timer too: the panic does not depend on
                // how quickly a short wait began.
                if at == PanicAt::Timer {
                    self.inner.parking.timer().fire_every_deadline();
                }
                self.inner.panic_at.lock().push(at)
            }
        }
    }

    /// Test-only: panic on the calling thread, which is inside the
    /// call of a builtin, if a test asked for it
    /// ([`PanicAt::Builtin`]).
    #[cfg(any(test, feature = "test-hooks"))]
    pub(crate) fn panic_in_a_builtin_for_test(&self) {
        let asked = &self.inner.panic_in_a_builtin;
        if asked.load(Ordering::Relaxed) && asked.swap(false, Ordering::SeqCst) {
            panic!("a panic made for a test: {:?}", PanicAt::Builtin);
        }
        let asked = &self.inner.panic_in_a_builtin_of_a_task;
        if asked.load(Ordering::Relaxed)
            && thread::current().name() == Some("silt-worker")
            && asked.swap(false, Ordering::SeqCst)
        {
            panic!("a panic made for a test: {:?}", PanicAt::BuiltinOfATask);
        }
    }

    /// Test-only: panic on the calling thread if it is unwinding from
    /// a panic and a test asked for a second one
    /// ([`PanicAt::Unwinding`]).
    #[cfg(any(test, feature = "test-hooks"))]
    pub(crate) fn panic_while_unwinding_for_test(&self) {
        (self.inner).panic_for_test(|at| at == PanicAt::Unwinding && thread::panicking());
    }

    /// Test-only: panic on the calling thread, the program's own, if
    /// a test asked for it ([`PanicAt::Program`]).
    #[cfg(any(test, feature = "test-hooks"))]
    pub(crate) fn panic_of_the_program_for_test(&self) {
        self.inner.panic_for_test(|at| at == PanicAt::Program);
    }
}

thread_local! {
    /// Whether a panic raised on this thread, outside every catch of
    /// a call's panic, is one of silt's own code: the command's own
    /// thread ([`exit_on_panic`]) and the threads of a runtime (a
    /// worker, the timer's thread, a thread of the I/O pool;
    /// [`a_thread_of_the_runtime`]).
    static ITS_PANICS_ARE_SILTS: Cell<bool> = const { Cell::new(false) };
    /// How many catches that make a panic the error of a call
    /// ([`catch_as_error`]) are around what this thread runs now, and
    /// calls that a panic is to unwind ([`unwinding`]).
    static CATCHES: Cell<u32> = const { Cell::new(0) };
}

/// The calling thread is a thread of a runtime: a panic that nothing
/// catches as the error of a call is the runtime's own. Each such
/// thread says so first thing.
pub(crate) fn a_thread_of_the_runtime() {
    ITS_PANICS_ARE_SILTS.with(|silts| silts.set(true));
}

/// `catch_unwind` where a panic is the error of what was called, and
/// the thread goes on: a builtin, a host function, an I/O operation,
/// the host's clock or output, a request of the language server. In
/// silt's own process the hook leaves a panic raised inside `call` to
/// this catch ([`exit_on_panic`]); a panic raised anywhere else on a
/// thread of silt's is the runtime's own and ends the process there.
#[inline]
pub(crate) fn catch_as_error<R>(call: impl FnOnce() -> R + UnwindSafe) -> thread::Result<R> {
    let around = CATCHES.with(|catches| catches.replace(catches.get() + 1));
    let caught = catch_unwind(call);
    CATCHES.with(|catches| catches.set(around));
    caught
}

/// Run `call` so that a panic raised inside it unwinds the calling
/// thread, as every panic does without the hook of [`exit_on_panic`]:
/// what the unwinding puts back is put back (the terminal, which the
/// REPL's line editor holds while it reads a line). On the command's
/// own thread the panic then reaches the top of the thread and ends
/// the process there ([`exit_with_internal_error`]).
#[cfg(feature = "repl")]
pub(crate) fn unwinding<R>(call: impl FnOnce() -> R) -> R {
    struct Around(u32);
    impl Drop for Around {
        fn drop(&mut self) {
            let _ = CATCHES.try_with(|catches| catches.set(self.0));
        }
    }
    let _around = Around(CATCHES.with(|catches| catches.replace(catches.get() + 1)));
    call()
}

/// The process is silt's own, not an embedder's, and the calling
/// thread is the command's own. From now on a panic of silt's own code
/// ends the process at once, where it is raised and on the thread
/// that raised it: Rust's lines for the panic, the internal-error
/// line, status 101 (`InternalError::end`). Such a panic is one
/// raised on the calling thread or on a thread of a runtime, outside
/// every catch that makes a panic the error of a call
/// (`catch_as_error`) and outside what is run to unwind
/// (`unwinding`). The `silt` command calls this first thing on
/// its thread. Nothing else does, and nothing an embedder does by
/// using the library sets it: without it the panic is caught at the
/// top of the runtime's thread, comes out of the embedder's call of
/// the VM, and its process lives.
///
/// It is done in Rust's panic hook, which runs before the panicking
/// thread unwinds: the process's end is decided at the moment of the
/// panic, not when the panic has reached the top of its thread, and
/// no other thread can end the process with a status of its own in
/// between. Rust's lines for a panic are written under one lock, and
/// only while the process is not ending:
///
/// - a panic of silt's own code that is the first to end the process
///   writes them, then the internal-error line;
/// - a panic of silt's own code that comes when the process is
///   already ending, with a status or with the internal error, writes
///   nothing, and its thread waits for the end;
/// - any other panic (the error of a call, or a thread that is not
///   silt's) writes them as it always did and goes on to its catch,
///   but writes nothing once the process is ending with the internal
///   error: nothing follows that line.
///
/// So a process has Rust's lines for a panic of silt's own code, the
/// internal-error line and the status 101 all together or none of
/// them. (A stdout whose reader has gone does not kill a process that
/// is ending so: `no_death_by_a_closed_stdout`.)
pub fn exit_on_panic() {
    ITS_PANICS_ARE_SILTS.with(|silts| silts.set(true));
    static HOOKED: Once = Once::new();
    HOOKED.call_once(|| {
        let rusts = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic| at_a_panic(&*rusts, panic)));
    });
}

/// The lock under which Rust's lines for a panic and the reports of
/// the command ([`report`]) are written: the lines of two panics do
/// not mix, and neither a line of another panic nor a report comes
/// after those of the panic that ends the process. (A thread may take
/// it again: a write of a report can panic.)
static PANIC_LINES: ReentrantMutex<()> = ReentrantMutex::new(());

/// How long the process waits for Rust's lines of the panic that ends
/// it. They go to stderr, which can be held up without end (a pipe
/// that is full and that nobody reads), and with a backtrace they
/// take a while. When it runs out the last words are said without the
/// rest of them, by another thread ([`InternalError::end`]). A stderr
/// that is a file is not held up for ever, only slow at times: it is
/// waited for without a bound ([`stderr_is_a_file`]).
const RUSTS_LINES: Duration = Duration::from_secs(10);

/// Whether the process's stderr is a file. A write to a file can be
/// slow (a disk that is busy, a backtrace that takes a while to make)
/// but always ends, unlike a write to a pipe or a terminal that nobody
/// reads: the process waits for Rust's lines and the internal-error
/// line on it without a bound, and they are not lost to a slow
/// moment. (They were: the bounds ran out on a busy machine, and the
/// process ended with 101 and without the line.)
fn stderr_is_a_file() -> bool {
    #[cfg(unix)]
    let stderr = std::os::fd::AsFd::as_fd(&std::io::stderr()).try_clone_to_owned();
    #[cfg(windows)]
    let stderr = std::os::windows::io::AsHandle::as_handle(&std::io::stderr()).try_clone_to_owned();
    #[cfg(any(unix, windows))]
    return stderr
        .map(std::fs::File::from)
        .and_then(|file| file.metadata())
        .is_ok_and(|metadata| metadata.is_file());
    #[cfg(not(any(unix, windows)))]
    false
}

/// The panic hook of silt's own process ([`exit_on_panic`]); `rusts`
/// is the hook that was there before, which writes Rust's lines.
fn at_a_panic(rusts: &(dyn Fn(&PanicHookInfo<'_>) + Sync + Send), panic: &PanicHookInfo<'_>) {
    // (A thread whose locals are gone is ending: not one of silt's.)
    let silts = ITS_PANICS_ARE_SILTS.try_with(Cell::get).unwrap_or(false)
        && CATCHES.try_with(Cell::get).unwrap_or(0) == 0;
    if !silts {
        let _lines = PANIC_LINES.lock();
        if ENDING.load(Ordering::SeqCst) != WITH_INTERNAL_ERROR {
            rusts(panic);
        }
        return;
    }
    // The state first, before anything is waited for: from here on no
    // other thread ends the process.
    let first = ENDING.compare_exchange(
        RUNNING,
        WITH_INTERNAL_ERROR,
        Ordering::SeqCst,
        Ordering::SeqCst,
    );
    if first.is_err() {
        loop {
            thread::park();
        }
    }
    no_death_by_a_closed_stdout();
    let text = panic.payload_as_str().unwrap_or(crate::vm::NO_PANIC_TEXT);
    // (If no thread can be started, nothing bounds the wait.)
    if !stderr_is_a_file() {
        let _ = thread::Builder::new().name("silt-exit".into()).spawn({
            let text = text.to_string();
            move || {
                thread::sleep(RUSTS_LINES);
                InternalError(()).end(&text)
            }
        });
    }
    let lines = PANIC_LINES.lock();
    rusts(panic);
    drop(lines);
    InternalError(()).end(text)
}

/// The process is ending with the internal error: from here on a
/// write to a stdout that its reader has closed is an error of the
/// write, and no signal that kills the process. (The `silt` command
/// sets SIGPIPE to its default, so that a program whose reader has
/// gone dies quietly; this process is to end with the internal-error
/// line and status 101, and its last words flush stdout.)
///
/// The disposition is set under the lock of the command's reports, which
/// a write to stderr holds while it ignores the signal for itself
/// (`vm::io::without_sigpipe`): that write puts back what it found,
/// and must not put back the default over this. The lock is waited for
/// no longer than the last words: a write held up by a full pipe keeps
/// it.
fn no_death_by_a_closed_stdout() {
    let _lines = PANIC_LINES.try_lock_for(LAST_WORDS);
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
}

/// The calling thread has the state for the internal error: it ends
/// the process ([`InternalError::end`]).
struct InternalError(());

/// How the process ends, from the moment a thread of silt sets out to
/// end it: with a status of the program's or the command's
/// ([`exit_with_status`]), or with the internal error
/// ([`exit_with_internal_error`]). Whoever changes it from `RUNNING`
/// ends the process; every other thread that comes to end it waits
/// for that. So the internal-error line and the status 101 go
/// together: a process never has one without the other.
static ENDING: AtomicU8 = AtomicU8::new(RUNNING);
const RUNNING: u8 = 0;
const WITH_STATUS: u8 = 1;
const WITH_INTERNAL_ERROR: u8 = 2;

/// The calling thread sets out to end the process as `how`. If
/// another thread has set out before it, this one waits for the
/// process to end and never returns.
fn end_as(how: u8) {
    let first = ENDING.compare_exchange(RUNNING, how, Ordering::SeqCst, Ordering::SeqCst);
    if first.is_err() {
        loop {
            thread::park();
        }
    }
}

/// End the process with `status`: how the `silt` command ends once a
/// program may have started threads (after a run, after the tests, on
/// a closed stdout). If a thread of a runtime has panicked and is
/// ending the process with the internal error, the process ends with
/// that, not with `status`.
///
/// A panic of silt's own code that comes after this was called writes
/// nothing, and the process ends with `status` ([`exit_on_panic`]).
pub fn exit_with_status(status: i32) -> ! {
    end_as(WITH_STATUS);
    std::process::exit(status)
}

/// Which of two ends of a program run by the `silt` command came first:
/// its failure (a runtime error of `main`, a `main` that returned `Err`,
/// tasks that failed and that nobody joined), which is reported and
/// ends the process with 1, or a `process.exit`, which reports nothing
/// and ends it with its code. The one that comes first is the end: a
/// report never stands beside the code of an exit, and an exit that
/// comes after the failure is ignored.
static PROGRAM_END: AtomicU8 = AtomicU8::new(OPEN);
const OPEN: u8 = 0;
const FAILED: u8 = 1;
const EXITED: u8 = 2;

/// The program run by the `silt` command has failed, and the command
/// is about to report it and end the process with 1: called before
/// anything of the report is made. From here on a `process.exit` of the
/// program is ignored: the thread that calls it waits for the end. If
/// the program has called `process.exit` first, the calling thread
/// waits for that to end the process, and never returns.
pub fn claim_program_failure() {
    let first = PROGRAM_END.compare_exchange(OPEN, FAILED, Ordering::SeqCst, Ordering::SeqCst);
    if first == Err(EXITED) {
        loop {
            thread::park();
        }
    }
}

/// End the process with `code`, from whichever thread of a program
/// calls `process.exit(code)` under `silt run`
/// ([`crate::vm::Exit::EndsProcess`]): no task is waited for, and
/// nothing is reported. If the program has failed first
/// ([`claim_program_failure`]), or another exit came first, the
/// calling thread waits for that end instead.
///
/// What the program printed and stdout still holds is written out
/// first, all of it, waiting for the reader as a normal end does.
/// stdout's lock is taken for that and kept until the process is gone,
/// so nothing is printed after the exit. Only the wait for the lock is
/// bounded: another task's write that holds it, held up by a full
/// pipe, is waited for no longer than the last words of an internal
/// error ([`LAST_WORDS`]), and nothing is flushed then. (stderr holds
/// nothing: it is not buffered.)
pub(crate) fn exit_from_the_program(code: u8) -> ! {
    use std::io::Write;
    let first = PROGRAM_END.compare_exchange(OPEN, EXITED, Ordering::SeqCst, Ordering::SeqCst);
    if first.is_err() {
        loop {
            thread::park();
        }
    }
    end_as(WITH_STATUS);
    let (locked, has_lock) = std::sync::mpsc::channel();
    let (done, flushed) = std::sync::mpsc::channel();
    let flushes = move || {
        let mut stdout = std::io::stdout().lock();
        let _ = locked.send(());
        let _ = stdout.flush();
        let _ = done.send(());
        loop {
            thread::park();
        }
    };
    // (No thread: std's clean-up at the exit flushes stdout if its
    // lock is free.)
    if thread::Builder::new()
        .name("silt-exit".into())
        .spawn(flushes)
        .is_ok()
        && has_lock.recv_timeout(LAST_WORDS).is_ok()
    {
        let _ = flushed.recv();
    }
    std::process::exit(i32::from(code))
}

/// Run `write`, a write to the process's stderr for the program, under
/// the lock of the command's reports ([`report`]): no report is
/// written while it runs. (The lock is reentrant: a report may make
/// such a write.)
#[cfg(unix)]
pub(crate) fn with_report_lock<R>(write: impl FnOnce() -> R) -> R {
    let _lines = PANIC_LINES.lock();
    write()
}

/// [`report`] of `text` on the process's stderr, a write that is
/// dropped where stderr's reader has gone, as the program's writes to
/// stderr are: the report of a failed program under `silt run`, which
/// a closed stderr must not turn into the death of a closed pipe
/// (status 141).
pub fn report_on_stderr(text: &str) {
    report(|| {
        let _ = crate::vm::write_stderr(text);
    });
}

/// Write a report of the `silt` command's own with `write`, while a
/// program's threads may exist: a program's runtime error, the tasks
/// that failed and were never joined, a test's result, the errors of
/// a REPL entry. If the process is ending with the internal error
/// nothing is written, and the calling thread waits for the end and
/// never returns: what a thread of a broken runtime was told is not
/// the program's error, and nothing of the command's follows the
/// internal-error line.
///
/// The look and the write are one step, under the lock of the panics'
/// lines. A panic of silt's own code that comes while a report is
/// being written takes the state at once and its lines wait for the
/// report: the report stands whole before them. (They were two steps,
/// and a panic between them put the report after the line.)
///
/// `write` only writes. The text is made before the call: making it
/// can run code of the program (a `Display` of its own), and a report
/// that is held up holds up the lines of a panic, for as long as the
/// process waits for them.
pub fn report(write: impl FnOnce()) {
    let lines = PANIC_LINES.lock();
    if ENDING.load(Ordering::SeqCst) == WITH_INTERNAL_ERROR {
        // (Without the lock: the panic that ends the process writes
        // its lines under it.)
        drop(lines);
        loop {
            thread::park();
        }
    }
    write();
}

/// End the process after a panic of silt's own code, which `payload`
/// was raised with: one line on stderr says that this is a bug in
/// silt (Rust has written the panic's thread, place and message when
/// it happened), what the program had printed is flushed, and the
/// status is 101, as Rust's own for a panic.
///
/// For the `silt` command, which calls it for a panic that reached the
/// top of its own thread. After [`exit_on_panic`] that is a panic
/// which did not end the process where it was raised: one raised on a
/// thread that is not silt's and passed on to the command's (the
/// formatter's), or one that was to unwind (`unwinding`). If the
/// process is already ending nothing is written, and it ends as it
/// was going to.
pub fn exit_with_internal_error(payload: &Box<dyn Any + Send>) -> ! {
    end_as(WITH_INTERNAL_ERROR);
    no_death_by_a_closed_stdout();
    // A panic that is writing Rust's lines ends them first, if it does
    // so in time: nothing follows the internal-error line.
    match stderr_is_a_file() {
        true => drop(PANIC_LINES.lock()),
        false => drop(PANIC_LINES.try_lock_for(LAST_WORDS)),
    }
    InternalError(()).end(crate::vm::panic_message(payload))
}

/// How long the thread that ends the process waits for the
/// internal-error line and the flush of stdout: both can be held up
/// without end by the program (a thread of it blocked in a write to a
/// pipe that nobody reads holds stdout's lock). When it runs out, what
/// is lost is the last text the program printed without a line end,
/// or, where stderr is what is blocked, the line itself. (A stderr that
/// is a file is waited for without a bound: [`stderr_is_a_file`].)
const LAST_WORDS: Duration = Duration::from_secs(1);

/// Set by the thread that says the last words: there is one
/// ([`InternalError::end`]).
static LAST_WORDS_BEGUN: AtomicBool = AtomicBool::new(false);

impl InternalError {
    /// The last words and the exit: see [`exit_with_internal_error`].
    /// One thread says them; another that comes here (the one that
    /// waited for Rust's lines in the place of the panicking thread,
    /// or that thread after all) waits for the end.
    fn end(self, text: &str) -> ! {
        use std::io::Write;
        if LAST_WORDS_BEGUN.swap(true, Ordering::SeqCst) {
            loop {
                thread::park();
            }
        }
        let line = format!(
            "silt: internal error: {text}. This is a bug in silt, not in your program: please \
             report it.\n"
        );
        // The thread that ends the process waits for nothing of the
        // program's without a bound: the line and the flush are each
        // another thread's, since stderr and stdout can each be held
        // up without end. (`exit` runs no destructor, so nobody else
        // flushes.)
        let (done, waits) = std::sync::mpsc::channel();
        let (line_said, line_waits) = std::sync::mpsc::channel::<()>();
        let says = {
            let (line, done) = (line.clone(), done.clone());
            move || {
                let _ = std::io::stderr().write_all(line.as_bytes());
                drop(line_said);
                let _ = done.send(());
            }
        };
        // The flush keeps stdout's lock until the process is gone:
        // std's clean-up at the exit flushes stdout itself if it gets
        // the lock, and that flush would wait for a stdout that is
        // held up. The program's text comes after the line, if the
        // line is written within half the time.
        let flushes = move || {
            let mut stdout = std::io::stdout().lock();
            let _ = line_waits.recv_timeout(LAST_WORDS / 2);
            let _ = stdout.flush();
            let _ = done.send(());
            loop {
                thread::park();
            }
        };
        let exit_thread = || thread::Builder::new().name("silt-exit".into());
        let mut awaited = 0;
        if stderr_is_a_file() {
            // Waited for without a bound: the line from here.
            says();
            awaited += 1;
        } else {
            match exit_thread().spawn(says) {
                Ok(_) => awaited += 1,
                // No thread: the line from here.
                Err(_) => {
                    let _ = std::io::stderr().write_all(line.as_bytes());
                }
            }
        }
        // (No thread: nothing is flushed.)
        if exit_thread().spawn(flushes).is_ok() {
            awaited += 1;
        }
        let until = std::time::Instant::now() + LAST_WORDS;
        for _ in 0..awaited {
            let left = until.saturating_duration_since(std::time::Instant::now());
            if waits.recv_timeout(left).is_err() {
                break;
            }
        }
        // The exit itself does not wait for a writer either: std's
        // clean-up of stdout takes its lock only if the lock is free,
        // and the thread that flushes, or a thread of the program that
        // is blocked in a write, holds it.
        std::process::exit(101)
    }
}

/// How a thread of the runtime that the scheduler did not start (a
/// thread of the I/O pool) says that a panic escaped to its top
/// ([`Inner::fail`]). It does not keep the scheduler alive: when the
/// program is gone there is nobody to give the panic to, and it is
/// dropped.
pub(crate) struct Alarm(Weak<Inner>);

impl Alarm {
    pub(crate) fn raise(&self, payload: Box<dyn Any + Send>) {
        if let Some(inner) = self.0.upgrade() {
            inner.fail(payload);
        }
    }
}

impl Inner {
    /// A panic escaped to the top of a thread of the runtime: a
    /// worker, the timer's thread, a thread of the I/O pool. (A panic
    /// inside a builtin, a host function or an I/O operation never
    /// comes here: it is caught at the call and is the call's error.)
    /// The state of the runtime is not to be trusted from here on (a
    /// queue or a wait may be half updated, a task is lost), so the
    /// program ends: no worker takes another task, and the program's
    /// own thread gives the panic on ([`Inner::give_panic_on`]), now
    /// if it waits, else after the slice it is running or at its next
    /// call of the VM.
    ///
    /// The thread that calls this has stopped using the runtime. What
    /// is touched here is the panic's own slot and flag, and the list
    /// of the threads that wait with the two flags of each: a list and
    /// flags, which no panic leaves half written, behind locks that a
    /// panic releases and does not poison.
    ///
    /// No panic comes here in silt's own process: there it has ended
    /// the process where it was raised ([`exit_on_panic`]).
    fn fail(&self, payload: Box<dyn Any + Send>) {
        let first = {
            let mut panic = self.panic.lock();
            // The first panic is the one that is reported; what
            // follows it on a runtime that is broken says nothing new.
            let first = !self.broken.swap(true, Ordering::SeqCst);
            if first {
                panic.text = crate::vm::panic_message(&payload).to_string();
                panic.payload = Some(payload);
            }
            first
        };
        if first {
            self.poke_threads();
        }
    }

    /// See [`Scheduler::give_panic_on`]. The thread that comes first
    /// resumes the panic itself; a call that comes later panics with
    /// its message.
    fn give_panic_on(&self) {
        if !self.broken.load(Ordering::SeqCst) {
            return;
        }
        let mut panic = self.panic.lock();
        let payload = panic.payload.take();
        let text = panic.text.clone();
        drop(panic);
        match payload {
            Some(payload) => resume_unwind(payload),
            None => panic!("a thread of this VM's runtime has panicked: {text}"),
        }
    }

    /// Whether the workers and the timer's thread are to end: the
    /// program is gone, or a thread of the runtime has panicked.
    fn ending(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst) || self.broken.load(Ordering::SeqCst)
    }

    /// Test-only: panic if a test asked for a panic at a place that
    /// `here` says this is.
    #[cfg(any(test, feature = "test-hooks"))]
    fn panic_for_test(&self, here: impl Fn(PanicAt) -> bool) {
        let mut at = self.panic_at.lock();
        if let Some(asked) = at.iter().position(|place| here(*place)) {
            let place = at.remove(asked);
            panic!("a panic made for a test: {place:?}");
        }
    }

    /// The owner tag of a task spawned here and now: that of the task
    /// whose slice the thread runs, or the one set for the program.
    fn current_owner(&self) -> u64 {
        RUNNING_TASK_OWNER
            .try_with(|owner| owner.get())
            .ok()
            .flatten()
            .unwrap_or_else(|| self.owner.load(Ordering::SeqCst))
    }

    /// The tasks of `owner`. A thread keeps the group it asked for
    /// last: nearly every call is for the same owner as the one
    /// before. (A group stays for as long as its scheduler.)
    fn group(&self, owner: u64) -> Arc<Group> {
        thread_local! {
            /// The scheduler ([`Inner::id`]) and the group of this
            /// thread's last call.
            static LAST: RefCell<Option<(u64, Arc<Group>)>> = const { RefCell::new(None) };
        }
        LAST.with(|last| {
            let mut last = last.borrow_mut();
            if let Some((scheduler, group)) = &*last
                && *scheduler == self.id
                && group.owner == owner
            {
                return group.clone();
            }
            let group = self.group_of(owner);
            *last = Some((self.id, group.clone()));
            group
        })
    }

    /// [`Inner::group`], looked up.
    fn group_of(&self, owner: u64) -> Arc<Group> {
        let mut groups = self.groups.lock();
        let group = groups.entry(owner).or_insert_with(|| {
            Arc::new(Group {
                owner,
                live: AtomicUsize::new(0),
                external: AtomicUsize::new(0),
                parking: AtomicUsize::new(0),
                stopped: AtomicBool::new(false),
            })
        });
        group.clone()
    }

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
    /// down, or a thread of the runtime has panicked. A worker that
    /// finds no task looks on for a moment before it sleeps: in a
    /// program whose tasks wake each other, the next one comes within
    /// microseconds, and putting a thread to sleep and waking it costs
    /// more than that.
    #[cfg(not(target_arch = "wasm32"))]
    fn next_task(&self) -> Option<Task> {
        /// How many workers look on at a time.
        const LOOKING: usize = 2;
        loop {
            {
                let mut queue = self.queue.lock();
                if self.ending() {
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
                        found = self.queued.load(Ordering::SeqCst) > 0 || self.ending();
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
            if self.ending() {
                return None;
            }
            if let Some(task) = self.pop(&mut queue) {
                return Some(task);
            }
            // Where there is no timer thread, a worker without work is
            // one of the threads that fire the timer: it sleeps until
            // the next deadline, not until there is a task. (Whoever
            // arms a deadline then tells the workers under the queue's
            // lock, so the look here does not miss it.)
            let deadline = match *self.timer_lock.lock() {
                TimerThread::Unavailable => self.parking.timer().real_wait(),
                TimerThread::Idle | TimerThread::Running => None,
            };
            queue.sleepers += 1;
            match deadline {
                None => self.work.wait(&mut queue),
                Some(wait) => {
                    let _ = self.work.wait_for(&mut queue, wait);
                }
            }
            queue.sleepers -= 1;
            drop(queue);
            if deadline.is_some() && self.parking.timer().fire_due(&self.parking) > 0 {
                self.check_all();
            }
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

    /// The task is ended by its deadline (`TaskHandle::deadline`): a
    /// wait of it ended by that time, having taken and sent nothing,
    /// or it computed on past it. It unwinds as a cancelled task does:
    /// no further instruction of it runs, and what its frames hold is
    /// given up.
    ///
    /// That comes first, and then the task's end is told: whoever
    /// waits for it (`task.deadline`) finds the function's I/O given
    /// up, a connection it was reading shut down.
    fn expire_task(&self, task: Task) {
        let Task {
            vm, handle, group, ..
        } = task;
        drop(vm);
        handle.expire(&self.parking);
        drop(handle);
        self.ended(&group);
    }

    /// Whether the time of the task is over (`TaskHandle::deadline`),
    /// asked at the start of a slice. This is where the task learns
    /// it: the handle is told (`TaskHandle::set_over`) before the
    /// slice's first instruction, and from then on the task takes
    /// nothing new (`Vm::takes_nothing_new` reads the handle's flag,
    /// not the clock). Once the time is over the clock is not asked
    /// again.
    fn over(&self, task: &Task) -> bool {
        let handle = &task.handle;
        if handle.is_over() {
            return true;
        }
        let passed = |deadline| self.parking.timer().now() >= deadline;
        let over = handle.deadline().is_some_and(passed);
        if over {
            handle.set_over();
        }
        over
    }

    /// One slice of a task whose time was over when the slice began,
    /// or that was cancelled by then: its result, and whether the work
    /// the task may still do is used up with it ([`LATE_STEPS`]).
    ///
    /// The task runs a step at a time, so that every such step is
    /// counted, whatever ends the slices the steps run in: a task
    /// that only does waits that are over at once never runs a slice
    /// out. The count is the handle's, and one for a task and the tasks
    /// of the deadlines inside it that have its time
    /// (`TaskHandle::late`): the work past a deadline is bounded in
    /// total, and a task that is past its deadline and cancelled has
    /// the one count, not two. Nothing resets it. (A slice that began
    /// in time, and before a cancel, is not counted, also when the
    /// time passed or the cancel came in it: a task that was given a
    /// value before that has the whole count to return it.)
    fn late_slice(task: &mut Task, slice: usize) -> (SliceResult, bool) {
        let mut ran = 0;
        loop {
            let result = task.vm.execute_slice(1);
            ran += 1;
            let spent = task.handle.count_late(1) >= LATE_STEPS;
            match result {
                // (A slice the task gave away is one that ran out
                // here: it has little left to give way for.)
                SliceResult::Yielded if !spent && ran < slice => {}
                result => return (result, spent),
            }
        }
    }

    /// A task has ended: it completed, failed, or was cancelled.
    fn end_task(&self, task: Task) {
        // Its frames are abandoned before it stops counting.
        let Task {
            vm, handle, group, ..
        } = task;
        drop(vm);
        drop(handle);
        self.ended(&group);
    }

    /// A task of `group` is gone: it counts no longer.
    fn ended(&self, group: &Group) {
        self.spawned.fetch_sub(1, Ordering::SeqCst);
        group.live.fetch_sub(1, Ordering::SeqCst);
        self.check(group);
    }

    /// The wait of `sleeper` has ended.
    fn ready(&self, sleeper: Sleeper, resumed: Resumed) {
        match sleeper {
            Sleeper::Task(mut task) => {
                task.group.parking.fetch_sub(1, Ordering::SeqCst);
                match resumed {
                    Resumed::Fired(fired) => {
                        task.vm.woken = Some(fired);
                        self.enqueue(task);
                    }
                    Resumed::Cancelled => self.end_task(task),
                    Resumed::Expired => self.expire_task(task),
                }
            }
            Sleeper::Thread(park) => {
                park.group.parking.fetch_sub(1, Ordering::SeqCst);
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
            // A thread of the runtime has panicked: nothing ends the
            // wait any more, and it is over (`Inner::fail` pokes the
            // threads that wait).
            if self.broken.load(Ordering::SeqCst) {
                return Resumed::Cancelled;
            }
            // Nor once the program has exited: its thread waits for
            // nothing more (`Scheduler::exit` pokes the threads).
            if self.exit.get().is_some() {
                return Resumed::Cancelled;
            }
            // What there is to do for a thread that waits.
            if !self.has_workers.load(Ordering::SeqCst) {
                let task = self.pop(&mut self.queue.lock());
                if let Some(task) = task {
                    self.run_slice(task, false);
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
                    self.check_all();
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
            // A target without threads cannot put this thread to sleep,
            // and has no other that would wake it: nothing happens
            // before the next deadline, and the clock lets the time up
            // to it pass.
            #[cfg(target_arch = "wasm32")]
            if next.is_some() {
                drop(state);
                timer.pass_to_next_deadline();
                if timer.fire_due(&self.parking) > 0 {
                    self.check_all();
                }
                continue;
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
                self.check_all();
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
            // A thread that waits, or a worker without work, may wait
            // for longer than up to the new deadline.
            _ => {
                drop(thread);
                self.poke_threads();
                drop(self.queue.lock());
                self.work.notify_all();
            }
        }
    }

    /// Run a task for one slice, and do what its end asks for.
    ///
    /// `on_worker` when a worker runs it: a task whose slice ran out
    /// then goes on where it is while no other task waits for a worker
    /// ([`Inner::keeps_on`]). A thread that runs tasks while it waits
    /// gives the task back after its slice, to look at its own wait.
    fn run_slice(self: &Arc<Self>, task: Task, on_worker: bool) {
        /// How often in a row a task whose wait was over at once goes
        /// on without giving way.
        const GO_ON: usize = 16;
        let mut task = task;
        let mut slices = 0;
        loop {
            match self.run_once(task, on_worker) {
                Some(again) => task = again,
                None => return,
            }
            slices += 1;
            if slices >= GO_ON && !(on_worker && self.keeps_on()) {
                break;
            }
        }
        self.enqueue(task);
    }

    /// Whether a task that a worker runs goes on there after a slice,
    /// instead of going through the run queue: while no other task is
    /// queued, it would be the next one taken off it, by this worker or
    /// by another one that is woken for it, on a processor whose caches
    /// do not have it. Asked again after every slice, so a task that is
    /// queued meanwhile waits no longer than a slice.
    ///
    /// The worker makes the checks that it would make looking for its
    /// next task ([`Inner::next_task`]): it stops when the scheduler
    /// shuts down or a thread of the runtime has panicked, and where
    /// there is no timer thread it fires the deadlines that are due,
    /// which may wake tasks that are then queued.
    fn keeps_on(&self) -> bool {
        if self.ending() {
            return false;
        }
        if *self.timer_lock.lock() == TimerThread::Unavailable
            && self.parking.timer().fire_due(&self.parking) > 0
        {
            self.check_all();
        }
        self.queued.load(Ordering::SeqCst) == 0
    }

    /// One slice of a task. `Some` when the task can go on at once: the
    /// slice ended in a wait that was over at once, and the task has
    /// its result; or, on a worker (`on_worker`), the slice ran out and
    /// the task keeps the worker ([`Inner::keeps_on`]).
    fn run_once(self: &Arc<Self>, mut task: Task, on_worker: bool) -> Option<Task> {
        // A task that was stopped with its owner's runs no further,
        // nor does one of a program that has exited, nor one that was
        // cancelled before its first slice.
        let cancelled = task.handle.is_cancelled();
        if task.group.stopped.load(Ordering::SeqCst)
            || self.exit.get().is_some()
            || (cancelled && !task.ran)
        {
            self.end_task(task);
            return None;
        }
        #[cfg(any(test, feature = "test-hooks"))]
        self.panic_for_test(|at| at == PanicAt::Task(task.id) && task.ran);
        // The tasks that this slice spawns belong to the owner of this
        // task. See `Vm::set_task_owner`.
        let outer = RUNNING_TASK_OWNER.with(|owner| owner.replace(Some(task.handle.owner())));
        let counted = COUNTED.with(|counted| counted.replace(true));
        let running = RUNNING_TASK.with(|running| running.replace(Some(task.id)));
        let slice = task.vm.time_slice().unwrap_or_else(time_slice);
        // A task whose time is over runs on for a counted number of
        // steps and is ended, by its deadline, where they are used up:
        // after the step that used them up, or at the entry of the
        // wait that step began, of which no arm is tried then. It is
        // never ended at the start of a slice, nor in a slice that it
        // ends by returning or failing.
        //
        // A task that was cancelled and is not waiting does the same,
        // on the same count, however it came to be here: its wait had
        // ended when the cancel came (it has what it was given to
        // use), its last slice ran out, or the cancel came while it
        // ran. It is ended as a cancelled task; and before the steps
        // are used up at its first wait that cannot end at once, which
        // is refused it below, as it is refused a task whose time is
        // over, or at the first operation that would take something
        // new, which begins a wait that nothing can end
        // (`Vm::takes_nothing_new`).
        let (result, spent) = match cancelled || self.over(&task) {
            true => Self::late_slice(&mut task, slice),
            false => (task.vm.execute_slice(slice), false),
        };
        task.ran = true;
        RUNNING_TASK.with(|was| was.set(running));
        COUNTED.with(|was| was.set(counted));
        RUNNING_TASK_OWNER.with(|owner| owner.set(outer));

        match result {
            SliceResult::Yielded | SliceResult::Blocked(_) if spent => {
                // The cancel has given the handle its result.
                match task.handle.is_cancelled() {
                    true => self.end_task(task),
                    false => self.expire_task(task),
                }
            }
            SliceResult::Yielded if on_worker && self.keeps_on() => return Some(task),
            SliceResult::Yielded => self.enqueue(task),
            SliceResult::Completed(value) => {
                task.handle.complete(Ok(value), &self.parking);
                self.end_task(task);
            }
            SliceResult::Failed(error) => {
                // A failure that no join receives is reported when the
                // program ends, so it is recorded here. `fail` returns
                // false if the task had been cancelled before: its
                // handle keeps the cancellation as its result then.
                let error = task.vm.enrich_error(error);
                if task.handle.fail(error, &self.parking) {
                    record_failed_task(self, &task.handle);
                }
                self.end_task(task);
            }
            SliceResult::Blocked(wait) => {
                // The task's own: its cancel flag and its deadline.
                // Every wait of a task comes through here, whichever
                // frame of it waits.
                let wait = wait
                    .cancel(Some(task.handle.cancel_flag()))
                    .task_deadline(task.handle.deadline());
                let timed = wait.deadline.is_some() || wait.task_deadline.is_some();
                let group = task.group.clone();
                let id = TaskId(task.id as u64);
                // Counted as on its way into the wait until the wait
                // has ended (`ready`), or was over at once.
                group.parking.fetch_add(1, Ordering::SeqCst);
                // A task that is stopped with its owner's begins no
                // wait, nor does one of a program that has exited. One
                // that is cancelled, or whose time is over, does a send
                // that is ready and is refused the wait otherwise: that
                // is the wait's own to say.
                let back = self.parking.park(id, Sleeper::Task(task), wait, || {
                    group.stopped.load(Ordering::SeqCst) || self.exit.get().is_some()
                });
                match back {
                    Some((Sleeper::Task(mut task), Resumed::Fired(fired))) => {
                        group.parking.fetch_sub(1, Ordering::SeqCst);
                        task.vm.woken = Some(fired);
                        return Some(task);
                    }
                    Some((sleeper, resumed)) => self.ready(sleeper, resumed),
                    None => {
                        if timed {
                            self.timer_armed();
                        }
                        self.check(&group);
                    }
                }
            }
        }
        None
    }

    /// The tasks of `group` may have come to a stop, each ended or
    /// waiting with nothing of theirs pending. If the program's own
    /// thread waits on them, it is told: a wait of its code fails as a
    /// deadlock; a wait for the program's end is over.
    ///
    /// Called after every event that can complete such a stop: a task
    /// of the group parks or ends, something external of it ends. The
    /// stop stays once it is there, so the last of those events sees
    /// it.
    fn check(&self, group: &Group) {
        // After a panic of the runtime the counts say nothing, and the
        // program's thread has been told.
        if self.broken.load(Ordering::SeqCst) {
            return;
        }
        // A first look without a lock.
        if group.parking.load(Ordering::SeqCst) < group.live.load(Ordering::SeqCst) {
            return;
        }
        let Some(main) = self.main.lock().clone() else {
            return;
        };
        if !main.counts(group.owner) {
            return;
        }
        // Stopped tasks have ended when none is left: whatever is
        // pending for them has nobody to wake.
        if let MainWaits::Stopping(park) = &main.waits {
            if main.group.live.load(Ordering::SeqCst) == 0 {
                park.state.lock().resumed = Some(Resumed::Cancelled);
                park.wake.notify_one();
            }
            return;
        }
        if group.external.load(Ordering::SeqCst) > 0 {
            return;
        }
        // The exact look: with the registry held, nothing parks or
        // wakes, and the counts are of the same moment.
        let stopped = self.parking.inspect(|waiting| {
            let own: Vec<&Waiting<'_, Sleeper>> = waiting
                .iter()
                .filter(|waiting| main.counts(owner_of(waiting.sleeper)))
                .collect();
            let count = |of: fn(&Group) -> &AtomicUsize| -> usize {
                main.groups().map(|g| of(g).load(Ordering::SeqCst)).sum()
            };
            // A wait that will end without anybody's doing: one with a
            // deadline, or one on a channel that a timer will close. A
            // timer whose channel nobody waits on wakes nobody.
            let will_end = |waiting: &&Waiting<'_, Sleeper>| {
                waiting.deadline.is_some() || waiting.on.iter().any(Source::closes_by_timer)
            };
            let stopped = own.len() == count(|g| &g.live)
                && !own.iter().any(will_end)
                && count(|g| &g.external) == 0;
            stopped.then(|| match &main.waits {
                MainWaits::Settling(_) | MainWaits::Stopping(_) => None,
                MainWaits::Blocked(id) => Some(verdict(&own, *id)),
            })
        });
        let Some(verdict) = stopped else {
            return;
        };
        match main.waits {
            MainWaits::Settling(park) | MainWaits::Stopping(park) => {
                park.state.lock().resumed = Some(Resumed::Cancelled);
                park.wake.notify_one();
            }
            MainWaits::Blocked(id) => {
                // `None`: the program's thread is not among those that
                // wait (its wait is over, or not yet begun).
                if let Some(verdict) = verdict.flatten() {
                    *self.deadlock.lock() = Some(verdict);
                    self.parking.cancel(id);
                }
            }
        }
    }

    /// [`Inner::check`] for every owner: after a timer has done
    /// something that woke nobody.
    fn check_all(&self) {
        let groups: Vec<Arc<Group>> = self.groups.lock().values().cloned().collect();
        for group in groups {
            self.check(&group);
        }
    }
}

/// The owner of the code that waits.
fn owner_of(sleeper: &Sleeper) -> u64 {
    match sleeper {
        Sleeper::Task(task) => task.handle.owner(),
        Sleeper::Thread(park) => park.group.owner,
    }
}

/// The deadlock error for the program's thread, whose wait is `main`,
/// from the waits of its owner's tasks. `None` if its own wait is not
/// among them.
fn verdict(own: &[&Waiting<'_, Sleeper>], main: TaskId) -> Option<VmError> {
    let mine = own.iter().find(|waiting| waiting.task == main)?;
    let what = match mine.on {
        [Source::Recv(_)] => "channel receive with no counterparty",
        [Source::Send(_)] => "channel send with no counterparty",
        [Source::Cell(_)] => "task.join with no progress possible",
        _ => "channel select with no counterparty",
    };
    let mut verdict = VmError::new(format!("deadlock on main thread: {what}"));
    for line in waits_of_the_others(own, main) {
        verdict = verdict.with_help(line);
    }
    Some(verdict)
}

/// What each parked task besides the program's own waits on, one line
/// each, for the deadlock report.
fn waits_of_the_others(own: &[&Waiting<'_, Sleeper>], main: TaskId) -> Vec<String> {
    /// How many tasks are listed.
    const LISTED: usize = 8;
    let others: Vec<&&Waiting<'_, Sleeper>> =
        own.iter().filter(|waiting| waiting.task != main).collect();
    let mut lines: Vec<String> = others
        .iter()
        .take(LISTED)
        .map(|waiting| {
            let on: Vec<String> = waiting
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
            let task = match waiting.sleeper {
                Sleeper::Task(task) => Some(task.id),
                Sleeper::Thread(park) => park.task,
            };
            let who = match task {
                Some(id) => format!("task <handle:{id}>"),
                None => "a thread of the runtime".to_string(),
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

/// How many steps a task runs before it gives way, unless
/// `SILT_TIME_SLICE` says otherwise.
const STANDARD_SLICE: usize = 2000;

/// How many steps the function of a `task.deadline` may run in all
/// after its time is over, the functions of the deadlines inside it
/// included: one standard slice, whatever the slice of the VM. It is
/// what a function that was given a value in time has to return it.
/// A task that is cancelled while it is not waiting has the same
/// steps, from the first slice that begins after the cancel: what a
/// task that was handed a value has to pass it on.
const LATE_STEPS: usize = STANDARD_SLICE;

/// How many steps a task runs before it gives way (`SILT_TIME_SLICE`).
pub(crate) fn time_slice() -> usize {
    static SLICE: OnceLock<usize> = OnceLock::new();
    *SLICE.get_or_init(|| {
        std::env::var("SILT_TIME_SLICE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(STANDARD_SLICE)
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
        #[cfg(any(test, feature = "test-hooks"))]
        inner.panic_for_test(|at| at == PanicAt::Shutdown);
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
/// A panic that escapes it, from a slice or from the scheduler's own
/// code, ends the program ([`Inner::fail`]); the worker has stopped
/// using the runtime when it says so, and ends.
#[cfg(not(target_arch = "wasm32"))]
fn worker_loop(inner: Arc<Inner>) {
    a_thread_of_the_runtime();
    let work = AssertUnwindSafe(|| {
        while let Some(task) = inner.next_task() {
            inner.run_slice(task, true);
        }
    });
    if let Err(payload) = catch_unwind(work) {
        inner.fail(payload);
    }
}

/// The thread that fires the timer: wait for the next deadline, or for
/// an earlier one to be armed, and end the waits that are due. A panic
/// that escapes it ends the program, as a worker's does.
fn timer_loop(inner: Arc<Inner>) {
    a_thread_of_the_runtime();
    if let Err(payload) = catch_unwind(AssertUnwindSafe(|| fire_timers(&inner))) {
        inner.fail(payload);
    }
}

fn fire_timers(inner: &Inner) {
    let timer = inner.parking.timer().clone();
    loop {
        {
            let mut thread = inner.timer_lock.lock();
            if inner.ending() {
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
        if inner.ending() {
            return;
        }
        #[cfg(any(test, feature = "test-hooks"))]
        inner.panic_for_test(|at| at == PanicAt::Timer);
        // A deadline that ended nobody's wait (a channel that closed
        // unheard) may have been the last thing pending.
        if timer.fire_due(&inner.parking) > 0 {
            inner.check_all();
        }
    }
}

/// Failed tasks kept for the report of failures that nobody joined, up
/// to `MAX_RECORDED_FAILURES` for each owner: the many failures of one
/// test do not push out the failure of another. Each scheduler has
/// one; while the failures are collected (`collect_unjoined_failures`),
/// the one of the process is used instead.
#[derive(Default)]
struct FailedTasks {
    /// Per owner tag, the handles of tasks that ended with an error,
    /// in the order of their failures. Whether a join has received the
    /// error since, or a cancel has dismissed it, is read from the
    /// handle at report time.
    handles: BTreeMap<u64, Vec<Arc<TaskHandle>>>,
    /// Per owner tag, the number of failed tasks that were not recorded
    /// because the owner's handles were full of failures that nobody
    /// had joined.
    not_recorded: BTreeMap<u64, usize>,
}

impl FailedTasks {
    /// Keep the handle of a task that ended with an error. Returns the
    /// handles that were let go to make room (failures that have been
    /// joined or cancelled since they were kept); the caller drops them
    /// after it has released the lock on the record.
    fn record(&mut self, handle: &Arc<TaskHandle>) -> Vec<Arc<TaskHandle>> {
        let owner = handle.owner();
        let kept = self.handles.entry(owner).or_default();
        let mut handled_since = Vec::new();
        if kept.len() >= MAX_RECORDED_FAILURES {
            let (unhandled, handled): (Vec<_>, Vec<_>) = std::mem::take(kept)
                .into_iter()
                .partition(|h| h.has_unjoined_failure());
            *kept = unhandled;
            handled_since = handled;
        }
        if kept.len() >= MAX_RECORDED_FAILURES {
            *self.not_recorded.entry(owner).or_insert(0) += 1;
        } else {
            kept.push(handle.clone());
        }
        handled_since
    }

    /// Empty the record, and return what it held.
    fn take(&mut self) -> (Vec<Arc<TaskHandle>>, BTreeMap<u64, usize>) {
        (
            std::mem::take(&mut self.handles)
                .into_values()
                .flatten()
                .collect(),
            std::mem::take(&mut self.not_recorded),
        )
    }
}

/// Keep the handle of a task that ended with an error, for the report
/// of failures that nobody joined: in the record of the process while
/// the failures are collected, in the scheduler's own record otherwise.
fn record_failed_task(inner: &Inner, handle: &Arc<TaskHandle>) {
    // A program that has exited reports no failure: which tasks had
    // failed by then is a race.
    if inner.exit.get().is_some() {
        return;
    }
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
    // (Nothing after the internal-error line: the failures of a
    // process that is ending with it stay unreported.)
    if ENDING.load(Ordering::SeqCst) == WITH_INTERNAL_ERROR {
        return 0;
    }
    let (handles, not_recorded) = inner.failed_tasks.lock().take();
    // (Nor after a `process.exit` that ended the run: what had failed
    // before it is dropped.)
    if inner.exit.get().is_some() {
        return 0;
    }
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
            stage: handle.stage(),
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

thread_local! {
    /// On a worker thread, while it runs a slice of a task: the owner
    /// tag of that task.
    static RUNNING_TASK_OWNER: Cell<Option<u64>> = const { Cell::new(None) };
}

/// Whose thread waits in [`Scheduler::block_thread`].
#[derive(Clone, Copy)]
pub(crate) enum Blocks {
    /// A thread of the runtime.
    Thread,
    /// The program's own, which is told of a deadlock.
    Program,
}

/// What the report of a failure that nobody joined advises.
const UNJOINED_FAILURE_HELP: &str = "join it with task.join to stop where you wait for it, or \
     cancel it with task.cancel if you don't need its result";

/// The first line of the report of a failure that nobody joined.
fn unjoined_failure_headline(task_id: usize, message: &str) -> String {
    format!("task <handle:{task_id}> failed and was never joined: {message}")
}

/// What the report of a stream stage's failure that nobody read advises.
const UNREAD_STAGE_FAILURE_HELP: &str =
    "read its output, where the failure is raised, or drop the stage if you don't need it";

/// The first line of the report of a stream stage's failure that
/// nobody read.
fn unread_stage_failure_headline(stage: &str, message: &str) -> String {
    format!("a `{stage}` stage failed and its output was never read: {message}")
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
    /// The owner tag the task was spawned under. See `Vm::set_task_owner`.
    pub owner: u64,
    /// The name of the stream stage the task ran (`stream.map`): its
    /// failure was in its output, which nobody read.
    pub stage: Option<&'static str>,
    /// The error the task ended with, as the task raised it.
    pub error: VmError,
}

impl UnjoinedFailure {
    /// The error to report: the task's error, with a message that names
    /// the task and a help line that says what to do. Span and call
    /// stack are the task's.
    pub fn report_error(&self) -> VmError {
        let mut error = self.error.clone();
        match self.stage {
            Some(stage) => {
                error.message = unread_stage_failure_headline(stage, &self.error.message);
                error.with_help(UNREAD_STAGE_FAILURE_HELP)
            }
            None => {
                error.message = unjoined_failure_headline(self.task_id, &self.error.message);
                error.with_help(UNJOINED_FAILURE_HELP)
            }
        }
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
/// A failure that is not taken is never reported: the front end takes
/// them when the program has ended ([`Vm::settle`]), after which no
/// task can fail.
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
                stage: handle.stage(),
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
    use std::sync::atomic::Ordering;
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

    /// The verdict is asked of the tasks of one owner. What another
    /// owner left on the same scheduler (a task asleep for an hour, a
    /// task that waits) neither delays it nor is counted in it.
    ///
    /// A thread that outlives its VM (it finishes an I/O operation
    /// that nobody waits for) and panics then has nobody to give the
    /// panic to. In a process that is not silt's own, as this one,
    /// that is all: the panic is dropped and the process lives.
    #[test]
    fn a_panic_after_the_vm_is_gone_is_dropped_in_an_embedders_process() {
        let scheduler = super::Scheduler::new(HostIo::buffer(&crate::vm::Buffer::new()));
        let alarm = scheduler.alarm();
        drop(scheduler);
        alarm.raise(Box::new("after the end"));
    }

    #[test]
    fn a_deadlock_is_asked_of_the_tasks_of_one_owner() {
        let compile = |source: &str| {
            crate::session::testing::compile_str(source).expect("the program compiles")
        };
        let leaves_tasks = compile(
            r#"
import channel
import task
import time
fn main() {
  let never = channel.new(0)
  let _asleep = task.spawn({ -> time.sleep(time.hours(1)) })
  let _waits = task.spawn({ -> channel.receive(never) })
  let ready = channel.new(0)
  let _third = task.spawn({ -> channel.send(ready, 1) })
  let _ = channel.receive(ready)
}
"#,
        );
        let deadlocked = compile(
            r#"
import channel
import task
fn main() {
  let never = channel.new(0)
  let _waits = task.spawn({ -> channel.receive(never) })
  channel.receive(never)
}
"#,
        );
        let out = Timed::default();
        let mut vm = Vm::new(HostIo::new(out.clone(), out.clone()));
        vm.set_task_owner(1);
        let first = vm.run_program(&leaves_tasks);
        vm.set_task_owner(2);
        let before = Instant::now();
        let second = vm.run_program(&deadlocked);
        let took = before.elapsed();
        assert_eq!(first.map_err(|e| e.message), Ok(Value::Unit));
        let error = second.expect_err("the second program is deadlocked");
        assert_eq!(
            error.message,
            "deadlock on main thread: channel receive with no counterparty"
        );
        // Its own task is listed, the first owner's two are not.
        assert_eq!(error.help.len(), 1, "{:?}", error.help);
        assert!(took < Duration::from_secs(30), "the verdict took {took:?}");
    }

    /// Where the timer thread cannot be started, the threads that
    /// wait and the workers without work fire the timer: sleeps,
    /// timeouts and timeout channels still end, on the program's own
    /// thread and in tasks, also while the program's thread computes
    /// and never waits, and the program ends when its tasks have.
    #[test]
    fn without_a_timer_thread_the_waiting_threads_fire_the_timer() {
        let program = crate::session::testing::compile_str(
            r#"
import channel
import task
import time

fn busy(woke) {
  match channel.try_receive(woke) {
    channel.Message(_) -> ()
    _ -> busy(woke)
  }
}

fn main() {
  -- main never waits here: the task's sleep ends all the same.
  let woke = channel.new(1)
  let _sleeper = task.spawn { ->
    time.sleep(time.ms(5))
    channel.send(woke, ())
  }
  busy(woke)
  println("a task woke while main was busy")
  time.sleep(time.ms(5))
  println("main slept")
  let ch = channel.new(0)
  let sender = task.spawn { ->
    time.sleep(time.ms(5))
    channel.send(ch, 1)
  }
  match channel.receive_timeout(ch, time.ms(20000)) {
    channel.Message(v) -> println("got {v}")
    _ -> println("timed out")
  }
  task.join(sender)
  match channel.select([channel.Recv(ch), channel.Recv(channel.timeout(time.ms(5)))]) {
    channel.Received(_, None) -> println("the timeout channel closed")
    _ -> println("something else")
  }
  println("main returns")
  let _late = task.spawn { ->
    time.sleep(time.ms(5))
    println("a task after main")
  }
}
"#,
        )
        .expect("the program compiles");
        let out = Timed::default();
        let mut vm = Vm::new(HostIo::new(out.clone(), out.clone()));
        *vm.scheduler().inner.timer_lock.lock() = super::TimerThread::Unavailable;
        let result = vm.run_program(&program);
        vm.settle();
        assert_eq!(result.map_err(|e| e.message), Ok(Value::Unit));
        let printed: Vec<String> = out.0.lock().iter().map(|(text, _)| text.clone()).collect();
        assert_eq!(
            printed.concat(),
            "a task woke while main was busy\nmain slept\ngot 1\nthe timeout channel closed\n\
             main returns\na task after main\n"
        );
        assert!(*vm.scheduler().inner.timer_lock.lock() == super::TimerThread::Unavailable);
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
import time
fn main() {
  let ch = channel.new(0)
  let timer = channel.timeout(time.ms(150))
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

    /// 20 pipelines that are cut short (`repeat |> take(3)`, and a
    /// `first` of a long range) leave no stage behind: every source is
    /// stopped when nobody reads it any more, so no task is left to
    /// use a processor or to keep a thread.
    #[test]
    fn a_truncated_pipeline_leaves_no_task() {
        let program = crate::session::testing::compile_str(
            r#"
import list
import stream
fn main() {
  let all = 1..20 |> list.map { i ->
    stream.repeat(i) |> stream.map({ x -> x + 1 }) |> stream.take(3) |> stream.collect
  }
  let _ = stream.from_range(1, 1000000) |> stream.map({ x -> x }) |> stream.first
  list.length(all)
}
"#,
        )
        .expect("the program compiles");
        let mut vm = Vm::new(HostIo::buffer(&crate::Buffer::new()));
        let result = vm.run_program(&program);
        assert!(matches!(result, Ok(Value::Int(20))), "got {result:?}");
        // The stages end on the workers, a moment after the sinks.
        let limit = Instant::now() + Duration::from_secs(10);
        let spawned = || vm.scheduler().inner.spawned.load(Ordering::SeqCst);
        while spawned() > 0 && Instant::now() < limit {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(spawned(), 0, "stages are left");
        assert_eq!(vm.scheduler().inner.parking.waiting(), 0);
    }

    #[test]
    fn a_wait_that_a_task_can_end_is_no_deadlock() {
        // The task is busy for many slices while the program waits.
        let (result, _, _) = run(r#"
import channel
import task
fn spin(n, acc) {
  match n {
    0 -> acc
    _ -> spin(n - 1, acc + 1)
  }
}
fn main() {
  let ch = channel.new(0)
  let _sends = task.spawn({ -> channel.send(ch, spin(300000, 0)) })
  when let Some(n) = channel.receive(ch) else { panic("closed") }
  n
}
"#);
        assert!(matches!(result, Ok(Value::Int(300000))), "got {result:?}");
    }

    /// As many workers as `Scheduler::ensure_workers` starts.
    fn workers() -> usize {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .max(2)
    }

    /// Wait until nothing holds the runtime any more: every worker and
    /// the timer's thread have ended, and every task is dropped. A
    /// worker that ran a task on for ever would hold it for ever.
    fn wait_for_the_end(inner: std::sync::Weak<super::Inner>) {
        while inner.strong_count() > 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// A task that computes and never waits keeps its worker while no
    /// other task is queued (`Inner::keeps_on`). It looks after every
    /// slice: a task spawned while every worker computes runs, and here
    /// it is the one that ends their work.
    #[test]
    fn a_task_that_computes_does_not_hold_back_one_spawned_after_it() {
        let (result, out, _) = run(&format!(
            r#"
import channel
import list
import task

fn busy(stop) {{
  match channel.try_receive(stop) {{
    channel.Closed -> ()
    _ -> busy(stop)
  }}
}}

fn main() {{
  let stop = channel.new(1)
  let busy_ones = loop i = 0, acc = [] {{
    match i == {workers} {{
      true -> acc
      false -> loop(i + 1, [task.spawn {{ -> busy(stop) }}, ..acc])
    }}
  }}
  let last = task.spawn {{ -> channel.close(stop) }}
  task.join(last)
  list.each(busy_ones) {{ t -> task.join(t) }}
  println("all ended")
}}
"#,
            workers = workers()
        ));
        assert_eq!(result.map_err(|e| e.message), Ok(Value::Unit));
        out.at("all ended");
    }

    /// A cancel ends a task that computes alone, on a worker it keeps:
    /// the program's receive is then a deadlock, which it is not while
    /// the task lives.
    #[test]
    fn a_cancel_ends_a_lone_task_that_never_waits() {
        let (result, _, _) = run(r#"
import channel
import task
import time

fn forever(n) {
  forever(n + 1)
}

fn main() {
  let computes = task.spawn { -> forever(0) }
  time.sleep(time.ms(5))
  task.cancel(computes)
  channel.receive(channel.new(0))
}
"#);
        assert_deadlock(result, "channel receive with no counterparty");
    }

    /// The same for the end of a `task.deadline`.
    #[test]
    fn a_deadline_ends_a_lone_task_that_never_waits() {
        let (result, out, _) = run(r#"
import channel
import task
import time

fn forever(n) {
  forever(n + 1)
}

fn main() {
  match task.deadline(time.ms(5), { -> forever(0) }) {
    Ok(_) -> println("returned")
    Err(_) -> println("over")
  }
  channel.receive(channel.new(0))
}
"#);
        assert_deadlock(result, "channel receive with no counterparty");
        out.at("over");
    }

    const COMPUTES_ALONE: &str = r#"
import task
import time

fn forever(n) {
  forever(n + 1)
}

fn main() {
  let _computes = task.spawn { -> forever(0) }
  let sleeps = task.spawn { -> time.sleep(time.ms(5)) }
  task.join(sleeps)
}
"#;

    /// The scheduler shuts down while a worker keeps a task that
    /// computes: the worker stops it and ends.
    #[test]
    fn a_shutdown_stops_a_task_that_keeps_its_worker() {
        let program = crate::session::testing::compile_str(COMPUTES_ALONE).expect("compiles");
        let mut vm = Vm::new(HostIo::buffer(&crate::vm::Buffer::new()));
        let result = vm.run_program(&program);
        assert_eq!(result.map_err(|e| e.message), Ok(Value::Unit));
        let inner = Arc::downgrade(&vm.scheduler().inner);
        drop(vm);
        wait_for_the_end(inner);
    }

    /// A thread of the runtime panics while a worker keeps a task that
    /// computes: the worker stops it and ends.
    #[test]
    fn a_panic_of_the_runtime_stops_a_task_that_keeps_its_worker() {
        let program = crate::session::testing::compile_str(COMPUTES_ALONE).expect("compiles");
        let mut vm = Vm::new(HostIo::buffer(&crate::vm::Buffer::new()));
        // The second task, as its sleep ends.
        vm.scheduler().panic_for_test(super::PanicAt::Task(1));
        let joined = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = vm.run_program(&program);
        }));
        assert!(joined.is_err(), "the join gives the panic");
        let inner = Arc::downgrade(&vm.scheduler().inner);
        drop(vm);
        wait_for_the_end(inner);
    }
}
