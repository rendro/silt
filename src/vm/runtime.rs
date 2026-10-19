//! VM runtime types: call frames, blocking reasons, timer manager, I/O pool,
//! shared runtime state, and regex cache.

use regex::Regex;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use crate::bytecode::{Function, VmClosure};
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

/// How many of the tail calls of one frame the log keeps: the latest
/// ones ([`TailCalls`]).
pub(crate) const TCO_ELIDED_CAP: usize = 32;

/// The callers that tail calls replaced: a tail call runs its callee
/// in the caller's frame, so the frames no longer say who called. For
/// each frame that has made tail calls, the log has the function the
/// frame was running and where it had come to, for the latest
/// [`TCO_ELIDED_CAP`] of them. `Vm::enrich_error` reads it, so that an
/// error's call stack names the callers a chain of tail calls went
/// through; a caller's name and place are looked up there, not here.
///
/// A frame's entries go when the frame does ([`TailCalls::prune`]): the
/// log is in the order of the frames, and the frame on top is the one
/// that logs. An entry costs no allocation, and once a frame has made as
/// many tail calls as the log keeps, an entry that takes the place of
/// one of the same function does not change the function's count
/// ([`TailCalls::log`]).
///
/// Lock: `callback_frame_capture__tail_call_chain_keeps_callers` and
/// `callback_frame_capture__tail_call_ring_buffer_caps_chain`
/// (tests/golden/lang/errors), `vm::tests::error_places::in_a_tail_call`.
#[derive(Default)]
pub(crate) struct TailCalls {
    /// For each frame that has made tail calls, the innermost last: its
    /// callers, then its record. (One list: a VM has a log whether its
    /// code makes tail calls or not, and a program can have a hundred
    /// thousand VMs.)
    entries: Vec<TailCall>,
}

enum TailCall {
    /// The function a frame was running when it made a tail call, and
    /// its instruction pointer then, past the call.
    Caller(Arc<Function>, usize),
    /// A frame that has made tail calls: its index in the VM's frames,
    /// and how many it has made. Its callers are the entries before
    /// this one, as many as it has made and at most
    /// [`TCO_ELIDED_CAP`]: in the order they were logged until there
    /// are that many; from then on each takes the place of the oldest.
    Frame { depth: usize, made: usize },
}

impl TailCalls {
    /// Whether no frame has made a tail call.
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The frame at `depth`, the one on top, makes a tail call: it was
    /// running `function` and had come to `ip`.
    ///
    /// The function is cloned only for a new entry, or for one that
    /// takes the place of another function's. When the oldest entry,
    /// the one the new one takes the place of, is of the same function,
    /// as in a function that calls itself or a cycle of functions whose
    /// length divides [`TCO_ELIDED_CAP`], only its `ip` is written: the
    /// function's count, which every task running it shares, is not
    /// touched.
    pub(crate) fn log(&mut self, depth: usize, function: &Arc<Function>, ip: usize) {
        // The frame's record is off the end while its callers change.
        let made = match self.entries.last() {
            Some(&TailCall::Frame { depth: top, made }) if top == depth => {
                self.entries.pop();
                made
            }
            _ => 0,
        };
        if made < TCO_ELIDED_CAP {
            self.entries
                .push(TailCall::Caller(Arc::clone(function), ip));
        } else {
            let start = self.entries.len() - TCO_ELIDED_CAP;
            match &mut self.entries[start + made % TCO_ELIDED_CAP] {
                TailCall::Caller(held, held_ip) if Arc::ptr_eq(held, function) => *held_ip = ip,
                oldest => *oldest = TailCall::Caller(Arc::clone(function), ip),
            }
        }
        self.entries.push(TailCall::Frame {
            depth,
            made: made + 1,
        });
    }

    /// Drop the entries of the frames at `keep_depth` and above: those
    /// frames are gone.
    pub(crate) fn prune(&mut self, keep_depth: usize) {
        while let Some(&TailCall::Frame { depth, made }) = self.entries.last()
            && depth >= keep_depth
        {
            let kept = made.min(TCO_ELIDED_CAP);
            self.entries.truncate(self.entries.len() - 1 - kept);
        }
    }

