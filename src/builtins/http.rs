//! The `http.*` builtin functions.

#[cfg(feature = "http")]
use std::collections::BTreeMap;
#[cfg(feature = "http")]
use std::collections::HashMap;
#[cfg(feature = "http")]
use std::sync::Arc;
#[cfg(feature = "http")]
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
#[cfg(feature = "http")]
use std::time::Duration;

#[cfg(feature = "http")]
use super::typed;
use super::typed::builtins;
#[cfg(feature = "http")]
use super::typed::{Arg, Bytes, Map, TcpListener, unsound};
#[cfg(feature = "http")]
use crate::runtime::handle::{Taken, TaskHandle, TcpListenerHandle, TcpStreamHandle};
#[cfg(feature = "http")]
use crate::runtime::sync::{Arm, Cell, Fired, Wait, Wake};
#[cfg(feature = "http")]
use crate::typeinfo::{BuiltinVariant, bv, ty};
#[cfg(feature = "http")]
use crate::url::Url;
use crate::value::Value;
#[cfg(feature = "http")]
use crate::vm::{Leave, Limit, Step, Vm, VmError};
#[cfg(feature = "http")]
use parking_lot::Mutex;

/// How `HttpError` shows the variant `tag` with `fields`:
/// `None` if they are no variant of it.
pub(crate) fn error_text(tag: &str, fields: &[Value]) -> Option<String> {
    Some(match (tag, fields) {
        ("Connect", [Value::String(m, _)]) => format!("http connect failed: {m}"),
        ("Tls", [Value::String(m, _)]) => format!("http TLS error: {m}"),
        ("Timeout", []) => "http request timed out".to_string(),
        ("InvalidUrl", [Value::String(u, _)]) => format!("http invalid url: {u}"),
        ("InvalidResponse", [Value::String(m, _)]) => {
            format!("http invalid response: {m}")
        }
        ("ClosedEarly", []) => "http connection closed before response completed".to_string(),
        ("TooManyRedirects", []) => {
            format!("http too many redirects: more than {REDIRECTS_MAX}")
        }
        ("Other", [Value::String(m, _)]) => m.to_string(),
        _ => return None,
    })
}

// ── HTTP dispatch ───────────────────────────────────────────────────

#[cfg(feature = "http")]
fn make_http_response(status: u16, headers: crate::value::Map, body: Vec<u8>) -> Value {
    Value::builtin_record(
        ty::RESPONSE,
        [
            ("status", Value::Int(status as i64)),
            ("body", Value::Bytes(Arc::new(body))),
            ("headers", Value::Map(headers)),
        ],
    )
}

#[cfg(feature = "http")]
fn make_http_request_value(
    method: BuiltinVariant,
    url: Url,
    headers: crate::value::Map,
    body: Vec<u8>,
    peer: &str,
) -> Value {
    Value::builtin_record(
        ty::REQUEST,
        [
            ("method", Value::variant(method, [])),
            ("url", Value::Url(Arc::new(url))),
            ("headers", Value::Map(headers)),
            ("body", Value::Bytes(Arc::new(body))),
            ("peer", Value::string(peer)),
        ],
    )
}

#[cfg(feature = "http")]
fn extract_http_response(val: &Value) -> Result<(u16, &[u8], &crate::value::Record), VmError> {
    // (What is no `Response` is no value the handler's type has.)
    let fields = match val {
        Value::Record(record) if record.type_id() == ty::RESPONSE => record,
        _ => return Err(unsound("http.serve", "handler")),
    };
    let (Some(given), Some(body)) = (
        fields.get("status").and_then(i64::take),
        fields.get("body").and_then(Bytes::take),
    ) else {
        return Err(unsound("http.serve", "handler"));
    };
    // A status has three digits, and the one of a response is final:
    // 1xx only announces a response, and a client that got one would
    // wait for the response itself.
    let status = match u16::try_from(given) {
        Ok(status) if (200..=999).contains(&status) => status,
        _ => {
            return Err(VmError::new(format!(
                "Response.status out of range: {given} is not the status of a response (200..=999)"
            )));
        }
    };
    Ok((status, body, fields))
}

/// The body of `response` as the program gets it: the bytes that
/// arrived, unpacked if `packed` (the response says `Content-Encoding:
/// gzip`). `None` for a body of more than `limit` bytes: no more than
/// `limit + 1` bytes of it are read (of a packed body, unpacked).
#[cfg(feature = "http")]
fn body_within(
    response: &mut ureq::http::Response<ureq::Body>,
    packed: bool,
    limit: usize,
) -> Result<Option<Vec<u8>>, ureq::Error> {
    use std::io::Read;
    // The library's own limit counts the bytes on the wire and is not
    // set (`as_reader` has none): this one counts what the program
    // gets, and is the only one.
    let most = u64::try_from(limit).map_or(u64::MAX, |limit| limit.saturating_add(1));
    let mut body = Vec::new();
    let reader = response.body_mut().as_reader();
    match packed {
        true => Unpacked(flate2::read::MultiGzDecoder::new(reader))
            .take(most)
            .read_to_end(&mut body)?,
        false => reader.take(most).read_to_end(&mut body)?,
    };
    Ok((body.len() <= limit).then_some(body))
}

/// The unpacker of a gzip body over the reader of the transport
/// library. A failure of the library (the connection, a time) is given
/// as it is; any other is the unpacker's, an [`Unpacking`].
#[cfg(feature = "http")]
struct Unpacked<R>(flate2::read::MultiGzDecoder<R>);

#[cfg(feature = "http")]
impl<R: std::io::Read> std::io::Read for Unpacked<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf).map_err(|e| {
            match e.get_ref().is_some_and(|inner| inner.is::<ureq::Error>()) {
                true => e,
                false => std::io::Error::other(Unpacking(e)),
            }
        })
    }
}

/// A failure while a packed body is unpacked: of the connection under
/// it, or of the packed bytes.
#[cfg(feature = "http")]
#[derive(Debug)]
struct Unpacking(std::io::Error);

#[cfg(feature = "http")]
impl std::fmt::Display for Unpacking {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "gzip decompression failed: {}", self.0)
    }
}

#[cfg(feature = "http")]
impl std::error::Error for Unpacking {}

/// The headers `headers`, of a request or a response, as the map a
/// program gets: the values of a header that came more than once are
/// joined with ", ", in the order they came, as HTTP lets a list be
/// written in one header.
#[cfg(feature = "http")]
fn header_map(headers: impl IntoIterator<Item = (String, String)>) -> crate::value::Map {
    use std::collections::btree_map::Entry;
    let mut joined: BTreeMap<String, String> = BTreeMap::new();
    for (name, value) in headers {
        match joined.entry(name) {
            Entry::Vacant(entry) => {
                entry.insert(value);
            }
            Entry::Occupied(mut entry) => {
                let joined = entry.get_mut();
                joined.push_str(", ");
                joined.push_str(&value);
            }
        }
    }
    (joined.into_iter())
        .map(|(name, value)| (Value::string(name), Value::string(value)))
        .collect()
}

/// The `Response` of a response of the transport library, or `None`
/// if its body is larger than a body may be ([`wire::BODY_MAX`]).
/// `head` says that the request was `HEAD`.
#[cfg(feature = "http")]
pub(crate) fn ureq_response_to_value(
    mut response: ureq::http::Response<ureq::Body>,
    head: bool,
) -> Result<Option<Value>, ureq::Error> {
    let status = response.status().as_u16();
    // The names are in lower case, as the names of a request's
    // headers are (`http::HeaderName` holds them so); a value is text,
    // a byte that is no UTF-8 in it U+FFFD, as of a request's header.
    let headers: Vec<(String, String)> = (response.headers().iter())
        .map(|(name, value)| {
            let value = String::from_utf8_lossy(value.as_bytes());
            (name.as_str().to_string(), value.into_owned())
        })
        .collect();
    // A body is unpacked when its coding, all `Content-Encoding` headers
    // joined, is `gzip`, written exactly so (`gzip, gzip` sent as one
    // header or as two is handed over as it came). The program then
    // gets the content: `content-encoding` and `content-length`, which
    // describe the packed bytes, are not among its headers.
    let codings: Vec<&str> = (headers.iter())
        .filter(|(name, _)| name == "content-encoding")
        .map(|(_, value)| value.as_str())
        .collect();
    let packed = codings == ["gzip"];
    let headers = header_map(headers.into_iter().filter(|(name, _)| {
        !(packed && matches!(name.as_str(), "content-encoding" | "content-length"))
    }));
    // An answer to `HEAD`, and one whose status says so, has no body,
    // whatever its headers say of one (they describe what a `GET`
    // would get): nothing is read, and so nothing is unpacked that is
    // not there.
    let body = match head || !wire::has_body(status) {
        true => Some(Vec::new()),
        false => body_within(&mut response, packed, wire::BODY_MAX)?,
    };
    Ok(body.map(|body| make_http_response(status, headers, body)))
}

/// Scrub URL userinfo (`user:password@`) from an error message.
///
/// `ureq::Error`'s Display impl can include the request URL, and if
/// the caller embedded credentials in the URL (`https://user:tok@h`),
/// those credentials leak into the `Err` string that's handed back
/// to the silt program (F12). Replace with `***` whatever stands after a
/// `//`, of any scheme or of none (`ftp://u:p@h`, `htp://u:p@h`,
/// `//u:p@h`), up to the LAST `@` of that URL (it ends at a space, a tab
/// or a line feed): what the client sends as the user and password is
/// all before the last `@` of the authority, and a password with a raw
/// `/`, `?` or `#` in it ends the authority there, so its rest, and the
/// `@` after it, are in what reads as the path or the query
/// (`http://user:1234/ss@host/`). Such a password cannot be told from an
/// `@` in a path by its shape, so the text before any `@` of a URL is
/// hidden (`https://h/users/@me` is shown `https://***@me`): what is
/// hidden may be more than a password, never less. `scheme://@host`
/// stays as it is. One pass over the text. (A `url.Url` is shown with
/// exactly its user and password hidden: its `Display`. The client
/// sends no user or password in the URL it hands the library, so this
/// is a guard.)
#[cfg(feature = "http")]
pub fn redact_http_url_userinfo(msg: &str) -> String {
    redact_counting(msg, &mut 0)
}

/// [`redact_http_url_userinfo`], adding to `looked` the bytes it looked
/// at (what a test asks: no more than a few times the text's length).
#[cfg(feature = "http")]
fn redact_counting(msg: &str, looked: &mut usize) -> String {
    crate::text::hide_url_userinfo_counting(msg, looked)
}

/// The error of an `http` function whose operation has no value of
/// its own: `Timeout` when its deadline passed, `Other` with
/// the reason when it could not run or panicked.
#[cfg(feature = "http")]
fn http_timeout_err(failure: crate::vm::IoFailure<'_>) -> Value {
    use crate::vm::IoFailure;
    let error = match failure {
        IoFailure::Timeout(_) => Value::variant(bv::HTTP_TIMEOUT, []),
        IoFailure::Panicked(why) | IoFailure::Refused(why) => {
            Value::variant(bv::HTTP_OTHER, [Value::string(why)])
        }
    };
    Value::variant(bv::ERR, [error])
}

/// The `HttpError` of a failure of the transport library, for a
/// request to `url` (as a message shows it).
///
/// The library says what kind of failure it is (the variants of
/// `ureq::Error`, and for one of the socket the kind of the
/// `io::Error`), and that alone decides the variant: no text of a
/// message is looked at. The message is what the variant carries.
///
/// (A URL is a `url.Url`, read and checked before the library sees it:
/// [`do_http_request_on`] refuses one of another scheme as
/// `InvalidUrl`.)
/// - No connection was made (refused, no route, no such host):
///   `Connect(why)`.
/// - One of the request's times passed: `Timeout`.
/// - The other end went away before the response was complete, head
///   or body, packed or not: `ClosedEarly`.
/// - What the server sent is no HTTP (a status line that is none, a
///   chunk length that is no number, a head that is too large, a body
///   that is all there and does not unpack): `InvalidResponse(why)`. The library
///   keeps the kinds of these to itself (`Protocol` holds a type it
///   does not export), so the few of them that are about the request
///   and not the response (a header of the program's that the protocol
///   forbids) are `InvalidResponse` too.
/// - TLS: `Tls(why)`. A failed handshake reaches silt as a failure of
///   the socket with invalid data, around an error of the TLS library;
///   on an `https` URL that is `Tls`.
/// - Too many redirects: `TooManyRedirects` (the client follows them
///   itself, [`do_http_request_on`]).
/// - Anything else is `Other(why)`: a header name or value of the
///   program's that is none, any failure of the socket not named above.
#[cfg(feature = "http")]
fn http_error(error: &ureq::Error, url: &str) -> Value {
    use std::io::ErrorKind;
    use ureq::Error;

    // (Without the library's name of the kind, `io: `, `protocol: `,
    // which the variant says; a TLS reason in the program's words.)
    let text = match error {
        Error::Io(io) => io.to_string(),
        Error::Protocol(protocol) => protocol.to_string(),
        Error::Http(http) => http.to_string(),
        Error::Rustls(tls) => tls.to_string(),
        Error::Other(other) => other.to_string(),
        _ => error.to_string(),
    };
    let why = || vec![Value::string(redact_http_url_userinfo(&text))];
    let tls = || {
        let reason = super::common::tls_reason(&text, "the server");
        Value::variant(
            bv::HTTP_TLS,
            [Value::string(redact_http_url_userinfo(&reason))],
        )
    };
    let https = url
        .get(.."https://".len())
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"));
    let unpacking = match error {
        Error::Io(io) => io.get_ref().and_then(|e| e.downcast_ref::<Unpacking>()),
        _ => None,
    };
    if let Some(unpacking) = unpacking {
        let under = &unpacking.0;
        let why = || {
            vec![Value::string(redact_http_url_userinfo(
                &unpacking.to_string(),
            ))]
        };
        return match under.kind() {
            ErrorKind::ConnectionReset
            | ErrorKind::ConnectionAborted
            | ErrorKind::BrokenPipe
            | ErrorKind::NotConnected => Value::variant(bv::HTTP_CLOSED_EARLY, []),
            // Two things end a packed body before its end, and both are
            // this kind: the connection, which the library reports
            // with an error of its own inside, and packed bytes that
            // stop short in a body that is all there, which the
            // unpacker reports as the bare kind.
            ErrorKind::UnexpectedEof if under.get_ref().is_some() => {
                Value::variant(bv::HTTP_CLOSED_EARLY, [])
            }
            _ => Value::variant(bv::HTTP_INVALID_RESPONSE, why()),
        };
    }
    match error {
        Error::Timeout(_) => Value::variant(bv::HTTP_TIMEOUT, []),
        // The name of the host was not found ([`Resolving`]).
        Error::Io(io) if io.get_ref().is_some_and(|e| e.is::<NoSuchHost>()) => {
            Value::variant(bv::HTTP_CONNECT, why())
        }
        Error::Io(io) => match io.kind() {
            ErrorKind::TimedOut | ErrorKind::WouldBlock => Value::variant(bv::HTTP_TIMEOUT, []),
            ErrorKind::ConnectionRefused
            | ErrorKind::AddrNotAvailable
            | ErrorKind::NetworkUnreachable
            | ErrorKind::HostUnreachable
            | ErrorKind::NetworkDown => Value::variant(bv::HTTP_CONNECT, why()),
            ErrorKind::UnexpectedEof
            | ErrorKind::ConnectionReset
            | ErrorKind::ConnectionAborted
            | ErrorKind::BrokenPipe
            | ErrorKind::NotConnected => Value::variant(bv::HTTP_CLOSED_EARLY, []),
            ErrorKind::InvalidData if https => tls(),
            _ => Value::variant(bv::HTTP_OTHER, why()),
        },
        Error::HostNotFound | Error::ConnectionFailed | Error::ConnectProxyFailed(_) => {
            Value::variant(bv::HTTP_CONNECT, why())
        }
        Error::Protocol(_) | Error::LargeResponseHeader(..) => {
            Value::variant(bv::HTTP_INVALID_RESPONSE, why())
        }
        Error::Tls(_) | Error::Rustls(_) | Error::Pem(_) | Error::TlsRequired => tls(),
        Error::TooManyRedirects => Value::variant(bv::HTTP_TOO_MANY_REDIRECTS, []),
        // (A status is no failure: silt asks the library for it as a
        // response, and the library's error for one is never made.)
        _ => Value::variant(bv::HTTP_OTHER, why()),
    }
}

/// What a request of the transport library came to, as the value of
/// `http.get` and `http.request`: `Ok(Response)`, or `Err(HttpError)`
/// for a failure of the request or of the read of the body
/// ([`http_error`]), and for a body that is larger than a body may be:
/// `InvalidResponse`, as for a head that is too large. `head` says
/// that the request was `HEAD`.
#[cfg(feature = "http")]
fn finish_http_response(
    result: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    url: &str,
    head: bool,
) -> Value {
    match result.and_then(|response| ureq_response_to_value(response, head)) {
        Ok(Some(response)) => Value::variant(bv::OK, [response]),
        Ok(None) => {
            let why = format!("the body is larger than {} bytes", wire::BODY_MAX);
            let error = Value::variant(bv::HTTP_INVALID_RESPONSE, [Value::string(why)]);
            Value::variant(bv::ERR, [error])
        }
        Err(error) => Value::variant(bv::ERR, [http_error(&error, url)]),
    }
}

/// The one client of the program: every `http.get`, `http.request` and
/// `http.request_once` goes through it, so that a connection to a server
/// that is kept after a response (keep-alive) serves the next request to
/// that server. An idle connection is kept for 15 s, at most 3 to one
/// server and 10 in all (ureq's pool). Security: conservative default
/// timeouts so a slow/unreachable peer cannot hang the underlying OS
/// socket indefinitely (HIGH-3). These apply even if the silt task's
/// SILT_IO_TIMEOUT unblocks the VM task, so we don't leak real file
/// descriptors.
#[cfg(feature = "http")]
fn client() -> &'static ureq::Agent {
    static CLIENT: std::sync::LazyLock<ureq::Agent> = std::sync::LazyLock::new(|| {
        client_with_times(Duration::from_secs(10), Duration::from_secs(60))
    });
    &CLIENT
}

/// One call of `http.get`, `http.request` or `http.request_once`: whether
/// it was stopped ([`Call::stop`]), and the attempt it makes now. The
/// call is the one of the thread it runs on ([`in_call`]): the
/// connector and the connections of the client, which ureq calls on that
/// thread, find it there.
#[cfg(feature = "http")]
#[derive(Default)]
struct Call {
    stopped: AtomicBool,
    now: Mutex<Attempt>,
}

/// One request of a call on the wire: each redirect it follows is one,
/// and so is a request sent again after a kept connection the server had
/// closed ([`send_once`]).
#[cfg(feature = "http")]
#[derive(Default)]
struct Attempt {
    /// Its number, one of its own in the process (0: none begun).
    id: u64,
    /// The connection it is sent on, once it has one.
    line: Option<Arc<Line>>,
    /// That connection served a request before: it was kept.
    reused: bool,
    /// A byte of the response has come.
    answered: bool,
}

/// The numbers of the attempts ([`Attempt::id`]).
#[cfg(feature = "http")]
static ATTEMPTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// The socket of a connection of the client, as a stop shuts it
/// ([`Shutter`]), and the attempt that uses the connection: a stop of
/// a call shuts only a connection that its own attempt uses, never one
/// that went back to the client and serves another call.
///
/// [`Shutter`]: crate::runtime::handle::Shutter
#[cfg(feature = "http")]
#[derive(Default)]
struct Line {
    shutter: crate::runtime::handle::Shutter,
    user: Mutex<u64>,
}

#[cfg(feature = "http")]
thread_local! {
    /// The call that runs on this thread ([`in_call`]).
    static CALL: std::cell::RefCell<Option<Arc<Call>>> = const { std::cell::RefCell::new(None) };
}

/// Runs `run` as the call `call` of this thread, and lets go of the
/// call's connection after it: a stop that comes later ends nothing.
#[cfg(feature = "http")]
fn in_call<T>(call: &Arc<Call>, run: impl FnOnce() -> T) -> T {
    struct Restore(Option<Arc<Call>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let before = self.0.take();
            if let Some(call) = CALL.with(|now| now.replace(before)) {
                call.now.lock().line = None;
            }
        }
    }
    let _restore = Restore(CALL.with(|now| now.replace(Some(call.clone()))));
    run()
}

/// Does `act` with the call of this thread, if there is one (a test may
/// use a client of its own without one).
#[cfg(feature = "http")]
fn with_call<T>(act: impl FnOnce(&Call) -> T) -> Option<T> {
    CALL.with(|now| now.borrow().as_deref().map(act))
}

