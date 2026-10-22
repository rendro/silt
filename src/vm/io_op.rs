//! The I/O pool, and the operations of a task on it: a blocking
//! operation runs on a thread of the pool, and the task waits for its
//! value.

use std::collections::VecDeque;
use std::sync::Arc;

use crate::runtime::sync::{Arm, Cell, Fired, Wait};
use crate::scheduler::{External, Scheduler};
use crate::value::Value;

use super::{Step, Vm, VmError};

impl Vm {
    /// The step of the I/O builtin `name` that runs the blocking
    /// operation `op` on the I/O pool and waits for its value, which
    /// is the builtin's: already `Ok(_)` or `Err(_)`.
    ///
    /// The wait ends at `SILT_IO_TIMEOUT`, if that is set, with
    /// `timeout_err`: the typed error the builtin's signature declares.
    pub(crate) fn io(
        &mut self,
        name: &'static str,
        timeout_err: ErrFactory,
        leave: Leave,
        op: impl FnOnce(&Wanted) -> Value + Send + 'static,
    ) -> Result<Step, VmError> {
        let op = self.io_op(timeout_err, leave, op);
        self.io_wait(name, op)
    }

    /// Wait for `op`, until `SILT_IO_TIMEOUT` ([`Limit::Io`]).
    pub(crate) fn io_wait(&mut self, name: &'static str, op: IoOp) -> Result<Step, VmError> {
        self.io_wait_then(name, op, |value| Ok(Step::Done(value)))
    }

    /// [`Vm::io_wait`] that goes on as `then` says with the operation's
    /// value: the builtin's value, or a frame that computes it (one
    /// that calls the program's code, as a postgres statement whose
    /// rows wait for the program's `json.Decode` impls does). An error
    /// of the program that the operation found (a value of the program
    /// it could not send, say) is `then`'s `Err`.
    pub(crate) fn io_wait_then(
        &mut self,
        name: &'static str,
        op: IoOp,
        then: impl FnOnce(Value) -> Result<Step, VmError> + Send + 'static,
    ) -> Result<Step, VmError> {
        let mut then = Some(then);
        let wait = op.wait(self, Limit::Io);
        // The frame owns the operation: when the wait is over, however
        // it ends, the operation has no waiter and no longer counts as
        // pending for the program (`IoOp`'s `Drop`).
        Ok(self.park(name, wait, move |_, fired| {
            // The value is taken only by a wait that it ended: one
            // that came as the deadline passed stays unheard.
            match (op.outcome(&fired), then.take()) {
                (Some(value), Some(then)) => then(value),
                (Some(value), None) => Ok(Step::Done(value)),
                (None, _) => Ok(Step::Done(op.value(&fired))),
            }
        }))
    }

    /// Hand the blocking operation `f` to the I/O pool
    /// ([`IoPool::submit`]): the one place where a task starts I/O. `f`
    /// is told whether its value is still wanted ([`Wanted`]); `leave`
    /// says what becomes of it when its waiter goes.
    ///
    /// A task that takes nothing new ([`Vm::takes_nothing_new`]) starts
    /// none: nothing is handed to the pool, and the operation that
    /// comes back never runs ([`IoPool::withheld`]). The wait for it,
    /// which the caller begins, ends the task.
    pub(crate) fn io_op(
        &self,
        failure: ErrFactory,
        leave: Leave,
        f: impl FnOnce(&Wanted) -> Value + Send + 'static,
    ) -> IoOp {
        if self.takes_nothing_new() {
            return self.runtime.io_pool.withheld(failure);
        }
        let task = self.cancelled.iter().chain(&self.over).cloned().collect();
        self.runtime.io_pool.submit(failure, task, leave, f)
    }
}

// ── I/O thread pool ─────────────────────────────────────────────

/// Why a timer or an I/O operation finds its threads stopped.
const VM_GONE: &str = "the VM that ran the program has been dropped";

/// How many threads the pool runs at most: how many operations can
/// be in flight at a time, since each has a thread of its own until
/// there is readiness-based I/O. An operation beyond that fails at
/// once with its module's error; it does not wait, since what it
/// would wait for may be what it was going to do (the write that
/// releases the readers). A blocked operation costs its thread's
/// kernel stack and the pages of the thread's own stack that it
/// touched, beside its task: about 25 KiB together, measured with
/// 3,000 tasks in `tcp.read` (105 MiB against 37 MiB with 300), so
/// 4,096 of them about 100 MiB.
pub(crate) const IO_POOL_THREADS_MAX: usize = 4096;

/// How many operations that cannot be interrupted may still run
/// although nobody waits for them (their waiter was cancelled, timed
/// out or dropped): a read of a FIFO or a terminal that never ends
/// stays. Beyond the bound new operations fail at once: the stuck
/// ones must not take a thread each without end.
///
/// An operation that was told to stop (its [`Leave`]: its
/// socket was shut down, its accept woken) does not count: it is on
/// its way out, and a server that ends with a thousand connections
/// has a thousand of them for a moment.
pub(crate) const IO_POOL_UNHEARD_MAX: usize = 64;

/// The stack of a thread of the pool. The operations are calls into
/// the OS and the libraries around it (TLS, HTTP), not silt code.
const IO_POOL_STACK_BYTES: usize = 512 * 1024;

/// How long a thread of the pool waits for work before it ends.
const IO_POOL_IDLE: std::time::Duration = std::time::Duration::from_secs(5);

/// A blocking operation handed to the pool. It gives back what hands
/// its value to the waiter, which the thread calls when it has said,
/// under the pool's lock, that the operation is done: a waiter that
/// is woken finds its operation done, and the thread free.
type IoJob = Box<dyn FnOnce() -> Box<dyn FnOnce() + Send> + Send>;

/// Where an operation stands, for its waiter and its thread.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Queued,
    Running,
    /// Running, and its waiter is gone: the thread no longer counts
    /// as one of the pool's, and ends when the operation returns.
    Left,
    /// It has returned; its thread is handing the value to the waiter.
    Finishing,
    /// As `Finishing`, and the waiter went in that moment.
    Gone,
    /// The value is there.
    Finished,
}

/// What an operation's waiter and its thread share.
struct OpState {
    phase: parking_lot::Mutex<Phase>,
    /// Completed with the operation's value.
    cell: Arc<Cell<Value>>,
    /// See [`Leave::Returns`]. Whoever knows first that the waiter
    /// is gone and the value is there calls it: the waiter if the
    /// value was there when it went, the thread otherwise.
    unheard: parking_lot::Mutex<Option<Unheard>>,
    /// Set when the waiter has taken the value ([`IoOp::take`]).
    taken: std::sync::atomic::AtomicBool,
    /// Set, under the pool's lock, when the waiter went while the
    /// operation ran and told it to stop: it is not one of those that
    /// cannot be interrupted.
    stopped: std::sync::atomic::AtomicBool,
    /// Whether the operation has taken what it cannot put back, or its
    /// waiter has gone first ([`Wanted::claim`]): one of
    /// [`WAITING`], [`TOOK`], [`GONE`], decided once.
    claim: std::sync::atomic::AtomicU8,
}

