//! HTTP/1.1 on the wire, as `http.serve` reads and writes it: one
//! request at a time from a connection, one response back.
//!
//! This is the part of the server that meets bytes from the network,
//! so it is plain functions over `Read` with nothing of the VM in it
//! (it is fuzzed: `fuzz/fuzz_targets/fuzz_http_request.rs`). The rules:
//!
//! - Every limit of the server is a constant below, and a request
//!   beyond one is refused with a status of its own, never read to the
//!   end.
//! - A request whose length is not certain is refused, not guessed:
//!   `Content-Length` twice, or not a plain number, or together with
//!   `Transfer-Encoding`; a transfer coding other than `chunked`; a
//!   chunk size that is no hexadecimal number or does not fit; a line
//!   of the head that ends in a bare LF, or a CR that ends no line.
//!   Such a request is what request smuggling is made of.
//! - After a refusal the connection is closed: what follows on it
//!   cannot be told from the rest of the refused request.
//! - Nothing here panics on any input.

use std::io::{self, Read};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

// ── The limits of the server ─────────────────────────────────────
//
// All of them, and nowhere else. `docs/stdlib/http.md` states them.

/// The longest request head (request line and headers), and the
/// longest trailer section of a chunked body. Beyond it: 431.
pub const HEAD_MAX: usize = 64 * 1024;

/// The most headers of a request. Beyond it: 431.
pub const HEADERS_MAX: usize = 100;

/// The longest request body, given by its length or in chunks.
/// Beyond it: 413.
pub const BODY_MAX: usize = 10 * 1024 * 1024;

/// The longest method. A connection whose first bytes are no method
/// and a space is refused at once (400), not waited out: a TLS
/// handshake sent to this port is no request, however long it lasts.
pub const METHOD_MAX: usize = 32;

/// The most bytes of request bodies that the server holds at a time
/// for requests that no handler has yet: bodies that are being read
/// (a body counts from when its length is known, or chunk by chunk)
/// and bodies that are read and wait to be handed over. A request
/// whose body does not fit is refused, 503: it is not made to wait,
/// which would hold its connection and a thread for a body that has
/// no room, on the client's time. A body of the largest size always
/// fits when no other is held.
///
/// The limit of handlers bounds the bodies beyond this: with every
/// handler holding a request of the largest size, [`HANDLERS_MAX`]
/// times [`BODY_MAX`] more.
pub const BODIES_MAX: usize = 256 * 1024 * 1024;

/// The longest line of a chunked body (a chunk size with its
/// extensions). Beyond it: 400.
pub const CHUNK_LINE_MAX: usize = 4 * 1024;

/// The most bytes of a chunked body that are not the body: the chunk
/// sizes, their extensions and the line ends. Beyond it: 400. A body
/// of the largest size in chunks of a hundred bytes is within it; a
/// body cut into single bytes, or padded with extensions, cannot make
/// the server read many times what it is given. (The trailers have
/// the limit of a head, [`HEAD_MAX`].)
pub const CHUNK_FRAMING_MAX: usize = 1024 * 1024;

/// The most handlers that are called at a time. A request beyond it
/// is answered 503.
pub const HANDLERS_MAX: usize = 128;

/// How long the server waits for the head of a request, from when it
/// starts to wait (the connection was accepted, or the response
/// before was sent) until the head is complete. A connection that is
/// kept alive and sends nothing is closed after it; one whose head
/// trickles in is answered 408 and closed.
pub const REQUEST_TIME: Duration = Duration::from_secs(30);

/// How long the body of a request may take to arrive once its head is
/// there (408 after it), and how long a response may take to be taken
/// by the client. The connection is closed after it.
pub const TRANSFER_TIME: Duration = Duration::from_secs(5 * 60);

/// How long the server goes on reading, and dropping what it reads,
/// after it has refused a request or given up waiting for the rest of
/// one, before it closes the connection. A connection closed with
/// something unread is reset, and the reset can reach a client that
/// is still sending before the answer does.
pub const REFUSAL_TIME: Duration = Duration::from_secs(5);

/// How long the server waits before it accepts again after an accept
/// that failed (the process has no descriptor left, the I/O pool no
/// thread).
pub const ACCEPT_RETRY: Duration = Duration::from_millis(100);

/// A request, read whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The method as sent: `GET`.
    pub method: String,
    /// The request target as sent: `/path?query`.
    pub target: String,
    /// The headers in the order sent, names as sent.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// The connection is to be closed after the response: the request
    /// says so, or it is HTTP/1.0 and does not ask to keep it.
    pub close: bool,
    /// The request is HTTP/1.0: a response after which the connection
    /// is kept says so.
    pub http10: bool,
}

/// Why a request is not served: the status to answer with, and a word
/// for the response body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refused {
    pub status: u16,
    pub why: &'static str,
}

const fn refused(status: u16, why: &'static str) -> Refused {
    Refused { status, why }
}

/// What a connection gives next.
#[derive(Debug)]
pub enum Next {
    Request(Request),
    /// The peer closed the connection, between requests or in the
    /// middle of one: there is nobody to answer.
    End,
    /// The request is refused: answer with the status, then close.
    Refused(Refused),
    /// Reading failed.
    Broken(io::Error),
}

/// Reads the requests of one connection, one after the other. What it
/// has read beyond the end of a request is the start of the next one
/// (a client may send several without waiting: pipelining).
pub struct Reader<R> {
    stream: R,
    /// Read from the stream and not yet used.
    buf: Vec<u8>,
    /// Whether bytes of a request that is not read yet have come
    /// ([`Reader::has_begun`]), for who waits for the reader
    /// elsewhere.
    begun: Arc<AtomicBool>,
    /// Asked before so many more bytes of a body are read: whether
    /// the server has room for them ([`BODIES_MAX`]).
    room: Box<dyn FnMut(usize) -> bool + Send>,
}

/// Either a value or the end of reading this request.
type Step<T> = Result<T, Next>;

impl<R: Read> Reader<R> {
    pub fn new(stream: R) -> Reader<R> {
        Reader::with_room(stream, |_| true)
    }

    /// A reader that asks `room` before it reads so many more bytes
    /// of a body, and refuses the request (503) where there is none.
    pub fn with_room(stream: R, room: impl FnMut(usize) -> bool + Send + 'static) -> Reader<R> {
        Reader {
            stream,
            buf: Vec::new(),
            begun: Arc::default(),
            room: Box::new(room),
        }
    }

    /// Whether the next request has begun to arrive: bytes were read
    /// that no request has used. An empty line before a request is
    /// not its beginning.
    pub fn has_begun(&self) -> bool {
        !matches!(&self.buf[..], [] | [b'\r'] | [b'\r', b'\n'])
    }

