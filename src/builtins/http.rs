//! The `http.*` builtin functions.

use std::collections::BTreeMap;
use std::sync::Arc;
#[cfg(feature = "http")]
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(feature = "http")]
use std::time::Duration;

use super::common::value_kind;
#[cfg(feature = "http")]
use crate::bytecode::record_type_matches;
#[cfg(feature = "http")]
use crate::runtime::handle::TaskHandle;
#[cfg(feature = "http")]
use crate::runtime::sync::{Arm, Wait};
#[cfg(feature = "http")]
use crate::typeinfo::{BuiltinVariant, bv, ty};
use crate::value::Value;
use crate::vm::{Step, Vm, VmError};
#[cfg(feature = "http")]
use parking_lot::Mutex;

/// Dispatch the builtin `trait Error for HttpError` method table.
/// Scaffolding lives in `super::dispatch_error_trait`; this site just
/// supplies the variant → message rendering.
pub fn call_http_error_trait(name: &str, args: &[Value]) -> Result<Value, VmError> {
    super::dispatch_error_trait("HttpError", name, args, |tag, fields| {
        Some(match (tag, fields) {
            ("HttpConnect", [Value::String(m)]) => format!("http connect failed: {m}"),
            ("HttpTls", [Value::String(m)]) => format!("http TLS error: {m}"),
            ("HttpTimeout", []) => "http request timed out".to_string(),
            ("HttpInvalidUrl", [Value::String(u)]) => format!("http invalid url: {u}"),
            ("HttpInvalidResponse", [Value::String(m)]) => {
                format!("http invalid response: {m}")
            }
            ("HttpClosedEarly", []) => {
                "http connection closed before response completed".to_string()
            }
            ("HttpStatusCode", [Value::Int(code), Value::String(body)]) => {
                if body.is_empty() {
                    format!("http status {code}")
                } else {
                    format!("http status {code}: {body}")
                }
            }
            ("HttpUnknown", [Value::String(m)]) => m.clone(),
            _ => return None,
        })
    })
}

// ── HTTP dispatch ───────────────────────────────────────────────────

#[cfg(feature = "http")]
fn make_http_response(
    status: u16,
    headers: BTreeMap<Value, Value>,
    body: std::string::String,
) -> Value {
    let mut fields = BTreeMap::new();
    fields.insert("status".into(), Value::Int(status as i64));
    fields.insert("body".into(), Value::String(body));
    fields.insert("headers".into(), Value::Map(Arc::new(headers)));
    Value::builtin_record(ty::RESPONSE, fields)
}

#[cfg(feature = "http")]
fn make_http_request_value(
    method: BuiltinVariant,
    path: &str,
    query: &str,
    headers: BTreeMap<Value, Value>,
    body: std::string::String,
) -> Value {
    let mut fields = BTreeMap::new();
    fields.insert("method".into(), Value::variant(method, vec![]));
    fields.insert("path".into(), Value::String(path.into()));
    fields.insert("query".into(), Value::String(query.into()));
    fields.insert("headers".into(), Value::Map(Arc::new(headers)));
    fields.insert("body".into(), Value::String(body));
    Value::builtin_record(ty::REQUEST, fields)
}

#[cfg(feature = "http")]
fn extract_http_response(
    val: &Value,
) -> Result<
    (
        u16,
        std::string::String,
        &BTreeMap<std::string::String, Value>,
    ),
    VmError,
> {
    let Value::Record(name, fields) = val else {
        return Err(VmError::new("handler must return a Response record".into()));
    };
    if !record_type_matches(name, ty::RESPONSE) {
        return Err(VmError::new(format!(
            "handler must return Response, got {}",
            name.name
        )));
    }
    let status = match fields.get("status") {
        Some(Value::Int(n)) => match u16::try_from(*n) {
            Ok(s) => s,
            Err(_) => {
                return Err(VmError::new(format!(
                    "Response.status out of range: {n} is not a valid HTTP status (0..=65535)"
                )));
            }
        },
        Some(other) => {
            return Err(VmError::new(format!(
                "Response.status requires Int, got {}",
                value_kind(other)
            )));
        }
        None => return Err(VmError::new("Response.status missing".into())),
    };
    let body = match fields.get("body") {
        Some(Value::String(s)) => s.clone(),
        Some(other) => {
            return Err(VmError::new(format!(
                "Response.body requires String, got {}",
                value_kind(other)
            )));
        }
        None => return Err(VmError::new("Response.body missing".into())),
    };
    Ok((status, body, fields))
}