/// Neither the operation has claimed, nor has its waiter gone.
const WAITING: u8 = 0;
/// The operation has claimed ([`Wanted::claim`]).
const TOOK: u8 = 1;
/// The waiter went before the operation claimed.
const GONE: u8 = 2;

type Unheard = Box<dyn FnOnce(&Value) + Send>;

impl OpState {
    fn new(phase: Phase) -> Arc<OpState> {
        Arc::new(OpState {
            phase: parking_lot::Mutex::new(phase),
            cell: Cell::new(),
            unheard: parking_lot::Mutex::new(None),
            taken: std::sync::atomic::AtomicBool::new(false),
            stopped: std::sync::atomic::AtomicBool::new(false),
            claim: std::sync::atomic::AtomicU8::new(WAITING),
        })
    }

    /// The waiter goes: an operation that has not claimed cannot from
    /// now on.
    fn leave(&self) {
        use std::sync::atomic::Ordering::SeqCst;
        let _ = self.claim.compare_exchange(WAITING, GONE, SeqCst, SeqCst);
    }

    /// The waiter is gone and the value is there: if the waiter did
    /// not take it, it is nobody's.
    fn unheard(&self) {
        let unheard = self.unheard.lock().take();
        if let Some(unheard) = unheard
            && !self.taken.load(std::sync::atomic::Ordering::SeqCst)
            && let Some(value) = self.cell.get()
        {
            unheard(value);
        }
    }
}

/// What an operation on the pool knows of its waiter: whether the
/// value is still wanted. It is handed to the operation, which asks it
/// on the pool's thread.
///
/// The value is wanted no longer when the waiter has gone, and when the
/// task that waits takes nothing new (it was cancelled, or its time is
/// over: the flags `Vm::takes_nothing_new` reads). Such a task takes
/// nothing new from the moment its flag is set, not from the moment it
/// waits: a client that connects after the cancel of a task that has
/// started an accept, and has not yet waited for it, is the next
/// accept's.
#[derive(Clone)]
pub struct Wanted {
    state: Arc<OpState>,
    /// The flags of the task that started the operation.
    task: Vec<Arc<std::sync::atomic::AtomicBool>>,
}

impl Wanted {
    fn new(state: Arc<OpState>, task: Vec<Arc<std::sync::atomic::AtomicBool>>) -> Wanted {
        Wanted { state, task }
    }

    /// For an operation of no pool, whose waiter is the caller: the
    /// operations of `runtime::handle` in their tests.
    #[cfg(test)]
    pub(crate) fn of_test(task: Vec<Arc<std::sync::atomic::AtomicBool>>) -> Wanted {
        Wanted::new(OpState::new(Phase::Running), task)
    }

    /// Whether nobody wants the value: the waiter has gone, or its
    /// task takes nothing new.
    pub(crate) fn gone(&self) -> bool {
        use std::sync::atomic::Ordering::SeqCst;
        self.state.claim.load(SeqCst) == GONE || self.task.iter().any(|flag| flag.load(SeqCst))
    }

    /// The operation takes what it cannot put back: a client's
    /// connection, the connection of a transaction. `false` if the
    /// value is not wanted ([`Wanted::gone`]): the operation then takes
    /// nothing, and gives up. `true` once it has claimed: from then on
    /// its waiter knows it ([`Wanted::claimed`]), and one that goes
    /// without the value leaves what it took to the operation's
    /// `unheard`. The claim and the waiter's going are one decision:
    /// exactly one of the two comes first.
    pub(crate) fn claim(&self) -> bool {
        use std::sync::atomic::Ordering::SeqCst;
        if self.task.iter().any(|flag| flag.load(SeqCst)) {
            return self.state.claim.load(SeqCst) == TOOK;
        }
        match self
            .state
            .claim
            .compare_exchange(WAITING, TOOK, SeqCst, SeqCst)
        {
            Ok(_) => true,
            Err(now) => now == TOOK,
        }
    }

    /// Whether the operation has claimed ([`Wanted::claim`]).
    #[cfg(any(test, feature = "tcp", feature = "postgres"))]
    pub(crate) fn claimed(&self) -> bool {
        self.state.claim.load(std::sync::atomic::Ordering::SeqCst) == TOOK
    }

    /// Which operation this is, to tell it from others.
    pub(crate) fn id(&self) -> usize {
        Arc::as_ptr(&self.state) as usize
    }

    /// The waiter goes (as `IoOp`'s `Drop` has it go): for the tests
    /// of the operations of `runtime::handle`.
    #[cfg(test)]
    pub(crate) fn leave(&self) {
        self.state.leave();
    }
}

/// The threads of a VM that run blocking operations (file and socket
/// I/O), so that the scheduler's workers never block.
///
/// The pool is elastic. An operation gets a thread at once: one that
/// waits for work, or a new one, up to [`IO_POOL_THREADS_MAX`]. A
/// thread that finds no work for [`IO_POOL_IDLE`] ends. So operations
/// that block for long (four tasks that each wait for a line from a
/// peer) never hold up one that would not (a file read).
///
/// An operation whose waiter is gone ([`IoOp`]'s `Drop`) is told to
/// stop if it can be (its [`Leave`]: a socket is shut down), and
/// its thread stops counting as the pool's whether or not it can: a
/// read of the terminal that never returns does not use up the pool.
pub(crate) struct IoPool {
    scheduler: Arc<Scheduler>,
    shared: Arc<PoolShared>,
}

struct PoolShared {
    state: parking_lot::Mutex<PoolState>,
    /// The threads that wait for work wait here.
    work: parking_lot::Condvar,
    max: usize,
    idle: std::time::Duration,
    /// The threads that exist: started and not yet returned.
    live: std::sync::atomic::AtomicUsize,
    /// Test-only: the next thread whose operation returns panics
    /// before it hands the value over (`PanicAt::Io`).
    #[cfg(any(test, feature = "test-hooks"))]
    panic_next: std::sync::atomic::AtomicBool,
}

struct PoolState {
    queue: VecDeque<(IoJob, Arc<OpState>)>,
    /// The pool's threads: those that run an operation whose waiter
    /// waits, and those that wait for work.
    threads: usize,
    /// Those of them that run an operation.
    busy: usize,
    /// The threads that run an operation whose waiter is gone and
    /// that cannot be told to stop: no longer the pool's, and not yet
    /// ended.
    unheard: usize,
    /// Whether a thread was ever started.
    started: bool,
    /// No thread could be started when one was needed and none
    /// existed (a platform without threads): operations run on the
    /// thread that submits them.
    unavailable: bool,
    /// The VM is gone ([`Runtime::shutdown`]).
    stopped: bool,
}

