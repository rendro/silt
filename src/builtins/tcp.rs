//! `tcp.*` builtin functions: TCP listeners and streams.
//!
//! The operations that block (`accept`, `connect`, `read`,
//! `read_exact`, `write`) run on the I/O pool and the task waits for
//! their value (`Vm::io`), so the scheduler runs other tasks
//! meanwhile.
//!
//! An operation on a connection or a listener can be made to return
//! when its task stops waiting for it (cancelled, timed out, dropped
//! at the end of the program): the connection is shut down; the accept
//! is woken by a connection of the listener to itself. Its thread then
//! ends instead of blocking for as long as the peer stays silent.
//!
//! A plain connection is read and written at the same time: a task
//! that reads and a task that writes do not wait for each other; a
//! TLS connection is one object behind one lock
//! ([`TcpStreamHandle`]).

use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

use super::common::{READ_AT_ONCE, ok};
use super::typed::{self, Bytes, builtins};
use crate::runtime::handle::{Accepted, TcpListenerHandle, TcpStreamHandle};
use crate::typeinfo::bv;
use crate::value::Value;
use crate::vm::{Leave, Step, Vm, VmError};

/// The error of a `tcp` function whose operation has no value of its
/// own: `Timeout` when its deadline passed, `Other` with the
/// reason when it could not run or panicked.
pub(crate) fn tcp_timeout_err(failure: crate::vm::IoFailure<'_>) -> Value {
    use crate::vm::IoFailure;
    let error = match failure {
        IoFailure::Timeout(_) => Value::variant(bv::TCP_TIMEOUT, vec![]),
        IoFailure::Panicked(why) | IoFailure::Refused(why) => {
            Value::variant(bv::TCP_OTHER, vec![Value::String(why.into())])
        }
    };
    Value::variant(bv::ERR, vec![error])
}

/// What `TcpError`'s `message` says of the variant `tag` with `fields`:
/// `None` if they are no variant of it.
pub(crate) fn error_text(tag: &str, fields: &[Value]) -> Option<String> {
    Some(match (tag, fields) {
        ("Connect", [Value::String(m)]) => format!("tcp connect failed: {m}"),
        ("Bind", [Value::String(m)]) => format!("tcp listen failed: {m}"),
        ("Tls", [Value::String(m)]) => format!("tcp TLS error: {m}"),
        ("Closed", []) => "tcp connection closed".to_string(),
        ("Timeout", []) => "tcp operation timed out".to_string(),
        ("Other", [Value::String(m)]) => m.to_string(),
        _ => return None,
    })
}

#[cfg(feature = "tcp-tls")]
pub(crate) use tls::{connect_tls, tls_server, tls_server_mtls};

#[cfg(feature = "tcp-tls")]
mod tls {
    //! TLS extension for the tcp module — gated by the `tcp-tls` feature.
    //!
    //! `connect_tls`, `tls_server` and `tls_server_mtls` return regular
    //! `Value::TcpStream` handles. The `Box<dyn ReadWrite>` inside the handle now carries a
    //! `rustls::StreamOwned<...>` instead of a bare `TcpStream`; from the
    //! caller's perspective `tcp.read` / `tcp.write` / `tcp.close` work
    //! identically. This is the payoff of PR 2's trait-object design.

    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::Arc;

    use rustls::pki_types::pem::{self, PemObject};
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
    use rustls::server::WebPkiClientVerifier;
    use rustls::{ClientConfig, ClientConnection, RootCertStore, ServerConfig, ServerConnection};

    use crate::typeinfo::bv;

    use super::{
        Doing, Leave, Step, TcpStreamHandle, Value, Vm, VmError, err_closed, tcp_io_err,
        tcp_timeout_err,
    };
    use crate::builtins::typed::{self, Bytes, builtins};
    use crate::runtime::handle::{HandOver, ReadWrite, Shutter};