/// Max number of bytes accepted in an HTTP request body by `http.serve`.
/// 10 MiB — large enough for typical form posts and JSON payloads, small
/// enough that a single unauthenticated client cannot OOM the server
/// (HIGH-1). Larger uploads must use chunked/streaming handlers, which
/// the current API does not expose.
#[cfg(feature = "http")]
const HTTP_SERVE_MAX_BODY_BYTES: u64 = 10 * 1024 * 1024;

/// How long `server.recv_timeout` will block waiting for the next request
/// before looping. The accept loop re-checks the shutdown flag each time,
/// so this bounds how long shutdown takes. It does NOT per-connection
/// cap slow header reads inside tiny_http's internal pool (that is a
/// library limitation — see HIGH-2 notes).
#[cfg(feature = "http")]
const HTTP_SERVE_RECV_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum number of concurrent request handler threads spawned by
/// `http.serve`. Requests beyond this cap are rejected with HTTP 503
/// so a slowloris / burst cannot force unbounded thread spawning.
#[cfg(feature = "http")]
const HTTP_SERVE_MAX_CONCURRENT_HANDLERS: usize = 128;

/// Extract a silt Response record and send it as an HTTP response.
/// Used by the per-request handler threads in `http.serve`.
#[cfg(feature = "http")]
fn send_http_response(io: &crate::vm::HostIo, response_val: &Value, req: tiny_http::Request) {
    match extract_http_response(response_val) {
        Ok((status, resp_body, resp_fields)) => {
            let mut response = tiny_http::Response::from_string(&resp_body)
                .with_status_code(tiny_http::StatusCode(status));

            if let Some(Value::Map(resp_headers)) = resp_fields.get("headers") {
                for (k, v) in resp_headers.iter() {
                    if let (Value::String(key), Value::String(val)) = (k, v)
                        && let Ok(header) =
                            tiny_http::Header::from_bytes(key.as_bytes(), val.as_bytes())
                    {
                        response = response.with_header(header);
                    }
                }
            }

            let _ = req.respond(response);
        }
        Err(e) => {
            // Security: don't leak VmError contents (call stack, line
            // numbers, internal function names, possibly-sensitive panic
            // payloads) over the HTTP wire (MED-1). Log internally,
            // respond generically.
            io.err(&format!(
                "http.serve: handler returned malformed Response: {e}\n"
            ));
            let resp = tiny_http::Response::from_string("Internal Server Error")
                .with_status_code(tiny_http::StatusCode(500));
            let _ = req.respond(resp);
        }
    }
}

