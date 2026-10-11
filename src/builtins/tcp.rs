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
use std::sync::atomic::AtomicBool;

use super::common::{READ_AT_ONCE, ok};
use super::typed::{self, Bytes, builtins};
use crate::runtime::handle::{Accepted, TcpListenerHandle, TcpStreamHandle};
use crate::typeinfo::bv;
use crate::value::Value;
use crate::vm::{Step, Vm, VmError};

/// The error of a `tcp` function whose operation has no value of its
/// own: `TcpTimeout` when its deadline passed, `TcpUnknown` with the
/// reason when it could not run or panicked.
fn tcp_timeout_err(failure: crate::vm::IoFailure<'_>) -> Value {
    use crate::vm::IoFailure;
    let error = match failure {
        IoFailure::Timeout(_) => Value::variant(bv::TCP_TIMEOUT, vec![]),
        IoFailure::Panicked(why) | IoFailure::Refused(why) => {
            Value::variant(bv::TCP_UNKNOWN, vec![Value::String(why.to_string())])
        }
    };
    Value::variant(bv::ERR, vec![error])
}

/// What `TcpError`'s `message` says of the variant `tag` with `fields`:
/// `None` if they are no variant of it.
pub(crate) fn error_text(tag: &str, fields: &[Value]) -> Option<String> {
    Some(match (tag, fields) {
        ("TcpConnect", [Value::String(m)]) => format!("tcp connect failed: {m}"),
        ("TcpTls", [Value::String(m)]) => format!("tcp TLS error: {m}"),
        ("TcpClosed", []) => "tcp connection closed".to_string(),
        ("TcpTimeout", []) => "tcp operation timed out".to_string(),
        ("TcpUnknown", [Value::String(m)]) => m.clone(),
        _ => return None,
    })
}

#[cfg(feature = "tcp-tls")]
pub(crate) use tls::{accept_tls, accept_tls_mtls, connect_tls};

#[cfg(feature = "tcp-tls")]
mod tls {
    //! TLS extension for the tcp module — gated by the `tcp-tls` feature.
    //!
    //! Both `connect_tls` and `accept_tls` return regular `Value::TcpStream`
    //! handles. The `Box<dyn ReadWrite>` inside the handle now carries a
    //! `rustls::StreamOwned<...>` instead of a bare `TcpStream`; from the
    //! caller's perspective `tcp.read` / `tcp.write` / `tcp.close` work
    //! identically. This is the payoff of PR 2's trait-object design.

    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
    use rustls::server::WebPkiClientVerifier;
    use rustls::{ClientConfig, ClientConnection, RootCertStore, ServerConfig, ServerConnection};

    use crate::typeinfo::bv;

    use super::{
        Accepted, SERVED, Step, TcpListenerHandle, TcpStreamHandle, Value, VmError, err, served,
        tcp_timeout_err,
    };
    use crate::builtins::typed::{self, Bytes, builtins};
    use crate::runtime::handle::ReadWrite;

