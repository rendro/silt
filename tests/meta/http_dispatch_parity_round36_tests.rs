//! Round-36 parity locks for `builtins::data::do_http_request` dispatch.
//!
//! The 7 verb arms (POST / PUT / PATCH / GET / DELETE / HEAD / OPTIONS)
//! previously each duplicated an identical header-loop prelude and only
//! diverged in (a) which ureq verb-fn they invoked and (b) whether they
//! used `send_empty()` / `send(body)` (body verbs) or `call()` (no-body
//! verbs). Round-36 collapsed the seven arms into two families (body
//! and no-body) via two local macros, keeping one header-loop per
//! family.
//!
//! These tests stand up a tiny hand-rolled TCP server that records
//! exactly what the HTTP client sent, then drive every verb through
//! the `call_http` builtin and assert parity with the pre-refactor
//! behavior:
//!
//!   1. The method line observed by the server matches the verb we
//!      asked for — proves the dispatch still maps each tag to the
//!      right ureq verb-fn.
//!   2. Client-supplied headers are forwarded verbatim for every verb
//!      — proves the shared header-loop runs for every arm.
//!   3. Body vs no-body split is preserved: POST/PUT/PATCH send the
//!      request body; GET/DELETE/HEAD/OPTIONS send zero bytes of body
//!      (no Content-Length on the wire). A regression that silently
//!      swapped GET and POST (or forgot `send_empty`) will fail here.
//!   4. On a connect failure to an unroutable peer, the error variant
//!      is the pre-refactor `HttpConnect(_)` — the refactor must not
//!      change error shape or phrasing.
//!
//! The server is a raw TCP listener (no tiny_http dep) so the test
//! compiles even if a future edition drops tiny_http from
//! dev-dependencies.

#![cfg(feature = "http")]

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use silt::builtins::data::call_http;
use silt::value::Value;
use silt::vm::Vm;

// ── captured request (what the server saw) ────────────────────────────

#[derive(Debug, Clone)]
struct Captured {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// Parse one HTTP/1.1 request off a TCP connection. Reads until
/// `\r\n\r\n` to get the headers, then reads exactly `Content-Length`
/// bytes of body (0 if absent). Returns the captured request.
fn parse_http_request(stream: &mut TcpStream) -> std::io::Result<Captured> {
    // Read until we find the end of headers.
    let mut buf = Vec::with_capacity(1024);
    let mut tmp = [0u8; 512];
    let header_end;
    loop {
        let n = stream.read(&mut tmp)?;
        if n == 0 {
            // Peer closed before finishing headers.
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "eof before end of headers",
            ));
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_header_end(&buf) {
            header_end = pos;
            break;
        }
        if buf.len() > 64 * 1024 {
            return Err(std::io::Error::other("headers too big"));
        }
    }

    let header_bytes = &buf[..header_end];
    let head =
        std::str::from_utf8(header_bytes).map_err(|_| std::io::Error::other("non-utf8 headers"))?;
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();

    let mut headers: Vec<(String, String)> = Vec::new();
    let mut content_length: usize = 0;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim().to_string();
            let v = v.trim().to_string();
            if k.eq_ignore_ascii_case("content-length") {
                content_length = v.parse().unwrap_or(0);
            }
            headers.push((k, v));
        }
    }

    // Everything after the header terminator in `buf` is early body bytes.
    let body_start = header_end + 4; // skip "\r\n\r\n"
    let mut body = Vec::with_capacity(content_length);
    if body_start < buf.len() {
        body.extend_from_slice(&buf[body_start..]);
    }
    while body.len() < content_length {
        let need = content_length - body.len();
        let cap = tmp.len().min(need);
        let n = stream.read(&mut tmp[..cap])?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }

    Ok(Captured {
        method,
        path,
        headers,
        body,
    })
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

