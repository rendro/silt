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

/// The threads of an [`IoPool`], reached through
/// the channel they take their work from.
enum Threads<T> {
    /// Not started: nothing has needed them yet.
    Idle,
    Running(std::sync::mpsc::Sender<T>),
    /// None could be started (a platform without threads): the work
    /// is done by the thread that asks for it.
    Unavailable,
    /// The VM is gone ([`Runtime::shutdown`]): they have been told to
    /// end and are not started again.
    Stopped,
}

impl<T> Threads<T> {
    /// Tell the threads to end: their channel closes, and what they
    /// still held is dropped.
    fn stop(&mut self) {
        *self = Threads::Stopped;
    }
}

/// Why a timer or an I/O operation finds its threads stopped.
const VM_GONE: &str = "the VM that ran the program has been dropped";

/// Upper bound on the number of I/O worker threads when the
/// `SILT_IO_POOL_SIZE` env var is set. More than this is almost
/// certainly misconfiguration (typical OS file-descriptor / blocking
/// thread-pool norms cluster well below this). The cap silently clamps
/// rather than erroring so a misset env var cannot brick startup.
pub(crate) const IO_POOL_SIZE_CAP: usize = 64;

/// Compute the default I/O pool worker count when `SILT_IO_POOL_SIZE` is
/// unset or invalid: `min(available_parallelism, 4)`, falling back to 2
/// only if the platform cannot report parallelism — a single-core host
/// gets 1, NOT 2 (unlike the scheduler worker pool's `.max(2)` deadlock
/// floor; I/O jobs never block on one another, so no minimum is needed).
pub(crate) fn default_io_pool_size() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get().min(4))
        .unwrap_or(2)
}

/// Resolve the I/O pool worker count, honoring the `SILT_IO_POOL_SIZE`
/// environment variable.
///
/// - If unset, returns [`default_io_pool_size`] (`min(cores, 4)`,
///   fallback 2).
/// - If set to a valid `usize > 0`, returns that value clamped at
///   [`IO_POOL_SIZE_CAP`] (currently 64).
/// - If set to `0`, an unparsable string, or anything else invalid,
///   silently falls back to [`default_io_pool_size`]. Matches the
///   shape of `SILT_TIME_SLICE` / `SILT_IO_TIMEOUT` parsing in
///   `scheduler.rs` (no panic, no eprintln, no error result — a typo
///   in the env var must never break startup).
///
/// The env var is read lazily at construction time, so changing it
/// after `Vm::new` has no effect on the running pool.
pub(crate) fn resolve_io_pool_size() -> usize {
    std::env::var("SILT_IO_POOL_SIZE")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .map(|n| n.min(IO_POOL_SIZE_CAP))
        .unwrap_or_else(default_io_pool_size)
}

/// A blocking operation handed to the pool's workers.
type IoJob = Box<dyn FnOnce() + Send>;

pub(crate) struct IoPool {
    io: HostIo,
    scheduler: Arc<Scheduler>,
    /// The workers, started by the first operation. A program that
    /// parks no task on I/O has none, and a platform without threads
    /// can make a VM.
    workers: parking_lot::Mutex<Threads<IoJob>>,
    /// Number of worker threads this pool runs.
    num_workers: usize,
}

impl IoPool {
    pub(super) fn new(num_threads: usize, io: HostIo, scheduler: Arc<Scheduler>) -> Self {
        IoPool {
            io,
            scheduler,
            workers: parking_lot::Mutex::new(Threads::Idle),
            num_workers: num_threads,
        }
    }

    /// End the workers, each when the operation it is running returns.
    fn stop(&self) {
        self.workers.lock().stop();
    }

    /// Start the workers. An error if not one could be started; if some
    /// could, the pool runs with those.
    fn start(&self) -> Result<std::sync::mpsc::Sender<IoJob>, std::io::Error> {
        if self.num_workers == 0 {
            return Err(std::io::Error::other("the pool has no threads"));
        }
        let (tx, rx) = std::sync::mpsc::channel::<IoJob>();
        let rx = Arc::new(parking_lot::Mutex::new(rx));
        let mut started = 0;
        for _ in 0..self.num_workers {
            let rx = rx.clone();
            let spawned = std::thread::Builder::new().spawn(move || {
                loop {
                    let task = {
                        let rx = rx.lock();
                        rx.recv()
                    };
                    match task {
                        Ok(f) => f(),
                        Err(_) => break, // Channel closed
                    }
                }
            });
            match spawned {
                Ok(_) => started += 1,
                Err(e) if started == 0 => return Err(e),
                Err(_) => break,
            }
        }
        Ok(tx)
    }