impl PoolShared {
    fn run(self: Arc<Self>) {
        /// The thread exists until this is dropped.
        struct Live<'a>(&'a std::sync::atomic::AtomicUsize);
        impl Drop for Live<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let _live = Live(&self.live);
        let mut state = self.state.lock();
        loop {
            if state.stopped {
                state.threads -= 1;
                return;
            }
            let Some((job, op)) = state.queue.pop_front() else {
                let timed_out = self.work.wait_for(&mut state, self.idle).timed_out();
                if timed_out && state.queue.is_empty() {
                    state.threads -= 1;
                    return;
                }
                continue;
            };
            *op.phase.lock() = Phase::Running;
            state.busy += 1;
            drop(state);
            let finish = job();
            #[cfg(any(test, feature = "test-hooks"))]
            if (self.panic_next).swap(false, std::sync::atomic::Ordering::SeqCst) {
                panic!("a panic made for a test: Io");
            }
            state = self.state.lock();
            let left = std::mem::replace(&mut *op.phase.lock(), Phase::Finishing) == Phase::Left;
            if !left {
                state.busy -= 1;
            } else if !op.stopped.load(std::sync::atomic::Ordering::SeqCst) {
                state.unheard -= 1;
            }
            drop(state);
            finish();
            let gone = std::mem::replace(&mut *op.phase.lock(), Phase::Finished) == Phase::Gone;
            if left || gone {
                // The waiter went before the value was there.
                op.unheard();
            }
            if left {
                // Not counted since its waiter left.
                return;
            }
            state = self.state.lock();
        }
    }
}

impl IoPool {
    pub(super) fn new(scheduler: Arc<Scheduler>) -> Self {
        Self::with(IO_POOL_THREADS_MAX, IO_POOL_IDLE, scheduler)
    }

    /// A pool of at most `max` threads, each of which ends after
    /// `idle` without work. `max` 0 stands in for a platform without
    /// threads.
    fn with(max: usize, idle: std::time::Duration, scheduler: Arc<Scheduler>) -> Self {
        IoPool {
            scheduler,
            shared: Arc::new(PoolShared {
                state: parking_lot::Mutex::new(PoolState {
                    queue: VecDeque::new(),
                    threads: 0,
                    busy: 0,
                    unheard: 0,
                    started: false,
                    unavailable: max == 0,
                    stopped: false,
                }),
                work: parking_lot::Condvar::new(),
                max,
                idle,
                live: std::sync::atomic::AtomicUsize::new(0),
                #[cfg(any(test, feature = "test-hooks"))]
                panic_next: std::sync::atomic::AtomicBool::new(false),
            }),
        }
    }

    /// End the threads, each when the operation it is running returns.
    /// What was queued is dropped.
    pub(super) fn stop(&self) {
        let queued = {
            let mut state = self.shared.state.lock();
            state.stopped = true;
            std::mem::take(&mut state.queue)
        };
        self.shared.work.notify_all();
        drop(queued);
    }

    /// How many threads the pool has: those that run an operation
    /// somebody waits for, and those that wait for work.
    #[cfg(any(test, feature = "test-hooks"))]
    pub(crate) fn threads(&self) -> usize {
        self.shared.state.lock().threads
    }

