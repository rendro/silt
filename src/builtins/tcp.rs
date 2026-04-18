//! `tcp.*` builtin functions: TCP listeners and streams with cooperative
//! I/O integration.
//!
//! Blocking ops (`accept`, `connect`, `read`, `read_exact`, `write`)
//! follow the same pattern as `io.read_file`: check `vm.io_entry_guard`,
//! submit to `vm.runtime.io_pool`, return a yield signal so the scheduler
//! can run other tasks while this one waits. On wake, the entry guard
//! polls completion and resumes.
//!
//! Stream payload type is `Value::Bytes` from PR 1 — read returns Bytes,
//! write accepts Bytes. The `TcpStreamHandle` wraps `Box<dyn ReadWrite>`
//! so v0.9 PR 3 can transparently substitute a TLS-wrapped stream behind
//! the same handle type.
//!
//! Listeners are deliberately bare `std::net::TcpListener` — `accept` is
//! the only blocking op and it locks the listener via the io_pool thread
//! so concurrent silt tasks do not deadlock.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;

use crate::value::{ReadWrite, TcpListenerHandle, TcpStreamHandle, Value};
use crate::vm::{BlockReason, Vm, VmError};

pub fn call(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "listen" => listen(vm, args),
        "accept" => accept(vm, args),
        "connect" => connect(vm, args),
        "read" => read(vm, args),
        "read_exact" => read_exact(vm, args),
        "write" => write(vm, args),
        "close" => close(args),
        "peer_addr" => peer_addr(args),
        "set_nodelay" => set_nodelay(args),
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
    use std::sync::atomic::AtomicBool;

    use parking_lot::Mutex;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
    use rustls::server::WebPkiClientVerifier;
    use rustls::{ClientConfig, ClientConnection, RootCertStore, ServerConfig, ServerConnection};

    use super::{
        BlockReason, ReadWrite, TcpStreamHandle, Value, Vm, VmError, err, ok, require_bytes,
        require_listener, require_string,
    };

    /// `connect_tls(addr, hostname) -> Result(TcpStream, String)`. Opens a
    /// TCP connection then performs the TLS client handshake using
    /// `webpki-roots` for trust anchors. The returned stream wraps a
    /// `rustls::StreamOwned<ClientConnection, TcpStream>` behind the same
    /// `TcpStreamHandle` as plain TCP.
    pub fn connect_tls(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
        if args.len() != 2 {
            return Err(VmError::new("tcp.connect_tls takes 2 arguments".into()));
        }
        let addr = require_string(&args[0], "tcp.connect_tls")?.to_string();
        let hostname = require_string(&args[1], "tcp.connect_tls")?.to_string();

        if let Some(r) = vm.io_entry_guard(args)? {
            return Ok(r);
        }
        if vm.is_scheduled_task {
            let next_id = vm.next_tcp_id();
            let completion = vm.runtime.io_pool.submit(move || {
                match do_connect_tls(&addr, &hostname, next_id) {
                    Ok(handle) => Value::Variant("Ok".into(), vec![Value::TcpStream(handle)]),
                    Err(e) => Value::Variant("Err".into(), vec![Value::String(e)]),
                }
            });
            vm.pending_io = Some(completion.clone());
            vm.block_reason = Some(BlockReason::Io(completion));
            for arg in args {
                vm.push(arg.clone());
            }
            return Err(VmError::yield_signal());
        }
        let next_id = vm.next_tcp_id();
        match do_connect_tls(&addr, &hostname, next_id) {
            Ok(handle) => Ok(ok(Value::TcpStream(handle))),
            Err(e) => Ok(err(e)),
        }
    }

    /// `accept_tls(listener, cert_pem, key_pem) -> Result(TcpStream, String)`.
    /// Waits for an incoming TCP connection then performs the TLS server
    /// handshake using the supplied PEM-encoded cert chain + private key.
    /// Returned stream is the same opaque `TcpStream` handle as plain TCP.
    pub fn accept_tls(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
        if args.len() != 3 {
            return Err(VmError::new("tcp.accept_tls takes 3 arguments".into()));
        }
        let listener = require_listener(&args[0], "tcp.accept_tls")?.clone();
        let cert_pem = require_bytes(&args[1], "tcp.accept_tls")?;
        let key_pem = require_bytes(&args[2], "tcp.accept_tls")?;

        if let Some(r) = vm.io_entry_guard(args)? {
            return Ok(r);
        }
        if vm.is_scheduled_task {
            let next_id = vm.next_tcp_id();
            let cert_clone = cert_pem.clone();
            let key_clone = key_pem.clone();
            let completion = vm.runtime.io_pool.submit(move || {
                match do_accept_tls(&listener.listener, &cert_clone, &key_clone, next_id) {
                    Ok(handle) => Value::Variant("Ok".into(), vec![Value::TcpStream(handle)]),
                    Err(e) => Value::Variant("Err".into(), vec![Value::String(e)]),
                }
            });
            vm.pending_io = Some(completion.clone());
            vm.block_reason = Some(BlockReason::Io(completion));
            for arg in args {
                vm.push(arg.clone());
            }
            return Err(VmError::yield_signal());
        }
        let next_id = vm.next_tcp_id();
        match do_accept_tls(&listener.listener, &cert_pem, &key_pem, next_id) {
            Ok(handle) => Ok(ok(Value::TcpStream(handle))),
            Err(e) => Ok(err(e)),
        }
    }

    /// `accept_tls_mtls(listener, cert_pem, key_pem, client_ca_pem)
    /// -> Result(TcpStream, String)`. Like `accept_tls` but also requires
    /// the connecting client to present a certificate chaining to one of
    /// the CAs in `client_ca_pem`. Built using
    /// `rustls::server::WebPkiClientVerifier::builder(roots).build()`.
    /// If the client does not present a cert, or the presented cert does
    /// not chain to the supplied CA bundle, the handshake fails and the
    /// call returns `Err(msg)`.
    pub fn accept_tls_mtls(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
        if args.len() != 4 {
            return Err(VmError::new("tcp.accept_tls_mtls takes 4 arguments".into()));
        }
        let listener = require_listener(&args[0], "tcp.accept_tls_mtls")?.clone();
        let cert_pem = require_bytes(&args[1], "tcp.accept_tls_mtls")?;
        let key_pem = require_bytes(&args[2], "tcp.accept_tls_mtls")?;
        let client_ca_pem = require_bytes(&args[3], "tcp.accept_tls_mtls")?;

        if let Some(r) = vm.io_entry_guard(args)? {
            return Ok(r);
        }
        if vm.is_scheduled_task {
            let next_id = vm.next_tcp_id();
            let cert_clone = cert_pem.clone();
            let key_clone = key_pem.clone();
            let ca_clone = client_ca_pem.clone();
            let completion = vm.runtime.io_pool.submit(move || {
                match do_accept_tls_mtls(
                    &listener.listener,
                    &cert_clone,
                    &key_clone,
                    &ca_clone,
                    next_id,
                ) {
                    Ok(handle) => Value::Variant("Ok".into(), vec![Value::TcpStream(handle)]),
                    Err(e) => Value::Variant("Err".into(), vec![Value::String(e)]),
                }
            });
            vm.pending_io = Some(completion.clone());
            vm.block_reason = Some(BlockReason::Io(completion));
            for arg in args {
                vm.push(arg.clone());
            }
            return Err(VmError::yield_signal());
        }
        let next_id = vm.next_tcp_id();
        match do_accept_tls_mtls(
            &listener.listener,
            &cert_pem,
            &key_pem,
            &client_ca_pem,
            next_id,
        ) {
            Ok(handle) => Ok(ok(Value::TcpStream(handle))),
            Err(e) => Ok(err(e)),
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
        let shutdown_sock = sock.try_clone().ok();
        let reader_socket = super::raw_socket_of(&sock);
        let stream = rustls::StreamOwned::new(conn, sock);
        // StreamOwned owns the connection + socket; once placed in the
        // trait object the caller can't reach into rustls internals,
        // matching plain TCP semantics.
        let mut wrapper = ClientStreamWrapper { inner: stream };
        // Force the handshake by performing one byte-less read attempt; if
        // the handshake fails it surfaces here rather than at first read.
        // We swallow WouldBlock since the TcpStream is blocking by default.
        wrapper.complete_io_handshake()?;
        Ok(Arc::new(TcpStreamHandle {
            id: next_id,
            inner: Mutex::new(Box::new(wrapper) as Box<dyn ReadWrite>),
            closed: AtomicBool::new(false),
            shutdown_sock: Mutex::new(shutdown_sock),
            reader_socket,
        }))
    }

    fn do_accept_tls(
        listener: &std::net::TcpListener,
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
        let (sock, _addr) = listener.accept().map_err(|e| format!("tcp accept: {e}"))?;
        let conn = ServerConnection::new(Arc::new(config))
            .map_err(|e| format!("server connection setup: {e}"))?;
        // See `do_connect_tls`: clone the fd before handing it to rustls
        // so `tcp.close` can `shutdown(Both)` the underlying socket even
        // while a concurrent read is parked inside `StreamOwned::read`.
        let shutdown_sock = sock.try_clone().ok();
        let reader_socket = super::raw_socket_of(&sock);
        let stream = rustls::StreamOwned::new(conn, sock);
        let mut wrapper = ServerStreamWrapper { inner: stream };
        wrapper.complete_io_handshake()?;
        Ok(Arc::new(TcpStreamHandle {
            id: next_id,
            inner: Mutex::new(Box::new(wrapper) as Box<dyn ReadWrite>),
            closed: AtomicBool::new(false),
            shutdown_sock: Mutex::new(shutdown_sock),
            reader_socket,
        }))
    }

    fn do_accept_tls_mtls(
        listener: &std::net::TcpListener,
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
        let (sock, _addr) = listener.accept().map_err(|e| format!("tcp accept: {e}"))?;
        let conn = ServerConnection::new(Arc::new(config))
            .map_err(|e| format!("server connection setup: {e}"))?;
        let shutdown_sock = sock.try_clone().ok();
        let reader_socket = super::raw_socket_of(&sock);
        let stream = rustls::StreamOwned::new(conn, sock);
        let mut wrapper = ServerStreamWrapper { inner: stream };
        wrapper.complete_io_handshake()?;
        Ok(Arc::new(TcpStreamHandle {
            id: next_id,
            inner: Mutex::new(Box::new(wrapper) as Box<dyn ReadWrite>),
            closed: AtomicBool::new(false),
            shutdown_sock: Mutex::new(shutdown_sock),
            reader_socket,
        }))
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
    struct ClientStreamWrapper {
        inner: rustls::StreamOwned<ClientConnection, TcpStream>,
    }
    impl ClientStreamWrapper {
        fn complete_io_handshake(&mut self) -> Result<(), String> {
            // rustls::StreamOwned negotiates lazily on first read/write.
            // Force handshake completion now so connect_tls failures are
            // reported synchronously rather than at the first I/O call.
            while self.inner.conn.is_handshaking() {
                self.inner
                    .conn
                    .complete_io(&mut self.inner.sock)
                    .map_err(|e| format!("tls handshake: {e}"))?;
            }
            Ok(())
        }
    }
    impl Read for ClientStreamWrapper {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.inner.read(buf)
        }
    }
    impl Write for ClientStreamWrapper {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.inner.write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.inner.flush()
        }
    }
    // Manual `ReadWrite` impl so `tcp.close` on Windows can reach the
    // underlying `TcpStream`'s SOCKET handle for `CancelIoEx`. The
    // blanket impl was removed when `ReadWrite::raw_socket` was added.
    impl ReadWrite for ClientStreamWrapper {
        fn raw_socket(&self) -> Option<usize> {
            self.inner.sock.raw_socket()
        }
    }

    struct ServerStreamWrapper {
        inner: rustls::StreamOwned<ServerConnection, TcpStream>,
    }
    impl ServerStreamWrapper {
        fn complete_io_handshake(&mut self) -> Result<(), String> {
            while self.inner.conn.is_handshaking() {
                self.inner
                    .conn
                    .complete_io(&mut self.inner.sock)
                    .map_err(|e| format!("tls handshake: {e}"))?;
            }
            Ok(())
        }
    }
    impl Read for ServerStreamWrapper {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.inner.read(buf)
        }
    }
    impl Write for ServerStreamWrapper {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.inner.write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.inner.flush()
        }
    }
    impl ReadWrite for ServerStreamWrapper {
        fn raw_socket(&self) -> Option<usize> {
            self.inner.sock.raw_socket()
        }
    }
}