#[cfg(feature = "http")]
impl Call {
    /// A new attempt begins: it has no connection yet.
    fn begin(&self) {
        let id = ATTEMPTS.fetch_add(1, Ordering::Relaxed);
        *self.now.lock() = Attempt {
            id,
            ..Attempt::default()
        };
    }

    /// The present attempt uses the connection `line`, which `served`
    /// (the attempt it served last, 0 for none). `NotConnected` if the
    /// call was stopped: the connection is shut then.
    fn enlist(&self, line: &Arc<Line>, served: &mut u64) -> std::io::Result<()> {
        let mut now = self.now.lock();
        if now.id == *served {
            return Ok(());
        }
        now.reused = *served != 0;
        now.answered = false;
        *served = now.id;
        *line.user.lock() = now.id;
        now.line = Some(line.clone());
        if self.stopped.load(Ordering::SeqCst) {
            line.shutter.shut();
            return Err(std::io::ErrorKind::NotConnected.into());
        }
        Ok(())
    }

    /// Whether the present attempt went out on a kept connection that
    /// ended before a byte of the response came: the server had closed
    /// it while it was idle, and the request may be sent again.
    fn met_a_closed_connection(&self) -> bool {
        let now = self.now.lock();
        now.reused && !now.answered && !self.stopped.load(Ordering::SeqCst)
    }

    /// What ends this call when nobody waits for it any more
    /// (`Leave::Closes`): the connection its attempt uses is shut, which
    /// ends a read, a write or a TLS handshake at once, and so is the
    /// socket of its connect; no connect, request or redirect begins after
    /// it. `false` while a connect is under way: whether a shutdown ends
    /// it depends on the system (Linux: it does), and the operation counts
    /// among those that cannot be interrupted until the connect has
    /// returned (at once, on Linux).
    fn stop(self: &Arc<Self>) -> impl FnOnce() -> bool + Send + 'static {
        let call = self.clone();
        move || {
            call.stopped.store(true, Ordering::SeqCst);
            let now = call.now.lock();
            let Some(line) = &now.line else {
                return true;
            };
            let user = line.user.lock();
            let ended = !line.shutter.is_connecting();
            if *user == now.id {
                line.shutter.shut();
            }
            ended
        }
    }
}

/// The TCP connector of the http client: in place of ureq's, it makes the
/// connection with `tcp::connect_shuttered`, so that a request given up
/// while its connect waits is ended there, and hands ureq the connected
/// socket ([`Connected`]). What it does besides is what ureq's (3.5)
/// does: each resolved address in turn with its share of the connect's
/// time ([`connect_each`]), `TCP_NODELAY` as configured. The new
/// connection is the one of the attempt of the thread's call ([`Call`]).
#[cfg(feature = "http")]
#[derive(Debug)]
struct Connecting;

#[cfg(feature = "http")]
impl<In: ureq::unversioned::transport::Transport> ureq::unversioned::transport::Connector<In>
    for Connecting
{
    type Out = ureq::unversioned::transport::Either<In, Connected>;

    fn connect(
        &self,
        details: &ureq::unversioned::transport::ConnectionDetails,
        chained: Option<In>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        use ureq::unversioned::transport::Either;
        // A connection made before (a proxy's) is used as it is.
        if chained.is_some() {
            return Ok(chained.map(Either::A));
        }
        let line = Arc::new(Line::default());
        let mut served = 0;
        // (A connect that would begin after the stop does not begin.)
        with_call(|call| call.enlist(&line, &mut served)).transpose()?;
        let stream = connect_each(&details.addrs, details.timeout, |addr, time| {
            super::tcp::connect_shuttered_to(addr, &line.shutter, time)
        })?;
        if details.config.no_delay() {
            stream.set_nodelay(true)?;
        }
        let config = details.config;
        let transport = Connected {
            stream: Arc::new(stream),
            buffers: ureq::unversioned::transport::LazyBuffers::new(
                config.input_buffer_size(),
                config.output_buffer_size(),
            ),
            timeout_read: None,
            timeout_write: None,
            line,
            served,
        };
        Ok(Some(Either::B(transport)))
    }
}

/// The resolved addresses `addrs` tried in turn with `connect`, as
/// ureq's TCP connector tries them: each has a share of the connect's
/// time `timeout`, the first half as much again as the next (with two
/// addresses and 10 s, 6.7 s and 3.3 s; one address has all of it; no
/// share is under 10 ms; after an address that refused, the next has
/// the same share). The next address is tried after a connect
/// that ran out of its share while the connect's time is not over, and
/// after a failure that concerns only that address
/// ([`only_this_address`]); any other failure ends the connect. When
/// every address failed, the last failure is the connect's.
#[cfg(feature = "http")]
fn connect_each<T>(
    addrs: &[std::net::SocketAddr],
    timeout: ureq::unversioned::transport::NextTimeout,
    mut connect: impl FnMut(std::net::SocketAddr, Option<Duration>) -> std::io::Result<T>,
) -> Result<T, ureq::Error> {
    const LEAST: Duration = Duration::from_millis(10);
    let started = std::time::Instant::now();
    let total_weight = 2.0 * (1.0 - 0.5_f64.powi(addrs.len() as i32));
    let mut weight = 1.0_f64;
    let mut last = None;
    for addr in addrs {
        let share = timeout.not_zero().map(|t| {
            let secs = t.as_secs_f64() * weight / total_weight;
            Duration::from_millis((secs * 1000.0) as u64).max(LEAST)
        });
        match connect(*addr, share) {
            Ok(connected) => return Ok(connected),
            Err(e) if only_this_address(&e) => {
                last = Some(ureq::Error::Io(e));
                continue;
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                if started.elapsed() > *timeout.after {
                    return Err(ureq::Error::Timeout(timeout.reason));
                }
                last = Some(ureq::Error::Timeout(ureq::Timeout::Connect));
            }
            Err(e) => return Err(ureq::Error::Io(e)),
        }
        weight /= 2.0;
    }
    Err(last.unwrap_or_else(|| {
        ureq::Error::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "Connection refused",
        ))
    }))
}

/// Whether a connect failed for a reason of the address tried only, so
/// that the next resolved address may still answer, as ureq decides it:
/// the address refused, or the system has no route to it (a host whose
/// first addresses are IPv6 where no IPv6 route is). On Windows, a VPN
/// or firewall that blocks an address family says `WSAEACCES`.
#[cfg(feature = "http")]
fn only_this_address(e: &std::io::Error) -> bool {
    #[cfg(windows)]
    if e.raw_os_error() == Some(10013) {
        return true;
    }
    matches!(
        e.kind(),
        std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::HostUnreachable
            | std::io::ErrorKind::NetworkUnreachable
            | std::io::ErrorKind::NetworkDown
            | std::io::ErrorKind::AddrNotAvailable
    )
}

/// The name lookup of the http client: its resolver's (ureq's, or a
/// test's), with a lookup that fails marked ([`NoSuchHost`]), so that
/// [`http_error`] makes it `Connect`, as `tcp.connect` does. The system
/// says such a failure in a kind of error that names nothing (on Linux
/// "failed to lookup address information"), and ureq hands it on as it
/// is.
#[cfg(feature = "http")]
#[derive(Debug)]
struct Resolving<R>(R);

#[cfg(feature = "http")]
impl<R: ureq::unversioned::resolver::Resolver> ureq::unversioned::resolver::Resolver
    for Resolving<R>
{
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        config: &ureq::config::Config,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
        self.0.resolve(uri, config, timeout).map_err(|e| match e {
            ureq::Error::Io(e) => ureq::Error::Io(std::io::Error::new(e.kind(), NoSuchHost(e))),
            e => e,
        })
    }
}

/// A failure of the name lookup of the http client ([`Resolving`]): its
/// words are the system's.
#[cfg(feature = "http")]
#[derive(Debug)]
struct NoSuchHost(std::io::Error);

#[cfg(feature = "http")]
impl std::fmt::Display for NoSuchHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(feature = "http")]
impl std::error::Error for NoSuchHost {}

/// A connection the http client made itself ([`Connecting`]), as ureq's
/// transport for TCP is: its buffers, the socket, and the read and write
/// timeouts it has (set only when they change). Besides, what a stop
/// shuts ([`Line`]) and the attempt it served last (0: none yet): each
/// read and write, and the check of a kept connection that a call takes
/// from the pool (`is_open`), enlists it in the attempt of the thread's call
/// ([`Call::enlist`]), which so knows the connection it uses and
/// whether that was kept.
#[cfg(feature = "http")]
struct Connected {
    stream: Arc<std::net::TcpStream>,
    buffers: ureq::unversioned::transport::LazyBuffers,
    timeout_read: Option<Duration>,
    timeout_write: Option<Duration>,
    line: Arc<Line>,
    served: u64,
}

#[cfg(feature = "http")]
impl std::fmt::Debug for Connected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connected")
            .field("peer", &self.stream.peer_addr().ok())
            .finish()
    }
}

/// The time of one read or write, or its end: an `Err` when none is left.
#[cfg(feature = "http")]
fn time_of(
    timeout: ureq::unversioned::transport::NextTimeout,
) -> Result<Option<std::time::Duration>, ureq::Error> {
    if timeout.after.is_zero() {
        return Err(ureq::Error::Timeout(timeout.reason));
    }
    Ok(timeout.not_zero().map(|t| *t))
}

/// Gives the socket the timeout `time` with `set`, unless it has it
/// (`had`), as ureq does. macOS may refuse a change of a timeout after
/// the peer closed with request data unread; the answer may still be
/// read, so the read or write says what happened, not the refusal.
#[cfg(feature = "http")]
fn set_timeout(
    had: &mut Option<Duration>,
    time: Option<Duration>,
    set: impl FnOnce(Option<Duration>) -> std::io::Result<()>,
) -> std::io::Result<()> {
    if *had == time {
        return Ok(());
    }
    match set(time) {
        Ok(()) => *had = time,
        Err(e) if cfg!(target_os = "macos") && e.kind() == std::io::ErrorKind::InvalidInput => {}
        Err(e) => return Err(e),
    }
    Ok(())
}

/// An I/O error of the socket, with a timeout as ureq's timeout.
#[cfg(feature = "http")]
fn io_of(e: std::io::Error, timeout: &ureq::unversioned::transport::NextTimeout) -> ureq::Error {
    match e.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => {
            ureq::Error::Timeout(timeout.reason)
        }
        _ => ureq::Error::Io(e),
    }
}

#[cfg(feature = "http")]
impl Connected {
    /// Enlists the connection in the attempt of the thread's call.
    fn enlist(&mut self) -> std::io::Result<()> {
        let (line, served) = (&self.line, &mut self.served);
        with_call(|call| call.enlist(line, served)).unwrap_or(Ok(()))
    }
}

#[cfg(feature = "http")]
impl ureq::unversioned::transport::Transport for Connected {
    fn buffers(&mut self) -> &mut dyn ureq::unversioned::transport::Buffers {
        &mut self.buffers
    }

    fn transmit_output(
        &mut self,
        amount: usize,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<(), ureq::Error> {
        use std::io::Write;
        use ureq::unversioned::transport::Buffers;
        self.enlist()?;
        let timeout = timeout.for_write();
        let stream = &self.stream;
        set_timeout(&mut self.timeout_write, time_of(timeout)?, |t| {
            stream.set_write_timeout(t)
        })?;
        let output = &self.buffers.output()[..amount];
        (&*self.stream)
            .write_all(output)
            .map_err(|e| io_of(e, &timeout))
    }

    fn await_input(
        &mut self,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<bool, ureq::Error> {
        use std::io::Read;
        use ureq::unversioned::transport::Buffers;
        self.enlist()?;
        let timeout = timeout.for_read();
        let time = time_of(timeout)?;
        let stream = &self.stream;
        set_timeout(&mut self.timeout_read, time, |t| stream.set_read_timeout(t))?;
        let input = self.buffers.input_append_buf();
        let started = std::time::Instant::now();
        // A read a signal interrupted is read again within what is left
        // of its time (Linux does not restart it); without a time, the
        // interruption is the read's.
        let read = loop {
            match (&**stream).read(input) {
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                    let Some(time) = time else {
                        break Err(e);
                    };
                    let left = time.saturating_sub(started.elapsed());
                    if left.is_zero() {
                        break Err(std::io::ErrorKind::TimedOut.into());
                    }
                    set_timeout(&mut self.timeout_read, Some(left), |t| {
                        stream.set_read_timeout(t)
                    })?;
                }
                read => break read,
            }
        };
        let read = read.map_err(|e| io_of(e, &timeout))?;
        self.buffers.input_appended(read);
        Ok(read > 0)
    }

    fn is_open(&mut self) -> bool {
        use std::io::Read;
        // ureq asks this of a kept connection when a call takes it from
        // the pool, on that call's thread: the call uses it from now on,
        // so that a stop of the call that used it before shuts nothing
        // of it. (A stopped call gets no connection.)
        if self.enlist().is_err() {
            return false;
        }
        // Nothing may wait to be read on a connection that is kept: a read
        // that would wait says it is open.
        if self.stream.set_nonblocking(true).is_err() {
            return false;
        }
        let mut byte = [0u8; 1];
        let open = matches!(
            (&*self.stream).read(&mut byte),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock
        );
        open && self.stream.set_nonblocking(false).is_ok()
    }
}

/// A client whose times to connect and in all are the given ones. Every
/// request says `user-agent: silt/<version>` and `accept-encoding:
/// gzip`, unless the program sets that header itself: ureq sends the
/// request's header then. Its connector is ureq's default (a CONNECT
/// proxy, TCP, TLS) with its own TCP connector ([`Connecting`]), and its
/// resolver ureq's, with a failed lookup marked ([`Resolving`]).
#[cfg(feature = "http")]
fn client_with_times(connect: Duration, global: Duration) -> ureq::Agent {
    client_resolving(
        connect,
        global,
        ureq::unversioned::resolver::DefaultResolver::default(),
    )
}

/// A client as [`client_with_times`] makes it, whose host names
/// `resolver` resolves.
#[cfg(feature = "http")]
fn client_resolving(
    connect: Duration,
    global: Duration,
    resolver: impl ureq::unversioned::resolver::Resolver,
) -> ureq::Agent {
    client_of(connect, global, resolver, ureq::tls::TlsConfig::default())
}

/// A client as [`client_resolving`] makes it, whose TLS is `tls`. On top
/// of its connections, which may be TLS, it notes when the answer begins
/// ([`Answering`]).
#[cfg(feature = "http")]
fn client_of(
    connect: Duration,
    global: Duration,
    resolver: impl ureq::unversioned::resolver::Resolver,
    tls: ureq::tls::TlsConfig,
) -> ureq::Agent {
    use ureq::unversioned::transport::{ConnectProxyConnector, Connector, RustlsConnector};
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(connect))
        .timeout_global(Some(global))
        .user_agent(format!("silt/{}", crate::VERSION))
        .accept_encoding("gzip")
        .tls_config(tls)
        .build();
    let connector =
        ().chain(ConnectProxyConnector::default())
            .chain(Connecting)
            .chain(RustlsConnector::default())
            .chain(Answering);
    ureq::Agent::with_parts(config, connector, Resolving(resolver))
}

/// The top of the connector of the http client: it hands up the
/// connection made below it (TCP, and TLS on it for `https`), noting in
/// the attempt of the thread's call when a byte of the answer comes
/// ([`Answered`]).
#[cfg(feature = "http")]
#[derive(Debug)]
struct Answering;

#[cfg(feature = "http")]
impl<In: ureq::unversioned::transport::Transport> ureq::unversioned::transport::Connector<In>
    for Answering
{
    type Out = Answered<In>;

    fn connect(
        &self,
        _details: &ureq::unversioned::transport::ConnectionDetails,
        chained: Option<In>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        Ok(chained.map(Answered))
    }
}

/// A connection of the client as ureq reads the answer from it: what it
/// reads is the answer itself, above TLS. A byte of it that comes says
/// so in the attempt of the thread's call ([`Attempt::answered`]); the
/// records of TLS below it do not (a server that closes a kept
/// connection sends one, `close_notify`, and no answer).
#[cfg(feature = "http")]
#[derive(Debug)]
struct Answered<T>(T);