#[cfg(feature = "http")]
fn ureq_response_to_value(
    mut response: ureq::http::Response<ureq::Body>,
) -> Result<Value, VmError> {
    let status = response.status().as_u16();
    let mut headers = BTreeMap::new();
    for (name, value) in response.headers().iter() {
        if let Ok(v) = value.to_str() {
            headers.insert(
                Value::String(name.as_str().to_string()),
                Value::String(v.to_string()),
            );
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
/// Factory: deadline-cancelled http op surfaces as `Err(HttpTimeout)`
/// rather than `Err(IoUnknown(_))`. It is given the deadline message,
/// which is dropped because `HttpTimeout` is a nullary variant. Used by http.get / http.request submits.
#[cfg(feature = "http")]
fn http_timeout_err(_msg: &str) -> Value {
    Value::variant(bv::ERR, vec![Value::variant(bv::HTTP_TIMEOUT, vec![])])
}

#[cfg(feature = "http")]
fn http_error_to_variant(raw_msg: &str, url: &str) -> Value {
    let msg = redact_http_url_userinfo(raw_msg);
    let lower = msg.to_lowercase();
    let url_redacted = redact_http_url_userinfo(url);
    if lower.contains("timed out") || lower.contains("timeout") {
        Value::variant(bv::HTTP_TIMEOUT, vec![])
    } else if lower.contains("invalid url") || lower.contains("not a valid url") {
        Value::variant(bv::HTTP_INVALID_URL, vec![Value::String(url_redacted)])
    } else if lower.contains("tls") || lower.contains("certificate") || lower.contains("handshake")
    {
        Value::variant(bv::HTTP_TLS, vec![Value::String(msg)])
    } else if lower.contains("bad status")
        || lower.contains("invalid response")
        || lower.contains("bad header")
    {
        Value::variant(bv::HTTP_INVALID_RESPONSE, vec![Value::String(msg)])
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
        Value::variant(bv::HTTP_CONNECT, vec![Value::String(msg)])
    } else {
        Value::variant(bv::HTTP_UNKNOWN, vec![Value::String(msg)])
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
            vec![Value::String(msg)],
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
                    vec![Value::String(format!("unknown method: {other}"))],
                )],
            );
        }
    };

    finish_http_response(result, url)
}

/// Decrements the count of requests in flight when the task of one
/// ends, however it ends.
#[cfg(feature = "http")]
struct Decrement(Arc<AtomicUsize>);

#[cfg(feature = "http")]
impl Drop for Decrement {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The request of a handler's task, until it is answered.
#[cfg(feature = "http")]
type Pending = Arc<Mutex<Option<tiny_http::Request>>>;

/// Read the request for the handler: the `Request` value it is called
/// with. `None` when the request was turned away here (and answered):
/// a method the server does not know, a body that is too large.
/// Reading the body blocks, so this runs on the I/O pool.
#[cfg(feature = "http")]
fn read_request(pending: &Pending) -> Option<Value> {
    let mut slot = pending.lock();
    let mut req = slot.take()?;
    // Parse the HTTP method
    let method = match req.method() {
        tiny_http::Method::Get => bv::GET,
        tiny_http::Method::Post => bv::POST,
        tiny_http::Method::Put => bv::PUT,
        tiny_http::Method::Patch => bv::PATCH,
        tiny_http::Method::Delete => bv::DELETE,
        tiny_http::Method::Head => bv::HEAD,
        tiny_http::Method::Options => bv::OPTIONS,
        _ => {
            let resp = tiny_http::Response::from_string("Method Not Allowed")
                .with_status_code(tiny_http::StatusCode(405));
            let _ = req.respond(resp);
            return None;
        }
    };

    // Parse URL into path and query
    let url = req.url().to_string();
    let (path, query) = match url.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (url, std::string::String::new()),
    };

    // Collect headers
    let mut headers = BTreeMap::new();
    for header in req.headers() {
        headers.insert(
            Value::String(header.field.as_str().to_string()),
            Value::String(header.value.as_str().to_string()),
        );
    }

    // Fast-reject oversized bodies based on the
    // declared Content-Length. Prevents a client from
    // forcing us to consume the whole body just to
    // discover we'd reject it. (HIGH-1)
    if let Some(declared) = req.body_length()
        && declared as u64 > HTTP_SERVE_MAX_BODY_BYTES
    {
        let resp = tiny_http::Response::from_string("Payload Too Large")
            .with_status_code(tiny_http::StatusCode(413));
        let _ = req.respond(resp);
        return None;
    }