    /// How many threads of the pool exist: started, and not returned.
    /// More than [`IoPool::threads`] while a thread still runs an
    /// operation that nobody waits for.
    #[cfg(any(test, feature = "test-hooks"))]
    pub(crate) fn live_threads(&self) -> usize {
        self.shared.live.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Test-only: see `PanicAt::Io`.
    #[cfg(any(test, feature = "test-hooks"))]
    pub(crate) fn panic_for_test(&self) {
        (self.shared.panic_next).store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// An operation that is not handed to the pool: it never runs,
    /// its cell is never completed, and nothing is done when its
    /// waiter goes (it is not told to stop: there is nothing to stop).
    /// See [`Vm::io_op`].
    pub(crate) fn withheld(&self, failure: ErrFactory) -> IoOp {
        let state = OpState::new(Phase::Left);
        IoOp {
            cell: state.cell.clone(),
            failure,
            in_flight: Arc::default(),
            pool: self.shared.clone(),
            state,
            interrupt: None,
            close: None,
            withheld: true,
        }
    }

    /// Run the blocking operation `f` on a thread of the pool. Its
    /// value completes the cell that is returned. Where no thread can
    /// be started, `f` runs on the calling thread before this returns.
    /// If `f` panics, or the VM is gone, the cell is completed with
    /// `failure` of the reason: the error the builtin's signature
    /// declares.
    pub(crate) fn submit(
        &self,
        failure: ErrFactory,
        task: Vec<Arc<std::sync::atomic::AtomicBool>>,
        leave: Leave,
        f: impl FnOnce(&Wanted) -> Value + Send + 'static,
    ) -> IoOp {
        let state = OpState::new(Phase::Queued);
        let wanted = Wanted::new(state.clone(), task);
        let f = move || f(&wanted);
        let (interrupt, close) = match leave {
            Leave::RunsOut => (None, None),
            Leave::Closes(close) => (None, Some(close)),
            Leave::Returns {
                interrupt,
                give_back,
            } => {
                *state.unheard.lock() = give_back;
                (interrupt, None)
            }
        };
        let op = IoOp {
            cell: state.cell.clone(),
            failure,
            in_flight: Arc::new(parking_lot::Mutex::new(Some(self.scheduler.external()))),
            pool: self.shared.clone(),
            state: state.clone(),
            interrupt,
            close,
            withheld: false,
        };
        let queued = state;
        let (cell, in_flight) = (op.cell.clone(), op.in_flight.clone());
        let scheduler = self.scheduler.clone();
        let job: IoJob = Box::new(move || {
            let result = match crate::scheduler::catch_as_error(std::panic::AssertUnwindSafe(f)) {
                Ok(value) => value,
                Err(panic) => {
                    let msg = if let Some(s) = panic.downcast_ref::<&str>() {
                        s.to_string()
                    } else if let Some(s) = panic.downcast_ref::<String>() {
                        s.clone()
                    } else {
                        "IO task panicked".to_string()
                    };
                    failure(IoFailure::Panicked(&format!("panic: {msg}")))
                }
            };
            Box::new(move || {
                let _ = cell.complete(result, scheduler.wake());
                // After the wake: the operation is in flight no longer.
                let done = in_flight.lock().take();
                drop(done);
            })
        });

        let shared = &self.shared;
        // The operation is not run: its value is the module's error.
        let refuse = |op: IoOp, why: String| {
            let err = failure(IoFailure::Refused(&why));
            *op.state.phase.lock() = Phase::Finished;
            let _ = op.cell.complete(err, self.scheduler.wake());
            let done = op.in_flight.lock().take();
            drop(done);
            op
        };
        let mut state = shared.state.lock();
        if state.stopped {
            drop(state);
            return refuse(op, format!("cannot run an I/O operation: {VM_GONE}"));
        }
        if state.unavailable {
            drop(state);
            let finish = job();
            finish();
            *op.state.phase.lock() = Phase::Finished;
            return op;
        }
        if state.unheard >= IO_POOL_UNHEARD_MAX {
            let unheard = state.unheard;
            drop(state);
            return refuse(
                op,
                format!(
                    "too many I/O operations that nobody waits for are still running \
                     ({unheard}); they cannot be interrupted"
                ),
            );
        }
        if state.queue.len() < state.threads - state.busy {
            // A thread that waits for work, or is about to look for
            // some, takes it.
            state.queue.push_back((job, queued));
            shared.work.notify_one();
            return op;
        }
        if state.threads >= shared.max {
            let in_flight = state.threads;
            drop(state);
            return refuse(
                op,
                format!("too many I/O operations in flight ({in_flight})"),
            );
        }
        state.queue.push_back((job, queued));
        state.threads += 1;
        shared
            .live
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let pool = shared.clone();
        // A panic of the operation is its error (`job`). One that
        // escapes the pool's own code around it, or what hands the
        // value to the waiter, is the runtime's, and ends the program
        // (`Alarm`): the waiter would wait for ever.
        let alarm = self.scheduler.alarm();
        let started = std::thread::Builder::new()
            .name("silt-io".into())
            .stack_size(IO_POOL_STACK_BYTES)
            .spawn(move || {
                crate::scheduler::a_thread_of_the_runtime();
                let run = std::panic::AssertUnwindSafe(|| pool.run());
                if let Err(payload) = std::panic::catch_unwind(run) {
                    alarm.raise(payload);
                }
            });
        let Err(e) = started else {
            state.started = true;
            return op;
        };
        shared
            .live
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        state.threads -= 1;
        if state.threads > 0 {
            // The threads there are take it when they are free.
            return op;
        }
        // No thread at all. The operation that asked is the last in
        // the queue.
        let (job, _) = state
            .queue
            .pop_back()
            .expect("the operation that was queued");
        if state.started {
            // The system had threads before and will have again: only
            // this operation fails, and the next one tries anew.
            drop(state);
            return refuse(op, format!("cannot start an I/O thread: {e}"));
        }
        // It never had one (a platform without threads): from now on
        // the thread that asks does the work.
        state.unavailable = true;
        drop(state);
        let finish = job();
        finish();
        *op.state.phase.lock() = Phase::Finished;
        op
    }
}

impl IoPool {
    /// Run `f` on a thread of the pool, for nobody to wait for: it
    /// counts as pending for nothing, and its thread stays the pool's.
    /// Its value is dropped. For work that goes on beside the program
    /// (it feeds a channel that the program reads, or it ends what a
    /// task left behind) and that counts as what it holds itself for
    /// that: a task while it runs ([`Scheduler::enter`]), or something
    /// outside the tasks that can end a wait ([`Scheduler::external`]),
    /// taken before this is called, so that there is no moment at which
    /// it counts as nothing; or as nothing, when nothing waits for it.
    ///
    /// [`Scheduler::enter`]: crate::scheduler::Scheduler::enter
    /// [`Scheduler::external`]: crate::scheduler::Scheduler::external
    #[cfg(feature = "postgres")]
    pub(crate) fn background(
        &self,
        failure: ErrFactory,
        f: impl FnOnce() -> Value + Send + 'static,
    ) {
        self.submit(failure, Vec::new(), Leave::RunsOut, move |_| f())
            .detach();
    }
}

/// Why an I/O operation has no value of its own, with the reason as
/// text.
#[derive(Clone, Copy, Debug)]
pub enum IoFailure<'a> {
    /// The wait's time was over (`SILT_IO_TIMEOUT`).
    Timeout(&'a str),
    /// The operation panicked.
    Panicked(&'a str),
    /// It was not run: the pool has no thread for it, or the VM is
    /// gone.
    Refused(&'a str),
}

impl IoFailure<'_> {
    pub fn text(&self) -> &str {
        match self {
            IoFailure::Timeout(text) | IoFailure::Panicked(text) | IoFailure::Refused(text) => text,
        }
    }
}

/// The typed error of a builtin module for an [`IoFailure`].
pub(crate) type ErrFactory = fn(IoFailure<'_>) -> Value;

/// An I/O operation on the pool, as the task that waits for it holds
/// it.
pub(crate) struct IoOp {
    /// Completed with the operation's value.
    pub(crate) cell: Arc<Cell<Value>>,
    /// The typed error of the module, for a wait that ends without the
    /// value ([`IoOp::value`]).
    failure: ErrFactory,
    /// What keeps the program from being called deadlocked, or ended,
    /// while the operation is in flight and somebody waits for it.
    /// Whoever is done with it first drops it: the thread when the
    /// operation ends, or the waiter when it stops waiting, on every
    /// way out of its wait (see the `Drop`).
    pub(crate) in_flight: Arc<parking_lot::Mutex<Option<External>>>,
    pool: Arc<PoolShared>,
    state: Arc<OpState>,
    /// See [`Leave::Returns`].
    interrupt: Option<Interrupt>,
    /// See [`Leave::Closes`].
    close: Option<Close>,
    /// It was never handed to the pool ([`IoPool::withheld`]).
    withheld: bool,
}

/// Where the wait of a task for an operation ends if the value does
/// not come ([`IoOp::wait`]).
#[derive(Clone, Copy)]
pub(crate) enum Limit {
    /// At `SILT_IO_TIMEOUT`, if that is set: a wait for I/O.
    Io,
    /// After this long: a time of the protocol's own (the server of
    /// `http.serve`).
    #[cfg_attr(not(feature = "http"), allow(dead_code))]
    Own(std::time::Duration),
    /// Never: the server's accept, the end of a transaction.
    #[cfg_attr(
        not(any(feature = "tcp", feature = "postgres", feature = "test-hooks")),
        allow(dead_code)
    )]
    None,
}

/// What becomes of an operation on the pool whose waiter goes without
/// waiting for it to the end: the wait's deadline passed, the task was
/// cancelled, its time is over, or it was dropped with the program.
/// The module that starts the operation says which of these it is
/// ([`Vm::io_op`]); `IoOp`'s `Drop` does it, in every one of those
/// cases and wherever the operation stood. (A waiter that has taken the
/// value goes too, and nothing is done then.)
pub(crate) enum Leave {
    /// It cannot be interrupted: it runs to its end, and its value is
    /// dropped. Its thread is the pool's no longer while it does
    /// ([`IO_POOL_UNHEARD_MAX`]). A file read, `tcp.connect`, a request
    /// of the HTTP client.
    RunsOut,
    /// It works on something that is of no use to a waiter who did not
    /// get its value: a connection, where a read that nobody heard has
    /// taken bytes that are gone with it. The function ends that thing
    /// (shuts the connection down), which also makes the operation
    /// return. It is called whenever the waiter goes without having
    /// taken the value ([`IoOp::take`]), whatever the operation was
    /// doing then: it had not begun, it ran, or it had ended. What the
    /// waiter is left with does not depend on the moment at which it
    /// stopped waiting.
    ///
    /// The function says whether it reached that thing. One that did
    /// not (the socket of a TLS connect is not there yet: the connect
    /// is still under way) leaves the operation to run on, counted as
    /// one that cannot be interrupted ([`IO_POOL_UNHEARD_MAX`]) until it
    /// ends, which it does at once when its socket comes.
    #[cfg_attr(not(feature = "tcp"), allow(dead_code))]
    Closes(Close),
    /// It can be made to return, and what it took must not be lost.
    ///
    /// `interrupt` is called if the waiter goes while the operation
    /// runs, with the operation's [`Wanted`], which says whether it had
    /// claimed ([`Wanted::claimed`]): it makes the operation return (it
    /// wakes an accept, asks the server to cancel a statement), so that
    /// its thread ends.
    ///
    /// `give_back` is called with the value if the waiter goes without
    /// having taken it, once the value is there, in whichever order the
    /// two come: for a value that must not be lost with a waiter that
    /// went just as it came (an accepted connection, a transaction that
    /// has begun).
    #[cfg_attr(not(any(feature = "tcp", feature = "postgres")), allow(dead_code))]
    Returns {
        interrupt: Option<Interrupt>,
        give_back: Option<Unheard>,
    },
}

/// See [`Leave::Returns`].
pub(crate) type Interrupt = Box<dyn FnOnce(&Wanted) + Send>;

/// See [`Leave::Closes`]: `true` if it reached what the operation works
/// on.
pub(crate) type Close = Box<dyn FnOnce() -> bool + Send>;

impl Leave {
    /// [`Leave::Returns`] with `interrupt` alone.
    #[cfg(any(test, feature = "tcp", feature = "postgres"))]
    pub(crate) fn interrupts(interrupt: impl FnOnce(&Wanted) + Send + 'static) -> Leave {
        Leave::Returns {
            interrupt: Some(Box::new(interrupt)),
            give_back: None,
        }
    }

    /// [`Leave::Returns`] with `give_back` alone: the operation runs
    /// out, and a value that nobody took is given back.
    #[cfg(feature = "postgres")]
    pub(crate) fn gives_back(give_back: impl FnOnce(&Value) + Send + 'static) -> Leave {
        Leave::Returns {
            interrupt: None,
            give_back: Some(Box::new(give_back)),
        }
    }

    /// [`Leave::Closes`].
    #[cfg(feature = "tcp")]
    pub(crate) fn closes(close: impl FnOnce() -> bool + Send + 'static) -> Leave {
        Leave::Closes(Box::new(close))
    }
}

impl IoOp {
    /// The wait of a task for the operation: it ends with the value,
    /// or at the deadline that `limit` says. Every wait for an
    /// operation on the pool is this one; one that waits for something
    /// else besides adds its arms after the operation's, which is arm
    /// 0.
    pub(crate) fn wait(&self, vm: &Vm, limit: Limit) -> Wait {
        let deadline = match limit {
            Limit::Io => vm.runtime.scheduler.io_deadline(),
            Limit::Own(time) => vm.runtime.io.deadline_after(time),
            Limit::None => None,
        };
        Wait::new(vec![Arm::Cell(self.cell.clone())]).deadline(deadline)
    }

    /// What the wait for the operation ([`IoOp::wait`]), which ended as
    /// `fired`, gave the task: the operation's value, which the task
    /// has from then on, or `None` if the wait ended without it.
    pub(crate) fn outcome(&self, fired: &Fired) -> Option<Value> {
        match fired {
            Fired::Arm(0, _) => self.take(),
            _ => None,
        }
    }

    /// [`IoOp::outcome`], or the module's typed timeout error if the
    /// wait ended at its deadline.
    pub(crate) fn value(&self, fired: &Fired) -> Value {
        self.outcome(fired)
            .unwrap_or_else(|| (self.failure)(IoFailure::Timeout(crate::scheduler::IO_TIMEOUT)))
    }

    /// The operation's value, for the waiter whose wait it ended: the
    /// value has reached the task.
    fn take(&self) -> Option<Value> {
        let value = self.cell.get()?.clone();
        self.state
            .taken
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Some(value)
    }

    /// Whether the operation is over: its value is there and its
    /// thread is done with it, so a waiter that goes now sees the value
    /// (`Leave::Returns`'s `give_back` runs in its `Drop`). The value
    /// alone (`cell`) comes a moment before that.
    #[cfg(all(test, feature = "http"))]
    pub(crate) fn finished(&self) -> bool {
        *self.state.phase.lock() == Phase::Finished
    }

    /// Nobody waits for the operation, and it counts as pending no
    /// longer; its thread stays the pool's, and it is not one of those
    /// that cannot be interrupted ([`IO_POOL_UNHEARD_MAX`]): work that
    /// ends by itself soon (the end of a transaction) or that goes on
    /// beside the program ([`IoPool::background`]).
    #[cfg(feature = "postgres")]
    pub(crate) fn detach(mut self) {
        let pending = self.in_flight.lock().take();
        drop(pending);
        self.state = OpState::new(Phase::Left);
    }
}

/// The waiter is gone: it has its value, its deadline passed, it was
/// cancelled, or it was dropped with its task at the end of the
/// program. An operation that nobody waits for is not pending for the
/// program. If it still runs, it is told to stop where it can be, and
/// its thread is the pool's no longer: whether or not it can be
/// stopped (a read of the terminal cannot), it does not use up the
/// pool. If it has not started, it never does. What else becomes of it
/// is its [`Leave`].
impl Drop for IoOp {
    fn drop(&mut self) {
        // Nothing was started, so nothing is left to stop or to count.
        if self.withheld {
            return;
        }
        let gone = self.in_flight.lock().take();
        drop(gone);
        // Before the operation is told: one that claims after this
        // finds its value unwanted.
        self.state.leave();
        // The waiter decides, not where the operation stood. (Before
        // the operation's place is changed: one that the close makes
        // return finishes as usual.)
        let closed = match self.close.take() {
            Some(close) if !self.state.taken.load(std::sync::atomic::Ordering::SeqCst) => close(),
            _ => false,
        };
        let (running, finished) = {
            let mut state = self.pool.state.lock();
            let mut phase = self.state.phase.lock();
            match *phase {
                Phase::Queued => {
                    let mine = &self.state;
                    state.queue.retain(|(_, op)| !Arc::ptr_eq(op, mine));
                    *phase = Phase::Left;
                    (false, false)
                }
                Phase::Running => {
                    *phase = Phase::Left;
                    state.threads -= 1;
                    state.busy -= 1;
                    // One whose close reached nothing (its socket is
                    // not there yet) runs on like one that cannot be
                    // interrupted.
                    match self.interrupt.is_some() || closed {
                        true => self
                            .state
                            .stopped
                            .store(true, std::sync::atomic::Ordering::SeqCst),
                        false => state.unheard += 1,
                    }
                    (true, false)
                }
                // The thread sees that, and calls `unheard`.
                Phase::Finishing => {
                    *phase = Phase::Gone;
                    (false, false)
                }
                Phase::Finished => (false, true),
                Phase::Left | Phase::Gone => (false, false),
            }
        };
        if running && let Some(interrupt) = self.interrupt.take() {
            interrupt(&Wanted::new(self.state.clone(), Vec::new()));
        }
        if finished {
            self.state.unheard();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::HostIo;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn failure(failure: IoFailure<'_>) -> Value {
        Value::String(failure.text().into())
    }

    fn pool(max: usize, idle: Duration) -> IoPool {
        IoPool::with(max, idle, Arc::new(Scheduler::new(HostIo::process())))
    }

    /// An idle limit that no test reaches.
    const NEVER_IDLE: Duration = Duration::from_secs(3600);

    /// Wait, for at most ten seconds, until `done` holds.
    fn until(what: &str, done: impl Fn() -> bool) {
        let limit = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < limit, "{what}");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Where no thread can be started (a pool of no threads stands in
    /// for a platform without them), an operation runs on the thread
    /// that submits it: its value is there when `submit` returns, a
    /// panic in it is the typed failure, and nothing stays pending.
    #[test]
    fn without_threads_an_operation_runs_on_the_caller() {
        let pool = pool(0, NEVER_IDLE);
        let here = std::thread::current().id();

        let op = pool.submit(failure, Vec::new(), Leave::RunsOut, move |_| {
            assert_eq!(std::thread::current().id(), here);
            Value::Int(7)
        });
        assert!(matches!(op.cell.get(), Some(Value::Int(7))));
        assert!(op.in_flight.lock().is_none());

        let op = pool.submit(failure, Vec::new(), Leave::RunsOut, |_| {
            panic!("no such file")
        });
        match op.cell.get() {
            Some(Value::String(msg)) => assert_eq!(&**msg, "panic: no such file"),
            other => panic!("expected the typed failure, got {other:?}"),
        }
        assert!(op.in_flight.lock().is_none());
        assert_eq!(pool.threads(), 0);
    }

    /// Operations that block do not hold up one that does not: each
    /// gets a thread of its own.
    #[test]
    fn a_thread_is_added_when_all_are_busy() {
        let pool = pool(IO_POOL_THREADS_MAX, NEVER_IDLE);
        let (release, held) = mpsc::channel::<()>();
        let held = Arc::new(parking_lot::Mutex::new(held));
        let blocked: Vec<IoOp> = (0..8)
            .map(|_| {
                let held = held.clone();
                pool.submit(failure, Vec::new(), Leave::RunsOut, move |_| {
                    let _ = held.lock().recv();
                    Value::Unit
                })
            })
            .collect();
        let quick = pool.submit(failure, Vec::new(), Leave::RunsOut, |_| Value::Int(1));
        until("the ninth operation ran while eight block", || {
            quick.cell.get().is_some()
        });
        assert_eq!(pool.threads(), 9);
        for _ in &blocked {
            release.send(()).unwrap();
        }
        until("the eight end", || {
            blocked.iter().all(|op| op.cell.get().is_some())
        });
    }

    /// A thread that is free is used before another is started.
    #[test]
    fn a_thread_is_reused() {
        let pool = pool(IO_POOL_THREADS_MAX, NEVER_IDLE);
        for n in 0..20 {
            let op = pool.submit(failure, Vec::new(), Leave::RunsOut, move |_| Value::Int(n));
            until("the operation ran", || op.cell.get().is_some());
            // The thread that woke the waiter is free for the next.
            assert_eq!(pool.threads(), 1);
            assert_eq!(pool.live_threads(), 1);
        }
    }

    /// A thread that finds no work for the idle limit ends, and the
    /// next operation gets a new one.
    #[test]
    fn a_thread_is_retired_when_idle() {
        let pool = pool(IO_POOL_THREADS_MAX, Duration::from_millis(1));
        let op = pool.submit(failure, Vec::new(), Leave::RunsOut, |_| Value::Int(1));
        until("the operation ran", || op.cell.get().is_some());
        until("the idle thread ended", || pool.live_threads() == 0);
        assert_eq!(pool.threads(), 0);
        let op = pool.submit(failure, Vec::new(), Leave::RunsOut, |_| Value::Int(2));
        until("a new thread ran it", || op.cell.get().is_some());
    }

    /// At the bound an operation is refused at once, with the module's
    /// error: it does not wait for a thread. When one is free again,
    /// operations run again.
    #[test]
    fn at_the_bound_an_operation_is_refused_at_once() {
        let pool = pool(2, NEVER_IDLE);
        let (release, held) = mpsc::channel::<()>();
        let held = Arc::new(parking_lot::Mutex::new(held));
        let running = Arc::new(AtomicUsize::new(0));
        let blocked: Vec<IoOp> = (0..2)
            .map(|_| {
                let (held, running) = (held.clone(), running.clone());
                pool.submit(failure, Vec::new(), Leave::RunsOut, move |_| {
                    running.fetch_add(1, Ordering::SeqCst);
                    let _ = held.lock().recv();
                    Value::Unit
                })
            })
            .collect();
        until("two run", || running.load(Ordering::SeqCst) == 2);
        let refused = pool.submit(failure, Vec::new(), Leave::RunsOut, |_| Value::Int(3));
        match refused.cell.get() {
            Some(Value::String(msg)) => {
                assert_eq!(&**msg, "too many I/O operations in flight (2)")
            }
            other => panic!("expected the refusal, got {other:?}"),
        }
        assert!(refused.in_flight.lock().is_none());
        assert_eq!(pool.threads(), 2);
        for _ in &blocked {
            release.send(()).unwrap();
        }
        until("the two end", || {
            blocked.iter().all(|op| op.cell.get().is_some())
        });
        let next = pool.submit(failure, Vec::new(), Leave::RunsOut, |_| Value::Int(4));
        until("the next runs", || next.cell.get().is_some());
        assert!(matches!(next.cell.get(), Some(Value::Int(4))));
    }

    /// Operations that cannot be stopped and that nobody waits for any
    /// more keep their threads, up to a bound of their own; beyond it
    /// new operations are refused, until some of them have ended.
    #[test]
    fn stuck_operations_without_a_waiter_are_bounded() {
        let pool = pool(IO_POOL_THREADS_MAX, NEVER_IDLE);
        let (release, held) = mpsc::channel::<()>();
        let held = Arc::new(parking_lot::Mutex::new(held));
        let running = Arc::new(AtomicUsize::new(0));
        for _ in 0..IO_POOL_UNHEARD_MAX {
            let (held, started) = (held.clone(), running.clone());
            let before = running.load(Ordering::SeqCst);
            let op = pool.submit(failure, Vec::new(), Leave::RunsOut, move |_| {
                started.fetch_add(1, Ordering::SeqCst);
                let _ = held.lock().recv();
                Value::Unit
            });
            until("it runs", || running.load(Ordering::SeqCst) > before);
            // Its waiter goes; it cannot be stopped.
            drop(op);
        }
        assert_eq!(pool.threads(), 0);
        assert_eq!(pool.live_threads(), IO_POOL_UNHEARD_MAX);
        let refused = pool.submit(failure, Vec::new(), Leave::RunsOut, |_| Value::Int(1));
        match refused.cell.get() {
            Some(Value::String(msg)) => assert_eq!(
                &**msg,
                &format!(
                    "too many I/O operations that nobody waits for are still running \
                     ({IO_POOL_UNHEARD_MAX}); they cannot be interrupted"
                )
            ),
            other => panic!("expected the refusal, got {other:?}"),
        }
        for _ in 0..IO_POOL_UNHEARD_MAX {
            release.send(()).unwrap();
        }
        until("the stuck threads ended", || pool.live_threads() == 0);
        let next = pool.submit(failure, Vec::new(), Leave::RunsOut, |_| Value::Int(2));
        until("the next runs", || next.cell.get().is_some());
    }

    /// Operations that were told to stop when their waiters went do
    /// not count against the bound for those that cannot be: a server
    /// that ends with more connections than that bound leaves the
    /// pool to everybody else. (Here they take their time to stop;
    /// with a count of them all, the last submit was refused with
    /// "they cannot be interrupted".)
    #[test]
    fn operations_that_were_stopped_do_not_count_as_stuck() {
        let pool = pool(IO_POOL_THREADS_MAX, NEVER_IDLE);
        let (release, held) = mpsc::channel::<()>();
        let held = Arc::new(parking_lot::Mutex::new(held));
        let running = Arc::new(AtomicUsize::new(0));
        let told = Arc::new(AtomicUsize::new(0));
        let many = 4 * IO_POOL_UNHEARD_MAX;
        for _ in 0..many {
            let (held, started, told) = (held.clone(), running.clone(), told.clone());
            let before = running.load(Ordering::SeqCst);
            let leave = Leave::interrupts(move |_| {
                told.fetch_add(1, Ordering::SeqCst);
            });
            let op = pool.submit(failure, Vec::new(), leave, move |_| {
                started.fetch_add(1, Ordering::SeqCst);
                let _ = held.lock().recv();
                Value::Unit
            });
            until("it runs", || running.load(Ordering::SeqCst) > before);
            drop(op);
        }
        assert_eq!(told.load(Ordering::SeqCst), many);
        // All of them are still on their way out.
        assert_eq!(pool.live_threads(), many);
        assert_eq!(pool.threads(), 0);
        let next = pool.submit(failure, Vec::new(), Leave::RunsOut, |_| Value::Int(1));
        until("the next runs", || next.cell.get().is_some());
        assert!(matches!(next.cell.get(), Some(Value::Int(1))));
        // One that cannot be stopped still counts, beside them.
        let (free, stuck) = mpsc::channel::<()>();
        let op = pool.submit(failure, Vec::new(), Leave::RunsOut, move |_| {
            let _ = stuck.recv();
            Value::Unit
        });
        until("it runs", || *op.state.phase.lock() == Phase::Running);
        drop(op);
        assert_eq!(pool.shared.state.lock().unheard, 1);
        for _ in 0..many {
            release.send(()).unwrap();
        }
        free.send(()).unwrap();
        until("the threads ended", || pool.live_threads() == 0);
        assert_eq!(pool.shared.state.lock().unheard, 0);
    }

    /// An operation whose waiter is gone is told to stop, and its
    /// thread is the pool's no longer, at once: also when it cannot be
    /// stopped.
    #[test]
    fn an_operation_without_a_waiter_does_not_use_up_the_pool() {
        let pool = pool(1, NEVER_IDLE);
        let stopped = Arc::new(AtomicBool::new(false));
        let started = Arc::new(AtomicBool::new(false));
        // One that can be stopped.
        let (flag, begun) = (stopped.clone(), started.clone());
        let leave = Leave::interrupts({
            let stopped = stopped.clone();
            move |_| stopped.store(true, Ordering::SeqCst)
        });
        let op = pool.submit(failure, Vec::new(), leave, move |_| {
            begun.store(true, Ordering::SeqCst);
            while !flag.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            Value::Unit
        });
        until("it runs", || started.load(Ordering::SeqCst));
        assert_eq!(pool.threads(), 1);
        drop(op);
        assert!(stopped.load(Ordering::SeqCst));
        assert_eq!(pool.threads(), 0);

        // One that cannot: the pool of one thread still runs the next.
        let (release, held) = mpsc::channel::<()>();
        let op = pool.submit(failure, Vec::new(), Leave::RunsOut, move |_| {
            let _ = held.recv();
            Value::Unit
        });
        until("it runs", || *op.state.phase.lock() == Phase::Running);
        drop(op);
        assert_eq!(pool.threads(), 0);
        let next = pool.submit(failure, Vec::new(), Leave::RunsOut, |_| Value::Int(2));
        until("the next ran", || next.cell.get().is_some());
        release.send(()).unwrap();
    }

    /// An operation that has not started when its waiter goes never
    /// does.
    #[test]
    fn an_operation_that_is_queued_when_its_waiter_goes_never_runs() {
        let pool = pool(1, NEVER_IDLE);
        let (release, held) = mpsc::channel::<()>();
        let first = pool.submit(failure, Vec::new(), Leave::RunsOut, move |_| {
            let _ = held.recv();
            Value::Unit
        });
        until("the first runs", || {
            *first.state.phase.lock() == Phase::Running
        });
        let ran = Arc::new(AtomicBool::new(false));
        let flag = ran.clone();
        let second = pool.submit(failure, Vec::new(), Leave::RunsOut, move |_| {
            flag.store(true, Ordering::SeqCst);
            Value::Unit
        });
        drop(second);
        release.send(()).unwrap();
        until("the first ends", || first.cell.get().is_some());
        let third = pool.submit(failure, Vec::new(), Leave::RunsOut, |_| Value::Unit);
        until("the third ran", || third.cell.get().is_some());
        assert!(!ran.load(Ordering::SeqCst));
    }

    /// An operation on something that is of no use to a waiter who
    /// did not get its value ([`Leave::Closes`]) is told to stop
    /// whenever its waiter goes without the value: while it was queued,
    /// while it ran, and when its value was there and nobody had taken
    /// it. A waiter that took the value leaves it alone, and so does
    /// one of an operation that is only interrupted while it runs
    /// ([`Leave::Returns`]).
    #[test]
    #[cfg(feature = "tcp")]
    fn a_waiter_that_goes_without_the_value_stops_the_operation_wherever_it_stands() {
        let pool = pool(1, NEVER_IDLE);
        let told = Arc::new(AtomicUsize::new(0));
        let tell = || {
            let told = told.clone();
            move || {
                told.fetch_add(1, Ordering::SeqCst);
                true
            }
        };
        let closes = || Leave::closes(tell());
        let told = || told.load(Ordering::SeqCst);
        // One runs, in the pool's one thread.
        let (release, held) = mpsc::channel::<()>();
        let running = pool.submit(failure, Vec::new(), closes(), move |_| {
            let _ = held.recv();
            Value::Unit
        });
        until("the first runs", || {
            *running.state.phase.lock() == Phase::Running
        });
        // Two are in the queue, where no thread takes them. (`submit`
        // starts a thread for what it queues, or refuses: an operation
        // stays queued for a moment only, so these are put there by
        // hand.)
        let ran = Arc::new(AtomicBool::new(false));
        let queued = |stop: bool| {
            let state = OpState::new(Phase::Queued);
            let flag = ran.clone();
            let job: IoJob = Box::new(move || {
                flag.store(true, Ordering::SeqCst);
                Box::new(|| {})
            });
            let mut pool_state = pool.shared.state.lock();
            pool_state.queue.push_back((job, state.clone()));
            let (interrupt, close): (Option<Interrupt>, Option<Close>) = match stop {
                true => (None, Some(Box::new(tell()))),
                false => {
                    let tell = tell();
                    (
                        Some(Box::new(move |_: &Wanted| {
                            tell();
                        })),
                        None,
                    )
                }
            };
            IoOp {
                cell: state.cell.clone(),
                failure,
                in_flight: Arc::default(),
                pool: pool.shared.clone(),
                state,
                interrupt,
                close,
                withheld: false,
            }
        };
        let (unheard, only_while_it_runs) = (queued(true), queued(false));
        drop(only_while_it_runs);
        assert_eq!(told(), 0);
        drop(unheard);
        assert_eq!(told(), 1);
        assert!(pool.shared.state.lock().queue.is_empty());
        drop(running);
        assert_eq!(told(), 2);
        release.send(()).unwrap();
        assert!(!ran.load(Ordering::SeqCst));
        // The value is there, and the waiter goes without it.
        let unheard = pool.submit(failure, Vec::new(), closes(), |_| Value::Int(2));
        until("its value is there", || {
            *unheard.state.phase.lock() == Phase::Finished
        });
        drop(unheard);
        assert_eq!(told(), 3);
        // The value is there, and the waiter takes it.
        let heard = pool.submit(failure, Vec::new(), closes(), |_| Value::Int(3));
        until("its value is there", || {
            *heard.state.phase.lock() == Phase::Finished
        });
        assert!(matches!(heard.take(), Some(Value::Int(3))));
        drop(heard);
        assert_eq!(told(), 3);
    }

    /// An operation whose close reached nothing (its socket was not
    /// there yet) runs on as one that cannot be interrupted: it counts
    /// among the stuck ones until it ends. One whose close reached its
    /// socket is on its way out and does not.
    #[test]
    fn an_operation_whose_close_reached_nothing_counts_as_stuck() {
        let pool = pool(IO_POOL_THREADS_MAX, NEVER_IDLE);
        for reached in [true, false] {
            let (release, held) = mpsc::channel::<()>();
            let leave = Leave::Closes(Box::new(move || reached));
            let op = pool.submit(failure, Vec::new(), leave, move |_| {
                let _ = held.recv();
                Value::Unit
            });
            until("it runs", || *op.state.phase.lock() == Phase::Running);
            drop(op);
            assert_eq!(pool.shared.state.lock().unheard, usize::from(!reached));
            release.send(()).unwrap();
            until("it ended", || {
                pool.shared.state.lock().unheard == 0 && pool.live_threads() == pool.threads()
            });
        }
    }

    /// An operation's claim and its waiter's going are one decision:
    /// whichever comes first stands. An operation that claims first
    /// keeps its claim when the waiter goes, and the waiter knows it;
    /// one that tries after the waiter went is refused, and so is one of
    /// a task that takes nothing new, before its waiter has gone.
    #[test]
    fn a_claim_and_the_waiters_going_are_one_decision() {
        let took = Wanted::of_test(Vec::new());
        assert!(!took.gone());
        assert!(took.claim());
        assert!(took.claim(), "a claim that was made stands");
        took.leave();
        assert!(took.claimed());
        assert!(!took.gone(), "the operation has what it claimed");

        let late = Wanted::of_test(Vec::new());
        late.leave();
        assert!(late.gone());
        assert!(!late.claim());
        assert!(!late.claimed());

        let cancelled = Arc::new(AtomicBool::new(false));
        let of_task = Wanted::of_test(vec![cancelled.clone()]);
        cancelled.store(true, Ordering::SeqCst);
        assert!(of_task.gone());
        assert!(!of_task.claim());
        of_task.leave();
        assert!(!of_task.claimed());
    }

    /// The waiter of an operation goes in `IoOp`'s `Drop`, wherever
    /// the operation stands: one that claims afterwards is refused.
    #[test]
    fn an_operation_whose_waiter_went_cannot_claim() {
        let pool = pool(IO_POOL_THREADS_MAX, NEVER_IDLE);
        let (release, held) = mpsc::channel::<()>();
        let (tell, told) = mpsc::channel::<bool>();
        let op = pool.submit(failure, Vec::new(), Leave::RunsOut, move |wanted| {
            let _ = held.recv();
            let _ = tell.send(wanted.claim());
            Value::Unit
        });
        until("it runs", || *op.state.phase.lock() == Phase::Running);
        drop(op);
        release.send(()).unwrap();
        assert!(!told.recv().unwrap());
    }

    /// After the VM is gone an operation fails with the module's
    /// error, and the threads end.
    #[test]
    fn a_stopped_pool_fails_its_operations() {
        let pool = pool(IO_POOL_THREADS_MAX, NEVER_IDLE);
        let op = pool.submit(failure, Vec::new(), Leave::RunsOut, |_| Value::Int(1));
        until("it ran", || op.cell.get().is_some());
        pool.stop();
        until("the thread ended", || pool.threads() == 0);
        let op = pool.submit(failure, Vec::new(), Leave::RunsOut, |_| Value::Int(2));
        match op.cell.get() {
            Some(Value::String(msg)) => assert!(msg.contains(VM_GONE), "{msg}"),
            other => panic!("expected the typed failure, got {other:?}"),
        }
    }
}
