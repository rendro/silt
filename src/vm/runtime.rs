//! VM runtime types: call frames, blocking reasons, timer manager, I/O pool,
//! shared runtime state, and regex cache.

use regex::Regex;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use crate::bytecode::VmClosure;
use crate::runtime::channel::Channel;
use crate::runtime::completion::IoCompletion;
use crate::runtime::handle::TaskHandle;
use crate::value::Value;

use super::{HostIo, VmError};

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

// ── Suspended invocation (for yield inside invoke_callable) ─────

/// The state of an `invoke_callable` that was interrupted by a yield (e.g.
/// an IO builtin yielding inside a callback passed to `channel.each`).
/// Stored on the VM so the caller can resume the callback instead of
/// re-running it from scratch.
///
/// Every callable that yields inside `invoke_callable` leaves exactly one
/// of these behind, whatever kind of callable it is. Callers rely on that:
/// they read `suspended_invoke.is_some()` as "my callback is mid-call" and
/// hand the state to `resume_suspended_invoke`.
pub(crate) enum SuspendedInvoke {
    /// A closure whose body was interrupted.
    Closure {
        /// The extra call frames that were pushed by invoke_callable.
        frames: Vec<CallFrame>,
        /// The stack values above `func_slot` (includes locals,
        /// temporaries, and any args re-pushed by the yielding builtin).
        stack: Vec<Value>,
        /// The stack index where the callback's "function slot" dummy
        /// lives.
        func_slot: usize,
    },
    /// A builtin passed as a function value (`list.map(chans,
    /// channel.receive)`) that yielded. A builtin has no frames to save:
    /// it is resumed by calling it again.
    Builtin {
        /// Qualified name of the builtin (e.g. "channel.receive").
        name: String,
        /// The arguments to call it with on resume: the ones the builtin
        /// re-pushed when it yielded, which it may have rewritten to
        /// carry its own resume state.
        args: Vec<Value>,
    },
}

// ── Suspended higher-order builtin iteration ────────────────────

/// Accumulator shapes for higher-order builtins that have been suspended
/// mid-iteration because their callback yielded.
#[allow(clippy::large_enum_variant)]
pub(crate) enum BuiltinAcc {
    /// No accumulator (e.g. `each`).
    Unit,
    /// A growing list of values (e.g. `map`, `filter`, `flat_map`, `set.map`).
    List(Vec<Value>),
    /// A running fold value (e.g. `fold`, `fold_until`).
    Fold(Value),
    /// Sort-key/item pairs (e.g. `sort_by`).
    SortPairs(Vec<(Value, Value)>),
    /// Group-by accumulator.
    Groups(std::collections::BTreeMap<Value, Vec<Value>>),
    /// Map entries accumulator (e.g. `map.filter`, `map.map`).
    MapEntries(std::collections::BTreeMap<Value, Value>),
    /// Best (key, item) so far for min_by/max_by; `None` until first item.
    Best(Option<(Value, Value)>),
    /// Scan accumulator: running value + accumulating prefix list.
    Scan(Value, Vec<Value>),
    /// Generic "current state" carrier (e.g. `list.unfold`'s state seed,
    /// `stream.fold`'s running accumulator).  Items grow into the optional
    /// `Vec<Value>` (used by `unfold` for the result list; `stream.fold`
    /// uses just the `Value`).
    State(Value, Vec<Value>),
}

/// State for a higher-order builtin whose callback yielded mid-iteration.
///
/// When a callback (e.g. `io.read_file` inside a `list.map`) yields, the
/// builtin stashes its partial state here and re-pushes its own args so the
/// outer `CallBuiltin` opcode will re-dispatch it on resume.  The builtin
/// then picks up from `next_index` using `acc` as its running accumulator.
pub(crate) struct SuspendedBuiltin {
    /// Qualified name of the builtin (e.g. "list.map") for validation.
    pub(crate) name: String,
    /// The materialized list of items being iterated over.  Stored as a
    /// `Vec<Value>` rather than re-iterating the original collection so that
    /// Range and lazy iterators work correctly across yields.
    pub(crate) items: Vec<Value>,
    /// Index of the next item to process (0-indexed into `items`).
    pub(crate) next_index: usize,
    /// The callback value (closure or BuiltinFn).
    pub(crate) callback: Value,
    /// The accumulator so far.
    pub(crate) acc: BuiltinAcc,
}

// ── Block reason (for M:N scheduler) ────────────────────────────

/// Describes whether a select operation is a receive or send.
#[derive(Clone)]
pub(crate) enum SelectOpKind {
    Receive,
    Send,
}

pub(crate) enum BlockReason {
    /// Blocked on channel.receive (channel was empty).
    Receive(Arc<Channel>),
    /// Blocked on channel.send (channel buffer was full).
    Send(Arc<Channel>),
    /// Blocked on channel.select — carries channels with their operation kinds.
    Select(Vec<(Arc<Channel>, SelectOpKind)>),
    /// Blocked on task.join (target task not yet complete).
    Join(Arc<TaskHandle>),
    /// Blocked on I/O completion.
    Io(Arc<IoCompletion>),
}