    // Read body with a hard cap. `take(N+1)` + length
    // check lets us detect overrun (e.g. chunked
    // encoding that lies about total length). (HIGH-1)
    let mut body_bytes: Vec<u8> = Vec::new();
    let cap = HTTP_SERVE_MAX_BODY_BYTES;
    let read_result = std::io::Read::read_to_end(
        &mut std::io::Read::take(req.as_reader(), cap + 1),
        &mut body_bytes,
    );
    if read_result.is_err() || body_bytes.len() as u64 > cap {
        let resp = tiny_http::Response::from_string("Payload Too Large")
            .with_status_code(tiny_http::StatusCode(413));
        let _ = req.respond(resp);
        return None;
    }
    // The Request API hands us body as a String; we
    // lossy-convert so non-UTF-8 bodies don't silently
    // drop. Handlers that need raw bytes should use
    // a separate API (future work).
    let body = std::string::String::from_utf8_lossy(&body_bytes).into_owned();

    // Build Request record
    let request_val = make_http_request_value(method, &path, &query, headers, body);
    *slot = Some(req);
    Some(request_val)
}

/// The task of one request: read it, call the handler with it, send
/// what the handler returns. The reading and the sending block, so
/// they run on the I/O pool and the task waits for them like for any
/// I/O; the handler itself may wait as long as it likes (a long poll)
/// without holding a thread.
#[cfg(feature = "http")]
struct Serve {
    request: Pending,
    handler: Value,
    /// The task's own handle: where its failure is read when the
    /// handler fails.
    handle: Arc<TaskHandle>,
    state: ServeState,
    _inflight: Decrement,
}

#[cfg(feature = "http")]
enum ServeState {
    Start,
    Reading(crate::vm::IoOp),
    Calling,
    Responding,
}

/// What a step of a request's task completes with when the I/O pool
/// cannot run it.
#[cfg(feature = "http")]
fn not_served(_why: &str) -> Value {
    Value::Unit
}

#[cfg(feature = "http")]
impl crate::vm::Native for Serve {
    fn name(&self) -> &str {
        "http.serve"
    }

    fn resume(&mut self, vm: &mut Vm, input: Value) -> Result<Step, VmError> {
        match std::mem::replace(&mut self.state, ServeState::Responding) {
            ServeState::Start => {
                let request = self.request.clone();
                let op =
                    vm.runtime
                        .io_pool
                        .submit(not_served, move || match read_request(&request) {
                            Some(request_val) => Value::variant(bv::SOME, vec![request_val]),
                            None => Value::variant(bv::NONE, vec![]),
                        });
                let wait = Wait::new(vec![Arm::Cell(op.cell.clone())]);
                self.state = ServeState::Reading(op);
                Ok(Step::Park(wait))
            }
            ServeState::Reading(op) => {
                vm.woken()?;
                match op.cell.get() {
                    Some(Value::Variant(tag, fields)) if tag.is(bv::SOME) && fields.len() == 1 => {
                        self.state = ServeState::Calling;
                        Ok(vm.call(self.handler.clone(), [fields[0].clone()]))
                    }
                    // Answered already, or nothing can read it.
                    _ => Ok(Step::Done(Value::Unit)),
                }
            }
            ServeState::Calling => {
                let request = self.request.clone();
                let io = vm.runtime.io.clone();
                let op = vm.runtime.io_pool.submit(not_served, move || {
                    if let Some(req) = request.lock().take() {
                        send_http_response(&io, &input, req);
                    }
                    Value::Unit
                });
                Ok(Step::Park(Wait::new(vec![Arm::Cell(op.cell)])))
            }
            ServeState::Responding => {
                vm.woken()?;
                Ok(Step::Done(Value::Unit))
            }
        }
    }

    fn abandon(&mut self, vm: &mut Vm) {
        // The handler failed, or the program ended, before the request
        // was answered.
        let Some(req) = self.request.lock().take() else {
            return;
        };
        if let Some(Err(e)) = self.handle.try_get()
            && !self.handle.is_cancelled()
        {
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
        }
        let resp = tiny_http::Response::from_string("Internal Server Error")
            .with_status_code(tiny_http::StatusCode(500));
        let _ = req.respond(resp);
    }
}

