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

pub(crate) struct IoPool {
    sender: parking_lot::Mutex<std::sync::mpsc::Sender<Box<dyn FnOnce() + Send>>>,
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
        }
    }

    /// Submit a blocking I/O operation. Returns a completion handle.
    pub(crate) fn submit(&self, f: impl FnOnce() -> Value + Send + 'static) -> Arc<IoCompletion> {
        let completion = IoCompletion::new();
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
                    Value::Variant("Err".into(), vec![Value::String(msg)])
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
    /// Maps variant tag names to their parent type name, for method dispatch.
    #[allow(dead_code)]
    pub(super) variant_types: HashMap<String, String>,

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