    /// The callers, each with its frame's depth: the innermost frame's
    /// first, and a frame's latest tail call first.
    pub(crate) fn newest_first(&self) -> impl Iterator<Item = (usize, &Function, usize)> + '_ {
        let mut end = self.entries.len();
        std::iter::from_fn(move || {
            // The record of the next frame down, after its callers.
            let record = end.checked_sub(1)?;
            let TailCall::Frame { depth, made } = self.entries[record] else {
                unreachable!("a frame's record is after its callers")
            };
            let kept = made.min(TCO_ELIDED_CAP);
            let callers = &self.entries[record - kept..record];
            end = record - kept;
            // The latest entry is before the place of the next one.
            let next = made % TCO_ELIDED_CAP;
            Some((0..kept).map(
                move |back| match &callers[(next + kept - 1 - back) % kept] {
                    TailCall::Caller(function, ip) => (depth, &**function, *ip),
                    TailCall::Frame { .. } => {
                        unreachable!("a frame's callers are before its record")
                    }
                },
            ))
        })
        .flatten()
    }
}

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

    /// Whether the task this is the VM of takes nothing new: it was
    /// cancelled, or its time is over (it is the function of a
    /// `task.deadline` that has passed, and a slice of it has begun
    /// since: `TaskHandle::set_over`). The steps such a task still
    /// has are for what it
    /// holds, which it may give (a send that is ready), not for new
    /// work. So every operation that would take something from outside
    /// the task asks this before it looks, and ends the task where it
    /// holds ([`Vm::end_here`], [`Vm::no_wait`]): a receive in each of
    /// its forms, the take of `channel.each` and of a stream's stage or
    /// sink, and the start of an I/O operation ([`Vm::start_io`]). So
    /// does what would start a task whose one purpose is to take in
    /// its place: a stream's stage, and `task.deadline`, which waits
    /// for the function it starts. The arms of a wait ask the same of
    /// the wait's flag (`runtime::sync::park`).
    ///
    /// It is the one condition of that rule, for a cancel and for a
    /// deadline alike. Neither is ever taken back, so an operation
    /// that found it true is refused the wait it then begins: as
    /// cancelled, or as ended by the task's deadline.
    ///
    /// It reads two flags and no clock: a loop of takes under a
    /// deadline was 15% slower with a reading of the clock at each. So
    /// the time is over, for this, from the first slice that begins
    /// past it, which is the moment the count of a late task's steps
    /// begins too. In the slice in which the time passes a take of what
    /// is there may still complete, and is the task's like what a wait
    /// that ended just before had given it; a take that has to wait is
    /// refused by the wait, which reads the clock.
    pub(crate) fn takes_nothing_new(&self) -> bool {
        let set = |flag: &Arc<std::sync::atomic::AtomicBool>| {
            flag.load(std::sync::atomic::Ordering::SeqCst)
        };
        self.cancelled.as_ref().is_some_and(set) || self.over.as_ref().is_some_and(set)
    }

    /// The flags that [`Vm::takes_nothing_new`] reads, for an
    /// operation on the I/O pool that takes something there, after the
    /// task has started it and before the task waits for it: an
    /// accept (`runtime::handle::GiveUp`).
    #[cfg(feature = "tcp")]
    pub(crate) fn nothing_new_flags(&self) -> Vec<Arc<std::sync::atomic::AtomicBool>> {
        self.cancelled.iter().chain(&self.over).cloned().collect()
    }

    /// The wait that ends a task which takes nothing new
    /// ([`Vm::takes_nothing_new`]) where it is: a wait with no arm,
    /// which such a task is refused, as it is refused every wait that
    /// cannot end at once.
    pub(crate) fn no_wait() -> Wait {
        Wait::new(Vec::new())
    }

    /// The step of a builtin `name` that ends the task here
    /// ([`Vm::no_wait`]), for a builtin that has no frame of its own.
    pub(crate) fn end_here(&mut self, name: &'static str) -> Step {
        self.park(name, Vm::no_wait(), |_, _| {
            Err(VmError::new(
                "internal VM error: a task went on that was to take nothing new".into(),
            ))
        })
    }

    /// Hand the blocking operation `f` to the I/O pool
    /// ([`IoPool::submit`]): the one place where a task starts I/O.
    ///
    /// A task that takes nothing new ([`Vm::takes_nothing_new`]) starts
    /// none: nothing is handed to the pool, and the operation that
    /// comes back never runs ([`IoPool::withheld`]). The wait for it,
    /// which the caller begins, ends the task.
    pub(crate) fn start_io(
        &self,
        failure: ErrFactory,
        f: impl FnOnce() -> Value + Send + 'static,
    ) -> IoOp {
        match self.takes_nothing_new() {
            true => self.runtime.io_pool.withheld(),
            false => self.runtime.io_pool.submit(failure, f),
        }
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

    /// Test-only: see `PanicAt::Io`.
    #[cfg(any(test, feature = "test-hooks"))]
    pub(crate) fn panic_for_test(&self) {
        (self.shared.panic_next).store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// An operation that is not handed to the pool: it never runs,
    /// its cell is never completed, and nothing is done when its
    /// waiter goes (it is not told to stop: there is nothing to stop).
    /// See [`Vm::start_io`].
    pub(crate) fn withheld(&self) -> IoOp {
        let state = OpState::new(Phase::Left);
        IoOp {
            cell: state.cell.clone(),
            in_flight: Arc::default(),
            pool: self.shared.clone(),
            state,
            stop: None,
            unless_taken: false,
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
        f: impl FnOnce() -> Value + Send + 'static,
    ) -> IoOp {
        let state = OpState::new(Phase::Queued);
        let op = IoOp {
            cell: state.cell.clone(),
            in_flight: Arc::new(parking_lot::Mutex::new(Some(self.scheduler.external()))),
            pool: self.shared.clone(),
            state: state.clone(),
            stop: None,
            unless_taken: false,
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
    /// `stop` is called whenever the waiter goes without the
    /// operation's value ([`IoOp::stop_unless_taken`]).
    unless_taken: bool,
    /// It was never handed to the pool ([`IoPool::withheld`]).
    withheld: bool,
}

impl IoOp {
    /// `stop` is called if the waiter goes while the operation runs:
    /// it makes the operation return (it shuts the socket down), so
    /// that its thread ends.
    pub(crate) fn stop_with(mut self, stop: impl FnOnce() + Send + 'static) -> IoOp {
        self.stop = Some(Box::new(stop));
        self
    }

    /// [`IoOp::stop_with`] for an operation on something that is of
    /// no use to a waiter who did not get the operation's value: a
    /// connection, where a read that nobody heard has taken bytes
    /// that are gone with it. `stop` is called when the waiter goes
    /// without having taken the value ([`IoOp::take`]), whatever the
    /// operation was doing then: it had not begun, it ran, or it had
    /// ended. What the waiter is left with does not depend on the
    /// moment at which it stopped waiting.
    #[cfg(feature = "tcp")]
    pub(crate) fn stop_unless_taken(mut self, stop: impl FnOnce() + Send + 'static) -> IoOp {
        self.stop = Some(Box::new(stop));
        self.unless_taken = true;
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
    /// came (an accepted connection, a transaction that has begun).
    #[cfg(any(feature = "tcp", feature = "postgres"))]
    pub(crate) fn unheard_with(self, unheard: impl FnOnce(&Value) + Send + 'static) -> IoOp {
        *self.state.unheard.lock() = Some(Box::new(unheard));
        self
    }

    /// Nobody waits for the operation, and it counts as pending no
    /// longer; its thread stays the pool's. It feeds a channel that
    /// the program reads, and counts as what it holds itself for that:
    /// a task while it runs ([`Scheduler::enter`]), or something
    /// outside the tasks that can end a wait ([`Scheduler::external`]).
    /// The caller takes the one or the other before the operation is
    /// submitted, so that there is no moment at which it counts as
    /// nothing.
    ///
    /// [`Scheduler::enter`]: crate::scheduler::Scheduler::enter
    /// [`Scheduler::external`]: crate::scheduler::Scheduler::external
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
/// pool. If it has not started, it never does. An operation that is to
/// be stopped whenever its value did not reach the waiter
/// ([`IoOp::stop_unless_taken`]) is told in every one of these cases.
impl Drop for IoOp {
    fn drop(&mut self) {
        // Nothing was started, so nothing is left to stop or to count.
        if self.withheld {
            return;
        }
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
        // The waiter decides, not where the operation stood.
        let unheard =
            self.unless_taken && !self.state.taken.load(std::sync::atomic::Ordering::SeqCst);
        if (running || unheard)
            && let Some(stop) = self.stop.take()
        {
            stop();
        }
        if finished {
            self.state.unheard();
        }
    }
}

// ── Runtime (shared state) ───────────────────────────────────────

/// What the VM that runs the program's own code has of the program,
/// for the VMs of its tasks to take. A REPL session's VM takes in a
/// program for each entry, each with the slots and tables of the
/// entries before it and its own after them; a task started in an
/// earlier entry finds here what the later ones added.
#[derive(Default)]
pub(crate) struct Newest {
    /// How often `view` was written. A VM that has the view of this
    /// count has the newest: it need not take the lock to know.
    pub(crate) count: std::sync::atomic::AtomicU64,
    pub(crate) view: parking_lot::Mutex<View>,
}

/// What a VM has of the program: the values of its global slots, and
/// what each slot is.
#[derive(Clone)]
pub(crate) struct View {
    /// The values, each unset until its definition's code has run. There
    /// may be more slots than the program has definitions: room for
    /// those of the next REPL entries.
    pub(crate) slots: Arc<[std::sync::OnceLock<Value>]>,
    /// What each global slot is, and the slot of each impl method: what
    /// the VM looks a method up in. For the VM of a task this is what
    /// STANDS of the program ([`crate::session::Program::settled`]):
    /// nothing in it is ever taken back or changed, so what the VM
    /// finds in it needs no second look.
    pub(crate) table: Arc<crate::bytecode::Globals>,
    /// All of the program that runs now: `table`, and what the REPL
    /// entry being run adds to it, which is gone again if the entry
    /// fails. The VM of a task looks here where `table` has nothing.
    /// (The VM that runs the entry itself has this as its `table`.)
    pub(crate) entry: Arc<crate::bytecode::Globals>,
}

impl Default for View {
    fn default() -> Self {
        // (One table for both: a VM whose `table` is its `entry` has
        // all of its program, see `Vm::wider_table`.)
        let table: Arc<crate::bytecode::Globals> = Arc::default();
        View {
            slots: Arc::from([]),
            entry: table.clone(),
            table,
        }
    }
}

impl View {
    /// The value of the global slot `slot`, if it is set.
    pub(crate) fn global(&self, slot: u16) -> Option<&Value> {
        self.slots.get(usize::from(slot))?.get()
    }

    /// Make this view the program's newest if it is not (`count` says
    /// which it is). See [`Vm::look_again`], which is this for a VM.
    pub(crate) fn renew(&mut self, count: &mut u64, newest: &Newest) -> bool {
        use std::sync::atomic::Ordering;
        if newest.count.load(Ordering::Relaxed) == *count {
            return false;
        }
        let view = newest.view.lock();
        *self = view.clone();
        *count = newest.count.load(Ordering::Relaxed);
        true
    }

    /// A read of the global slot `slot` found no value: its value in
    /// the program's newest slots, or the error.
    ///
    /// The VM of a task comes here for a slot of a REPL entry later
    /// than the one it was started in (it runs a closure of that
    /// entry). Nothing else does: the checker orders the top-level
    /// `let`s so that each is set before anything can read it, and the
    /// definitions are set before any `let` runs. That holds for a task
    /// as for the program's own code, since a task has the code of a
    /// later entry only from that entry's code, which ran after what it
    /// reads was set. A slot that is not set in the newest slots either
    /// is a bug of silt, and said so.
    #[cold]
    #[inline(never)]
    pub(crate) fn global_of_newest(
        &mut self,
        count: &mut u64,
        newest: &Newest,
        slot: u16,
    ) -> Result<Value, VmError> {
        self.renew(count, newest);
        self.global(slot).cloned().ok_or_else(|| {
            VmError::type_confusion(format!(
                "the global '{}' is read before it is set",
                self.entry.name(slot)
            ))
        })
    }
}

/// The VM's own copies of the closures it has read from the global
/// slots (top-level functions, impl methods), by slot.
///
/// A closure in a global slot is one allocation for every VM of the
/// program, and each call of it counts a reference to it up and down:
/// tasks on several cores that call one function would each take the
/// cache line of that count from the others, every call. So a VM calls
/// a copy of its own, made the first time it reads the slot: the same
/// `Function`, the same captures, and so the same value
/// (src/value/key.rs).
///
/// With each copy is kept the closure it was copied from, and a copy is
/// used only for that one: a slot that is set again (an embedder runs
/// another program on the VM, see [`Vm::set_global`]) holds another
/// closure, which is copied in turn. (The closure kept also keeps its
/// address from being that of another.)
///
/// The copies are in a table of their own size, not one of the
/// program's slots: a task that calls one function of a program of
/// thousands holds one copy. The copy of the slot `s` is at the place
/// `s` modulo the number of places, or at the first place after it
/// that is free or has it; at most half of the places are taken.
#[derive(Default)]
pub(crate) struct OwnClosures {
    /// A power of two of places, or none.
    places: Vec<Option<Copied>>,
    /// How many places are taken.
    taken: usize,
}

/// The copy of the closure of a global slot.
struct Copied {
    slot: u16,
    /// The closure in the slot when it was copied.
    shared: Arc<VmClosure>,
    own: Arc<VmClosure>,
}

impl OwnClosures {
    /// This VM's copy of `shared`, the closure in the global slot
    /// `slot`.
    #[inline(always)]
    pub(crate) fn of(&mut self, slot: u16, shared: &Arc<VmClosure>) -> Arc<VmClosure> {
        // (With no places, the mask is all ones and the first look
        // finds none.)
        let mask = self.places.len().wrapping_sub(1);
        let mut at = usize::from(slot) & mask;
        while let Some(Some(copied)) = self.places.get(at) {
            if copied.slot == slot {
                if Arc::ptr_eq(&copied.shared, shared) {
                    return copied.own.clone();
                }
                break;
            }
            at = (at + 1) & mask;
        }
        self.copy(slot, shared)
    }

    /// The value `value` of the global slot `slot` as this VM uses it:
    /// its copy if it is a closure.
    pub(crate) fn value(&mut self, slot: u16, value: Value) -> Value {
        match value {
            Value::VmClosure(shared) => Value::VmClosure(self.of(slot, &shared)),
            value => value,
        }
    }

    /// Copy `shared`, the closure in the global slot `slot`, for this
    /// VM, in place of any copy the slot had.
    #[cold]
    #[inline(never)]
    fn copy(&mut self, slot: u16, shared: &Arc<VmClosure>) -> Arc<VmClosure> {
        let own = Arc::new(VmClosure {
            function: shared.function.clone(),
            upvalues: shared.upvalues.clone(),
        });
        self.forget(slot);
        if 2 * (self.taken + 1) > self.places.len() {
            let places = (2 * self.places.len()).max(8);
            let copies = std::mem::replace(&mut self.places, (0..places).map(|_| None).collect());
            copies
                .into_iter()
                .flatten()
                .for_each(|copied| self.put(copied));
        }
        self.put(Copied {
            slot,
            shared: shared.clone(),
            own: own.clone(),
        });
        self.taken += 1;
        own
    }

    /// Put `copied` at its place, in a table with a free place and no
    /// copy of its slot.
    fn put(&mut self, copied: Copied) {
        let mask = self.places.len() - 1;
        let mut at = usize::from(copied.slot) & mask;
        while self.places[at].is_some() {
            at = (at + 1) & mask;
        }
        self.places[at] = Some(copied);
    }

    /// How many places the table has.
    #[cfg(test)]
    pub(crate) fn places(&self) -> usize {
        self.places.len()
    }

    /// Let go of the copy of the closure that was in the global slot
    /// `slot`, if there is one: the slot is set again. (Seldom: the
    /// copies are put in their places again without it.)
    pub(crate) fn forget(&mut self, slot: u16) {
        let had = |copied: &Option<Copied>| matches!(copied, Some(copied) if copied.slot == slot);
        if !self.places.iter().any(had) {
            return;
        }
        let places = self.places.len();
        let copies = std::mem::replace(&mut self.places, (0..places).map(|_| None).collect());
        for copied in copies.into_iter().flatten() {
            match copied.slot == slot {
                true => self.taken -= 1,
                false => self.put(copied),
            }
        }
    }
}

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
    /// The slice an embedder set ([`Vm::set_time_slice`]); 0 while it
    /// has set none.
    pub(crate) time_slice: std::sync::atomic::AtomicUsize,
    /// The steps the program may still run ([`Vm::set_step_budget`]);
    /// `u64::MAX` while it has no budget.
    pub(crate) steps_left: std::sync::atomic::AtomicU64,

    // ── The program ─────────────────────────────────────────────
    /// The global slots and the tables of the newest program the VM
    /// that owns this runtime took in, for the VMs of its tasks
    /// ([`Vm::look_again`]).
    pub(crate) newest: Newest,
    /// The program's types, by id: the decoders find the record types
    /// of record fields here ([`Vm::type_info`]). One table for the VM
    /// and the VMs of its tasks; a REPL entry adds its types to it.
    pub(crate) types: parking_lot::Mutex<crate::typeinfo::TypeTable>,

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

    /// Take `used` steps, those of a slice that ran its full length,
    /// off the step budget. The error of a program whose budget is
    /// used up, from then on at every call.
    pub(crate) fn charge(&self, used: usize) -> Result<(), VmError> {
        use std::sync::atomic::Ordering::Relaxed;
        if self.steps_left.load(Relaxed) == u64::MAX {
            return Ok(());
        }
        let used = used as u64;
        let mut left = self.steps_left.load(Relaxed);
        // (A loop of its own: `fetch_update` is deprecated on newer
        // toolchains, and its new name is not on older ones.)
        while let Err(now) =
            self.steps_left
                .compare_exchange_weak(left, left.saturating_sub(used), Relaxed, Relaxed)
        {
            left = now;
        }
        match left > used {
            true => Ok(()),
            false => Err(VmError::budget_used_up()),
        }
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
    /// caching it if necessary; the error of a pattern that does not
    /// compile.
    pub(super) fn get(&mut self, pattern: &str) -> Result<&Regex, regex::Error> {
        if !self.map.contains_key(pattern) {
            let re = Regex::new(pattern)?;
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

        let op = pool.submit(failure, move || {
            assert_eq!(std::thread::current().id(), here);
            Value::Int(7)
        });
        assert!(matches!(op.cell.get(), Some(Value::Int(7))));
        assert!(op.in_flight.lock().is_none());

        let op = pool.submit(failure, || panic!("no such file"));
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

    /// An operation on something that is of no use to a waiter who
    /// did not get its value ([`IoOp::stop_unless_taken`]) is told to
    /// stop whenever its waiter goes without the value: while it was
    /// queued, while it ran, and when its value was there and nobody
    /// had taken it. A waiter that took the value leaves it alone, and
    /// so does one of an operation that is only stopped while it runs
    /// ([`IoOp::stop_with`]).
    #[test]
    #[cfg(feature = "tcp")]
    fn a_waiter_that_goes_without_the_value_stops_the_operation_wherever_it_stands() {
        let pool = pool(1, NEVER_IDLE);
        let told = Arc::new(AtomicUsize::new(0));
        let tell = || {
            let told = told.clone();
            move || {
                told.fetch_add(1, Ordering::SeqCst);
            }
        };
        let told = || told.load(Ordering::SeqCst);
        // One runs, in the pool's one thread.
        let (release, held) = mpsc::channel::<()>();
        let running = pool
            .submit(failure, move || {
                let _ = held.recv();
                Value::Unit
            })
            .stop_unless_taken(tell());
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
            let op = IoOp {
                cell: state.cell.clone(),
                in_flight: Arc::default(),
                pool: pool.shared.clone(),
                state,
                stop: None,
                unless_taken: false,
                withheld: false,
            };
            match stop {
                true => op.stop_unless_taken(tell()),
                false => op.stop_with(tell()),
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
        let unheard = pool
            .submit(failure, || Value::Int(2))
            .stop_unless_taken(tell());
        until("its value is there", || {
            *unheard.state.phase.lock() == Phase::Finished
        });
        drop(unheard);
        assert_eq!(told(), 3);
        // The value is there, and the waiter takes it.
        let heard = pool
            .submit(failure, || Value::Int(3))
            .stop_unless_taken(tell());
        until("its value is there", || {
            *heard.state.phase.lock() == Phase::Finished
        });
        assert!(matches!(heard.take(), Some(Value::Int(3))));
        drop(heard);
        assert_eq!(told(), 3);
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
