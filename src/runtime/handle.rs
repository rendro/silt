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

/// A listening socket.
///
/// An accept blocks in the OS until a connection comes: it costs
/// nothing while it waits. It can still be given up
/// ([`TcpListenerHandle::stop`]): the listener then connects to
/// itself, the accept returns with that connection, knows it by its
/// address, drops it and ends. One accept at a time is in the OS call
/// (the others wait their turn here), so the connection that wakes
/// reaches the accept it is meant for.
pub struct TcpListenerHandle {
    pub id: usize,
    listener: std::net::TcpListener,
    accepting: Mutex<Accepting>,
    /// The accepts that wait their turn wait here.
    turn: parking_lot::Condvar,
}

#[derive(Default)]
struct Accepting {
    /// The stop flag of the accept that is in the OS call.
    current: Option<Arc<AtomicBool>>,
    /// The addresses that the connections made to wake an accept come
    /// from: such a connection is not a client's.
    wakes: Vec<std::net::SocketAddr>,
}

impl TcpListenerHandle {
    pub fn new(id: usize, listener: std::net::TcpListener) -> Self {
        TcpListenerHandle {
            id,
            listener,
            accepting: Mutex::default(),
            turn: parking_lot::Condvar::new(),
        }
    }

    /// The address the listener is bound to.
    pub fn local_addr(&self) -> std::io::Result<std::net::SocketAddr> {
        self.listener.local_addr()
    }

    /// Another handle to the listening socket, for a server that
    /// accepts on it itself (`http.serve`).
    pub fn to_std(&self) -> std::io::Result<std::net::TcpListener> {
        self.listener.try_clone()
    }

    /// The next connection; `None` if the accept was given up
    /// ([`TcpListenerHandle::stop`] with the same flag).
    pub fn accept(
        &self,
        stopped: &Arc<AtomicBool>,
    ) -> std::io::Result<Option<std::net::TcpStream>> {
        {
            let mut accepting = self.accepting.lock();
            loop {
                if stopped.load(AtomicOrdering::SeqCst) {
                    return Ok(None);
                }
                if accepting.current.is_none() {
                    break;
                }
                self.turn.wait(&mut accepting);
            }
            accepting.current = Some(stopped.clone());
        }
        let result = loop {
            match self.listener.accept() {
                Ok((stream, peer)) => {
                    // Whoever wakes an accept holds this lock until the
                    // address of its connection is noted.
                    let mut accepting = self.accepting.lock();
                    let wake = accepting.wakes.iter().position(|addr| *addr == peer);
                    let Some(wake) = wake else {
                        break Ok(Some(stream));
                    };
                    accepting.wakes.swap_remove(wake);
                    if stopped.load(AtomicOrdering::SeqCst) {
                        break Ok(None);
                    }
                    // Meant for an accept that had its connection
                    // already: this one goes on.
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => break Err(e),
            }
        };
        self.accepting.lock().current = None;
        self.turn.notify_all();
        result
    }

    /// Give up the accept that was called with `stopped`: it returns
    /// `None`, at once if it waits its turn, and as soon as the
    /// connection made here reaches it if it is in the OS call.
    ///
    /// That connection goes to the listener's own port on the loopback
    /// address. If it cannot be made (the listener's backlog is full,
    /// a packet filter forbids loopback traffic to the port, the
    /// process has no descriptor left), the accept stays in the OS
    /// call until a client connects: its thread lingers.
    pub fn stop(&self, stopped: &Arc<AtomicBool>) {
        stopped.store(true, AtomicOrdering::SeqCst);
        let mut accepting = self.accepting.lock();
        let in_the_call = accepting
            .current
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, stopped));
        if !in_the_call {
            drop(accepting);
            self.turn.notify_all();
            return;
        }
        let woken = self.wake_addr().and_then(|addr| {
            let conn = std::net::TcpStream::connect_timeout(&addr, WAKE_CONNECT_LIMIT)?;
            conn.local_addr()
        });
        if let Ok(from) = woken {
            accepting.wakes.push(from);
        }
    }

    /// Where a connection to this listener from this machine goes.
    fn wake_addr(&self) -> std::io::Result<std::net::SocketAddr> {
        let mut addr = self.listener.local_addr()?;
        if addr.ip().is_unspecified() {
            addr.set_ip(match addr {
                std::net::SocketAddr::V4(_) => std::net::Ipv4Addr::LOCALHOST.into(),
                std::net::SocketAddr::V6(_) => std::net::Ipv6Addr::LOCALHOST.into(),
            });
        }
        Ok(addr)
    }
}

/// How long the connection that wakes an accept may take to be made.
/// On the loopback interface it is made at once.
const WAKE_CONNECT_LIMIT: std::time::Duration = std::time::Duration::from_secs(1);

/// A connection.
///
/// A plain TCP connection has two halves, each a handle of its own to
/// the one socket (`try_clone`): a task that reads and a task that
/// writes do not wait for each other. A TLS connection is one object
/// behind one lock: its reads and writes take turns.
pub struct TcpStreamHandle {
    pub id: usize,
    io: TcpIo,
    closed: AtomicBool,
    /// A third handle to the socket, to shut it down
    /// ([`TcpStreamHandle::shut_down`]) without the lock of a half,
    /// which a blocked read or write holds. On Unix a shutdown through
    /// any handle ends a blocked `recv` on the others. On Windows it
    /// does not: there the handle is also taken out of this slot and
    /// dropped. `None` if the socket could not be cloned, or after a
    /// shutdown on Windows.
    shutdown_sock: Mutex<Option<std::net::TcpStream>>,
    /// The OS socket that a blocked read uses (`SOCKET as usize` on
    /// Windows, where `CancelIoEx` on it ends that read; the file
    /// descriptor on Unix, unused).
    reader_socket: Option<usize>,
}

