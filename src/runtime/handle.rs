use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

use crate::runtime::sync::{Cell, Completion, Wake};
use crate::source::Span;
use crate::value::Value;
use crate::vm::{VmError, Wanted};

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

    /// What is decrypted already, read without touching the socket:
    /// `Some(n)` (`Some(0)` at the end of the connection), `None` when
    /// the TLS library needs bytes of the socket first
    /// ([`ReadWrite::read_socket`]). A plain socket has no such part.
    fn read_buffered(&mut self, _buf: &mut [u8]) -> std::io::Result<Option<usize>> {
        Ok(None)
    }

    /// Take into the TLS library what the socket has: one read of it,
    /// which the caller has waited for, so that it does not wait.
    fn read_socket(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    /// Whether it reads in those two parts ([`ReadWrite::read_buffered`],
    /// [`ReadWrite::read_socket`]): the TLS connections do. Another
    /// reads with `read`, which may wait.
    fn reads_in_parts(&self) -> bool {
        false
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
///
/// A listener that is closed ([`TcpListenerHandle::close`]) accepts
/// nothing more: its socket is closed as soon as no accept is in the OS
/// call, and a client that connects then is refused.
pub struct TcpListenerHandle {
    pub id: usize,
    /// The socket; `None` once the listener is closed. An accept in the
    /// OS call holds it until it returns.
    listener: Mutex<Option<Arc<std::net::TcpListener>>>,
    /// The address it was bound to, which a closed listener still says.
    addr: std::io::Result<std::net::SocketAddr>,
    closed: AtomicBool,
    accepting: Mutex<Accepting>,
    /// The accepts that wait their turn wait here.
    turn: parking_lot::Condvar,
    /// The `http.serve` that has the listener to itself.
    served: Mutex<Option<Served>>,
}

/// What an accept on a listener came to.
pub enum Accepted {
    Conn(std::net::TcpStream),
    /// The accept was given up: nobody wants its connection
    /// (`Wanted::gone`).
    GivenUp,
    /// An `http.serve` has the listener to itself: no other accept
    /// takes a connection from it, also not one that was waiting when
    /// the server started.
    Served,
    /// The listener is closed.
    Closed,
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
    /// The accept that is in the OS call.
    current: Option<Current>,
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
            addr: listener.local_addr(),
            listener: Mutex::new(Some(Arc::new(listener))),
            closed: AtomicBool::new(false),
            accepting: Mutex::default(),
            turn: parking_lot::Condvar::new(),
            served: Mutex::new(None),
        }
    }

    /// The address the listener is bound to.
    pub fn local_addr(&self) -> std::io::Result<std::net::SocketAddr> {
        match &self.addr {
            Ok(addr) => Ok(*addr),
            Err(e) => Err(std::io::Error::new(e.kind(), e.to_string())),
        }
    }

    /// Whether the listener is closed.
    pub fn is_closed(&self) -> bool {
        self.closed.load(AtomicOrdering::SeqCst)
    }

    /// Close the listener: no accept takes a connection from it any
    /// more. An accept that waits returns [`Accepted::Closed`] (the one
    /// in the OS call is woken as one that is given up is), the
    /// connections kept for the next accept are closed, and the socket
    /// is closed once no accept holds it, so a client that connects
    /// after that is refused. Closing a closed listener does nothing.
    pub fn close(&self) {
        let mut accepting = self.accepting.lock();
        if self.closed.swap(true, AtomicOrdering::SeqCst) {
            return;
        }
        // (An accept that is in the OS call holds the socket: it is
        // woken while the socket still listens, and drops it as it
        // returns.)
        let socket = self.listener.lock().take();
        if accepting.current.is_some() {
            self.wake_the_call(&mut accepting);
        }
        accepting.kept.clear();
        drop(accepting);
        drop(socket);
        self.turn.notify_all();
    }

    /// The socket, for an accept; `None` once the listener is closed.
    fn socket(&self) -> Option<Arc<std::net::TcpListener>> {
        self.listener.lock().clone()
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
        let mut accepting = self.accepting.lock();
        if let Some(current) = &accepting.current {
            current.stopped.store(true, AtomicOrdering::SeqCst);
            self.wake_the_call(&mut accepting);
        }
        drop(accepting);
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

    /// The next connection, for the accept whose waiter is `wanted`. A
    /// client's connection is never lost to an accept that was given
    /// up, or that a server has taken the listener from: it is kept for
    /// the next one. The accept claims the connection it returns
    /// (`Wanted::claim`): one that it cannot claim is kept.
    ///
    /// `server` says that the accept is the one of the `http.serve`
    /// that has the listener. Such an accept also takes the
    /// connections that are ready besides the one it returns, without
    /// waiting, and keeps them ([`TcpListenerHandle::take_kept`]): a
    /// server takes a burst of connections off the system's queue in
    /// one go, not one for each time its task comes round. Any other
    /// accept ends with [`Accepted::Served`] while a server has the
    /// listener.
    pub fn accept(&self, wanted: &Wanted, server: bool) -> std::io::Result<Accepted> {
        let foreign = |listener: &Self| !server && listener.is_served();
        let stopped = Arc::new(AtomicBool::new(false));
        let given_up = || stopped.load(AtomicOrdering::SeqCst) || wanted.gone();
        let socket;
        {
            let mut accepting = self.accepting.lock();
            loop {
                if self.is_closed() {
                    return Ok(Accepted::Closed);
                }
                if foreign(self) {
                    return Ok(Accepted::Served);
                }
                if given_up() {
                    return Ok(Accepted::GivenUp);
                }
                if let Some(stream) = accepting.kept.pop_front() {
                    if wanted.claim() {
                        return Ok(Accepted::Conn(stream));
                    }
                    accepting.kept.push_front(stream);
                    return Ok(Accepted::GivenUp);
                }
                if accepting.current.is_none() {
                    break;
                }
                self.turn.wait(&mut accepting);
            }
            // (Taken under the lock that `close` takes: a close either
            // comes before, and the socket is gone, or after, and wakes
            // this accept.)
            let Some(taken) = self.socket() else {
                return Ok(Accepted::Closed);
            };
            socket = taken;
            accepting.current = Some(Current {
                op: wanted.id(),
                stopped: stopped.clone(),
            });
        }
        let result = loop {
            match socket.accept() {
                Ok((stream, peer)) => {
                    // Whoever wakes an accept holds this lock until the
                    // address of its connection is noted.
                    let mut accepting = self.accepting.lock();
                    let wake = accepting.wakes.iter().position(|addr| *addr == peer);
                    // A client that came as the listener closed is
                    // refused: its connection is closed.
                    if self.is_closed() {
                        if let Some(wake) = wake {
                            accepting.wakes.swap_remove(wake);
                        }
                        break Ok(None);
                    }
                    let Some(wake) = wake else {
                        if given_up() || foreign(self) || !wanted.claim() {
                            // A client's, and this accept is not to
                            // have it: it is the next accept's.
                            accepting.kept.push_back(stream);
                            break Ok(None);
                        }
                        break Ok(Some(stream));
                    };
                    accepting.wakes.swap_remove(wake);
                    if given_up() {
                        break Ok(None);
                    }
                    if let Some(kept) = accepting.kept.pop_front() {
                        if wanted.claim() {
                            break Ok(Some(kept));
                        }
                        accepting.kept.push_front(kept);
                        break Ok(None);
                    }
                    // Meant for an accept that had its connection
                    // already: this one goes on.
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => break Err(e),
            }
        };
        if server && matches!(result, Ok(Some(_))) {
            self.keep_the_ready(&socket);
        }
        self.accepting.lock().current = None;
        self.turn.notify_all();
        // The socket of a closed listener is closed here, if this
        // accept held it last.
        drop(socket);
        Ok(match result? {
            None if self.is_closed() => Accepted::Closed,
            Some(stream) => Accepted::Conn(stream),
            // A server took the listener while this accept waited.
            None if foreign(self) => Accepted::Served,
            None => Accepted::GivenUp,
        })
    }

    /// Accept the connections that are ready, without waiting, and
    /// keep them. Called by the accept that has the turn.
    fn keep_the_ready(&self, socket: &std::net::TcpListener) {
        /// How many at a time: the accept returns with what it has.
        const AT_ONCE: usize = 256;
        if socket.set_nonblocking(true).is_err() {
            return;
        }
        let mut taken = 0;
        while taken < AT_ONCE {
            match socket.accept() {
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
        let _ = socket.set_nonblocking(false);
    }

    /// A connection that was kept for the next accept, if there is
    /// one: for a server that has the listener to itself, which takes
    /// them where it runs.
    pub fn take_kept(&self) -> Option<std::net::TcpStream> {
        self.accepting.lock().kept.pop_front()
    }

    /// Keep a client's connection for the next accept: one that an
    /// accept had taken for a task which never received it.
    pub fn keep(&self, stream: std::net::TcpStream) {
        // (A closed listener keeps nothing: the connection is closed.)
        if self.is_closed() {
            return;
        }
        self.accepting.lock().kept.push_back(stream);
        self.turn.notify_all();
        // An accept in the OS call does not see it: it is woken as
        // one that is given up is, by a connection it drops, and
        // looks at what is kept before it calls the OS again.
        let mut accepting = self.accepting.lock();
        if accepting.current.is_some() {
            self.wake_the_call(&mut accepting);
        }
    }

    /// Wake the accept whose waiter `wanted` has gone
    /// (`Wanted::gone`): it returns [`Accepted::GivenUp`], at once if
    /// it waits its turn, and as soon as the connection made here
    /// reaches it if it is in the OS call.
    ///
    /// That connection goes to the listener's own port on the loopback
    /// address. If it cannot be made (the listener's backlog is full,
    /// a packet filter forbids loopback traffic to the port, the
    /// process has no descriptor left), the accept stays in the OS
    /// call until a client connects: its thread lingers.
    pub fn stop(&self, wanted: &Wanted) {
        let mut accepting = self.accepting.lock();
        let in_the_call = accepting
            .current
            .as_ref()
            .is_some_and(|current| current.op == wanted.id());
        if !in_the_call {
            drop(accepting);
            self.turn.notify_all();
            return;
        }
        self.wake_the_call(&mut accepting);
    }

    /// Wake the accept that is in the OS call, by a connection that it
    /// knows by its address and drops.
    fn wake_the_call(&self, accepting: &mut Accepting) {
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
        let mut addr = self.local_addr()?;
        if addr.ip().is_unspecified() {
            addr.set_ip(match addr {
                std::net::SocketAddr::V4(_) => std::net::Ipv4Addr::LOCALHOST.into(),
                std::net::SocketAddr::V6(_) => std::net::Ipv6Addr::LOCALHOST.into(),
            });
        }
        Ok(addr)
    }
}

/// The accept that is in the system's call.
struct Current {
    /// Its operation (`Wanted::id`).
    op: usize,
    /// Set when a server takes the listener from it
    /// ([`TcpListenerHandle::serve`]).
    stopped: Arc<AtomicBool>,
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

/// Why a connection's socket was not handed over
/// ([`TcpStreamHandle::hand_over`]).
#[derive(Debug)]
pub enum HandOver {
    /// The connection is closed (or was handed over before).
    Closed,
    /// It is a TLS connection already.
    Tls,
    /// The socket could not be duplicated.
    Io(std::io::Error),
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
    /// Its socket was handed over ([`TcpStreamHandle::hand_over`]).
    handed_over: AtomicBool,
    /// The OS socket that a blocked read or write uses (`SOCKET as
    /// usize` on Windows, where `CancelIoEx` on it ends what blocks on
    /// it; the file descriptor on Unix, unused).
    io_socket: Option<usize>,
    /// The OS socket that the connection's bytes come on, plain or under
    /// TLS: what a read that can be woken waits on
    /// ([`TcpStreamHandle::read_unless_woken`]). It stays open while
    /// the handle lives.
    wait_socket: Option<usize>,
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
        /// What ends a read or a write that blocks in the TLS library:
        /// it has the socket that the library reads and writes (the
        /// one `CancelIoEx` must name on Windows), and is the one its
        /// handshake was ended with.
        shutter: Shutter,
        /// Held by the task whose read is in flight: readers take turns,
        /// and a write does not wait behind them
        /// ([`TcpStreamHandle::read_tls`]).
        reading: Mutex<()>,
        /// Ends a read's wait for the socket when the connection is shut
        /// down, on Windows, where a shutdown of the socket does not end
        /// a wait for it (`TcpStreamHandle::wait_tls_socket`). `None`
        /// elsewhere, where the shutdown ends it and no descriptor is
        /// needed, and where the system gave no descriptor for it: a read
        /// on Windows then waits in the TLS library, under the lock of
        /// `both`.
        shut: Option<ReadWake>,
    },
}

impl TcpStreamHandle {
    /// A plain TCP connection.
    pub fn plain(id: usize, socket: std::net::TcpStream) -> Arc<Self> {
        Arc::new(TcpStreamHandle {
            id,
            io_socket: socket.raw_socket(),
            wait_socket: socket.raw_socket(),
            io: TcpIo::Plain {
                socket,
                reading: Mutex::new(()),
                writing: Mutex::new(()),
                switching: Mutex::new(()),
            },
            closed: AtomicBool::new(false),
            handed_over: AtomicBool::new(false),
        })
    }

    /// A connection that is one object (TLS) over `socket`, a handle
    /// of the socket that `wrap` hands to that object; `shutter` was
    /// set with that socket ([`Shutter::set`]).
    pub fn whole(
        id: usize,
        socket: &std::net::TcpStream,
        shutter: &Shutter,
        wrap: impl FnOnce() -> Result<Box<dyn ReadWrite>, String>,
    ) -> Result<Arc<Self>, String> {
        let other = Mutex::new(socket.try_clone().ok());
        let both = wrap()?;
        let wait_socket = both.raw_socket();
        Ok(Arc::new(TcpStreamHandle {
            id,
            io: TcpIo::Tls {
                both: Mutex::new(both),
                socket: other,
                shutter: shutter.clone(),
                reading: Mutex::new(()),
                // (A descriptor of its own for the life of the connection
                // took one more of each TLS connection, two on macOS, where
                // the limit is 256 by default.)
                shut: match cfg!(windows) {
                    true => ReadWake::new().ok(),
                    false => None,
                },
            },
            closed: AtomicBool::new(false),
            handed_over: AtomicBool::new(false),
            // The shutter ends what blocks.
            io_socket: None,
            wait_socket,
        }))
    }

    /// The socket of a plain connection, handed over to what speaks
    /// over it from now on (`tcp.tls_server`). The connection is closed
    /// for every other use: it counts as closed, a read or a write of
    /// it that has not begun gives `NotConnected`, and
    /// [`TcpStreamHandle::shut_down`] leaves the socket alone.
    pub fn hand_over(&self) -> Result<std::net::TcpStream, HandOver> {
        if self.is_closed() {
            return Err(HandOver::Closed);
        }
        let TcpIo::Plain { socket, .. } = &self.io else {
            return Err(HandOver::Tls);
        };
        let socket = socket.try_clone().map_err(HandOver::Io)?;
        if self.closed.swap(true, AtomicOrdering::SeqCst) {
            return Err(HandOver::Closed);
        }
        self.handed_over.store(true, AtomicOrdering::SeqCst);
        Ok(socket)
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

    /// The address of this end: where the other end reached it.
    pub fn local_addr(&self) -> std::io::Result<std::net::SocketAddr> {
        self.with_socket(|socket| socket.local_addr())
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
                if self.handed_over.load(AtomicOrdering::SeqCst) {
                    return Err(std::io::ErrorKind::NotConnected.into());
                }
                loop {
                    match (&*socket).read(buf) {
                        Err(e) if switched_meanwhile(&e) => drop(switching.lock()),
                        read => return read,
                    }
                }
            }
            TcpIo::Tls { .. } => self.read_tls(buf),
        }
    }

    /// A read of a TLS connection. What is decrypted already is read at
    /// once; else the read waits for the socket outside the connection's
    /// lock, so that a write on the connection (another task's) does not
    /// wait behind it, and then takes the socket's bytes into the TLS
    /// library under the lock, one read at a time. Two readers take
    /// turns. A shutdown ends the wait (`NotConnected`). Without a socket
    /// to wait on, or on Windows a wake for the shutdown, the read waits
    /// in the TLS library, under the lock.
    fn read_tls(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        use std::io::Read;
        let TcpIo::Tls {
            both,
            reading,
            shut,
            ..
        } = &self.io
        else {
            return Err(std::io::ErrorKind::Unsupported.into());
        };
        let Some(raw) = self.wait_socket else {
            return both.lock().read(buf);
        };
        if (cfg!(windows) && shut.is_none()) || !both.lock().reads_in_parts() {
            return both.lock().read(buf);
        }
        let _turn = reading.lock();
        loop {
            if let Some(read) = both.lock().read_buffered(buf)? {
                return Ok(read);
            }
            if self.is_closed() || !self.wait_tls_socket(raw, shut.as_ref()) {
                return Err(std::io::ErrorKind::NotConnected.into());
            }
            both.lock().read_socket()?;
        }
    }

    /// Wait until the socket `raw` of a TLS connection has something to
    /// read, its end or an error: `false` when the connection was shut
    /// down meanwhile. On Unix the wait is on the socket alone: the
    /// shutdown ([`TcpStreamHandle::shut_down`], of another handle of
    /// the same socket) ends it, as the socket's end. On Windows, where
    /// it does not, beside the connection's wake `shut`, which the
    /// shutdown raises.
    fn wait_tls_socket(&self, raw: usize, shut: Option<&ReadWake>) -> bool {
        #[cfg(windows)]
        return shut.is_some_and(|shut| shut.wait(raw));
        #[cfg(unix)]
        {
            let _ = shut;
            wait_readable(raw);
            !self.is_closed()
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (raw, shut);
            !self.is_closed()
        }
    }

    /// [`TcpStreamHandle::read`], which first waits for something to
    /// read beside `wake`: `Ok(None)` when it is woken
    /// ([`ReadWake::wake`]) instead. Nothing is taken from the connection
    /// then, and the connection is as it was. On a TLS connection what is
    /// decrypted already is read at once; else the wait for the socket is
    /// made outside the connection's lock, so a write on the connection
    /// does not wait behind it, and the socket's bytes are then taken
    /// into the TLS library under the lock, one read at a time.
    pub fn read_unless_woken(
        &self,
        buf: &mut [u8],
        wake: &ReadWake,
    ) -> std::io::Result<Option<usize>> {
        use std::io::Read;
        match &self.io {
            TcpIo::Plain {
                socket,
                reading,
                switching,
                ..
            } => {
                let _turn = reading.lock();
                if self.handed_over.load(AtomicOrdering::SeqCst) {
                    return Err(std::io::ErrorKind::NotConnected.into());
                }
                if let Some(raw) = self.wait_socket
                    && !wake.wait(raw)
                {
                    return Ok(None);
                }
                loop {
                    match (&*socket).read(buf) {
                        Err(e) if switched_meanwhile(&e) => drop(switching.lock()),
                        read => return read.map(Some),
                    }
                }
            }
            TcpIo::Tls { both, .. } if !both.lock().reads_in_parts() => {
                both.lock().read(buf).map(Some)
            }
            TcpIo::Tls { both, .. } => loop {
                if let Some(read) = both.lock().read_buffered(buf)? {
                    return Ok(Some(read));
                }
                if self.is_closed() {
                    return Err(std::io::ErrorKind::NotConnected.into());
                }
                if let Some(raw) = self.wait_socket
                    && !wake.wait(raw)
                {
                    return Ok(None);
                }
                both.lock().read_socket()?;
            },
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
                if self.handed_over.load(AtomicOrdering::SeqCst) {
                    return Err(std::io::ErrorKind::NotConnected.into());
                }
                Read::take(socket, most).read_to_end(&mut buf)?;
            }
            TcpIo::Tls { .. } => {
                // (In reads of the connection, which do not hold its lock
                // while they wait.)
                let mut chunk = vec![0u8; n.min(16 * 1024)];
                while buf.len() < n {
                    let want = (n - buf.len()).min(chunk.len());
                    match self.read_tls(&mut chunk[..want]) {
                        Ok(0) => break,
                        Ok(got) => buf.extend_from_slice(&chunk[..got]),
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(e) => return Err(e),
                    }
                }
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
                if self.handed_over.load(AtomicOrdering::SeqCst) {
                    return Err(std::io::ErrorKind::NotConnected.into());
                }
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

    /// Whether [`TcpStreamHandle::read_now`] can read anything: a plain
    /// connection.
    pub fn reads_now(&self) -> bool {
        matches!(self.io, TcpIo::Plain { .. })
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
            TcpIo::Tls {
                socket,
                shutter,
                shut,
                ..
            } => {
                // A read that waits for the socket ends.
                if let Some(shut) = shut {
                    shut.wake();
                }
                // (What the TLS writer still buffers is not flushed: a
                // write flushes before it returns, so bytes are left only
                // after a write that failed, and a flush of them is a
                // write that blocks while the peer reads nothing, with no
                // bound: a stop, and the end of a program that waits for
                // it, would wait with it.)
                #[allow(unused_mut)]
                let mut other = socket.lock();
                if let Some(other) = other.as_ref() {
                    let _ = other.shutdown(std::net::Shutdown::Both);
                }
                // On Windows the duplicate is closed too, and the call
                // that blocks on the socket of the TLS library is
                // cancelled (the shutter).
                #[cfg(windows)]
                let _ = other.take();
                drop(other);
                let _ = shutter.shut();
            }
        }
        cancel_blocked_io(self.io_socket);
    }
}

/// End a `recv` or a `send` that blocks on the OS socket `socket` (as
/// [`ReadWrite::raw_socket`] gives it), after a shutdown of the socket.
/// Winsock's `shutdown(SD_BOTH)` does not end such a call, also not
/// one on a duplicate of the socket; `CancelIoEx` on the SOCKET that
/// the blocked call uses does. Elsewhere the shutdown ends it, and this
/// does nothing.
fn cancel_blocked_io(socket: Option<usize>) {
    #[cfg(windows)]
    if let Some(sock) = socket {
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
    let _ = socket;
}

/// What shuts down the socket of an operation that has no connection
/// handle yet, from another thread: a TLS handshake, in which the
/// socket is inside the TLS library. A handshake that nobody waits for
/// any more is ended so (`Leave::Closes`), wherever it stood: the
/// socket may not be there yet (the connect is still under way), and is
/// then shut down as soon as it is.
#[derive(Clone, Default)]
pub struct Shutter(Arc<Mutex<Shut>>);

#[derive(Default)]
struct Shut {
    /// A handle of the socket, and the OS socket that the blocked call
    /// uses ([`cancel_blocked_io`]).
    socket: Option<(std::net::TcpStream, Option<usize>)>,
    /// The shutter was shut.
    shut: bool,
    /// The socket's connect is under way ([`Shutter::connecting`]).
    connecting: bool,
}

impl Shutter {
    /// The operation's socket is `socket`, the one its reads and
    /// writes block on (before it is handed to the TLS library).
    /// `false` if the shutter was shut already: the socket is shut down
    /// here then, and the operation is to give up.
    pub fn set(&self, socket: &std::net::TcpStream) -> bool {
        let used = socket.raw_socket();
        let mut shut = self.0.lock();
        if shut.shut {
            shut_socket(socket, used);
            return false;
        }
        if let Ok(handle) = socket.try_clone() {
            shut.socket = Some((handle, used));
        }
        true
    }

    /// [`Shutter::set`], for a socket whose connect is about to begin:
    /// until [`Shutter::connected`], a shut does not count as having
    /// ended the operation. Whether a shutdown ends a connect that waits
    /// for its peer depends on the system (Linux: it does), so the
    /// operation counts among those that cannot be interrupted until its
    /// connect has returned.
    pub fn connecting(&self, socket: &std::net::TcpStream) -> bool {
        let set = self.set(socket);
        self.0.lock().connecting = set;
        set
    }

    /// Whether a connect begun at [`Shutter::connecting`] is under way.
    pub fn is_connecting(&self) -> bool {
        self.0.lock().connecting
    }

    /// The connect begun at [`Shutter::connecting`] has returned.
    pub fn connected(&self) {
        self.0.lock().connecting = false;
    }

    /// Whether the shutter was shut: the operation is to give up.
    pub fn is_shut(&self) -> bool {
        self.0.lock().shut
    }

    /// Shut the operation's socket down, now or as soon as it has one:
    /// a read or a write that blocks on it returns. `false` if it has
    /// none yet (and none was shut before), or if its connect is under
    /// way ([`Shutter::connecting`]): the operation may run on then.
    pub fn shut(&self) -> bool {
        let mut shut = self.0.lock();
        let before = std::mem::replace(&mut shut.shut, true);
        match shut.socket.take() {
            Some((socket, used)) => {
                shut_socket(&socket, used);
                !shut.connecting
            }
            None => before && !shut.connecting,
        }
    }
}

/// What wakes a read that waits for a connection to have something to
/// read, without shutting the connection down
/// ([`TcpStreamHandle::read_unless_woken`]): the read waits on the
/// connection and on a descriptor of the wake's own, and a wake makes
/// that descriptor readable. A `stream.tcp_*` source has one: when its
/// stream is let go of while the connection still has other holders,
/// its read in flight ends so, and the connection stays theirs.
///
/// The descriptor is an `eventfd` on Linux (one descriptor), a pipe on
/// the other Unix systems (two), and on Windows, where a wait on a
/// socket can only wait on sockets beside it, a UDP socket on the
/// loopback address that a datagram from itself wakes. Nobody else can
/// end a wait: the flag decides, the descriptor only wakes.
pub struct ReadWake {
    woken: AtomicBool,
    signal: Signal,
}

#[cfg(any(target_os = "linux", target_os = "android"))]
struct Signal(std::os::fd::OwnedFd);

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
struct Signal {
    read: std::os::fd::OwnedFd,
    write: std::os::fd::OwnedFd,
}

#[cfg(windows)]
struct Signal {
    socket: std::net::UdpSocket,
    at: std::net::SocketAddr,
}

#[cfg(not(any(unix, windows)))]
struct Signal;

impl ReadWake {
    /// A wake: an error when the system gives no descriptor for it
    /// (none left), which the caller reports.
    pub fn new() -> std::io::Result<ReadWake> {
        Ok(ReadWake {
            woken: AtomicBool::new(false),
            signal: Signal::new()?,
        })
    }

    /// End the wait of the read, now or when it begins: from now on it
    /// reads nothing.
    pub fn wake(&self) {
        self.woken.store(true, AtomicOrdering::SeqCst);
        self.signal.raise();
    }

    fn is_woken(&self) -> bool {
        self.woken.load(AtomicOrdering::SeqCst)
    }

    /// Wait until the socket `conn` (as [`ReadWrite::raw_socket`] gives
    /// it) has something to read, its end or an error: `true`; `false`
    /// when woken.
    fn wait(&self, conn: usize) -> bool {
        loop {
            if self.is_woken() {
                return false;
            }
            match poll_two(conn, &self.signal) {
                Ok(conn_ready) => {
                    self.signal.drain();
                    if self.is_woken() {
                        return false;
                    }
                    if conn_ready {
                        return true;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                // The read itself says what is wrong.
                Err(_) => return true,
            }
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
impl Signal {
    fn new() -> std::io::Result<Signal> {
        use std::os::fd::FromRawFd;
        // SAFETY: `eventfd` makes a descriptor or fails; a descriptor it
        // makes is owned here alone.
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: as above: `fd` is open and nobody else owns it.
        Ok(Signal(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) }))
    }

    fn raise(&self) {
        use std::os::fd::AsRawFd;
        let one: u64 = 1;
        // SAFETY: writes the 8 bytes of `one` to a descriptor this owns.
        unsafe {
            libc::write(self.0.as_raw_fd(), (&one as *const u64).cast(), 8);
        }
    }

    fn fd(&self) -> libc::c_int {
        use std::os::fd::AsRawFd;
        self.0.as_raw_fd()
    }

    /// (Only the wake writes to it.)
    fn drain(&self) {}
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
impl Signal {
    fn new() -> std::io::Result<Signal> {
        use std::os::fd::FromRawFd;
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: `pipe` fills the two descriptors or fails.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: both are open and owned here alone.
        let (read, write) = unsafe {
            (
                std::os::fd::OwnedFd::from_raw_fd(fds[0]),
                std::os::fd::OwnedFd::from_raw_fd(fds[1]),
            )
        };
        for fd in fds {
            // SAFETY: `fcntl` on descriptors this owns: close on exec, and
            // a write to a full pipe does not wait.
            unsafe {
                libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
                let flags = libc::fcntl(fd, libc::F_GETFL);
                libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
        }
        Ok(Signal { read, write })
    }

    fn raise(&self) {
        use std::os::fd::AsRawFd;
        let byte = 1u8;
        // SAFETY: writes one byte to a descriptor this owns.
        unsafe {
            libc::write(self.write.as_raw_fd(), (&byte as *const u8).cast(), 1);
        }
    }

    fn fd(&self) -> libc::c_int {
        use std::os::fd::AsRawFd;
        self.read.as_raw_fd()
    }

    /// (Only the wake writes to it.)
    fn drain(&self) {}
}

#[cfg(windows)]
impl Signal {
    fn new() -> std::io::Result<Signal> {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0")?;
        let at = socket.local_addr()?;
        socket.set_nonblocking(true)?;
        Ok(Signal { socket, at })
    }

    fn raise(&self) {
        let _ = self.socket.send_to(&[0], self.at);
    }

    /// Take what came to the socket: a datagram of another process to
    /// the port ends no wait, and must not leave the socket readable.
    fn drain(&self) {
        let mut byte = [0u8; 1];
        while self.socket.recv(&mut byte).is_ok() {}
    }
}

#[cfg(not(any(unix, windows)))]
impl Signal {
    fn new() -> std::io::Result<Signal> {
        Ok(Signal)
    }

    fn raise(&self) {}

    fn drain(&self) {}
}

/// Wait until the socket `conn` or the wake's descriptor has something
/// to read (or its end, or an error): whether `conn` does. (The wake's
/// descriptor is never drained: once raised, every later wait returns
/// at once, and the flag of the wake ends it.)
#[cfg(unix)]
fn poll_two(conn: usize, wake: &Signal) -> std::io::Result<bool> {
    let mut fds = [
        libc::pollfd {
            fd: conn as libc::c_int,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: wake.fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    // SAFETY: `fds` is an array of two valid `pollfd`s of descriptors
    // that the callers keep open for the call; its length is passed.
    let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
    if n < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(fds[0].revents != 0)
}

/// Wait until the socket `conn` has something to read, its end, or an
/// error (which the read then says).
#[cfg(unix)]
fn wait_readable(conn: usize) {
    let mut fds = [libc::pollfd {
        fd: conn as libc::c_int,
        events: libc::POLLIN,
        revents: 0,
    }];
    loop {
        // SAFETY: `fds` is one valid `pollfd` of a socket that the
        // caller keeps open for the call.
        let n = unsafe { libc::poll(fds.as_mut_ptr(), 1, -1) };
        if n >= 0 || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return;
        }
    }
}

#[cfg(windows)]
fn poll_two(conn: usize, wake: &Signal) -> std::io::Result<bool> {
    use std::os::windows::io::AsRawSocket;
    use windows_sys::Win32::Networking::WinSock::{POLLRDNORM, SOCKET_ERROR, WSAPOLLFD, WSAPoll};
    let mut fds = [
        WSAPOLLFD {
            fd: conn as _,
            events: POLLRDNORM,
            revents: 0,
        },
        WSAPOLLFD {
            fd: wake.socket.as_raw_socket() as _,
            events: POLLRDNORM,
            revents: 0,
        },
    ];
    // SAFETY: `fds` is an array of two valid `WSAPOLLFD`s of sockets
    // that the callers keep open for the call; its length is passed.
    let n = unsafe { WSAPoll(fds.as_mut_ptr(), 2, -1) };
    if n == SOCKET_ERROR {
        return Err(std::io::Error::last_os_error());
    }
    Ok(fds[0].revents != 0)
}

#[cfg(not(any(unix, windows)))]
fn poll_two(_conn: usize, _wake: &Signal) -> std::io::Result<bool> {
    Ok(true)
}

/// Shut `socket` down, and end what blocks on `used`.
fn shut_socket(socket: &std::net::TcpStream, used: Option<usize>) {
    let _ = socket.shutdown(std::net::Shutdown::Both);
    cancel_blocked_io(used);
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

/// How a task ended.
struct Ended(Result<Value, VmError>);

impl Drop for Ended {
    fn drop(&mut self) {
        // On no native stack, however deep the value nests: a task may
        // give the handle of a task that gave the handle of a task.
        if let Ok(value) = &mut self.0 {
            crate::value::drop_held(std::mem::replace(value, Value::Unit));
        }
    }
}

/// The stream stage a task runs: its name (`stream.map`), for the
/// report of a failure that nobody read, whether it is a source (a stage
/// with no input), the place of the call that started it, if it has
/// one in the program, and the ids of its own channels, its inputs and
/// its output, which the deadlock report tells apart from a channel
/// that the stage's function waits on.
#[derive(Clone, Debug)]
pub struct StageOf {
    pub name: &'static str,
    pub source: bool,
    pub at: Option<Span>,
    pub inputs: Arc<[usize]>,
    pub output: usize,
    /// It only passes on what another task of the same call sends (the
    /// function of `stream.from_fn`): the deadlock report names that one.
    pub(crate) relays: bool,
}

/// Handle to a task the runtime started. Thread-safe — shared between the
/// call that started it and the worker that runs it.
pub struct TaskHandle {
    pub id: usize,
    /// The task's result, set once: what it returned, the error it
    /// failed with, or that it was cancelled. Its call waits for it.
    result: Arc<Cell<Ended>>,
    /// Set by a cancel (`Scheduler::cancel`, a stop of the tasks of a
    /// call, a stream stage that nobody reads any more). The scheduler reads it before each slice
    /// of the task and before each park, and the task's VM before each
    /// operation that takes (`Vm::takes_nothing_new`): a cancelled
    /// task begins no wait, has none completed for it and takes
    /// nothing new, and runs on only for the steps that
    /// [`TaskHandle::count_late`] counts.
    cancelled: Arc<AtomicBool>,
    /// True while the task has failed and nothing has read the failure:
    /// no reader of a stage's output has received it, no call that
    /// waits for the task has taken it, and no cancel has dismissed it.
    /// Read when the program ends, for the report of failures that
    /// nothing read.
    unread_failure: AtomicBool,
    /// Set when the code of the task's owner has ended with the failure
    /// unhandled ([`TaskHandle::keep_failure`]): a cancel no longer
    /// dismisses it.
    kept: AtomicBool,
    /// Set when every reader of the output of the task's stage let it
    /// go while the code of its owner still ran ([`TaskHandle::let_go`]):
    /// a failure of the task is dismissed, whenever it comes.
    let_go: AtomicBool,
    /// Who the task belongs to (`Vm::set_task_owner`): the
    /// program, or one test of a test run.
    owner: u64,
    /// When the task is over, as a reading of the clock's monotonic
    /// time: the task of a function that `task.deadline` runs has one.
    /// The scheduler puts it on every wait of the task
    /// (`runtime::sync::Wait::task_deadline`) and ends the task when
    /// one of them ends by it, or when the task has run on past it for
    /// the steps a late task has ([`TaskHandle::count_late`]).
    deadline: Option<std::time::Duration>,
    /// Set when the task was ended by its deadline: that is its
    /// result then ([`TaskHandle::expire`]).
    expired: AtomicBool,
    /// Set when the time of the task is over, as the scheduler finds
    /// it at the start of a slice ([`TaskHandle::set_over`]): from
    /// then on the task takes nothing new. The task's VM reads it at
    /// every operation that takes, in place of the clock.
    over: Arc<AtomicBool>,
    /// The steps run past the deadline, or after a cancel, counted by
    /// the scheduler: by this task, and by every task of a deadline
    /// inside it that has the same time (they share the count: of them
    /// only one runs at a time, the innermost).
    late: Arc<std::sync::atomic::AtomicUsize>,
    /// The stream stage the task runs, if it runs one.
    stage: Option<StageOf>,
    /// Set when the stage failed with the failure of an earlier stage,
    /// read from its input ([`TaskHandle::fails_with_its_input`]).
    failed_with_input: AtomicBool,
    /// Where the task was started (the call of `task.join`, of
    /// `task.background`, ...), when that is a place in the program:
    /// the deadlock report names it.
    started: Option<Span>,
    /// Completed when the task has ended: its last step has run and its
    /// frames are gone (`Scheduler::end_task`). Later than its result
    /// when the task was cancelled: a stop waits for it.
    stopped: Arc<Cell<()>>,
    /// The task's place among the tasks that spawned it and that it
    /// spawned, for a cancel to reach them (`runtime::family`).
    family: super::family::Member,
    /// The `task.join` or `task.background` the task is one of, and
    /// what it is there: its failure and its end are told to it.
    together: Option<(Arc<super::together::Together>, super::together::Role)>,
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
            unread_failure: AtomicBool::new(false),
            kept: AtomicBool::new(false),
            let_go: AtomicBool::new(false),
            owner,
            deadline: None,
            expired: AtomicBool::new(false),
            over: Arc::default(),
            late: Arc::default(),
            stage: None,
            failed_with_input: AtomicBool::new(false),
            started: None,
            stopped: Cell::new(),
            family: Default::default(),
            together: None,
        }
    }

    /// The handle, for a task that is `role` in the call of `together`.
    pub(crate) fn in_together(
        self,
        together: Arc<super::together::Together>,
        role: super::together::Role,
    ) -> Self {
        Self {
            together: Some((together, role)),
            ..self
        }
    }

    /// The `task.join` or `task.background` the task is one of, and
    /// what it is there.
    pub(crate) fn together(
        &self,
    ) -> Option<(&Arc<super::together::Together>, super::together::Role)> {
        self.together
            .as_ref()
            .map(|(together, role)| (together, *role))
    }

    /// The handle, for a task that the call `call` (`task.deadline`)
    /// waits for: a wait for its end names the call (the deadlock
    /// report of the program's thread).
    pub(crate) fn for_call(self, call: &'static str) -> Self {
        Self {
            result: Cell::of_call(format!("task <handle:{}>", self.id), call),
            ..self
        }
    }

    /// The handle, for a task started by the call at `at` (none: by
    /// the runtime, or a front end).
    pub fn started_at(self, at: Option<Span>) -> Self {
        Self {
            started: at,
            ..self
        }
    }

    /// Where the task was started, if that is a place in the program.
    pub fn start_site(&self) -> Option<Span> {
        self.started
    }

    /// The handle, for the task of the stream stage `name`, started by
    /// the call at `at`.
    pub fn of_stage(self, stage: StageOf) -> Self {
        Self {
            stage: Some(stage),
            ..self
        }
    }

    /// The name of the stream stage the task runs, if it runs one.
    pub fn stage(&self) -> Option<&'static str> {
        self.stage.as_ref().map(|stage| stage.name)
    }

    /// The stream stage the task runs, if it runs one.
    pub fn stage_of(&self) -> Option<&StageOf> {
        self.stage.as_ref()
    }

    /// The stage fails with the failure it read from its input, which
    /// an earlier stage raised.
    pub fn fails_with_its_input(&self) {
        self.failed_with_input.store(true, AtomicOrdering::SeqCst);
    }

    /// The place of the call that started the stage, if it failed with
    /// the failure of an earlier stage: its report shows that call, the
    /// stage it names, not where the earlier stage failed.
    pub fn stage_call_of_a_failed_input(&self) -> Option<Span> {
        let stage = self.stage.as_ref()?;
        self.failed_with_input
            .load(AtomicOrdering::SeqCst)
            .then_some(stage.at?)
    }

    /// The handle, for a task that is over at `deadline`. `late` is the
    /// count of the steps run past that time, if the task is to go on
    /// with the count of another task that has the same time (the one
    /// whose `task.deadline` it is the function of).
    pub fn with_deadline(
        self,
        deadline: std::time::Duration,
        late: Option<Arc<std::sync::atomic::AtomicUsize>>,
    ) -> Self {
        Self {
            deadline: Some(deadline),
            late: late.unwrap_or_default(),
            ..self
        }
    }

    /// The count of the steps run past the deadline
    /// ([`TaskHandle::count_late`]), to be shared.
    pub fn late(&self) -> Arc<std::sync::atomic::AtomicUsize> {
        self.late.clone()
    }

    /// The task has run `steps` steps past its deadline, or after it
    /// was cancelled: how many that makes. It is one count for both.
    pub fn count_late(&self, steps: usize) -> usize {
        self.late.fetch_add(steps, AtomicOrdering::SeqCst) + steps
    }

    /// When the task is over, if it has such a time.
    pub fn deadline(&self) -> Option<std::time::Duration> {
        self.deadline
    }

    /// The time of the task is over: a slice of it begins past its
    /// deadline. It is never taken back.
    pub fn set_over(&self) {
        self.over.store(true, AtomicOrdering::SeqCst);
    }

    /// Whether [`TaskHandle::set_over`] was called.
    pub fn is_over(&self) -> bool {
        self.over.load(AtomicOrdering::SeqCst)
    }

    /// The flag that says so, for the task's VM
    /// (`Vm::takes_nothing_new`).
    pub fn over_flag(&self) -> Arc<AtomicBool> {
        self.over.clone()
    }

    /// The task is ended by its deadline: that is its result, unless
    /// it had one (it was cancelled). Nobody but the scheduler, which
    /// has the task in hand, ends a task so; it is no failure.
    pub fn expire(&self, wake: &dyn Wake) {
        // Before whoever waits for the task is woken.
        self.expired.store(true, AtomicOrdering::SeqCst);
        let _ = self.result.complete(
            Ended(Err(VmError::new("ended by its deadline".to_string()))),
            wake,
        );
    }

    /// Whether the task was ended by its deadline.
    pub fn is_expired(&self) -> bool {
        self.expired.load(AtomicOrdering::SeqCst)
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
        self.result.complete(Ended(result), wake).is_ok()
    }

    /// The task failed with `error`. `true` when that is the handle's
    /// result now and nothing has dismissed it: the failure is then
    /// unread until a reader receives it or a cancel dismisses it.
    pub fn fail(&self, error: VmError, wake: &dyn Wake) -> bool {
        if self.result.get().is_some() {
            return false;
        }
        // Before the readers are woken: one of them reads it.
        self.unread_failure.store(true, AtomicOrdering::SeqCst);
        let failed = self.result.complete(Ended(Err(error)), wake).is_ok();
        // A stage whose output its readers let go of has nobody to read
        // its failure. (Read after the store above, as `let_go` stores
        // before it clears: one of the two sees the other.)
        if self.let_go.load(AtomicOrdering::SeqCst) {
            self.unread_failure.store(false, AtomicOrdering::SeqCst);
            return false;
        }
        failed
    }

    /// Every reader of the output of the task's stage has let it go
    /// (the output is dropped), or of the output of a stage it feeds
    /// (`Channel::let_go_of_feeders`): nothing can read a failure of the
    /// task any more, and it is dismissed, the one it has had and one that
    /// comes later, unless the code of its owner has ended
    /// ([`TaskHandle::keep_failure`]). The same as the cancel of the
    /// task that the output's stop does, but at once: the stop is
    /// deferred (`scheduler::defer`), and a report that came before it
    /// ran (`Vm::run_program`'s, as `main` returns) showed the failure
    /// of a stage that the program had let go of.
    pub(crate) fn let_go(&self) {
        if !self.kept.load(AtomicOrdering::SeqCst) {
            self.let_go.store(true, AtomicOrdering::SeqCst);
            self.unread_failure.store(false, AtomicOrdering::SeqCst);
        }
    }

    /// A cancel: the task's result is the cancellation unless it
    /// had one, at once, whatever the task still does (what it returns
    /// or fails with later changes nothing). A failure it had is
    /// dismissed, unless the code of its owner has ended with it
    /// (`TaskHandle::keep_failure`). The caller ends the task's
    /// wait, if it waits (`Scheduler::cancel`).
    pub fn cancel(&self, wake: &dyn Wake) {
        self.cancelled.store(true, AtomicOrdering::SeqCst);
        self.end_by_cancel(wake);
        if !self.kept.load(AtomicOrdering::SeqCst) {
            self.unread_failure.store(false, AtomicOrdering::SeqCst);
        }
    }

    /// The code of the task's owner has ended: a failure that nobody
    /// has handled by now is reported, and a cancel from now on does
    /// not dismiss it. What cancels the task then is the end itself:
    /// the stop of a stream stage whose output a task held until the
    /// end, and let go of as it was dropped with it. That stream was
    /// never left (a stream that the program leaves is stopped while
    /// its code still runs).
    pub(crate) fn keep_failure(&self) {
        self.kept.store(true, AtomicOrdering::SeqCst);
    }

    /// Whether the task has its result.
    pub(crate) fn has_result(&self) -> bool {
        self.result.get().is_some()
    }

    /// Completed when the task has ended ([`TaskHandle::stop`]).
    pub(crate) fn stopped(&self) -> Arc<Cell<()>> {
        self.stopped.clone()
    }

    /// Whether the task has ended.
    pub(crate) fn has_stopped(&self) -> bool {
        self.stopped.get().is_some()
    }

    /// The task has ended: its last step has run and its frames are
    /// gone. Whoever waits for that is woken.
    pub(crate) fn stop(&self, wake: &dyn Wake) {
        if self.stopped.complete((), wake).is_ok()
            && let Some((together, role)) = &self.together
        {
            together.stopped(*role, wake);
        }
    }

    /// A cancel of a task that spawned this one, or of one of their
    /// spawners, reaches it while it runs: as a cancel of it, except that
    /// a failure that comes first stands and is not dismissed (a task
    /// that has ended is not cancelled so: `runtime::family`).
    pub fn cancel_spawned(&self, wake: &dyn Wake) {
        self.mark_cancelled();
        self.end_by_cancel(wake);
    }

    /// The cancellation is the task's result, unless it had one.
    fn end_by_cancel(&self, wake: &dyn Wake) {
        let cancelled = VmError {
            by_a_cancel: true,
            ..VmError::new("cancelled".to_string())
        };
        let _ = self.result.complete(Ended(Err(cancelled)), wake);
    }

    /// Whether a cancel gave the task its result
    /// ([`TaskHandle::cancel`]): it neither returned nor failed before.
    pub(crate) fn was_ended_by_cancel(&self) -> bool {
        matches!(self.result.get(), Some(Ended(Err(error))) if error.by_a_cancel)
    }

    /// The flag of [`TaskHandle::cancel`], set alone: the rest of the
    /// cancel follows ([`TaskHandle::cancel_spawned`]).
    pub(crate) fn mark_cancelled(&self) {
        self.cancelled.store(true, AtomicOrdering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(AtomicOrdering::SeqCst)
    }

    /// The task's place in the tree of who spawned whom.
    pub(crate) fn family(&self) -> &super::family::Member {
        &self.family
    }

    /// The flag that says so, for the waits of the task
    /// (`runtime::sync::Wait::cancel`).
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.cancelled.clone()
    }

    /// The result, once the task has one.
    pub fn try_get(&self) -> Option<Result<Value, VmError>> {
        self.result.get().map(|ended| ended.0.clone())
    }

    /// The failure of the task is read: the call that waited for the
    /// task took it, or the code that reads the task's output did.
    pub fn mark_read(&self) {
        self.unread_failure.store(false, AtomicOrdering::Release);
    }

    pub fn has_unread_failure(&self) -> bool {
        self.unread_failure.load(AtomicOrdering::Acquire)
    }

    /// The failure of the task, if nothing has read it; it counts as
    /// read from now on.
    pub fn take_unread_failure(&self) -> Option<VmError> {
        if !self.unread_failure.swap(false, AtomicOrdering::AcqRel) {
            return None;
        }
        match self.result.get() {
            Some(Ended(Err(error))) => Some(error.clone()),
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
        // A connection is in the listener's queue when `connect` has
        // returned.
        let clients: Vec<TcpStream> = (0..40)
            .map(|_| TcpStream::connect(addr).expect("connect"))
            .collect();
        let flag = Wanted::of_test(Vec::new());
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
                    let flag = Wanted::of_test(Vec::new());
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
        let flag = Wanted::of_test(Vec::new());
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

    /// A connection that reads in two parts, as a TLS one does, over a
    /// plain socket; it tells `entered` when a read first comes to it.
    struct Parts {
        sock: TcpStream,
        got: Vec<u8>,
        ended: bool,
        entered: Option<std::sync::mpsc::Sender<()>>,
    }

    impl Parts {
        fn enter(&mut self) {
            if let Some(entered) = self.entered.take() {
                let _ = entered.send(());
            }
        }
    }

    impl std::io::Read for Parts {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.enter();
            self.sock.read(buf)
        }
    }

    impl std::io::Write for Parts {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.sock.write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.sock.flush()
        }
    }

    impl ReadWrite for Parts {
        fn raw_socket(&self) -> Option<usize> {
            self.sock.raw_socket()
        }
        fn reads_in_parts(&self) -> bool {
            true
        }
        fn read_buffered(&mut self, buf: &mut [u8]) -> std::io::Result<Option<usize>> {
            self.enter();
            if self.got.is_empty() && !self.ended {
                return Ok(None);
            }
            let n = buf.len().min(self.got.len());
            buf[..n].copy_from_slice(&self.got[..n]);
            self.got.drain(..n);
            Ok(Some(n))
        }
        fn read_socket(&mut self) -> std::io::Result<()> {
            let mut chunk = [0u8; 64];
            let n = self.sock.read(&mut chunk)?;
            self.ended = n == 0;
            self.got.extend_from_slice(&chunk[..n]);
            Ok(())
        }
    }

    use std::io::{Read as _, Write as _};

    /// A read of a TLS connection that waits for the peer does not hold
    /// the connection's lock while it waits: a write of another thread
    /// goes out meanwhile, and the read gets the answer to it. (The read
    /// held the lock in the TLS library: the write waited behind it, here
    /// for ever, since the peer answers only the write.) A shutdown ends
    /// the read's wait.
    #[test]
    fn a_tls_read_that_waits_lets_a_write_of_the_connection_through() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let mut peer = TcpStream::connect(listener.local_addr().expect("addr")).expect("connect");
        let (sock, _) = listener.accept().expect("accept");
        let other = sock.try_clone().expect("clone");
        let shutter = Shutter::default();
        assert!(shutter.set(&sock));
        let (entered, in_the_read) = std::sync::mpsc::channel();
        let parts = Parts {
            sock,
            got: Vec::new(),
            ended: false,
            entered: Some(entered),
        };
        let conn = TcpStreamHandle::whole(0, &other, &shutter, move || {
            Ok(Box::new(parts) as Box<dyn ReadWrite>)
        })
        .expect("a connection");
        let reader = conn.clone();
        let read = std::thread::spawn(move || {
            let mut buf = [0u8; 4];
            let n = reader.read(&mut buf).expect("the read");
            buf[..n].to_vec()
        });
        in_the_read.recv().expect("the read has begun");
        let (writer, (wrote, written)) = (conn.clone(), std::sync::mpsc::channel());
        std::thread::spawn(move || {
            let _ = wrote.send(writer.write_all(b"ping").is_ok());
        });
        // (Only what fails waits for the bound.)
        let went = written.recv_timeout(std::time::Duration::from_secs(20));
        assert_eq!(went, Ok(true), "the write waited behind the read");
        let mut ping = [0u8; 4];
        peer.read_exact(&mut ping).expect("the peer reads");
        assert_eq!(&ping, b"ping");
        peer.write_all(b"pong").expect("the peer answers");
        assert_eq!(read.join().expect("joined"), b"pong");
        // A read that waits is ended by a shutdown.
        let reader = conn.clone();
        let read = std::thread::spawn(move || reader.read(&mut [0u8; 4]).is_err());
        conn.shut_down();
        assert!(read.join().expect("joined"));
    }

    /// The descriptors of the process, counted by its descriptor
    /// directory.
    #[cfg(unix)]
    fn open_descriptors() -> usize {
        std::fs::read_dir("/dev/fd")
            .expect("the descriptors")
            .count()
    }

    /// Set in the process that `a_tls_connection_holds_no_wake_descriptor`
    /// starts to count the descriptors.
    #[cfg(unix)]
    const COUNTING: &str = "SILT_TEST_COUNTS_DESCRIPTORS";

    /// A TLS connection takes one descriptor of its own beside its socket
    /// (the handle of the socket that shuts it down without its lock), no
    /// wake descriptor for its whole life: 32 connections take 32. FAILS
    /// on 0.43.0: each held an eventfd too (a pipe, two, on macOS), 64 on
    /// Linux. (32, not more: the test holds some five descriptors a
    /// connection, and macOS allows a process 256 by default.)
    ///
    /// The descriptors counted are the whole process's, and the other
    /// tests of this binary open and close theirs at the same time: the
    /// count is made in a process of this binary that runs this test
    /// alone. (Made here it came out less than the count before, or more
    /// than the connections took.)
    #[test]
    #[cfg(unix)]
    fn a_tls_connection_holds_no_wake_descriptor() {
        if std::env::var_os(COUNTING).is_some() {
            let taken = descriptors_of_tls_connections(32);
            println!("{COUNTING}: 32 connections took {taken}");
            return;
        }
        // The test's name as the test binary gives it, without the crate.
        let path = module_path!();
        let path = path.split_once("::").map_or(path, |(_, path)| path);
        let name = format!("{path}::a_tls_connection_holds_no_wake_descriptor");
        let child = std::process::Command::new(std::env::current_exe().expect("the test binary"))
            .args([name.as_str(), "--exact", "--nocapture", "--test-threads=1"])
            .env(COUNTING, "1")
            .output()
            .expect("the test binary runs");
        let printed = String::from_utf8_lossy(&child.stdout);
        let errors = String::from_utf8_lossy(&child.stderr);
        assert!(child.status.success(), "{printed}\n{errors}");
        assert!(
            printed.contains(&format!("{COUNTING}: 32 connections took")),
            "the child ran no test named {name}:\n{printed}\n{errors}"
        );
    }

    /// How many descriptors `n` TLS-shaped connections take beside their
    /// sockets; asserts it is about `n`. Run in a process alone.
    #[cfg(unix)]
    fn descriptors_of_tls_connections(n: usize) -> usize {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let mut kept = Vec::new();
        let mut parts = Vec::new();
        for _ in 0..n {
            let peer = TcpStream::connect(addr).expect("connect");
            let (sock, _) = listener.accept().expect("accept");
            let shutter = Shutter::default();
            assert!(shutter.set(&sock));
            let held = sock.try_clone().expect("clone");
            kept.push((peer, shutter.clone()));
            parts.push((sock, held, shutter));
        }
        let before = open_descriptors();
        // (Each `held` is kept to the count: dropped as its connection is
        // made, it hid the descriptor the connection took, and 0.43.0's
        // two a connection came out one.)
        let conns: Vec<_> = (parts.into_iter())
            .map(|(sock, held, shutter)| {
                let conn = TcpStreamHandle::whole(0, &held, &shutter, move || {
                    Ok(Box::new(sock) as Box<dyn ReadWrite>)
                })
                .expect("a connection");
                (conn, held)
            })
            .collect();
        let taken = open_descriptors() - before;
        drop(conns);
        drop(kept);
        assert!(
            taken <= n + n / 4,
            "{n} TLS connections took {taken} descriptors of their own"
        );
        taken
    }

    /// A connection that is one object (TLS) is shut down while a read
    /// blocks in it, on the socket that the object has: the read
    /// returns. On Windows a shutdown of another handle of the socket
    /// does not end that read; the shutter cancels it on the object's
    /// own socket.
    #[test]
    fn a_whole_connection_shut_down_ends_the_read_that_blocks_in_it() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let _peer = TcpStream::connect(listener.local_addr().expect("addr")).expect("connect");
        let (sock, _) = listener.accept().expect("accept");
        let other = sock.try_clone().expect("clone");
        let shutter = Shutter::default();
        assert!(shutter.set(&sock));
        let conn = TcpStreamHandle::whole(0, &other, &shutter, move || {
            Ok(Box::new(sock) as Box<dyn ReadWrite>)
        })
        .expect("a connection");
        let (reader, (tell, told)) = (conn.clone(), std::sync::mpsc::channel());
        let read = std::thread::spawn(move || {
            let _ = tell.send(());
            let mut buf = [0u8; 8];
            // Whatever the system says: it returns.
            let _ = reader.read(&mut buf);
        });
        told.recv().expect("the reader runs");
        std::thread::sleep(std::time::Duration::from_millis(50));
        conn.shut_down();
        let limit = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !read.is_finished() {
            assert!(std::time::Instant::now() < limit, "the read still blocks");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    /// A TLS connection whose writer still buffers bytes it cannot send
    /// (a peer that reads nothing) is shut down at once: nothing is
    /// flushed. FAIL on the base: the shutdown flushed what the writer
    /// held, a write that waited here for the flush to be let go.
    #[test]
    fn a_tls_shut_down_waits_for_no_flush() {
        struct HeldFlush {
            sock: TcpStream,
            release: std::sync::mpsc::Receiver<()>,
        }
        impl std::io::Read for HeldFlush {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                self.sock.read(buf)
            }
        }
        impl std::io::Write for HeldFlush {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                Ok(buf.len())
            }
            // As a write to a peer that reads nothing waits: until the
            // test lets it go.
            fn flush(&mut self) -> std::io::Result<()> {
                let _ = self.release.recv();
                Ok(())
            }
        }
        impl ReadWrite for HeldFlush {}
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let _peer = TcpStream::connect(listener.local_addr().expect("addr")).expect("connect");
        let (sock, _) = listener.accept().expect("accept");
        let other = sock.try_clone().expect("clone");
        let shutter = Shutter::default();
        assert!(shutter.set(&sock));
        let (let_go, release) = std::sync::mpsc::channel();
        let conn = TcpStreamHandle::whole(0, &other, &shutter, move || {
            Ok(Box::new(HeldFlush { sock, release }) as Box<dyn ReadWrite>)
        })
        .expect("a connection");
        let (done, shut) = std::sync::mpsc::channel();
        let closer = conn.clone();
        std::thread::spawn(move || {
            closer.shut_down();
            let _ = done.send(());
        });
        let returned = shut.recv_timeout(std::time::Duration::from_secs(10));
        drop(let_go);
        assert!(returned.is_ok(), "the shutdown waited for the flush");
    }

    /// A task that is cancelled after it started an accept, and
    /// before it waits for it, takes no client that connects after
    /// the cancel: the accept gives up, as one that nobody waits for
    /// does, and the client is the next accept's. (Taken by the task,
    /// the connection went to whatever the task did in the steps it
    /// had left, and was lost to the program.)
    #[test]
    fn an_accept_of_a_task_that_was_cancelled_keeps_the_client_for_the_next() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let listener = Arc::new(TcpListenerHandle::new(0, listener));
        let cancelled = Arc::new(AtomicBool::new(false));
        let give_up = Wanted::of_test(vec![cancelled.clone()]);
        let accept = {
            let (listener, give_up) = (listener.clone(), give_up.clone());
            std::thread::spawn(move || listener.accept(&give_up, false).expect("accept"))
        };
        while listener.accepting.lock().current.is_none() {
            std::thread::yield_now();
        }
        cancelled.store(true, AtomicOrdering::SeqCst);
        let client = TcpStream::connect(addr).expect("connect");
        assert!(matches!(accept.join().expect("joined"), Accepted::GivenUp));
        let kept = listener.take_kept().expect("the client is kept");
        assert_eq!(
            kept.peer_addr().expect("peer"),
            client.local_addr().expect("local")
        );
        // An accept that begins when the task is cancelled ends at
        // once.
        assert!(matches!(
            listener.accept(&give_up, false).expect("accept"),
            Accepted::GivenUp
        ));
    }
}