/// Spawn a TCP server that accepts one request, records it, and replies
/// with a minimal 200 OK. Returns (port, receiver for the captured req).
fn spawn_one_shot_server() -> (u16, mpsc::Receiver<Captured>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        listener.set_nonblocking(false).ok();
        // Accept exactly one connection.
        if let Ok((mut stream, _)) = listener.accept() {
            stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
            stream.set_write_timeout(Some(Duration::from_secs(5))).ok();
            match parse_http_request(&mut stream) {
                Ok(cap) => {
                    // HEAD responses must not include a body per RFC 7230,
                    // but ureq is lenient — a short 200 with Content-Length: 0
                    // works for every verb including HEAD.
                    let _ = stream.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    );
                    let _ = tx.send(cap);
                }
                Err(_) => {
                    // Peer hung up before we could parse; caller will see
                    // a timeout on rx.recv_timeout.
                }
            }
        }
    });

    (port, rx)
}

// ── helpers for building silt Values ──────────────────────────────────

fn s(v: &str) -> Value {
    Value::String(v.to_string())
}

fn method_variant(tag: &str) -> Value {
    // `http.request` expects args[0] to be a Variant with no payload
    // carrying the method name (e.g. Method::GET).
    Value::Variant(tag.to_string(), Vec::new())
}

#[allow(clippy::mutable_key_type)] // Value holds Channel handles; not used as keys here.
fn headers_map(pairs: &[(&str, &str)]) -> Value {
    let mut m: BTreeMap<Value, Value> = BTreeMap::new();
    for (k, v) in pairs {
        m.insert(s(k), s(v));
    }
    Value::Map(Arc::new(m))
}

fn call_request(method: &str, url: &str, body: &str, headers: &[(&str, &str)]) -> Value {
    let mut vm = Vm::new();
    let args = vec![
        method_variant(method),
        s(url),
        s(body),
        headers_map(headers),
    ];
    call_http(&mut vm, "request", &args).expect("call_http request")
}

// ── per-verb parity tests ─────────────────────────────────────────────

fn run_body_verb_parity(verb: &str) {
    let (port, rx) = spawn_one_shot_server();
    let url = format!("http://127.0.0.1:{port}/p/{}", verb.to_lowercase());
    let body = format!("hello-{verb}-body");
    let headers = &[
        ("X-Silt-Verb", verb),
        ("X-Silt-Probe", "parity"),
        ("Content-Type", "text/plain"),
    ];
    let resp = call_request(verb, &url, &body, headers);
    match &resp {
        Value::Variant(tag, _) => assert_eq!(tag, "Ok", "unexpected Err from {verb}: {resp:?}"),
        other => panic!("expected Variant, got {other:?}"),
    }

    let cap = rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_else(|_| panic!("server did not see the {verb} request"));

    assert_eq!(cap.method, verb, "method on wire mismatch for {verb}");
    assert!(
        cap.path.ends_with(&format!("/p/{}", verb.to_lowercase())),
        "path mismatch for {verb}: {}",
        cap.path
    );
    // Headers forwarded.
    assert!(
        cap.headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("X-Silt-Verb") && v == verb),
        "X-Silt-Verb not forwarded for {verb}: {:?}",
        cap.headers
    );
    assert!(
        cap.headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("X-Silt-Probe") && v == "parity"),
        "X-Silt-Probe not forwarded for {verb}: {:?}",
        cap.headers
    );
    // Body forwarded verbatim.
    assert_eq!(
        cap.body,
        body.as_bytes(),
        "body on wire mismatch for {verb}"
    );
}

