use parking_lot::{Condvar, Mutex};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};

use crate::runtime::channel::Waker;
use crate::value::Value;
use crate::vm::VmError;

/// A combined Read + Write trait object used as the inner stream type for
/// `TcpStreamHandle`. Plain TCP impls it directly; the rustls wrappers in
/// `src/builtins/tcp.rs::tls` impl it manually so they can expose the
/// underlying `TcpStream`'s raw socket via `raw_socket`.
///
/// `raw_socket` exists so `tcp.close` on Windows can call `CancelIoEx`
/// on the SOCKET handle the parked `recv` is using. Winsock's
/// `shutdown(SD_BOTH)` does NOT cancel an in-progress blocking `recv`
/// on a duplicate handle (created via `WSADuplicateSocket` aka
/// `TcpStream::try_clone`), so we have to reach the actual SOCKET held
/// by the inner stream and explicitly cancel pending I/O on it.
///
/// On Unix, `raw_socket` is unused (Linux/macOS `shutdown(Both)` on the
/// cloned fd already delivers EOF to the parked reader). The default
/// returns `None` so non-socket implementors (e.g. test fakes) need not
/// override.
pub trait ReadWrite: std::io::Read + std::io::Write + Send {
    /// Underlying OS socket handle for the inner stream, if known.
    /// On Windows this is the `SOCKET` cast to `usize` (matching
    /// `std::os::windows::io::AsRawSocket::as_raw_socket() as usize`).
    /// On Unix this is the `RawFd` cast to `usize`. Used by `tcp.close`
    /// on Windows to call `CancelIoEx` on the parked reader's handle.
    fn raw_socket(&self) -> Option<usize> {
        None
    }
}

impl ReadWrite for std::net::TcpStream {
    fn raw_socket(&self) -> Option<usize> {
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawSocket;
            Some(self.as_raw_socket() as usize)
        }
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            Some(self.as_raw_fd() as usize)
        }
        #[cfg(not(any(windows, unix)))]
        {
            None
        }
    }
}

pub struct TcpListenerHandle {
    pub id: usize,
    pub listener: std::net::TcpListener,
}

pub struct TcpStreamHandle {
    pub id: usize,
    pub inner: Mutex<Box<dyn ReadWrite>>,
    pub closed: std::sync::atomic::AtomicBool,
    /// Side-channel handle to the underlying `TcpStream` fd, used by
    /// `tcp.close` to call `shutdown(Both)` without having to acquire
    /// `inner`'s mutex (which a concurrent `tcp.read` on another task may
    /// be holding). Obtained via `TcpStream::try_clone` at construction
    /// time; for TLS streams this is a clone of the socket that was
    /// subsequently handed to `rustls::StreamOwned`. On Unix, a
    /// `shutdown(Both)` on any clone affects the shared fd, causing any
    /// blocked reader to return EOF promptly. On Windows, `shutdown`
    /// alone does NOT reliably unblock a parked `recv`; after the
    /// shutdown we take the cloned handle out of this slot and drop it
    /// (which calls `closesocket` on the duplicate handle) to cancel
    /// pending I/O on the underlying socket. Wrapped in a `Mutex` so
    /// `close()` can `take()` the handle out from `&self` without having
    /// to contend for `inner`'s mutex. `None` if cloning the fd failed
    /// at construction (best-effort — callers fall back to Drop
    /// semantics) or after `close()` has consumed it on Windows.
    pub shutdown_sock: Mutex<Option<std::net::TcpStream>>,
    /// Raw OS socket handle for the **inner** stream (the one a parked
    /// `tcp.read` is using). Cached at construction time so `tcp.close`
    /// can issue `CancelIoEx` on Windows WITHOUT acquiring `inner`'s
    /// mutex — which is held by the parked reader and would deadlock.
    ///
    /// On Windows: `SOCKET as usize` (matches
    /// `AsRawSocket::as_raw_socket() as usize`).
    /// On Unix: `RawFd as usize`. Currently unused on Unix because
    /// `shutdown(Both)` on the cloned fd already wakes the reader.
    /// `None` only if the underlying stream type does not expose a
    /// raw socket (e.g. test fakes).
    pub reader_socket: Option<usize>,
}