    /// [`Reader::has_begun`] as of the reader's last read, for who
    /// cannot ask the reader while it reads.
    pub fn begun(&self) -> Arc<AtomicBool> {
        self.begun.clone()
    }

    /// Take in bytes of the connection that were read elsewhere.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
        self.begun.store(self.has_begun(), Ordering::SeqCst);
    }

    /// The next request, or its refusal, if all of it is among the
    /// bytes that were read already: nothing is read for it. `None`
    /// if more bytes are needed (or the client waits for `100
    /// Continue`); the reader is as it was then, and
    /// [`Reader::next`] reads on.
    pub fn buffered(&mut self) -> Option<Next> {
        /// Beyond this the bytes are not looked through twice.
        const LOOKED_AT: usize = 64 * 1024;
        if self.buf.is_empty() || self.buf.len() > LOOKED_AT {
            return None;
        }
        // Read from a copy with nothing behind it: what it gives is
        // what a read of the connection would have given first.
        // (What is read already is held already: no room is asked.)
        let mut ahead = Reader::new(io::empty());
        ahead.buf.clone_from(&self.buf);
        let mut never_waits = |waits: bool| match waits {
            true => Err(io::Error::other("the client waits")),
            false => Ok(()),
        };
        match ahead.next(&mut never_waits) {
            next @ (Next::Request(_) | Next::Refused(_)) => {
                self.buf = ahead.buf;
                self.begun.store(self.has_begun(), Ordering::SeqCst);
                Some(next)
            }
            Next::End | Next::Broken(_) => None,
        }
    }

    /// The next request. `before_body` is called once when the head
    /// of a request has been read and its body is about to be: with
    /// `true` if the client waits for `100 Continue` before it sends
    /// the body (`Expect: 100-continue`), which the callee then writes
    /// to the connection.
    pub fn next(&mut self, before_body: &mut dyn FnMut(bool) -> io::Result<()>) -> Next {
        match self.request(before_body) {
            Ok(request) => Next::Request(request),
            Err(next) => next,
        }
    }

    fn request(&mut self, before_body: &mut dyn FnMut(bool) -> io::Result<()>) -> Step<Request> {
        self.begun.store(self.has_begun(), Ordering::SeqCst);
        let head = self.head()?;
        let framing = framing(&head)?;
        // A request names the host it is for, once: with none or two,
        // two readers could take it for different ones. (HTTP/1.0 had
        // no such header.)
        let hosts = head.headers.iter();
        let hosts = hosts.filter(|(name, _)| name.eq_ignore_ascii_case("host"));
        let hosts = hosts.count();
        if hosts > 1 || (hosts == 0 && head.minor >= 1) {
            return Err(Next::Refused(refused(400, "Bad Request")));
        }
        let waits = expects_continue(&head)?;
        let mut announce = |reader: &Self| {
            // What is here already was sent without waiting.
            before_body(waits && reader.buf.is_empty()).map_err(Next::Broken)
        };
        let body = match framing {
            Framing::None | Framing::Length(0) => Vec::new(),
            Framing::Length(length) => {
                if !(self.room)(length) {
                    return Err(Next::Refused(NO_ROOM));
                }
                announce(self)?;
                self.exactly(length)?
            }
            Framing::Chunked => {
                announce(self)?;
                self.chunked()?
            }
        };
        let asks = |token: &str| {
            head.headers
                .iter()
                .filter(|(name, _)| name.eq_ignore_ascii_case("connection"))
                .flat_map(|(_, value)| value.split(','))
                .any(|part| part.trim().eq_ignore_ascii_case(token))
        };
        let close = match head.minor {
            0 => !asks("keep-alive"),
            _ => asks("close"),
        };
        // A connection that is kept does not keep the room of the
        // largest body it has carried.
        self.buf.shrink_to(HEAD_MAX);
        Ok(Request {
            method: head.method,
            target: head.target,
            headers: head.headers,
            body,
            close,
            http10: head.minor == 0,
        })
    }

    /// Read more from the stream. `Ok(false)` at its end.
    fn more(&mut self) -> Step<bool> {
        let mut chunk = [0u8; 8 * 1024];
        loop {
            match self.stream.read(&mut chunk) {
                Ok(0) => return Ok(false),
                Ok(n) => {
                    self.buf.extend_from_slice(&chunk[..n]);
                    self.begun.store(self.has_begun(), Ordering::SeqCst);
                    return Ok(true);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(Next::Broken(e)),
            }
        }
    }

    /// The request line and the headers.
    ///
    /// The head is parsed once, when its end (an empty line) is there:
    /// a head that comes byte by byte costs what one that comes whole
    /// does.
    fn head(&mut self) -> Step<Head> {
        let bad = || Next::Refused(refused(400, "Bad Request"));
        // One empty line before the request line is passed over:
        // clients have sent one after a body.
        let mut passed_over = false;
        // How far the buffer has been searched for the head's end.
        let mut searched = 0;
        loop {
            match (self.buf.first(), self.buf.get(1)) {
                (Some(b'\r'), Some(b'\n')) if !passed_over => {
                    self.buf.drain(..2);
                    passed_over = true;
                    continue;
                }
                // Its LF may still come.
                (Some(b'\r'), None) if !passed_over => {}
                // What comes first is a method and a space: bytes that
                // begin no request are refused here, without waiting
                // for the rest of them.
                _ => {
                    let begins = &self.buf[..self.buf.len().min(METHOD_MAX + 1)];
                    let method = begins
                        .split(|byte| *byte == b' ')
                        .next()
                        .unwrap_or_default();
                    if method.len() > METHOD_MAX
                        || begins.starts_with(b" ")
                        || !method.iter().all(|byte| is_token(*byte))
                    {
                        return Err(bad());
                    }
                }
            }
            let from = searched.min(self.buf.len()).saturating_sub(2);
            let ended = self.buf[from..]
                .windows(2)
                .enumerate()
                .any(|(at, pair)| pair == b"\n\n" || self.buf[from + at..].starts_with(b"\n\r\n"));
            searched = self.buf.len();
            if ended {
                let mut headers = [httparse::EMPTY_HEADER; HEADERS_MAX];
                let mut request = httparse::Request::new(&mut headers);
                let length = match request.parse(&self.buf) {
                    Ok(httparse::Status::Complete(length)) => length,
                    Err(httparse::Error::TooManyHeaders) => {
                        return Err(Next::Refused(refused(431, "Too Many Headers")));
                    }
                    // (An empty line ends a head: none is partial.)
                    Ok(httparse::Status::Partial) | Err(_) => return Err(bad()),
                };
                if length > HEAD_MAX {
                    return Err(Next::Refused(HEAD_TOO_LARGE));
                }
                // Every line of the head ends with CRLF, and neither
                // CR nor LF stands anywhere else in it: where a line
                // ends is not for two readers to see differently.
                let raw = &self.buf[..length];
                let lone = (0..raw.len()).any(|i| match raw[i] {
                    b'\n' => i == 0 || raw[i - 1] != b'\r',
                    b'\r' => raw.get(i + 1) != Some(&b'\n'),
                    _ => false,
                });
                if lone {
                    return Err(bad());
                }
                let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
                let head = Head {
                    method: request.method.unwrap_or_default().to_string(),
                    target: request.path.unwrap_or_default().to_string(),
                    minor: request.version.unwrap_or(1),
                    headers: request
                        .headers
                        .iter()
                        .map(|header| (header.name.to_string(), text(header.value)))
                        .collect(),
                };
                self.buf.drain(..length);
                return Ok(head);
            }
            if self.buf.len() >= HEAD_MAX {
                return Err(Next::Refused(HEAD_TOO_LARGE));
            }
            if !self.more()? {
                return Err(Next::End);
            }
        }
    }

    /// Exactly `length` bytes.
    fn exactly(&mut self, length: usize) -> Step<Vec<u8>> {
        while self.buf.len() < length {
            if !self.more()? {
                return Err(Next::End);
            }
        }
        // The bytes themselves, not a copy of them: what was read
        // beyond them is the lesser part.
        let beyond = self.buf.split_off(length);
        Ok(std::mem::replace(&mut self.buf, beyond))
    }

    /// A line of at most `limit` bytes, without its CRLF.
    fn line(&mut self, limit: usize, too_long: Refused) -> Step<Vec<u8>> {
        loop {
            if let Some(end) = self.buf.iter().position(|byte| *byte == b'\n') {
                // The line, its CR, and the LF at `end`.
                if end > limit + 1 {
                    return Err(Next::Refused(too_long));
                }
                let mut line: Vec<u8> = self.buf.drain(..=end).collect();
                line.pop();
                // A line ends with CRLF: a bare LF is not its end, and
                // a CR within it is not either, nor any other control
                // byte (a tab may stand in it).
                let control = |byte: &u8| (*byte < b' ' && *byte != b'\t') || *byte == 0x7f;
                if line.pop() != Some(b'\r') || line.iter().any(control) {
                    return Err(Next::Refused(BAD_CHUNKS));
                }
                return Ok(line);
            }
            // The line and its CR may be here, with the LF to come.
            if self.buf.len() > limit + 1 {
                return Err(Next::Refused(too_long));
            }
            if !self.more()? {
                return Err(Next::End);
            }
        }
    }

    /// A body in chunks: each a size in hexadecimal on a line, that
    /// many bytes, and a line end; a chunk of size 0, then trailers
    /// (read and dropped) up to an empty line.
    fn chunked(&mut self) -> Step<Vec<u8>> {
        let mut body = Vec::new();
        // The bytes that are not the body: each chunk's line with its
        // line end, and the line end after its bytes.
        let mut framing = 0;
        loop {
            let line = self.line(CHUNK_LINE_MAX, BAD_CHUNKS)?;
            framing += line.len() + 4;
            if framing > CHUNK_FRAMING_MAX {
                return Err(Next::Refused(BAD_CHUNKS));
            }
            let digits = match line.iter().position(|byte| *byte == b';') {
                Some(extensions) => &line[..extensions],
                None => &line[..],
            };
            let digits = std::str::from_utf8(digits)
                .unwrap_or("")
                .trim_end_matches([' ', '\t']);
            // Sixteen hexadecimal digits are a u64; a size that does
            // not fit is no size.
            if digits.is_empty()
                || digits.len() > 16
                || !digits.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Err(Next::Refused(BAD_CHUNKS));
            }
            let Ok(size) = u64::from_str_radix(digits, 16) else {
                return Err(Next::Refused(BAD_CHUNKS));
            };
            if size == 0 {
                break;
            }
            if size > (BODY_MAX - body.len()) as u64 {
                return Err(Next::Refused(BODY_TOO_LARGE));
            }
            if !(self.room)(size as usize) {
                return Err(Next::Refused(NO_ROOM));
            }
            body.extend(self.exactly(size as usize)?);
            if !self.line(0, BAD_CHUNKS)?.is_empty() {
                return Err(Next::Refused(BAD_CHUNKS));
            }
        }
        let mut trailers = 0;
        loop {
            let line = self.line(HEAD_MAX, HEAD_TOO_LARGE)?;
            if line.is_empty() {
                return Ok(body);
            }
            trailers += line.len() + 2;
            if trailers > HEAD_MAX {
                return Err(Next::Refused(HEAD_TOO_LARGE));
            }
        }
    }
}

