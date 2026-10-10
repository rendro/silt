//! The `http.*` builtin functions.

use std::collections::BTreeMap;
#[cfg(feature = "http")]
use std::collections::HashMap;
#[cfg(feature = "http")]
use std::sync::Arc;
#[cfg(feature = "http")]
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
#[cfg(feature = "http")]
use std::time::Duration;

use super::encoding::form_decode_component;
use super::typed::builtins;
#[cfg(feature = "http")]
use super::typed::{Arg, Map, TcpListener, unsound};
#[cfg(feature = "http")]
use crate::runtime::handle::{TaskHandle, TcpListenerHandle, TcpStreamHandle};
#[cfg(feature = "http")]
use crate::runtime::sync::{Arm, Cell, Fired, Wait};
#[cfg(feature = "http")]
use crate::typeinfo::{BuiltinVariant, bv, ty};
use crate::value::Value;
use crate::vm::VmError;
#[cfg(feature = "http")]
use crate::vm::{Step, Vm};
#[cfg(feature = "http")]
use parking_lot::Mutex;

/// What `HttpError`'s `message` says of the variant `tag` with `fields`:
/// `None` if they are no variant of it.
pub(crate) fn error_text(tag: &str, fields: &[Value]) -> Option<String> {
    Some(match (tag, fields) {
        ("HttpConnect", [Value::String(m)]) => format!("http connect failed: {m}"),
        ("HttpTls", [Value::String(m)]) => format!("http TLS error: {m}"),
        ("HttpTimeout", []) => "http request timed out".to_string(),
        ("HttpInvalidUrl", [Value::String(u)]) => format!("http invalid url: {u}"),
        ("HttpInvalidResponse", [Value::String(m)]) => {
            format!("http invalid response: {m}")
        }
        ("HttpClosedEarly", []) => "http connection closed before response completed".to_string(),
        ("HttpStatusCode", [Value::Int(code), Value::String(body)]) => {
            if body.is_empty() {
                format!("http status {code}")
            } else {
                format!("http status {code}: {body}")
            }
        }
        ("HttpUnknown", [Value::String(m)]) => m.to_string(),
        _ => return None,
    })
}

// ── HTTP dispatch ───────────────────────────────────────────────────

#[cfg(feature = "http")]
fn make_http_response(
    status: u16,
    headers: BTreeMap<Value, Value>,
    body: std::string::String,
) -> Value {
    Value::builtin_record(
        ty::RESPONSE,
        [
            ("status", Value::Int(status as i64)),
            ("body", Value::String(body.into())),
            ("headers", Value::Map(Arc::new(headers))),
        ],
    )
}

#[cfg(feature = "http")]
fn make_http_request_value(
    method: BuiltinVariant,
    path: &str,
    query: &str,
    headers: BTreeMap<Value, Value>,
    body: std::string::String,
) -> Value {
    Value::builtin_record(
        ty::REQUEST,
        [
            ("method", Value::variant(method, vec![])),
            ("path", Value::String(path.into())),
            ("query", Value::String(query.into())),
            ("headers", Value::Map(Arc::new(headers))),
            ("body", Value::String(body.into())),
        ],
    )
}

#[cfg(feature = "http")]
fn extract_http_response(
    val: &Value,
) -> Result<(u16, std::string::String, &crate::value::Record), VmError> {
    // (What is no `Response` is no value the handler's type has.)
    let fields = match val {
        Value::Record(record) if record.type_id() == ty::RESPONSE => record,
        _ => return Err(unsound("http.serve", "handler")),
    };
    let (Some(given), Some(body)) = (
        fields.get("status").and_then(i64::take),
        fields.get("body").and_then(<&str>::take),
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
    let body = body.to_string();
    Ok((status, body, fields))
}

#[cfg(feature = "http")]
fn ureq_response_to_value(
    mut response: ureq::http::Response<ureq::Body>,
) -> Result<Value, VmError> {
    let status = response.status().as_u16();
    let mut headers = BTreeMap::new();
    for (name, value) in response.headers().iter() {
        if let Ok(v) = value.to_str() {
            headers.insert(Value::String(name.as_str().into()), Value::String(v.into()));
        }
    }
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|e| VmError::new(format!("http: failed to read body: {e}")))?;
    Ok(make_http_response(status, headers, body))
}