    builtins! {
        /// Opens a TCP connection then performs the TLS client handshake
        /// using `webpki-roots` for trust anchors. The returned stream
        /// wraps a `rustls::StreamOwned<ClientConnection, TcpStream>`
        /// behind the same `TcpStreamHandle` as plain TCP.
        fn connect_tls(vm, addr: &str, hostname: &str) -> Result<Step, VmError> {
            let (addr, hostname) = (addr.to_string(), hostname.to_string());
            let next_id = vm.next_tcp_id();
            vm.io("tcp", tcp_timeout_err, move || {
                match do_connect_tls(&addr, &hostname, next_id) {
                    Ok(handle) => Value::variant(bv::OK, vec![Value::TcpStream(handle)]),
                    Err(e) => Value::variant(
                        bv::ERR,
                        vec![Value::variant(bv::TCP_TLS, vec![Value::String(e)])],
                    ),
                }
            })
        }

        /// Waits for an incoming TCP connection then performs the TLS
        /// server handshake using the supplied PEM-encoded cert chain +
        /// private key. Returned stream is the same opaque `TcpStream`
        /// handle as plain TCP.
        fn accept_tls(
            vm,
            listener: typed::TcpListener,
            cert_pem: Bytes,
            key_pem: Bytes,
        ) -> Result<Step, VmError> {
            if let Some(served) = served(listener) {
                return Ok(Step::Done(served));
            }
            let (listener, cert_pem, key_pem) = (listener.clone(), cert_pem.clone(), key_pem.clone());
            let next_id = vm.next_tcp_id();
            let (giving_up, stop) = GivingUp::of(&listener);
            vm.io_stoppable("tcp", tcp_timeout_err, stop, move || {
                accepted(do_accept_tls(
                    &listener, &giving_up, &cert_pem, &key_pem, next_id,
                ))
            })
        }

        /// Like `accept_tls` but also requires the connecting client to
        /// present a certificate chaining to one of the CAs in
        /// `client_ca_pem`. Built using
        /// `rustls::server::WebPkiClientVerifier::builder(roots).build()`.
        /// If the client does not present a cert, or the presented cert
        /// does not chain to the supplied CA bundle, the handshake fails
        /// and the call returns `Err(msg)`.
        fn accept_tls_mtls(
            vm,
            listener: typed::TcpListener,
            cert_pem: Bytes,
            key_pem: Bytes,
            client_ca_pem: Bytes,
        ) -> Result<Step, VmError> {
            if let Some(served) = served(listener) {
                return Ok(Step::Done(served));
            }
            let listener = listener.clone();
            let (cert_pem, key_pem) = (cert_pem.clone(), key_pem.clone());
            let client_ca_pem = client_ca_pem.clone();
            let next_id = vm.next_tcp_id();
            let (giving_up, stop) = GivingUp::of(&listener);
            vm.io_stoppable("tcp", tcp_timeout_err, stop, move || {
                accepted(do_accept_tls_mtls(
                    &listener,
                    &giving_up,
                    &cert_pem,
                    &key_pem,
                    &client_ca_pem,
                    next_id,
                ))
            })
        }
    }

    /// The value of a TLS accept: the connection, or why there is
    /// none. A server that has taken the listener is the same error
    /// here as for a plain accept.
    fn accepted(result: Result<Arc<TcpStreamHandle>, String>) -> Value {
        match result {
            Ok(handle) => Value::variant(bv::OK, vec![Value::TcpStream(handle)]),
            Err(e) if e == SERVED => err(e),
            Err(e) => Value::variant(
                bv::ERR,
                vec![Value::variant(bv::TCP_TLS, vec![Value::String(e)])],
            ),
        }
    }

    fn do_connect_tls(
        addr: &str,
        hostname: &str,
        next_id: usize,
    ) -> Result<Arc<TcpStreamHandle>, String> {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let server_name = ServerName::try_from(hostname.to_string())
            .map_err(|e| format!("invalid hostname '{hostname}': {e}"))?;
        let conn = ClientConnection::new(Arc::new(config), server_name)
            .map_err(|e| format!("client connection setup: {e}"))?;
        let sock = TcpStream::connect(addr).map_err(|e| format!("tcp connect {addr}: {e}"))?;
        // Clone the fd for the shutdown side-channel BEFORE handing the
        // socket to rustls. `StreamOwned` takes the socket by value, and
        // once inside rustls we can no longer reach the raw fd through
        // the trait object. Both handles reference the same OS fd, so a
        // `shutdown(Both)` on the clone is observed by the rustls stream.
        let socket = sock
            .try_clone()
            .map_err(|e| format!("tcp connect {addr}: {e}"))?;
        TcpStreamHandle::whole(next_id, &socket, move || {
            let stream = rustls::StreamOwned::new(conn, sock);
            let mut wrapper = ClientStreamWrapper { inner: stream };
            // The handshake fails here rather than at the first read.
            wrapper.complete_io_handshake()?;
            Ok(Box::new(wrapper) as Box<dyn ReadWrite>)
        })
    }