// ── Helpers ────────────────────────────────────────────────────────────

fn ok(v: Value) -> Value {
    Value::Variant("Ok".into(), vec![v])
}

fn err(s: impl Into<String>) -> Value {
    Value::Variant("Err".into(), vec![Value::String(s.into())])
}

fn require_string<'a>(arg: &'a Value, fn_label: &str) -> Result<&'a str, VmError> {
    match arg {
        Value::String(s) => Ok(s.as_str()),
        _ => Err(VmError::new(format!("{fn_label} requires String"))),
    }
}

fn require_int(arg: &Value, fn_label: &str) -> Result<i64, VmError> {
    match arg {
        Value::Int(n) => Ok(*n),
        _ => Err(VmError::new(format!("{fn_label} requires Int"))),
    }
}

#[cfg(feature = "tcp-tls")]
fn require_bytes(arg: &Value, fn_label: &str) -> Result<Arc<Vec<u8>>, VmError> {
    match arg {
        Value::Bytes(b) => Ok(b.clone()),
        _ => Err(VmError::new(format!("{fn_label} requires Bytes"))),
    }
}

fn require_bool(arg: &Value, fn_label: &str) -> Result<bool, VmError> {
    match arg {
        Value::Bool(b) => Ok(*b),
        _ => Err(VmError::new(format!("{fn_label} requires Bool"))),
    }
}

