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
///
/// `http.serve` accepts in the same way, and has the listener to
/// itself while it serves ([`TcpListenerHandle::serve`]).
pub struct TcpListenerHandle {
    pub id: usize,
    listener: std::net::TcpListener,
    accepting: Mutex<Accepting>,
    /// The accepts that wait their turn wait here.
    turn: parking_lot::Condvar,
    /// The `http.serve` that has the listener to itself.
    served: Mutex<Option<Served>>,
}

/// What an accept on a listener came to.
pub enum Accepted {
    Conn(std::net::TcpStream),
    /// The accept was given up ([`TcpListenerHandle::stop`] with its
    /// flag): nobody waits for it.
    GivenUp,
    /// An `http.serve` has the listener to itself: no other accept
    /// takes a connection from it, also not one that was waiting when
    /// the server started.
    Served,
}

/// The mark of an `http.serve` on its listener.
struct Served {
    /// Which call of `http.serve` it is.
    token: u64,
    /// The cancel flag of the task that serves; `None` for the
    /// program's own thread.
    cancelled: Option<Arc<AtomicBool>>,
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
            served: Mutex::new(None),
        }
    }

    /// The address the listener is bound to.
    pub fn local_addr(&self) -> std::io::Result<std::net::SocketAddr> {
        self.listener.local_addr()
    }

    /// Whether an `http.serve` has the listener to itself: nothing
    /// else accepts on it then.
    ///
    /// A server whose task has been cancelled counts no longer, even
    /// before that task has ended: who cancels a server and at once
    /// accepts on its listener, or serves it again, is not refused.
    /// (The accept of the old server is given up when its task ends,
    /// and a connection that reached it is kept for the next accept,
    /// as after any accept that was given up.)
    pub fn is_served(&self) -> bool {
        self.served.lock().as_ref().is_some_and(|served| {
            !served
                .cancelled
                .as_ref()
                .is_some_and(|cancelled| cancelled.load(AtomicOrdering::SeqCst))
        })
    }

    /// An `http.serve` takes the listener for itself, until
    /// [`TcpListenerHandle::served_no_more`] with the token given here.
    /// `cancelled` is the cancel flag of the task that serves. `None`
    /// if the listener is served already.
    pub fn serve(&self, cancelled: Option<Arc<AtomicBool>>) -> Option<u64> {
        static TOKENS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let mut served = self.served.lock();
        let lives = served.as_ref().is_some_and(|served| {
            !served
                .cancelled
                .as_ref()
                .is_some_and(|cancelled| cancelled.load(AtomicOrdering::SeqCst))
        });
        if lives {
            return None;
        }
        let token = TOKENS.fetch_add(1, AtomicOrdering::Relaxed);
        *served = Some(Served { token, cancelled });
        drop(served);
        // An accept that waits on the listener gives way: the one in
        // the system's call is woken as one that is given up is, and
        // those that wait their turn look again. Each ends with
        // [`Accepted::Served`].
        let current = self.accepting.lock().current.clone();
        if let Some(current) = current {
            self.stop(&current);
        }
        self.turn.notify_all();
        Some(token)
    }

    /// The `http.serve` that took the listener with `token` has ended.
    pub fn served_no_more(&self, token: u64) {
        let mut served = self.served.lock();
        if served.as_ref().is_some_and(|served| served.token == token) {
            *served = None;
        }
    }

    /// The next connection, for the accept whose flag is `stopped`.
    /// A client's connection is never lost to an accept that was
    /// given up, or that a server has taken the listener from: it is
    /// kept for the next one.
    ///
    /// `server` says that the accept is the one of the `http.serve`
    /// that has the listener. Such an accept also takes the
    /// connections that are ready besides the one it returns, without
    /// waiting, and keeps them ([`TcpListenerHandle::take_kept`]): a
    /// server takes a burst of connections off the system's queue in
    /// one go, not one for each time its task comes round. Any other
    /// accept ends with [`Accepted::Served`] while a server has the
    /// listener.
    pub fn accept(&self, stopped: &Arc<AtomicBool>, server: bool) -> std::io::Result<Accepted> {
        let foreign = |listener: &Self| !server && listener.is_served();
        {
            let mut accepting = self.accepting.lock();
            loop {
                if foreign(self) {
                    return Ok(Accepted::Served);
                }
                if stopped.load(AtomicOrdering::SeqCst) {
                    return Ok(Accepted::GivenUp);
                }
                if let Some(stream) = accepting.kept.pop_front() {
                    return Ok(Accepted::Conn(stream));
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
                        if stopped.load(AtomicOrdering::SeqCst) || foreign(self) {
                            // A client's, and this accept is not to
                            // have it: it is the next accept's.
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
        if server && matches!(result, Ok(Some(_))) {
            self.keep_the_ready();
        }
        self.accepting.lock().current = None;
        self.turn.notify_all();
        Ok(match result? {
            Some(stream) => Accepted::Conn(stream),
            // A server took the listener while this accept waited.
            None if foreign(self) => Accepted::Served,
            None => Accepted::GivenUp,
        })
    }

    /// Accept the connections that are ready, without waiting, and
    /// keep them. Called by the accept that has the turn.
    fn keep_the_ready(&self) {
        /// How many at a time: the accept returns with what it has.
        const AT_ONCE: usize = 256;
        if self.listener.set_nonblocking(true).is_err() {
            return;
        }
        let mut taken = 0;
        while taken < AT_ONCE {
            match self.listener.accept() {
                Ok((stream, peer)) => {
                    let mut accepting = self.accepting.lock();
                    match accepting.wakes.iter().position(|addr| *addr == peer) {
                        // A connection made to wake an accept: not a
                        // client's.
                        Some(wake) => {
                            accepting.wakes.swap_remove(wake);
                        }
                        // (Where a connection takes after its listener,
                        // it would not wait either.)
                        None if stream.set_nonblocking(false).is_ok() => {
                            accepting.kept.push_back(stream);
                            taken += 1;
                        }
                        None => {}
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                // Nothing more is ready.
                Err(_) => break,
            }
        }
        let _ = self.listener.set_nonblocking(false);
    }

    /// A connection that was kept for the next accept, if there is
    /// one: for a server that has the listener to itself, which takes
    /// them where it runs.
    pub fn take_kept(&self) -> Option<std::net::TcpStream> {
        self.accepting.lock().kept.pop_front()
    }

    /// Ask the system to queue more connections that wait to be
    /// accepted than the 128 a listener is bound with: a burst of
    /// clients beyond the queue is not refused but left to try again,
    /// a second or more later each time. The system caps the number
    /// at its own limit. (Elsewhere than on Unix the queue stays as it
    /// was bound.)
    pub fn widen_backlog(&self) {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            /// What Linux allows by default (`net.core.somaxconn`).
            const BACKLOG: libc::c_int = 4096;
            // SAFETY: `listen` on a descriptor that this handle owns
            // and that is listening already changes the length of its
            // queue and nothing else; a failure leaves it as it was.
            unsafe {
                let _ = libc::listen(self.listener.as_raw_fd(), BACKLOG);
            }
        }
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
        /// Held while the socket is switched to non-blocking for one
        /// call, where the system has no call that does not wait
        /// (see [`without_waiting`]). Nowhere on Unix.
        switching: Mutex<()>,
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
                switching: Mutex::new(()),
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
                socket,
                reading,
                switching,
                ..
            } => {
                let _turn = reading.lock();
                loop {
                    match (&*socket).read(buf) {
                        Err(e) if switched_meanwhile(&e) => drop(switching.lock()),
                        read => return read,
                    }
                }
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
                socket,
                writing,
                switching,
                ..
            } => {
                let _turn = writing.lock();
                let mut rest = buf;
                while !rest.is_empty() {
                    match (&*socket).write(rest) {
                        Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
                        Ok(n) => rest = &rest[n..],
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(e) if switched_meanwhile(&e) => drop(switching.lock()),
                        Err(e) => return Err(e),
                    }
                }
                (&*socket).flush()
            }
            TcpIo::Tls { both, .. } => {
                let mut both = both.lock();
                both.write_all(buf)?;
                both.flush()
            }
        }
    }

    /// Write what of `bytes` the system takes at once, without waiting
    /// for anything, and say how much that was: for a writer that
    /// must not wait where it is (a worker of the scheduler, a task
    /// that is being dropped). Nothing if a write is in flight, or the
    /// connection is closed or not a plain one.
    ///
    /// A read of the connection may be in flight on another thread
    /// meanwhile: it is not disturbed (see [`send_now`]).
    pub fn write_now(&self, bytes: &[u8]) -> usize {
        if self.is_closed() {
            return 0;
        }
        let TcpIo::Plain {
            socket,
            writing,
            switching,
            ..
        } = &self.io
        else {
            return 0;
        };
        let Some(_turn) = writing.try_lock() else {
            return 0;
        };
        let mut written = 0;
        while written < bytes.len() {
            match send_now(socket, switching, &bytes[written..]) {
                Ok(0) => break,
                Ok(n) => written += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        written
    }

    /// Read what is there on a plain connection, without waiting for
    /// anything: `Some(0)` at its end, `None` if nothing is there (or
    /// a read is in flight, or the connection is closed or not a plain
    /// one).
    pub fn read_now(&self, buf: &mut [u8]) -> Option<usize> {
        if self.is_closed() {
            return None;
        }
        let TcpIo::Plain {
            socket,
            reading,
            switching,
            ..
        } = &self.io
        else {
            return None;
        };
        let _turn = reading.try_lock()?;
        loop {
            match recv_now(socket, switching, buf) {
                Ok(n) => return Some(n),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return None,
            }
        }
    }

    /// Say that nothing more is written on a plain connection: the
    /// peer reads its end, and may still send.
    pub fn end_writes(&self) {
        if let TcpIo::Plain { socket, .. } = &self.io {
            let _ = socket.shutdown(std::net::Shutdown::Write);
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

// "Without waiting" is a property of the one call, not a mode of the
// socket: another thread may be in a read or a write of the same
// connection (the reader of a request while its 408 is written), and
// must not find the socket non-blocking.
//
// - Unix: `send` and `recv` with `MSG_DONTWAIT`. The socket's mode is
//   never touched.
// - Elsewhere (Windows) no such call exists. The socket is switched to
//   non-blocking for the one call, under the connection's `switching`
//   lock ([`without_waiting`]); a call that blocks already is not
//   affected by the switch, and one that another thread begins inside
//   the switch and that finds nothing to do (`WouldBlock`) waits for
//   the lock and tries again ([`switched_meanwhile`]): to its caller
//   it has only waited.

/// Send what the system takes of `bytes` at once.
#[cfg(unix)]
fn send_now(socket: &std::net::TcpStream, _: &Mutex<()>, bytes: &[u8]) -> std::io::Result<usize> {
    use std::os::unix::io::AsRawFd;
    // A connection that the peer has closed gives an error, not a
    // signal. (Apple's systems have no such flag: there the socket
    // has the option, set by the standard library.)
    #[cfg(not(target_vendor = "apple"))]
    const FLAGS: libc::c_int = libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL;
    #[cfg(target_vendor = "apple")]
    const FLAGS: libc::c_int = libc::MSG_DONTWAIT;
    // SAFETY: the descriptor is the open socket's, and the pointer and
    // length are those of `bytes`, which outlives the call.
    let sent = unsafe {
        libc::send(
            socket.as_raw_fd(),
            bytes.as_ptr().cast(),
            bytes.len(),
            FLAGS,
        )
    };
    usize::try_from(sent).map_err(|_| std::io::Error::last_os_error())
}

/// Receive what is there, into `buf`; an error if nothing is.
#[cfg(unix)]
fn recv_now(socket: &std::net::TcpStream, _: &Mutex<()>, buf: &mut [u8]) -> std::io::Result<usize> {
    use std::os::unix::io::AsRawFd;
    // SAFETY: the descriptor is the open socket's, and the pointer and
    // length are those of `buf`, which is not used otherwise during
    // the call.
    let received = unsafe {
        libc::recv(
            socket.as_raw_fd(),
            buf.as_mut_ptr().cast(),
            buf.len(),
            libc::MSG_DONTWAIT,
        )
    };
    usize::try_from(received).map_err(|_| std::io::Error::last_os_error())
}

#[cfg(not(unix))]
fn send_now(
    socket: &std::net::TcpStream,
    switching: &Mutex<()>,
    bytes: &[u8],
) -> std::io::Result<usize> {
    use std::io::Write;
    without_waiting(socket, switching, |mut socket| socket.write(bytes))
}

#[cfg(not(unix))]
fn recv_now(
    socket: &std::net::TcpStream,
    switching: &Mutex<()>,
    buf: &mut [u8],
) -> std::io::Result<usize> {
    use std::io::Read;
    without_waiting(socket, switching, |mut socket| socket.read(buf))
}

/// Do one call on `socket` that does not wait, where only a mode of
/// the socket can say so: the socket is non-blocking for the call,
/// under `switching`.
#[cfg(any(not(unix), test))]
fn without_waiting<T>(
    socket: &std::net::TcpStream,
    switching: &Mutex<()>,
    call: impl FnOnce(&std::net::TcpStream) -> std::io::Result<T>,
) -> std::io::Result<T> {
    let _switched = switching.lock();
    socket.set_nonblocking(true)?;
    let result = call(socket);
    socket.set_nonblocking(false)?;
    result
}

/// Whether a call that waits found nothing to do because the socket
/// was non-blocking for another thread's call ([`without_waiting`]):
/// it waits for that call's lock and tries again. Never on Unix,
/// where no socket of a connection is switched.
fn switched_meanwhile(error: &std::io::Error) -> bool {
    cfg!(not(unix)) && error.kind() == std::io::ErrorKind::WouldBlock
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};

    /// An accept that is asked for all that is ready returns one
    /// connection and keeps the others that have arrived: a server
    /// takes a burst off the system's queue with one accept. A plain
    /// accept takes its one connection and leaves the rest where they
    /// are.
    #[test]
    fn an_accept_for_all_that_is_ready_keeps_the_rest() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let listener = TcpListenerHandle::new(0, listener);
        listener.widen_backlog();
        // A connection is in the listener's queue when `connect` has
        // returned.
        let clients: Vec<TcpStream> = (0..40)
            .map(|_| TcpStream::connect(addr).expect("connect"))
            .collect();
        let flag = Arc::new(AtomicBool::new(false));
        let first = listener.accept(&flag, false).expect("accept");
        assert!(matches!(first, Accepted::Conn(_)));
        assert!(listener.take_kept().is_none());
        let second = listener.accept(&flag, true).expect("accept");
        assert!(matches!(second, Accepted::Conn(_)));
        let mut kept = Vec::new();
        while let Some(conn) = listener.take_kept() {
            kept.push(conn);
        }
        assert_eq!(kept.len(), clients.len() - 2);
        // What was kept waits like any connection: a read on it is
        // not refused for want of bytes.
        use std::io::{Read, Write};
        let mut client = &clients[2];
        client.write_all(b"x").expect("write");
        let mut byte = [0u8; 1];
        let from = client.local_addr().expect("addr");
        let conn = kept
            .iter_mut()
            .find(|conn| conn.peer_addr().ok() == Some(from))
            .expect("the client's connection");
        conn.read_exact(&mut byte).expect("a read that waits");
        assert_eq!(&byte, b"x");
    }

    /// What must not wait does not: a write to a peer that reads
    /// nothing stops when the system takes no more, a read of a
    /// connection that has nothing says so, and a read on another
    /// thread that waits meanwhile is not disturbed: it gets its bytes
    /// when they come.
    #[test]
    fn a_call_that_does_not_wait_disturbs_no_call_that_does() {
        use std::io::Write;
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let mut peer = TcpStream::connect(listener.local_addr().expect("addr")).expect("connect");
        let conn = TcpStreamHandle::plain(0, listener.accept().expect("accept").0);
        let mut buf = [0u8; 16];
        assert_eq!(conn.read_now(&mut buf), None);
        // A reader waits on the connection on a thread of its own.
        let reader = {
            let conn = conn.clone();
            std::thread::spawn(move || {
                let mut all = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    match conn.read(&mut buf) {
                        Ok(0) => return Ok(all),
                        Ok(n) => all.extend_from_slice(&buf[..n]),
                        Err(e) => return Err(e),
                    }
                }
            })
        };
        // Meanwhile, writes that do not wait: more than the system
        // takes for a peer that reads nothing.
        let chunk = vec![b'w'; 64 * 1024];
        let mut written = 0;
        let mut full = false;
        for _ in 0..10_000 {
            let n = conn.write_now(&chunk);
            written += n;
            if n < chunk.len() {
                full = true;
                break;
            }
        }
        assert!(
            full,
            "64 KiB x 10,000 were taken for a peer that reads nothing"
        );
        // The reader is still waiting, and gets what the peer sends.
        for _ in 0..200 {
            peer.write_all(b"0123456789").expect("write");
            let _ = conn.write_now(b"x");
        }
        peer.shutdown(std::net::Shutdown::Write).expect("shutdown");
        let read = reader
            .join()
            .expect("joined")
            .expect("the reader was disturbed");
        assert_eq!(read.len(), 2000);
        assert!(written > 0);
        assert_eq!(conn.read_now(&mut buf), Some(0));
    }

    /// The form for systems without a call that does not wait: the
    /// socket is non-blocking for the one call and blocking after it.
    #[test]
    fn a_switched_call_leaves_the_socket_as_it_was() {
        use std::io::{Read, Write};
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let mut peer = TcpStream::connect(listener.local_addr().expect("addr")).expect("connect");
        let (conn, _) = listener.accept().expect("accept");
        let switching = Mutex::new(());
        let mut buf = [0u8; 4];
        let nothing = without_waiting(&conn, &switching, |mut conn| conn.read(&mut buf));
        assert_eq!(
            nothing.expect_err("nothing to read").kind(),
            std::io::ErrorKind::WouldBlock
        );
        // Blocking again: a read waits for the peer's bytes.
        let late = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            peer.write_all(b"late").expect("write");
        });
        (&conn).read_exact(&mut buf).expect("a read that waits");
        assert_eq!(&buf, b"late");
        late.join().expect("joined");
    }

    /// An accept that waits in the system's call when an `http.serve`
    /// takes the listener gives way at that moment, and so does one
    /// that waits its turn behind it: the server owns every client
    /// from then on.
    #[test]
    fn a_server_takes_the_listener_from_the_accepts_that_wait() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let listener = Arc::new(TcpListenerHandle::new(0, listener));
        let waiting: Vec<_> = (0..2)
            .map(|_| {
                let listener = listener.clone();
                std::thread::spawn(move || {
                    let flag = Arc::new(AtomicBool::new(false));
                    listener.accept(&flag, false).expect("accept")
                })
            })
            .collect();
        // One of them is in the call, or about to be.
        while listener.accepting.lock().current.is_none() {
            std::thread::yield_now();
        }
        let token = listener.serve(None).expect("nobody serves it yet");
        for accept in waiting {
            assert!(matches!(accept.join().expect("joined"), Accepted::Served));
        }
        // The next client is the server's, and no connection that woke
        // an accept is taken for one.
        let mut client = TcpStream::connect(addr).expect("connect");
        let flag = Arc::new(AtomicBool::new(false));
        let Accepted::Conn(mut conn) = listener.accept(&flag, true).expect("accept") else {
            panic!("the server's accept got no connection");
        };
        use std::io::{Read, Write};
        client.write_all(b"x").expect("write");
        let mut byte = [0u8; 1];
        conn.read_exact(&mut byte).expect("the client's byte");
        assert!(listener.take_kept().is_none());
        // Any other accept is told, at once.
        assert!(matches!(
            listener.accept(&flag, false).expect("accept"),
            Accepted::Served
        ));
        listener.served_no_more(token);
    }
}