const HEAD_TOO_LARGE: Refused = refused(431, "Request Header Fields Too Large");
const BODY_TOO_LARGE: Refused = refused(413, "Payload Too Large");
const BAD_CHUNKS: Refused = refused(400, "Bad Request");
const NO_ROOM: Refused = refused(503, "Service Unavailable");

struct Head {
    method: String,
    target: String,
    /// The minor version: HTTP/1.`minor`.
    minor: u8,
    headers: Vec<(String, String)>,
}

/// How long the body of a request is.
enum Framing {
    None,
    Length(usize),
    Chunked,
}

fn framing(head: &Head) -> Step<Framing> {
    let bad = || Next::Refused(refused(400, "Bad Request"));
    let of = |wanted: &str| -> Vec<&str> {
        head.headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case(wanted))
            .map(|(_, value)| value.trim_matches([' ', '\t']))
            .collect()
    };
    let lengths = of("content-length");
    let codings = of("transfer-encoding");
    match (&lengths[..], &codings[..]) {
        ([], []) => Ok(Framing::None),
        ([length], []) => {
            if length.is_empty() || !length.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(bad());
            }
            // A number that does not fit is longer than any body.
            match length.parse::<usize>() {
                Ok(length) if length <= BODY_MAX => Ok(Framing::Length(length)),
                Ok(_) => Err(Next::Refused(BODY_TOO_LARGE)),
                Err(_) if length.len() > 15 => Err(Next::Refused(BODY_TOO_LARGE)),
                Err(_) => Err(bad()),
            }
        }
        ([], [coding]) if coding.eq_ignore_ascii_case("chunked") && head.minor >= 1 => {
            Ok(Framing::Chunked)
        }
        // Two lengths, a length and a coding, a coding that is not
        // plain `chunked`, a coding in HTTP/1.0: the length of the
        // request is not certain.
        _ => Err(bad()),
    }
}