fn require_listener<'a>(
    arg: &'a Value,
    fn_label: &str,
) -> Result<&'a Arc<TcpListenerHandle>, VmError> {
    match arg {
        Value::TcpListener(l) => Ok(l),
        _ => Err(VmError::new(format!("{fn_label} requires TcpListener"))),
    }
}

fn require_stream<'a>(arg: &'a Value, fn_label: &str) -> Result<&'a Arc<TcpStreamHandle>, VmError> {
    match arg {
        Value::TcpStream(s) => Ok(s),
        _ => Err(VmError::new(format!("{fn_label} requires TcpStream"))),
    }
}

fn make_stream(stream: TcpStream, vm: &mut Vm) -> Value {
    let id = vm.next_tcp_id();
    // Clone the fd for the side-channel shutdown path. `try_clone` can only
    // fail on resource exhaustion; if it does, we drop back to Drop-based
    // close semantics (the closed flag still prevents further ops).
    let shutdown_sock = stream.try_clone().ok();
    let reader_socket = raw_socket_of(&stream);
    Value::TcpStream(Arc::new(TcpStreamHandle {
        id,
        inner: Mutex::new(Box::new(stream) as Box<dyn ReadWrite>),
        closed: AtomicBool::new(false),
        shutdown_sock: Mutex::new(shutdown_sock),
        reader_socket,
    }))
}