/// Handle to a spawned task. Thread-safe — shared between spawner and worker.
pub struct TaskHandle {
    pub id: usize,
    result: Mutex<Option<Result<Value, VmError>>>,
    condvar: Condvar,
    /// Wakers to call when the task completes (for scheduler-based join).
    /// Each entry carries a monotonic id so a `JoinWakerRegistration`
    /// guard can deregister exactly its own entry on drop, avoiding the
    /// leak that occurred when a `task.join(h)`-blocked task was
    /// cancelled while the joinee was still running (the closure stayed
    /// in this Vec holding `Arc<Mutex<Option<Task>>>` + `Arc<SchedulerInner>`
    /// until the joinee finally completed).
    join_wakers: Mutex<Vec<(u64, Waker)>>,
    /// Monotonic counter for minting `join_wakers` entry ids.
    next_join_waker_id: AtomicU64,
    /// Cleanup to run when a blocked task is cancelled (removes stale waker state).
    ///
    /// Lock order: this mutex is a leaf. It is held only to move a
    /// closure in or out, never while a closure runs or is dropped, and
    /// no other lock is acquired while it is held. The scheduler
    /// acquires it while holding the lock of a parked task's slot; a
    /// cleanup closure locks that slot. If the closure ran under this
    /// mutex, the two orders would meet and `task.cancel` would hang.
    cancel_cleanup: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// True while the task has ended with an error of its own that no
    /// join has received, that no cancel has dismissed, and that has not
    /// been reported. Set by `fail`, cleared by `join`, `mark_joined`
    /// and `take_unjoined_failure`.
    unjoined_failure: AtomicBool,
    /// Who spawned the task, as the tag that was current where it was
    /// spawned (`crate::scheduler::set_task_owner`); 0 when nobody set
    /// one. A report of the task's failure carries it, so `silt test`
    /// can fail the test that spawned the task.
    owner: u64,
}

impl TaskHandle {
    pub fn new(id: usize) -> Self {
        Self::with_owner(id, 0)
    }

    /// A handle for a task spawned under the owner tag `owner`.
    pub fn with_owner(id: usize, owner: u64) -> Self {
        Self {
            id,
            result: Mutex::new(None),
            condvar: Condvar::new(),
            join_wakers: Mutex::new(Vec::new()),
            next_join_waker_id: AtomicU64::new(0),
            cancel_cleanup: Mutex::new(None),
            unjoined_failure: AtomicBool::new(false),
            owner,
        }
    }

    /// The owner tag the task was spawned under. See `with_owner`.
    pub fn owner(&self) -> u64 {
        self.owner
    }

    /// Register a cleanup closure to run when the task completes or is cancelled
    /// while blocked. This removes stale waker registrations from channels.
    ///
    /// A closure that was registered before is dropped, after the lock
    /// on the cleanup has been released.
    pub fn set_cancel_cleanup(&self, f: Box<dyn FnOnce() + Send>) {
        // The guard is a temporary of this statement, so the lock is
        // released before `previous` is dropped.
        let previous = self.cancel_cleanup.lock().replace(f);
        drop(previous);
    }

    /// Clear any pending cancel-cleanup closure so it won't fire when
    /// the task completes normally (prevents double-decrement of
    /// `live_tasks` and double-removal of the wake-graph node).
    ///
    /// The closure is dropped after the lock on the cleanup has been
    /// released.
    pub fn clear_cancel_cleanup(&self) {
        let previous = self.cancel_cleanup.lock().take();
        drop(previous);
    }