    builtins! {
        /// Opens a TCP connection then performs the TLS client handshake
        /// using `webpki-roots` for trust anchors. The returned stream
        /// wraps a `rustls::StreamOwned<ClientConnection, TcpStream>`
        /// behind the same `TcpStreamHandle` as plain TCP.
        ///
        /// A connect that nobody waits for any more shuts its socket
        /// down, as a handshake of `tls_server` does: a server that
        /// accepts and never answers holds no thread.
        fn connect_tls(vm, addr: &str, hostname: &str) -> Result<Step, VmError> {
            let (addr, hostname) = (addr.to_string(), hostname.to_string());
            let next_id = vm.next_tcp_id();
            let shutter = Shutter::default();
            let shut = shutter.clone();
            vm.io("tcp", tcp_timeout_err, Leave::closes(move || shut.shut()), move |_| {
                match do_connect_tls(&addr, &hostname, next_id, &shutter) {
                    Ok(handle) => Value::variant(bv::OK, vec![Value::TcpStream(handle)]),
                    Err(NoTls::Connection(e)) => tcp_io_err(Doing::Connecting, &e),
                    Err(NoTls::Tls(e)) => tls_err(e),
                }
            })
        }

        /// Runs the TLS server handshake on `conn` with the PEM-encoded
        /// cert chain and private key; the stream it returns wraps a
        /// `rustls::StreamOwned<ServerConnection, TcpStream>`. `conn` is
        /// taken ([`TcpStreamHandle::hand_over`]).
        fn tls_server(
            vm,
            conn: typed::TcpStream,
            cert_pem: Bytes,
            key_pem: Bytes,
        ) -> Result<Step, VmError> {
            let (cert_pem, key_pem) = (cert_pem.clone(), key_pem.clone());
            handshake(vm, conn, move || {
                ServerConfig::builder()
                    .with_no_client_auth()
                    .with_single_cert(parse_cert_chain(&cert_pem)?, parse_private_key(&key_pem)?)
                    .map_err(|e| format!("server config: {e}"))
            })
        }

        /// Like `tls_server`, but the client must present a certificate
        /// chaining to one of the CAs in `client_ca_pem`
        /// (`rustls::server::WebPkiClientVerifier`): without one, or with
        /// one that does not chain, the handshake fails.
        fn tls_server_mtls(
            vm,
            conn: typed::TcpStream,
            cert_pem: Bytes,
            key_pem: Bytes,
            client_ca_pem: Bytes,
        ) -> Result<Step, VmError> {
            let (cert_pem, key_pem) = (cert_pem.clone(), key_pem.clone());
            let client_ca_pem = client_ca_pem.clone();
            handshake(vm, conn, move || {
                let certs = parse_cert_chain(&cert_pem)?;
                let key = parse_private_key(&key_pem)?;
                let ca_certs = parse_cert_chain(&client_ca_pem)
                    .map_err(|e| format!("client CA bundle: {e}"))?;
                let mut roots = RootCertStore::empty();
                for ca in ca_certs {
                    roots
                        .add(ca)
                        .map_err(|e| format!("client CA trust anchor: {e}"))?;
                }
                // `build()` requires a client certificate
                // (`allow_unauthenticated()` would let anonymous clients
                // in).
                let verifier = WebPkiClientVerifier::builder(Arc::new(roots))
                    .build()
                    .map_err(|e| format!("client verifier: {e}"))?;
                ServerConfig::builder()
                    .with_client_cert_verifier(verifier)
                    .with_single_cert(certs, key)
                    .map_err(|e| format!("server config: {e}"))
            })
        }
    }

    /// `Err(Tls(why))`.
    fn tls_err(why: impl Into<Arc<str>>) -> Value {
        Value::variant(
            bv::ERR,
            vec![Value::variant(bv::TCP_TLS, vec![Value::String(why.into())])],
        )
    }

