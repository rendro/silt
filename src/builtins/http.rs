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
use super::typed::{Arg, Bytes, Map, TcpListener, unsound};
#[cfg(feature = "http")]
use crate::runtime::handle::{Taken, TaskHandle, TcpListenerHandle, TcpStreamHandle};
#[cfg(feature = "http")]
use crate::runtime::sync::{Arm, Cell, Fired, Wait, Wake};
use crate::typeinfo::bv;
#[cfg(feature = "http")]
use crate::typeinfo::{BuiltinVariant, ty};
use crate::value::Value;
#[cfg(feature = "http")]
use crate::vm::{Leave, Limit, Step, Vm, VmError};
#[cfg(feature = "http")]
use parking_lot::Mutex;

/// What `HttpError`'s `message` says of the variant `tag` with `fields`:
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
    path: &str,
    query: &str,
    headers: crate::value::Map,
    body: Vec<u8>,
    peer: &str,
) -> Value {
    Value::builtin_record(
        ty::REQUEST,
        [
            ("method", Value::variant(method, [])),
            ("path", Value::string(path)),
            ("query", Value::string(query)),
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
fn ureq_response_to_value(
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
/// request to `url`.
///
/// The library says what kind of failure it is (the variants of
/// `ureq::Error`, and for one of the socket the kind of the
/// `io::Error`), and that alone decides the variant: no text of a
/// message is looked at. The message is what the variant carries.
///
/// - A URL that cannot be read as one, or whose scheme is not `http`
///   or `https`, or that has no host: `InvalidUrl(url)`, with the URL
///   as it was written (less a password in it).
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
/// - Anything else is `Other(why)`: a header name or value of the
///   program's that is none, too many redirects, any failure of the
///   socket not named above.
#[cfg(feature = "http")]
fn http_error(error: &ureq::Error, url: &str) -> Value {
    use std::io::ErrorKind;
    use ureq::Error;

    let why = || vec![Value::string(redact_http_url_userinfo(&error.to_string()))];
    let invalid_url = || {
        let url = redact_http_url_userinfo(url);
        Value::variant(bv::HTTP_INVALID_URL, [Value::string(url)])
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
            ErrorKind::InvalidData if https => Value::variant(bv::HTTP_TLS, why()),
            _ => Value::variant(bv::HTTP_OTHER, why()),
        },
        Error::HostNotFound | Error::ConnectionFailed | Error::ConnectProxyFailed(_) => {
            Value::variant(bv::HTTP_CONNECT, why())
        }
        Error::BadUri(_) => invalid_url(),
        Error::Http(http) if http.is::<ureq::http::uri::InvalidUri>() => invalid_url(),
        Error::Http(http) if http.is::<ureq::http::uri::InvalidUriParts>() => invalid_url(),
        Error::Protocol(_) | Error::LargeResponseHeader(..) => {
            Value::variant(bv::HTTP_INVALID_RESPONSE, why())
        }
        Error::Tls(_) | Error::Rustls(_) | Error::Pem(_) | Error::TlsRequired => {
            Value::variant(bv::HTTP_TLS, why())
        }
        // (A status is no failure: silt asks the library for it as a
        // response, and the library's error for one is never made.)
        _ => Value::variant(bv::HTTP_OTHER, why()),
    }
}

/// `Err(InvalidUrl(url))` for a URL whose port is no port: not a
/// number, empty, or not in 1..=65535.
///
/// The transport library does not refuse such a URL. Its URL type
/// keeps the port as text and gives no port when the text is not one,
/// and the request then goes to the default port of the scheme:
/// `http://host:99999/` was asked of port 80. So it is refused here,
/// before any connection is made. (A URL that cannot be read at all
/// is the library's to refuse: [`http_error`].)
#[cfg(feature = "http")]
fn refuse_port(url: &str) -> Option<Value> {
    let uri = url.parse::<ureq::http::Uri>().ok()?;
    let authority = uri.authority()?.as_str();
    // Behind the user's name and password, and behind the brackets of
    // an IPv6 address, whose colons are no port's.
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let port = match host.rfind(']') {
        Some(bracket) => host[bracket + 1..].strip_prefix(':'),
        None => host.rsplit_once(':').map(|(_, port)| port),
    }?;
    let digits = !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit());
    if digits && matches!(port.parse::<u16>(), Ok(1..)) {
        return None;
    }
    let url = redact_http_url_userinfo(url);
    let invalid = Value::variant(bv::HTTP_INVALID_URL, [Value::string(url)]);
    Some(Value::variant(bv::ERR, [invalid]))
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

/// The client of `http.get` and `http.request`. Security: conservative
/// default timeouts so a slow/unreachable peer cannot hang the
/// underlying OS socket indefinitely (HIGH-3). These apply even if the
/// silt task's SILT_IO_TIMEOUT unblocks the VM task, so we don't leak
/// real file descriptors.
#[cfg(feature = "http")]
fn client() -> ureq::Agent {
    client_with_times(Duration::from_secs(10), Duration::from_secs(60))
}

/// A client whose times to connect and in all are the given ones. Every
/// request says `user-agent: silt/<version>` and `accept-encoding:
/// gzip`, unless the program sets that header itself: ureq sends the
/// request's header then.
#[cfg(feature = "http")]
fn client_with_times(connect: Duration, global: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(connect))
        .timeout_global(Some(global))
        .user_agent(format!("silt/{}", crate::VERSION))
        .accept_encoding("gzip")
        .build()
        .into()
}

/// Test-only: an HTTP GET as `http.get` makes it, with a client of its
/// own.
#[cfg(all(test, feature = "http"))]
fn do_http_get(url: &str) -> Value {
    do_http_request("GET", url, &[], &[])
}

/// How many redirects `http.get` and `http.request` follow, as many as
/// ureq would.
#[cfg(feature = "http")]
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
    do_http_request_on(&client(), method_tag, url, body, headers)
}

/// Perform a synchronous HTTP request with the client `agent` and
/// return a `Value`.
///
/// The request follows redirects itself, because ureq does not send a
/// body again: a 303 goes on as a GET without a body (a HEAD stays a
/// HEAD) and without the program's `content-type` header, and so does
/// a POST that gets a 301 or 302; any other request goes on with the
/// same method and body. As ureq does, a
/// redirect drops the `authorization` and `cookie` headers. A `content-length` header of the program's is not sent:
/// the body's own length is (an empty body of a method that takes
/// none by default has none).
///
/// A shutdown of `agent` (`ureq::Agent::shutdown`, from another thread)
/// ends the request: a read or a write it waits in, a TLS handshake,
/// the next redirect. A connect under way runs on to its end (at most
/// the connect time of [`client`]), and the request ends after it.
#[cfg(feature = "http")]
fn do_http_request_on(
    agent: &ureq::Agent,
    method_tag: &str,
    url: &str,
    body: &[u8],
    headers: &[(String, String)],
) -> Value {
    if !matches!(
        method_tag,
        "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS"
    ) {
        return Value::variant(
            bv::ERR,
            [Value::variant(
                bv::HTTP_INVALID_URL,
                [Value::string(format!("unknown method: {method_tag}"))],
            )],
        );
    }
    let mut method = method_tag;
    let mut url = url.to_string();
    let mut body = body;
    let mut headers: Vec<(&str, &str)> = headers
        .iter()
        .filter(|(key, _)| !key.eq_ignore_ascii_case("content-length"))
        .map(|(key, val)| (key.as_str(), val.as_str()))
        .collect();
    for _ in 0..=REDIRECTS_MAX {
        if let Some(refused) = refuse_port(&url) {
            return refused;
        }
        let result = send_request(agent, method, &url, body, &headers);
        let Some((status, next)) = result.as_ref().ok().and_then(|r| redirect(r, &url)) else {
            return finish_http_response(result, &url, method == "HEAD");
        };
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
        headers.retain(|(key, _)| {
            !key.eq_ignore_ascii_case("authorization") && !key.eq_ignore_ascii_case("cookie")
        });
        url = next;
    }
    finish_http_response(Err(ureq::Error::TooManyRedirects), &url, false)
}

/// Send one request of `http.request`, following no redirect. Every
/// method sends the body, with its length; an empty body sends
/// nothing, not even a length for the methods that take none by
/// default (GET, DELETE, HEAD, OPTIONS: ureq makes them take one with
/// `force_send_body`).
#[cfg(feature = "http")]
fn send_request(
    agent: &ureq::Agent,
    method: &str,
    url: &str,
    body: &[u8],
    headers: &[(&str, &str)],
) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    fn headed<B>(
        mut req: ureq::RequestBuilder<B>,
        headers: &[(&str, &str)],
    ) -> ureq::RequestBuilder<B> {
        for (key, val) in headers {
            req = req.header(*key, *val);
        }
        req.config().max_redirects(0).build()
    }
    let with_body = |req: ureq::RequestBuilder<ureq::typestate::WithBody>| {
        let req = headed(req, headers);
        if body.is_empty() {
            req.send_empty()
        } else {
            req.send(body)
        }
    };
    let without_body = |req: ureq::RequestBuilder<ureq::typestate::WithoutBody>| {
        let req = headed(req, headers);
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
/// redirect without a location that is text.
#[cfg(feature = "http")]
fn redirect(response: &ureq::http::Response<ureq::Body>, url: &str) -> Option<(u16, String)> {
    let status = response.status().as_u16();
    if !matches!(status, 301 | 302 | 303 | 307 | 308) {
        return None;
    }
    let location = response.headers().get("location")?.to_str().ok()?;
    Some((status, resolve_location(url, location)))
}

/// `location` resolved against `base` (RFC 3986): a URL with a scheme
/// as it is, one without (`//host/x`) on the base's scheme, a path from
/// the root on the base's scheme and host, a query alone (`?y=2`) on
/// the base's path, any other path next to the base's; a path's `.` and
/// `..` segments are taken away, and its query is left as it is. A
/// fragment (`#x`) is not sent to a server, so it is dropped first; a
/// location that is then empty is the base's own path and query.
#[cfg(feature = "http")]
fn resolve_location(base: &str, location: &str) -> String {
    let location = location.split('#').next().unwrap_or_default();
    let has_scheme = location
        .parse::<ureq::http::Uri>()
        .is_ok_and(|uri| uri.scheme().is_some());
    if has_scheme {
        return with_dot_segments_taken_away(location);
    }
    let Ok(uri) = base.parse::<ureq::http::Uri>() else {
        return location.to_string();
    };
    let origin = match (uri.scheme_str(), uri.authority()) {
        (Some(scheme), Some(authority)) => format!("{scheme}://{authority}"),
        _ => String::new(),
    };
    if location.starts_with("//") {
        return match uri.scheme_str() {
            Some(scheme) => with_dot_segments_taken_away(&format!("{scheme}:{location}")),
            None => location.to_string(),
        };
    }
    if location.starts_with('?') {
        return format!("{origin}{}{location}", uri.path());
    }
    if location.is_empty() {
        let own = uri.path_and_query().map_or("/", |own| own.as_str());
        return format!("{origin}{own}");
    }
    let (path, query) = location.split_at(location.find('?').unwrap_or(location.len()));
    let path = match path.starts_with('/') {
        true => path.to_string(),
        // (Next to the base's: after its last `/`.)
        false => {
            let own = uri.path();
            format!("{}{path}", &own[..own.rfind('/').map_or(0, |at| at + 1)])
        }
    };
    format!("{origin}{}{query}", without_dot_segments(&path))
}

/// The URL `url`, which has a scheme and a host, with the `.` and `..`
/// segments of its path taken away; its query as it is.
#[cfg(feature = "http")]
fn with_dot_segments_taken_away(url: &str) -> String {
    let Ok(uri) = url.parse::<ureq::http::Uri>() else {
        return url.to_string();
    };
    let dotted = uri
        .path()
        .split('/')
        .any(|segment| matches!(segment, "." | ".."));
    match (dotted, uri.scheme_str(), uri.authority()) {
        (true, Some(scheme), Some(authority)) => {
            let query = uri
                .query()
                .map_or(String::new(), |query| format!("?{query}"));
            format!(
                "{scheme}://{authority}{}{query}",
                without_dot_segments(uri.path())
            )
        }
        _ => url.to_string(),
    }
}

/// The path `path`, which starts with `/`, with its `.` and `..`
/// segments taken away (RFC 3986 5.2.4): a last one leaves the path
/// ending in `/` (`/a/b/..` is `/a/`).
#[cfg(feature = "http")]
fn without_dot_segments(path: &str) -> String {
    let segments: Vec<&str> = path.split('/').skip(1).collect();
    let mut kept: Vec<&str> = Vec::new();
    for (at, segment) in segments.iter().enumerate() {
        let last = at + 1 == segments.len();
        match *segment {
            "." | ".." => {
                if *segment == ".." {
                    kept.pop();
                }
                if last {
                    kept.push("");
                }
            }
            other => kept.push(other),
        }
    }
    format!("/{}", kept.join("/"))
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
    fn of(next: wire::Next, held: Held, peer: &str) -> Got {
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
        // A header's name is in lower case, however the client wrote
        // it: a name is not case-sensitive, and a lookup has one
        // spelling to ask for.
        let headers =
            (request.headers.into_iter()).map(|(name, value)| (name.to_ascii_lowercase(), value));
        let headers = header_map(headers);
        // The body is the bytes that arrived.
        Got::Request(
            make_http_request_value(method, path, query, headers, request.body, peer),
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

/// What the body of a response of the server's own is: a word.
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
                e.message
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
        let got = |next| Got::of(next, Held(self.server.clone(), 0), &self.peer);
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
                    Go::Step(Step::Park(wait))
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
        let peer = self.peer.clone();
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
                *result.lock() = Some(Got::of(next, held, &peer));
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
        Step::Park(wait)
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
            peer: self.peer.clone(),
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
                        Go::Step(Step::Park(wait))
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
                        Go::Step(Step::Park(wait))
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
                // (A server that drains closes the connection after it,
                // and tells the client so.)
                let reply = match self.server.draining() {
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
                // The failure is handled here: it is logged, and not
                // reported as a task that nobody joined. Not a test's
                // `process.exit`: that fails the test, as the failure of
                // a task that nobody joined does.
                if !e.exit_in_a_test {
                    self.handle.mark_joined();
                    // Security: do NOT include VmError details (call
                    // stack, line numbers, panic payload) in the response
                    // body — that leaks implementation details and
                    // potentially sensitive values across the security
                    // boundary (MED-1). Log to the host's stderr instead.
                    vm.runtime
                        .io
                        .err(&format!("http.serve: handler error: {}\n", e.message));
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
        child.push_native_frame(Box::new(Conn {
            peer,
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
                    return Ok(Step::Park(Vm::no_wait()));
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
                        return Ok(Step::Park(wait));
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
        let wait = op.wait(vm, Limit::None);
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
        // A request that nobody waits for any more (cancelled, ended
        // by its deadline) is ended: its client is shut down.
        let agent = client();
        let stop = agent.clone();
        vm.io("http", http_timeout_err, Leave::interrupts(move |_| stop.shutdown()), move |_| {
            do_http_request_on(&agent, "GET", &url, &[], &[])
        })
    }

    #[cfg(feature = "http")]
    fn request(vm, method: Method, url: &str, body: Bytes, headers: Map) -> Result<Step, VmError> {
        let (method, url, body) = (method.0.to_string(), url.to_string(), body.clone());
        let headers: Vec<(String, String)> = headers
            .iter()
            .filter_map(|(k, v)| Some((<&str>::take(k)?.to_string(), <&str>::take(v)?.to_string())))
            .collect();
        let agent = client();
        let stop = agent.clone();
        vm.io("http", http_timeout_err, Leave::interrupts(move |_| stop.shutdown()), move |_| {
            do_http_request_on(&agent, &method, &url, &body, &headers)
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
            .map(Value::string)
            .collect()
    }

    // The pairs are decoded as `encoding.form_decode` decodes them, and
    // a pair that does not decode is its error: a query string comes
    // from whoever sent the request.
    fn parse_query(query: &str) -> Value {
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
            let key = match form_decode_component(raw_key) {
                Ok(key) => key,
                Err(error) => return error.err(&format!("pair {i}: key: ")),
            };
            let val = match form_decode_component(raw_val) {
                Ok(val) => val,
                Err(error) => return error.err(&format!("pair {i}: value: ")),
            };
            out.entry(Value::string(key)).or_default().push(Value::string(val));
        }
        let pairs = out.into_iter().map(|(key, values)| (key, Value::list(values)));
        Value::variant(bv::OK, [Value::map(pairs)])
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
            "Response {status: 204, body: bytes(00 ff 64, length: 3), headers: #{\"accept\": \"*/*\"}}"
        );
        let (status, body, record) = extract_http_response(&response).expect("a Response");
        assert_eq!((status, body), (204, &[0, 0xff, b'd'][..]));
        assert_eq!(record.fields()[0], Value::Int(204));

        let request = make_http_request_value(
            bv::POST,
            "/jobs",
            "n=1",
            headers(),
            b"go".to_vec(),
            "[::1]:5000",
        );
        assert_eq!(
            request.to_string(),
            "Request {method: POST, path: \"/jobs\", query: \"n=1\", headers: #{\"accept\": \"*/*\"}, body: bytes(67 6f, length: 2), peer: \"[::1]:5000\"}"
        );
        let Value::Record(record) = &request else {
            panic!("a record");
        };
        assert_eq!(record.get("path"), Some(&Value::string("/jobs")));
        assert_eq!(record.fields()[1], Value::string("/jobs"));
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

    #[test]
    fn a_url_that_is_none_is_invalid_url_with_the_url_as_written() {
        for url in [
            "not a url",
            "",
            "example.com/page",
            "http://",
            "http://127.0.0.1/a b",
            "ftp://127.0.0.1/file",
            "mailto:someone@example.com",
        ] {
            let expected = format!("InvalidUrl({url})");
            assert_eq!(failure(&do_http_get(url)), expected);
            assert_eq!(
                failure(&do_http_request("POST", url, b"body", &[])),
                expected
            );
        }
        // A password in an http URL is not handed back.
        assert_eq!(
            failure(&do_http_get("http://user:secret@127.0.0.1/a b")),
            "InvalidUrl(http://***@127.0.0.1/a b)"
        );
    }

    #[test]
    fn a_port_that_is_no_port_is_invalid_url_and_nothing_is_asked() {
        for url in [
            "http://127.0.0.1:99999/",
            "http://127.0.0.1:65536/",
            "http://127.0.0.1:0/",
            "http://127.0.0.1:abc/",
            "http://127.0.0.1:/",
            "http://127.0.0.1:80x/page?q=1",
            "http://127.0.0.1:-1/",
            "http://127.0.0.1:+80/",
            "http://[::1]:99999/",
            "https://example.com:123456",
        ] {
            let expected = format!("InvalidUrl({url})");
            assert_eq!(failure(&do_http_get(url)), expected);
            assert_eq!(
                failure(&do_http_request("POST", url, b"body", &[])),
                expected
            );
        }
        assert_eq!(
            failure(&do_http_get("http://user:secret@127.0.0.1:99999/")),
            "InvalidUrl(http://***@127.0.0.1:99999/)"
        );
        // A port that is one is not refused here: the colons of an IPv6
        // address and of a password are no port's.
        for url in [
            "http://127.0.0.1:1/",
            "http://127.0.0.1:65535/",
            "http://127.0.0.1/",
            "http://[::1]:1/",
            "http://[::1]/",
            "http://user:secret@127.0.0.1:1/",
            "http://user:99999@127.0.0.1/",
        ] {
            assert!(refuse_port(url).is_none(), "{url}");
        }
    }

    #[test]
    fn a_port_nobody_listens_on_is_connect() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a listener");
        let url = format!("http://{}/", listener.local_addr().expect("its address"));
        drop(listener);
        let got = failure(&do_http_get(&url));
        assert!(got.starts_with("Connect("), "{got}");
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
        assert!(got.starts_with("Tls("), "{got}");
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
        let disagree = "InvalidResponse(protocol: conflicting content-length headers)";
        let no_number = "InvalidResponse(protocol: content-length header not a number)";
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

    /// A redirect's location is resolved against the URL that answered
    /// as RFC 3986 says.
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
        ] {
            assert_eq!(resolve_location(base, location), resolved, "{location}");
        }
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
