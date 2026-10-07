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
    /// Clients' connections that reached an accept which had been
    /// given up: the next accepts take them, oldest first, before
    /// they ask the OS.
    kept: std::collections::VecDeque<std::net::TcpStream>,
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
    /// ([`TcpListenerHandle::stop`] with the same flag). A client's
    /// connection is never lost to an accept that was given up: it is
    /// kept for the next one.
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
                if let Some(stream) = accepting.kept.pop_front() {
                    return Ok(Some(stream));
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
                        if stopped.load(AtomicOrdering::SeqCst) {
                            // A client's, and nobody waits for this
                            // accept: it is the next accept's.
                            accepting.kept.push_back(stream);
                            break Ok(None);
                        }
                        break Ok(Some(stream));
                    };
                    accepting.wakes.swap_remove(wake);
                    if stopped.load(AtomicOrdering::SeqCst) {
                        break Ok(None);
                    }
                    if let Some(kept) = accepting.kept.pop_front() {
                        break Ok(Some(kept));
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

    /// Keep a client's connection for the next accept: one that an
    /// accept had taken for a task which never received it.
    pub fn keep(&self, stream: std::net::TcpStream) {
        self.accepting.lock().kept.push_back(stream);
        self.turn.notify_all();
        // An accept in the OS call does not see it: it is woken as
        // one that is given up is, by a connection it drops, and
        // looks at what is kept before it calls the OS again.
        let mut accepting = self.accepting.lock();
        if accepting.current.is_some() {
            let woken = self.wake_addr().and_then(|addr| {
                let conn = std::net::TcpStream::connect_timeout(&addr, WAKE_CONNECT_LIMIT)?;
                conn.local_addr()
            });
            if let Ok(from) = woken {
                accepting.wakes.push(from);
            }
        }
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
/// A plain TCP connection is read and written at the same time: a
/// task that reads and a task that writes do not wait for each other
/// (the socket is used through shared references; each direction has
/// a lock of its own, so two readers, or two writers, take turns). A
/// TLS connection is one object behind one lock: its reads and writes
/// take turns.
pub struct TcpStreamHandle {
    pub id: usize,
    io: TcpIo,
    closed: AtomicBool,
    /// The OS socket that a blocked read or write uses (`SOCKET as
    /// usize` on Windows, where `CancelIoEx` on it ends what blocks on
    /// it; the file descriptor on Unix, unused).
    io_socket: Option<usize>,
}

enum TcpIo {
    Plain {
        socket: std::net::TcpStream,
        /// Held by the task whose read is in flight; likewise
        /// `writing`.
        reading: Mutex<()>,
        writing: Mutex<()>,
    },
    Tls {
        both: Mutex<Box<dyn ReadWrite>>,
        /// Another handle to the socket, to shut it down and to set
        /// its options without the lock of `both`, which a blocked
        /// read or write holds. `None` if the socket could not be
        /// cloned, or after a shutdown on Windows (where the handle
        /// is closed to end what blocks on the other).
        socket: Mutex<Option<std::net::TcpStream>>,
    },
}

impl TcpStreamHandle {
    /// A plain TCP connection.
    pub fn plain(id: usize, socket: std::net::TcpStream) -> Arc<Self> {
        Arc::new(TcpStreamHandle {
            id,
            io_socket: socket.raw_socket(),
            io: TcpIo::Plain {
                socket,
                reading: Mutex::new(()),
                writing: Mutex::new(()),
            },
            closed: AtomicBool::new(false),
        })
    }

    /// A connection that is one object (TLS) over `socket`. Called
    /// with the socket before it is handed to that object.
    pub fn whole(
        id: usize,
        socket: &std::net::TcpStream,
        wrap: impl FnOnce() -> Result<Box<dyn ReadWrite>, String>,
    ) -> Result<Arc<Self>, String> {
        let other = Mutex::new(socket.try_clone().ok());
        let io_socket = socket.raw_socket();
        Ok(Arc::new(TcpStreamHandle {
            id,
            io: TcpIo::Tls {
                both: Mutex::new(wrap()?),
                socket: other,
            },
            closed: AtomicBool::new(false),
            io_socket,
        }))
    }

    /// Another handle to the socket of a plain connection that no
    /// task has: `None` for TLS, and if the socket cannot be cloned.
    pub fn socket(&self) -> Option<std::net::TcpStream> {
        match &self.io {
            TcpIo::Plain { socket, .. } => socket.try_clone().ok(),
            TcpIo::Tls { .. } => None,
        }
    }

    /// Whether the connection was shut down from this side.
    pub fn is_closed(&self) -> bool {
        self.closed.load(AtomicOrdering::SeqCst)
    }

    /// Do `f` with the socket, whatever blocks on the connection.
    fn with_socket<T>(
        &self,
        f: impl FnOnce(&std::net::TcpStream) -> std::io::Result<T>,
    ) -> std::io::Result<T> {
        match &self.io {
            TcpIo::Plain { socket, .. } => f(socket),
            TcpIo::Tls { socket, .. } => match socket.lock().as_ref() {
                Some(socket) => f(socket),
                None => Err(std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "the connection is closed",
                )),
            },
        }
    }

    /// The address of the other end.
    pub fn peer_addr(&self) -> std::io::Result<std::net::SocketAddr> {
        self.with_socket(|socket| socket.peer_addr())
    }

    /// Send small writes at once (`on`), or let the system gather
    /// them (Nagle's algorithm, the default).
    pub fn set_nodelay(&self, on: bool) -> std::io::Result<()> {
        self.with_socket(|socket| socket.set_nodelay(on))
    }

    pub fn read(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        use std::io::Read;
        match &self.io {
            TcpIo::Plain {
                socket, reading, ..
            } => {
                let _turn = reading.lock();
                (&*socket).read(buf)
            }
            TcpIo::Tls { both, .. } => both.lock().read(buf),
        }
    }

    pub fn read_exact(&self, buf: &mut [u8]) -> std::io::Result<()> {
        use std::io::Read;
        match &self.io {
            TcpIo::Plain {
                socket, reading, ..
            } => {
                let _turn = reading.lock();
                (&*socket).read_exact(buf)
            }
            TcpIo::Tls { both, .. } => both.lock().read_exact(buf),
        }
    }

    /// Write all of `buf`, and flush.
    pub fn write_all(&self, buf: &[u8]) -> std::io::Result<()> {
        use std::io::Write;
        match &self.io {
            TcpIo::Plain {
                socket, writing, ..
            } => {
                let _turn = writing.lock();
                (&*socket).write_all(buf)?;
                (&*socket).flush()
            }
            TcpIo::Tls { both, .. } => {
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
        // Errors (not connected, the peer closed first) are ignored:
        // the connection is closed either way.
        match &self.io {
            TcpIo::Plain { socket, .. } => {
                let _ = socket.shutdown(std::net::Shutdown::Both);
            }
            TcpIo::Tls { both, socket } => {
                // What the TLS writer still buffers, if nobody is in
                // it: a blocked read or write holds the lock, and
                // waiting for it here would wait for the very thing
                // this is to end.
                if let Some(mut both) = both.try_lock() {
                    let _ = std::io::Write::flush(&mut *both);
                }
                #[allow(unused_mut)]
                let mut other = socket.lock();
                if let Some(other) = other.as_ref() {
                    let _ = other.shutdown(std::net::Shutdown::Both);
                }
                // On Windows the duplicate is closed too (see below).
                #[cfg(windows)]
                let _ = other.take();
            }
        }
        // Winsock's `shutdown(SD_BOTH)` does not end a `recv` or a
        // `send` that blocks on the socket. `CancelIoEx` on the SOCKET
        // that the blocked call uses does.
        #[cfg(windows)]
        if let Some(sock) = self.io_socket {
            // SAFETY: `CancelIoEx` may be called on any HANDLE, a
            // SOCKET included; it fails harmlessly if the handle is
            // invalid or nothing is pending. A null `lpOverlapped`
            // cancels all pending I/O on it.
            use std::ptr;
            use windows_sys::Win32::Foundation::HANDLE;
            use windows_sys::Win32::System::IO::CancelIoEx;
            unsafe {
                let _ = CancelIoEx(sock as HANDLE, ptr::null_mut());
            }
        }
        #[cfg(not(windows))]
        let _ = self.io_socket;
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