    /// Number of worker threads spawned for this pool. Test-only:
    /// gated on `cfg(test)` for in-crate unit tests and the
    /// `test-hooks` feature for external integration tests that need
    /// to assert the `SILT_IO_POOL_SIZE` knob propagated to the
    /// running pool. Not part of the public API.
    #[cfg(any(test, feature = "test-hooks"))]
    pub(crate) fn worker_count(&self) -> usize {
        self.num_workers
    }

    /// Run the blocking operation `f` on a worker. Its value completes
    /// the cell that is returned. Where no worker thread can be
    /// started, `f` runs on the calling thread before this returns. If
    /// `f` panics, or the VM is gone, the cell is completed with
    /// `failure` of the reason: the error the builtin's signature
    /// declares.
    pub(crate) fn submit(
        &self,
        failure: ErrFactory,
        f: impl FnOnce() -> Value + Send + 'static,
    ) -> IoOp {
        let op = IoOp {
            cell: Cell::new(),
            in_flight: Arc::new(parking_lot::Mutex::new(Some(self.scheduler.external()))),
        };
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
                    failure(&format!("panic: {msg}"))
                }
            };
            let _ = cell.complete(result, scheduler.wake());
            // After the wake: the operation is in flight no longer.
            let done = in_flight.lock().take();
            drop(done);
        });
        // The VM is gone: the operation fails.
        let fail = |why: &dyn std::fmt::Display| {
            let err = failure(&format!("cannot run an I/O operation: {why}"));
            let _ = op.cell.complete(err, self.scheduler.wake());
            let done = op.in_flight.lock().take();
            drop(done);
        };
        let mut workers = self.workers.lock();
        if let Threads::Idle = *workers {
            *workers = match self.start() {
                Ok(tx) => Threads::Running(tx),
                Err(_) => Threads::Unavailable,
            };
        }
        let sent = match &*workers {
            Threads::Running(tx) => tx.send(job),
            // Without threads the operation runs here, and its value
            // is there when the caller looks.
            Threads::Unavailable => {
                drop(workers);
                job();
                return op;
            }
            Threads::Idle | Threads::Stopped => {
                drop(workers);
                fail(&VM_GONE);
                return op;
            }
        };
        drop(workers);
        if let Err(e) = sent {
            debug_assert!(false, "IoPool worker threads are gone: {e}");
            self.io.err(&format!(
                "silt: IoPool workers unreachable ({e}); IO task will never complete\n"
            ));
        }
        op
    }
}

/// The typed error of a builtin module for a reason given as text: an
/// I/O operation timed out, panicked, or could not run.
pub(crate) type ErrFactory = fn(&str) -> Value;

/// An I/O operation on the pool, as the task that waits for it holds
/// it.
pub(crate) struct IoOp {
    /// Completed with the operation's value.
    pub(crate) cell: Arc<Cell<Value>>,
    /// What keeps the program from being called deadlocked, or ended,
    /// while the operation is in flight and somebody waits for it.
    /// Whoever is done with it first drops it: the worker when the
    /// operation ends, or the waiter when it stops waiting, on every
    /// way out of its wait (see the `Drop`).
    pub(crate) in_flight: Arc<parking_lot::Mutex<Option<External>>>,
}

impl IoOp {
    /// Nobody waits for the operation, which counts as pending until
    /// it ends: it feeds a channel that the program reads.
    #[cfg(feature = "postgres")]
    pub(crate) fn detach(mut self) {
        self.in_flight = Arc::default();
    }
}

/// The waiter is gone: it has its value, its deadline passed, it was
/// cancelled, or it was dropped with its task at the end of the
/// program. An operation that nobody waits for is not pending for the
/// program, whether or not its thread can be told to stop (a read of
/// the terminal cannot).
impl Drop for IoOp {
    fn drop(&mut self) {
        let gone = self.in_flight.lock().take();
        drop(gone);
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

    /// Serialize `SILT_IO_POOL_SIZE` mutations across tests in this module
    /// so concurrent runs don't observe each other's transient values.
    /// `std::sync::Mutex` is sufficient — we never panic while holding it
    /// (each test cleans up via `remove_var` before assertions could fail
    /// in a way that poisons).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// RAII guard: removes `SILT_IO_POOL_SIZE` on drop so a panicking test
    /// can't leak state to its neighbours.
    struct EnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn acquire() -> Self {
            // If a prior test poisoned the lock, recover — env-var state
            // is still safe to clean up below.
            let lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
            // SAFETY: see scheduler.rs:1830 — Rust 1.80+ uses thread-local
            // env caches and these tests do not spawn concurrent env readers.
            unsafe { std::env::remove_var("SILT_IO_POOL_SIZE") };
            EnvGuard { _lock: lock }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: see acquire().
            unsafe { std::env::remove_var("SILT_IO_POOL_SIZE") };
        }
    }

