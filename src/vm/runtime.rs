//! VM runtime types: call frames, blocking reasons, timer manager, I/O pool,
//! shared runtime state, and regex cache.

use regex::Regex;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::bytecode::VmClosure;
use crate::value::{Channel, IoCompletion, TaskHandle, Value};

use super::VmError;

/// Type alias for foreign (Rust-side) functions registered with the VM.
pub(crate) type ForeignFn = Arc<dyn Fn(&[Value]) -> Result<Value, VmError> + Send + Sync>;

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

/// Captures the frames and stack portion from an `invoke_callable` that was
/// interrupted by a yield (e.g. an IO builtin yielding inside a callback
/// passed to `channel.each`).  Stored on the VM so the caller can resume
/// the callback instead of re-running it from scratch.
pub(crate) struct SuspendedInvoke {
    /// The extra call frames that were pushed by invoke_callable.
    pub(crate) frames: Vec<CallFrame>,
    /// The stack values above `func_slot` (includes locals, temporaries, and
    /// any args re-pushed by the yielding builtin).
    pub(crate) stack: Vec<Value>,
    /// The stack index where the callback's "function slot" dummy lives.
    pub(crate) func_slot: usize,
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

/// Manages all pending timer deadlines on a single background thread.
/// Instead of spawning one OS thread per `channel.timeout` or `time.sleep`,
/// all deadlines are submitted here and fired from a single long-lived
/// thread. This keeps timer cost O(1) threads regardless of how many
/// concurrent sleepers/timeouts exist.
pub(crate) struct TimerManager {
    sender: parking_lot::Mutex<std::sync::mpsc::Sender<(Instant, TimerTarget)>>,
}

impl TimerManager {
    pub(super) fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<(Instant, TimerTarget)>();
        std::thread::spawn(move || {
            let mut deadlines: BTreeMap<Instant, Vec<TimerTarget>> = BTreeMap::new();
            loop {
                // Calculate how long to sleep until the next deadline.
                let timeout = deadlines
                    .first_key_value()
                    .map(|(deadline, _)| deadline.saturating_duration_since(Instant::now()))
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
                let now = Instant::now();
                let expired: Vec<Instant> = deadlines.range(..=now).map(|(k, _)| *k).collect();
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
        });
        TimerManager {
            sender: parking_lot::Mutex::new(tx),
        }
    }

    /// Schedule a channel to be closed after `delay`.
    pub(crate) fn schedule(&self, delay: Duration, ch: Arc<Channel>) {
        let deadline = Instant::now() + delay;
        // Tell the channel it has an incoming close so the main-thread
        // deadlock check doesn't fire while the timer is pending.
        ch.mark_pending_timer_close();
        if let Err(e) = self
            .sender
            .lock()
            .send((deadline, TimerTarget::Channel(ch)))
        {
            debug_assert!(false, "TimerManager worker thread is gone: {e}");
            eprintln!(
                "silt: TimerManager worker thread unreachable ({e}); channel.timeout will not fire"
            );
        }
    }

    /// Schedule an `IoCompletion` to be completed with `Value::Unit` after
    /// `delay`. Used by `time.sleep` to cooperatively park a scheduled task
    /// without consuming an I/O worker thread. Multiple concurrent sleepers
    /// all share the single timer thread.
    pub(crate) fn schedule_completion(&self, delay: Duration, completion: Arc<IoCompletion>) {
        let deadline = Instant::now() + delay;
        if let Err(e) = self
            .sender
            .lock()
            .send((deadline, TimerTarget::Completion(completion)))
        {
            debug_assert!(false, "TimerManager worker thread is gone: {e}");
            eprintln!(
                "silt: TimerManager worker thread unreachable ({e}); time.sleep will not fire"
            );
        }
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
/// if the platform cannot report parallelism. Mirrors the historical
/// hardcoded sizing inside `Vm::new` so the resolved value is identical
/// when the knob is absent.
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

pub(crate) struct IoPool {
    sender: parking_lot::Mutex<std::sync::mpsc::Sender<Box<dyn FnOnce() + Send>>>,
    /// Number of worker threads spawned for this pool. Retained so
    /// test-only introspection (`worker_count`) can confirm the
    /// `SILT_IO_POOL_SIZE` knob took effect; the production code path
    /// does not consult it. The field is unused outside `cfg(test)` /
    /// the `test-hooks` feature, so silence the dead-code warning on
    /// release builds rather than gate the field itself (which would
    /// fork the struct layout between configurations).
    #[allow(dead_code)]
    num_workers: usize,
}

impl IoPool {
    pub(super) fn new(num_threads: usize) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<Box<dyn FnOnce() + Send>>();
        let rx = Arc::new(parking_lot::Mutex::new(rx));
        for _ in 0..num_threads {
            let rx = rx.clone();
            std::thread::spawn(move || {
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
        }
        IoPool {
            sender: parking_lot::Mutex::new(tx),
            num_workers: num_threads,
        }
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
        let send_result = self.sender.lock().send(Box::new(move || {
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
        }));
        if let Err(e) = send_result {
            debug_assert!(false, "IoPool worker threads are gone: {e}");
            eprintln!("silt: IoPool workers unreachable ({e}); IO task will never complete");
        }
        completion
    }
}

// ── Runtime (shared state) ───────────────────────────────────────

/// Shared, read-only-after-init state for a Silt program.
/// Created once during initialization, then shared across spawned tasks via `Arc`.
pub struct Runtime {
    // ── Foreign function interface ──────────────────────────────
    pub(super) foreign_fns: HashMap<String, ForeignFn>,

    // ── M:N scheduler ──────────────────────────────────────────
    /// The shared scheduler for spawned tasks (None until first task.spawn).
    pub(super) scheduler: parking_lot::Mutex<Option<Arc<crate::scheduler::Scheduler>>>,

    // ── Timer manager ──────────────────────────────────────────
    /// Shared timer thread for `channel.timeout`.
    pub(crate) timer: TimerManager,

    // ── I/O pool ────────────────────────────────────────────────
    /// Thread pool for async I/O operations.
    pub(crate) io_pool: IoPool,
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
            // SAFETY: see scheduler.rs:1614 — Rust 1.80+ uses thread-local
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
            let pool = IoPool::new(n);
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
    fn default_io_pool_size_floor_two() {
        // Any platform we run on must produce at least 2 (the fallback
        // when available_parallelism returns Err is 2; otherwise it is
        // capped at 4). Locks the documented "floor 2" guarantee.
        let n = default_io_pool_size();
        assert!((2..=4).contains(&n), "default out of [2,4]: {n}");
    }
}