// ── Timer manager (shared single-thread timer wheel) ────────────

/// Target to fire when a scheduled deadline expires. Channel targets are
/// closed (used by `channel.timeout`); Completion targets are marked
/// complete with `Value::Unit` (used by `time.sleep`).
pub(crate) enum TimerTarget {
    Channel(Arc<Channel>),
    Completion(Arc<IoCompletion>),
}

/// A deadline and what to fire at it, as sent to the timer thread. The
/// deadline is a reading of the host clock.
type TimerRequest = (Duration, TimerTarget);

/// Manages all pending timer deadlines on a single background thread.
/// Instead of spawning one OS thread per `channel.timeout` or `time.sleep`,
/// all deadlines are submitted here and fired from a single long-lived
/// thread. This keeps timer cost O(1) threads regardless of how many
/// concurrent sleepers/timeouts exist.
///
/// The thread is started by the first deadline, so a program without
/// timers has none, and a platform without threads can make a VM.
pub(crate) struct TimerManager {
    io: HostIo,
    thread: parking_lot::Mutex<Threads<TimerRequest>>,
}

/// The threads of a [`TimerManager`] or an [`IoPool`], reached through
/// the channel they take their work from.
enum Threads<T> {
    /// Not started: nothing has needed them yet.
    Idle,
    Running(std::sync::mpsc::Sender<T>),
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

impl TimerManager {
    pub(super) fn new(io: HostIo) -> Self {
        TimerManager {
            io,
            thread: parking_lot::Mutex::new(Threads::Idle),
        }
    }

    /// End the timer thread. The deadlines that are pending are
    /// discarded: they never fire.
    fn stop(&self) {
        self.thread.lock().stop();
    }