enum TcpIo {
    Halves {
        read: Mutex<std::net::TcpStream>,
        write: Mutex<std::net::TcpStream>,
    },
    Whole(Mutex<Box<dyn ReadWrite>>),
}

impl TcpStreamHandle {
    /// A plain TCP connection.
    pub fn plain(id: usize, stream: std::net::TcpStream) -> std::io::Result<Arc<Self>> {
        let write = stream.try_clone()?;
        Ok(Arc::new(TcpStreamHandle {
            id,
            shutdown_sock: Mutex::new(stream.try_clone().ok()),
            reader_socket: stream.raw_socket(),
            io: TcpIo::Halves {
                read: Mutex::new(stream),
                write: Mutex::new(write),
            },
            closed: AtomicBool::new(false),
        }))
    }

    /// A connection that is one object (TLS) over `socket`. Called
    /// with the socket before it is handed to that object.
    pub fn whole(
        id: usize,
        socket: &std::net::TcpStream,
        wrap: impl FnOnce() -> Result<Box<dyn ReadWrite>, String>,
    ) -> Result<Arc<Self>, String> {
        let shutdown_sock = Mutex::new(socket.try_clone().ok());
        let reader_socket = socket.raw_socket();
        Ok(Arc::new(TcpStreamHandle {
            id,
            io: TcpIo::Whole(Mutex::new(wrap()?)),
            closed: AtomicBool::new(false),
            shutdown_sock,
            reader_socket,
        }))
    }

    /// Whether the connection was shut down from this side.
    pub fn is_closed(&self) -> bool {
        self.closed.load(AtomicOrdering::SeqCst)
    }

    pub fn read(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        use std::io::Read;
        match &self.io {
            TcpIo::Halves { read, .. } => read.lock().read(buf),
            TcpIo::Whole(both) => both.lock().read(buf),
        }
    }

    pub fn read_exact(&self, buf: &mut [u8]) -> std::io::Result<()> {
        use std::io::Read;
        match &self.io {
            TcpIo::Halves { read, .. } => read.lock().read_exact(buf),
            TcpIo::Whole(both) => both.lock().read_exact(buf),
        }
    }

    /// Write all of `buf`, and flush.
    pub fn write_all(&self, buf: &[u8]) -> std::io::Result<()> {
        use std::io::Write;
        match &self.io {
            TcpIo::Halves { write, .. } => {
                let mut write = write.lock();
                write.write_all(buf)?;
                write.flush()
            }
            TcpIo::Whole(both) => {
                let mut both = both.lock();
                both.write_all(buf)?;
                both.flush()
            }
        }
    }

    /// Shut the connection down, once: a read or a write that blocks
    /// on it returns, and later ones are refused. This is `tcp.close`,
    /// and what ends an operation that nobody waits for any more (its
    /// task was cancelled, timed out, or dropped at the end of the
    /// program): the thread that ran it is free again.
    ///
    /// A TLS connection gets no `close_notify`: a rough shutdown is
    /// preferred to a thread that never returns.
    pub fn shut_down(&self) {
        if self.closed.swap(true, AtomicOrdering::SeqCst) {
            return;
        }
        // What a TLS writer still buffers, if nobody is in it: a
        // blocked read or write holds the lock, and waiting for it
        // here would wait for the very thing this is to end.
        if let TcpIo::Whole(both) = &self.io
            && let Some(mut both) = both.try_lock()
        {
            let _ = std::io::Write::flush(&mut *both);
        }
        // Errors (not connected, the peer closed first) are ignored:
        // the connection is closed either way.
        #[allow(unused_mut)]
        let mut slot = self.shutdown_sock.lock();
        if let Some(sock) = slot.as_ref() {
            let _ = sock.shutdown(std::net::Shutdown::Both);
        }
        // Winsock's `shutdown(SD_BOTH)` does not end a `recv` that
        // blocks on another handle of the socket (the reader's, of
        // which `shutdown_sock` is a duplicate). `CancelIoEx` on the
        // reader's own SOCKET does; the duplicate is then closed.
        #[cfg(windows)]
        {
            if let Some(sock) = self.reader_socket {
                // SAFETY: `CancelIoEx` may be called on any HANDLE, a
                // SOCKET included; it fails harmlessly if the handle
                // is invalid or nothing is pending. A null
                // `lpOverlapped` cancels all pending I/O on it.
                use std::ptr;
                use windows_sys::Win32::Foundation::HANDLE;
                use windows_sys::Win32::System::IO::CancelIoEx;
                unsafe {
                    let _ = CancelIoEx(sock as HANDLE, ptr::null_mut());
                }
            }
            let _ = slot.take();
        }
        #[cfg(not(windows))]
        let _ = self.reader_socket;
        drop(slot);
    }
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
    /// Who the task belongs to (`Vm::set_task_owner`): the
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
            result: Cell::labelled(format!("task <handle:{id}>")),
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