/// Scrub URL userinfo (`user:password@`) from an error message.
///
/// `ureq::Error`'s Display impl can include the request URL, and if
/// the caller embedded credentials in the URL (`https://user:tok@h`),
/// those credentials leak into the `Err` string that's handed back
/// to the silt program (F12). Replace the userinfo segment with
/// `***@` for any `http://` / `https://` substring that carries one.
///
/// Grammar: the userinfo component of RFC 3986 is
/// `*( unreserved / pct-encoded / sub-delims / ":" )`. We accept a
/// conservative superset: alphanumerics, `%` escapes, and the common
/// URL-safe punctuation (`._~-+`) in the user segment, optionally
/// followed by `:<password>` where password is any non-`@`,
/// non-whitespace run. The `@` terminates userinfo.
#[cfg(feature = "http")]
pub fn redact_http_url_userinfo(msg: &str) -> String {
    let mut out = String::with_capacity(msg.len());
    let bytes = msg.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Look for scheme prefix at position `i`.
        let rest = &msg[i..];
        let scheme_len = if rest.starts_with("https://") {
            8
        } else if rest.starts_with("http://") {
            7
        } else {
            out.push(bytes[i] as char);
            i += 1;
            continue;
        };
        // Scan forward for `@` before any URL terminator char.
        // Valid userinfo chars: alphanumeric, `%`, `.`, `_`, `~`, `-`, `+`, `:`.
        let host_start = i + scheme_len;
        let mut j = host_start;
        let mut at_pos: Option<usize> = None;
        while j < bytes.len() {
            let b = bytes[j];
            // Terminators for the authority component.
            if b == b'@' {
                at_pos = Some(j);
                break;
            }
            if b == b'/' || b == b'?' || b == b'#' || b == b' ' || b == b'\t' || b == b'\n' {
                break;
            }
            // Accept any non-terminator byte (covers alphanumeric, `%`
            // escapes, `:` separator, and the `._~-+` URL-safe set).
            j += 1;
        }
        // Copy scheme verbatim.
        out.push_str(&msg[i..host_start]);
        if let Some(at) = at_pos {
            // Only scrub if we actually have content before the `@`
            // (otherwise `scheme://@host` stays as-is).
            if at > host_start {
                out.push_str("***@");
                i = at + 1; // skip the original userinfo + `@`
                continue;
            }
        }
        i = host_start;
    }
    out
}

/// Classify a ureq error (or any transport-style error string) into a
/// typed `HttpError` variant. The classifier is string-based because
/// ureq's error enum shape is minor-version-unstable; we match on
/// the rendered message and fall back to `HttpUnknown`.
#[cfg(feature = "http")]
/// The error of an `http` function whose operation has no value of
/// its own: `HttpTimeout` when its deadline passed, `HttpUnknown` with
/// the reason when it could not run or panicked.
#[cfg(feature = "http")]
fn http_timeout_err(failure: crate::vm::IoFailure<'_>) -> Value {
    use crate::vm::IoFailure;
    let error = match failure {
        IoFailure::Timeout(_) => Value::variant(bv::HTTP_TIMEOUT, vec![]),
        IoFailure::Panicked(why) | IoFailure::Refused(why) => {
            Value::variant(bv::HTTP_UNKNOWN, vec![Value::String(why.into())])
        }
    };
    Value::variant(bv::ERR, vec![error])
}