/// Whether the client waits for `100 Continue` before it sends the
/// body. Any other expectation is one the server cannot meet.
fn expects_continue(head: &Head) -> Step<bool> {
    let mut expects = head
        .headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("expect"));
    let Some((_, expected)) = expects.next() else {
        return Ok(false);
    };
    if !expected.trim().eq_ignore_ascii_case("100-continue") || expects.next().is_some() {
        return Err(Next::Refused(refused(417, "Expectation Failed")));
    }
    // HTTP/1.0 knows no `100 Continue`.
    Ok(head.minor >= 1)
}

/// The reason phrase of a status.
pub fn reason(status: u16) -> &'static str {
    match status {
        100 => "Continue",
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        304 => "Not Modified",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        410 => "Gone",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        417 => "Expectation Failed",
        422 => "Unprocessable Content",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        // A status without a phrase of its own has its class's.
        _ => match status / 100 {
            1 => "Informational",
            2 => "Success",
            3 => "Redirection",
            4 => "Client Error",
            _ => "Server Error",
        },
    }
}

/// Whether a response of this status has a body: 1xx, 204 and 304
/// have none.
pub fn has_body(status: u16) -> bool {
    !matches!(status, 100..=199 | 204 | 304)
}

/// Whether a method or the name of a header can have this byte.
fn is_token(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

/// The line that tells a client to send its body.
pub const CONTINUE: &[u8] = b"HTTP/1.1 100 Continue\r\n\r\n";

/// How a response is sent.
#[derive(Debug, Clone, Copy, Default)]
pub struct Sending<'a> {
    /// The date, as the `Date` header has it.
    pub date: &'a str,
    /// Say that the connection is closed after this response.
    pub close: bool,
    /// Say that it is kept: to a client of HTTP/1.0, which otherwise
    /// takes it as closed.
    pub keep_alive: bool,
    /// The request was `HEAD`: the head of the response, with the
    /// length of the body, and no body.
    pub head_only: bool,
}