    /// The step of `tcp.tls_server` and `tcp.tls_server_mtls`: `conn`
    /// is taken, and the server handshake with `config` runs over its
    /// socket on the I/O pool, as one operation that the task waits
    /// for, like a read.
    ///
    /// A handshake that nobody waits for any more (its time was over,
    /// its task was cancelled or dropped with the program) shuts its
    /// connection down, wherever it stood, as a read's does: a client
    /// that connects and sends nothing holds no thread. So does a
    /// handshake that fails: `conn`'s handle still holds the socket,
    /// which dropping the TLS side alone would not close.
    fn handshake(
        vm: &mut Vm,
        conn: &TcpStreamHandle,
        config: impl FnOnce() -> Result<ServerConfig, String> + Send + 'static,
    ) -> Result<Step, VmError> {
        let sock = match conn.hand_over() {
            Ok(sock) => sock,
            Err(HandOver::Closed) => return Ok(Step::Done(err_closed())),
            Err(HandOver::Tls) => {
                return Ok(Step::Done(tls_err("the connection speaks TLS already")));
            }
            Err(HandOver::Io(e)) => return Ok(Step::Done(tcp_io_err(Doing::Using, &e))),
        };
        let socket = match sock.try_clone() {
            Ok(socket) => socket,
            Err(e) => {
                let _ = sock.shutdown(std::net::Shutdown::Both);
                return Ok(Step::Done(tcp_io_err(Doing::Using, &e)));
            }
        };
        let next_id = vm.next_tcp_id();
        let shutter = Shutter::default();
        shutter.set(&sock);
        let shut = shutter.clone();
        let op = vm.io_op(
            tcp_timeout_err,
            Leave::closes(move || shutter.shut()),
            move |_| {
                let wrapped = config().and_then(|config| {
                    let conn = ServerConnection::new(Arc::new(config))
                        .map_err(|e| format!("server connection setup: {e}"))?;
                    TcpStreamHandle::whole(next_id, &socket, &shut, move || {
                        let stream = rustls::StreamOwned::new(conn, sock);
                        let mut wrapper = ServerStreamWrapper { inner: stream };
                        // The handshake fails here rather than at the first read.
                        wrapper.complete_io_handshake()?;
                        Ok(Box::new(wrapper) as Box<dyn ReadWrite>)
                    })
                });
                match wrapped {
                    Ok(handle) => Value::variant(bv::OK, vec![Value::TcpStream(handle)]),
                    Err(why) => {
                        let _ = socket.shutdown(std::net::Shutdown::Both);
                        tls_err(why)
                    }
                }
            },
        );
        vm.io_wait("tcp", op)
    }

    /// Why `tcp.connect_tls` made no connection: none at all, or none
    /// that speaks TLS with the host.
    enum NoTls {
        Connection(std::io::Error),
        Tls(String),
    }

    impl From<String> for NoTls {
        fn from(why: String) -> NoTls {
            NoTls::Tls(why)
        }
    }

    fn do_connect_tls(
        addr: &str,
        hostname: &str,
        next_id: usize,
        shutter: &Shutter,
    ) -> Result<Arc<TcpStreamHandle>, NoTls> {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let server_name = ServerName::try_from(hostname.to_string())
            .map_err(|e| format!("invalid hostname '{hostname}': {e}"))?;
        let conn = ClientConnection::new(Arc::new(config), server_name)
            .map_err(|e| format!("client connection setup: {e}"))?;
        let sock = TcpStream::connect(addr).map_err(NoTls::Connection)?;
        // Nobody waits for the connect any more: it ends here.
        if !shutter.set(&sock) {
            return Err(NoTls::Connection(std::io::ErrorKind::NotConnected.into()));
        }
        // Clone the fd for the shutdown side-channel BEFORE handing the
        // socket to rustls. `StreamOwned` takes the socket by value, and
        // once inside rustls we can no longer reach the raw fd through
        // the trait object. Both handles reference the same OS fd, so a
        // `shutdown(Both)` on the clone is observed by the rustls stream.
        let socket = sock
            .try_clone()
            .map_err(|e| format!("tcp connect {addr}: {e}"))?;
        let whole = TcpStreamHandle::whole(next_id, &socket, shutter, move || {
            let stream = rustls::StreamOwned::new(conn, sock);
            let mut wrapper = ClientStreamWrapper { inner: stream };
            // The handshake fails here rather than at the first read.
            wrapper.complete_io_handshake()?;
            Ok(Box::new(wrapper) as Box<dyn ReadWrite>)
        })?;
        Ok(whole)
    }