#[cfg(feature = "http")]
fn http_error_to_variant(raw_msg: &str, url: &str) -> Value {
    let msg = redact_http_url_userinfo(raw_msg);
    let lower = msg.to_lowercase();
    let url_redacted = redact_http_url_userinfo(url);
    if lower.contains("timed out") || lower.contains("timeout") {
        Value::variant(bv::HTTP_TIMEOUT, vec![])
    } else if lower.contains("invalid url") || lower.contains("not a valid url") {
        Value::variant(
            bv::HTTP_INVALID_URL,
            vec![Value::String(url_redacted.into())],
        )
    } else if lower.contains("tls") || lower.contains("certificate") || lower.contains("handshake")
    {
        Value::variant(bv::HTTP_TLS, vec![Value::String(msg.into())])
    } else if lower.contains("bad status")
        || lower.contains("invalid response")
        || lower.contains("bad header")
    {
        Value::variant(bv::HTTP_INVALID_RESPONSE, vec![Value::String(msg.into())])
    } else if lower.contains("connection closed")
        || lower.contains("unexpected eof")
        || lower.contains("closed before")
    {
        Value::variant(bv::HTTP_CLOSED_EARLY, vec![])
    } else if lower.contains("resolve")
        || lower.contains("refused")
        || lower.contains("no such host")
        || lower.contains("network unreachable")
        || lower.contains("connect")
    {
        Value::variant(bv::HTTP_CONNECT, vec![Value::String(msg.into())])
    } else {
        Value::variant(bv::HTTP_UNKNOWN, vec![Value::String(msg.into())])
    }
}

/// Wrap a ureq error in `Err(HttpError)`.
#[cfg(feature = "http")]
fn http_err(raw_msg: &str, url: &str) -> Value {
    Value::variant(bv::ERR, vec![http_error_to_variant(raw_msg, url)])
}

/// Wrap a response-body decode failure as `Err(HttpInvalidResponse(msg))`.
#[cfg(feature = "http")]
fn http_response_decode_err(msg: String) -> Value {
    Value::variant(
        bv::ERR,
        vec![Value::variant(
            bv::HTTP_INVALID_RESPONSE,
            vec![Value::String(msg.into())],
        )],
    )
}

/// Convert a completed ureq request `Result` into the silt `Value` returned
/// to the program. Shared by `do_http_get` and `do_http_request`, which had
/// byte-identical match blocks here. Behavior: on transport success, decode
/// the response (success → `Ok(resp)`, body-read failure →
/// `Err(HttpInvalidResponse(..))`); on transport error → `Err(HttpError(..))`.
#[cfg(feature = "http")]
fn finish_http_response(
    result: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    url: &str,
) -> Value {
    match result {
        Ok(response) => match ureq_response_to_value(response) {
            Ok(resp) => Value::variant(bv::OK, vec![resp]),
            Err(e) => http_response_decode_err(e.message),
        },
        Err(e) => http_err(&format!("{e}"), url),
    }
}

/// Perform a synchronous HTTP GET and return a `Value`.
#[cfg(feature = "http")]
fn do_http_get(url: &str) -> Value {
    // Security: conservative default timeouts so a slow/unreachable peer
    // cannot hang the underlying OS socket indefinitely (HIGH-3). These
    // apply even if the silt task's SILT_IO_TIMEOUT unblocks the VM task,
    // so we don't leak real file descriptors.
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_global(Some(Duration::from_secs(60)))
        .build()
        .into();
    finish_http_response(agent.get(url).call(), url)
}