    fn do_accept_tls(
        listener: &TcpListenerHandle,
        giving_up: &GivingUp,
        cert_pem: &[u8],
        key_pem: &[u8],
        next_id: usize,
    ) -> Result<Arc<TcpStreamHandle>, String> {
        let certs = parse_cert_chain(cert_pem)?;
        let key = parse_private_key(key_pem)?;
        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|e| format!("server config: {e}"))?;
        let sock = giving_up.accepted(listener)?;
        let conn = ServerConnection::new(Arc::new(config))
            .map_err(|e| format!("server connection setup: {e}"))?;
        // See `do_connect_tls`: clone the fd before handing it to rustls
        // so `tcp.close` can `shutdown(Both)` the underlying socket even
        // while a concurrent read is parked inside `StreamOwned::read`.
        let socket = sock.try_clone().map_err(|e| format!("tcp accept: {e}"))?;
        TcpStreamHandle::whole(next_id, &socket, move || {
            let stream = rustls::StreamOwned::new(conn, sock);
            let mut wrapper = ServerStreamWrapper { inner: stream };
            // The handshake fails here rather than at the first read.
            wrapper.complete_io_handshake()?;
            giving_up.handshake_done();
            Ok(Box::new(wrapper) as Box<dyn ReadWrite>)
        })
    }

    fn do_accept_tls_mtls(
        listener: &TcpListenerHandle,
        giving_up: &GivingUp,
        cert_pem: &[u8],
        key_pem: &[u8],
        client_ca_pem: &[u8],
        next_id: usize,
    ) -> Result<Arc<TcpStreamHandle>, String> {
        let certs = parse_cert_chain(cert_pem)?;
        let key = parse_private_key(key_pem)?;
        let ca_certs =
            parse_cert_chain(client_ca_pem).map_err(|e| format!("client CA bundle: {e}"))?;
        let mut roots = RootCertStore::empty();
        for ca in ca_certs {
            roots
                .add(ca)
                .map_err(|e| format!("client CA trust anchor: {e}"))?;
        }
        // Build a WebPkiClientVerifier that *requires* a client cert
        // chaining to the supplied CA bundle. `builder(...).build()`
        // defaults to required-auth (use `allow_unauthenticated()` on
        // the builder if anonymous clients should be accepted). If no
        // cert is offered, or the offered cert does not chain, the
        // handshake fails and `complete_io_handshake` surfaces the
        // error via `Err`.
        let verifier = WebPkiClientVerifier::builder(Arc::new(roots))
            .build()
            .map_err(|e| format!("client verifier: {e}"))?;
        let config = ServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(certs, key)
            .map_err(|e| format!("server config: {e}"))?;
        let sock = giving_up.accepted(listener)?;
        let conn = ServerConnection::new(Arc::new(config))
            .map_err(|e| format!("server connection setup: {e}"))?;
        let socket = sock.try_clone().map_err(|e| format!("tcp accept: {e}"))?;
        TcpStreamHandle::whole(next_id, &socket, move || {
            let stream = rustls::StreamOwned::new(conn, sock);
            let mut wrapper = ServerStreamWrapper { inner: stream };
            // The handshake fails here rather than at the first read.
            wrapper.complete_io_handshake()?;
            giving_up.handshake_done();
            Ok(Box::new(wrapper) as Box<dyn ReadWrite>)
        })
    }

    /// How a TLS accept is given up when nobody waits for it any more:
    /// while it waits for a connection, as a plain accept is; while it
    /// is in the handshake with a client that sends nothing, by
    /// shutting that connection down. Either way its thread ends. A
    /// connection that is given up in its handshake is closed: it was
    /// no task's yet, and it is not one the next accept could take
    /// over.
    #[derive(Clone)]
    pub(super) struct GivingUp {
        stopped: Arc<AtomicBool>,
        /// The connection, while the handshake runs.
        handshaking: Arc<parking_lot::Mutex<Option<TcpStream>>>,
    }

    impl GivingUp {
        fn of(listener: &Arc<TcpListenerHandle>) -> (GivingUp, impl FnOnce() + Send + 'static) {
            let giving_up = GivingUp {
                stopped: Arc::new(AtomicBool::new(false)),
                handshaking: Arc::default(),
            };
            let (listener, mine) = (listener.clone(), giving_up.clone());
            (giving_up, move || {
                // The flag first: an accept that returns now reads it
                // under the lock below.
                listener.stop(&mine.stopped);
                if let Some(socket) = mine.handshaking.lock().take() {
                    let _ = socket.shutdown(std::net::Shutdown::Both);
                }
            })
        }

        /// The next connection of `listener`, noted as in its
        /// handshake; an error if the accept was given up.
        fn accepted(&self, listener: &TcpListenerHandle) -> Result<TcpStream, String> {
            let sock = match listener.accept(&self.stopped, false) {
                Ok(Accepted::Conn(sock)) => sock,
                Ok(Accepted::GivenUp) => return Err("tcp accept: given up".into()),
                Ok(Accepted::Served) => return Err(SERVED.into()),
                Err(e) => return Err(format!("tcp accept: {e}")),
            };
            let mut handshaking = self.handshaking.lock();
            if self.stopped.load(Ordering::SeqCst) {
                let _ = sock.shutdown(std::net::Shutdown::Both);
                return Err("tcp accept: given up".into());
            }
            *handshaking = sock.try_clone().ok();
            Ok(sock)
        }

        fn handshake_done(&self) {
            self.handshaking.lock().take();
        }
    }

    fn parse_cert_chain(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>, String> {
        let mut reader = std::io::BufReader::new(pem);
        let certs: Result<Vec<_>, _> = rustls_pemfile::certs(&mut reader).collect();
        let certs = certs.map_err(|e| format!("parse cert chain: {e}"))?;
        if certs.is_empty() {
            return Err("cert PEM contains no certificates".into());
        }
        Ok(certs)
    }

    fn parse_private_key(pem: &[u8]) -> Result<PrivateKeyDer<'static>, String> {
        let mut reader = std::io::BufReader::new(pem);
        rustls_pemfile::private_key(&mut reader)
            .map_err(|e| format!("parse private key: {e}"))?
            .ok_or_else(|| "key PEM contains no private key".into())
    }

    /// Newtype wrappers so we can implement `Read`/`Write` for both client
    /// and server `StreamOwned` types behind the same trait object. The
    /// inner `StreamOwned` type already implements `Read + Write`, but the
    /// monomorphised type names differ (`ClientConnection` vs
    /// `ServerConnection`), so we wrap rather than carrying a bound through.
    ///
    /// Round 80 dedup (DUP-1): the byte-identical impl bodies for
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
                    // connect_tls / accept_tls failures are reported
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