/// Cache the OS socket for the inner stream so `tcp.close` on Windows
/// can issue `CancelIoEx` on it without needing to acquire `inner`'s
/// mutex (which a parked reader may be holding). On Unix this is also
/// computed but currently unused.
fn raw_socket_of(stream: &TcpStream) -> Option<usize> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawSocket;
        Some(stream.as_raw_socket() as usize)
    }
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        Some(stream.as_raw_fd() as usize)
    }
    #[cfg(not(any(windows, unix)))]
    {
        None
    }
}

// ── Non-blocking ops ───────────────────────────────────────────────────

fn listen(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("tcp.listen takes 1 argument".into()));
    }
    let addr = require_string(&args[0], "tcp.listen")?;
    match TcpListener::bind(addr) {
        Ok(listener) => {
            let id = vm.next_tcp_id();
            Ok(ok(Value::TcpListener(Arc::new(TcpListenerHandle {
                id,
                listener,
            }))))
        }
        Err(e) => Ok(err(format!("tcp.listen({addr}): {e}"))),
    }
}

fn close(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("tcp.close takes 1 argument".into()));
    }
    let s = require_stream(&args[0], "tcp.close")?;
    if !s.closed.swap(true, Ordering::SeqCst) {
        // Best-effort flush of anything buffered in the Rust-side writer.
        // We use `try_lock` so we do not block here — a concurrent
        // `tcp.read` on another task holds `inner` while parked on the fd,
        // and calling `lock()` would deadlock until that read returns.
        // The subsequent `shutdown(Both)` is what kicks the reader loose.
        if let Some(mut guard) = s.inner.try_lock() {
            let _ = guard.flush();
        }
        // Shut down the underlying fd so any task blocked inside
        // `TcpStream::read` (possibly holding `inner`) wakes up with EOF
        // and releases its handle promptly, rather than keeping the fd
        // pinned until the last `Arc<TcpStreamHandle>` drops. For TLS
        // streams this skips rustls `close_notify`; we accept a rough
        // shutdown over a deadlocked or indefinitely-open fd. Errors
        // (EBADF, ENOTCONN, peer already closed, etc.) are ignored —
        // close() ergonomically can't surface a partial failure and the
        // `closed` flag is already set so subsequent ops will error.
        //
        // Platform-specific unblock behavior:
        //   * Unix: `shutdown(Both)` on any fd sharing the underlying
        //     open-file description delivers EOF to a parked `recv` on
        //     a sibling clone. We keep the cloned fd alive so buffered
        //     data in the socket receive queue (if any) can still be
        //     drained by late readers before they notice the shutdown.
        //   * Windows: Winsock's `shutdown(SD_BOTH)` does NOT cancel an
        //     already-in-progress blocking `recv` on a duplicate handle.
        //     After issuing the shutdown, we also drop our cloned
        //     `TcpStream` (which invokes `closesocket` on the duplicate
        //     handle via its `Drop` impl). On Windows this cancels
        //     pending I/O on the underlying socket, which is what
        //     unblocks a `recv` parked on a sibling handle held by the
        //     `inner` stream. Caveat: this also prevents any further
        //     reads from draining already-buffered receive data on that
        //     handle — but since the user called `close`, that's the
        //     intended semantics.
        // `mut` is required on Windows where we `take()` the handle
        // out of the slot; on Unix we only call `shutdown` through a
        // shared ref. The `#[allow]` keeps both cfgs clean.
        #[allow(unused_mut)]
        let mut slot = s.shutdown_sock.lock();
        if let Some(sock) = slot.as_ref() {
            let _ = sock.shutdown(Shutdown::Both);
        }
        // On Windows, the shutdown above is not enough: Winsock's
        // `shutdown(SD_BOTH)` does NOT cancel an in-progress blocking
        // `recv` on a duplicate SOCKET handle (created via
        // `WSADuplicateSocket` aka `TcpStream::try_clone`). The parked
        // reader is using the SOCKET held by `inner`, NOT this
        // duplicate, so the duplicate's shutdown has no effect on it.
        //
        // The fix is to call `CancelIoEx(inner_socket, NULL)`, which
        // tells the kernel to cancel pending I/O on the parked
        // reader's SOCKET. The parked `recv` returns with
        // `WSAENOTSOCK` / `WSAEINTR` and the task wakes.
        //
        // We can't acquire `inner`'s mutex to read its raw socket
        // because the parked reader is holding it (that's the whole
        // point: it's parked inside `recv`). Instead we read the
        // socket value cached on the handle at construction time
        // (`s.reader_socket`), which never changes for the lifetime
        // of the handle.
        //
        // We then also drop the cloned `TcpStream` (the duplicate
        // SOCKET) to keep the `closesocket` semantics from round 30
        // and avoid leaking the duplicate handle.
        #[cfg(windows)]
        {
            if let Some(sock) = s.reader_socket {
                // SAFETY: `CancelIoEx` is safe to call on any HANDLE,
                // including a SOCKET. It returns 0/error if the
                // handle is invalid or no I/O is pending — both of
                // which are fine ignored outcomes for `close`. We
                // cast `usize -> HANDLE` (a pointer-sized integer in
                // both 32- and 64-bit Windows). NULL `lpOverlapped`
                // means "cancel ALL pending I/O on this handle from
                // any thread", which is exactly what we want.
                use std::ptr;
                use windows_sys::Win32::Foundation::HANDLE;
                use windows_sys::Win32::System::IO::CancelIoEx;
                unsafe {
                    let _ = CancelIoEx(sock as HANDLE, ptr::null_mut());
                }
            }
            let _ = slot.take();
        }
        drop(slot);
    }
    Ok(Value::Unit)
}