/// Shared implementation of `http.serve` and `http.serve_all`.
///
/// `bind_host` is the interface portion of the bind address ("127.0.0.1"
/// for `http.serve`, "0.0.0.0" for `http.serve_all`). `name_for_err` is
/// the user-visible builtin name used in error messages.
#[cfg(feature = "http")]
fn do_http_serve_inner(
    vm: &mut Vm,
    bind_host: &str,
    name_for_err: &'static str,
    args: &[Value],
) -> Result<Step, VmError> {
    if args.len() != 2 {
        return Err(VmError::new(format!(
            "{name_for_err} takes 2 arguments (port, handler)"
        )));
    }
    let Value::Int(port) = &args[0] else {
        return Err(VmError::new(format!(
            "{name_for_err} requires Int, got {}",
            value_kind(&args[0])
        )));
    };
    let handler = args[1].clone();

    let addr = format!("{bind_host}:{port}");
    let server = Arc::new(
        tiny_http::Server::http(&addr)
            .map_err(|e| VmError::new(format!("{name_for_err}: failed to bind: {e}")))?,
    );

    // The VM that the VM of each request's task is made from.
    let mut template_vm = vm.spawn_child();
    let task_id = vm.next_task_id();
    let handle = Arc::new(TaskHandle::new(task_id));
    let serve_handle = handle.clone();

    // Counter of requests in flight. Caps them at
    // HTTP_SERVE_MAX_CONCURRENT_HANDLERS so bursts / slowloris cannot
    // force unbounded tasks (HIGH-2).
    let inflight = Arc::new(AtomicUsize::new(0));

    // Spawn the accept loop on a dedicated OS thread so it doesn't
    // block a scheduler worker or the main thread. While it serves, a
    // task that waits for the server is not deadlocked.
    let serving = vm.scheduler().external();
    let scheduler = vm.scheduler().clone();
    std::thread::spawn(move || {
        let _serving = serving;
        loop {
            // Use recv_timeout so the accept loop periodically
            // unblocks and can notice a shutdown. Note: this
            // does NOT per-connection bound the time tiny_http
            // spends reading headers from a slow client — tiny_http
            // does that inside its internal task pool and doesn't
            // expose the TcpStream to let us call
            // set_read_timeout. The concurrent-handler cap below
            // bounds the blast radius. (HIGH-2)
            let req = match server.recv_timeout(HTTP_SERVE_RECV_TIMEOUT) {
                Ok(Some(req)) => req,
                Ok(None) => continue, // timeout, re-loop
                Err(_) => break,      // server shut down
            };

            // Enforce concurrency cap. If we're at the cap, fast-reject
            // with 503 instead of starting another task.
            if inflight.load(Ordering::Acquire) >= HTTP_SERVE_MAX_CONCURRENT_HANDLERS {
                let resp = tiny_http::Response::from_string("Service Unavailable")
                    .with_status_code(tiny_http::StatusCode(503));
                let _ = req.respond(resp);
                continue;
            }

            // Each accepted request is handled by a task of its own.
            inflight.fetch_add(1, Ordering::AcqRel);
            let id = template_vm.next_task_id();
            let task_handle = Arc::new(TaskHandle::with_owner(
                id,
                crate::scheduler::current_task_owner(),
            ));
            let request = Arc::new(Mutex::new(Some(req)));
            let mut request_vm = template_vm.spawn_child();
            request_vm.spawned = true;
            request_vm.push_native_frame(Box::new(Serve {
                request: request.clone(),
                handler: handler.clone(),
                handle: task_handle.clone(),
                state: ServeState::Start,
                _inflight: Decrement(inflight.clone()),
            }));
            let submitted = scheduler.submit(crate::scheduler::Task {
                id,
                vm: request_vm,
                handle: task_handle,
            });
            // The program is ending: the request is turned away.
            if submitted.is_err()
                && let Some(req) = request.lock().take()
            {
                let resp = tiny_http::Response::from_string("Service Unavailable")
                    .with_status_code(tiny_http::StatusCode(503));
                let _ = req.respond(resp);
            }
        }
        // Accept loop ended (server shut down) — complete the handle.
        serve_handle.complete(Ok(Value::Unit), scheduler.wake());
    });

    // The caller waits until the server shuts down.
    let wait = Wait::new(vec![Arm::Cell(handle.done())]);
    Ok(
        vm.park(name_for_err, wait, move |_, _| match handle.try_get() {
            Some(Err(mut inner)) => {
                inner.message = format!("{name_for_err} failed: {}", inner.message);
                Err(inner)
            }
            Some(Ok(value)) => Ok(Step::Done(value)),
            None => Err(VmError::new(format!(
                "internal VM error: {name_for_err} ended before its server"
            ))),
        }),
    )
}