/// Perform a synchronous HTTP request and return a `Value`.
#[cfg(feature = "http")]
fn do_http_request(method_tag: &str, url: &str, body: &str, headers: &[(String, String)]) -> Value {
    // Security: conservative default timeouts (HIGH-3). See do_http_get.
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_global(Some(Duration::from_secs(60)))
        .build()
        .into();

    // Unified dispatch — each ureq verb-fn returns a differently-typed
    // `RequestBuilder<WithBody>` vs `RequestBuilder<WithoutBody>` (phantom
    // type carries whether a body is expected), so we can't bind a
    // single variable to the builder. Two small local macros collapse
    // the prior 7 near-identical arms into one header-apply + send line
    // per family. Semantics preserved: same header-setting order, same
    // send_empty()/send(body) split for body verbs, same call() for
    // no-body verbs (GET with a body is still a no-op, matching the
    // prior silt API surprise).
    macro_rules! with_body {
        ($verb:ident) => {{
            let mut req = agent.$verb(url);
            for (key, val) in headers {
                req = req.header(key.as_str(), val.as_str());
            }
            if body.is_empty() {
                req.send_empty()
            } else {
                req.send(body)
            }
        }};
    }
    macro_rules! no_body {
        ($verb:ident) => {{
            let mut req = agent.$verb(url);
            for (key, val) in headers {
                req = req.header(key.as_str(), val.as_str());
            }
            req.call()
        }};
    }

    let result = match method_tag {
        "POST" => with_body!(post),
        "PUT" => with_body!(put),
        "PATCH" => with_body!(patch),
        "GET" => no_body!(get),
        "DELETE" => no_body!(delete),
        "HEAD" => no_body!(head),
        "OPTIONS" => no_body!(options),
        other => {
            return Value::variant(
                bv::ERR,
                vec![Value::variant(
                    bv::HTTP_INVALID_URL,
                    vec![Value::String(format!("unknown method: {other}").into())],
                )],
            );
        }
    };

    finish_http_response(result, url)
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
    /// How many handlers are being called.
    handlers: AtomicUsize,
    /// How many bytes of bodies the server holds for requests that no
    /// handler has yet ([`wire::BODIES_MAX`]).
    bodies: AtomicUsize,
    /// The tasks of the connections that have not ended; `None` once
    /// the server has ended: no connection goes on then.
    conns: Mutex<Option<HashMap<usize, Arc<TaskHandle>>>>,
}