    fn parse_cert_chain(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>, String> {
        let certs: Result<Vec<_>, _> = CertificateDer::pem_slice_iter(pem).collect();
        let certs = certs.map_err(|e| format!("parse cert chain: {}", pem_error(&e)))?;
        if certs.is_empty() {
            return Err("cert PEM contains no certificates".into());
        }
        Ok(certs)
    }

    fn parse_private_key(pem: &[u8]) -> Result<PrivateKeyDer<'static>, String> {
        PrivateKeyDer::from_pem_slice(pem).map_err(|e| match e {
            pem::Error::NoItemsFound => "key PEM contains no private key".into(),
            e => format!("parse private key: {}", pem_error(&e)),
        })
    }

    /// What a PEM that does not parse is: `section end "PRIVATE KEY"
    /// missing`, `illegal section start: "..."`, or the base64 decoder's
    /// own words (`InvalidCharacter(33)`).
    fn pem_error(e: &pem::Error) -> String {
        match e {
            pem::Error::MissingSectionEnd { end_marker } => {
                format!(
                    "section end {:?} missing",
                    String::from_utf8_lossy(end_marker)
                )
            }
            pem::Error::IllegalSectionStart { line } => {
                format!("illegal section start: {:?}", String::from_utf8_lossy(line))
            }
            pem::Error::Base64Decode(text) => text.clone(),
            other => format!("{other:?}"),
        }
    }

    /// Newtype wrappers so we can implement `Read`/`Write` for both client
    /// and server `StreamOwned` types behind the same trait object. The
    /// inner `StreamOwned` type already implements `Read + Write`, but the
    /// monomorphised type names differ (`ClientConnection` vs
    /// `ServerConnection`), so we wrap rather than carrying a bound through.
    ///
    /// The byte-identical impl bodies for
    /// `ClientStreamWrapper` and `ServerStreamWrapper` (same
    /// `complete_io_handshake`, same Read/Write/ReadWrite forwards) are
    /// emitted from a single `stream_wrapper!` macro arm. Adding a third
    /// connection type (e.g. a future Quic/0-RTT variant) is one
    /// invocation; before this round it was ~50 LOC of copy-paste.
    macro_rules! stream_wrapper {
        ($name:ident, $conn:ty) => {
            struct $name {
                inner: rustls::StreamOwned<$conn, TcpStream>,
            }
            impl $name {
                fn complete_io_handshake(&mut self) -> Result<(), String> {
                    // rustls::StreamOwned negotiates lazily on first
                    // read/write. Force handshake completion now so
                    // connect_tls / tls_server failures are reported
                    // synchronously rather than at the first I/O call.
                    while self.inner.conn.is_handshaking() {
                        self.inner
                            .conn
                            .complete_io(&mut self.inner.sock)
                            .map_err(|e| format!("tls handshake: {e}"))?;
                    }
                    Ok(())
                }
            }
            impl Read for $name {
                fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                    self.inner.read(buf)
                }
            }
            impl Write for $name {
                fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                    self.inner.write(buf)
                }
                fn flush(&mut self) -> std::io::Result<()> {
                    self.inner.flush()
                }
            }
            // Manual `ReadWrite` impl so `tcp.close` on Windows can reach
            // the underlying `TcpStream`'s SOCKET handle for `CancelIoEx`.
            // The blanket impl was removed when `ReadWrite::raw_socket`
            // was added.
            impl ReadWrite for $name {
                fn raw_socket(&self) -> Option<usize> {
                    self.inner.sock.raw_socket()
                }
            }
        };
    }

    stream_wrapper!(ClientStreamWrapper, ClientConnection);
    stream_wrapper!(ServerStreamWrapper, ServerConnection);
}

// ── Helpers ────────────────────────────────────────────────────────────

/// What a tcp operation that failed was doing: it says which variant
/// of `TcpError` its failure is.
#[derive(Clone, Copy)]
pub(crate) enum Doing {
    /// `tcp.connect`: no connection was made.
    Connecting,
    /// `tcp.listen`: no listener was bound.
    Binding,
    /// Anything on a connection or a listener that exists.
    Using,
}