#[cfg(feature = "http")]
impl<T: ureq::unversioned::transport::Transport> ureq::unversioned::transport::Transport
    for Answered<T>
{
    fn buffers(&mut self) -> &mut dyn ureq::unversioned::transport::Buffers {
        self.0.buffers()
    }

    fn transmit_output(
        &mut self,
        amount: usize,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<(), ureq::Error> {
        self.0.transmit_output(amount, timeout)
    }

    fn await_input(
        &mut self,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<bool, ureq::Error> {
        let came = self.0.await_input(timeout)?;
        if came {
            with_call(|call| call.now.lock().answered = true);
        }
        Ok(came)
    }

    fn is_open(&mut self) -> bool {
        self.0.is_open()
    }

    fn is_tls(&self) -> bool {
        self.0.is_tls()
    }
}

/// Test-only: an HTTP GET as `http.get` makes it, with a client of its
/// own.
#[cfg(all(test, feature = "http"))]
fn do_http_get(url: &str) -> Value {
    do_http_request("GET", url, &[], &[])
}

/// Test-only: the Url of `text`, which is one.
#[cfg(all(test, feature = "http"))]
fn url_of(text: &str) -> Url {
    crate::url::parse(text).unwrap_or_else(|e| panic!("{text}: {e}"))
}

/// How many redirects `http.get` and `http.request` follow, as many as
/// ureq would.
const REDIRECTS_MAX: usize = 10;

/// Test-only: a request as `http.request` makes it, with a client of
/// its own.
#[cfg(all(test, feature = "http"))]
fn do_http_request(
    method_tag: &str,
    url: &str,
    body: &[u8],
    headers: &[(String, String)],
) -> Value {
    let agent = client_with_times(Duration::from_secs(10), Duration::from_secs(60));
    let call = Arc::new(Call::default());
    do_http_request_on(&agent, &call, method_tag, &url_of(url), body, headers, true)
}

/// Perform a synchronous HTTP request with the client `agent` as the
/// call `call`, and return a `Value`. It follows the redirects it gets
/// if `follow` (`http.get`, `http.request`); otherwise a redirect is
/// the response (`http.request_once`).
///
/// The request follows redirects itself, because ureq does not send a
/// body again: a 303 goes on as a GET without a body (a HEAD stays a
/// HEAD) and without the program's `content-type` header, and so does
/// a POST that gets a 301 or 302; any other request goes on with the
/// same method and body. As ureq does, a redirect drops the
/// `authorization`, `proxy-authorization` and `cookie` headers, and the
/// user and password of the URL, which are sent as `authorization`
/// (decoded, [`basic_of_url`]) where the program sets none, never in the
/// URL. A `content-length` header of the program's is not sent: the
/// body's own length is (an empty body of a method that takes none by
/// default has none).
///
/// A stop of `call` ([`Call::stop`], from another thread) ends the
/// request: a read or a write it waits in, a TLS handshake, the next
/// redirect. A connect under way runs on to its end (at most the connect
/// time of [`client`]), and the request ends after it.
#[cfg(feature = "http")]
fn do_http_request_on(
    agent: &ureq::Agent,
    call: &Arc<Call>,
    method_tag: &str,
    url: &Url,
    body: &[u8],
    headers: &[(String, String)],
    follow: bool,
) -> Value {
    in_call(call, || {
        request_hops(agent, call, method_tag, url, body, headers, follow)
    })
}

/// [`do_http_request_on`] in its call.
#[cfg(feature = "http")]
fn request_hops(
    agent: &ureq::Agent,
    call: &Call,
    method_tag: &str,
    url: &Url,
    body: &[u8],
    headers: &[(String, String)],
    follow: bool,
) -> Value {
    if !matches!(
        method_tag,
        "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS"
    ) {
        return invalid_url(format!("unknown method: {method_tag}"));
    }
    let mut method = method_tag;
    let mut url = url.clone();
    let mut body = body;
    // How the body goes and what becomes of the connection are the
    // client's to say: the body is sent whole, with its length.
    let framing = ["content-length", "transfer-encoding", "connection"];
    let mut headers: Vec<(&str, &str)> = headers
        .iter()
        .filter(|(key, _)| !framing.iter().any(|own| key.eq_ignore_ascii_case(own)))
        .map(|(key, val)| (key.as_str(), val.as_str()))
        .collect();
    for _ in 0..=REDIRECTS_MAX {
        // (A Url of another scheme is a valid URL, and no URL of http's.)
        if !matches!(url.scheme(), "http" | "https") {
            return invalid_url(url.to_string());
        }
        // The user and password of the URL, when the program set no
        // `authorization` itself.
        let own = headers
            .iter()
            .any(|(key, _)| key.eq_ignore_ascii_case("authorization"));
        let basic = match own {
            true => None,
            false => match basic_of_url(&url) {
                Ok(basic) => basic,
                Err(()) => return invalid_url(url.to_string()),
            },
        };
        let mut sent = headers.clone();
        if let Some(basic) = &basic {
            sent.push(("authorization", basic));
        }
        // Without its user and password, which are the authorization
        // above or none, and without its fragment, which is no server's.
        let sent_url = url.with_user("").with_password("").with_fragment("");
        if sent_url.as_str().len() > URL_MAX {
            return invalid_url(cut_short(&url.to_string()));
        }
        let result = send_once(agent, call, method, sent_url.as_str(), body, &sent);
        let shown = url.to_string();
        if !follow {
            return finish_http_response(result, &shown, method == "HEAD");
        }
        let Some((status, next)) = result.as_ref().ok().and_then(|r| redirect(r, &url)) else {
            return finish_http_response(result, &shown, method == "HEAD");
        };
        let next = match next {
            Ok(next) => next,
            Err(location) => return invalid_url(location),
        };
        // A redirect to another scheme is not followed: `file:`, `ftp:`
        // and the like are no URL of http's.
        if !matches!(next.scheme(), "http" | "https") {
            return invalid_url(next.to_string());
        }
        // A 303 goes on without a body, as a GET (a HEAD stays a HEAD);
        // a 301 or 302 so only for a POST (RFC 9110); a 307 or 308
        // keeps the request as it is.
        let bodiless = match status {
            303 => true,
            301 | 302 => method == "POST",
            _ => false,
        };
        if bodiless {
            if method != "HEAD" {
                method = "GET";
            }
            body = &[];
            // (What the body was is no part of a request without one.)
            headers.retain(|(key, _)| !key.eq_ignore_ascii_case("content-type"));
        }
        // The credentials do not go on: to the same host neither.
        headers.retain(|(key, _)| {
            !["authorization", "proxy-authorization", "cookie"]
                .iter()
                .any(|credential| key.eq_ignore_ascii_case(credential))
        });
        if let Ok(mut response) = result {
            drain(&mut response);
        }
        url = next;
    }
    Value::variant(bv::ERR, [Value::variant(bv::HTTP_TOO_MANY_REDIRECTS, [])])
}

/// Reads the body of a redirect that is followed, when it says it is
/// short (its `content-length` at most 64 KiB), so that its connection
/// is kept for the next request; a body of another length closes it.
#[cfg(feature = "http")]
fn drain(response: &mut ureq::http::Response<ureq::Body>) {
    let short = response
        .headers()
        .get("content-length")
        .and_then(|length| length.to_str().ok())
        .and_then(|length| length.trim().parse::<u64>().ok())
        .is_some_and(|length| length <= 64 * 1024);
    if short {
        let _ = std::io::copy(&mut response.body_mut().as_reader(), &mut std::io::sink());
    }
}

/// Sends one request of `call` ([`send_request`]) as a new attempt. A
/// request of a method that may be sent twice (GET, HEAD, PUT, DELETE,
/// OPTIONS: idempotent, RFC 9110) that went out on a kept connection
/// which ended before a byte of the response came is sent once more, on
/// a new connection: the server had closed the connection while it was
/// idle, and the client saw it too late. A POST or a PATCH is not: the
/// server may have acted on it before it closed.
#[cfg(feature = "http")]
fn send_once(
    agent: &ureq::Agent,
    call: &Call,
    method: &str,
    url: &str,
    body: &[u8],
    headers: &[(&str, &str)],
) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    call.begin();
    let result = send_request(agent, method, url, body, headers, false);
    let idempotent = matches!(method, "GET" | "HEAD" | "PUT" | "DELETE" | "OPTIONS");
    match &result {
        Err(e) if idempotent && ended_unanswered(e) && call.met_a_closed_connection() => {
            call.begin();
            send_request(agent, method, url, body, headers, true)
        }
        _ => result,
    }
}

/// Whether `error` is the end of the connection (it was closed, or
/// reset).
#[cfg(feature = "http")]
fn ended_unanswered(error: &ureq::Error) -> bool {
    use std::io::ErrorKind;
    matches!(
        error,
        ureq::Error::Io(io) if matches!(
            io.kind(),
            ErrorKind::UnexpectedEof
                | ErrorKind::ConnectionReset
                | ErrorKind::ConnectionAborted
                | ErrorKind::BrokenPipe
        )
    )
}

/// The longest URL the transport sends: its reader of URLs takes no
/// longer one.
#[cfg(feature = "http")]
const URL_MAX: usize = 65_534;

/// A URL too long to send, as an `InvalidUrl` shows it: its first 200
/// characters and how long it is, not 65 KB of text.
#[cfg(feature = "http")]
fn cut_short(shown: &str) -> String {
    match shown.char_indices().nth(200) {
        Some((at, _)) => format!("{}... ({} bytes)", &shown[..at], shown.len()),
        None => shown.to_string(),
    }
}

/// `Err(InvalidUrl(text))`.
#[cfg(feature = "http")]
fn invalid_url(text: String) -> Value {
    Value::variant(
        bv::ERR,
        [Value::variant(bv::HTTP_INVALID_URL, [Value::string(text)])],
    )
}
/// The `authorization` header of the user and password of `url`:
/// `Basic` with `user:password`, each decoded (RFC 3986 writes a `@`,
/// `:`, `/` or a space in them escaped, RFC 7617 sends them as they
/// are). `Ok(None)` for a URL without them. `Err(())` for a user that
/// has a colon once decoded (`us%3Aer`): RFC 7617 has none in it, and a
/// server would read the user up to it and the rest as the password.
#[cfg(feature = "http")]
fn basic_of_url(url: &Url) -> Result<Option<String>, ()> {
    use base64::Engine;
    let (user, password) = (url.user(), url.password());
    if user.is_empty() && password.is_empty() {
        return Ok(None);
    }
    if user.contains(':') {
        return Err(());
    }
    Ok(Some(format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
    )))
}
/// Send one request of `http.request`, following no redirect. Every
/// method sends the body, with its length; an empty body sends
/// nothing, not even a length for the methods that take none by
/// default (GET, DELETE, HEAD, OPTIONS: ureq makes them take one with
/// `force_send_body`). A `new` request takes no kept connection: it
/// makes one (and the kept connections to its server are let go of).
#[cfg(feature = "http")]
fn send_request(
    agent: &ureq::Agent,
    method: &str,
    url: &str,
    body: &[u8],
    headers: &[(&str, &str)],
    new: bool,
) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    fn headed<B>(
        mut req: ureq::RequestBuilder<B>,
        headers: &[(&str, &str)],
        new: bool,
    ) -> ureq::RequestBuilder<B> {
        for (key, val) in headers {
            req = req.header(*key, *val);
        }
        let config = req.config().max_redirects(0);
        match new {
            true => config.max_idle_age(Duration::ZERO).build(),
            false => config.build(),
        }
    }
    let with_body = |req: ureq::RequestBuilder<ureq::typestate::WithBody>| {
        let req = headed(req, headers, new);
        if body.is_empty() {
            req.send_empty()
        } else {
            req.send(body)
        }
    };
    let without_body = |req: ureq::RequestBuilder<ureq::typestate::WithoutBody>| {
        let req = headed(req, headers, new);
        if body.is_empty() {
            req.call()
        } else {
            req.force_send_body().send(body)
        }
    };
    match method {
        "POST" => with_body(agent.post(url)),
        "PUT" => with_body(agent.put(url)),
        "PATCH" => with_body(agent.patch(url)),
        "GET" => without_body(agent.get(url)),
        "DELETE" => without_body(agent.delete(url)),
        "HEAD" => without_body(agent.head(url)),
        _ => without_body(agent.options(url)),
    }
}

/// The status of a redirect and where it goes, `url` being where the
/// response came from; `None` for any other response, and for a
/// redirect without a location. A byte of the location that is not
/// printable ASCII (a space, the bytes of `é`, which a URL cannot hold)
/// is escaped first, as browsers and curl do (`%20`, `%C3%A9`); then it
/// is resolved against `url` without its user and password, which are
/// not sent on (`url.resolve`), and without a fragment, which is never
/// sent. A location that does not resolve is `Err` of its text, with what
/// may be a user and password in it hidden.
#[cfg(feature = "http")]
pub(crate) fn redirect(
    response: &ureq::http::Response<ureq::Body>,
    url: &Url,
) -> Option<(u16, Result<Url, String>)> {
    use std::fmt::Write;
    let status = response.status().as_u16();
    if !matches!(status, 301 | 302 | 303 | 307 | 308) {
        return None;
    }
    let location = response.headers().get("location")?.as_bytes().trim_ascii();
    let mut text = String::with_capacity(location.len());
    for &byte in location {
        match byte {
            b'!'..=b'~' => text.push(byte as char),
            _ => {
                let _ = write!(text, "%{byte:02X}");
            }
        }
    }
    let base = url.with_user("").with_password("");
    let next = crate::url::resolve(&base, &text)
        .map(|next| next.with_fragment(""))
        .map_err(|_| redact_http_url_userinfo(&text));
    Some((status, next))
}
// ── The server ──────────────────────────────────────────────────────
//
// `http.serve` is a frame of the task that calls it ([`Serve`]): it
// accepts on the listener as `tcp.accept` does, with the same
// operation, and starts a task for each connection ([`Conn`]). That
// task reads a request, calls the handler with it, writes what the
// handler returns, and goes on with the next request of the
// connection. Every accept, read and write is an operation of the I/O
// pool that its task waits for, with a deadline where the server has
// a limit: no thread is the server's, and one that waits for nothing
// costs nothing. The bytes are read and written by `crate::http_wire`,
// where every limit of the server is.

#[cfg(feature = "http")]
use crate::http_wire as wire;

/// What an `http.serve` and the tasks of its connections share.
#[cfg(feature = "http")]
struct Server {
    handler: Value,
    /// Whose the tasks of the connections are: whoever serves.
    owner: u64,
    /// How many handlers are being called. (Changed under the lock of
    /// `queue`: a handler that returns hands its place to the request
    /// that waits longest.)
    handlers: AtomicUsize,
    /// The requests that wait for a handler, oldest first: each is
    /// told by its cell that it has one ([`Waiting`]).
    queue: Mutex<std::collections::VecDeque<Arc<Cell<()>>>>,
    /// Who wakes a request that is given a handler.
    scheduler: std::sync::Weak<crate::scheduler::Scheduler>,
    /// The listener was closed: the server takes no connection more, and
    /// ends when the requests in flight have been answered
    /// ([`Server::drain`]).
    draining: AtomicBool,
    /// Completed when the drain begins: a connection that waits for its
    /// next request stops waiting then.
    drain_begun: Arc<Cell<()>>,
    /// Completed when the drain is over: the last connection has ended.
    drained: Arc<Cell<()>>,
    /// How many bytes of bodies the server holds for requests that no
    /// handler has yet ([`wire::BODIES_MAX`]).
    bodies: AtomicUsize,
    /// The tasks of the connections that have not ended; `None` once
    /// the server has ended: no connection goes on then.
    conns: Mutex<Option<HashMap<usize, Arc<TaskHandle>>>>,
    /// The flags that stop the task that serves: its cancel and the end
    /// of its deadline. Once one is set the server is stopped
    /// ([`Server::stopped`]), though its frame may not have ended yet.
    stops: Vec<Arc<AtomicBool>>,
}

/// What a request is given when it asks for a handler.
#[cfg(feature = "http")]
enum Admitted {
    /// A handler: it is called now.
    Called(Called),
    /// A place in the queue: the cell is completed when a handler is
    /// handed to it.
    Queued(Arc<Cell<()>>),
    /// Nothing: the queue is full.
    Full,
}

#[cfg(feature = "http")]
impl Server {
    /// Whether the server was stopped: its task was, or it has ended. A
    /// stopped server serves no further request on any of its
    /// connections, also before its frame has ended and cancelled their
    /// tasks: a client that keeps a connection to it may send the next
    /// request on it while another server already serves the listener.
    fn stopped(&self) -> bool {
        self.stops.iter().any(|stop| stop.load(Ordering::SeqCst)) || self.conns.lock().is_none()
    }

    /// A handler for a request, if one is free and no request waits
    /// for one; else a place in the queue, if it has one.
    fn admit(self: &Arc<Self>) -> Admitted {
        let mut queue = self.queue.lock();
        // (A request that stopped waiting has left its cell complete.)
        queue.retain(|waiting| waiting.get().is_none());
        if queue.is_empty() && self.handlers.load(Ordering::Acquire) < wire::HANDLERS_MAX {
            self.handlers.fetch_add(1, Ordering::AcqRel);
            return Admitted::Called(Called(self.clone()));
        }
        if queue.len() >= wire::QUEUED_MAX {
            return Admitted::Full;
        }
        let cell = Cell::new();
        queue.push_back(cell.clone());
        Admitted::Queued(cell)
    }

    /// Whether the server drains: its listener was closed.
    fn draining(&self) -> bool {
        self.draining.load(Ordering::SeqCst)
    }

    /// Begin the drain: a connection answers the request it has and
    /// ends, one that waits for a request ends now. `true` if no
    /// connection is left.
    fn drain(&self) -> bool {
        self.draining.store(true, Ordering::SeqCst);
        self.complete(&self.drain_begun);
        let left = self.conns.lock().as_ref().map_or(0, HashMap::len);
        if left == 0 {
            self.complete(&self.drained);
        }
        left == 0
    }

    /// Complete `cell`, of a request that waits: `false` if it is
    /// complete already.
    fn complete(&self, cell: &Cell<()>) -> bool {
        match self.scheduler.upgrade() {
            Some(scheduler) => cell.complete((), scheduler.wake()).is_ok(),
            None => cell.complete((), &Nobody).is_ok(),
        }
    }
}

/// Who wakes nobody: the program has ended.
#[cfg(feature = "http")]
struct Nobody;

#[cfg(feature = "http")]
impl Wake for Nobody {
    fn wake(&self, _task: crate::runtime::sync::TaskId) {}
}

/// A request that waits for a handler, until it is told whether it has
/// one ([`Waiting::settle`]) or is dropped: then a handler that was
/// handed to it is handed on.
#[cfg(feature = "http")]
struct Waiting {
    server: Arc<Server>,
    cell: Option<Arc<Cell<()>>>,
}

#[cfg(feature = "http")]
impl Waiting {
    /// The handler that was handed to the request, if one was: else
    /// the request stops waiting, and none is handed to it after.
    ///
    /// The cell is completed once: by a handler that returns and hands
    /// its place on, or here. Whoever completes it first decides.
    fn settle(&mut self) -> Option<Called> {
        let cell = self.cell.take()?;
        let _queue = self.server.queue.lock();
        match self.server.complete(&cell) {
            // It stops waiting.
            true => None,
            // A handler was handed to it.
            false => Some(Called(self.server.clone())),
        }
    }
}

#[cfg(feature = "http")]
impl Drop for Waiting {
    fn drop(&mut self) {
        // (A handler handed to it is handed on as it is dropped.)
        drop(self.settle());
    }
}

/// Bytes of bodies that the server holds for a request that no
/// handler has yet, until this is dropped: when the request is handed
/// to its handler, or given up.
#[cfg(feature = "http")]
struct Held(Arc<Server>, usize);

#[cfg(feature = "http")]
impl Held {
    /// Count `bytes` more as held, if the server has room for them.
    /// What is counted is noted in `pending`, for who makes the
    /// [`Held`] of it.
    fn reserve(server: &Server, pending: &AtomicUsize, bytes: usize) -> bool {
        let mut held = server.bodies.load(Ordering::Acquire);
        loop {
            if bytes > wire::BODIES_MAX.saturating_sub(held) {
                return false;
            }
            let counted = server.bodies.compare_exchange_weak(
                held,
                held + bytes,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            match counted {
                Ok(_) => break,
                Err(now) => held = now,
            }
        }
        pending.fetch_add(bytes, Ordering::AcqRel);
        true
    }
}

#[cfg(feature = "http")]
impl Drop for Held {
    fn drop(&mut self) {
        self.0.bodies.fetch_sub(self.1, Ordering::AcqRel);
    }
}

/// A handler that is being called, until this is dropped: when it has
/// returned or failed, or its task was stopped.
#[cfg(feature = "http")]
struct Called(Arc<Server>);

#[cfg(feature = "http")]
impl Drop for Called {
    fn drop(&mut self) {
        // The place goes to the request that waits longest, if one
        // does: the count stays.
        let mut queue = self.0.queue.lock();
        while let Some(waiting) = queue.pop_front() {
            if self.0.complete(&waiting) {
                return;
            }
        }
        self.0.handlers.fetch_sub(1, Ordering::AcqRel);
    }
}

/// How the response to a request is sent.
#[cfg(feature = "http")]
#[derive(Clone, Copy)]
struct Reply {
    /// The connection is closed after it.
    close: bool,
    /// The connection is kept, and the client (HTTP/1.0) is told.
    keep_alive: bool,
    /// The request was `HEAD`.
    head_only: bool,
}

#[cfg(feature = "http")]
impl Reply {
    /// The last word on a connection that the server ends.
    const LAST: Reply = Reply {
        close: true,
        keep_alive: false,
        head_only: false,
    };