#[cfg(feature = "http")]
impl Server {
    /// Count one more handler as being called; `None` at the bound.
    fn call(self: &Arc<Self>) -> Option<Called> {
        let mut handlers = self.handlers.load(Ordering::Acquire);
        loop {
            if handlers >= wire::HANDLERS_MAX {
                return None;
            }
            let counted = self.handlers.compare_exchange_weak(
                handlers,
                handlers + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            match counted {
                Ok(_) => return Some(Called(self.clone())),
                Err(now) => handlers = now,
            }
        }
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
    /// Made on the thread that read the request: a body of megabytes
    /// is not turned into a string on a worker of the scheduler.
    /// `held` is what was counted for the request's body: it goes
    /// with a request for the handler, and ends here otherwise.
    fn of(next: wire::Next, held: Held) -> Got {
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
        let (path, query) = match request.target.split_once('?') {
            Some((path, query)) => (path, query),
            None => (request.target.as_str(), ""),
        };
        let mut headers = BTreeMap::new();
        for (name, value) in request.headers {
            headers.insert(Value::String(name.into()), Value::String(value.into()));
        }
        // The Request API hands the body over as a String: bytes that
        // are not UTF-8 are replaced, not dropped. (A body that is
        // UTF-8 is the string, not a copy of it.)
        let body = std::string::String::from_utf8(request.body)
            .unwrap_or_else(|e| std::string::String::from_utf8_lossy(e.as_bytes()).into_owned());
        Got::Request(
            make_http_request_value(method, path, query, headers, body),
            reply,
            held,
        )
    }
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

/// What a body is unless the handler says otherwise.
#[cfg(feature = "http")]
const TEXT: &str = "text/plain; charset=UTF-8";

/// A response of the server's own: a status, and a word as its body.
#[cfg(feature = "http")]
fn plain_response(vm: &Vm, status: u16, why: &str, reply: Reply) -> Vec<u8> {
    let mut headers = vec![("Content-Type".to_string(), TEXT.to_string())];
    // The methods that are allowed are those of `Method`.
    if status == 405 {
        let allowed = "GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS";
        headers.push(("Allow".to_string(), allowed.to_string()));
    }
    response_bytes(vm, status, &headers, why.as_bytes(), reply)
}

/// The response that the handler returned, or 500 if what it returned
/// is no response.
#[cfg(feature = "http")]
fn handler_response(vm: &Vm, returned: &Value, reply: Reply) -> Vec<u8> {
    match extract_http_response(returned) {
        Ok((status, body, fields)) => {
            let mut headers = Vec::new();
            if let Some(Value::Map(given)) = fields.get("headers") {
                for (name, value) in given.iter() {
                    if let (Value::String(name), Value::String(value)) = (name, value) {
                        headers.push((name.to_string(), value.to_string()));
                    }
                }
            }
            let typed = |(name, _): &(String, String)| name.eq_ignore_ascii_case("content-type");
            if wire::has_body(status) && !headers.iter().any(typed) {
                headers.push(("Content-Type".to_string(), TEXT.to_string()));
            }
            response_bytes(vm, status, &headers, body.as_bytes(), reply)
        }
        Err(e) => {
            // Security: don't leak VmError contents (call stack, line
            // numbers, internal function names, possibly-sensitive panic
            // payloads) over the HTTP wire (MED-1). Log internally,
            // respond generically.
            vm.runtime.io.err(&format!(
                "http.serve: handler returned malformed Response: {e}\n"
            ));
            plain_response(vm, 500, wire::reason(500), reply)
        }
    }
}

/// The task of one connection: read a request, call the handler with
/// it, send what the handler returns, and go on with the next request
/// while the connection is kept.
///
/// What is there to be read, and what the system takes of a response
/// at once, is read and written where the task runs, without waiting
/// ([`TcpStreamHandle::read_now`], [`TcpStreamHandle::write_now`]): a
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
    },
    /// The handler is being called.
    Calling { reply: Reply, called: Called },
    /// A response is to be sent.
    Answering { bytes: Vec<u8>, then: Then },
    /// A response is being sent.
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
        let got = |next| Got::of(next, Held(self.server.clone(), 0));
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

    /// Do what a read gave asks for: call the handler, or answer.
    fn serve(&mut self, vm: &mut Vm, got: Got) -> Go {
        self.served += 1;
        match got {
            Got::Request(request, reply, held) => match self.server.call() {
                Some(called) => {
                    // The body is the handler's now.
                    drop(held);
                    self.state = ConnState::Calling { reply, called };
                    Go::Step(vm.call(self.server.handler.clone(), [request]))
                }
                // As many handlers as the server calls at a time are
                // being called.
                None => {
                    let bytes = plain_response(vm, 503, wire::reason(503), reply);
                    self.send(vm, bytes, reply.then())
                }
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
        let scheduler = vm.scheduler().clone();
        let op = vm
            .runtime
            .io_pool
            .submit(not_served, move || {
                let mut before_body = |waits: bool| {
                    let _ = announced.complete((), scheduler.wake());
                    match waits {
                        true => stream.write_all(wire::CONTINUE),
                        false => Ok(()),
                    }
                };
                let next = reader.lock().next(&mut before_body);
                // What the reader had counted for the body.
                let held = Held(server, pending.swap(0, Ordering::AcqRel));
                *result.lock() = Some(Got::of(next, held));
                Value::Unit
            })
            .stop_with(move || stopped.shut_down());
        let wait = Wait::new(vec![Arm::Cell(op.cell.clone()), Arm::Cell(body_follows)])
            .deadline(vm.runtime.io.deadline_after(wire::REQUEST_TIME));
        self.state = ConnState::Reading {
            op,
            got,
            in_body: false,
        };
        Step::Park(wait)
    }

    /// Send `bytes`, and go on with `then`. What the system takes at
    /// once is written here; if that is not all of it, the task waits
    /// until the client has taken the rest, for at most
    /// [`wire::TRANSFER_TIME`].
    fn send(&mut self, vm: &mut Vm, bytes: Vec<u8>, then: Then) -> Go {
        let written = self.stream.write_now(&bytes);
        if written == bytes.len() {
            return self.sent(vm, then);
        }
        // A task that was cancelled meanwhile (the server has ended)
        // waits for nothing more.
        if self.handle.is_cancelled() {
            return Conn::END;
        }
        let (stream, stopped) = (self.stream.clone(), self.stream.clone());
        let op = vm
            .runtime
            .io_pool
            .submit(not_served, move || {
                Value::Bool(stream.write_all(&bytes[written..]).is_ok())
            })
            .stop_with(move || stopped.shut_down());
        let wait = Wait::new(vec![Arm::Cell(op.cell.clone())])
            .deadline(vm.runtime.io.deadline_after(wire::TRANSFER_TIME));
        self.state = ConnState::Sending { op, then };
        Go::Step(Step::Park(wait))
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
        let op = vm
            .runtime
            .io_pool
            .submit(not_served, move || {
                stream.end_writes();
                let mut dropped = [0u8; 8 * 1024];
                while matches!(stream.read(&mut dropped), Ok(n) if n > 0) {}
                Value::Unit
            })
            .stop_with(move || stopped.shut_down());
        let wait = Wait::new(vec![Arm::Cell(op.cell.clone())])
            .deadline(vm.runtime.io.deadline_after(wire::REFUSAL_TIME));
        self.state = ConnState::Draining(op);
        Step::Park(wait)
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
            // A task that was cancelled (the server has ended)
            // serves no further request.
            ConnState::Next if self.handle.is_cancelled() => Conn::END,
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
            ConnState::Reading { op, got, in_body } => match vm.woken()? {
                // The head is there: the body has its own time.
                Fired::Arm(1, _) if !in_body => {
                    let wait = Wait::new(vec![Arm::Cell(op.cell.clone())])
                        .deadline(vm.runtime.io.deadline_after(wire::TRANSFER_TIME));
                    self.state = ConnState::Reading {
                        op,
                        got,
                        in_body: true,
                    };
                    Go::Step(Step::Park(wait))
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
                        let _ = self.stream.write_now(&bytes);
                        self.stream.end_writes();
                        let wait = Wait::new(vec![Arm::Cell(op.cell.clone())])
                            .deadline(vm.runtime.io.deadline_after(wire::REFUSAL_TIME));
                        self.state = ConnState::Draining(op);
                        Go::Step(Step::Park(wait))
                    }
                },
            },
            ConnState::Calling { reply, called } => {
                // The handler has returned.
                drop(called);
                let bytes = handler_response(vm, &input, reply);
                self.send(vm, bytes, reply.then())
            }
            ConnState::Answering { bytes, then } => self.send(vm, bytes, then),
            ConnState::Sending { op, then } => {
                let sent = matches!(vm.woken()?, Fired::Arm(..))
                    && matches!(op.take(), Some(Value::Bool(true)));
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
                return Err(VmError::new(
                    "internal VM error: the task of an HTTP connection was resumed after its end"
                        .into(),
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
        // request that is in flight is answered; nothing here waits,
        // so the answer is what the system takes at once. (A program
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
                    let _ = self.stream.write_now(&bytes);
                    return;
                };
                // The failure is handled here: it is logged, and not
                // reported as a task that nobody joined.
                self.handle.mark_joined();
                // Security: do NOT include VmError details (call stack,
                // line numbers, panic payload) in the response body — that
                // leaks implementation details and potentially sensitive
                // values across the security boundary (MED-1). Log to the
                // host's stderr instead.
                vm.runtime
                    .io
                    .err(&format!("http.serve: handler error: {e}\n"));
                if !self.go_on_in_a_new_task(vm, reply) && !self.handed_on {
                    let bytes = plain_response(vm, 500, wire::reason(500), unavailable);
                    let _ = self.stream.write_now(&bytes);
                }
            }
            // The answer that a task took over and could not send.
            ConnState::Answering { bytes, .. } => {
                let _ = self.stream.write_now(&bytes);
            }
            ConnState::Reading { in_body: true, .. } => {
                let bytes = plain_response(vm, 503, wire::reason(503), unavailable);
                let _ = self.stream.write_now(&bytes);
            }
            // Nothing is in flight: between requests, or the response
            // is on its way.
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
        if let Some(conns) = self.server.conns.lock().as_mut() {
            conns.remove(&self.handle.id);
        }
    }
}

/// `http.serve` itself, in the task that called it: accept a
/// connection, start its task, accept the next. It ends when its task
/// does (cancelled, or dropped at the end of the program), and the
/// connections with it.
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
        child.push_native_frame(Box::new(Conn {
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
            ServeState::Accepting(op) => {
                vm.woken()?;
                let accepted = op.take();
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
                    return Ok(Step::Park(wait));
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
        let wait = Wait::new(vec![Arm::Cell(op.cell.clone())]);
        self.state = ServeState::Accepting(op);
        Ok(Step::Park(wait))
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
    fn get(vm, url: &str) -> Result<Step, VmError> {
        let url = url.to_string();
        vm.io("http", http_timeout_err, move || do_http_get(&url))
    }

    #[cfg(feature = "http")]
    fn request(vm, method: Method, url: &str, body: &str, headers: Map) -> Result<Step, VmError> {
        let (method, url, body) = (method.0.to_string(), url.to_string(), body.to_string());
        let headers: Vec<(String, String)> = headers
            .iter()
            .filter_map(|(k, v)| Some((<&str>::take(k)?.to_string(), <&str>::take(v)?.to_string())))
            .collect();
        vm.io("http", http_timeout_err, move || {
            do_http_request(&method, &url, &body, &headers)
        })
    }

    // Serves HTTP on a listener that `tcp.listen` bound. Which
    // interfaces the server is reached on, and on which port, is what
    // was written there.
    #[cfg(feature = "http")]
    fn serve(vm, listener: TcpListener, handler: &Value) -> Result<Step, VmError> {
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
                bodies: AtomicUsize::new(0),
                conns: Mutex::new(Some(HashMap::new())),
            }),
            listener: listener.clone(),
            token,
            scheduler: Arc::downgrade(vm.scheduler()),
            state: ServeState::Start,
            failing: false,
        })))
    }