/// Dispatch `http.<name>(args)`.
#[cfg_attr(not(feature = "http"), allow(unused_variables))]
pub(crate) fn call_http(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Step, VmError> {
    match name {
        "get" => {
            #[cfg(feature = "http")]
            {
                if args.len() != 1 {
                    return Err(VmError::new("http.get takes 1 argument (url)".into()));
                }
                let Value::String(url) = &args[0] else {
                    return Err(VmError::new(format!(
                        "http.get requires String, got {}",
                        value_kind(&args[0])
                    )));
                };

                let url = url.clone();
                vm.io("http", http_timeout_err, move || do_http_get(&url))
            }
            #[cfg(not(feature = "http"))]
            {
                let _ = args;
                Err(VmError::new("http.get requires the 'http' feature".into()))
            }
        }

        "request" => {
            #[cfg(feature = "http")]
            {
                if args.len() != 4 {
                    return Err(VmError::new(
                        "http.request takes 4 arguments (method, url, body, headers)".into(),
                    ));
                }
                let Value::Variant(method_tag, method_args) = &args[0] else {
                    return Err(VmError::new(format!(
                        "http.request requires Method, got {}",
                        value_kind(&args[0])
                    )));
                };
                if !method_args.is_empty() || !method_tag.of(ty::METHOD) {
                    return Err(VmError::new("http.request: invalid Method variant".into()));
                }
                let Value::String(url) = &args[1] else {
                    return Err(VmError::new(format!(
                        "http.request requires String, got {}",
                        value_kind(&args[1])
                    )));
                };
                let Value::String(body) = &args[2] else {
                    return Err(VmError::new(format!(
                        "http.request requires String, got {}",
                        value_kind(&args[2])
                    )));
                };
                let Value::Map(header_map) = &args[3] else {
                    return Err(VmError::new(format!(
                        "http.request requires Map, got {}",
                        value_kind(&args[3])
                    )));
                };

                let method_tag = method_tag.name().to_string();
                let url = url.clone();
                let body = body.clone();
                let headers: Vec<(String, String)> = header_map
                    .iter()
                    .filter_map(|(k, v)| {
                        if let (Value::String(key), Value::String(val)) = (k, v) {
                            Some((key.clone(), val.clone()))
                        } else {
                            None
                        }
                    })
                    .collect();
                vm.io("http", http_timeout_err, move || {
                    do_http_request(&method_tag, &url, &body, &headers)
                })
            }
            #[cfg(not(feature = "http"))]
            {
                let _ = args;
                Err(VmError::new(
                    "http.request requires the 'http' feature".into(),
                ))
            }
        }

        "serve" => {
            #[cfg(feature = "http")]
            {
                // Security: default to loopback only (HIGH-5). Developers who
                // want to expose the server on all interfaces must opt in via
                // `http.serve_all`.
                do_http_serve_inner(vm, "127.0.0.1", "http.serve", args)
            }
            #[cfg(not(feature = "http"))]
            {
                let _ = args;
                Err(VmError::new(
                    "http.serve requires the 'http' feature".into(),
                ))
            }
        }

        "serve_all" => {
            #[cfg(feature = "http")]
            {
                // Explicit opt-in to binding 0.0.0.0 (all interfaces). (HIGH-5)
                do_http_serve_inner(vm, "0.0.0.0", "http.serve_all", args)
            }
            #[cfg(not(feature = "http"))]
            {
                let _ = args;
                Err(VmError::new(
                    "http.serve_all requires the 'http' feature".into(),
                ))
            }
        }

        _ => http_plain(name, args).map(Step::Done),
    }
}

/// The `http` functions that do not wait.
fn http_plain(name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "segments" => {
            if args.len() != 1 {
                return Err(VmError::new("http.segments takes 1 argument (path)".into()));
            }
            let Value::String(path) = &args[0] else {
                return Err(VmError::new(format!(
                    "http.segments requires String, got {}",
                    value_kind(&args[0])
                )));
            };
            let segments: Vec<Value> = path
                .split('/')
                .filter(|s| !s.is_empty())
                .map(|s| Value::String(s.to_string()))
                .collect();
            Ok(Value::List(Arc::new(segments)))
        }

        "parse_query" => {
            if args.len() != 1 {
                return Err(VmError::new(
                    "http.parse_query takes 1 argument (query)".into(),
                ));
            }
            let Value::String(raw) = &args[0] else {
                return Err(VmError::new(format!(
                    "http.parse_query requires String, got {}",
                    value_kind(&args[0])
                )));
            };
            // Accept a leading `?` for convenience — e.g. directly
            // passing a URL fragment like `?a=1&b=2` shouldn't require
            // the caller to strip it first.
            let body = raw.strip_prefix('?').unwrap_or(raw);
            // Preserve insertion order of first appearance for each
            // key. BTreeMap gives us stable ordering by key, which is
            // fine for a value-semantic Map — repeated keys always
            // append to the same List in encounter order.
            let mut out: BTreeMap<Value, Value> = BTreeMap::new();
            if body.is_empty() {
                return Ok(Value::Map(Arc::new(out)));
            }
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
                let (raw_key, raw_val) = match segment.find('=') {
                    Some(pos) => (&segment[..pos], &segment[pos + 1..]),
                    None => (segment, ""),
                };
                let key =
                    crate::builtins::encoding::form_decode_component(raw_key).map_err(|msg| {
                        VmError::new(format!("http.parse_query: pair {i} key: {msg}"))
                    })?;
                let val =
                    crate::builtins::encoding::form_decode_component(raw_val).map_err(|msg| {
                        VmError::new(format!("http.parse_query: pair {i} value: {msg}"))
                    })?;
                let entry = out
                    .entry(Value::String(key))
                    .or_insert_with(|| Value::List(Arc::new(Vec::new())));
                if let Value::List(list) = entry {
                    // `Arc::make_mut` clones the Vec only on the
                    // second and later pushes for the same key; the
                    // first push sees refcount 1 and mutates in place.
                    Arc::make_mut(list).push(Value::String(val));
                }
            }
            Ok(Value::Map(Arc::new(out)))
        }

        _ => Err(VmError::new(format!("unknown http function: {name}"))),
    }
}

#[cfg(all(test, feature = "http"))]
mod http_response_tests {
    use super::*;

    fn make_response(status: i64) -> Value {
        let mut fields: BTreeMap<String, Value> = BTreeMap::new();
        fields.insert("status".to_string(), Value::Int(status));
        fields.insert("body".to_string(), Value::String(String::new()));
        fields.insert("headers".to_string(), Value::Map(Arc::new(BTreeMap::new())));
        Value::builtin_record(ty::RESPONSE, fields)
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
    fn test_response_status_at_u16_max_ok() {
        let val = make_response(65535);
        let result = extract_http_response(&val);
        assert!(result.is_ok(), "status 65535 should be accepted");
        assert_eq!(result.unwrap().0, 65535);
    }

    #[test]
    fn test_response_status_zero_ok() {
        let val = make_response(0);
        let result = extract_http_response(&val);
        assert!(result.is_ok(), "status 0 should be accepted");
        assert_eq!(result.unwrap().0, 0);
    }
}
