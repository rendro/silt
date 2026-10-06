use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

use crate::runtime::sync::{Cell, Completion, Wake};
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
    /// The task's result, set once: what it returned, the error it
    /// failed with, or that it was cancelled. A join waits for it.
    result: Arc<Cell<Result<Value, VmError>>>,
    /// Set by `task.cancel`. The scheduler reads it before each slice
    /// of the task and before each park, so a cancelled task runs no
    /// further and takes nothing.
    cancelled: Arc<AtomicBool>,
    /// True while the task has failed and nobody has handled the
    /// failure: no join has received it and no cancel has dismissed
    /// it. Read when the program ends, for the report of failures that
    /// nobody joined.
    unjoined_failure: AtomicBool,
    /// Who the task belongs to (`scheduler::set_task_owner`): the
    /// program, or one test of a test run.
    owner: u64,
}

impl TaskHandle {
    pub fn new(id: usize) -> Self {
        Self::with_owner(id, 0)
    }

    /// A handle for a task that belongs to `owner`.
    pub fn with_owner(id: usize, owner: u64) -> Self {
        Self {
            id,
            result: Cell::new(),
            cancelled: Arc::new(AtomicBool::new(false)),
            unjoined_failure: AtomicBool::new(false),
            owner,
        }
    }

    /// The owner tag of the task.
    pub fn owner(&self) -> u64 {
        self.owner
    }

    /// The task's end, as an arm of a wait: a join.
    pub fn done(&self) -> Arc<dyn Completion> {
        self.result.clone()
    }

    /// The task ended with `result`. `false` when the handle has a
    /// result already (the task was cancelled): the first stands.
    pub fn complete(&self, result: Result<Value, VmError>, wake: &dyn Wake) -> bool {
        self.result.complete(result, wake).is_ok()
    }

    /// The task failed with `error`. `true` when that is the handle's
    /// result now: the failure is then unhandled until a join receives
    /// it or a cancel dismisses it.
    pub fn fail(&self, error: VmError, wake: &dyn Wake) -> bool {
        if self.result.get().is_some() {
            return false;
        }
        // Before the joiners are woken: one of them handles it.
        self.unjoined_failure.store(true, AtomicOrdering::Release);
        self.result.complete(Err(error), wake).is_ok()
    }

    /// `task.cancel`: the task runs no further, and its result is the
    /// cancellation unless it had one. A failure it had is dismissed.
    /// The caller ends the task's wait, if it waits
    /// (`Scheduler::cancel`).
    pub fn cancel(&self, wake: &dyn Wake) {
        self.cancelled.store(true, AtomicOrdering::SeqCst);
        let _ = self
            .result
            .complete(Err(VmError::new("cancelled".to_string())), wake);
        self.mark_joined();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(AtomicOrdering::SeqCst)
    }

    /// The flag that says so, for the waits of the task
    /// (`runtime::sync::Wait::cancel`).
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.cancelled.clone()
    }

    /// The result, once the task has one.
    pub fn try_get(&self) -> Option<Result<Value, VmError>> {
        self.result.get().cloned()
    }

    /// The failure of the task is handled: a join received it, or a
    /// cancel dismissed it.
    pub fn mark_joined(&self) {
        self.unjoined_failure.store(false, AtomicOrdering::Release);
    }

    pub fn has_unjoined_failure(&self) -> bool {
        self.unjoined_failure.load(AtomicOrdering::Acquire)
    }

    /// The failure of the task, if nobody has handled it; it counts as
    /// handled from now on.
    pub fn take_unjoined_failure(&self) -> Option<VmError> {
        if !self.unjoined_failure.swap(false, AtomicOrdering::AcqRel) {
            return None;
        }
        match self.result.get() {
            Some(Err(error)) => Some(error.clone()),
            _ => None,
        }
    }
}