    fn segments(path: &str) -> Vec<Value> {
        path.split('/')
            .filter(|s| !s.is_empty())
            .map(|s| Value::String(s.into()))
            .collect()
    }

    fn parse_query(query: &str) -> Result<BTreeMap<Value, Value>, VmError> {
        // Accept a leading `?` for convenience — e.g. directly
        // passing a URL fragment like `?a=1&b=2` shouldn't require
        // the caller to strip it first.
        let body = query.strip_prefix('?').unwrap_or(query);
        // Preserve insertion order of first appearance for each
        // key. BTreeMap gives us stable ordering by key, which is
        // fine for a value-semantic Map — repeated keys always
        // append to the same List in encounter order.
        let mut out: BTreeMap<Value, Vec<Value>> = BTreeMap::new();
        for (i, segment) in body.split('&').enumerate() {
            // Empty segments (leading `&`, `&&`, trailing `&`) are
            // skipped, matching WHATWG's form-urlencoded parser and
            // `encoding.form_decode`.
            if segment.is_empty() {
                continue;
            }
            // Split on the FIRST `=`. Missing `=` → value is "".
            // The spec says a bare key with no separator means
            // "present with empty value", which matches how forms
            // serialize a checkbox with value "".
            let (raw_key, raw_val) = segment.split_once('=').unwrap_or((segment, ""));
            let key = form_decode_component(raw_key)
                .map_err(|msg| VmError::new(format!("http.parse_query: pair {i} key: {msg}")))?;
            let val = form_decode_component(raw_val)
                .map_err(|msg| VmError::new(format!("http.parse_query: pair {i} value: {msg}")))?;
            out.entry(Value::String(key.into())).or_default().push(Value::String(val.into()));
        }
        Ok(out
            .into_iter()
            .map(|(key, values)| (key, Value::list(values)))
            .collect())
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
                ("body", Value::String(String::new().into())),
                ("headers", Value::Map(Arc::new(BTreeMap::new()))),
            ],
        )
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
