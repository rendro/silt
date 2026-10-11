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

/// What the system took of bytes that are written whole or not at all
/// ([`TcpStreamHandle::write_whole_in_turn`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Taken {
    All,
    /// None of them: nothing stands on the connection.
    Nothing,
    /// Some of them: the connection was shut down behind them.
    Part,
}

/// What a write that takes its turn does with the connection before
/// the turn passes on.
#[derive(Clone, Copy)]
enum Turn {
    /// Nothing.
    Passes,
    /// It shuts the connection down if the system took a part of its
    /// bytes.
    WholeOrNothing,
    /// It ends the writes.
    EndsWrites,
}

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
        /// call, where the system has no call that is certain not to
        /// wait (see `without_waiting`). Nowhere on Linux.
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

    /// Read exactly `n` bytes. The buffer grows as the bytes arrive: a
    /// peer that sends few costs what it sent, however many were asked
    /// for. A connection that ends before the `n`th byte is
    /// `UnexpectedEof`.
    pub fn read_exact(&self, n: usize) -> std::io::Result<Vec<u8>> {
        use std::io::Read;
        let most = u64::try_from(n).unwrap_or(u64::MAX);
        let mut buf = Vec::new();
        match &self.io {
            TcpIo::Plain {
                socket, reading, ..
            } => {
                let _turn = reading.lock();
                Read::take(socket, most).read_to_end(&mut buf)?;
            }
            TcpIo::Tls { both, .. } => {
                (&mut *both.lock()).take(most).read_to_end(&mut buf)?;
            }
        }
        match buf.len() == n {
            true => Ok(buf),
            false => Err(std::io::ErrorKind::UnexpectedEof.into()),
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

    /// Write what of `bytes` the system takes at once, and say how
    /// much that was: for a writer that must not wait for the system
    /// where it is (a worker of the scheduler, a task that is being
    /// dropped). Nothing if the connection is closed or not a plain
    /// one.
    ///
    /// The writer has its turn after a write that is in flight, and
    /// waits for that turn. So this is for a connection on which no
    /// write that waits for the system ([`TcpStreamHandle::write_all`])
    /// is in flight when it is called: a connection of `http.serve`,
    /// whose task writes nothing while the rest of a response is on
    /// its way and ends the connection if it gives that up. (The rule
    /// stands at `ConnState::Sending` in `builtins/http.rs`; a new
    /// call there has to keep it.) Whoever has the turn then is in a
    /// call of this kind itself: it asks the system for what it takes
    /// at once, and does nothing else under the lock. The wait is as
    /// long as that, and a thread of the scheduler that waits here is
    /// not held: the other writer needs nothing of the scheduler to
    /// finish.
    ///
    /// A read of the connection may be in flight on another thread
    /// meanwhile: it is not disturbed (see `send_now`).
    pub fn write_in_turn(&self, bytes: &[u8]) -> usize {
        self.write_at_once(bytes, Turn::Passes)
    }

    /// [`TcpStreamHandle::write_in_turn`] for the last bytes that are
    /// written on the connection (a last word): the writes are ended
    /// in the same turn ([`TcpStreamHandle::end_writes`]), so that no
    /// other writer comes between the two. One whose turn comes later
    /// writes nothing.
    pub fn write_last_in_turn(&self, bytes: &[u8]) -> usize {
        self.write_at_once(bytes, Turn::EndsWrites)
    }

    /// [`TcpStreamHandle::write_in_turn`] for bytes that mean nothing
    /// in part and need not be written at all (an interim response):
    /// what the system took of them. If it took a part, the
    /// connection is shut down before the turn passes on: who writes
    /// next writes nothing, and not something behind a part. If it
    /// took nothing, nothing of them stands on the connection, which
    /// is as it was.
    pub fn write_whole_in_turn(&self, bytes: &[u8]) -> Taken {
        match self.write_at_once(bytes, Turn::WholeOrNothing) {
            0 => Taken::Nothing,
            taken if taken == bytes.len() => Taken::All,
            _ => Taken::Part,
        }
    }

    fn write_at_once(&self, bytes: &[u8], turn: Turn) -> usize {
        let TcpIo::Plain {
            socket,
            writing,
            switching,
            ..
        } = &self.io
        else {
            return 0;
        };
        let _turn = match writing.try_lock() {
            Some(turn) => turn,
            None => {
                #[cfg(any(test, feature = "test-hooks"))]
                tell_write_watches(self, WriteMoment::Waits);
                writing.lock()
            }
        };
        let mut written = 0;
        // Asked in its turn: a connection that the writer before shut
        // down is closed for this one.
        while written < bytes.len() && !self.is_closed() {
            match send_now(socket, switching, &bytes[written..]) {
                Ok(0) => break,
                Ok(n) => written += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        match turn {
            Turn::Passes => {}
            Turn::WholeOrNothing => {
                if (1..bytes.len()).contains(&written) {
                    self.shut_down();
                }
            }
            Turn::EndsWrites => {
                let _ = socket.shutdown(std::net::Shutdown::Write);
            }
        }
        #[cfg(any(test, feature = "test-hooks"))]
        tell_write_watches(self, WriteMoment::Written);
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

// ── What a test is told of the writes that take their turn ─────────

/// Test-only: a moment in a write that takes its turn
/// ([`TcpStreamHandle::write_in_turn`]).
#[cfg(any(test, feature = "test-hooks"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteMoment {
    /// Another writer has the turn: this one is about to wait for it.
    Waits,
    /// The system has taken what it takes at once, and the writer
    /// still has its turn.
    Written,
}

#[cfg(any(test, feature = "test-hooks"))]
type WriteWatch = Arc<dyn Fn(&TcpStreamHandle, WriteMoment) + Send + Sync>;

#[cfg(any(test, feature = "test-hooks"))]
static WRITE_WATCHES: Mutex<Vec<(u64, WriteWatch)>> = Mutex::new(Vec::new());

/// Test-only: a watch of the writes ([`watch_writes`]), until it is
/// dropped.
#[cfg(any(test, feature = "test-hooks"))]
pub struct WriteWatching(u64);

/// Test-only: call `watch` at the moments of every write that takes
/// its turn, on whichever connection of the process (the watch tells
/// its own by the connection's addresses), on the thread that writes.
/// A watch that does not return holds the writer where it is: at
/// [`WriteMoment::Written`], in its turn. That is how a test brings
/// two writes of a connection together, which nothing outside the
/// process can do.
#[cfg(any(test, feature = "test-hooks"))]
pub fn watch_writes(
    watch: impl Fn(&TcpStreamHandle, WriteMoment) + Send + Sync + 'static,
) -> WriteWatching {
    static WATCHES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = WATCHES.fetch_add(1, AtomicOrdering::Relaxed);
    WRITE_WATCHES.lock().push((id, Arc::new(watch)));
    WriteWatching(id)
}

#[cfg(any(test, feature = "test-hooks"))]
impl Drop for WriteWatching {
    fn drop(&mut self) {
        WRITE_WATCHES.lock().retain(|(id, _)| *id != self.0);
    }
}

#[cfg(any(test, feature = "test-hooks"))]
fn tell_write_watches(conn: &TcpStreamHandle, moment: WriteMoment) {
    // Called outside the lock of the list: a watch may hold its
    // writer.
    let watches: Vec<WriteWatch> = WRITE_WATCHES
        .lock()
        .iter()
        .map(|(_, watch)| watch.clone())
        .collect();
    for watch in watches {
        watch(conn, moment);
    }
}

// "Without waiting" is a property of the one call, not a mode of the
// socket: another thread may be in a read or a write of the same
// connection (the reader of a request while its 408 is written), and
// must not find the socket non-blocking.
//
// - Linux: `send` and `recv` with `MSG_DONTWAIT`. The socket's mode is
//   never touched.
// - Elsewhere no call does that for certain. (Windows has no such
//   flag. Apple's systems have the flag, and their `send` honours it
//   only for the socket buffer's lock: with no room in the buffer it
//   waits all the same, which held a worker in the write of a
//   response that its client did not take.) The socket is switched to
//   non-blocking for the one call, under the connection's `switching`
//   lock (`without_waiting`); a call that blocks already goes on, and
//   one of another thread that finds nothing to do while the socket
//   is switched (`WouldBlock`) waits for the lock and tries again
//   (`switched_meanwhile`): to its caller it has only waited.

/// Send what the system takes of `bytes` at once.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn send_now(socket: &std::net::TcpStream, _: &Mutex<()>, bytes: &[u8]) -> std::io::Result<usize> {
    use std::os::unix::io::AsRawFd;
    // A connection that the peer has closed gives an error, not a
    // signal.
    const FLAGS: libc::c_int = libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL;
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
#[cfg(any(target_os = "linux", target_os = "android"))]
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

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn send_now(
    socket: &std::net::TcpStream,
    switching: &Mutex<()>,
    bytes: &[u8],
) -> std::io::Result<usize> {
    use std::io::Write;
    without_waiting(socket, switching, |mut socket| socket.write(bytes))
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
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
#[cfg(any(not(any(target_os = "linux", target_os = "android")), test))]
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
/// was non-blocking for another thread's call (`without_waiting`):
/// it waits for that call's lock and tries again. Never on Linux,
/// where no socket of a connection is switched.
fn switched_meanwhile(error: &std::io::Error) -> bool {
    cfg!(not(any(target_os = "linux", target_os = "android")))
        && error.kind() == std::io::ErrorKind::WouldBlock
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
            let n = conn.write_in_turn(&chunk);
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
            let _ = conn.write_in_turn(b"x");
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

    /// A write that takes its turn waits for the write that has it,
    /// and goes on when that one is done: the peer gets both whole,
    /// one after the other. The first writer is held in its turn by
    /// the hook of the writes, when the system has taken its bytes;
    /// the second reports that it waits, and has not written when it
    /// does.
    #[test]
    fn a_write_in_turn_comes_after_the_write_that_has_the_turn() {
        use std::io::Read;
        use std::sync::mpsc;
        let patience = std::time::Duration::from_secs(60);
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let mut peer = TcpStream::connect(listener.local_addr().expect("addr")).expect("connect");
        peer.set_read_timeout(Some(patience)).expect("read timeout");
        let conn = TcpStreamHandle::plain(0, listener.accept().expect("accept").0);
        // The first write of this connection stays in its turn until
        // `go` is dropped; a writer that finds the turn taken says so.
        let (go, held) = mpsc::channel::<()>();
        let held = Mutex::new(Some(held));
        let waits = Arc::new(AtomicBool::new(false));
        let (waiting, this) = (waits.clone(), Arc::as_ptr(&conn) as usize);
        let _watching = watch_writes(move |conn, moment| {
            if !std::ptr::eq(conn, this as *const TcpStreamHandle) {
                return;
            }
            match moment {
                WriteMoment::Written => {
                    let held = held.lock().take();
                    if let Some(held) = held {
                        let _ = held.recv();
                    }
                }
                WriteMoment::Waits => waiting.store(true, AtomicOrdering::SeqCst),
            }
        });
        let first = {
            let conn = conn.clone();
            std::thread::spawn(move || conn.write_in_turn(b"first"))
        };
        // The first write has the turn when its bytes have arrived.
        let mut both = [0u8; 11];
        peer.read_exact(&mut both[..5]).expect("the first write");
        let second = {
            let conn = conn.clone();
            std::thread::spawn(move || conn.write_in_turn(b"second"))
        };
        let limit = std::time::Instant::now() + patience;
        while !waits.load(AtomicOrdering::SeqCst) {
            assert!(
                std::time::Instant::now() < limit,
                "the second write did not find the turn taken"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(
            !first.is_finished(),
            "the first write did not keep its turn"
        );
        assert!(!second.is_finished(), "the second write did not wait");
        drop(go);
        peer.read_exact(&mut both[5..]).expect("the second write");
        assert_eq!(first.join().expect("joined"), 5);
        assert_eq!(second.join().expect("joined"), 6);
        assert_eq!(&both, b"firstsecond");
    }

    /// A last word and the end of the writes are one turn: the peer
    /// reads the word and then the end, a writer whose turn comes
    /// later writes nothing, and the connection still reads.
    #[test]
    fn a_last_write_ends_the_writes_in_its_turn() {
        use std::io::{Read, Write};
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let mut peer = TcpStream::connect(listener.local_addr().expect("addr")).expect("connect");
        peer.set_read_timeout(Some(std::time::Duration::from_secs(60)))
            .expect("read timeout");
        let conn = TcpStreamHandle::plain(0, listener.accept().expect("accept").0);
        assert_eq!(conn.write_last_in_turn(b"the last word"), 13);
        assert_eq!(conn.write_in_turn(b"behind it"), 0);
        assert_eq!(conn.write_whole_in_turn(b"behind it"), Taken::Nothing);
        assert!(!conn.is_closed());
        let mut all = Vec::new();
        peer.read_to_end(&mut all).expect("the end");
        assert_eq!(all, b"the last word");
        peer.write_all(b"heard").expect("write");
        let mut heard = [0u8; 5];
        let mut read = 0;
        while read < heard.len() {
            read += conn.read(&mut heard[read..]).expect("a read");
        }
        assert_eq!(&heard, b"heard");
    }

    /// Bytes that mean nothing in part are written whole or not at
    /// all, or the connection ends with them. Whole chunks go to a
    /// peer that reads nothing until the system does not take one. If
    /// it took a part of that one, the connection is closed for every
    /// writer that comes after, and the peer reads the chunks, the
    /// part, and the end. If it took nothing, the connection is as it
    /// was: the peer reads the chunks, and what is written then.
    /// (Which of the two it is, is the system's to say.)
    #[test]
    fn what_is_not_written_whole_is_not_written_or_ends_the_connection() {
        use std::io::Read;
        let patience = std::time::Duration::from_secs(60);
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let mut peer = TcpStream::connect(listener.local_addr().expect("addr")).expect("connect");
        peer.set_read_timeout(Some(patience)).expect("read timeout");
        let conn = TcpStreamHandle::plain(0, listener.accept().expect("accept").0);
        let chunk = vec![b'w'; 64 * 1024];
        let mut whole = 0;
        let last = loop {
            match conn.write_whole_in_turn(&chunk) {
                Taken::All => whole += 1,
                last => break last,
            }
            assert!(
                whole < 10_000,
                "64 KiB x 10,000 were taken for a peer that reads nothing"
            );
        };
        if last == Taken::Part {
            assert!(conn.is_closed());
            assert_eq!(conn.write_in_turn(b"behind the part"), 0);
            assert_eq!(conn.write_whole_in_turn(b"behind the part"), Taken::Nothing);
            let mut all = Vec::new();
            peer.read_to_end(&mut all).expect("the end");
            assert!(
                (whole * chunk.len() + 1..(whole + 1) * chunk.len()).contains(&all.len()),
                "{} bytes after {whole} whole chunks",
                all.len()
            );
            assert!(all.iter().all(|byte| *byte == b'w'));
            return;
        }
        assert!(!conn.is_closed());
        let mut chunks = vec![0u8; whole * chunk.len()];
        peer.read_exact(&mut chunks).expect("the whole chunks");
        // The system has room again when the peer has read.
        let limit = std::time::Instant::now() + patience;
        while conn.write_whole_in_turn(b"then") != Taken::All {
            assert!(!conn.is_closed(), "a part of four bytes was taken");
            assert!(
                std::time::Instant::now() < limit,
                "nothing is taken for a peer that has read everything"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let mut then = [0u8; 4];
        peer.read_exact(&mut then).expect("what was written then");
        assert_eq!(&then, b"then");
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