    /// Store the task result and notify any joiners.
    /// If the task has already completed, this is a no-op (prevents
    /// cancel from overwriting a finished task's result).
    pub fn complete(&self, result: Result<Value, VmError>) {
        self.finish(result, false);
    }

    /// Store the error that the task itself ended with, and notify any
    /// joiners. Like `complete`, and in addition the error counts as
    /// not joined until a join receives it.
    ///
    /// Returns `true` if this call stored the error, `false` if the
    /// handle already had a result (the task was cancelled before it
    /// failed); the error is dropped then.
    pub fn fail(&self, error: VmError) -> bool {
        self.finish(Err(error), true)
    }

    /// Shared body of `complete` and `fail`. Returns `true` if this
    /// call stored the result.
    fn finish(&self, result: Result<Value, VmError>, task_failed: bool) -> bool {
        {
            let mut guard = self.result.lock();
            if guard.is_some() {
                return false; // Already completed, don't overwrite
            }
            if task_failed {
                // Set before the result becomes visible: a join that
                // sees the result clears the flag after this.
                self.unjoined_failure.store(true, AtomicOrdering::Release);
            }
            *guard = Some(result);
        }
        // Fire cancel cleanup (removes stale waker state for blocked
        // tasks). The closure is taken out first and runs after the
        // lock on the cleanup has been released: see `cancel_cleanup`.
        let cleanup = self.cancel_cleanup.lock().take();
        if let Some(cleanup) = cleanup {
            cleanup();
        }
        self.condvar.notify_all();
        // Wake all tasks blocked on join.
        let wakers: Vec<(u64, Waker)> = {
            let mut guard = self.join_wakers.lock();
            std::mem::take(&mut *guard)
        };
        for (_, w) in wakers {
            w();
        }
        true
    }

    /// Block until the task produces a result.
    pub fn join(&self) -> Result<Value, VmError> {
        let mut guard = self.result.lock();
        loop {
            if let Some(result) = guard.clone() {
                self.mark_joined();
                return result;
            }
            self.condvar.wait(&mut guard);
        }
    }

    /// Whether the task has its result: it ended, or was cancelled.
    pub fn is_finished(&self) -> bool {
        self.result.lock().is_some()
    }

    /// Non-blocking poll.
    pub fn try_get(&self) -> Option<Result<Value, VmError>> {
        self.result.lock().clone()
    }

    /// Note that the program has handled the task: a join has received
    /// its result, or `task.cancel` was called on it. A failure of the
    /// task is the program's to handle from here on, and is not reported
    /// as unjoined.
    pub fn mark_joined(&self) {
        self.unjoined_failure.store(false, AtomicOrdering::Release);
    }

    /// True iff the task failed and no join has received the error.
    pub fn has_unjoined_failure(&self) -> bool {
        self.unjoined_failure.load(AtomicOrdering::Acquire)
    }

    /// The error of a task that failed and that no join has received.
    /// Returns it once: after this call the failure counts as reported.
    pub fn take_unjoined_failure(&self) -> Option<VmError> {
        if !self.unjoined_failure.swap(false, AtomicOrdering::AcqRel) {
            return None;
        }
        match self.result.lock().as_ref() {
            Some(Err(error)) => Some(error.clone()),
            _ => None,
        }
    }

    /// Mint a fresh id for a new join-waker registration.
    fn mint_join_waker_id(&self) -> u64 {
        self.next_join_waker_id
            .fetch_add(1, AtomicOrdering::Relaxed)
    }