/// Classify a `std::io::Error` into a `TcpError` variant.
fn tcp_error_to_variant(err: &std::io::Error) -> Value {
    use std::io::ErrorKind;
    let msg = err.to_string();
    match err.kind() {
        ErrorKind::ConnectionRefused
        | ErrorKind::ConnectionReset
        | ErrorKind::NotConnected
        | ErrorKind::AddrInUse
        | ErrorKind::AddrNotAvailable
        | ErrorKind::HostUnreachable
        | ErrorKind::NetworkUnreachable => {
            Value::variant(bv::TCP_CONNECT, vec![Value::String(msg)])
        }
        ErrorKind::BrokenPipe | ErrorKind::ConnectionAborted | ErrorKind::UnexpectedEof => {
            Value::variant(bv::TCP_CLOSED, vec![])
        }
        ErrorKind::TimedOut | ErrorKind::WouldBlock => Value::variant(bv::TCP_TIMEOUT, vec![]),
        _ => Value::variant(bv::TCP_UNKNOWN, vec![Value::String(msg)]),
    }
}

/// Build `Err(TcpError)` from a `std::io::Error`.
fn tcp_io_err(err: &std::io::Error) -> Value {
    Value::variant(bv::ERR, vec![tcp_error_to_variant(err)])
}

/// Build `Err(TcpUnknown(msg))` for string-form failures (TLS stringly
/// errors from the rustls code path; ad-hoc arg/validation failures).
fn err(s: impl Into<String>) -> Value {
    Value::variant(
        bv::ERR,
        vec![Value::variant(
            bv::TCP_UNKNOWN,
            vec![Value::String(s.into())],
        )],
    )
}

/// Build `Err(TcpClosed)`.
fn err_closed() -> Value {
    Value::variant(bv::ERR, vec![Value::variant(bv::TCP_CLOSED, vec![])])
}