    fn then(self) -> Then {
        match self.close {
            true => Then::Close,
            false => Then::Next,
        }
    }
}

/// What a connection does when a response has been sent.
#[cfg(feature = "http")]
#[derive(Clone, Copy)]
enum Then {
    /// Read the next request.
    Next,
    Close,
    /// Close, when the client has stopped sending: after a refusal
    /// (see [`wire::REFUSAL_TIME`]).
    Drain,
}

/// What a connection gave when the server read it for a request.
#[cfg(feature = "http")]
enum Got {
    /// A request for the handler: the `Request` value, and the count
    /// of its body among those the server holds.
    Request(Value, Reply, Held),
    /// A request of a method that the server does not know: it
    /// answers itself, with a status and a word.
    Answered(u16, &'static str, Reply),
    /// A request that the server refuses, with a status and a word.
    /// The connection ends with it.
    Refused(u16, &'static str),
    /// Nothing more: the client closed the connection, or it broke.
    End,
}

#[cfg(feature = "http")]
impl Got {
    /// Made on the thread that read the request: the value of a
    /// request with a body of megabytes is not built on a worker of
    /// the scheduler.
    /// `held` is what was counted for the request's body: it goes
    /// with a request for the handler, and ends here otherwise. `peer`
    /// is the client's address (`ip:port`).
    fn of(next: wire::Next, held: Held, peer: &str, local: &str) -> Got {
        let request = match next {
            wire::Next::Request(request) => request,
            wire::Next::Refused(refused) => return Got::Refused(refused.status, refused.why),
            wire::Next::End | wire::Next::Broken(_) => return Got::End,
        };
        let reply = Reply {
            close: request.close,
            keep_alive: request.http10 && !request.close,
            head_only: request.method == "HEAD",
        };
        let method = match request.method.as_str() {
            "GET" => bv::GET,
            "POST" => bv::POST,
            "PUT" => bv::PUT,
            "PATCH" => bv::PATCH,
            "DELETE" => bv::DELETE,
            "HEAD" => bv::HEAD,
            "OPTIONS" => bv::OPTIONS,
            _ => return Got::Answered(405, wire::reason(405), reply),
        };
        // `OPTIONS *` asks the server, not a resource: it answers itself,
        // with the methods it serves. No other request names `*`.
        if request.target == "*" {
            return match method == bv::OPTIONS {
                true => Got::Answered(204, "", reply),
                false => Got::Answered(400, wire::reason(400), reply),
            };
        }
        let Some(url) = request_url(&request.target, &request.headers, local) else {
            return Got::Answered(400, wire::reason(400), reply);
        };
        // A header's name is in lower case, however the client wrote
        // it: a name is not case-sensitive, and a lookup has one
        // spelling to ask for.
        let headers =
            (request.headers.into_iter()).map(|(name, value)| (name.to_ascii_lowercase(), value));
        let headers = header_map(headers);
        // The body is the bytes that arrived.
        Got::Request(
            make_http_request_value(method, url, headers, request.body, peer),
            reply,
            held,
        )
    }
}

/// The URL a request asks for (`Request.url`): `http://`, the host the
/// request names (its `Host`; for an HTTP/1.0 request without one, the
/// address the client reached, `local`) and the target as sent. A target
/// in absolute form (`http://h/x`) is that URL, of `http` only: the server
/// speaks no TLS, so the scheme is always `http`. Its `Host`, when the
/// request has one, must be one the server takes in a request of its own
/// and name the same host and port (the host in any case, port 80 the same
/// as none), so that the URL and the header never name two hosts. `None`
/// when they make no URL, or make one that two readers could take
/// differently: a `#` in the target (no client sends a fragment, and a
/// proxy may read it as part of the path), a `Host` that is more than a
/// host and a port, a target with a user or a password, a target that
/// `url.parse` refuses (an escape that is none, or no UTF-8 text), one of
/// another scheme, one whose host and port are not the `Host`'s. The
/// server answers such a request 400.
#[cfg(feature = "http")]
pub(crate) fn request_url(target: &str, headers: &[(String, String)], local: &str) -> Option<Url> {
    if target.contains('#') {
        return None;
    }
    let host = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("host"))
        .map(|(_, host)| host.as_str());
    // (A `@`, `/`, `?` or `#` would make another part of the URL of it.)
    let plain = |byte: u8| byte.is_ascii_alphanumeric() || b"-._:[]".contains(&byte);
    let named = |host: &str, target: &str| {
        let taken = !host.is_empty() && host.bytes().all(plain);
        taken
            .then(|| crate::url::parse(&format!("http://{host}{target}")).ok())
            .flatten()
    };
    if !target.starts_with('/') {
        let (scheme, rest) = target.split_once("://")?;
        if !scheme.eq_ignore_ascii_case("http") {
            return None;
        }
        let url = crate::url::parse(&format!("http://{rest}")).ok()?;
        if !url.user().is_empty() || !url.password().is_empty() {
            return None;
        }
        if let Some(host) = host {
            let host = named(host, "/")?;
            if (host.host(), host.port()) != (url.host(), url.port()) {
                return None;
            }
        }
        return Some(url);
    }
    named(host.unwrap_or(local), target)
}

/// The read half of a connection, for the reader of its requests.
#[cfg(feature = "http")]
struct Reads(Arc<TcpStreamHandle>);

#[cfg(feature = "http")]
impl std::io::Read for Reads {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}

/// What a step of a connection's task completes with when the I/O
/// pool cannot run it: the connection is closed then.
#[cfg(feature = "http")]
fn not_served(_why: crate::vm::IoFailure<'_>) -> Value {
    Value::Unit
}

/// The bytes of a response, sent as `reply` says, with the date of
/// the host's clock.
#[cfg(feature = "http")]
fn response_bytes(
    vm: &Vm,
    status: u16,
    headers: &[(String, String)],
    body: &[u8],
    reply: Reply,
) -> Vec<u8> {
    let now = vm.runtime.io.now().as_secs();
    let date = i64::try_from(now)
        .ok()
        .and_then(|now| chrono::DateTime::from_timestamp(now, 0))
        .map(|now| now.format("%a, %d %b %Y %H:%M:%S GMT").to_string())
        .unwrap_or_default();
    let how = wire::Sending {
        date: &date,
        close: reply.close,
        keep_alive: reply.keep_alive,
        head_only: reply.head_only,
    };
    wire::response(status, headers, body, how)
}

/// What the body of a response of the server's own is: a word.
#[cfg(feature = "http")]
const TEXT: &str = "text/plain; charset=UTF-8";

/// The message of a handler's failure as the one log line of
/// `http.serve` writes it: its line ends as `\n` (and `\r`). The text
/// can be the program's (a panic with data of the request in it), and
/// each log line is one of the server's: a line end in it must not
/// start a line of its own. (The other control characters of a panic's
/// text are escapes already, `VmError::panic`; the report of a failed
/// program keeps a panic's line ends.)
#[cfg(feature = "http")]
fn log_text(message: &str) -> String {
    message.replace('\r', "\\r").replace('\n', "\\n")
}

/// A response of the server's own: a status, and a word as its body (a
/// `204` has none).
#[cfg(feature = "http")]
fn plain_response(vm: &Vm, status: u16, why: &str, reply: Reply) -> Vec<u8> {
    let mut headers = Vec::new();
    if wire::has_body(status) {
        headers.push(("Content-Type".to_string(), TEXT.to_string()));
    }
    // The methods that are allowed are those of `Method`: what a `405`
    // and the answer to `OPTIONS *` say.
    if matches!(status, 405 | 204) {
        let allowed = "GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS";
        headers.push(("Allow".to_string(), allowed.to_string()));
    }
    response_bytes(vm, status, &headers, why.as_bytes(), reply)
}

/// The response that the handler returned, or 500 if what it returned
/// is no response. The body is the handler's bytes, and what they are
/// is the handler's to say: no `Content-Type` is added.
#[cfg(feature = "http")]
fn handler_response(vm: &Vm, returned: &Value, reply: Reply) -> Vec<u8> {
    match extract_http_response(returned) {
        Ok((status, body, fields)) => {
            let mut headers = Vec::new();
            if let Some(Value::Map(given)) = fields.get("headers") {
                for (name, value) in given.iter() {
                    if let (Value::String(name, _), Value::String(value, _)) = (name, value) {
                        headers.push((name.to_string(), value.to_string()));
                    }
                }
            }
            response_bytes(vm, status, &headers, body, reply)
        }
        Err(e) => {
            // Security: don't leak VmError contents (call stack, line
            // numbers, internal function names, possibly-sensitive panic
            // payloads) over the HTTP wire (MED-1). Log internally,
            // respond generically.
            vm.runtime.io.err(&format!(
                "http.serve: handler returned malformed Response: {}\n",
                log_text(&e.message)
            ));
            plain_response(vm, 500, wire::reason(500), reply)
        }
    }
}

/// `addr` as a client's address is written: an IPv4 client of a
/// dual-stack listener (`[::]`) by its IPv4 address (`127.0.0.1:5000`),
/// not the IPv6 form the system gives it (`[::ffff:127.0.0.1]:5000`).
#[cfg(feature = "http")]
fn canonical(addr: std::net::SocketAddr) -> std::net::SocketAddr {
    std::net::SocketAddr::new(addr.ip().to_canonical(), addr.port())
}

/// What a connection that reads a request does when the server begins
/// to drain.
#[cfg(feature = "http")]
#[derive(Debug, PartialEq, Eq)]
enum OnDrain {
    /// Nothing of a request has come: the connection ends.
    End,
    /// A request has begun to come: it is read on, in the time its head
    /// or its body has left (not a new one), and answered.
    ReadOn,
}

/// [`OnDrain`] for a connection that is reading a body (`in_body`), or
/// whose request's head has `begun`.
#[cfg(feature = "http")]
fn on_drain(in_body: bool, begun: bool) -> OnDrain {
    match in_body || begun {
        true => OnDrain::ReadOn,
        false => OnDrain::End,
    }
}

/// The task of one connection: read a request, call the handler with
/// it, send what the handler returns, and go on with the next request
/// while the connection is kept.
///
/// What is there to be read, and what the system takes of a response
/// at once, is read and written where the task runs, without waiting
/// ([`TcpStreamHandle::read_now`], [`TcpStreamHandle::write_in_turn`]): a
/// request that has arrived is answered without a thread changing
/// hands. Whatever has to be waited for (a request that has not come,
/// a body, a client that takes its response slowly) is an operation of
/// the I/O pool that the task waits for like for any I/O, with the
/// server's time limit as the deadline of the wait. The handler itself
/// may wait as long as it likes (a long poll) without holding a
/// thread.
#[cfg(feature = "http")]
struct Conn {
    server: Arc<Server>,
    /// The task's own handle: where its failure is read when the
    /// handler fails.
    handle: Arc<TaskHandle>,
    stream: Arc<TcpStreamHandle>,
    /// The client's address, `ip:port` (`[::1]:port` for IPv6): the
    /// `peer` of each request.
    peer: Arc<str>,
    /// The address the client reached, as `peer` is written: the host of
    /// the `url` of a request that names none (HTTP/1.0).
    local: Arc<str>,
    /// Reads the requests; it holds what was read beyond the end of
    /// one. Locked by the operation that reads.
    reader: Arc<Mutex<wire::Reader<Reads>>>,
    /// Bytes of the request that is being waited for have come, as
    /// the reader says ([`wire::Reader::begun`]).
    begun: Arc<AtomicBool>,
    /// The bytes that the reader has had counted for the body of the
    /// request it is reading ([`Held::reserve`]).
    pending: Arc<AtomicUsize>,
    /// How many requests the task has served since it last gave way
    /// (see [`Conn::TURN`]).
    served: usize,
    state: ConnState,
    /// Another task goes on with the connection (see
    /// [`Conn::go_on_in_a_new_task`]): this one leaves it open.
    handed_on: bool,
}

#[cfg(feature = "http")]
enum ConnState {
    /// The next request is to be read.
    Next,
    /// A request is being read.
    Reading {
        op: crate::vm::IoOp,
        got: Arc<Mutex<Option<Got>>>,
        /// Its head is there, and its body is being read.
        in_body: bool,
        /// Completed when the head is there and a body follows.
        body_follows: Arc<Cell<()>>,
        /// When the wait for the head, or for the body, is over.
        deadline: Option<Duration>,
    },
    /// The request waits for a handler (see [`wire::QUEUED_MAX`]).
    Queued {
        request: Value,
        reply: Reply,
        held: Held,
        waiting: Waiting,
    },
    /// The handler is being called.
    Calling { reply: Reply, called: Called },
    /// A response is to be sent.
    Answering { bytes: Vec<u8>, then: Then },
    /// A response is being sent: its rest is written by an operation
    /// of the pool that waits for the system, with the lock of the
    /// connection's writes. Nothing else is written on the connection
    /// in this state, by the task or when it is abandoned: a writer
    /// that takes its turn ([`TcpStreamHandle::write_in_turn`]) would
    /// wait behind that operation for as long as the client does not
    /// read, on a thread of the scheduler. Every way out of the state
    /// has the operation done, or ends the connection without another
    /// word.
    Sending { op: crate::vm::IoOp, then: Then },
    /// The server has said its last word, a refusal: what the client
    /// still sends is read and dropped.
    Draining(crate::vm::IoOp),
    /// The frame has returned.
    Ended,
}

/// What a step of a connection's task comes to.
#[cfg(feature = "http")]
enum Go {
    /// A step of the task's frame: it waits, calls or ends.
    Step(Step),
    /// Nothing to wait for: the state it left is gone on with.
    Again,
}

#[cfg(feature = "http")]
impl Conn {
    /// The end of the task: the connection is closed when the frame
    /// is dropped.
    const END: Go = Go::Step(Step::Done(Value::Unit));

    /// How many requests a connection serves before its task gives
    /// way to the others. A connection whose requests are there
    /// already (sent without waiting for the answers, or as fast as
    /// they are answered) never has to wait, and a request is few
    /// steps of the VM but a parse and two system calls: counted in
    /// steps like a task that computes, such a connection would hold
    /// its worker for milliseconds. Its turn is counted in requests,
    /// whoever answers them: the handler or the server itself.
    const TURN: usize = 8;

    /// The next request, if it need not be waited for: it is among
    /// the bytes read already, or with those that are there to be
    /// read. `None` if it has to be waited for.
    fn at_hand(&mut self) -> Option<Got> {
        let mut reader = self.reader.try_lock()?;
        // (A body that is among the bytes read is held already:
        // nothing is counted for it.)
        let got = |next| Got::of(next, Held(self.server.clone(), 0), &self.peer, &self.local);
        if let Some(next) = reader.buffered() {
            return Some(got(next));
        }
        let mut read = [0u8; 8 * 1024];
        match self.stream.read_now(&mut read)? {
            // The client has closed the connection.
            0 => Some(Got::End),
            n => {
                reader.feed(&read[..n]);
                reader.buffered().map(got)
            }
        }
    }

    /// Do what a read gave asks for: call the handler, or answer. A
    /// stopped server answers nothing more: the connection is closed,
    /// and the client sends the request again elsewhere if it may.
    fn serve(&mut self, vm: &mut Vm, got: Got) -> Go {
        self.served += 1;
        if self.server.stopped() {
            return Conn::END;
        }
        match got {
            Got::Request(request, reply, held) => match self.server.admit() {
                Admitted::Called(called) => self.call(vm, request, reply, held, called),
                // As many handlers as the server calls at a time are
                // being called: the request waits for one, for a while.
                Admitted::Queued(cell) => {
                    let waiting = Waiting {
                        server: self.server.clone(),
                        cell: Some(cell.clone()),
                    };
                    let wait = Wait::new(vec![Arm::Cell(cell)])
                        .deadline(vm.runtime.io.deadline_after(wire::QUEUED_TIME));
                    self.state = ConnState::Queued {
                        request,
                        reply,
                        held,
                        waiting,
                    };
                    Go::Step(Step::park(wait))
                }
                // And as many requests wait as may.
                Admitted::Full => self.unavailable(vm, reply),
            },
            Got::Answered(status, why, reply) => {
                let bytes = plain_response(vm, status, why, reply);
                self.send(vm, bytes, reply.then())
            }
            Got::Refused(status, why) => {
                let bytes = plain_response(vm, status, why, Reply::LAST);
                self.send(vm, bytes, Then::Drain)
            }
            // Nothing more comes.
            Got::End => Conn::END,
        }
    }

    /// Call the handler with `request`, with the place `called`.
    fn call(
        &mut self,
        vm: &mut Vm,
        request: Value,
        reply: Reply,
        held: Held,
        called: Called,
    ) -> Go {
        // The body is the handler's now.
        drop(held);
        self.state = ConnState::Calling { reply, called };
        Go::Step(vm.call(self.server.handler.clone(), [request]))
    }

    /// Answer 503: no handler for the request.
    fn unavailable(&mut self, vm: &mut Vm, reply: Reply) -> Go {
        let bytes = plain_response(vm, 503, wire::reason(503), reply);
        self.send(vm, bytes, reply.then())
    }

    /// Read the next request: the task waits for it.
    ///
    /// The wait has a deadline, [`wire::REQUEST_TIME`] for the head,
    /// and when the reader says that the head is there and a body
    /// follows, a new one for the body ([`wire::TRANSFER_TIME`]). A
    /// deadline that passes ends the task's wait, and with it the
    /// read: the connection is shut down (see `IoOp`'s `Drop`).
    fn read(&mut self, vm: &mut Vm) -> Step {
        // The task waits: its turn is over.
        self.served = 0;
        let got: Arc<Mutex<Option<Got>>> = Arc::default();
        let body_follows: Arc<Cell<()>> = Cell::new();
        let (reader, stream, stopped) = (
            self.reader.clone(),
            self.stream.clone(),
            self.stream.clone(),
        );
        let (result, announced) = (got.clone(), body_follows.clone());
        let (server, pending) = (self.server.clone(), self.pending.clone());
        let (peer, local) = (self.peer.clone(), self.local.clone());
        let scheduler = vm.scheduler().clone();
        let op = vm.io_op(
            not_served,
            Leave::interrupts(move |_| stopped.shut_down()),
            move |_| {
                let mut before_body = |waits: bool| {
                    let _ = announced.complete((), scheduler.wake());
                    // The interim response is written like everything
                    // that the connection's task writes itself: what
                    // the system takes at once. The task may have its
                    // last word to say meanwhile, and waits for this
                    // write to say it. A client that waits takes these
                    // bytes at once. Where the system takes none of
                    // them (its buffer is full of earlier responses
                    // that the client has not read, or the task has
                    // ended the writes), nothing stands on the
                    // connection and nothing is owed: a server may
                    // leave the interim response out, and the request
                    // is read on. Where it takes a part of them, half
                    // an interim response would stand before whatever
                    // came next: the connection has been shut down
                    // (`write_whole_in_turn`), and the request ends
                    // here.
                    match waits {
                        false => Ok(()),
                        true => match stream.write_whole_in_turn(wire::CONTINUE) {
                            Taken::All | Taken::Nothing => Ok(()),
                            Taken::Part => Err(std::io::Error::other(
                                "the client took a part of the interim response",
                            )),
                        },
                    }
                };
                let next = reader.lock().next(&mut before_body);
                // What the reader had counted for the body.
                let held = Held(server, pending.swap(0, Ordering::AcqRel));
                *result.lock() = Some(Got::of(next, held, &peer, &local));
                Value::Unit
            },
        );
        let deadline = vm.runtime.io.deadline_after(wire::REQUEST_TIME);
        let mut wait = op.wait(vm, Limit::None).deadline(deadline);
        wait.arms.push(Arm::Cell(body_follows.clone()));
        wait.arms.push(Arm::Cell(self.server.drain_begun.clone()));
        self.state = ConnState::Reading {
            op,
            got,
            in_body: false,
            body_follows,
            deadline,
        };
        Step::park(wait)
    }

    /// Send `bytes`, and go on with `then`. What the system takes at
    /// once is written here; if that is not all of it, the task waits
    /// until the client has taken the rest, for at most
    /// [`wire::TRANSFER_TIME`].
    fn send(&mut self, vm: &mut Vm, bytes: Vec<u8>, then: Then) -> Go {
        let written = self.stream.write_in_turn(&bytes);
        if written == bytes.len() {
            return self.sent(vm, then);
        }
        // A task that was cancelled meanwhile (the server has ended)
        // waits for nothing more.
        if self.handle.is_cancelled() {
            return Conn::END;
        }
        let (stream, stopped) = (self.stream.clone(), self.stream.clone());
        let op = vm.io_op(
            not_served,
            Leave::interrupts(move |_| stopped.shut_down()),
            move |_| Value::Bool(stream.write_all(&bytes[written..]).is_ok()),
        );
        let wait = op.wait(vm, Limit::Own(wire::TRANSFER_TIME));
        self.state = ConnState::Sending { op, then };
        Go::Step(Step::park(wait))
    }

    /// A response has been sent.
    fn sent(&mut self, vm: &mut Vm, then: Then) -> Go {
        match then {
            Then::Next => {
                self.state = ConnState::Next;
                Go::Again
            }
            Then::Close => Conn::END,
            Then::Drain => Go::Step(self.drain(vm)),
        }
    }

    /// The server has said its last word on the connection, a refusal,
    /// while the client may still be sending: tell the client that
    /// nothing more comes, and read what it sends until it stops, for
    /// at most [`wire::REFUSAL_TIME`]. Then the connection is closed.
    fn drain(&mut self, vm: &mut Vm) -> Step {
        let (stream, stopped) = (self.stream.clone(), self.stream.clone());
        let op = vm.io_op(
            not_served,
            Leave::interrupts(move |_| stopped.shut_down()),
            move |_| {
                stream.end_writes();
                let mut dropped = [0u8; 8 * 1024];
                while matches!(stream.read(&mut dropped), Ok(n) if n > 0) {}
                Value::Unit
            },
        );
        let wait = op.wait(vm, Limit::Own(wire::REFUSAL_TIME));
        self.state = ConnState::Draining(op);
        Step::park(wait)
    }