    /// Register a waker to be called when the task completes.
    ///
    /// Legacy entry point: callers that need RAII deregistration on
    /// cancel should prefer [`register_join_waker_guard`](Self::register_join_waker_guard).
    /// This non-guard variant remains for stable call sites that join
    /// with no cancellation pressure (e.g. `task.join(h)` from main in
    /// `concurrency::main_thread_wait_for_join`).
    pub fn register_join_waker(&self, waker: Waker) {
        // Allocate an id even on the non-guard path so the storage
        // shape stays uniform — drop(_id) is a no-op once the closure
        // has been fired or drained.
        let id = self.mint_join_waker_id();
        // Check if already complete to avoid missed wakeups.
        let already_done = self.result.lock().is_some();
        if already_done {
            waker();
        } else {
            self.join_wakers.lock().push((id, waker));
            // Double-check to avoid race: if result was set between our check and push.
            if self.result.lock().is_some() {
                // It completed in the meantime; drain and fire.
                let wakers: Vec<(u64, Waker)> = {
                    let mut guard = self.join_wakers.lock();
                    std::mem::take(&mut *guard)
                };
                for (_, w) in wakers {
                    w();
                }
            }
        }
    }

    /// Register a join waker and return a `JoinWakerRegistration` RAII
    /// guard that deregisters the entry on drop. Required for cancel-
    /// path correctness: a `task.join(h)`-blocked task that is cancelled
    /// before the joinee completes must NOT leave its waker closure in
    /// `join_wakers`, because the closure holds `Arc<Mutex<Option<Task>>>`
    /// (with the Task already taken, so it would be inert) plus
    /// `Arc<SchedulerInner>`. Without the guard, the entry persists
    /// until the joinee finally completes — N cancelled joiners means N
    /// leaked closures on a long-running joinee.
    ///
    /// If the joinee is already complete, this fires the waker inline
    /// and returns a guard whose `id` does not appear in the Vec; the
    /// guard's Drop is a no-op deregister in that case.
    pub fn register_join_waker_guard(self: &Arc<Self>, waker: Waker) -> JoinWakerRegistration {
        let id = self.mint_join_waker_id();
        let already_done = self.result.lock().is_some();
        if already_done {
            waker();
        } else {
            self.join_wakers.lock().push((id, waker));
            // Double-check to avoid race: if result was set between our check and push.
            if self.result.lock().is_some() {
                let wakers: Vec<(u64, Waker)> = {
                    let mut guard = self.join_wakers.lock();
                    std::mem::take(&mut *guard)
                };
                for (_, w) in wakers {
                    w();
                }
            }
        }
        JoinWakerRegistration {
            handle: self.clone(),
            id,
        }
    }

    /// Remove a previously-registered join waker by id. Returns `true`
    /// if the entry was found and removed, `false` if it had already
    /// been drained (e.g. by `complete()` firing all pending wakers).
    pub fn remove_join_waker(&self, id: u64) -> bool {
        let mut guard = self.join_wakers.lock();
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

    /// Test/introspection accessor: number of join-waker entries
    /// currently registered. Used by regression tests that verify
    /// cancelled `task.join(h)` blocks do not leak waker closures.
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn join_waker_count(&self) -> usize {
        self.join_wakers.lock().len()
    }
}

/// RAII guard that owns a registered join-waker entry on a
/// `TaskHandle` and deregisters it on drop. Construct via
/// [`TaskHandle::register_join_waker_guard`].
///
/// Ensures the cancel path for `task.join(h)`-blocked tasks does not
/// leak waker closures into `TaskHandle::join_wakers`. The guard's
/// Drop calls `remove_join_waker`, which is idempotent: if the waker
/// already fired (drained by `complete()`), Drop returns `false`
/// without further action.
pub struct JoinWakerRegistration {
    handle: Arc<TaskHandle>,
    id: u64,
}

impl JoinWakerRegistration {
    /// Expose the underlying entry id. Primarily for tests; production
    /// code should not need this because the guard owns deregistration.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Expose the handle this registration is on. Useful for tests
    /// that want to query `join_waker_count` without re-plumbing the
    /// handle separately.
    pub fn handle(&self) -> &Arc<TaskHandle> {
        &self.handle
    }
}

impl Drop for JoinWakerRegistration {
    fn drop(&mut self) {
        self.handle.remove_join_waker(self.id);
    }
}