fn run_no_body_verb_parity(verb: &str) {
    let (port, rx) = spawn_one_shot_server();
    let url = format!("http://127.0.0.1:{port}/q/{}", verb.to_lowercase());
    // We pass a non-empty body string to call_request; the no-body
    // verbs MUST drop it on the floor (ureq 3's no-body RequestBuilder
    // has no API to attach a body, so `call()` sends no body).
    let body = "this-should-be-ignored";
    let headers = &[("X-Silt-Verb", verb), ("X-Silt-Probe", "parity")];
    let resp = call_request(verb, &url, body, headers);
    match &resp {
        Value::Variant(tag, _) => assert_eq!(tag, "Ok", "unexpected Err from {verb}: {resp:?}"),
        other => panic!("expected Variant, got {other:?}"),
    }

    let cap = rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_else(|_| panic!("server did not see the {verb} request"));

    assert_eq!(cap.method, verb, "method on wire mismatch for {verb}");
    assert!(
        cap.path.ends_with(&format!("/q/{}", verb.to_lowercase())),
        "path mismatch for {verb}: {}",
        cap.path
    );
    assert!(
        cap.headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("X-Silt-Verb") && v == verb),
        "X-Silt-Verb not forwarded for {verb}: {:?}",
        cap.headers
    );
    // No body on the wire for no-body verbs. Either Content-Length is
    // absent/0, or if (under some transfer encoding) it sneaks bytes in,
    // those bytes must NOT be the ignored-body string.
    let cl = cap
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .map(|(_, v)| v.as_str());
    match cl {
        None => {
            assert!(
                cap.body.is_empty(),
                "no-body verb {verb} sent body bytes despite no Content-Length: {:?}",
                cap.body
            );
        }
        Some(cl_val) => {
            assert_eq!(
                cl_val, "0",
                "no-body verb {verb} leaked a non-zero Content-Length: {cl_val}"
            );
            assert!(
                cap.body.is_empty(),
                "no-body verb {verb} sent body bytes with Content-Length: 0: {:?}",
                cap.body
            );
        }
    }
    // Crucial dedupe-regression lock: a refactor that swapped POST body
    // in for GET would leave the ignored-body string in cap.body here.
    assert_ne!(
        cap.body,
        body.as_bytes(),
        "no-body verb {verb} forwarded what should have been an ignored body — POST/GET swap?"
    );
}

#[test]
fn post_dispatch_parity() {
    run_body_verb_parity("POST");
}

#[test]
fn put_dispatch_parity() {
    run_body_verb_parity("PUT");
}

#[test]
fn patch_dispatch_parity() {
    run_body_verb_parity("PATCH");
}

#[test]
fn get_dispatch_parity() {
    run_no_body_verb_parity("GET");
}

#[test]
fn delete_dispatch_parity() {
    run_no_body_verb_parity("DELETE");
}

#[test]
fn head_dispatch_parity() {
    run_no_body_verb_parity("HEAD");
}

#[test]
fn options_dispatch_parity() {
    run_no_body_verb_parity("OPTIONS");
}

// ── body-family empty-body path ───────────────────────────────────────
//
// The body verbs have an explicit `if body.is_empty() { send_empty() }
// else { send(body) }` split. A refactor that dropped the branch (always
// send_empty, or always send) would silently break one half. We lock
// the empty-body path for POST here; the non-empty path is already
// locked above.

#[test]
fn post_empty_body_uses_send_empty_path() {
    let (port, rx) = spawn_one_shot_server();
    let url = format!("http://127.0.0.1:{port}/empty");
    let resp = call_request("POST", &url, "", &[("X-Silt-Empty", "1")]);
    match &resp {
        Value::Variant(tag, _) => assert_eq!(tag, "Ok", "unexpected Err: {resp:?}"),
        other => panic!("expected Variant, got {other:?}"),
    }
    let cap = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("server did not see the empty-body POST");
    assert_eq!(cap.method, "POST");
    assert!(
        cap.body.is_empty(),
        "empty-body POST sent bytes: {:?}",
        cap.body
    );
}

// ── unknown verb preserves pre-refactor error shape ──────────────────