fn peer_addr(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("tcp.peer_addr takes 1 argument".into()));
    }
    let _ = require_stream(&args[0], "tcp.peer_addr")?;
    // peer_addr requires the underlying TcpStream; the trait-object form
    // hides it. Returning a placeholder Err for now — full support comes
    // when TLS streams need a uniform peer_addr interface.
    // For PR 2 alone, this is acceptable since most flows don't need it.
    Ok(err(
        "tcp.peer_addr is not yet implemented for trait-object stream handles",
    ))
}

fn set_nodelay(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("tcp.set_nodelay takes 2 arguments".into()));
    }
    let _ = require_stream(&args[0], "tcp.set_nodelay")?;
    let _ = require_bool(&args[1], "tcp.set_nodelay")?;
    // Same trait-object issue as peer_addr.
    Ok(err(
        "tcp.set_nodelay is not yet implemented for trait-object stream handles",
    ))
}

// ── Cooperative I/O ops ────────────────────────────────────────────────

fn accept(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("tcp.accept takes 1 argument".into()));
    }
    let listener = require_listener(&args[0], "tcp.accept")?.clone();
    if let Some(r) = vm.io_entry_guard(args)? {
        return Ok(r);
    }
    if vm.is_scheduled_task {
        let next_id = vm.next_tcp_id();
        let completion = vm
            .runtime
            .io_pool
            .submit(move || match listener.listener.accept() {
                Ok((stream, _addr)) => {
                    let shutdown_sock = stream.try_clone().ok();
                    let reader_socket = raw_socket_of(&stream);
                    let handle = Arc::new(TcpStreamHandle {
                        id: next_id,
                        inner: Mutex::new(Box::new(stream) as Box<dyn ReadWrite>),
                        closed: AtomicBool::new(false),
                        shutdown_sock: Mutex::new(shutdown_sock),
                        reader_socket,
                    });
                    Value::Variant("Ok".into(), vec![Value::TcpStream(handle)])
                }
                Err(e) => Value::Variant("Err".into(), vec![Value::String(e.to_string())]),
            });
        vm.pending_io = Some(completion.clone());
        vm.block_reason = Some(BlockReason::Io(completion));
        for arg in args {
            vm.push(arg.clone());
        }
        return Err(VmError::yield_signal());
    }
    // Main thread: synchronous fallback.
    match listener.listener.accept() {
        Ok((stream, _)) => Ok(ok(make_stream(stream, vm))),
        Err(e) => Ok(err(e.to_string())),
    }
}