    /// The handler failed, and its task with it. The request is
    /// answered 500, and the connection is as usable as after any
    /// response: a new task takes it over, sends the answer and goes
    /// on. `false` if there is none (the server has ended, or the
    /// program has).
    fn go_on_in_a_new_task(&mut self, vm: &mut Vm, reply: Reply) -> bool {
        let id = vm.next_task_id();
        let handle = Arc::new(TaskHandle::with_owner(id, self.server.owner));
        {
            let mut conns = self.server.conns.lock();
            let Some(conns) = conns.as_mut() else {
                return false;
            };
            conns.remove(&self.handle.id);
            conns.insert(id, handle.clone());
        }
        let mut child = vm.spawn_child();
        child.spawned = true;
        child.push_native_frame(Box::new(Conn {
            server: self.server.clone(),
            handle: handle.clone(),
            stream: self.stream.clone(),
            peer: self.peer.clone(),
            local: self.local.clone(),
            reader: self.reader.clone(),
            begun: self.begun.clone(),
            pending: self.pending.clone(),
            served: 0,
            state: ConnState::Answering {
                bytes: plain_response(vm, 500, wire::reason(500), reply),
                then: reply.then(),
            },
            handed_on: false,
        }));
        // From here the connection is the new task's, which closes it
        // if it cannot be started.
        self.handed_on = true;
        vm.scheduler().submit(id, child, handle).is_ok()
    }
}

#[cfg(feature = "http")]
impl Conn {
    /// Go on from the state the task is in. `input` is what the
    /// handler returned, where it was called.
    fn go(&mut self, vm: &mut Vm, input: Value) -> Result<Go, VmError> {
        Ok(match std::mem::replace(&mut self.state, ConnState::Ended) {
            // A task that was cancelled, or whose server was stopped,
            // serves no further request.
            ConnState::Next if self.handle.is_cancelled() || self.server.stopped() => Conn::END,
            // The server drains: the request answered was the last one.
            ConnState::Next if self.server.draining() => Conn::END,
            // Its turn is over: the other tasks have theirs before the
            // next request of this connection.
            ConnState::Next if self.served >= Conn::TURN => {
                self.served = 0;
                self.state = ConnState::Next;
                Go::Step(Step::Yield)
            }
            ConnState::Next => match self.at_hand() {
                Some(got) => self.serve(vm, got),
                None => Go::Step(self.read(vm)),
            },
            ConnState::Reading {
                op,
                got,
                in_body,
                body_follows,
                deadline,
            } => match vm.woken()? {
                // The server drains. A connection that waits for a request
                // ends; one whose request has begun to come reads it on,
                // in the time it had (what is left of the head's, or the
                // body's), and it is answered.
                Fired::Arm(2, _) => match on_drain(in_body, self.begun.load(Ordering::SeqCst)) {
                    OnDrain::End => {
                        drop(op);
                        Conn::END
                    }
                    OnDrain::ReadOn => {
                        let mut wait = op.wait(vm, Limit::None).deadline(deadline);
                        if !in_body {
                            wait.arms.push(Arm::Cell(body_follows.clone()));
                        }
                        self.state = ConnState::Reading {
                            op,
                            got,
                            in_body,
                            body_follows,
                            deadline,
                        };
                        Go::Step(Step::park(wait))
                    }
                },
                // The head is there: the body has its own time.
                Fired::Arm(1, _) if !in_body => {
                    let deadline = vm.runtime.io.deadline_after(wire::TRANSFER_TIME);
                    let wait = op.wait(vm, Limit::None).deadline(deadline);
                    self.state = ConnState::Reading {
                        op,
                        got,
                        in_body: true,
                        body_follows,
                        deadline,
                    };
                    Go::Step(Step::park(wait))
                }
                Fired::Arm(..) => {
                    drop(op);
                    let got = got.lock().take();
                    match got {
                        Some(got) => self.serve(vm, got),
                        // Nothing could read it.
                        None => Conn::END,
                    }
                }
                // The request did not come in time. A connection that
                // sent nothing of one is closed. A request that had
                // begun is answered, and the read goes on for a moment
                // as after a refusal: its bytes are dropped.
                Fired::Deadline => match in_body || self.begun.load(Ordering::SeqCst) {
                    false => Conn::END,
                    true => {
                        let bytes = plain_response(vm, 408, wire::reason(408), Reply::LAST);
                        // The last word and the end of the writes are
                        // one turn: after the interim response, if the
                        // reader's thread is writing it, and with
                        // nothing between the two. An interim response
                        // that the reader's thread comes to write
                        // later finds the writes ended and is left
                        // out: the reader reads on, and the client is
                        // heard out.
                        let _ = self.stream.write_last_in_turn(&bytes);
                        let wait = op.wait(vm, Limit::Own(wire::REFUSAL_TIME));
                        self.state = ConnState::Draining(op);
                        Go::Step(Step::park(wait))
                    }
                },
            },
            // A handler was handed to the request, or its time to wait
            // is over (or both at once: the handler counts).
            ConnState::Queued {
                request,
                reply,
                held,
                mut waiting,
            } => {
                vm.woken()?;
                match waiting.settle() {
                    Some(called) => self.call(vm, request, reply, held, called),
                    None => self.unavailable(vm, reply),
                }
            }
            ConnState::Calling { reply, called } => {
                // The handler has returned.
                drop(called);
                // (A server that drains, or was stopped, closes the
                // connection after it, and tells the client so.)
                let reply = match self.server.draining() || self.server.stopped() {
                    true => Reply {
                        close: true,
                        keep_alive: false,
                        ..reply
                    },
                    false => reply,
                };
                let bytes = handler_response(vm, &input, reply);
                self.send(vm, bytes, reply.then())
            }
            ConnState::Answering { bytes, then } => self.send(vm, bytes, then),
            ConnState::Sending { op, then } => {
                let sent = matches!(op.outcome(&vm.woken()?), Some(Value::Bool(true)));
                drop(op);
                match sent {
                    true => self.sent(vm, then),
                    // The client did not take the response.
                    false => Conn::END,
                }
            }
            // The client has stopped sending, or its time is over.
            ConnState::Draining(op) => {
                vm.woken()?;
                drop(op);
                Conn::END
            }
            ConnState::Ended => {
                return Err(VmError::internal(
                    "the task of an HTTP connection was resumed after its end",
                ));
            }
        })
    }
}

#[cfg(feature = "http")]
impl crate::vm::Native for Conn {
    fn name(&self) -> &str {
        "http.serve"
    }

    fn resume(&mut self, vm: &mut Vm, input: Value) -> Result<Step, VmError> {
        let mut input = Some(input);
        loop {
            match self.go(vm, input.take().unwrap_or(Value::Unit))? {
                Go::Step(step) => return Ok(step),
                Go::Again => {}
            }
        }
    }

    fn abandon(&mut self, vm: &mut Vm) {
        // The task ends in the middle: the handler failed, or the task
        // was stopped with the server, or dropped with the VM. A
        // request that is in flight is answered; nothing here waits
        // for the system, so the answer is what the system takes at
        // once, in its turn behind an interim response that the
        // reader's thread is writing (`write_in_turn`). (A program
        // that fails under `silt run` ends as a process: nothing runs
        // here then, and its connections are just closed.)
        let unavailable = Reply::LAST;
        match std::mem::replace(&mut self.state, ConnState::Ended) {
            ConnState::Calling { reply, called } => {
                drop(called);
                let failure = match self.handle.try_get() {
                    Some(Err(e)) if !self.handle.is_cancelled() => Some(e),
                    _ => None,
                };
                let Some(e) = failure else {
                    let bytes = plain_response(vm, 503, wire::reason(503), unavailable);
                    let _ = self.stream.write_in_turn(&bytes);
                    return;
                };
                // The failure is read here: it is logged, and not
                // reported as a failure that nothing read. Not a test's
                // `process.exit`: that fails the test, as a failure that
                // nothing read does.
                if !e.exit_in_a_test {
                    self.handle.mark_read();
                    // Security: do NOT include VmError details (call
                    // stack, line numbers, panic payload) in the response
                    // body — that leaks implementation details and
                    // potentially sensitive values across the security
                    // boundary (MED-1). Log to the host's stderr instead.
                    vm.runtime.io.err(&format!(
                        "http.serve: handler error: {}\n",
                        log_text(&e.message)
                    ));
                }
                if !self.go_on_in_a_new_task(vm, reply) && !self.handed_on {
                    let bytes = plain_response(vm, 500, wire::reason(500), unavailable);
                    let _ = self.stream.write_in_turn(&bytes);
                }
            }
            // The answer that a task took over and could not send.
            ConnState::Answering { bytes, .. } => {
                let _ = self.stream.write_in_turn(&bytes);
            }
            ConnState::Reading { in_body: true, .. } | ConnState::Queued { .. } => {
                let bytes = plain_response(vm, 503, wire::reason(503), unavailable);
                let _ = self.stream.write_in_turn(&bytes);
            }
            // Nothing is in flight: between requests, or the response
            // is on its way. (Nothing may be written in that case: see
            // `ConnState::Sending`.)
            _ => {}
        }
    }
}

#[cfg(feature = "http")]
impl Drop for Conn {
    fn drop(&mut self) {
        if self.handed_on {
            return;
        }
        // An operation in flight first: it is given up with the
        // connection.
        self.state = ConnState::Ended;
        self.stream.shut_down();
        let left = match self.server.conns.lock().as_mut() {
            Some(conns) => {
                conns.remove(&self.handle.id);
                conns.len()
            }
            None => 0,
        };
        // The last connection of a server that drains: the drain is over.
        if left == 0 && self.server.draining() {
            self.server.complete(&self.server.drained);
        }
    }
}

/// `http.serve` itself, in the task that called it: accept a
/// connection, start its task, accept the next. It ends when its listener
/// is closed (it returns), or when its task does (cancelled, or dropped at
/// the end of the program), and the connections with it.
#[cfg(feature = "http")]
struct Serve {
    server: Arc<Server>,
    listener: Arc<TcpListenerHandle>,
    /// This server's mark on the listener.
    token: u64,
    scheduler: std::sync::Weak<crate::scheduler::Scheduler>,
    state: ServeState,
    /// The accept before this one failed: that is said once.
    failing: bool,
}

#[cfg(feature = "http")]
enum ServeState {
    Start,
    /// An accept is in flight: the operation of a `tcp.accept`.
    Accepting(crate::vm::IoOp),
    /// An accept failed: the next one comes after
    /// [`wire::ACCEPT_RETRY`].
    Retrying,
    /// The listener was closed: the server waits for the requests in
    /// flight to be answered, then returns.
    Draining,
}

#[cfg(feature = "http")]
impl Serve {
    /// Start the task of a connection.
    fn connection(&mut self, vm: &mut Vm, stream: Arc<TcpStreamHandle>) {
        let id = vm.next_task_id();
        let handle = Arc::new(TaskHandle::with_owner(id, self.server.owner));
        if let Some(conns) = self.server.conns.lock().as_mut() {
            conns.insert(id, handle.clone());
        }
        let mut child = vm.spawn_child();
        child.spawned = true;
        let pending = Arc::new(AtomicUsize::new(0));
        let (server, counted) = (self.server.clone(), pending.clone());
        let reader = wire::Reader::with_room(Reads(stream.clone()), move |bytes| {
            Held::reserve(&server, &counted, bytes)
        });
        let begun = reader.begun();
        // (A connection whose address cannot be read, already reset by
        // its client, has an empty one: it reads nothing either.)
        let peer: Arc<str> = match stream.peer_addr() {
            Ok(addr) => canonical(addr).to_string().into(),
            Err(_) => "".into(),
        };
        let local: Arc<str> = match stream.local_addr() {
            Ok(addr) => canonical(addr).to_string().into(),
            Err(_) => "".into(),
        };
        child.push_native_frame(Box::new(Conn {
            peer,
            local,
            server: self.server.clone(),
            handle: handle.clone(),
            reader: Arc::new(Mutex::new(reader)),
            begun,
            pending,
            served: 0,
            stream,
            state: ConnState::Next,
            handed_on: false,
        }));
        // A task that cannot be started (the program has as many as it
        // may have) is dropped, and its connection closed.
        let _ = vm.scheduler().submit(id, child, handle);
    }
}

#[cfg(feature = "http")]
impl crate::vm::Native for Serve {
    fn name(&self) -> &str {
        "http.serve"
    }

    fn resume(&mut self, vm: &mut Vm, _input: Value) -> Result<Step, VmError> {
        match std::mem::replace(&mut self.state, ServeState::Start) {
            ServeState::Start => {}
            ServeState::Retrying => {
                vm.woken()?;
            }
            // Every connection has ended.
            ServeState::Draining => {
                vm.woken()?;
                return Ok(Step::Done(Value::Unit));
            }
            ServeState::Accepting(op) => {
                let fired = vm.woken()?;
                // A server whose task was cancelled takes no connection
                // more, though the task still runs for some steps: the
                // one its accept was given is left with the operation,
                // which hands it back to the listener for the next
                // accept or the next server. The task ends in the wait
                // it begins here.
                if vm.takes_nothing_new() {
                    drop(op);
                    return Ok(Step::park(Vm::no_wait()));
                }
                let accepted = op.outcome(&fired);
                drop(op);
                let stream = match &accepted {
                    Some(Value::Variant(accepted)) if accepted.is(bv::OK) => {
                        match accepted.fields() {
                            [Value::TcpStream(stream)] => Some(stream.clone()),
                            _ => None,
                        }
                    }
                    _ => None,
                };
                let Some(stream) = stream else {
                    // The listener was closed: the server takes no
                    // connection more, and returns when the requests in
                    // flight have been answered. (A task cancelled
                    // meanwhile ends it at once, see `Drop`.)
                    if self.listener.is_closed() {
                        if self.server.drain() {
                            return Ok(Step::Done(Value::Unit));
                        }
                        self.state = ServeState::Draining;
                        let wait = Wait::new(vec![Arm::Cell(self.server.drained.clone())]);
                        return Ok(Step::park(wait));
                    }
                    // No descriptor left, no thread for the accept: the
                    // server stays, and tries again in a moment.
                    if !self.failing {
                        let why = match &accepted {
                            Some(Value::Variant(refused)) if refused.fields().len() == 1 => {
                                vm.display_value(&refused.fields()[0])
                            }
                            _ => "no value".to_string(),
                        };
                        vm.runtime.io.err(&format!(
                            "http.serve: cannot accept a connection: {why}; trying again\n"
                        ));
                    }
                    self.failing = true;
                    self.state = ServeState::Retrying;
                    let wait = Wait::new(vec![])
                        .deadline(vm.runtime.io.deadline_after(wire::ACCEPT_RETRY));
                    return Ok(Step::park(wait));
                };
                self.failing = false;
                self.connection(vm, stream);
                // The accept took what else was ready with it.
                while let Some(ready) = self.listener.take_kept() {
                    let ready = TcpStreamHandle::plain(vm.next_tcp_id(), ready);
                    self.connection(vm, ready);
                }
            }
        }
        let op = super::tcp::accept_op(vm, &self.listener, true);
        let wait = op.wait(vm, Limit::None);
        self.state = ServeState::Accepting(op);
        Ok(Step::park(wait))
    }
}

/// The server ends with the frame of its `http.serve`, however that
/// ends: its accept is given up as a `tcp.accept` is, the listener is
/// nobody's again, and the tasks of its connections are cancelled (one
/// with a request in flight answers 503, see [`Conn`]'s `abandon`).
#[cfg(feature = "http")]
impl Drop for Serve {
    fn drop(&mut self) {
        self.state = ServeState::Start;
        self.listener.served_no_more(self.token);
        let conns = self.server.conns.lock().take();
        if let (Some(conns), Some(scheduler)) = (conns, self.scheduler.upgrade()) {
            for handle in conns.values() {
                scheduler.cancel(handle);
            }
        }
    }
}

/// The headers of a request as the program gave them.
#[cfg(feature = "http")]
fn header_pairs(headers: Map) -> Vec<(String, String)> {
    headers
        .iter()
        .filter_map(|(k, v)| Some((<&str>::take(k)?.to_string(), <&str>::take(v)?.to_string())))
        .collect()
}

/// A `Method` argument: its name (`GET`).
#[cfg(feature = "http")]
struct Method<'a>(&'a str);

#[cfg(feature = "http")]
impl<'a> Arg<'a> for Method<'a> {
    fn take(value: &'a Value) -> Option<Self> {
        match value {
            Value::Variant(variant) if variant.fields().is_empty() && variant.of(ty::METHOD) => {
                Some(Method(variant.name()))
            }
            _ => None,
        }
    }
}

builtins! {
    #[cfg(feature = "http")]
    fn get(vm, u: typed::Url) -> Result<Step, VmError> {
        let url = Url::clone(u);
        // A request that nobody waits for any more (cancelled, ended
        // by its deadline) is ended: its client is shut down.
        let call = Arc::new(Call::default());
        vm.io("http", http_timeout_err, Leave::closes(call.stop()), move |_| {
            do_http_request_on(client(), &call, "GET", &url, &[], &[], true)
        })
    }

    #[cfg(feature = "http")]
    fn request(vm, method: Method, u: typed::Url, body: Bytes, headers: Map) -> Result<Step, VmError> {
        let (method, url, body) = (method.0.to_string(), Url::clone(u), body.clone());
        let headers = header_pairs(headers);
        let call = Arc::new(Call::default());
        vm.io("http", http_timeout_err, Leave::closes(call.stop()), move |_| {
            do_http_request_on(client(), &call, &method, &url, &body, &headers, true)
        })
    }

    // One request, whose answer is the response, a redirect too: the
    // program follows a redirect itself if it likes.
    #[cfg(feature = "http")]
    fn request_once(vm, method: Method, u: typed::Url, body: Bytes, headers: Map) -> Result<Step, VmError> {
        let (method, url, body) = (method.0.to_string(), Url::clone(u), body.clone());
        let headers = header_pairs(headers);
        let call = Arc::new(Call::default());
        vm.io("http", http_timeout_err, Leave::closes(call.stop()), move |_| {
            do_http_request_on(client(), &call, &method, &url, &body, &headers, false)
        })
    }

    // Serves HTTP on a listener that `tcp.listen` bound. Which
    // interfaces the server is reached on, and on which port, is what
    // was written there.
    #[cfg(feature = "http")]
    fn serve(vm, listener: TcpListener, handler: &Value) -> Result<Step, VmError> {
        // A closed listener has nothing to serve.
        if listener.is_closed() {
            return Ok(Step::Done(Value::Unit));
        }
        // The listener is this server's alone while it serves: two that
        // accept on one listener would take each other's connections.
        let Some(token) = listener.serve(vm.cancelled.clone()) else {
            return Err(VmError::new(
                "http.serve: the listener is already served by another http.serve".into(),
            ));
        };
        Ok(Step::Run(Box::new(Serve {
            server: Arc::new(Server {
                handler: handler.clone(),
                // The tasks of the connections belong to whoever serves.
                owner: vm.scheduler().current_owner(),
                handlers: AtomicUsize::new(0),
                queue: Mutex::default(),
                scheduler: Arc::downgrade(vm.scheduler()),
                draining: AtomicBool::new(false),
                drain_begun: Cell::new(),
                drained: Cell::new(),
                bodies: AtomicUsize::new(0),
                conns: Mutex::new(Some(HashMap::new())),
                stops: vm.cancelled.iter().chain(&vm.over).cloned().collect(),
            }),
            listener: listener.clone(),
            token,
            scheduler: Arc::downgrade(vm.scheduler()),
            state: ServeState::Start,
            failing: false,
        })))
    }

}

#[cfg(all(test, feature = "http"))]
mod http_response_tests {
    use super::*;

    fn make_response(status: i64) -> Value {
        Value::builtin_record(
            ty::RESPONSE,
            [
                ("status", Value::Int(status)),
                ("body", Value::Bytes(Arc::new(Vec::new()))),
                ("headers", Value::Map(crate::value::Map::new())),
            ],
        )
    }

    /// The records the module builds have the fields of their types,
    /// in the types' order (`Value::builtin_record` says so in a debug
    /// build), and are shown in that order.
    #[test]
    fn the_records_of_the_module_have_their_types_fields() {
        let headers = || {
            let name = Value::string("accept");
            crate::value::Map::from_iter([(name, Value::string("*/*"))])
        };
        let response = make_http_response(204, headers(), vec![0, 0xff, b'd']);
        assert_eq!(
            response.to_string(),
            "Response { status: 204, body: bytes(00 ff 64, length: 3), headers: #{ \"accept\": \"*/*\" } }"
        );
        let (status, body, record) = extract_http_response(&response).expect("a Response");
        assert_eq!((status, body), (204, &[0, 0xff, b'd'][..]));
        assert_eq!(record.fields()[0], Value::Int(204));

        let url = crate::url::parse("http://h:8080/jobs?n=1").expect("a URL");
        let request =
            make_http_request_value(bv::POST, url, headers(), b"go".to_vec(), "[::1]:5000");
        assert_eq!(
            request.to_string(),
            "Request { method: POST, url: <url:http://h:8080/jobs?n=1>, headers: #{ \"accept\": \"*/*\" }, body: bytes(67 6f, length: 2), peer: \"[::1]:5000\" }"
        );
        let Value::Record(record) = &request else {
            panic!("a record");
        };
        let Some(Value::Url(url)) = record.get("url") else {
            panic!("a url field");
        };
        assert_eq!(url.path(), "/jobs");
        assert!(matches!(&record.fields()[1], Value::Url(_)));
    }

    #[test]
    fn test_response_status_out_of_u16_range_rejected() {
        let val = make_response(99999);
        let err = extract_http_response(&val).unwrap_err();
        assert!(
            err.message.contains("out of range") && err.message.contains("99999"),
            "expected 'out of range' error mentioning 99999, got: {}",
            err.message
        );
    }

    #[test]
    fn test_response_status_negative_rejected() {
        let val = make_response(-1);
        let err = extract_http_response(&val).unwrap_err();
        assert!(
            err.message.contains("out of range"),
            "expected out-of-range error for negative status, got: {}",
            err.message
        );
    }

    #[test]
    fn test_response_status_bounds() {
        for status in [200, 404, 599, 999] {
            let val = make_response(status);
            assert_eq!(extract_http_response(&val).unwrap().0 as i64, status);
        }
        // 1xx is no final status; a status has three digits.
        for status in [0, 100, 101, 199, 1000, 65535] {
            let val = make_response(status);
            let err = extract_http_response(&val).unwrap_err();
            assert!(
                err.message.contains("out of range"),
                "{status}: {}",
                err.message
            );
        }
    }
}

#[cfg(all(test, feature = "http"))]
mod serve_tests {
    use super::*;
    use crate::runtime::sync::Outcome;
    use crate::vm::Native;
    use std::time::Instant;

