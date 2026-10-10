//! VM runtime types: call frames, blocking reasons, timer manager, I/O pool,
//! shared runtime state, and regex cache.

use regex::Regex;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use crate::bytecode::VmClosure;
use crate::runtime::sync::{Cell, Fired, Wait};
use crate::scheduler::{External, Scheduler};
use crate::value::Value;

use super::{HostIo, Vm, VmError};

// ── Call frame ────────────────────────────────────────────────────

pub(crate) struct CallFrame {
    pub(crate) closure: Arc<VmClosure>,
    pub(crate) ip: usize,
    pub(crate) base_slot: usize,
}

/// Upper bound on how many elided tail-call frames we retain per physical
/// frame before the oldest entries are dropped from the ring buffer. Used
/// by the VM's parallel `tco_elided` diagnostic log. See `Vm::push_frame`,
/// `Vm::pop_frame`, and the `Op::TailCall` dispatcher.
pub(crate) const TCO_ELIDED_CAP: usize = 32;

// ── Frames ───────────────────────────────────────────────────────

/// One frame of a VM: a silt function being run, or a builtin that
/// calls back into silt code or waits.
pub(crate) enum Frame {
    Code(CallFrame),
    Native(Box<dyn Native>),
}

/// A builtin that does not finish in one go: it calls silt functions
/// (`list.map`), or waits (`channel.each`). It is a state machine and a
/// frame of the VM, so a call it makes is a frame above it in the one
/// instruction loop, never a loop of its own on the host stack.
pub(crate) trait Native: Send {
    /// The builtin's name, `list.map`.
    fn name(&self) -> &str;

    /// Go on. `input` is unit the first time and after the frame
    /// parked; after a [`Step::Call`], the value the call returned.
    fn resume(&mut self, vm: &mut Vm, input: Value) -> Result<Step, VmError>;

    /// The frame is dropped because an error passes through it: undo
    /// what the builtin did to the VM for the time of its call.
    fn abandon(&mut self, _vm: &mut Vm) {}
}

/// What a builtin, or a [`Native`] frame, does next.
pub(crate) enum Step {
    /// It is finished, with this value.
    Done(Value),
    /// Call `callee` with the top `argc` values of the stack, which
    /// are above a slot of the call's own; the value it returns is the
    /// frame's next input. Made by [`Vm::call`].
    Call { callee: Value, argc: usize },
    /// Go on as this frame, whose value is the builtin's.
    Run(Box<dyn Native>),
    /// The task waits for this. When the wait has ended the frame is
    /// resumed, with unit, and finds how it ended in [`Vm::woken`].
    Park(Wait),
    /// The task's slice ends here, to give way to the other tasks; the
    /// frame is resumed, with unit, when the task runs again.
    Yield,
}

/// A frame that waits once: [`Vm::park`].
struct Await<F> {
    name: &'static str,
    /// The wait, until the frame has parked.
    wait: Option<Wait>,
    then: F,
}

impl<F> Native for Await<F>
where
    F: FnMut(&mut Vm, Fired) -> Result<Step, VmError> + Send,
{
    fn name(&self) -> &str {
        self.name
    }

    fn resume(&mut self, vm: &mut Vm, _input: Value) -> Result<Step, VmError> {
        if let Some(wait) = self.wait.take() {
            return Ok(Step::Park(wait));
        }
        let fired = vm.woken()?;
        let step = (self.then)(vm, fired)?;
        // A clock that has panicked ended every wait that was pending
        // on it: the waiter fails as a builtin that reads the clock
        // does, unless it has an error of its own.
        match vm.runtime.io.clock_failure() {
            Some(failure) => Err(VmError::new(failure)),
            None => Ok(step),
        }
    }
}

impl Vm {
    /// The step of a builtin `name` that waits for `wait` and goes on
    /// with `then`, which gets how the wait ended.
    pub(crate) fn park(
        &mut self,
        name: &'static str,
        wait: Wait,
        then: impl FnMut(&mut Vm, Fired) -> Result<Step, VmError> + Send + 'static,
    ) -> Step {
        Step::Run(Box::new(Await {
            name,
            wait: Some(wait),
            then,
        }))
    }