fn connect(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("tcp.connect takes 1 argument".into()));
    }
    let addr = require_string(&args[0], "tcp.connect")?.to_string();
    if let Some(r) = vm.io_entry_guard(args)? {
        return Ok(r);
    }
    if vm.is_scheduled_task {
        let next_id = vm.next_tcp_id();
        let addr_for_closure = addr.clone();
        let completion =
            vm.runtime
                .io_pool
                .submit(move || match TcpStream::connect(&addr_for_closure) {
                    Ok(stream) => {
                        let shutdown_sock = stream.try_clone().ok();
                        let reader_socket = raw_socket_of(&stream);
                        let handle = Arc::new(TcpStreamHandle {
                            id: next_id,
                            inner: Mutex::new(Box::new(stream) as Box<dyn ReadWrite>),
                            closed: AtomicBool::new(false),
                            shutdown_sock: Mutex::new(shutdown_sock),
                            reader_socket,
                        });
                        Value::Variant("Ok".into(), vec![Value::TcpStream(handle)])
                    }
                    Err(e) => Value::Variant("Err".into(), vec![Value::String(e.to_string())]),
                });
        vm.pending_io = Some(completion.clone());
        vm.block_reason = Some(BlockReason::Io(completion));
        for arg in args {
            vm.push(arg.clone());
        }
        return Err(VmError::yield_signal());
    }
    match TcpStream::connect(&addr) {
        Ok(stream) => Ok(ok(make_stream(stream, vm))),
        Err(e) => Ok(err(e.to_string())),
    }
}