    /// The `url` of a request: `http://`, its `Host` (the address reached
    /// when it names none) and its target, in normal form; a target in
    /// absolute form as it is, of `http` only, with a `Host` of the same
    /// host and port; `None`, which the server answers 400, where they make
    /// no URL or two readers could take them differently.
    #[test]
    fn a_request_s_url_is_its_host_and_target() {
        let host = |h: &str| vec![("Host".to_string(), h.to_string())];
        let url = |target: &str, headers: &[(String, String)]| {
            request_url(target, headers, "127.0.0.1:9").map(|u| u.as_str().to_string())
        };
        let some = |text: &str| Some(text.to_string());
        assert_eq!(
            url("/a/../b?x=1", &host("H:8080")),
            some("http://h:8080/b?x=1")
        );
        assert_eq!(url("/x", &[]), some("http://127.0.0.1:9/x"));
        assert_eq!(url("//x/%7e", &host("h")), some("http://h//x/~"));
        assert_eq!(
            url("/users/a%2Fb", &host("[::1]:80")),
            some("http://[::1]/users/a%2Fb")
        );
        // A target in absolute form is that URL, and its Host names the same
        // host and port: the host in any case, port 80 the same as none. An
        // HTTP/1.0 request may have no Host.
        for (target, headers, want) in [
            (
                "http://other.test/x",
                host("other.test"),
                "http://other.test/x",
            ),
            (
                "HTTP://Other.TEST:80/x?q",
                host("OTHER.test"),
                "http://other.test/x?q",
            ),
            (
                "http://other.test/x",
                host("other.test:80"),
                "http://other.test/x",
            ),
            (
                "http://o.test:8080/x",
                host("O.test:8080"),
                "http://o.test:8080/x",
            ),
            ("http://[::1]/x", host("[::1]:80"), "http://[::1]/x"),
            ("http://o.test/x", vec![], "http://o.test/x"),
        ] {
            assert_eq!(url(target, &headers), some(want), "{target} {headers:?}");
        }
        for (target, headers) in [
            // A fragment, in either form.
            ("/x#top", host("h")),
            ("/x?q=1#top", host("h")),
            ("/x#", host("h")),
            ("http://h/x#top", host("h")),
            // Only http in absolute form.
            ("https://h/x", host("h")),
            ("HTTPS://h:443/x", host("h:443")),
            ("ftp://h/x", host("h")),
            ("ws://h/x", host("h")),
            // A Host of another host or port than the target's, or one the
            // server would not take on its own.
            ("http://other.test/x", host("h.test")),
            ("http://h.test:8080/x", host("h.test")),
            ("http://h.test/x", host("h.test:8080")),
            ("http://h.test/x", host("h.test:")),
            ("http://h.test/x", host("u@h.test")),
            ("http://h.test/x", host("h.test/x")),
            ("http://h.test/x", host("")),
            // A user or password, or a Host or target that is no URL.
            ("http://u:p@other.test/x", host("other.test")),
            ("http://u@other.test/x", vec![]),
            ("/x", host("h.test:")),
            ("/x", host("u@h")),
            ("/x", host("h/y")),
            ("/x", host("h?y")),
            ("/x", host("")),
            ("/x", host("0x7f.1")),
            ("/%zz", host("h")),
            ("/caf%E9", host("h")),
            ("h:80", host("h")),
        ] {
            assert_eq!(url(target, &headers), None, "{target} {headers:?}");
        }
        assert_eq!(request_url("/x", &[], ""), None);
    }

    /// A connection of server A on the server's side, and the client's
    /// end of it.
    fn connection_of(vm: &mut Vm, server: &Arc<Server>) -> (Conn, std::net::TcpStream) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a listener");
        let client =
            std::net::TcpStream::connect(listener.local_addr().unwrap()).expect("a connection");
        let (accepted, _) = listener.accept().expect("accepted");
        let stream = TcpStreamHandle::plain(vm.next_tcp_id(), accepted);
        let pending = Arc::new(AtomicUsize::new(0));
        let (held_by, counted) = (server.clone(), pending.clone());
        let reader = wire::Reader::with_room(Reads(stream.clone()), move |bytes| {
            Held::reserve(&held_by, &counted, bytes)
        });
        let conn = Conn {
            peer: "127.0.0.1:1".into(),
            local: "127.0.0.1:2".into(),
            server: server.clone(),
            handle: Arc::new(TaskHandle::with_owner(vm.next_task_id(), 0)),
            begun: reader.begun(),
            reader: Arc::new(Mutex::new(reader)),
            pending,
            served: 0,
            stream,
            state: ConnState::Next,
            handed_on: false,
        };
        (conn, client)
    }

    /// A server whose task's stop flag is `stop`.
    fn stoppable(stop: &Arc<AtomicBool>) -> Arc<Server> {
        Arc::new(Server {
            handler: Value::Unit,
            owner: 0,
            handlers: AtomicUsize::new(0),
            queue: Mutex::default(),
            scheduler: std::sync::Weak::new(),
            draining: AtomicBool::new(false),
            drain_begun: Cell::new(),
            drained: Cell::new(),
            bodies: AtomicUsize::new(0),
            conns: Mutex::new(Some(HashMap::new())),
            stops: vec![stop.clone()],
        })
    }

    /// All that the client gets until the connection ends.
    fn all_of(mut client: std::net::TcpStream) -> String {
        use std::io::Read;
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("a timeout");
        let mut got = Vec::new();
        let _ = client.read_to_end(&mut got);
        String::from_utf8_lossy(&got).into_owned()
    }

    /// The race of a server stopped while a client keeps a connection to
    /// it, made certain: the task that served (server A) was stopped,
    /// but its frame has not ended, so the task of the connection was
    /// not cancelled; the client's next request comes on the kept
    /// connection, meant for the server that serves the listener now (B).
    /// A's connection answers nothing and is closed: the client sends a
    /// GET again on a new connection, which B accepts. (Before, A's
    /// handler answered it.)
    #[test]
    fn a_stopped_server_answers_no_request_on_a_kept_connection() {
        use std::io::Write;
        let mut vm = Vm::new(crate::HostIo::process());
        let stop = Arc::new(AtomicBool::new(false));
        let server = stoppable(&stop);
        let (mut conn, mut client) = connection_of(&mut vm, &server);
        stop.store(true, Ordering::SeqCst);
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: h\r\n\r\n")
            .expect("the request");
        // The request is there to be read when the task runs.
        std::thread::sleep(Duration::from_millis(100));
        assert!(!conn.handle.is_cancelled());
        let step = conn.resume(&mut vm, Value::Unit);
        assert!(matches!(step, Ok(Step::Done(_))), "the connection went on");
        drop(conn);
        assert_eq!(all_of(client), "", "a stopped server answered");
    }

    /// A request whose handler runs when its server is stopped is
    /// answered, and the answer says `connection: close`: the client
    /// sends nothing more on the connection.
    #[test]
    fn a_request_handled_when_its_server_stops_is_answered_with_connection_close() {
        let mut vm = Vm::new(crate::HostIo::process());
        let stop = Arc::new(AtomicBool::new(false));
        let server = stoppable(&stop);
        let (mut conn, client) = connection_of(&mut vm, &server);
        let Admitted::Called(called) = server.admit() else {
            panic!("a free handler");
        };
        let reply = Reply {
            close: false,
            keep_alive: false,
            head_only: false,
        };
        conn.state = ConnState::Calling { reply, called };
        stop.store(true, Ordering::SeqCst);
        let response = make_http_response(200, header_map(Vec::new()), b"A".to_vec());
        let step = conn.resume(&mut vm, response);
        assert!(matches!(step, Ok(Step::Done(_))), "the connection was kept");
        drop(conn);
        let got = all_of(client).to_ascii_lowercase();
        assert!(got.starts_with("http/1.1 200"), "{got}");
        assert!(got.contains("\r\nconnection: close\r\n"), "{got}");
        assert!(got.ends_with("\r\n\r\na"), "{got}");
    }

    /// A server whose task was cancelled just as its accept was given
    /// a connection takes nothing: the task still runs for some steps,
    /// and a server that took the connection then would start a task
    /// for it and end, and the client would find its connection closed.
    /// The connection stays with the listener, for the next accept or
    /// the next server; no task is started for it.
    #[test]
    fn a_cancelled_server_leaves_an_accepted_connection_to_the_listener() {
        let mut vm = Vm::new(crate::HostIo::process());
        let bound = std::net::TcpListener::bind("127.0.0.1:0").expect("a listener");
        let addr = bound.local_addr().expect("its address");
        let listener = Arc::new(TcpListenerHandle::new(0, bound));
        let flag = Arc::new(AtomicBool::new(false));
        let token = listener
            .serve(Some(flag.clone()))
            .expect("nobody serves it");
        // The server's accept is given a client's connection, and is
        // over. (Its value comes before its thread is done with it: a
        // cancel in between leaves the connection to that thread, which
        // gives it to the listener a moment later.)
        let op = super::super::tcp::accept_op(&mut vm, &listener, true);
        let _client = std::net::TcpStream::connect(addr).expect("a connection");
        let limit = Instant::now() + Duration::from_secs(10);
        while !op.finished() {
            assert!(Instant::now() < limit, "the accept did not return");
            std::thread::sleep(Duration::from_millis(1));
        }
        let server = Arc::new(Server {
            handler: Value::Unit,
            owner: 0,
            handlers: AtomicUsize::new(0),
            queue: Mutex::default(),
            scheduler: Arc::downgrade(vm.scheduler()),
            draining: AtomicBool::new(false),
            drain_begun: Cell::new(),
            drained: Cell::new(),
            bodies: AtomicUsize::new(0),
            conns: Mutex::new(Some(HashMap::new())),
            stops: Vec::new(),
        });
        let mut serve = Serve {
            server: server.clone(),
            listener: listener.clone(),
            token,
            scheduler: Arc::downgrade(vm.scheduler()),
            state: ServeState::Accepting(op),
            failing: false,
        };
        // The cancel comes before the task runs again.
        flag.store(true, Ordering::SeqCst);
        vm.cancelled = Some(flag);
        vm.woken = Some(Fired::Arm(0, Outcome::Done));
        let step = serve.resume(&mut vm, Value::Unit);
        assert!(matches!(step, Ok(Step::Park(_))), "the server went on");
        let started = server.conns.lock().as_ref().map(HashMap::len);
        assert_eq!(started, Some(0), "a task was started for the connection");
        assert!(
            listener.take_kept().is_some(),
            "the connection is not the listener's"
        );
    }
}

#[cfg(all(test, feature = "http"))]
mod queue_tests {
    use super::*;

    /// A client's address is written as itself: an IPv4 client of a
    /// dual-stack listener by its IPv4 address.
    #[test]
    fn a_peer_s_address_is_its_canonical_form() {
        let shown = |s: &str| canonical(s.parse().expect("an address")).to_string();
        assert_eq!(shown("[::ffff:127.0.0.1]:5000"), "127.0.0.1:5000");
        assert_eq!(shown("[::1]:5000"), "[::1]:5000");
        assert_eq!(shown("10.0.0.2:80"), "10.0.0.2:80");
    }

    /// When the server begins to drain, a connection that has nothing of
    /// a request ends, and one whose request has begun reads it on: in
    /// the time it had (the wait keeps its deadline, `Reading`'s
    /// `deadline`), so a head that trickles still gets its 408 when
    /// the head's time is over, not a body's time later.
    #[test]
    fn a_drain_ends_an_idle_connection_and_reads_a_begun_request_on() {
        assert_eq!(on_drain(false, false), OnDrain::End);
        assert_eq!(on_drain(false, true), OnDrain::ReadOn);
        assert_eq!(on_drain(true, true), OnDrain::ReadOn);
    }

    fn server() -> Arc<Server> {
        Arc::new(Server {
            handler: Value::Unit,
            owner: 0,
            handlers: AtomicUsize::new(0),
            queue: Mutex::default(),
            scheduler: std::sync::Weak::new(),
            draining: AtomicBool::new(false),
            drain_begun: Cell::new(),
            drained: Cell::new(),
            bodies: AtomicUsize::new(0),
            conns: Mutex::new(Some(HashMap::new())),
            stops: Vec::new(),
        })
    }

    fn waiting(server: &Arc<Server>, admitted: Admitted) -> (Arc<Cell<()>>, Waiting) {
        let Admitted::Queued(cell) = admitted else {
            panic!("a place in the queue");
        };
        let waiting = Waiting {
            server: server.clone(),
            cell: Some(cell.clone()),
        };
        (cell, waiting)
    }

    /// The handlers a server calls at a time are taken first, then the
    /// places of its queue; a request beyond both is refused. A handler
    /// that returns is handed to the request that waits longest, which
    /// has it when it settles; one that stopped waiting is passed over,
    /// and a request that is dropped with a handler hands it on. With
    /// nobody waiting, the handler is free again.
    #[test]
    fn a_request_beyond_the_handlers_waits_in_order_and_one_beyond_the_queue_is_refused() {
        let server = server();
        let mut called: Vec<Called> = (0..wire::HANDLERS_MAX)
            .map(|_| match server.admit() {
                Admitted::Called(called) => called,
                _ => panic!("a free handler"),
            })
            .collect();
        let mut queued: Vec<(Arc<Cell<()>>, Waiting)> = (0..wire::QUEUED_MAX)
            .map(|_| waiting(&server, server.admit()))
            .collect();
        assert!(matches!(server.admit(), Admitted::Full), "a full queue");
        // The oldest is handed the handler that returns; the count stays.
        drop(called.pop());
        assert!(queued[0].0.get().is_some() && queued[1].0.get().is_none());
        assert_eq!(server.handlers.load(Ordering::SeqCst), wire::HANDLERS_MAX);
        let (_, mut first) = queued.remove(0);
        let handed = first.settle().expect("the handler was handed to it");
        // The next stops waiting (its time is over): passed over.
        let (_, mut gave_up) = queued.remove(0);
        assert!(gave_up.settle().is_none(), "it had no handler");
        drop(handed);
        assert!(
            queued[0].0.get().is_some(),
            "the one after it has the handler"
        );
        // Dropped with the handler: it hands it on.
        let (_, dropped) = queued.remove(0);
        drop(dropped);
        assert!(queued[0].0.get().is_some(), "handed on");
        // A request that comes now waits behind the others: a newcomer
        // does not jump the queue. The handler goes down the line as
        // the ones before it are dropped, and reaches it.
        let newcomer = waiting(&server, server.admit());
        drop(queued);
        assert!(
            newcomer.0.get().is_some(),
            "the newcomer has the handler last"
        );
        // Nobody waits: a handler that returns is free again.
        let before = server.handlers.load(Ordering::SeqCst);
        drop(newcomer);
        drop(called.pop());
        assert_eq!(server.handlers.load(Ordering::SeqCst), before - 2);
        assert!(matches!(server.admit(), Admitted::Called(_)));
    }
}