/// The bytes of a response. `Content-Length`, `Date` and `Connection`
/// are the server's to say: such headers among `headers` are left out,
/// as is a header whose name or value could not stand in a header
/// line (a line end in it would start another header, or another
/// response).
pub fn response(status: u16, headers: &[(String, String)], body: &[u8], how: Sending) -> Vec<u8> {
    let own = ["content-length", "transfer-encoding", "connection", "date"];
    let token = |name: &str| !name.is_empty() && name.bytes().all(is_token);
    let plain = |value: &str| {
        value
            .bytes()
            .all(|byte| byte == b'\t' || (byte >= b' ' && byte != 0x7f))
    };
    let bodiless = !has_body(status);
    let mut out = Vec::with_capacity(body.len() + 256);
    out.extend_from_slice(format!("HTTP/1.1 {status} {}\r\n", reason(status)).as_bytes());
    for (name, value) in headers {
        if token(name) && plain(value) && !own.iter().any(|own| name.eq_ignore_ascii_case(own)) {
            out.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
        }
    }
    if !how.date.is_empty() {
        out.extend_from_slice(format!("Date: {}\r\n", how.date).as_bytes());
    }
    if !bodiless {
        out.extend_from_slice(format!("Content-Length: {}\r\n", body.len()).as_bytes());
    }
    if how.close {
        out.extend_from_slice(b"Connection: close\r\n");
    } else if how.keep_alive {
        out.extend_from_slice(b"Connection: keep-alive\r\n");
    }
    out.extend_from_slice(b"\r\n");
    if !bodiless && !how.head_only {
        out.extend_from_slice(body);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read every request of `bytes`, in pieces of `piece` bytes. With
    /// the requests and what ended the reading: how often a client
    /// that waited was told to send its body.
    fn read_all(bytes: &[u8], piece: usize) -> (Vec<Request>, Next, usize) {
        struct Pieces<'a>(&'a [u8], usize);
        impl Read for Pieces<'_> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                let n = self.0.len().min(self.1).min(buf.len());
                buf[..n].copy_from_slice(&self.0[..n]);
                self.0 = &self.0[n..];
                Ok(n)
            }
        }
        let mut reader = Reader::new(Pieces(bytes, piece.max(1)));
        let mut requests = Vec::new();
        let mut continues = 0;
        loop {
            match reader.next(&mut |waits| {
                continues += usize::from(waits);
                Ok(())
            }) {
                Next::Request(request) => requests.push(request),
                last => return (requests, last, continues),
            }
        }
    }

    /// [`read_all`], with every request that is among the bytes read
    /// already taken from there ([`Reader::buffered`]), as the server
    /// does.
    fn read_all_buffered_first(bytes: &[u8], piece: usize) -> (Vec<Request>, Next) {
        struct Pieces<'a>(&'a [u8], usize);
        impl Read for Pieces<'_> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                let n = self.0.len().min(self.1).min(buf.len());
                buf[..n].copy_from_slice(&self.0[..n]);
                self.0 = &self.0[n..];
                Ok(n)
            }
        }
        // Some of it comes to the reader from elsewhere.
        let fed = bytes.len().min(piece);
        let mut reader = Reader::new(Pieces(&bytes[fed..], piece.max(1)));
        reader.feed(&bytes[..fed]);
        let mut requests = Vec::new();
        loop {
            let next = match reader.buffered() {
                Some(next) => next,
                None => reader.next(&mut |_| Ok(())),
            };
            match next {
                Next::Request(request) => requests.push(request),
                last => return (requests, last),
            }
        }
    }

    /// The one request of `bytes`, however it arrives.
    fn one(bytes: &[u8]) -> Request {
        let mut read = None;
        for piece in [1, 2, 3, 7, 4096] {
            let (mut requests, last, _) = read_all(bytes, piece);
            assert!(matches!(last, Next::End), "{last:?}");
            assert_eq!(requests.len(), 1, "{requests:?}");
            let request = requests.remove(0);
            assert!(read.is_none() || read == Some(request.clone()));
            read = Some(request);
        }
        read.expect("read")
    }

    fn refusal(bytes: &[u8]) -> u16 {
        for piece in [1, 2, 5, 4096] {
            match read_all(bytes, piece) {
                (requests, Next::Refused(refused), _) if requests.is_empty() => {
                    if piece == 4096 {
                        return refused.status;
                    }
                }
                other => panic!("expected a refusal, got {other:?}"),
            }
        }
        unreachable!()
    }

    #[test]
    fn a_request_without_a_body() {
        let request = one(b"GET /a?b=1 HTTP/1.1\r\nHost: x\r\nX-Two: 2\r\n\r\n");
        assert_eq!(request.method, "GET");
        assert_eq!(request.target, "/a?b=1");
        assert_eq!(
            request.headers,
            [
                ("Host".to_string(), "x".to_string()),
                ("X-Two".to_string(), "2".to_string())
            ]
        );
        assert!(request.body.is_empty());
        assert!(!request.close);
    }

    #[test]
    fn a_body_of_a_given_length() {
        let request = one(b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\n\r\nhello");
        assert_eq!(request.body, b"hello");
    }

    #[test]
    fn a_body_in_chunks_with_extensions_and_trailers() {
        let request = one(
            b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n\
              5;name=value\r\nhello\r\n1\r\n \r\n6\r\nworld!\r\n0\r\nTrailer: dropped\r\n\r\n",
        );
        assert_eq!(request.body, b"hello world!");
    }

    #[test]
    fn requests_sent_without_waiting_come_in_order() {
        let (requests, last, _) = read_all(
            b"GET /1 HTTP/1.1\r\nHost: x\r\n\r\nPOST /2 HTTP/1.1\r\nHost: x\r\nContent-Length: 2\r\n\r\nhiGET /3 HTTP/1.1\r\nHost: x\r\n\r\n",
            3,
        );
        assert!(matches!(last, Next::End));
        let targets: Vec<&str> = requests.iter().map(|r| r.target.as_str()).collect();
        assert_eq!(targets, ["/1", "/2", "/3"]);
        assert_eq!(requests[1].body, b"hi");
    }

    /// Bytes that begin no request are refused when they are there:
    /// the reader does not wait for more of them.
    #[test]
    fn what_begins_no_request_is_refused_at_once() {
        struct Once(Option<Vec<u8>>);
        impl Read for Once {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                let bytes = self.0.take().expect("the reader waits for more");
                buf[..bytes.len()].copy_from_slice(&bytes);
                Ok(bytes.len())
            }
        }
        let long = "M".repeat(METHOD_MAX + 1);
        for begins in [
            // A TLS handshake.
            &b"\x16\x03\x01\x02\x00\x01\x00\x01\xfc\x03\x03"[..],
            b" GET / HTTP/1.1",
            b"GET\t/ HTTP/1.1",
            b"G\x00T / HTTP/1.1",
            b"\n",
            b"\r\r",
            long.as_bytes(),
        ] {
            let mut reader = Reader::new(Once(Some(begins.to_vec())));
            let next = reader.next(&mut |_| Ok(()));
            assert!(
                matches!(next, Next::Refused(Refused { status: 400, .. })),
                "{begins:?}: {next:?}"
            );
        }
        // A method of the longest length is one.
        let longest = format!("{} / HTTP/1.1\r\nHost: x\r\n\r\n", "M".repeat(METHOD_MAX));
        assert_eq!(one(longest.as_bytes()).method.len(), METHOD_MAX);
    }

    /// A request that is among the bytes read already is taken from
    /// them, and one that is not all there leaves the reader as it
    /// was.
    #[test]
    fn a_request_that_was_read_already_needs_no_read() {
        struct Never;
        impl Read for Never {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                panic!("the reader reads")
            }
        }
        let mut reader = Reader::new(Never);
        assert!(reader.buffered().is_none());
        reader.feed(
            b"GET /1 HTTP/1.1\r\nHost: x\r\n\r\nPOST /2 HTTP/1.1\r\nHost: x\r\nContent-Length: 2\r\n\r\nhiGET /3 HT",
        );
        let mut targets = Vec::new();
        while let Some(Next::Request(request)) = reader.buffered() {
            targets.push(request.target);
        }
        assert_eq!(targets, ["/1", "/2"]);
        // The third is not all there, twice over; then it is.
        assert!(reader.buffered().is_none());
        reader.feed(b"TP/1.1\r\nHost: x\r\nContent-Length: 3\r\n\r\nab");
        assert!(reader.buffered().is_none());
        reader.feed(b"c");
        assert!(
            matches!(reader.buffered(), Some(Next::Request(request)) if request.body == b"abc")
        );
        // A refusal is there as soon as it is certain.
        reader.feed(b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: -1\r\n\r\n");
        assert!(matches!(
            reader.buffered(),
            Some(Next::Refused(Refused { status: 400, .. }))
        ));
        // A client that waits to be told is not served from here: the
        // answer to it is written by who reads the connection.
        let mut reader = Reader::new(Never);
        reader.feed(
            b"POST / HTTP/1.1\r\nHost: x\r\nExpect: 100-continue\r\nContent-Length: 2\r\n\r\n",
        );
        assert!(reader.buffered().is_none());
    }

    /// The framing of a chunked body has a bound of its own: a body
    /// cut small enough, or padded with extensions, is refused when
    /// what is not body goes beyond it, and not read on.
    #[test]
    fn the_framing_of_chunks_is_bounded() {
        let head: &[u8] = b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n";
        // One byte to a chunk: seven bytes on the wire, six of them
        // framing as it is counted (the size, two line ends, one more
        // for the line end after the bytes).
        let chunks = |n: usize| {
            let mut bytes = head.to_vec();
            for _ in 0..n {
                bytes.extend_from_slice(b"1\r\nx\r\n");
            }
            bytes
        };
        let within = CHUNK_FRAMING_MAX / 5 - 1;
        let mut whole = chunks(within);
        whole.extend_from_slice(b"0\r\n\r\n");
        match read_all(&whole, 64 * 1024) {
            (requests, Next::End, _) => assert_eq!(requests[0].body.len(), within),
            other => panic!("{:?}", other.1),
        }
        // More chunks than fit, and no end to them.
        let endless = chunks(CHUNK_FRAMING_MAX / 5 + 1);
        assert!(matches!(
            read_all(&endless, 64 * 1024),
            (requests, Next::Refused(Refused { status: 400, .. }), _) if requests.is_empty()
        ));
        // A few bytes of body, and extensions as long as a line may be.
        let mut padded = head.to_vec();
        let extension = "e".repeat(CHUNK_LINE_MAX - 8);
        for _ in 0..CHUNK_FRAMING_MAX / CHUNK_LINE_MAX + 1 {
            padded.extend_from_slice(format!("1;{extension}\r\nx\r\n").as_bytes());
        }
        assert!(matches!(
            read_all(&padded, 64 * 1024),
            (requests, Next::Refused(Refused { status: 400, .. }), _) if requests.is_empty()
        ));
    }

    /// A reader that is told there is no room refuses the body, by
    /// its length before a byte of it is read, or at the chunk that
    /// does not fit; and it asks for no more than the body has.
    #[test]
    fn a_body_that_has_no_room_is_refused() {
        let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
        let reader = |bytes: &'static [u8], room: usize| {
            let asked = asked.clone();
            asked.lock().unwrap().clear();
            let mut left = room;
            Reader::with_room(bytes, move |wanted| {
                asked.lock().unwrap().push(wanted);
                let fits = wanted <= left;
                if fits {
                    left -= wanted;
                }
                fits
            })
        };
        let by_length: &[u8] = b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\n\r\nhello";
        let next = reader(by_length, 5).next(&mut |_| Ok(()));
        assert!(matches!(next, Next::Request(request) if request.body == b"hello"));
        assert_eq!(*asked.lock().unwrap(), [5]);
        let mut announced = false;
        let next = reader(by_length, 4).next(&mut |_| {
            announced = true;
            Ok(())
        });
        assert!(matches!(next, Next::Refused(Refused { status: 503, .. })));
        // The client was not told to send it.
        assert!(!announced);

        let chunked: &[u8] = b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n\
              3\r\nabc\r\n4\r\ndefg\r\n0\r\n\r\n";
        let next = reader(chunked, 7).next(&mut |_| Ok(()));
        assert!(matches!(next, Next::Request(request) if request.body == b"abcdefg"));
        assert_eq!(*asked.lock().unwrap(), [3, 4]);
        let next = reader(chunked, 6).next(&mut |_| Ok(()));
        assert!(matches!(next, Next::Refused(Refused { status: 503, .. })));
        // A request without a body asks for nothing.
        let next = reader(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n", 0).next(&mut |_| Ok(()));
        assert!(matches!(next, Next::Request(_)));
        assert!(asked.lock().unwrap().is_empty());
    }

    /// HTTP/1.0 had no Host header: a request without one is served.
    #[test]
    fn a_request_names_its_host_once() {
        assert_eq!(one(b"GET / HTTP/1.0\r\n\r\n").headers.len(), 0);
        assert_eq!(one(b"GET / HTTP/1.0\r\nHost: a\r\n\r\n").headers.len(), 1);
        assert_eq!(one(b"GET / HTTP/1.1\r\nhOsT: a\r\n\r\n").headers.len(), 1);
    }

    /// What tells a request that has begun from a connection that has
    /// sent nothing: an empty line before a request is nothing.
    #[test]
    fn an_empty_line_is_no_beginning() {
        let mut reader = Reader::new(io::empty());
        let begun = reader.begun();
        for (bytes, has) in [
            (&b"\r"[..], false),
            (b"\n", false),
            (b"G", true),
            (b"ET / HTTP/1.1\r\nHost: x\r\n\r\n", true),
        ] {
            reader.feed(bytes);
            assert_eq!(reader.has_begun(), has, "{bytes:?}");
            assert_eq!(begun.load(Ordering::SeqCst), has, "{bytes:?}");
        }
        assert!(matches!(reader.buffered(), Some(Next::Request(_))));
        assert!(!reader.has_begun() && !begun.load(Ordering::SeqCst));
    }

    #[test]
    fn who_wants_the_connection_closed() {
        assert!(one(b"GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").close);
        assert!(one(b"GET / HTTP/1.1\r\nHost: x\r\nConnection: Keep-Alive, Close\r\n\r\n").close);
        assert!(one(b"GET / HTTP/1.0\r\n\r\n").close);
        assert!(!one(b"GET / HTTP/1.0\r\nConnection: keep-alive\r\n\r\n").close);
        assert!(!one(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n").close);
        assert!(one(b"GET / HTTP/1.0\r\n\r\n").http10);
        assert!(!one(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n").http10);
    }

    #[test]
    fn one_empty_line_before_a_request_is_passed_over() {
        let (requests, last, _) = read_all(
            b"POST /1 HTTP/1.1\r\nHost: x\r\nContent-Length: 2\r\n\r\nhi\r\nGET /2 HTTP/1.1\r\nHost: x\r\n\r\n",
            4,
        );
        assert!(matches!(last, Next::End));
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].target, "/2");
        // Empty lines without end are no request.
        assert_eq!(refusal(&b"\r\n".repeat(40)), 400);
        assert_eq!(
            refusal(b"\r\n\r\n\r\nGET / HTTP/1.1\r\nHost: x\r\n\r\n"),
            400
        );
    }

    #[test]
    fn a_client_that_waits_is_told_to_go_on() {
        // The body is not there yet when the head has been read.
        struct Waits(Vec<&'static [u8]>);
        impl Read for Waits {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.0.is_empty() {
                    return Ok(0);
                }
                let part = self.0.remove(0);
                buf[..part.len()].copy_from_slice(part);
                Ok(part.len())
            }
        }
        // What the reader was told before each body: whether the
        // client waits.
        let told = |parts: Vec<&'static [u8]>| {
            let mut reader = Reader::new(Waits(parts));
            let mut told = Vec::new();
            let next = reader.next(&mut |waits| {
                told.push(waits);
                Ok(())
            });
            assert!(matches!(next, Next::Request(request) if request.body == b"ok"));
            told
        };
        let head: &[u8] =
            b"POST / HTTP/1.1\r\nHost: x\r\nExpect: 100-continue\r\nContent-Length: 2\r\n\r\n";
        assert_eq!(told(vec![head, b"ok"]), [true]);
        let chunked: &[u8] =
            b"POST / HTTP/1.1\r\nHost: x\r\nExpect: 100-Continue\r\nTransfer-Encoding: chunked\r\n\r\n";
        assert_eq!(told(vec![chunked, b"2\r\nok\r\n0\r\n\r\n"]), [true]);
        // A client that did not wait is not told.
        assert_eq!(
            told(vec![
                b"POST / HTTP/1.1\r\nHost: x\r\nExpect: 100-continue\r\nContent-Length: 2\r\n\r\nok"
            ]),
            [false]
        );
        // One that expects nothing is not, and HTTP/1.0 knows no such
        // answer.
        assert_eq!(
            told(vec![
                b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 2\r\n\r\n",
                b"ok"
            ]),
            [false]
        );
        assert_eq!(
            told(vec![
                b"POST / HTTP/1.0\r\nExpect: 100-continue\r\nContent-Length: 2\r\n\r\n",
                b"ok"
            ]),
            [false]
        );
        // Without a body nothing is announced.
        let mut reader = Reader::new(Waits(vec![
            b"GET / HTTP/1.1\r\nHost: x\r\nExpect: 100-continue\r\nContent-Length: 0\r\n\r\n",
        ]));
        let mut announced = 0;
        let next = reader.next(&mut |_| {
            announced += 1;
            Ok(())
        });
        assert!(matches!(next, Next::Request(_)));
        assert_eq!(announced, 0);
        // An expectation the server cannot meet.
        assert_eq!(
            refusal(
                b"POST / HTTP/1.1\r\nHost: x\r\nExpect: something\r\nContent-Length: 2\r\n\r\nok"
            ),
            417
        );
        // The answer cannot be written: the connection is broken.
        let mut reader = Reader::new(Waits(vec![head, b"ok"]));
        let next = reader.next(&mut |_| Err(io::Error::other("gone")));
        assert!(matches!(next, Next::Broken(_)));
    }

    #[test]
    fn the_limits() {
        // The head.
        let mut long = b"GET / HTTP/1.1\r\nHost: x\r\nX: ".to_vec();
        long.extend(std::iter::repeat_n(b'a', HEAD_MAX));
        long.extend_from_slice(b"\r\n\r\n");
        assert_eq!(refusal(&long), 431);
        let mut under = b"GET / HTTP/1.1\r\nHost: x\r\nX: ".to_vec();
        under.extend(std::iter::repeat_n(b'a', HEAD_MAX - 100));
        under.extend_from_slice(b"\r\n\r\n");
        assert_eq!(one(&under).headers[1].1.len(), HEAD_MAX - 100);
        // The headers.
        let many = |n: usize| {
            let mut bytes = b"GET / HTTP/1.1\r\nHost: x\r\n".to_vec();
            for i in 1..n {
                bytes.extend_from_slice(format!("H{i}: v\r\n").as_bytes());
            }
            bytes.extend_from_slice(b"\r\n");
            bytes
        };
        assert_eq!(one(&many(HEADERS_MAX)).headers.len(), HEADERS_MAX);
        assert_eq!(refusal(&many(HEADERS_MAX + 1)), 431);
        // The body: by its declared length, without reading it.
        let declared = format!(
            "POST / HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n",
            BODY_MAX + 1
        );
        assert_eq!(refusal(declared.as_bytes()), 413);
        assert_eq!(
            refusal(
                b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 99999999999999999999999999\r\n\r\n"
            ),
            413
        );
        // The body in chunks: when the chunk that goes over is announced.
        let mut chunks =
            b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        let half = BODY_MAX / 2 + 1;
        for _ in 0..2 {
            chunks.extend_from_slice(format!("{half:x}\r\n").as_bytes());
            chunks.extend(std::iter::repeat_n(b'b', half));
            chunks.extend_from_slice(b"\r\n");
        }
        assert!(matches!(
            read_all(&chunks, 64 * 1024),
            (requests, Next::Refused(Refused { status: 413, .. }), _) if requests.is_empty()
        ));
        // A body of exactly the limit is taken.
        let mut exact = format!("POST / HTTP/1.1\r\nHost: x\r\nContent-Length: {BODY_MAX}\r\n\r\n")
            .into_bytes();
        exact.extend(std::iter::repeat_n(b'c', BODY_MAX));
        match read_all(&exact, 64 * 1024) {
            (requests, Next::End, _) => assert_eq!(requests[0].body.len(), BODY_MAX),
            other => panic!("{:?}", other.1),
        }
        // The trailers.
        let mut trailers =
            b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n".to_vec();
        for _ in 0..(HEAD_MAX / 10 + 10) {
            trailers.extend_from_slice(b"T: 123456\r\n");
        }
        trailers.extend_from_slice(b"\r\n");
        assert_eq!(refusal(&trailers), 431);
    }

    /// The shapes that request smuggling is made of: a request whose
    /// length two readers could take differently is refused.
    #[test]
    fn a_request_of_uncertain_length_is_refused() {
        for request in [
            // Two lengths, the same or not.
            &b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 4\r\nContent-Length: 4\r\n\r\nabcd"[..],
            b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 4\r\nContent-Length: 5\r\n\r\nabcd",
            b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 4, 4\r\n\r\nabcd",
            // A length and a coding.
            b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 4\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
            b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\nContent-Length: 4\r\n\r\n0\r\n\r\n",
            // No plain number.
            b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: -1\r\n\r\n",
            b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: +4\r\n\r\nabcd",
            b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 0x4\r\n\r\nabcd",
            b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 4 4\r\n\r\nabcd",
            b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length:\r\n\r\n",
            // A coding that is not plain `chunked`, or twice.
            b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: gzip, chunked\r\n\r\n0\r\n\r\n",
            b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: xchunked\r\n\r\n0\r\n\r\n",
            b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
            b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: identity\r\n\r\n",
            // A coding in HTTP/1.0.
            b"POST / HTTP/1.0\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
            // Chunk sizes that are no size.
            b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n",
            b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n-1\r\n",
            b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n0x5\r\nhello\r\n0\r\n\r\n",
            b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n\r\n",
            b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n1ffffffffffffffff\r\n",
            b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n5 5\r\nhello\r\n0\r\n\r\n",
            // A chunk that is not followed by a line end, a bare LF.
            b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhelloXX0\r\n\r\n",
            b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n5\nhello\r\n0\r\n\r\n",
            // No host, or two: which one is it for?
            b"GET / HTTP/1.1\r\n\r\n",
            b"GET / HTTP/1.1\r\nX: y\r\n\r\n",
            b"GET / HTTP/1.1\r\nHost: a\r\nHost: b\r\n\r\n",
            b"GET / HTTP/1.1\r\nHost: a\r\nhost: a\r\n\r\n",
            b"GET / HTTP/1.0\r\nHost: a\r\nHost: b\r\n\r\n",
            // A control byte in a chunk's line or in a trailer.
            b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n5;a=\x00\r\nhello\r\n0\r\n\r\n",
            b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n5;a=\x7f\r\nhello\r\n0\r\n\r\n",
            b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n0\r\nT: a\x01b\r\n\r\n",
            // A CR within a chunk's line.
            b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n5;a=\rb\r\nhello\r\n0\r\n\r\n",
            b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n0\r\nT: a\rb\r\n\r\n",
            // A bare LF in a trailer.
            b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n0\r\nT: a\n\r\n",
            // Lines of the head that end in a bare LF; a CR that ends
            // no line.
            b"GET / HTTP/1.1\nHost: x\n\n",
            b"GET / HTTP/1.1\r\nHost: x\n\r\n",
            b"GET / HTTP/1.1\r\nHost: x\r\n\n",
            b"GET / HTTP/1.1\nHost: x\r\n\r\n",
            b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 4\nX: y\r\n\r\nabcd",
            b"GET / HTTP/1.1\r\nHost: x\r\nX: a\rb\r\n\r\n",
            b"\nGET / HTTP/1.1\r\nHost: x\r\n\r\n",
            // Whitespace before the colon of a header; a header that
            // goes on in the next line.
            b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length : 4\r\n\r\nabcd",
            b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding : chunked\r\n\r\n0\r\n\r\n",
            b"GET / HTTP/1.1\r\nHost: x\r\nX: a\r\n b\r\n\r\n",
            b"GET / HTTP/1.1\r\nHost: x\r\n X: a\r\n\r\n",
            // A head that is no head.
            b"GET\r\n\r\n",
            b"GET  / HTTP/1.1\r\nHost: x\r\n\r\n",
            b"GET / HTTP/1.1 \r\n\r\n",
            b"GET /a b HTTP/1.1\r\nHost: x\r\n\r\n",
            b"GET / HTTP/1.1\r\nHost: x\r\n: x\r\n\r\n",
            b"GET / HTTP/1.1\r\nHost: x\r\nX: a\x00b\r\n\r\n",
            b"GET / HTTP/2.0\r\n\r\n",
            b"GET / HTTP/1.1\r\nHost: x\r\nNo colon\r\n\r\n",
            b"GET / HTTP/1.1\r\nHost: x\r\nBad Name: x\r\n\r\n",
            b"\x00\x01\x02\x03\r\n\r\n",
        ] {
            assert_eq!(refusal(request), 400, "{}", String::from_utf8_lossy(request));
        }
        // A size of sixteen digits fits a number and not the limit.
        assert_eq!(
            refusal(b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\nffffffffffffffff\r\n"),
            413
        );
    }

    /// A request that stops anywhere before its end is no request:
    /// there is nobody to answer, and nothing is made up.
    #[test]
    fn a_request_cut_short_is_the_end() {
        let whole: &[u8] =
            b"POST /p HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n";
        for cut in 0..whole.len() {
            let (requests, last, _) = read_all(&whole[..cut], 3);
            assert!(requests.is_empty(), "cut at {cut}");
            assert!(matches!(last, Next::End), "cut at {cut}: {last:?}");
        }
        let whole: &[u8] = b"POST /p HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\n\r\nhello";
        for cut in 0..whole.len() {
            let (requests, last, _) = read_all(&whole[..cut], 3);
            assert!(
                requests.is_empty() && matches!(last, Next::End),
                "cut at {cut}"
            );
        }
    }

    /// Bytes that are no HTTP at all: every outcome but a panic, and
    /// never a request out of nothing.
    #[test]
    fn garbage_is_refused_or_the_end() {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut byte = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        };
        for round in 0..2000 {
            let length = (round * 7) % 600;
            let garbage: Vec<u8> = (0..length).map(|_| byte()).collect();
            let (requests, last, _) = read_all(&garbage, 1 + round % 50);
            assert!(requests.is_empty(), "{garbage:?}");
            assert!(matches!(last, Next::End | Next::Refused(_)), "{last:?}");
        }
    }

    /// The seeds of the fuzz target (`fuzz/corpus/fuzz_http_request`)
    /// hold what the target asserts: read the same however they
    /// arrive.
    #[test]
    fn the_fuzz_seeds_read_the_same_however_they_arrive() {
        let seeds = concat!(env!("CARGO_MANIFEST_DIR"), "/fuzz/corpus/fuzz_http_request");
        let ended = |last: Next| match last {
            Next::Refused(refused) => Some(refused.status),
            Next::End => None,
            other => panic!("{other:?}"),
        };
        let outcome = |bytes: &[u8], piece: usize| {
            let (requests, last, _) = read_all(bytes, piece);
            (requests, ended(last))
        };
        let buffered_first = |bytes: &[u8], piece: usize| {
            let (requests, last) = read_all_buffered_first(bytes, piece);
            (requests, ended(last))
        };
        let mut read = 0;
        for seed in std::fs::read_dir(seeds).expect("the seeds") {
            let path = seed.expect("a seed").path();
            let bytes = std::fs::read(&path).expect("a seed's bytes");
            let whole = outcome(&bytes, usize::MAX);
            let named = path.file_name().unwrap().to_string_lossy().into_owned();
            // What a seed is called says how it ends.
            assert_eq!(whole.1.is_some(), named.starts_with("refused_"), "{named}");
            assert_eq!(
                whole.0.is_empty(),
                !named.starts_with("request_"),
                "{named}"
            );
            for piece in [1, 7] {
                assert_eq!(outcome(&bytes, piece), whole, "{named}");
            }
            for piece in [1, 7, 30, 4096] {
                assert_eq!(buffered_first(&bytes, piece), whole, "{named}");
            }
            read += 1;
        }
        assert!(read >= 10, "{read} seeds");
    }

    #[test]
    fn a_response_as_sent() {
        let headers = [
            ("Content-Type".to_string(), "text/plain".to_string()),
            // The server's own, and what cannot stand in a header line.
            ("Content-Length".to_string(), "999".to_string()),
            ("connection".to_string(), "keep-alive".to_string()),
            ("X-Split".to_string(), "a\r\nSet-Cookie: stolen".to_string()),
            ("Bad Name".to_string(), "x".to_string()),
        ];
        let how = Sending {
            date: "Thu, 01 Jan 2026 00:00:00 GMT",
            ..Sending::default()
        };
        assert_eq!(
            String::from_utf8(response(200, &headers, b"hi", how)).unwrap(),
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\
             Date: Thu, 01 Jan 2026 00:00:00 GMT\r\nContent-Length: 2\r\n\r\nhi"
        );
        let closing = Sending {
            close: true,
            head_only: true,
            ..how
        };
        assert_eq!(
            String::from_utf8(response(404, &[], b"gone", closing)).unwrap(),
            "HTTP/1.1 404 Not Found\r\nDate: Thu, 01 Jan 2026 00:00:00 GMT\r\n\
             Content-Length: 4\r\nConnection: close\r\n\r\n"
        );
        assert_eq!(
            String::from_utf8(response(204, &[], b"ignored", Sending::default())).unwrap(),
            "HTTP/1.1 204 No Content\r\n\r\n"
        );
        let kept = Sending {
            keep_alive: true,
            ..Sending::default()
        };
        assert_eq!(
            String::from_utf8(response(200, &[], b"", kept)).unwrap(),
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: keep-alive\r\n\r\n"
        );
    }
}