    /// How the wait of the frame that is resumed after a
    /// [`Step::Park`] ended.
    pub(crate) fn woken(&mut self) -> Result<Fired, VmError> {
        self.woken.take().ok_or_else(|| {
            VmError::new("internal VM error: a frame was resumed with its wait open".into())
        })
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
/// An operation that was told to stop ([`IoOp::stop_with`]: its
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
    /// See [`IoOp::unheard_with`]. Whoever knows first that the waiter
    /// is gone and the value is there calls it: the waiter if the
    /// value was there when it went, the thread otherwise.
    unheard: parking_lot::Mutex<Option<Unheard>>,
    /// Set when the waiter has taken the value ([`IoOp::take`]).
    taken: std::sync::atomic::AtomicBool,
    /// Set, under the pool's lock, when the waiter went while the
    /// operation ran and told it to stop: it is not one of those that
    /// cannot be interrupted.
    stopped: std::sync::atomic::AtomicBool,
}

type Unheard = Box<dyn FnOnce(&Value) + Send>;

impl OpState {
    fn new(phase: Phase) -> Arc<OpState> {
        Arc::new(OpState {
            phase: parking_lot::Mutex::new(phase),
            cell: Cell::new(),
            unheard: parking_lot::Mutex::new(None),
            taken: std::sync::atomic::AtomicBool::new(false),
            stopped: std::sync::atomic::AtomicBool::new(false),
        })
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
/// stop if it can be ([`IoOp::stop_with`]: a socket is shut down), and
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
            }),
        }
    }

    /// End the threads, each when the operation it is running returns.
    /// What was queued is dropped.
    fn stop(&self) {
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

    /// Run the blocking operation `f` on a thread of the pool. Its
    /// value completes the cell that is returned. Where no thread can
    /// be started, `f` runs on the calling thread before this returns.
    /// If `f` panics, or the VM is gone, the cell is completed with
    /// `failure` of the reason: the error the builtin's signature
    /// declares.
    pub(crate) fn submit(
        &self,
        failure: ErrFactory,
        f: impl FnOnce() -> Value + Send + 'static,
    ) -> IoOp {
        let state = OpState::new(Phase::Queued);
        let op = IoOp {
            cell: state.cell.clone(),
            in_flight: Arc::new(parking_lot::Mutex::new(Some(self.scheduler.external()))),
            pool: self.shared.clone(),
            state: state.clone(),
            stop: None,
        };
        let queued = state;
        let (cell, in_flight) = (op.cell.clone(), op.in_flight.clone());
        let scheduler = self.scheduler.clone();
        let job: IoJob = Box::new(move || {
            let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
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
        let started = std::thread::Builder::new()
            .name("silt-io".into())
            .stack_size(IO_POOL_STACK_BYTES)
            .spawn(move || pool.run());
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

/// Why an I/O operation has no value of its own, with the reason as
/// text.
#[derive(Clone, Copy, Debug)]
pub enum IoFailure<'a> {
    /// A deadline passed (`task.deadline`, `SILT_IO_TIMEOUT`).
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
    /// What keeps the program from being called deadlocked, or ended,
    /// while the operation is in flight and somebody waits for it.
    /// Whoever is done with it first drops it: the thread when the
    /// operation ends, or the waiter when it stops waiting, on every
    /// way out of its wait (see the `Drop`).
    pub(crate) in_flight: Arc<parking_lot::Mutex<Option<External>>>,
    pool: Arc<PoolShared>,
    state: Arc<OpState>,
    /// What makes the operation return when nobody waits for it.
    stop: Option<Box<dyn FnOnce() + Send>>,
}

impl IoOp {
    /// `stop` is called if the waiter goes while the operation runs:
    /// it makes the operation return (it shuts the socket down), so
    /// that its thread ends.
    pub(crate) fn stop_with(mut self, stop: impl FnOnce() + Send + 'static) -> IoOp {
        self.stop = Some(Box::new(stop));
        self
    }

    /// The operation's value, for the waiter whose wait it ended: the
    /// value has reached the task.
    pub(crate) fn take(&self) -> Option<Value> {
        let value = self.cell.get()?.clone();
        self.state
            .taken
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Some(value)
    }

    /// `unheard` is called with the operation's value if the waiter
    /// goes without having taken it ([`IoOp::take`]), once the value
    /// is there, in whichever order the two come. For a value that
    /// must not be lost with a waiter that was cancelled just as it
    /// came (an accepted connection).
    #[cfg(feature = "tcp")]
    pub(crate) fn unheard_with(self, unheard: impl FnOnce(&Value) + Send + 'static) -> IoOp {
        *self.state.unheard.lock() = Some(Box::new(unheard));
        self
    }

    /// Nobody waits for the operation, which counts as pending until
    /// it ends, and its thread as the pool's: it feeds a channel that
    /// the program reads.
    #[cfg(feature = "postgres")]
    pub(crate) fn detach(mut self) {
        self.in_flight = Arc::default();
        self.state = OpState::new(Phase::Left);
    }
}

/// The waiter is gone: it has its value, its deadline passed, it was
/// cancelled, or it was dropped with its task at the end of the
/// program. An operation that nobody waits for is not pending for the
/// program. If it still runs, it is told to stop where it can be, and
/// its thread is the pool's no longer: whether or not it can be
/// stopped (a read of the terminal cannot), it does not use up the
/// pool. If it has not started, it never does.
impl Drop for IoOp {
    fn drop(&mut self) {
        let gone = self.in_flight.lock().take();
        drop(gone);
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
                    match self.stop.is_some() {
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
        if running && let Some(stop) = self.stop.take() {
            stop();
        }
        if finished {
            self.state.unheard();
        }
    }
}

// ── Runtime (shared state) ───────────────────────────────────────

/// Shared, read-only-after-init state for a Silt program.
/// Created once during initialization, then shared across spawned tasks via `Arc`.
pub struct Runtime {
    // ── M:N scheduler ──────────────────────────────────────────
    /// The scheduler of the program's tasks.
    pub(crate) scheduler: Arc<Scheduler>,

    // ── I/O pool ────────────────────────────────────────────────
    /// Thread pool for async I/O operations.
    pub(crate) io_pool: IoPool,

    // ── Host ────────────────────────────────────────────────────
    /// Where the program's output goes and which clock it reads.
    pub(crate) io: HostIo,

    // ── Per-VM generators ───────────────────────────────────────
    /// The state of `math.random`; `None` until the first call seeds it
    /// from the host clock.
    pub(crate) rng: parking_lot::Mutex<Option<u64>>,
    /// The counter that keeps the `uuid.v7`s minted within one
    /// millisecond of the host clock in order.
    pub(crate) uuid_v7: std::sync::Mutex<uuid::ContextV7>,
}

impl Runtime {
    /// End the threads that serve the program: the scheduler's workers
    /// and timer thread and the I/O workers. Called when
    /// the VM that was made for the program is dropped. Tasks that have
    /// not ended never run again, and pending timers never fire.
    ///
    /// The threads are told to end, not waited for: a worker inside a
    /// builtin that blocks ends when the builtin returns.
    pub(super) fn shutdown(&self) {
        self.scheduler.shutdown();
        self.io_pool.stop();
    }

    /// The next value of `math.random`, in `[0, 1)`.
    pub(crate) fn random(&self) -> f64 {
        let mut state = self.rng.lock();
        let mut s = state.unwrap_or_else(|| {
            // splitmix64 of the host clock's time: clocks that read
            // close to each other, or close to zero, still start far
            // apart. xorshift64 must not be seeded with 0.
            let mut z = (self.io.now().as_nanos() as u64).wrapping_add(0x9E37_79B9_7F4A_7C15);
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            (z ^ (z >> 31)).max(1)
        });
        // xorshift64
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        *state = Some(s);
        // Convert to [0.0, 1.0)
        (s >> 11) as f64 / ((1u64 << 53) as f64)
    }
}

// ── Regex cache ──────────────────────────────────────────────────

/// Bounded cache for compiled regex patterns.
///
/// Tracks insertion order with a `VecDeque`. When the cache exceeds
/// `MAX_ENTRIES`, the oldest 25% of entries are evicted instead of
/// clearing the entire cache.
pub(crate) struct RegexCache {
    map: HashMap<String, Regex>,
    order: VecDeque<String>,
}

impl RegexCache {
    const MAX_ENTRIES: usize = 256;
    const EVICT_COUNT: usize = 64; // 25% of MAX_ENTRIES

    pub(super) fn new() -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    /// Return a reference to the cached `Regex` for `pattern`, compiling and
    /// caching it if necessary.
    pub(super) fn get(&mut self, pattern: &str) -> Result<&Regex, VmError> {
        if !self.map.contains_key(pattern) {
            let re =
                Regex::new(pattern).map_err(|e| VmError::new(format!("invalid regex: {e}")))?;
            if self.map.len() >= Self::MAX_ENTRIES {
                // Evict the oldest 25% of entries.
                for _ in 0..Self::EVICT_COUNT {
                    if let Some(old_key) = self.order.pop_front() {
                        self.map.remove(&old_key);
                    }
                }
            }
            self.order.push_back(pattern.to_string());
            self.map.insert(pattern.to_string(), re);
        }
        Ok(self.map.get(pattern).expect("pattern was just inserted"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn failure(failure: IoFailure<'_>) -> Value {
        Value::String(failure.text().to_string())
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

        let op = pool.submit(failure, move || {
            assert_eq!(std::thread::current().id(), here);
            Value::Int(7)
        });
        assert!(matches!(op.cell.get(), Some(Value::Int(7))));
        assert!(op.in_flight.lock().is_none());

        let op = pool.submit(failure, || panic!("no such file"));
        match op.cell.get() {
            Some(Value::String(msg)) => assert_eq!(msg, "panic: no such file"),
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
                pool.submit(failure, move || {
                    let _ = held.lock().recv();
                    Value::Unit
                })
            })
            .collect();
        let quick = pool.submit(failure, || Value::Int(1));
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
            let op = pool.submit(failure, move || Value::Int(n));
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
        let op = pool.submit(failure, || Value::Int(1));
        until("the operation ran", || op.cell.get().is_some());
        until("the idle thread ended", || pool.live_threads() == 0);
        assert_eq!(pool.threads(), 0);
        let op = pool.submit(failure, || Value::Int(2));
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
                pool.submit(failure, move || {
                    running.fetch_add(1, Ordering::SeqCst);
                    let _ = held.lock().recv();
                    Value::Unit
                })
            })
            .collect();
        until("two run", || running.load(Ordering::SeqCst) == 2);
        let refused = pool.submit(failure, || Value::Int(3));
        match refused.cell.get() {
            Some(Value::String(msg)) => {
                assert_eq!(msg, "too many I/O operations in flight (2)")
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
        let next = pool.submit(failure, || Value::Int(4));
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
            let op = pool.submit(failure, move || {
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
        let refused = pool.submit(failure, || Value::Int(1));
        match refused.cell.get() {
            Some(Value::String(msg)) => assert_eq!(
                msg,
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
        let next = pool.submit(failure, || Value::Int(2));
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
            let op = pool
                .submit(failure, move || {
                    started.fetch_add(1, Ordering::SeqCst);
                    let _ = held.lock().recv();
                    Value::Unit
                })
                .stop_with(move || {
                    told.fetch_add(1, Ordering::SeqCst);
                });
            until("it runs", || running.load(Ordering::SeqCst) > before);
            drop(op);
        }
        assert_eq!(told.load(Ordering::SeqCst), many);
        // All of them are still on their way out.
        assert_eq!(pool.live_threads(), many);
        assert_eq!(pool.threads(), 0);
        let next = pool.submit(failure, || Value::Int(1));
        until("the next runs", || next.cell.get().is_some());
        assert!(matches!(next.cell.get(), Some(Value::Int(1))));
        // One that cannot be stopped still counts, beside them.
        let (free, stuck) = mpsc::channel::<()>();
        let op = pool.submit(failure, move || {
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
        let op = pool
            .submit(failure, move || {
                begun.store(true, Ordering::SeqCst);
                while !flag.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Value::Unit
            })
            .stop_with({
                let stopped = stopped.clone();
                move || stopped.store(true, Ordering::SeqCst)
            });
        until("it runs", || started.load(Ordering::SeqCst));
        assert_eq!(pool.threads(), 1);
        drop(op);
        assert!(stopped.load(Ordering::SeqCst));
        assert_eq!(pool.threads(), 0);

        // One that cannot: the pool of one thread still runs the next.
        let (release, held) = mpsc::channel::<()>();
        let op = pool.submit(failure, move || {
            let _ = held.recv();
            Value::Unit
        });
        until("it runs", || *op.state.phase.lock() == Phase::Running);
        drop(op);
        assert_eq!(pool.threads(), 0);
        let next = pool.submit(failure, || Value::Int(2));
        until("the next ran", || next.cell.get().is_some());
        release.send(()).unwrap();
    }

    /// An operation that has not started when its waiter goes never
    /// does.
    #[test]
    fn an_operation_that_is_queued_when_its_waiter_goes_never_runs() {
        let pool = pool(1, NEVER_IDLE);
        let (release, held) = mpsc::channel::<()>();
        let first = pool.submit(failure, move || {
            let _ = held.recv();
            Value::Unit
        });
        until("the first runs", || {
            *first.state.phase.lock() == Phase::Running
        });
        let ran = Arc::new(AtomicBool::new(false));
        let flag = ran.clone();
        let second = pool.submit(failure, move || {
            flag.store(true, Ordering::SeqCst);
            Value::Unit
        });
        drop(second);
        release.send(()).unwrap();
        until("the first ends", || first.cell.get().is_some());
        let third = pool.submit(failure, || Value::Unit);
        until("the third ran", || third.cell.get().is_some());
        assert!(!ran.load(Ordering::SeqCst));
    }

    /// After the VM is gone an operation fails with the module's
    /// error, and the threads end.
    #[test]
    fn a_stopped_pool_fails_its_operations() {
        let pool = pool(IO_POOL_THREADS_MAX, NEVER_IDLE);
        let op = pool.submit(failure, || Value::Int(1));
        until("it ran", || op.cell.get().is_some());
        pool.stop();
        until("the thread ended", || pool.threads() == 0);
        let op = pool.submit(failure, || Value::Int(2));
        match op.cell.get() {
            Some(Value::String(msg)) => assert!(msg.contains(VM_GONE), "{msg}"),
            other => panic!("expected the typed failure, got {other:?}"),
        }
    }
}