/// What each failure of a request is sorted as, with the failures made
/// on the loopback interface: a port nobody listens on, URLs that are
/// none, a server that goes away, one that sends no HTTP, one that
/// never answers, one that speaks no TLS.
#[cfg(all(test, feature = "http"))]
mod http_error_tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

    /// A client whose times are `ms`, so that a test of a timeout does
    /// not wait for the times of `http.get`.
    fn agent(ms: u64) -> ureq::Agent {
        client_with_times(Duration::from_millis(ms), Duration::from_millis(ms))
    }

    /// The URL of a server that takes one connection, reads what the
    /// client sends first, and does `act` with the connection.
    pub(super) fn server(act: impl FnOnce(TcpStream) + Send + 'static) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a listener");
        let addr = listener.local_addr().expect("its address");
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = stream.read(&mut [0u8; 1024]);
                act(stream);
            }
        });
        format!("http://{addr}/")
    }

    /// The variant of the error in `value` and what it carries:
    /// `InvalidUrl(not a url)`, `ClosedEarly`.
    pub(super) fn failure(value: &Value) -> String {
        let named = [
            (bv::HTTP_CONNECT, "Connect"),
            (bv::HTTP_TLS, "Tls"),
            (bv::HTTP_TIMEOUT, "Timeout"),
            (bv::HTTP_INVALID_URL, "InvalidUrl"),
            (bv::HTTP_INVALID_RESPONSE, "InvalidResponse"),
            (bv::HTTP_CLOSED_EARLY, "ClosedEarly"),
            (bv::HTTP_TOO_MANY_REDIRECTS, "TooManyRedirects"),
            (bv::HTTP_OTHER, "Other"),
        ];
        let Value::Variant(result) = value else {
            return format!("no Result: {value}");
        };
        let [Value::Variant(error)] = result.fields() else {
            return format!("no error: {value}");
        };
        if !result.is(bv::ERR) {
            return format!("no error: {value}");
        }
        let name = named.iter().find(|(which, _)| error.is(*which));
        let name = name.map_or("no HttpError", |(_, name)| name);
        match error.fields() {
            [] => name.to_string(),
            fields => {
                let fields: Vec<String> = fields.iter().map(Value::to_string).collect();
                format!("{name}({})", fields.join(", "))
            }
        }
    }

    fn get(url: &str, ms: u64) -> String {
        failure(&finish_http_response(agent(ms).get(url).call(), url, false))
    }

    /// A Url of another scheme than `http` and `https` is a valid URL and no
    /// URL of http's: `InvalidUrl`, with its user and password hidden.
    #[test]
    fn a_url_of_another_scheme_is_invalid_url() {
        for (url, expected) in [
            ("ftp://127.0.0.1/file", "InvalidUrl(ftp://127.0.0.1/file)"),
            ("file:///etc/hosts", "InvalidUrl(file:///etc/hosts)"),
            (
                "ftp://user:secret@127.0.0.1/x",
                "InvalidUrl(ftp://***@127.0.0.1/x)",
            ),
        ] {
            assert_eq!(failure(&do_http_get(url)), expected);
            assert_eq!(
                failure(&do_http_request("POST", url, b"body", &[])),
                expected
            );
        }
    }

    /// A URL longer than the transport takes (65,534 bytes) is
    /// `InvalidUrl`, and nothing is asked: nothing listens at port 9.
    #[test]
    fn a_url_past_the_transport_s_limit_is_invalid_url() {
        let long = format!("http://user:secret@127.0.0.1:9/{}", "a".repeat(65_534));
        let got = failure(&do_http_get(&long));
        // (Cut short: its first 200 characters, and its length as shown.)
        let shown = format!("http://***@127.0.0.1:9/{}", "a".repeat(65_534));
        let want = format!("InvalidUrl({}... ({} bytes))", &shown[..200], shown.len());
        assert_eq!(got, want);
        let just = format!("http://127.0.0.1:9/{}", "a".repeat(65_534 - 19));
        assert_eq!(just.len(), 65_534);
        let got = failure(&do_http_get(&just));
        assert!(got.starts_with("Connect("), "{got}");
    }

    /// A host whose name is not found is `Connect`, as for `tcp.connect`
    /// ("no such host", http.md), not `Other`. (`.invalid` is never found,
    /// RFC 6761.)
    #[test]
    fn a_host_that_is_not_found_is_connect() {
        let failed = failure(&do_http_get("http://no-such-host.invalid/"));
        assert!(failed.starts_with("Connect("), "{failed}");
    }

    /// What a server that answers 200 got as the head of the request to
    /// `url` with `headers`: the url is a template, `{}` for the server's
    /// `host:port`.
    fn head_sent(url: &str, headers: &[(String, String)]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a listener");
        let addr = listener.local_addr().expect("its address");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut head = Vec::new();
                let mut buf = [0u8; 1024];
                while !head.ends_with(b"\r\n\r\n") {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => head.extend_from_slice(&buf[..n]),
                    }
                }
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n");
                let _ = tx.send(String::from_utf8_lossy(&head).into_owned());
            }
        });
        let url = url.replace("{}", &addr.to_string());
        let done = do_http_request("GET", &url, &[], headers);
        assert!(failure(&done).starts_with("no error"), "{}", failure(&done));
        rx.recv_timeout(Duration::from_secs(10)).expect("the head")
    }

    /// The client's own headers are its own: a `Transfer-Encoding` and a
    /// `Connection` among the program's are not sent (a `chunked` made
    /// the request chunked, and a `Connection` went through), as a
    /// `Content-Length` is not. A `Host` of the program's is sent.
    #[test]
    fn the_framing_headers_of_the_program_are_not_sent() {
        let own = [
            ("Transfer-Encoding".to_string(), "chunked".to_string()),
            ("Connection".to_string(), "keep-alive".to_string()),
            ("Content-Length".to_string(), "99".to_string()),
        ];
        let head = head_sent("http://{}/x", &own);
        let lower = head.to_ascii_lowercase();
        assert!(!lower.contains("transfer-encoding"), "{head}");
        assert!(!lower.contains("connection: keep-alive"), "{head}");
        assert!(!lower.contains("content-length: 99"), "{head}");
        let host = [("Host".to_string(), "example.test".to_string())];
        let head = head_sent("http://{}/x", &host);
        let lower = head.to_ascii_lowercase();
        assert!(lower.contains("host: example.test\r\n"), "{head}");
        assert_eq!(lower.matches("host:").count(), 1, "{head}");
    }

    /// A path and a query that are no ASCII go out escaped: the Url holds
    /// them so (they went out raw: `GET /h\xc3\xa9llo`).
    #[test]
    fn a_path_that_is_no_ascii_is_sent_percent_encoded() {
        let head = head_sent("http://{}/h\u{e9}llo?q=\u{fc}", &[]);
        assert!(
            head.starts_with("GET /h%C3%A9llo?q=%C3%BC HTTP/1.1\r\n"),
            "{head}"
        );
    }

    /// The fragment of a Url is not sent, and the path goes out in its
    /// normal form: without dot segments.
    #[test]
    fn the_fragment_is_not_sent_and_the_path_is_normal() {
        let head = head_sent("http://{}/a/./b/../c?x=1#top", &[]);
        assert!(head.starts_with("GET /a/c?x=1 HTTP/1.1\r\n"), "{head}");
    }

    /// A user with a colon once decoded (`us%3Aer`) is refused before
    /// anything is sent: it was sent as `us:er:pw`, which a server reads
    /// as the user `us`. A URL whose user and password are both empty
    /// (`http://@host/`) sends no `authorization` (it sent `Basic Og==`).
    #[test]
    fn a_colon_in_the_user_is_refused_and_no_user_sends_nothing() {
        for url in ["http://@{}/x", "http://:@{}/x"] {
            let head = head_sent(url, &[]);
            assert!(!head.contains("uthorization"), "{url}: {head}");
        }
        // (No connection is made: nothing listens at port 9.)
        let done = do_http_request("GET", "http://us%3Aer:pw@127.0.0.1:9/x", &[], &[]);
        assert_eq!(failure(&done), "InvalidUrl(http://***@127.0.0.1:9/x)");
        // A password may have one.
        let head = head_sent("http://user:p%3Aw@{}/x", &[]);
        assert!(head.contains("authorization: Basic "), "{head}");
    }

    /// The user and password of a URL are sent as `Basic`, each
    /// percent-decoded first: `al%40ice:p%3Aw%2Fd%21%20x` is the user
    /// `al@ice` with the password `p:w/d! x` (they were sent encoded, and
    /// a server read another password). An `authorization` of the
    /// program's is sent instead.
    #[test]
    fn the_user_and_password_of_a_url_are_sent_decoded() {
        use base64::Engine;
        let basic = |pair: &str| {
            format!(
                "authorization: Basic {}\r\n",
                base64::engine::general_purpose::STANDARD.encode(pair)
            )
        };
        let head = head_sent("http://al%40ice:p%3Aw%2Fd%21%20x@{}/x", &[]);
        assert!(head.contains(&basic("al@ice:p:w/d! x")), "{head}");
        let head = head_sent("http://caf%C3%A9@{}/x", &[]);
        assert!(head.contains(&basic("café:")), "{head}");
        let own = [("Authorization".to_string(), "Bearer t".to_string())];
        let head = head_sent("http://user:pw@{}/x", &own);
        assert!(head.contains("authorization: Bearer t\r\n"), "{head}");
        assert_eq!(head.matches("uthorization").count(), 1, "{head}");
    }

    /// The redaction of credentials looks at each byte of a text a
    /// bounded number of times, however many URLs begin in it: 1 MB of
    /// `http://` or of `https://a@` (each search for the next scheme went
    /// over the rest of the text, and such a text did not end in two
    /// minutes).
    #[test]
    fn the_redaction_of_credentials_is_one_pass() {
        for piece in ["http://", "https://a@", "HTTP://", "httpx"] {
            let text = piece.repeat(1_000_000 / piece.len());
            let mut looked = 0;
            let out = redact_counting(&text, &mut looked);
            assert!(
                looked <= 4 * text.len(),
                "{piece}: {looked} for {}",
                text.len()
            );
            if piece == "https://a@" {
                assert!(!out.contains("a@"), "{piece}: the user is hidden");
            }
        }
    }

    #[test]
    fn a_port_nobody_listens_on_is_connect() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a listener");
        let url = format!("http://{}/", listener.local_addr().expect("its address"));
        drop(listener);
        let got = failure(&do_http_get(&url));
        assert!(got.starts_with("Connect("), "{got}");
        // (In silt's words: without the library's `io: `.)
        assert!(!got.contains("io: "), "{got}");
    }

    #[test]
    fn a_server_that_goes_away_is_closed_early() {
        // Before any answer, in the middle of the head, in the middle
        // of the body.
        assert_eq!(get(&server(drop), 5000), "ClosedEarly");
        let half_a_head = server(|mut stream| {
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Le");
        });
        assert_eq!(get(&half_a_head, 5000), "ClosedEarly");
        let half_a_body = server(|mut stream| {
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc");
        });
        assert_eq!(get(&half_a_body, 5000), "ClosedEarly");
    }

    #[test]
    fn a_server_that_sends_no_http_is_invalid_response() {
        let no_status_line = server(|mut stream| {
            let _ = stream.write_all(b"garbage\r\n\r\n");
        });
        let got = get(&no_status_line, 5000);
        assert!(got.starts_with("InvalidResponse("), "{got}");
        let no_chunk_length = server(|mut stream| {
            let head = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\nabc";
            let _ = stream.write_all(head);
        });
        let got = get(&no_chunk_length, 5000);
        assert!(got.starts_with("InvalidResponse("), "{got}");
    }

    #[test]
    fn a_server_that_never_answers_is_timeout() {
        let (done, wait) = std::sync::mpsc::channel::<()>();
        let silent = server(move |stream| {
            // Until the client has given up.
            let _ = wait.recv();
            drop(stream);
        });
        assert_eq!(get(&silent, 150), "Timeout");
        drop(done);
    }

    #[test]
    fn a_server_that_speaks_no_tls_is_tls() {
        let (done, wait) = std::sync::mpsc::channel::<()>();
        let plain = server(move |mut stream| {
            let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n");
            let _ = wait.recv();
        });
        let got = get(&plain.replace("http://", "https://"), 5000);
        assert_eq!(got, "Tls(what the server sent is not TLS)");
        drop(done);
    }

    #[test]
    fn a_header_of_the_program_that_is_none_is_other() {
        let url = "http://127.0.0.1:9/";
        let headers = [("a name".to_string(), "x".to_string())];
        let got = failure(&do_http_request("POST", url, b"body", &headers));
        assert!(got.starts_with("Other("), "{got}");
        let headers = [("name".to_string(), "a\nb".to_string())];
        let got = failure(&do_http_request("GET", url, b"", &headers));
        assert!(got.starts_with("Other("), "{got}");
    }
}

/// What the client makes of the body of a response: the bytes that
/// arrived, unpacked if the response says that they are packed, and
/// never more of them than a body may have.
#[cfg(all(test, feature = "http"))]
mod http_body_tests {
    use super::http_error_tests::{failure, server};
    use super::*;
    use std::io::Write;

    /// `pieces` times `piece`, packed as `Content-Encoding: gzip` says.
    fn packed(pieces: usize, piece: &[u8]) -> Vec<u8> {
        let level = flate2::Compression::fast();
        let mut packer = flate2::write::GzEncoder::new(Vec::new(), level);
        for _ in 0..pieces {
            packer.write_all(piece).expect("packed");
        }
        packer.finish().expect("packed")
    }

    /// The URL of a server that answers one request with `body`, which
    /// it says is packed.
    fn says_packed(body: Vec<u8>) -> String {
        server(move |mut stream| {
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            // (A client that has read enough goes away.)
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        })
    }

    /// The body and the names of the headers of the `Response` in
    /// `value`.
    fn response(value: &Value) -> (Vec<u8>, Vec<String>) {
        let Value::Variant(result) = value else {
            panic!("no Result: {value}");
        };
        let [Value::Record(record)] = result.fields() else {
            panic!("no response: {value}");
        };
        let (Some(Value::Bytes(body)), Some(Value::Map(headers))) =
            (record.get("body"), record.get("headers"))
        else {
            panic!("no Response: {value}");
        };
        let names = headers.keys().map(Value::to_string).collect();
        (body.to_vec(), names)
    }

    #[test]
    fn a_packed_body_arrives_unpacked_and_exact() {
        let every_byte: Vec<u8> = (0..=255).collect();
        let (body, headers) = response(&do_http_get(&says_packed(packed(1, &every_byte))));
        assert_eq!(body, every_byte);
        // What described the packed bytes describes nothing the
        // program has.
        assert_eq!(headers, Vec::<String>::new());
    }

    #[test]
    fn the_limit_counts_what_a_packed_body_unpacks_to() {
        let mib = vec![0u8; 1024 * 1024];
        // At the limit: all of it.
        let tenth = vec![0u8; wire::BODY_MAX / 10];
        let at = packed(10, &tenth);
        let (body, _) = response(&do_http_get(&says_packed(at)));
        assert_eq!(body.len(), wire::BODY_MAX);
        // One byte more, and 128 MiB, in an answer that is small on
        // the wire: refused, however little arrived.
        let mut one_over = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        one_over
            .write_all(&vec![0u8; wire::BODY_MAX + 1])
            .expect("packed");
        for over in [one_over.finish().expect("packed"), packed(128, &mib)] {
            assert!(over.len() < 1024 * 1024, "{} bytes on the wire", over.len());
            assert_eq!(
                failure(&do_http_get(&says_packed(over))),
                "InvalidResponse(the body is larger than 10000000 bytes)"
            );
        }
    }

    #[test]
    fn a_body_that_says_packed_and_is_not_is_invalid_response() {
        let got = failure(&do_http_get(&says_packed(b"plain bytes".to_vec())));
        assert!(got.starts_with("InvalidResponse("), "{got}");
        // Packed bytes that stop short of their end.
        let mut cut = packed(1, &[7u8; 4096]);
        cut.truncate(cut.len() / 2);
        let got = failure(&do_http_get(&says_packed(cut)));
        assert!(got.starts_with("InvalidResponse("), "{got}");
    }

    /// The URL of a server that answers one request with `message`
    /// and closes.
    fn says(message: Vec<u8>) -> String {
        server(move |mut stream| {
            let _ = stream.write_all(&message);
        })
    }

    #[test]
    fn a_packed_body_whose_connection_ends_is_closed_early() {
        let whole = packed(1, &[7u8; 4096]);
        let half = &whole[..whole.len() / 2];
        // Half of what the length announces, and half of a chunk.
        let length = format!("Content-Length: {}\r\n\r\n", whole.len());
        let chunk = format!("Transfer-Encoding: chunked\r\n\r\n{:x}\r\n", whole.len());
        for framing in [length, chunk] {
            let mut message =
                format!("HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\n{framing}").into_bytes();
            message.extend_from_slice(half);
            assert_eq!(failure(&do_http_get(&says(message))), "ClosedEarly");
        }
        // The same half as the whole of a body is a body that does not
        // unpack: nothing went away.
        let got = failure(&do_http_get(&says_packed(half.to_vec())));
        assert!(got.starts_with("InvalidResponse("), "{got}");
    }

    /// A response whose `Content-Length` headers disagree is refused,
    /// packed or not, as one whose length is no number: which of them
    /// frames its body would be a guess (RFC 9112 6.3). The same length
    /// twice is one length (RFC 9110 8.6).
    #[test]
    fn content_lengths_that_disagree_are_an_invalid_response() {
        let says_lengths = |lengths: &str, method: &str| {
            let message = format!("HTTP/1.1 200 OK\r\n{lengths}\r\n\r\nabc").into_bytes();
            do_http_request(method, &says(message), b"", &[])
        };
        let disagree = "InvalidResponse(conflicting content-length headers)";
        let no_number = "InvalidResponse(content-length header not a number)";
        for (lengths, expected) in [
            ("Content-Length: 2\r\nContent-Length: 3", disagree),
            ("Content-Length: 2\r\nContent-Length: 2, 3", disagree),
            ("Content-Length: 2, 3", disagree),
            ("Content-Length: 2\r\nContent-Length: x", no_number),
        ] {
            for method in ["GET", "HEAD"] {
                let got = failure(&says_lengths(lengths, method));
                assert_eq!(got, expected, "{lengths} {method}");
            }
        }
        for lengths in [
            "Content-Length: 2\r\nContent-Length: 2",
            "Content-Length: 2, 2",
        ] {
            let (body, names) = response(&says_lengths(lengths, "GET"));
            let names = (body, names);
            assert_eq!(names, (b"ab".to_vec(), vec!["content-length".to_string()]));
        }
        // A packed body: the lengths are not in what the library hands
        // on, and are looked at all the same.
        let body = packed(1, b"abc");
        let mut message = format!(
            "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\n\
             Content-Length: {}\r\n\r\n",
            body.len(),
            body.len() + 1
        )
        .into_bytes();
        message.extend_from_slice(&body);
        message.push(b'x');
        assert_eq!(failure(&do_http_get(&says(message))), disagree);
        // A chunked body is framed by its chunks, and one length beside
        // them is not looked at; two that disagree are refused all the
        // same, as the RFC lets a recipient refuse a message with both.
        let chunked = |lengths: &str| {
            let message = format!(
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n{lengths}\r\n\r\n\
                 3\r\nabc\r\n0\r\n\r\n"
            );
            do_http_get(&says(message.into_bytes()))
        };
        assert_eq!(response(&chunked("Content-Length: 2")).0, b"abc");
        let got = failure(&chunked("Content-Length: 2\r\nContent-Length: 3"));
        assert_eq!(got, disagree);
    }

    /// An answer that has no body by its nature has none, whatever it
    /// says of one: a server that packs what it answers to `GET` says
    /// so in these too. The library takes the two headers that
    /// describe a packed body out of every answer that says gzip.
    #[test]
    fn an_answer_without_a_body_is_read_as_none() {
        let headers = "Content-Encoding: gzip\r\nContent-Length: 1234\r\nETag: x\r\n\r\n";
        for status in ["204 No Content", "304 Not Modified"] {
            let message = format!("HTTP/1.1 {status}\r\n{headers}").into_bytes();
            let (body, names) = response(&do_http_get(&says(message)));
            assert_eq!(
                (body.len(), names),
                (0, vec!["etag".to_string()]),
                "{status}"
            );
        }
        let message = format!("HTTP/1.1 200 OK\r\n{headers}").into_bytes();
        let (body, names) = response(&do_http_request("HEAD", &says(message), b"", &[]));
        assert_eq!((body.len(), names), (0, vec!["etag".to_string()]));
        // Not packed, the length of what a `GET` would get stays.
        let message = b"HTTP/1.1 200 OK\r\nContent-Length: 1234\r\n\r\n".to_vec();
        let (body, names) = response(&do_http_request("HEAD", &says(message), b"", &[]));
        assert_eq!((body.len(), names), (0, vec!["content-length".to_string()]));
    }

    /// A status that has a body, a body of no bytes, and the word that
    /// it is packed: no bytes are no packed body. (The library keeps
    /// the length on the wire to itself once an answer says gzip, so
    /// this is not told from a packed body that stops short.)
    #[test]
    fn no_bytes_that_say_packed_are_invalid_response() {
        let got = failure(&do_http_get(&says_packed(Vec::new())));
        assert!(got.starts_with("InvalidResponse("), "{got}");
    }

    /// Where the redirect of `status` to `location` from `base` goes.
    fn redirected(base: &str, status: u16, location: &[u8]) -> Option<Result<String, String>> {
        let response = ureq::http::Response::builder()
            .status(status)
            .header("location", location)
            .body(ureq::Body::builder().data(Vec::new()))
            .expect("a response");
        let (code, next) = redirect(&response, &url_of(base))?;
        assert_eq!(code, status);
        Some(next.map(|next| next.as_str().to_string()))
    }

    /// A redirect's location is resolved against the URL that answered
    /// as RFC 3986 says (`url.resolve`), without a fragment, and without
    /// the user and password of the URL that answered.
    #[test]
    fn a_location_is_resolved_against_the_url_that_answered() {
        let base = "http://a.test:8/p/q?x=1";
        for (location, resolved) in [
            ("https://b.test/y", "https://b.test/y"),
            ("//b.test:9/y?z", "http://b.test:9/y?z"),
            ("/y", "http://a.test:8/y"),
            ("?y=2", "http://a.test:8/p/q?y=2"),
            ("r", "http://a.test:8/p/r"),
            ("../r", "http://a.test:8/r"),
            ("", "http://a.test:8/p/q?x=1"),
            ("#f", "http://a.test:8/p/q?x=1"),
            ("?y=2#f", "http://a.test:8/p/q?y=2"),
            ("r#f", "http://a.test:8/p/r"),
            ("https://b.test/y#f", "https://b.test/y"),
            (
                "echo?next=/../../etc",
                "http://a.test:8/p/echo?next=/../../etc",
            ),
            ("../r?a=./b/../c", "http://a.test:8/r?a=./b/../c"),
            ("/x/../y?z=/..", "http://a.test:8/y?z=/.."),
            (".", "http://a.test:8/p/"),
            ("..", "http://a.test:8/"),
            ("./", "http://a.test:8/p/"),
            ("s/..", "http://a.test:8/p/"),
            ("../../../r", "http://a.test:8/r"),
            ("http://b.test/./x/../y?q=/..", "http://b.test/y?q=/.."),
            ("//b.test:9/a/./b/../c", "http://b.test:9/a/c"),
            ("https://b.test/a/..", "https://b.test/"),
            ("https://b.test/a/b?c=/../d", "https://b.test/a/b?c=/../d"),
            ("/h\u{e9} x", "http://a.test:8/h%C3%A9%20x"),
        ] {
            let got = redirected(base, 302, location.as_bytes());
            assert_eq!(got, Some(Ok(resolved.to_string())), "{location}");
        }
        // The user and password of the URL that answered do not go on; a
        // location's own do.
        let with_user = "http://u:p@a.test/p/q";
        assert_eq!(
            redirected(with_user, 301, b"r"),
            Some(Ok("http://a.test/p/r".to_string()))
        );
        assert_eq!(
            redirected(with_user, 301, b"http://v:w@b.test/"),
            Some(Ok("http://v:w@b.test/".to_string()))
        );
        // A location that does not resolve is its text, a user and
        // password in it hidden; a Latin-1 byte is no UTF-8 text.
        assert_eq!(
            redirected(base, 302, b"http://u:secret@h:99999/"),
            Some(Err("http://***@h:99999/".to_string()))
        );
        assert_eq!(
            redirected(base, 302, b"mailto:a@b"),
            Some(Err("mailto:a@b".to_string()))
        );
        assert_eq!(
            redirected(base, 302, b"/caf\xe9"),
            Some(Err("/caf%E9".to_string()))
        );
        assert_eq!(redirected(base, 200, b"/x"), None);
    }
}

/// The `user-agent` a request of silt's client sends.
#[cfg(all(test, feature = "http"))]
mod user_agent_tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    /// The `user-agent` lines of the request that `ask` makes of a
    /// server on the loopback interface, which answers `200` with no
    /// body.
    fn sent(ask: impl FnOnce(&str) -> Value) -> Vec<String> {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a listener");
        let addr = listener.local_addr().expect("its address");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut head = [0u8; 4096];
                let n = stream.read(&mut head).unwrap_or(0);
                let _ = tx.send(String::from_utf8_lossy(&head[..n]).into_owned());
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
            }
        });
        ask(&format!("http://{addr}/"));
        let head = rx.recv().expect("the request");
        head.lines()
            .filter(|line| line.to_ascii_lowercase().starts_with("user-agent:"))
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn every_request_says_silt_and_its_version() {
        let ours = format!("user-agent: silt/{}", env!("CARGO_PKG_VERSION"));
        assert_eq!(sent(do_http_get), vec![ours.clone()]);
        assert_eq!(
            sent(|url| do_http_request("POST", url, b"body", &[])),
            vec![ours]
        );
    }

    #[test]
    fn a_user_agent_the_program_sets_is_sent_instead() {
        for name in ["user-agent", "User-Agent"] {
            let headers = [(name.to_string(), "probe/1".to_string())];
            let got = sent(|url| do_http_request("GET", url, b"", &headers));
            assert_eq!(got.len(), 1, "{got:?}");
            assert!(got[0].ends_with(": probe/1"), "{got:?}");
        }
    }
}

/// The http client's connector tries the resolved addresses of a host as
/// ureq's own TCP connector does (it made the connection before the
/// client made it itself, so that a stop ends a connect that waits).
#[cfg(all(test, feature = "http"))]
mod connect_each_tests {
    use super::*;
    use std::io::ErrorKind;
    use std::net::SocketAddr;

    fn connect_time(ms: u64) -> ureq::unversioned::transport::NextTimeout {
        ureq::unversioned::transport::NextTimeout {
            after: Duration::from_millis(ms).into(),
            reason: ureq::Timeout::Connect,
            per_read: None,
            per_write: None,
        }
    }