    /// The timer thread's loop: take in deadlines, and fire each when
    /// the host clock reaches it.
    fn run(io: HostIo, rx: std::sync::mpsc::Receiver<TimerRequest>) {
        let mut deadlines: BTreeMap<Duration, Vec<TimerTarget>> = BTreeMap::new();
        loop {
            // Calculate how long to sleep until the next deadline.
            let timeout = deadlines
                .first_key_value()
                .map(|(deadline, _)| io.real_wait(deadline.saturating_sub(io.monotonic())))
                .unwrap_or(Duration::from_secs(60));

            // Wait for a new timeout request or until the next deadline fires.
            match rx.recv_timeout(timeout) {
                Ok((deadline, target)) => {
                    deadlines.entry(deadline).or_default().push(target);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }

            // Fire all expired deadlines. The timer thread owns its own
            // BTreeMap and holds no scheduler locks here, so firing
            // `completion.complete(...)` (which runs wakers → requeue →
            // watchdog.remove) is safe in a disjoint lock domain.
            // If the clock has panicked, every wait ends now: what
            // was waiting runs into the clock's failure at its next
            // builtin call.
            let now = io.monotonic();
            let now = if io.clock_failure().is_some() {
                Duration::MAX
            } else {
                now
            };
            let expired: Vec<Duration> = deadlines.range(..=now).map(|(k, _)| *k).collect();
            for key in expired {
                if let Some(targets) = deadlines.remove(&key) {
                    for target in targets {
                        match target {
                            TimerTarget::Channel(ch) => ch.close(),
                            TimerTarget::Completion(c) => {
                                c.complete(Value::Unit);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Hand `target` to the timer thread, to fire after `delay` on the
    /// host clock. Starts the thread if this is the first deadline; an
    /// error if it cannot be started (a platform without threads).
    fn submit(&self, delay: Duration, target: TimerTarget) -> Result<(), VmError> {
        let unavailable =
            |why: &dyn std::fmt::Display| VmError::new(format!("cannot start a timer: {why}"));
        let deadline = self
            .io
            .deadline_after(delay)
            .ok_or_else(|| unavailable(&"the duration is out of range"))?;
        if let Some(failure) = self.io.clock_failure() {
            return Err(VmError::new(failure));
        }
        let mut thread = self.thread.lock();
        if let Threads::Idle = *thread {
            let (tx, rx) = std::sync::mpsc::channel::<TimerRequest>();
            let io = self.io.clone();
            std::thread::Builder::new()
                .spawn(move || TimerManager::run(io, rx))
                .map_err(|e| unavailable(&e))?;
            *thread = Threads::Running(tx);
        }
        match &*thread {
            Threads::Running(tx) => tx.send((deadline, target)).map_err(|e| unavailable(&e)),
            Threads::Idle | Threads::Stopped => Err(unavailable(&VM_GONE)),
        }
    }

    /// Schedule a channel to be closed after `delay`.
    pub(crate) fn schedule(&self, delay: Duration, ch: Arc<Channel>) -> Result<(), VmError> {
        // Tell the channel it has an incoming close so the main-thread
        // deadlock check doesn't fire while the timer is pending.
        ch.mark_pending_timer_close();
        self.submit(delay, TimerTarget::Channel(ch))
    }

    /// Schedule an `IoCompletion` to be completed with `Value::Unit` after
    /// `delay`. Used by `time.sleep` to cooperatively park a scheduled task
    /// without consuming an I/O worker thread. Multiple concurrent sleepers
    /// all share the single timer thread.
    pub(crate) fn schedule_completion(
        &self,
        delay: Duration,
        completion: Arc<IoCompletion>,
    ) -> Result<(), VmError> {
        self.submit(delay, TimerTarget::Completion(completion))
    }
}

// ── I/O thread pool ─────────────────────────────────────────────

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
    /// The workers, started by the first operation. A program that
    /// parks no task on I/O has none, and a platform without threads
    /// can make a VM.
    workers: parking_lot::Mutex<Threads<IoJob>>,
    /// Number of worker threads this pool runs.
    num_workers: usize,
}

impl IoPool {
    pub(super) fn new(num_threads: usize, io: HostIo) -> Self {
        IoPool {
            io,
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

    /// Submit a blocking I/O operation using a caller-supplied
    /// completion handle. The handle's `timeout_err` factory determines
    /// the typed variant the scheduler watchdog surfaces when the
    /// task's deadline cancels this op.
    pub(crate) fn submit_with(
        &self,
        completion: Arc<IoCompletion>,
        f: impl FnOnce() -> Value + Send + 'static,
    ) -> Arc<IoCompletion> {
        let completion2 = completion.clone();
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
                    // Route the panic message through the completion's
                    // typed-error factory rather than emitting the legacy
                    // untyped `Err(String)` shape that bypassed every
                    // caller's typed match arms (callers typecheck as
                    // `Result(T, PgError)` / `Result(T, TcpError)` /
                    // `Result(T, IoError)` etc — a stringly-shaped
                    // `Err` matched no arm). Using `build_timeout_err`
                    // here is semantically a slight bend (panic !=
                    // timeout) but it is the right shape: each
                    // factory's "unknown / timeout" variant is the
                    // closest typed bucket for an unexpected internal
                    // failure, and the "panic: " prefix preserves the
                    // distinction in the message text. Without this,
                    // user `match` arms over the typed error enum
                    // never fire on the panic path.
                    let prefixed = format!("panic: {msg}");
                    completion2.build_timeout_err(&prefixed)
                }
            };
            completion2.complete(result);
        });
        // Nothing can run the operation: it fails, with the error its
        // builtin's signature declares.
        let fail = |why: &dyn std::fmt::Display| {
            let err = completion.build_timeout_err(&format!("cannot run an I/O operation: {why}"));
            completion.complete(err);
        };
        let mut workers = self.workers.lock();
        if let Threads::Idle = *workers {
            match self.start() {
                Ok(tx) => *workers = Threads::Running(tx),
                Err(e) => {
                    drop(workers);
                    fail(&e);
                    return completion;
                }
            }
        }
        let sent = match &*workers {
            Threads::Running(tx) => tx.send(job),
            Threads::Idle | Threads::Stopped => {
                drop(workers);
                fail(&VM_GONE);
                return completion;
            }
        };
        drop(workers);
        if let Err(e) = sent {
            debug_assert!(false, "IoPool worker threads are gone: {e}");
            self.io.err(&format!(
                "silt: IoPool workers unreachable ({e}); IO task will never complete\n"
            ));
        }
        completion
    }
}

// ── Runtime (shared state) ───────────────────────────────────────

/// Shared, read-only-after-init state for a Silt program.
/// Created once during initialization, then shared across spawned tasks via `Arc`.
pub struct Runtime {
    // ── M:N scheduler ──────────────────────────────────────────
    /// The shared scheduler for spawned tasks (None until first task.spawn).
    pub(super) scheduler: parking_lot::Mutex<Option<Arc<crate::scheduler::Scheduler>>>,

    // ── Timer manager ──────────────────────────────────────────
    /// Shared timer thread for `channel.timeout`.
    pub(crate) timer: TimerManager,

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
    /// and watchdog, the timer thread and the I/O workers. Called when
    /// the VM that was made for the program is dropped. Tasks that have
    /// not ended never run again, and pending timers never fire.
    ///
    /// The threads are told to end, not waited for: a worker inside a
    /// builtin that blocks ends when the builtin returns.
    pub(super) fn shutdown(&self) {
        // A scheduler is made if there was none, so that a thread that
        // outlives the VM (a stream stage) cannot start one.
        let scheduler = self
            .scheduler
            .lock()
            .get_or_insert_with(|| Arc::new(crate::scheduler::Scheduler::new(self.io.clone())))
            .clone();
        scheduler.shutdown();
        self.timer.stop();
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

    #[test]
    fn worker_count_reports_constructor_argument() {
        for n in [1usize, 2, 4, 8, 16] {
            let pool = IoPool::new(n, HostIo::process());
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