fn read(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("tcp.read takes 2 arguments".into()));
    }
    let stream = require_stream(&args[0], "tcp.read")?.clone();
    let max = require_int(&args[1], "tcp.read")?;
    if max < 0 {
        return Ok(err(format!("max must be non-negative, got {max}")));
    }
    let max = max as usize;
    // Drain any already-pending completion first so a close() that
    // races with an in-flight read surfaces the read's actual result
    // (typically Ok(empty) = EOF after shutdown) rather than the
    // synthetic "stream is closed" error below. Only reject fresh
    // calls on a stream closed before we submitted anything.
    if let Some(r) = vm.io_entry_guard(args)? {
        return Ok(r);
    }
    if stream.closed.load(Ordering::SeqCst) {
        return Ok(err("tcp.read: stream is closed"));
    }
    if vm.is_scheduled_task {
        let stream_clone = stream.clone();
        let completion = vm.runtime.io_pool.submit(move || {
            let mut buf = vec![0u8; max];
            let mut guard = stream_clone.inner.lock();
            match guard.read(&mut buf) {
                Ok(n) => {
                    buf.truncate(n);
                    Value::Variant("Ok".into(), vec![Value::Bytes(Arc::new(buf))])
                }
                Err(e) => {
                    // If the stream was closed (locally) while/before this
                    // read, surface as EOF rather than the platform-specific
                    // cancellation error (Windows: WSACancelBlockingCall /
                    // WSA_OPERATION_ABORTED from CancelIoEx in close()).
                    if stream_clone.closed.load(Ordering::SeqCst) {
                        Value::Variant("Ok".into(), vec![Value::Bytes(Arc::new(Vec::new()))])
                    } else {
                        Value::Variant("Err".into(), vec![Value::String(e.to_string())])
                    }
                }
            }
        });
        vm.pending_io = Some(completion.clone());
        vm.block_reason = Some(BlockReason::Io(completion));
        for arg in args {
            vm.push(arg.clone());
        }
        return Err(VmError::yield_signal());
    }
    let mut buf = vec![0u8; max];
    let mut guard = stream.inner.lock();
    match guard.read(&mut buf) {
        Ok(n) => {
            buf.truncate(n);
            Ok(ok(Value::Bytes(Arc::new(buf))))
        }
        Err(e) => {
            if stream.closed.load(Ordering::SeqCst) {
                Ok(ok(Value::Bytes(Arc::new(Vec::new()))))
            } else {
                Ok(err(e.to_string()))
            }
        }
    }
}

