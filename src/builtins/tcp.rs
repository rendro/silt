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
//! A plain connection has a half for reading and one for writing, so a
//! task that reads and a task that writes do not wait for each other;
//! a TLS connection is one object behind one lock
//! ([`TcpStreamHandle`]).

use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use super::common::ok;
use crate::runtime::handle::{TcpListenerHandle, TcpStreamHandle};
use crate::typeinfo::bv;
use crate::value::Value;
use crate::vm::{Step, Vm, VmError};

/// Factory: deadline-cancelled tcp op surfaces as `Err(TcpTimeout)`
/// rather than the default `Err(IoUnknown(_))`. Used by every tcp.*
/// builtin that runs on the I/O pool. The message text is dropped because
/// `TcpTimeout` is a nullary variant; `e.message()` still produces a
/// helpful string via the trait impl.
fn tcp_timeout_err(_msg: &str) -> Value {
    Value::variant(bv::ERR, vec![Value::variant(bv::TCP_TIMEOUT, vec![])])
}

/// Dispatch the builtin `trait Error for TcpError` method table.
/// Scaffolding lives in `super::dispatch_error_trait`; this site just
/// supplies the variant → message rendering.
pub fn call_tcp_error_trait(name: &str, args: &[Value]) -> Result<Value, VmError> {
    super::dispatch_error_trait("TcpError", name, args, |tag, fields| {
        Some(match (tag, fields) {
            ("TcpConnect", [Value::String(m)]) => format!("tcp connect failed: {m}"),
            ("TcpTls", [Value::String(m)]) => format!("tcp TLS error: {m}"),
            ("TcpClosed", []) => "tcp connection closed".to_string(),
            ("TcpTimeout", []) => "tcp operation timed out".to_string(),
            ("TcpUnknown", [Value::String(m)]) => m.clone(),
            _ => return None,
        })
    })
}