/// `Err(TcpError)` of the failure `err` of an operation.
///
/// A deadline that passed is `Timeout` whatever the operation. A
/// connect that fails is `Connect`, a listen that fails `Bind`, each
/// with the system's reason. On a connection that exists, the other
/// end going away is `Closed` however the system reports it (a reset,
/// an abort, a broken pipe, an end of data in the middle of a read),
/// and anything else is `Other`.
pub(crate) fn tcp_io_err(doing: Doing, err: &std::io::Error) -> Value {
    use std::io::ErrorKind;
    let reason = || vec![Value::String(err.to_string().into())];
    let variant = match (err.kind(), doing) {
        (ErrorKind::TimedOut | ErrorKind::WouldBlock, _) => Value::variant(bv::TCP_TIMEOUT, vec![]),
        (_, Doing::Connecting) => Value::variant(bv::TCP_CONNECT, reason()),
        (_, Doing::Binding) => Value::variant(bv::TCP_BIND, reason()),
        (
            ErrorKind::ConnectionReset
            | ErrorKind::ConnectionAborted
            | ErrorKind::BrokenPipe
            | ErrorKind::NotConnected
            | ErrorKind::UnexpectedEof,
            Doing::Using,
        ) => Value::variant(bv::TCP_CLOSED, vec![]),
        (_, Doing::Using) => Value::variant(bv::TCP_OTHER, reason()),
    };
    Value::variant(bv::ERR, vec![variant])
}

/// Build `Err(Other(msg))` for string-form failures (TLS stringly
/// errors from the rustls code path; ad-hoc arg/validation failures).
fn err(s: impl Into<Arc<str>>) -> Value {
    Value::variant(
        bv::ERR,
        vec![Value::variant(bv::TCP_OTHER, vec![Value::String(s.into())])],
    )
}

/// Build `Err(Closed)`.
fn err_closed() -> Value {
    Value::variant(bv::ERR, vec![Value::variant(bv::TCP_CLOSED, vec![])])
}

/// What an accept on a listener that an `http.serve` has to itself
/// says.
const SERVED: &str = "the listener is served by http.serve";

/// The error of an accept on a listener that an `http.serve` has to
/// itself.
fn served(listener: &TcpListenerHandle) -> Option<Value> {
    listener.is_served().then(|| err(SERVED))
}

/// An accept on `listener`, on the I/O pool: the operation of
/// `tcp.accept`, and of each accept of `http.serve`. Its value is
/// `Ok(TcpStream)` or `Err(TcpError)`. It is given up when its waiter
/// goes ([`TcpListenerHandle::stop`]). `server` says that it is the
/// accept of the `http.serve` that has the listener: it also takes the
/// other connections that are ready, for the server to take from the
/// listener ([`TcpListenerHandle::take_kept`]); any other accept ends
/// with an error when a server takes the listener.
pub(crate) fn accept_op(
    vm: &mut Vm,
    listener: &Arc<TcpListenerHandle>,
    server: bool,
) -> crate::vm::IoOp {
    let next_id = vm.next_tcp_id();
    let (accepting, stopping, listener) = (listener.clone(), listener.clone(), listener.clone());
    let leave = Leave::Returns {
        // An accept that has claimed its connection is on its way
        // back.
        interrupt: Some(Box::new(move |wanted| {
            if !wanted.claimed() {
                stopping.stop(wanted)
            }
        })),
        // A connection that came just as the task stopped waiting (it
        // was cancelled, its deadline passed) reached no task: only the
        // operation's value holds it. It is a client's, and goes to the
        // next accept.
        give_back: Some(Box::new(move |value| {
            if let Value::Variant(variant) = value
                && variant.is(bv::OK)
                && let [Value::TcpStream(conn)] = variant.fields()
                && let Some(socket) = conn.socket()
            {
                listener.keep(socket);
            }
        })),
    };
    vm.io_op(tcp_timeout_err, leave, move |wanted| {
        match accepting.accept(wanted, server) {
            Ok(Accepted::Conn(stream)) => Value::variant(
                bv::OK,
                vec![Value::TcpStream(TcpStreamHandle::plain(next_id, stream))],
            ),
            // Given up: nobody reads the value, or a task that was
            // cancelled, or whose time is over, in the steps it has
            // left.
            Ok(Accepted::GivenUp) => err_closed(),
            // A server took the listener while the accept waited.
            Ok(Accepted::Served) => err(SERVED),
            Err(e) => tcp_io_err(Doing::Using, &e),
        }
    })
}

