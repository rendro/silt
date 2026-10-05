use parking_lot::{Condvar, Mutex};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

use crate::runtime::channel::Waker;
use crate::typeinfo::bv;
use crate::value::Value;

/// Per-completion factory that builds the typed `Err` variant the
/// scheduler watchdog (or an already-elapsed entry guard) should
/// surface when the task's deadline cancels this I/O op. Each builtin
/// provides its own factory so a timed-out `tcp.read` produces
/// `Err(TcpTimeout)`, a timed-out `io.read_file` produces
/// `Err(IoUnknown(msg))`, etc.
///
/// Stored on `IoCompletion` so the scheduler thread can construct the
/// shape without knowing which module's submit it was built for.
pub type TimeoutErrFactory = std::sync::Arc<dyn Fn(&str) -> Value + Send + Sync>;

/// Default factory: wraps `msg` in `Err(IoUnknown(msg))`. Used by
/// `IoCompletion::new()` and by builtins on the io/fs family, which
/// still use `IoError` as their error type. Any builtin whose
/// signature declares a different error enum MUST construct its own
/// factory and pass it via `with_timeout_err` + `submit_with`.
pub fn io_unknown_timeout_err(msg: &str) -> Value {
    Value::variant(
        bv::ERR,
        vec![Value::variant(
            bv::IO_UNKNOWN,
            vec![Value::String(msg.to_string())],
        )],
    )
}

/// Completion handle for async I/O operations.
pub struct IoCompletion {
    result: Mutex<Option<Value>>,
    condvar: Condvar,
    /// Wakers to call when the I/O op completes. Each entry carries a
    /// monotonic id so an `IoWakerRegistration` guard can deregister
    /// exactly its own entry on drop, avoiding the leak that occurred
    /// when an I/O-blocked task was cancelled (or its deadline elapsed)
    /// before the completion fired — the closure stayed in this Vec
    /// holding `Arc<Mutex<Option<Task>>>` + `Arc<SchedulerInner>` until
    /// the underlying I/O finally produced a value.
    wakers: Mutex<Vec<(u64, Waker)>>,
    /// Monotonic counter for minting `wakers` entry ids.
    next_waker_id: AtomicU64,
    timeout_err: TimeoutErrFactory,
}

impl IoCompletion {
    /// Default constructor: deadline-cancellation surfaces as
    /// `Err(IoUnknown(msg))`. This is the standard entry point and is
    /// used by the io/fs family of builtins (and by any builtin that
    /// has not declared a typed error enum). Builtins whose signature
    /// declares a different error enum should call
    /// [`with_timeout_err`](Self::with_timeout_err) and pass a
    /// module-specific factory so a deadline-cancelled `tcp.read`
    /// produces `Err(TcpTimeout)` rather than `Err(IoUnknown(_))`.
    pub fn new() -> Arc<Self> {
        Self::with_timeout_err(std::sync::Arc::new(io_unknown_timeout_err))
    }

    /// Build a completion with a caller-supplied timeout-error factory.
    pub fn with_timeout_err(timeout_err: TimeoutErrFactory) -> Arc<Self> {
        Arc::new(Self {
            result: Mutex::new(None),
            condvar: Condvar::new(),
            wakers: Mutex::new(Vec::new()),
            next_waker_id: AtomicU64::new(0),
            timeout_err,
        })
    }

    /// Construct the typed `Err` variant this completion should surface
    /// when its task's deadline elapses. Called by the scheduler
    /// watchdog and by `deadline_exceeded_err_value` at entry-guard.
    pub fn build_timeout_err(&self, msg: &str) -> Value {
        (self.timeout_err)(msg)
    }

    /// Store the I/O result and notify all waiters. First-writer-wins:
    /// once a result is stored, subsequent calls are no-ops. Returns
    /// `true` if this call stored the result, `false` if a previous
    /// caller already did. This lets the scheduler watchdog set a
    /// timeout error without racing against a late-arriving real result.
    pub fn complete(&self, value: Value) -> bool {
        {
            let mut guard = self.result.lock();
            if guard.is_some() {
                return false;
            }
            *guard = Some(value);
        }
        self.condvar.notify_all();
        let wakers: Vec<(u64, Waker)> = {
            let mut guard = self.wakers.lock();
            std::mem::take(&mut *guard)
        };
        for (_, w) in wakers {
            w();
        }
        true
    }