pub(crate) fn call(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Step, VmError> {
    match name {
        "listen" => listen(vm, args).map(Step::Done),
        "local_port" => local_port(args).map(Step::Done),
        "accept" => accept(vm, args),
        "connect" => connect(vm, args),
        "read" => read(vm, args),
        "read_exact" => read_exact(vm, args),
        "write" => write(vm, args),
        "close" => close(args).map(Step::Done),
        "peer_addr" => peer_addr(args).map(Step::Done),
        "set_nodelay" => set_nodelay(args).map(Step::Done),
        #[cfg(feature = "tcp-tls")]
        "connect_tls" => tls::connect_tls(vm, args),
        #[cfg(feature = "tcp-tls")]
        "accept_tls" => tls::accept_tls(vm, args),
        #[cfg(feature = "tcp-tls")]
        "accept_tls_mtls" => tls::accept_tls_mtls(vm, args),
        _ => Err(VmError::new(format!("unknown tcp function: {name}"))),
    }
}

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
        Step, TcpListenerHandle, TcpStreamHandle, Value, Vm, VmError, require_bytes,
        require_listener, require_string, tcp_timeout_err,
    };
    use crate::runtime::handle::ReadWrite;

    /// `connect_tls(addr, hostname) -> Result(TcpStream, String)`. Opens a
    /// TCP connection then performs the TLS client handshake using
    /// `webpki-roots` for trust anchors. The returned stream wraps a
    /// `rustls::StreamOwned<ClientConnection, TcpStream>` behind the same
    /// `TcpStreamHandle` as plain TCP.
    pub fn connect_tls(vm: &mut Vm, args: &[Value]) -> Result<Step, VmError> {
        if args.len() != 2 {
            return Err(VmError::new("tcp.connect_tls takes 2 arguments".into()));
        }
        let addr = require_string(&args[0], "tcp.connect_tls")?.to_string();
        let hostname = require_string(&args[1], "tcp.connect_tls")?.to_string();
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

    /// `accept_tls(listener, cert_pem, key_pem) -> Result(TcpStream, String)`.
    /// Waits for an incoming TCP connection then performs the TLS server
    /// handshake using the supplied PEM-encoded cert chain + private key.
    /// Returned stream is the same opaque `TcpStream` handle as plain TCP.
    pub fn accept_tls(vm: &mut Vm, args: &[Value]) -> Result<Step, VmError> {
        if args.len() != 3 {
            return Err(VmError::new("tcp.accept_tls takes 3 arguments".into()));
        }
        let listener = require_listener(&args[0], "tcp.accept_tls")?.clone();
        let cert_pem = require_bytes(&args[1], "tcp.accept_tls")?;
        let key_pem = require_bytes(&args[2], "tcp.accept_tls")?;
        let next_id = vm.next_tcp_id();
        let (stopped, stop) = super::accept_stop(&listener);
        vm.io_stoppable("tcp", tcp_timeout_err, stop, move || {
            match do_accept_tls(&listener, &stopped, &cert_pem, &key_pem, next_id) {
                Ok(handle) => Value::variant(bv::OK, vec![Value::TcpStream(handle)]),
                Err(e) => Value::variant(
                    bv::ERR,
                    vec![Value::variant(bv::TCP_TLS, vec![Value::String(e)])],
                ),
            }
        })
    }

    /// `accept_tls_mtls(listener, cert_pem, key_pem, client_ca_pem)
    /// -> Result(TcpStream, String)`. Like `accept_tls` but also requires
    /// the connecting client to present a certificate chaining to one of
    /// the CAs in `client_ca_pem`. Built using
    /// `rustls::server::WebPkiClientVerifier::builder(roots).build()`.
    /// If the client does not present a cert, or the presented cert does
    /// not chain to the supplied CA bundle, the handshake fails and the
    /// call returns `Err(msg)`.
    pub fn accept_tls_mtls(vm: &mut Vm, args: &[Value]) -> Result<Step, VmError> {
        if args.len() != 4 {
            return Err(VmError::new("tcp.accept_tls_mtls takes 4 arguments".into()));
        }
        let listener = require_listener(&args[0], "tcp.accept_tls_mtls")?.clone();
        let cert_pem = require_bytes(&args[1], "tcp.accept_tls_mtls")?;
        let key_pem = require_bytes(&args[2], "tcp.accept_tls_mtls")?;
        let client_ca_pem = require_bytes(&args[3], "tcp.accept_tls_mtls")?;
        let next_id = vm.next_tcp_id();
        let (stopped, stop) = super::accept_stop(&listener);
        vm.io_stoppable(
            "tcp",
            tcp_timeout_err,
            stop,
            move || match do_accept_tls_mtls(
                &listener,
                &stopped,
                &cert_pem,
                &key_pem,
                &client_ca_pem,
                next_id,
            ) {
                Ok(handle) => Value::variant(bv::OK, vec![Value::TcpStream(handle)]),
                Err(e) => Value::variant(
                    bv::ERR,
                    vec![Value::variant(bv::TCP_TLS, vec![Value::String(e)])],
                ),
            },
        )
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
        stopped: &Arc<AtomicBool>,
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
        let sock = accepted(listener, stopped)?;
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
            Ok(Box::new(wrapper) as Box<dyn ReadWrite>)
        })
    }

    fn do_accept_tls_mtls(
        listener: &TcpListenerHandle,
        stopped: &Arc<AtomicBool>,
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
        let sock = accepted(listener, stopped)?;
        let conn = ServerConnection::new(Arc::new(config))
            .map_err(|e| format!("server connection setup: {e}"))?;
        let socket = sock.try_clone().map_err(|e| format!("tcp accept: {e}"))?;
        TcpStreamHandle::whole(next_id, &socket, move || {
            let stream = rustls::StreamOwned::new(conn, sock);
            let mut wrapper = ServerStreamWrapper { inner: stream };
            // The handshake fails here rather than at the first read.
            wrapper.complete_io_handshake()?;
            Ok(Box::new(wrapper) as Box<dyn ReadWrite>)
        })
    }

    /// The next connection of `listener`; an error if the accept was
    /// given up.
    fn accepted(
        listener: &TcpListenerHandle,
        stopped: &Arc<AtomicBool>,
    ) -> Result<TcpStream, String> {
        match listener.accept(stopped) {
            Ok(Some(sock)) => Ok(sock),
            Ok(None) => {
                debug_assert!(stopped.load(Ordering::SeqCst));
                Err("tcp accept: given up".into())
            }
            Err(e) => Err(format!("tcp accept: {e}")),
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

// Round 65 dedup (DC2): the byte-identical bodies of these helpers
// previously lived in both `tcp.rs` and `stream.rs`. They now delegate
// to `super::common::{require_str_borrow, require_int}`. The
// thin wrappers preserve the local function names so the dozens of
// existing call sites in this module stay unchanged.
fn require_string<'a>(arg: &'a Value, fn_label: &str) -> Result<&'a str, VmError> {
    super::common::require_str_borrow(arg, fn_label)
}