/// What makes an operation on `stream` return when nobody waits for
/// it any more: the connection is shut down.
fn stopper(stream: &Arc<TcpStreamHandle>) -> impl FnOnce() -> bool + Send + 'static {
    let stream = stream.clone();
    move || {
        stream.shut_down();
        true
    }
}

/// The step of a read or a write on `stream`: `op` runs on the I/O
/// pool, and the task waits for its value.
///
/// A wait that ends without that value ends the connection: its
/// time was over (`SILT_IO_TIMEOUT`), its task was cancelled (so by
/// a `task.deadline` that passed, whose function is a task), or
/// dropped with the program. The connection is shut down then,
/// wherever the operation stood: it was queued, it ran (the shutdown
/// makes it return, and its thread end), or its value had come and
/// nobody took it (what a read took is gone with it). What a timeout
/// leaves of a connection does not depend on the moment at which it
/// came.
fn on_connection(
    vm: &mut Vm,
    stream: &Arc<TcpStreamHandle>,
    op: impl FnOnce() -> Value + Send + 'static,
) -> Result<Step, VmError> {
    let unheard = stopper(stream);
    // A close() that races with an operation in flight surfaces that
    // operation's own result. Only a call on a connection that was
    // closed before is rejected here.
    if stream.is_closed() {
        return Ok(Step::Done(err_closed()));
    }
    let op = vm.io_op(tcp_timeout_err, Leave::closes(unheard), move |_| op());
    vm.io_wait("tcp", op)
}

// ── The functions ──────────────────────────────────────────────────────