    /// Where no thread can be started (a pool of no workers stands in
    /// for a platform without threads), an operation runs on the thread
    /// that submits it: its value is there when `submit` returns, a
    /// panic in it is the typed failure, and nothing stays pending.
    #[test]
    fn without_threads_an_operation_runs_on_the_caller() {
        fn failure(msg: &str) -> Value {
            Value::String(msg.to_string())
        }
        let io = HostIo::process();
        let scheduler = Arc::new(Scheduler::new(io.clone()));
        let pool = IoPool::new(0, io, scheduler);
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
    }

    #[test]
    fn worker_count_reports_constructor_argument() {
        for n in [1usize, 2, 4, 8, 16] {
            let io = HostIo::process();
            let pool = IoPool::new(n, io.clone(), Arc::new(Scheduler::new(io)));
            assert_eq!(pool.worker_count(), n, "worker_count drift for n={n}");
        }
    }

    #[test]
    fn resolve_io_pool_size_unset_returns_default() {
        let _g = EnvGuard::acquire();
        assert_eq!(resolve_io_pool_size(), default_io_pool_size());
    }

    #[test]
    fn resolve_io_pool_size_env_overrides_to_8() {
        let _g = EnvGuard::acquire();
        // SAFETY: see EnvGuard::acquire.
        unsafe { std::env::set_var("SILT_IO_POOL_SIZE", "8") };
        assert_eq!(resolve_io_pool_size(), 8);
    }

    #[test]
    fn resolve_io_pool_size_zero_falls_back_to_default() {
        let _g = EnvGuard::acquire();
        unsafe { std::env::set_var("SILT_IO_POOL_SIZE", "0") };
        assert_eq!(resolve_io_pool_size(), default_io_pool_size());
    }

    #[test]
    fn resolve_io_pool_size_invalid_falls_back_to_default() {
        let _g = EnvGuard::acquire();
        unsafe { std::env::set_var("SILT_IO_POOL_SIZE", "abc") };
        assert_eq!(resolve_io_pool_size(), default_io_pool_size());
    }

    #[test]
    fn resolve_io_pool_size_negative_falls_back_to_default() {
        let _g = EnvGuard::acquire();
        // "-1" is not a valid usize — must fall back, not silently accept
        // wrap-around or anything else exotic.
        unsafe { std::env::set_var("SILT_IO_POOL_SIZE", "-1") };
        assert_eq!(resolve_io_pool_size(), default_io_pool_size());
    }

    #[test]
    fn resolve_io_pool_size_caps_at_upper_bound() {
        let _g = EnvGuard::acquire();
        unsafe { std::env::set_var("SILT_IO_POOL_SIZE", "100000") };
        assert_eq!(resolve_io_pool_size(), IO_POOL_SIZE_CAP);
    }

    #[test]
    fn resolve_io_pool_size_at_cap_returns_cap() {
        let _g = EnvGuard::acquire();
        unsafe { std::env::set_var("SILT_IO_POOL_SIZE", "64") };
        assert_eq!(resolve_io_pool_size(), IO_POOL_SIZE_CAP);
    }

    #[test]
    fn default_io_pool_size_matches_documented_formula() {
        // Locks the documented formula: min(available_parallelism, 4),
        // where the fallback 2 applies ONLY when available_parallelism()
        // returns Err — NOT as a floor. On a single-core host (e.g. a
        // cgroup cpu limit of 1) the default is legitimately 1, so the
        // valid range is [1, 4], not [2, 4]. The I/O pool needs no
        // deadlock-driven minimum (unlike the scheduler worker pool's
        // `.max(2)`) because I/O jobs never block on one another.
        let n = default_io_pool_size();
        let expected = std::thread::available_parallelism()
            .map(|p| p.get().min(4))
            .unwrap_or(2);
        assert_eq!(
            n, expected,
            "default must be min(available_parallelism, 4) with fallback 2 on Err"
        );
        assert!((1..=4).contains(&n), "default out of [1,4]: {n}");
    }
}