    /// Non-blocking poll.
    pub fn try_get(&self) -> Option<Value> {
        self.result.lock().clone()
    }

    /// Blocking wait (for main thread). Clones the result rather than
    /// taking it, so the first-writer-wins invariant on `complete` is
    /// preserved: a subsequent `try_get` still observes the same value.
    pub fn wait(&self) -> Value {
        let mut guard = self.result.lock();
        loop {
            if let Some(result) = guard.clone() {
                return result;
            }
            self.condvar.wait(&mut guard);
        }
    }

    /// Mint a fresh id for a new waker registration.
    fn mint_waker_id(&self) -> u64 {
        self.next_waker_id.fetch_add(1, AtomicOrdering::Relaxed)
    }

    // Round 80 dead-code removal (DEAD-FN): the non-guard
    // `register_waker` had zero production callers — every I/O entry
    // guard now uses `register_waker_guard` for cancel-path
    // correctness. The only remaining caller was the
    // `legacy_register_io_waker_still_works` test asserting the
    // function existed; both have been removed. The waker-id minting
    // and double-check pattern survive verbatim inside
    // `register_waker_guard` below.

    /// Register a waker and return an `IoWakerRegistration` RAII guard
    /// that deregisters the entry on drop. Required for cancel-path
    /// correctness: an I/O-blocked task that is cancelled (or whose
    /// deadline elapses) before the I/O completes must NOT leave its
    /// waker closure in `wakers`, because the closure holds
    /// `Arc<Mutex<Option<Task>>>` plus `Arc<SchedulerInner>`. Without
    /// the guard, the entry persists until the I/O finally produces a
    /// value — N cancelled waiters on a slow I/O op means N leaked
    /// closures on the completion handle.
    pub fn register_waker_guard(self: &Arc<Self>, waker: Waker) -> IoWakerRegistration {
        let id = self.mint_waker_id();
        let already_done = self.result.lock().is_some();
        if already_done {
            waker();
        } else {
            self.wakers.lock().push((id, waker));
            // Double-check: result may have arrived between check and push
            if self.result.lock().is_some() {
                let wakers: Vec<(u64, Waker)> = {
                    let mut guard = self.wakers.lock();
                    std::mem::take(&mut *guard)
                };
                for (_, w) in wakers {
                    w();
                }
            }
        }
        IoWakerRegistration {
            completion: self.clone(),
            id,
        }
    }

    /// Remove a previously-registered waker by id. Returns `true` if
    /// the entry was found and removed, `false` if it had already been
    /// drained (e.g. by `complete()` firing all pending wakers).
    pub fn remove_waker(&self, id: u64) -> bool {
        let mut guard = self.wakers.lock();
        if let Some(pos) = guard.iter().position(|(wid, _)| *wid == id) {
            // Drop the (id, Waker) tuple — we are intentionally
            // discarding the closure without firing it; cancellation
            // of a parked task means its waker should never run.
            let _ = guard.remove(pos);
            true
        } else {
            false
        }
    }

    /// Test/introspection accessor: number of waker entries currently
    /// registered. Used by regression tests that verify cancelled
    /// I/O-blocked tasks do not leak waker closures.
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn waker_count(&self) -> usize {
        self.wakers.lock().len()
    }
}

/// RAII guard that owns a registered waker entry on an `IoCompletion`
/// and deregisters it on drop. Construct via
/// [`IoCompletion::register_waker_guard`].
///
/// Ensures the cancel path for I/O-blocked tasks does not leak waker
/// closures into `IoCompletion::wakers`. The guard's Drop calls
/// `remove_waker`, which is idempotent: if the waker already fired
/// (drained by `complete()`), Drop returns `false` without further
/// action.
pub struct IoWakerRegistration {
    completion: Arc<IoCompletion>,
    id: u64,
}

impl IoWakerRegistration {
    /// Expose the underlying entry id. Primarily for tests.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Expose the completion this registration is on. Useful for
    /// tests that want to query `waker_count` without re-plumbing the
    /// completion separately.
    pub fn completion(&self) -> &Arc<IoCompletion> {
        &self.completion
    }
}

impl Drop for IoWakerRegistration {
    fn drop(&mut self) {
        self.completion.remove_waker(self.id);
    }
}