builtins! {
    // ── Without waiting ────────────────────────────────────────────────

    fn listen(vm, addr: &str) -> Value {
        let id = vm.next_tcp_id();
        match TcpListener::bind(addr) {
            Ok(listener) => {
                let listener = TcpListenerHandle::new(id, listener);
                listener.widen_backlog();
                ok(Value::TcpListener(Arc::new(listener)))
            }
            Err(e) => tcp_io_err(Doing::Binding, &e),
        }
    }

    fn local_port(listener: typed::TcpListener) -> Result<i64, VmError> {
        match listener.local_addr() {
            Ok(addr) => Ok(i64::from(addr.port())),
            Err(e) => Err(VmError::new(format!(
                "tcp.local_port: the listener has no address: {e}"
            ))),
        }
    }

    fn close(stream: typed::TcpStream) {
        stream.shut_down();
    }

    fn peer_addr(stream: typed::TcpStream) -> Value {
        if stream.is_closed() {
            return err_closed();
        }
        match stream.peer_addr() {
            Ok(addr) => ok(Value::String(addr.to_string().into())),
            Err(e) => tcp_io_err(Doing::Using, &e),
        }
    }

    fn set_nodelay(stream: typed::TcpStream, on: bool) -> Value {
        if stream.is_closed() {
            return err_closed();
        }
        match stream.set_nodelay(on) {
            Ok(()) => ok(Value::Unit),
            Err(e) => tcp_io_err(Doing::Using, &e),
        }
    }

    // ── Cooperative I/O ────────────────────────────────────────────────

    fn accept(vm, listener: typed::TcpListener) -> Result<Step, VmError> {
        if let Some(served) = served(listener) {
            return Ok(Step::Done(served));
        }
        let op = accept_op(vm, listener, false);
        vm.io_wait("tcp", op)
    }

    fn connect(vm, addr: &str) -> Result<Step, VmError> {
        let addr = addr.to_string();
        let next_id = vm.next_tcp_id();
        vm.io("tcp", tcp_timeout_err, Leave::RunsOut, move |_| {
            match TcpStream::connect(&addr) {
                Ok(stream) => Value::variant(
                    bv::OK,
                    vec![Value::TcpStream(TcpStreamHandle::plain(next_id, stream))],
                ),
                Err(e) => tcp_io_err(Doing::Connecting, &e),
            }
        })
    }

    fn read(vm, stream: typed::TcpStream, max: i64) -> Result<Step, VmError> {
        let Ok(max) = usize::try_from(max) else {
            return Ok(Step::Done(err(format!(
                "max must be non-negative, got {max}"
            ))));
        };
        let stream = stream.clone();
        on_connection(vm, &stream.clone(), move || {
            // One read takes no more than this at once, whatever was
            // asked for: the buffer is for what can arrive, not for
            // the number.
            let mut buf = vec![0u8; max.min(READ_AT_ONCE)];
            match stream.read(&mut buf) {
                Ok(n) => {
                    buf.truncate(n);
                    Value::variant(bv::OK, vec![Value::Bytes(Arc::new(buf))])
                }
                Err(e) => {
                    // If the stream was closed (locally) while/before this
                    // read, surface as EOF rather than the platform-specific
                    // cancellation error (Windows: WSACancelBlockingCall /
                    // WSA_OPERATION_ABORTED from CancelIoEx in close()).
                    if stream.is_closed() {
                        Value::variant(bv::OK, vec![Value::Bytes(Arc::new(Vec::new()))])
                    } else {
                        tcp_io_err(Doing::Using, &e)
                    }
                }
            }
        })
    }

    fn read_exact(vm, stream: typed::TcpStream, n: i64) -> Result<Step, VmError> {
        let Ok(n) = usize::try_from(n) else {
            return Ok(Step::Done(err(format!("n must be non-negative, got {n}"))));
        };
        let stream = stream.clone();
        on_connection(vm, &stream.clone(), move || {
            match stream.read_exact(n) {
                Ok(buf) => Value::variant(bv::OK, vec![Value::Bytes(Arc::new(buf))]),
                Err(e) => tcp_io_err(Doing::Using, &e),
            }
        })
    }

    fn write(vm, stream: typed::TcpStream, data: Bytes) -> Result<Step, VmError> {
        let (stream, buf) = (stream.clone(), data.clone());
        on_connection(vm, &stream.clone(), move || {
            match stream.write_all(&buf) {
                Ok(()) => Value::variant(bv::OK, vec![Value::Unit]),
                Err(e) => tcp_io_err(Doing::Using, &e),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Error, ErrorKind};

    /// The variant of `Err(TcpError)` that `tcp_io_err` makes.
    fn variant(doing: Doing, kind: ErrorKind) -> String {
        let Value::Variant(result) = tcp_io_err(doing, &Error::from(kind)) else {
            panic!("an Err");
        };
        assert!(result.is(bv::ERR));
        match result.fields() {
            [Value::Variant(error)] => error.name().to_string(),
            other => panic!("one TcpError, not {other:?}"),
        }
    }

    /// The other end going away is `Closed` on a connection, however the
    /// system reports it: a reset read is no failed connect.
    #[test]
    fn a_connection_the_other_end_dropped_is_closed() {
        for kind in [
            ErrorKind::ConnectionReset,
            ErrorKind::ConnectionAborted,
            ErrorKind::BrokenPipe,
            ErrorKind::NotConnected,
            ErrorKind::UnexpectedEof,
        ] {
            assert_eq!(variant(Doing::Using, kind), "Closed", "{kind:?}");
        }
        assert_eq!(variant(Doing::Using, ErrorKind::PermissionDenied), "Other");
    }

    /// A connect that fails is `Connect` and a listen that fails `Bind`,
    /// whatever the reason; a deadline that passed is `Timeout` for
    /// every operation.
    #[test]
    fn a_failure_is_named_after_the_operation() {
        for kind in [
            ErrorKind::ConnectionRefused,
            ErrorKind::ConnectionReset,
            ErrorKind::AddrNotAvailable,
            ErrorKind::InvalidInput,
        ] {
            assert_eq!(variant(Doing::Connecting, kind), "Connect", "{kind:?}");
        }
        for kind in [
            ErrorKind::AddrInUse,
            ErrorKind::AddrNotAvailable,
            ErrorKind::PermissionDenied,
        ] {
            assert_eq!(variant(Doing::Binding, kind), "Bind", "{kind:?}");
        }
        for doing in [Doing::Connecting, Doing::Binding, Doing::Using] {
            assert_eq!(variant(doing, ErrorKind::TimedOut), "Timeout");
            assert_eq!(variant(doing, ErrorKind::WouldBlock), "Timeout");
        }
    }
}