fn require_int(arg: &Value, fn_label: &str) -> Result<i64, VmError> {
    super::common::require_int(arg, fn_label)
}

// Round 79 (F5 BLOAT/parity): the tcp-local `require_bool` /
// `require_bytes` previously emitted `"<fn> requires Bool"` /
// `"<fn> requires Bytes"` without the canonical `, got <kind>` tail
// established in round 75 across `numeric.rs`, `string.rs`,
// `collections.rs`, `bytes.rs`, `crypto.rs`, `encoding.rs`, and
// `uuid.rs`. The TCP-specific `require_listener` / `require_stream`
// were similarly truncated. We now route through the canonical
// helpers in `super::common` (for Bool/Bytes) and use
// `super::common::value_kind` to render the offending kind in the
// listener/stream arms — keeping the diagnostic shape uniform.
//
// Round 79: `require_bytes` is no longer gated behind `tcp-tls` because
// `tcp.write` (always available) needs the same canonical-shape error
// when its second arg is not Bytes. The pre-fix `tcp.write` body had a
// hand-rolled `"tcp.write requires Bytes"` (no `, got <kind>` tail) at
// the inline match arm; routing through the common helper unifies the
// shape.
fn require_bytes(arg: &Value, fn_label: &str) -> Result<Arc<Vec<u8>>, VmError> {
    super::common::require_bytes(arg, fn_label)
}

fn require_bool(arg: &Value, fn_label: &str) -> Result<bool, VmError> {
    super::common::require_bool(arg, fn_label)
}

fn require_listener<'a>(
    arg: &'a Value,
    fn_label: &str,
) -> Result<&'a Arc<TcpListenerHandle>, VmError> {
    match arg {
        Value::TcpListener(l) => Ok(l),
        other => Err(VmError::new(format!(
            "{fn_label} requires TcpListener, got {}",
            super::common::value_kind(other)
        ))),
    }
}

fn require_stream<'a>(arg: &'a Value, fn_label: &str) -> Result<&'a Arc<TcpStreamHandle>, VmError> {
    match arg {
        Value::TcpStream(s) => Ok(s),
        other => Err(VmError::new(format!(
            "{fn_label} requires TcpStream, got {}",
            super::common::value_kind(other)
        ))),
    }
}

/// The flag of an accept on `listener`, and what gives the accept up.
fn accept_stop(
    listener: &Arc<TcpListenerHandle>,
) -> (Arc<AtomicBool>, impl FnOnce() + Send + 'static) {
    let stopped = Arc::new(AtomicBool::new(false));
    let (listener, flag) = (listener.clone(), stopped.clone());
    (stopped, move || listener.stop(&flag))
}

// ── Non-blocking ops ───────────────────────────────────────────────────

fn listen(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("tcp.listen takes 1 argument".into()));
    }
    let addr = require_string(&args[0], "tcp.listen")?;
    let id = vm.next_tcp_id();
    match TcpListener::bind(addr) {
        Ok(listener) => Ok(ok(Value::TcpListener(Arc::new(TcpListenerHandle::new(
            id, listener,
        ))))),
        Err(e) => Ok(tcp_io_err(&e)),
    }
}

fn local_port(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("tcp.local_port takes 1 argument".into()));
    }
    let listener = require_listener(&args[0], "tcp.local_port")?;
    match listener.local_addr() {
        Ok(addr) => Ok(Value::Int(i64::from(addr.port()))),
        Err(e) => Err(VmError::new(format!(
            "tcp.local_port: the listener has no address: {e}"
        ))),
    }
}

fn close(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("tcp.close takes 1 argument".into()));
    }
    require_stream(&args[0], "tcp.close")?.shut_down();
    Ok(Value::Unit)
}

fn peer_addr(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("tcp.peer_addr takes 1 argument".into()));
    }
    let stream = require_stream(&args[0], "tcp.peer_addr")?;
    if stream.is_closed() {
        return Ok(err_closed());
    }
    Ok(match stream.peer_addr() {
        Ok(addr) => ok(Value::String(addr.to_string())),
        Err(e) => tcp_io_err(&e),
    })
}

fn set_nodelay(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("tcp.set_nodelay takes 2 arguments".into()));
    }
    let stream = require_stream(&args[0], "tcp.set_nodelay")?;
    let on = require_bool(&args[1], "tcp.set_nodelay")?;
    if stream.is_closed() {
        return Ok(err_closed());
    }
    Ok(match stream.set_nodelay(on) {
        Ok(()) => ok(Value::Unit),
        Err(e) => tcp_io_err(&e),
    })
}