    fn addrs(n: u16) -> Vec<SocketAddr> {
        (1..=n)
            .map(|port| SocketAddr::from(([127, 0, 0, 1], port)))
            .collect()
    }

    /// Each address has a share of the connect's time, the first half as
    /// much again as the next: all of it for one address, 6.7 s and 3.3 s
    /// of 10 s for two; 5.3, 2.7, 1.3 and 0.7 s for four. An address that
    /// refused at once used none of its share: the next has the same.
    #[test]
    fn each_address_has_its_share_of_the_connect_time() {
        let shares = |n: u16, ms: u64, failure: ErrorKind| {
            let mut given = Vec::new();
            let _ = connect_each(&addrs(n), connect_time(ms), |_, share| {
                given.push(share.map(|s| s.as_millis()));
                Err::<(), _>(failure.into())
            });
            given
        };
        let timed_out = ErrorKind::TimedOut;
        assert_eq!(shares(1, 10_000, timed_out), [Some(10_000)]);
        assert_eq!(shares(2, 10_000, timed_out), [Some(6_666), Some(3_333)]);
        assert_eq!(
            shares(4, 10_000, timed_out),
            [Some(5_333), Some(2_666), Some(1_333), Some(666)]
        );
        let refused = ErrorKind::ConnectionRefused;
        assert_eq!(shares(2, 10_000, refused), [Some(6_666), Some(6_666)]);
        // No share is under 10 ms.
        assert_eq!(shares(2, 3, refused), [Some(10), Some(10)]);
    }

    /// The next address is tried after a connect that ran out of its
    /// share, and after a failure of that address only (refused, no
    /// route, no network); any other failure ends the connect, and so does
    /// a connect that ran out of the whole time.
    #[test]
    fn the_next_address_is_tried_after_a_failure_of_one_address() {
        let tried = |failure: ErrorKind, ms: u64| {
            let mut tried = Vec::new();
            let got = connect_each(&addrs(2), connect_time(ms), |addr, _| {
                tried.push(addr.port());
                match addr.port() {
                    1 => Err(failure.into()),
                    _ => Ok(addr.port()),
                }
            });
            (tried, got.ok())
        };
        for failure in [
            ErrorKind::TimedOut,
            ErrorKind::ConnectionRefused,
            ErrorKind::HostUnreachable,
            ErrorKind::NetworkUnreachable,
            ErrorKind::NetworkDown,
            ErrorKind::AddrNotAvailable,
        ] {
            assert_eq!(tried(failure, 10_000), (vec![1, 2], Some(2)), "{failure:?}");
        }
        assert_eq!(tried(ErrorKind::PermissionDenied, 10_000), (vec![1], None));
        // A first address that took the whole time: the connect timed out.
        let got = connect_each(&addrs(2), connect_time(1), |addr, _| match addr.port() {
            1 => {
                std::thread::sleep(Duration::from_millis(5));
                Err(ErrorKind::TimedOut.into())
            }
            _ => Ok(()),
        });
        assert!(
            matches!(got, Err(ureq::Error::Timeout(ureq::Timeout::Connect))),
            "{got:?}"
        );
        // Every address failed: the last failure is the connect's.
        let got = connect_each(&addrs(2), connect_time(10_000), |addr, _| {
            Err::<(), _>(match addr.port() {
                1 => ErrorKind::ConnectionRefused.into(),
                _ => std::io::Error::new(ErrorKind::NetworkUnreachable, "the last"),
            })
        });
        assert!(
            matches!(&got, Err(ureq::Error::Io(e)) if e.to_string() == "the last"),
            "{got:?}"
        );
    }

    /// Resolves every host name to the given addresses, in order. (For
    /// the test below, which runs on Linux only.)
    #[derive(Debug)]
    #[cfg(target_os = "linux")]
    struct Resolves(Vec<SocketAddr>);

    #[cfg(target_os = "linux")]
    impl ureq::unversioned::resolver::Resolver for Resolves {
        fn resolve(
            &self,
            _: &ureq::http::Uri,
            _: &ureq::config::Config,
            _: ureq::unversioned::transport::NextTimeout,
        ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
            let mut addrs = self.empty();
            for addr in &self.0 {
                addrs.push(*addr);
            }
            Ok(addrs)
        }
    }

    /// A host name whose first address does not answer (a listener whose
    /// queue is full: a connect to it waits) and whose second does is
    /// reached through the second, within the connect time: the first
    /// has two thirds of it. (Before, the first took all of it, and the
    /// request timed out.)
    #[test]
    #[cfg(target_os = "linux")]
    fn a_host_whose_first_address_does_not_answer_is_reached_through_the_next() {
        use std::io::{Read, Write};
        let full = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None)
            .expect("a socket");
        let any: SocketAddr = "127.0.0.1:0".parse().unwrap();
        full.bind(&any.into()).expect("bound");
        full.listen(0).expect("listening");
        let dead = full.local_addr().unwrap().as_socket().unwrap();
        let mut held = Vec::new();
        let waits = (0..64).any(|_| {
            match std::net::TcpStream::connect_timeout(&dead, Duration::from_millis(200)) {
                Ok(conn) => {
                    held.push(conn);
                    false
                }
                Err(e) => matches!(e.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock),
            }
        });
        assert!(waits, "a connect to the full queue waits");
        let live = std::net::TcpListener::bind("127.0.0.1:0").expect("a listener");
        let live_addr = live.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut conn, _)) = live.accept() {
                let _ = conn.read(&mut [0u8; 1024]);
                let _ = conn.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok");
            }
        });
        let client = client_resolving(
            Duration::from_millis(1500),
            Duration::from_secs(20),
            Resolves(vec![dead, live_addr]),
        );
        let started = std::time::Instant::now();
        let got = client.get("http://two.test/").call();
        let took = started.elapsed();
        let mut response = got.expect("the second address answers");
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.body_mut().read_to_string().expect("the body"),
            "ok"
        );
        // The first address had its share, 1 s of the 1.5 s.
        assert!(took >= Duration::from_millis(900), "{took:?}");
        drop(held);
    }
}

/// The client keeps a connection that the server keeps, and sends the
/// next request to that server on it; a kept connection that the server
/// closed is given up for a new one, and a request that went out on it
/// unanswered is sent once more. Every server here is a thread on the
/// loopback interface that does with each connection what its script
/// says.
#[cfg(all(test, feature = "http"))]
mod keep_alive_tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

    /// What a server does with one request of a connection.
    #[derive(Clone, Copy)]
    enum Reply {
        /// Answers `200` with this body, and keeps the connection.
        Answer(&'static str),
        /// Answers, and closes the connection without saying so.
        AnswerAndClose(&'static str),
        /// Reads the request and closes the connection without a word.
        Close,
        /// Reads the request and answers nothing for 30 s.
        Hold,
    }

    /// A server whose `n`th connection does with its requests what the
    /// `n`th script says (one past the scripts is closed at once), and
    /// the request lines that each connection got.
    struct Server {
        url: String,
        got: Arc<Mutex<Vec<Vec<String>>>>,
    }

    impl Server {
        fn new(scripts: Vec<Vec<Reply>>) -> Server {
            let listener = TcpListener::bind("127.0.0.1:0").expect("a listener");
            let url = format!("http://{}/", listener.local_addr().expect("its address"));
            let got: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
            let seen = got.clone();
            std::thread::spawn(move || {
                for (n, stream) in listener.incoming().enumerate() {
                    let Ok(stream) = stream else { return };
                    seen.lock().push(Vec::new());
                    let script = scripts.get(n).cloned().unwrap_or_default();
                    let seen = seen.clone();
                    std::thread::spawn(move || serve(stream, &script, n, &seen));
                }
            });
            Server { url, got }
        }

        /// How many connections the server took.
        fn connections(&self) -> usize {
            self.got.lock().len()
        }

        /// The request lines that each connection got.
        fn requests(&self) -> Vec<Vec<String>> {
            self.got.lock().clone()
        }
    }

    fn serve(mut stream: TcpStream, script: &[Reply], n: usize, seen: &Mutex<Vec<Vec<String>>>) {
        for reply in script {
            let Some(line) = read_request(&mut stream) else {
                return;
            };
            seen.lock()[n].push(line);
            let answer = |body: &str| {
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{body}",
                    body.len()
                )
            };
            match *reply {
                Reply::Answer(body) => {
                    let _ = stream.write_all(answer(body).as_bytes());
                }
                Reply::AnswerAndClose(body) => {
                    let _ = stream.write_all(answer(body).as_bytes());
                    return;
                }
                Reply::Close => return,
                Reply::Hold => {
                    std::thread::sleep(Duration::from_secs(30));
                    return;
                }
            }
        }
    }

    /// The request line of the next request of `stream`, read whole with
    /// its body (of a `content-length`); `None` at the end of the
    /// connection.
    fn read_request(stream: &mut TcpStream) -> Option<String> {
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            match stream.read(&mut byte) {
                Ok(1) => head.push(byte[0]),
                _ => return None,
            }
        }
        let head = String::from_utf8_lossy(&head).into_owned();
        let length = head
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = vec![0u8; length];
        stream.read_exact(&mut body).ok()?;
        head.lines().next().map(str::to_string)
    }

    /// A client of its own, as the program's is.
    fn agent() -> ureq::Agent {
        client_with_times(Duration::from_secs(5), Duration::from_secs(10))
    }

    /// What `method` of `url` with `body` comes to, as a call of its
    /// own of `agent`.
    fn ask(agent: &ureq::Agent, call: &Arc<Call>, method: &str, url: &str, body: &[u8]) -> String {
        let got = do_http_request_on(agent, call, method, &url_of(url), body, &[], true);
        answered(&got)
    }

    /// The body of the response in `got`, or the failure.
    fn answered(got: &Value) -> String {
        match got {
            Value::Variant(result) if result.is(bv::OK) => match result.fields() {
                [response] => match extract_http_response(response) {
                    Ok((_, body, _)) => String::from_utf8_lossy(body).into_owned(),
                    Err(e) => format!("no response: {e:?}"),
                },
                _ => format!("no response: {got}"),
            },
            _ => http_error_tests::failure(got),
        }
    }

    fn get(agent: &ureq::Agent, url: &str) -> String {
        ask(agent, &Arc::new(Call::default()), "GET", url, &[])
    }

    #[test]
    fn requests_to_one_server_go_on_one_connection() {
        let server = Server::new(vec![vec![Reply::Answer("ok"); 6]]);
        let agent = agent();
        let call = || Arc::new(Call::default());
        for _ in 0..3 {
            assert_eq!(get(&agent, &server.url), "ok");
        }
        assert_eq!(ask(&agent, &call(), "POST", &server.url, b"abc"), "ok");
        assert_eq!(ask(&agent, &call(), "PUT", &server.url, b""), "ok");
        let once = do_http_request_on(
            &agent,
            &call(),
            "GET",
            &url_of(&server.url),
            &[],
            &[],
            false,
        );
        assert_eq!(answered(&once), "ok");
        assert_eq!(server.connections(), 1, "{:?}", server.requests());
    }

    /// A server that closes a kept connection while it is idle (here
    /// after one answer, without saying so): the next request sees it
    /// closed and goes on a new one.
    #[test]
    fn a_kept_connection_the_server_closed_while_idle_is_given_up() {
        let server = Server::new(vec![
            vec![Reply::AnswerAndClose("one")],
            vec![Reply::Answer("two")],
        ]);
        let agent = agent();
        assert_eq!(get(&agent, &server.url), "one");
        // The server's close has arrived.
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(get(&agent, &server.url), "two");
        assert_eq!(server.connections(), 2);
    }

    /// A server that closes a kept connection just as the next request
    /// goes out on it, before a byte of the answer: a request of a method
    /// that may be sent twice is sent once more, on a new connection.
    #[test]
    fn a_request_that_meets_a_kept_connection_closed_is_sent_again_on_a_new_one() {
        for method in ["GET", "PUT", "DELETE", "OPTIONS"] {
            let server = Server::new(vec![
                vec![Reply::Answer("one"), Reply::Close],
                vec![Reply::Answer("two")],
            ]);
            let agent = agent();
            let call = || Arc::new(Call::default());
            assert_eq!(ask(&agent, &call(), method, &server.url, b"abc"), "one");
            assert_eq!(ask(&agent, &call(), method, &server.url, b"abc"), "two");
            let line = format!("{method} / HTTP/1.1");
            assert_eq!(
                server.requests(),
                vec![vec![line.clone(), line.clone()], vec![line]],
                "{method}"
            );
        }
    }

    /// A POST or a PATCH that meets a kept connection closed so is not
    /// sent again: the server may have acted on it. It is `ClosedEarly`.
    #[test]
    fn a_post_or_patch_that_meets_a_kept_connection_closed_is_closed_early() {
        for method in ["POST", "PATCH"] {
            let server = Server::new(vec![
                vec![Reply::Answer("one"), Reply::Close],
                vec![Reply::Answer("two")],
            ]);
            let agent = agent();
            let call = || Arc::new(Call::default());
            assert_eq!(ask(&agent, &call(), method, &server.url, b"abc"), "one");
            assert_eq!(
                ask(&agent, &call(), method, &server.url, b"abc"),
                "ClosedEarly"
            );
            std::thread::sleep(Duration::from_millis(100));
            let line = format!("{method} / HTTP/1.1");
            assert_eq!(
                server.requests(),
                vec![vec![line.clone(), line]],
                "{method}"
            );
        }
    }

    /// The same over TLS: a server that closes a kept connection as the
    /// next request goes out sends `close_notify` before it closes, a
    /// record of TLS and no byte of the answer, so the request is sent
    /// once more as over plain http. The client takes the server's
    /// self-signed certificate (verification off, in this test only).
    #[test]
    #[cfg(feature = "tcp-tls")]
    fn a_kept_https_connection_closed_with_close_notify_is_sent_again() {
        use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
            .expect("a certificate");
        let chain = vec![CertificateDer::from(cert.cert.der().to_vec())];
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()));
        let config = Arc::new(
            rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(chain, key)
                .expect("a server config"),
        );
        let listener = TcpListener::bind("127.0.0.1:0").expect("a listener");
        let url = format!("https://{}/", listener.local_addr().expect("its address"));
        let got: Arc<Mutex<Vec<usize>>> = Arc::default();
        let seen = got.clone();
        std::thread::spawn(move || {
            for (n, stream) in listener.incoming().enumerate() {
                let Ok(stream) = stream else { return };
                let conn = rustls::ServerConnection::new(config.clone()).expect("a connection");
                let mut tls = rustls::StreamOwned::new(conn, stream);
                seen.lock().push(0);
                // The first connection answers one request and closes
                // with close_notify when the second has come; the next
                // answers.
                let replies: &[Option<&str>] = match n {
                    0 => &[Some("one"), None],
                    _ => &[Some("two")],
                };
                for reply in replies {
                    if read_tls_request(&mut tls).is_none() {
                        break;
                    }
                    seen.lock()[n] += 1;
                    match reply {
                        Some(body) => {
                            let answer = format!(
                                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{body}",
                                body.len()
                            );
                            let _ = tls.write_all(answer.as_bytes());
                            let _ = tls.flush();
                        }
                        None => {
                            tls.conn.send_close_notify();
                            let _ = tls.flush();
                            break;
                        }
                    }
                }
            }
        });
        let tls = ureq::tls::TlsConfig::builder()
            .disable_verification(true)
            .build();
        let agent = client_of(
            Duration::from_secs(5),
            Duration::from_secs(10),
            ureq::unversioned::resolver::DefaultResolver::default(),
            tls,
        );
        assert_eq!(get(&agent, &url), "one");
        assert_eq!(get(&agent, &url), "two");
        assert_eq!(*got.lock(), vec![2, 1]);
    }

    /// The head of the next request of `tls`; `None` at its end.
    #[cfg(feature = "tcp-tls")]
    fn read_tls_request(tls: &mut impl Read) -> Option<()> {
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            match tls.read(&mut byte) {
                Ok(1) => head.push(byte[0]),
                _ => return None,
            }
        }
        Some(())
    }

    /// The request is sent again once: a new connection that ends
    /// unanswered too is the failure.
    #[test]
    fn a_request_is_sent_again_once_only() {
        let server = Server::new(vec![
            vec![Reply::Answer("one"), Reply::Close],
            vec![Reply::Close],
            vec![Reply::Answer("three")],
        ]);
        let agent = agent();
        assert_eq!(get(&agent, &server.url), "one");
        assert_eq!(get(&agent, &server.url), "ClosedEarly");
        assert_eq!(server.connections(), 2);
    }

    /// A new connection that ends before its answer is not sent again:
    /// only a kept one may have been closed while it was idle.
    #[test]
    fn a_request_on_a_new_connection_is_not_sent_again() {
        let server = Server::new(vec![vec![Reply::Close], vec![Reply::Answer("two")]]);
        assert_eq!(get(&agent(), &server.url), "ClosedEarly");
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(server.connections(), 1);
    }

    /// A stop of a call ends its request on a kept connection at once,
    /// and the client goes on with a new connection.
    #[test]
    fn a_stop_ends_a_request_on_a_kept_connection() {
        let server = Server::new(vec![
            vec![Reply::Answer("one"), Reply::Hold],
            vec![Reply::Answer("three")],
        ]);
        let agent = agent();
        assert_eq!(get(&agent, &server.url), "one");
        let call = Arc::new(Call::default());
        let stop = call.stop();
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            stop()
        });
        let started = std::time::Instant::now();
        let got = ask(&agent, &call, "GET", &server.url, &[]);
        assert!(started.elapsed() < Duration::from_secs(4), "{got}");
        assert!(
            stopper.join().expect("the stop"),
            "the stop ended the request"
        );
        assert_ne!(got, "one");
        assert_eq!(get(&agent, &server.url), "three");
    }

    /// A call that takes a kept connection from the pool uses it from that
    /// moment: a stop of the call that used it before, coming while that
    /// call still names it, shuts nothing of it. Here the connection is
    /// checked out by call B (ureq's check, `is_open`) before it writes,
    /// and A's stop comes in between.
    #[test]
    fn a_connection_taken_from_the_pool_is_the_taker_s_at_once() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a listener");
        let near = TcpStream::connect(listener.local_addr().unwrap()).expect("a connection");
        let (_far, _) = listener.accept().expect("accepted");
        let mut conn = Connected {
            stream: Arc::new(near),
            buffers: ureq::unversioned::transport::LazyBuffers::new(1024, 1024),
            timeout_read: None,
            timeout_write: None,
            line: Arc::new(Line::default()),
            served: 0,
        };
        let (a, b) = (Arc::new(Call::default()), Arc::new(Call::default()));
        in_call(&a, || {
            a.begin();
            conn.enlist().expect("A uses the connection");
            // A's request is done; B takes the connection from the pool.
            in_call(&b, || {
                b.begin();
                assert!(
                    ureq::unversioned::transport::Transport::is_open(&mut conn),
                    "open"
                );
            });
            assert_eq!(*conn.line.user.lock(), b.now.lock().id, "B uses it");
            // A's stop, while A still names the connection.
            assert!(a.stop()());
        });
        assert!(!conn.line.shutter.is_shut(), "A's stop shut B's connection");
    }

    /// The same through the client: call A's request is done and its
    /// connection back in the pool while A still names it; call B takes
    /// it for a request that the server answers after a while; A's stop
    /// in that while leaves B its answer.
    #[test]
    fn a_stop_of_one_call_leaves_the_next_call_its_connection() {
        let server = Server::new(vec![vec![Reply::Answer("one"), Reply::Answer("two")]]);
        let agent = Arc::new(agent());
        let a = Arc::new(Call::default());
        in_call(&a, || {
            let got = request_hops(&agent, &a, "GET", &url_of(&server.url), &[], &[], true);
            assert_eq!(answered(&got), "one");
            let (agent, url) = (agent.clone(), server.url.clone());
            let b = std::thread::spawn(move || get(&agent, &url));
            std::thread::sleep(Duration::from_millis(100));
            a.stop()();
            assert_eq!(b.join().expect("B"), "two");
        });
        assert_eq!(server.connections(), 1);
    }

    /// A stop that comes after its call has ended (nobody took the value)
    /// does not shut the connection, which went back to the client and
    /// serves the next call.
    #[test]
    fn a_stop_after_its_call_leaves_the_connection_to_the_next() {
        let server = Server::new(vec![vec![Reply::Answer("one"), Reply::Answer("two")]]);
        let agent = agent();
        let call = Arc::new(Call::default());
        assert_eq!(ask(&agent, &call, "GET", &server.url, &[]), "one");
        assert!(call.stop()());
        assert_eq!(get(&agent, &server.url), "two");
        assert_eq!(server.connections(), 1);
    }
}