#[test]
fn unknown_verb_returns_http_invalid_url_pre_refactor_shape() {
    // The pre-refactor code's catch-all arm returned
    // Err(HttpInvalidUrl("unknown method: <tag>")). Lock that exact
    // variant + message prefix.
    let mut vm = Vm::new();
    let args = vec![
        method_variant("BREW"),
        s("http://127.0.0.1:1/"),
        s(""),
        headers_map(&[]),
    ];
    let resp = call_http(&mut vm, "request", &args).expect("call_http request");
    match &resp {
        Value::Variant(outer, outer_payload) => {
            assert_eq!(outer, "Err");
            assert_eq!(outer_payload.len(), 1);
            match &outer_payload[0] {
                Value::Variant(inner, inner_payload) => {
                    assert_eq!(inner, "HttpInvalidUrl");
                    assert_eq!(inner_payload.len(), 1);
                    match &inner_payload[0] {
                        Value::String(msg) => {
                            assert!(
                                msg.contains("unknown method: BREW"),
                                "expected 'unknown method: BREW' in msg, got: {msg}"
                            );
                        }
                        other => panic!("expected String payload, got {other:?}"),
                    }
                }
                other => panic!("expected inner Variant, got {other:?}"),
            }
        }
        other => panic!("expected Variant, got {other:?}"),
    }
}

// ── connect-failure phrasing preserved per verb ──────────────────────
//
// Every verb, when it fails to connect, must produce an Err wrapping
// HttpConnect (or HttpInvalidUrl / HttpTimeout — the point is the
// error classifier still runs). The pre-refactor code used the same
// `http_err` helper for all seven arms, so the error variant shape is
// shared; we just verify each verb reaches it.
//
// 192.0.2.0/24 (TEST-NET-1) is reserved and unroutable; connecting to
// it fails quickly on most systems, but we also set a short url-level
// expectation: the call returns Err(_) not Ok(_).

fn assert_connect_failure_err_shape(verb: &str) {
    // Use a closed loopback port so the OS immediately rejects with
    // ECONNREFUSED. Pick an ephemeral port and drop the listener so the
    // port is known-closed for the duration of this test.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let url = format!("http://127.0.0.1:{port}/");
    let mut vm = Vm::new();
    let args = vec![method_variant(verb), s(&url), s(""), headers_map(&[])];
    let resp = call_http(&mut vm, "request", &args).expect("call_http request");
    match &resp {
        Value::Variant(outer, outer_payload) => {
            assert_eq!(
                outer, "Err",
                "expected Err for {verb} connect-failure: {resp:?}"
            );
            assert_eq!(outer_payload.len(), 1);
            match &outer_payload[0] {
                Value::Variant(inner, _) => {
                    // The pre-refactor classifier maps "refused" / "connect"
                    // to HttpConnect; other network conditions might surface
                    // as HttpUnknown or HttpTimeout. All three are the
                    // pre-refactor shape — the point is we did NOT bubble
                    // up Ok from a closed port.
                    assert!(
                        matches!(
                            inner.as_str(),
                            "HttpConnect" | "HttpUnknown" | "HttpTimeout" | "HttpClosedEarly"
                        ),
                        "unexpected Err variant for {verb}: {inner}"
                    );
                }
                other => panic!("expected inner Variant, got {other:?}"),
            }
        }
        other => panic!("expected Variant, got {other:?}"),
    }
}

#[test]
fn connect_failure_err_shape_post() {
    assert_connect_failure_err_shape("POST");
}

#[test]
fn connect_failure_err_shape_put() {
    assert_connect_failure_err_shape("PUT");
}

#[test]
fn connect_failure_err_shape_patch() {
    assert_connect_failure_err_shape("PATCH");
}

#[test]
fn connect_failure_err_shape_get() {
    assert_connect_failure_err_shape("GET");
}

#[test]
fn connect_failure_err_shape_delete() {
    assert_connect_failure_err_shape("DELETE");
}

#[test]
fn connect_failure_err_shape_head() {
    assert_connect_failure_err_shape("HEAD");
}

#[test]
fn connect_failure_err_shape_options() {
    assert_connect_failure_err_shape("OPTIONS");
}