// ── Cooperative I/O ops ────────────────────────────────────────────────

fn accept(vm: &mut Vm, args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("tcp.accept takes 1 argument".into()));
    }
    let listener = require_listener(&args[0], "tcp.accept")?.clone();
    let next_id = vm.next_tcp_id();
    let (stopped, stop) = accept_stop(&listener);
    let accepting = listener.clone();
    let op = vm
        .runtime
        .io_pool
        .submit(tcp_timeout_err, move || {
            let stream = match accepting.accept(&stopped) {
                Ok(Some(stream)) => TcpStreamHandle::plain(next_id, stream),
                // Given up: nobody reads the value.
                Ok(None) => return err_closed(),
                Err(e) => Err(e),
            };
            match stream {
                Ok(handle) => Value::variant(bv::OK, vec![Value::TcpStream(handle)]),
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
        });
    vm.io_wait("tcp", tcp_timeout_err, op)
}

fn connect(vm: &mut Vm, args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("tcp.connect takes 1 argument".into()));
    }
    let addr = require_string(&args[0], "tcp.connect")?.to_string();
    let next_id = vm.next_tcp_id();
    vm.io("tcp", tcp_timeout_err, move || {
        match TcpStream::connect(&addr).and_then(|s| TcpStreamHandle::plain(next_id, s)) {
            Ok(handle) => Value::variant(bv::OK, vec![Value::TcpStream(handle)]),
            Err(e) => tcp_io_err(&e),
        }
    })
}

fn read(vm: &mut Vm, args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("tcp.read takes 2 arguments".into()));
    }
    let stream = require_stream(&args[0], "tcp.read")?.clone();
    let max = require_int(&args[1], "tcp.read")?;
    if max < 0 {
        return Ok(Step::Done(err(format!(
            "max must be non-negative, got {max}"
        ))));
    }
    let max = max as usize;
    // A close() that races with a read in flight surfaces the read's
    // actual result (typically Ok(empty) = EOF after shutdown). Only a
    // call on a stream that was closed before is rejected here.
    if let Some(r) = vm.deadline_exceeded_with(tcp_timeout_err) {
        return Ok(Step::Done(r));
    }
    if stream.is_closed() {
        return Ok(Step::Done(err_closed()));
    }
    let stop = stopper(&stream);
    vm.io_stoppable("tcp", tcp_timeout_err, stop, move || {
        let mut buf = vec![0u8; max];
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

fn read_exact(vm: &mut Vm, args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("tcp.read_exact takes 2 arguments".into()));
    }
    let stream = require_stream(&args[0], "tcp.read_exact")?.clone();
    let n = require_int(&args[1], "tcp.read_exact")?;
    if n < 0 {
        return Ok(Step::Done(err(format!("n must be non-negative, got {n}"))));
    }
    let n = n as usize;
    if let Some(r) = vm.deadline_exceeded_with(tcp_timeout_err) {
        return Ok(Step::Done(r));
    }
    if stream.is_closed() {
        return Ok(Step::Done(err_closed()));
    }
    let stop = stopper(&stream);
    vm.io_stoppable("tcp", tcp_timeout_err, stop, move || {
        let mut buf = vec![0u8; n];
        match stream.read_exact(&mut buf) {
            Ok(()) => Value::variant(bv::OK, vec![Value::Bytes(Arc::new(buf))]),
            Err(e) => tcp_io_err(&e),
        }
    })
}

fn write(vm: &mut Vm, args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("tcp.write takes 2 arguments".into()));
    }
    let stream = require_stream(&args[0], "tcp.write")?.clone();
    let buf = require_bytes(&args[1], "tcp.write")?;
    if let Some(r) = vm.deadline_exceeded_with(tcp_timeout_err) {
        return Ok(Step::Done(r));
    }
    if stream.is_closed() {
        return Ok(Step::Done(err_closed()));
    }
    let stop = stopper(&stream);
    vm.io_stoppable("tcp", tcp_timeout_err, stop, move || {
        match stream.write_all(&buf) {
            Ok(()) => Value::variant(bv::OK, vec![Value::Unit]),
            Err(e) => tcp_io_err(&e),
        }
    })
}

/// What makes an operation on `stream` return when nobody waits for
/// it any more: the connection is shut down.
fn stopper(stream: &Arc<TcpStreamHandle>) -> impl FnOnce() + Send + 'static {
    let stream = stream.clone();
    move || stream.shut_down()
}