/// The flag of an accept on `listener`, and what gives the accept up.
fn accept_stop(
    listener: &Arc<TcpListenerHandle>,
) -> (Arc<AtomicBool>, impl FnOnce() + Send + 'static) {
    let stopped = Arc::new(AtomicBool::new(false));
    let (listener, flag) = (listener.clone(), stopped.clone());
    (stopped, move || listener.stop(&flag))
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
    let (stopped, stop) = accept_stop(listener);
    let (accepting, listener) = (listener.clone(), listener.clone());
    vm.runtime
        .io_pool
        .submit(tcp_timeout_err, move || {
            match accepting.accept(&stopped, server) {
                Ok(Accepted::Conn(stream)) => Value::variant(
                    bv::OK,
                    vec![Value::TcpStream(TcpStreamHandle::plain(next_id, stream))],
                ),
                // Given up: nobody reads the value.
                Ok(Accepted::GivenUp) => err_closed(),
                // A server took the listener while the accept waited.
                Ok(Accepted::Served) => err(SERVED),
                Err(e) => tcp_io_err(&e),
            }
        })
        .stop_with(stop)
        // A connection that came just as the task stopped waiting (it
        // was cancelled, its deadline passed) reached no task: only
        // the operation's value holds it. It is a client's, and goes
        // to the next accept.
        .unheard_with(move |value| {
            if let Value::Variant(tag, fields) = value
                && tag.is(bv::OK)
                && let [Value::TcpStream(conn)] = &fields[..]
                && let Some(socket) = conn.socket()
            {
                listener.keep(socket);
            }
        })
}

/// What makes an operation on `stream` return when nobody waits for
/// it any more: the connection is shut down.
fn stopper(stream: &Arc<TcpStreamHandle>) -> impl FnOnce() + Send + 'static {
    let stream = stream.clone();
    move || stream.shut_down()
}

/// The step of a read or a write on `stream`: `op` runs on the I/O
/// pool, and the task waits for its value.
///
/// A wait that ends without that value ends the connection: its
/// deadline passed (`task.deadline`, `SILT_IO_TIMEOUT`), its task was
/// cancelled, or dropped with the program. The connection is shut down
/// then, wherever the operation stood: it had not begun (its time was
/// over before it started, and nothing is run), it was queued, it ran
/// (the shutdown makes it return, and its thread end), or its value
/// had come and nobody took it (what a read took is gone with it).
/// What a timeout leaves of a connection does not depend on the
/// moment at which it came.
fn on_connection(
    vm: &mut Vm,
    stream: &Arc<TcpStreamHandle>,
    op: impl FnOnce() -> Value + Send + 'static,
) -> Result<Step, VmError> {
    let unheard = stopper(stream);
    if let Some(timed_out) = vm.deadline_exceeded_with(tcp_timeout_err) {
        unheard();
        return Ok(Step::Done(timed_out));
    }
    // A close() that races with an operation in flight surfaces that
    // operation's own result. Only a call on a connection that was
    // closed before is rejected here.
    if stream.is_closed() {
        return Ok(Step::Done(err_closed()));
    }
    let op = vm
        .runtime
        .io_pool
        .submit(tcp_timeout_err, op)
        .stop_unless_taken(unheard);
    vm.io_wait("tcp", tcp_timeout_err, op)
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
            Err(e) => tcp_io_err(&e),
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
            Ok(addr) => ok(Value::String(addr.to_string())),
            Err(e) => tcp_io_err(&e),
        }
    }

    fn set_nodelay(stream: typed::TcpStream, on: bool) -> Value {
        if stream.is_closed() {
            return err_closed();
        }
        match stream.set_nodelay(on) {
            Ok(()) => ok(Value::Unit),
            Err(e) => tcp_io_err(&e),
        }
    }

    // ── Cooperative I/O ────────────────────────────────────────────────

    fn accept(vm, listener: typed::TcpListener) -> Result<Step, VmError> {
        if let Some(served) = served(listener) {
            return Ok(Step::Done(served));
        }
        let op = accept_op(vm, listener, false);
        vm.io_wait("tcp", tcp_timeout_err, op)
    }

    fn connect(vm, addr: &str) -> Result<Step, VmError> {
        let addr = addr.to_string();
        let next_id = vm.next_tcp_id();
        vm.io("tcp", tcp_timeout_err, move || {
            match TcpStream::connect(&addr) {
                Ok(stream) => Value::variant(
                    bv::OK,
                    vec![Value::TcpStream(TcpStreamHandle::plain(next_id, stream))],
                ),
                Err(e) => tcp_io_err(&e),
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
                        tcp_io_err(&e)
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
                Err(e) => tcp_io_err(&e),
            }
        })
    }

    fn write(vm, stream: typed::TcpStream, data: Bytes) -> Result<Step, VmError> {
        let (stream, buf) = (stream.clone(), data.clone());
        on_connection(vm, &stream.clone(), move || {
            match stream.write_all(&buf) {
                Ok(()) => Value::variant(bv::OK, vec![Value::Unit]),
                Err(e) => tcp_io_err(&e),
            }
        })
    }
}