fn read_exact(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("tcp.read_exact takes 2 arguments".into()));
    }
    let stream = require_stream(&args[0], "tcp.read_exact")?.clone();
    let n = require_int(&args[1], "tcp.read_exact")?;
    if n < 0 {
        return Ok(err(format!("n must be non-negative, got {n}")));
    }
    let n = n as usize;
    // See `read`: io_entry_guard before closed-check so a pending
    // completion wins over a racing close().
    if let Some(r) = vm.io_entry_guard(args)? {
        return Ok(r);
    }
    if stream.closed.load(Ordering::SeqCst) {
        return Ok(err("tcp.read_exact: stream is closed"));
    }
    if vm.is_scheduled_task {
        let stream_clone = stream.clone();
        let completion = vm.runtime.io_pool.submit(move || {
            let mut buf = vec![0u8; n];
            let mut guard = stream_clone.inner.lock();
            match guard.read_exact(&mut buf) {
                Ok(()) => Value::Variant("Ok".into(), vec![Value::Bytes(Arc::new(buf))]),
                Err(e) => Value::Variant("Err".into(), vec![Value::String(e.to_string())]),
            }
        });
        vm.pending_io = Some(completion.clone());
        vm.block_reason = Some(BlockReason::Io(completion));
        for arg in args {
            vm.push(arg.clone());
        }
        return Err(VmError::yield_signal());
    }
    let mut buf = vec![0u8; n];
    let mut guard = stream.inner.lock();
    match guard.read_exact(&mut buf) {
        Ok(()) => Ok(ok(Value::Bytes(Arc::new(buf)))),
        Err(e) => Ok(err(e.to_string())),
    }
}

fn write(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("tcp.write takes 2 arguments".into()));
    }
    let stream = require_stream(&args[0], "tcp.write")?.clone();
    let buf = match &args[1] {
        Value::Bytes(b) => b.clone(),
        _ => return Err(VmError::new("tcp.write requires Bytes".into())),
    };
    // See `read`: io_entry_guard before closed-check so a pending
    // completion wins over a racing close().
    if let Some(r) = vm.io_entry_guard(args)? {
        return Ok(r);
    }
    if stream.closed.load(Ordering::SeqCst) {
        return Ok(err("tcp.write: stream is closed"));
    }
    if vm.is_scheduled_task {
        let stream_clone = stream.clone();
        let completion = vm.runtime.io_pool.submit(move || {
            let mut guard = stream_clone.inner.lock();
            match guard.write_all(&buf) {
                Ok(()) => match guard.flush() {
                    Ok(()) => Value::Variant("Ok".into(), vec![Value::Unit]),
                    Err(e) => Value::Variant("Err".into(), vec![Value::String(e.to_string())]),
                },
                Err(e) => Value::Variant("Err".into(), vec![Value::String(e.to_string())]),
            }
        });
        vm.pending_io = Some(completion.clone());
        vm.block_reason = Some(BlockReason::Io(completion));
        for arg in args {
            vm.push(arg.clone());
        }
        return Err(VmError::yield_signal());
    }
    let mut guard = stream.inner.lock();
    match guard.write_all(&buf) {
        Ok(()) => match guard.flush() {
            Ok(()) => Ok(ok(Value::Unit)),
            Err(e) => Ok(err(e.to_string())),
        },
        Err(e) => Ok(err(e.to_string())),
    }
}
