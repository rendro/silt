//! `http.serve` on the wire: what a client that speaks HTTP/1.1 over a
//! socket gets from it, request by request, and what one that speaks
//! something else gets.
//!
//! The server runs in a VM of the test's own, on a port the system
//! chose, and the clients are plain sockets: a silt string cannot hold
//! a CR, and most of what is tested here is where the lines end. The
//! time limits are tested on a clock that the test moves. Nothing here
//! waits for a length of time to pass in order to be right: a wait is
//! either for something that must come (bounded by [`PATIENCE`]) or a
//! look whether something that must not come has come.

#![cfg(feature = "http")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use silt::http_wire::{
    BODIES_MAX, BODY_MAX, HEAD_MAX, HEADERS_MAX, REFUSAL_TIME, REQUEST_TIME, TRANSFER_TIME,
};
use silt::session::testing::compile_str;
use silt::{Buffer, Clock, HostIo, Value, Vm};

/// How long a test waits for what must come before it calls it lost.
const PATIENCE: Duration = Duration::from_secs(60);

/// A handler that answers with what it was asked, and fails on
/// `/fail`.
const ECHO: &str = r#"match req.path {
        "/fail" -> panic("the handler failed: secret-detail")
        _ -> http.Response { status: 200, body: "{req.method} {req.path} [{req.body}]", headers: #{} }
      }"#;

/// A clock that stands still unless the test moves it.
#[derive(Clone, Default)]
struct TestClock(Arc<AtomicU64>);

impl TestClock {
    fn advance(&self, by: Duration) {
        self.0.fetch_add(by.as_millis() as u64, Ordering::SeqCst);
    }
}

impl Clock for TestClock {
    fn now(&self) -> Duration {
        // 2026-10-05T12:00:00Z.
        Duration::from_millis(1_791_201_600_000) + self.monotonic()
    }

    fn monotonic(&self) -> Duration {
        Duration::from_millis(self.0.load(Ordering::SeqCst))
    }

    fn sleep(&self, duration: Duration) {
        self.advance(duration);
    }
}

/// A program that serves HTTP with a handler, in a VM of the test's.
/// It ends when this is dropped.
struct Server {
    port: u16,
    control: u16,
    out: Buffer,
    err: Buffer,
    running: Option<thread::JoinHandle<Result<Value, String>>>,
}

impl Server {
    /// Serve with `handler`, the body of a function of `req`.
    fn new(handler: &str) -> Server {
        Server::on(handler, None)
    }

    fn on(handler: &str, clock: Option<TestClock>) -> Server {
        let source = format!(
            r#"
import http
import string
import task
import tcp

fn main() {{
  when let Ok(listener) = tcp.listen("127.0.0.1:0") else {{ panic("cannot listen") }}
  when let Ok(control) = tcp.listen("127.0.0.1:0") else {{ panic("cannot listen") }}
  let server = task.spawn {{ ->
    http.serve(listener) {{ req ->
      {handler}
    }}
  }}
  println("{{tcp.local_port(listener)}} {{tcp.local_port(control)}} {{string.length("")}}")
  -- The test connects here when it is done.
  let _ = tcp.accept(control)
  task.cancel(server)
}}
"#
        );
        Server::program(&source, clock)
    }

    /// Run `source`, which prints the port it serves on and the port
    /// of a listener that the test connects to when it is done, on
    /// one line.
    fn program(source: &str, clock: Option<TestClock>) -> Server {
        let program = compile_str(source).unwrap_or_else(|errors| panic!("{errors:?}"));
        let (out, err) = (Buffer::new(), Buffer::new());
        let mut io = HostIo::new(out.clone(), err.clone());
        if let Some(clock) = clock {
            io = io.clock(clock);
        }
        let running = thread::spawn(move || {
            let mut vm = Vm::new(io);
            let result = vm.run_program(&program).map_err(|e| e.message);
            vm.settle();
            result
        });
        let limit = Instant::now() + PATIENCE;
        let ports = loop {
            let printed = out.contents();
            if let Some((line, _)) = printed.split_once('\n') {
                break line.to_string();
            }
            assert!(
                Instant::now() < limit && !running.is_finished(),
                "the server did not start: {}",
                err.contents()
            );
            thread::sleep(Duration::from_millis(2));
        };
        let mut ports = ports.split(' ').map(|port| port.parse().expect("a port"));
        Server {
            port: ports.next().expect("the port"),
            control: ports.next().expect("the control port"),
            out,
            err,
            running: Some(running),
        }
    }

    fn connect(&self) -> Client {
        let conn = TcpStream::connect(("127.0.0.1", self.port)).expect("connect");
        conn.set_read_timeout(Some(PATIENCE)).expect("read timeout");
        conn.set_write_timeout(Some(PATIENCE))
            .expect("write timeout");
        Client(BufReader::new(conn))
    }

    /// One request on a connection of its own, and its response.
    fn ask(&self, request: &[u8]) -> Response {
        let mut client = self.connect();
        client.send(request);
        client.response()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        drop(TcpStream::connect(("127.0.0.1", self.control)));
        if let Some(running) = self.running.take() {
            let result = running.join();
            if !thread::panicking() {
                assert_eq!(result.expect("the program ran"), Ok(Value::Unit));
            }
        }
    }
}

struct Response {
    status: u16,
    /// The header lines as sent, without their line ends.
    headers: Vec<String>,
    body: String,
}

impl Response {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find_map(|line| {
            let (found, value) = line.split_once(':')?;
            found.eq_ignore_ascii_case(name).then(|| value.trim())
        })
    }
}

struct Client(BufReader<TcpStream>);

impl Client {
    fn send(&mut self, bytes: &[u8]) {
        self.0.get_mut().write_all(bytes).expect("send");
    }

    /// The status line and the headers of the next response.
    fn head(&mut self) -> (u16, Vec<String>) {
        let mut lines = Vec::new();
        loop {
            let mut line = String::new();
            let n = self.0.read_line(&mut line).expect("a line of the head");
            assert!(n > 0, "the connection ended in a head: {lines:?}");
            assert!(line.ends_with("\r\n"), "a line without CRLF: {line:?}");
            line.truncate(line.len() - 2);
            if line.is_empty() {
                break;
            }
            lines.push(line);
        }
        let status = lines.remove(0);
        let mut parts = status.splitn(3, ' ');
        assert_eq!(parts.next(), Some("HTTP/1.1"), "{status:?}");
        let status = parts.next().and_then(|code| code.parse().ok());
        (status.expect("a status"), lines)
    }

    /// The next response, whose body has the length its head says.
    fn response(&mut self) -> Response {
        let (status, headers) = self.head();
        let mut response = Response {
            status,
            headers,
            body: String::new(),
        };
        if let Some(length) = response.header("Content-Length") {
            let mut body = vec![0; length.parse().expect("a length")];
            self.0.read_exact(&mut body).expect("the body");
            response.body = String::from_utf8(body).expect("text");
        }
        response
    }

    /// What still comes until the server closes the connection.
    fn rest(&mut self) -> Vec<u8> {
        let mut rest = Vec::new();
        match self.0.read_to_end(&mut rest) {
            Ok(_) => {}
            // Closed with something of ours unread.
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {}
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionAborted => {}
            Err(e) => panic!("the server did not close the connection: {e}"),
        }
        rest
    }

    /// Whether the server has sent something, or closed the
    /// connection: a look, without waiting. `false` is "not yet".
    fn has_word(&mut self) -> bool {
        let conn = self.0.get_mut();
        conn.set_nonblocking(true).expect("nonblocking");
        let mut byte = [0u8; 1];
        let word = match conn.peek(&mut byte) {
            Ok(_) => true,
            Err(e) => e.kind() != std::io::ErrorKind::WouldBlock,
        };
        conn.set_nonblocking(false).expect("blocking");
        word
    }

    /// Whether the server has closed the connection, having sent
    /// nothing: a look, without waiting. `false` is "not yet".
    fn is_closed(&mut self) -> bool {
        let conn = self.0.get_mut();
        conn.set_nonblocking(true).expect("nonblocking");
        let mut byte = [0u8; 1];
        let closed = match conn.read(&mut byte) {
            Ok(0) => true,
            Ok(_) => panic!("the server sent something"),
            Err(e) => e.kind() != std::io::ErrorKind::WouldBlock,
        };
        conn.set_nonblocking(false).expect("blocking");
        closed
    }
}

// ── Requests and responses ──────────────────────────────────────────

#[test]
fn the_headers_of_a_response() {
    let server = Server::new(
        r#"http.Response { status: 201, body: "made", headers: #{
        "X-Made": "yes", "Content-Length": "999", "Connection": "keep-alive", "X-Two\nLines": "no",
        "X-Split": "a\nSet-Cookie: stolen",
      } }"#,
    );
    let response = server.ask(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
    assert_eq!(response.status, 201);
    assert_eq!(response.body, "made");
    // The length is the body's, whatever the handler said.
    assert_eq!(response.header("Content-Length"), Some("4"));
    assert_eq!(response.header("X-Made"), Some("yes"));
    assert_eq!(
        response.header("Content-Type"),
        Some("text/plain; charset=UTF-8")
    );
    // A date as HTTP writes it: `Mon, 05 Oct 2026 12:00:00 GMT`.
    let date = response.header("Date").expect("a date");
    assert!(date.len() == 29 && date.ends_with(" GMT"), "{date:?}");
    assert_eq!(response.header("Server"), None);
    assert_eq!(response.header("Connection"), None);
    // A header that would have been two lines is none.
    assert!(
        !response.headers.iter().any(|line| {
            let line = line.to_ascii_lowercase();
            line.contains("cookie") || line.contains("x-split") || line.contains("lines")
        }),
        "{:?}",
        response.headers
    );
}

/// A connection is kept: requests on it are answered one after the
/// other, and those sent without waiting for an answer in their order.
#[test]
fn requests_on_one_connection_are_answered_in_order() {
    let server = Server::new(ECHO);
    let mut client = server.connect();
    client.send(b"GET /1 HTTP/1.1\r\nHost: x\r\n\r\n");
    assert_eq!(client.response().body, "GET /1 []");
    client.send(b"POST /2 HTTP/1.1\r\nHost: x\r\nContent-Length: 3\r\n\r\nabc");
    assert_eq!(client.response().body, "POST /2 [abc]");
    // Five at once, in one write, a body among them.
    client.send(
        b"GET /3 HTTP/1.1\r\nHost: x\r\n\r\nPUT /4 HTTP/1.1\r\nHost: x\r\nContent-Length: 2\r\n\r\nhiGET /5 HTTP/1.1\r\nHost: x\r\n\r\n\
          DELETE /6 HTTP/1.1\r\nHost: x\r\n\r\nGET /7 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\nGET /never HTTP/1.1\r\nHost: x\r\n\r\n",
    );
    let bodies: Vec<String> = (0..4).map(|_| client.response().body).collect();
    assert_eq!(
        bodies,
        ["GET /3 []", "PUT /4 [hi]", "GET /5 []", "DELETE /6 []"]
    );
    // The one that asks for the end of the connection is the last.
    let last = client.response();
    assert_eq!(last.body, "GET /7 []");
    assert_eq!(last.header("Connection"), Some("close"));
    assert_eq!(client.rest(), b"");
}

#[test]
fn a_body_in_chunks_reaches_the_handler_whole() {
    let server = Server::new(ECHO);
    let mut client = server.connect();
    client.send(
        b"POST /chunks HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n\
          5\r\nhello\r\n1;ext=1\r\n \r\n6\r\nworld!\r\n0\r\nTrailer: dropped\r\n\r\n",
    );
    assert_eq!(client.response().body, "POST /chunks [hello world!]");
    // The connection goes on after the trailers.
    client.send(b"GET /next HTTP/1.1\r\nHost: x\r\n\r\n");
    assert_eq!(client.response().body, "GET /next []");
}

/// A client that asks before it sends its body is answered before the
/// body is read: it has not sent a byte of it when `100 Continue`
/// comes.
#[test]
fn a_client_that_waits_for_100_continue_is_told_to_go_on() {
    let server = Server::new(ECHO);
    let mut client = server.connect();
    client.send(
        b"POST /wait HTTP/1.1\r\nHost: x\r\nExpect: 100-continue\r\nContent-Length: 4\r\n\r\n",
    );
    let mut line = String::new();
    client.0.read_line(&mut line).expect("the interim response");
    assert_eq!(line, "HTTP/1.1 100 Continue\r\n");
    client.0.read_line(&mut line).expect("its end");
    client.send(b"body");
    let response = client.response();
    assert_eq!(
        (response.status, response.body.as_str()),
        (200, "POST /wait [body]")
    );
    // An expectation the server cannot meet.
    let refused =
        server.ask(b"POST / HTTP/1.1\r\nHost: x\r\nExpect: a-miracle\r\nContent-Length: 4\r\n\r\n");
    assert_eq!(refused.status, 417);
}

#[test]
fn http_1_0_is_closed_unless_it_asks_to_be_kept() {
    let server = Server::new(ECHO);
    let mut client = server.connect();
    client.send(b"GET /old HTTP/1.0\r\n\r\n");
    let response = client.response();
    assert_eq!(response.body, "GET /old []");
    assert_eq!(response.header("Connection"), Some("close"));
    assert_eq!(client.rest(), b"");

    let mut client = server.connect();
    client.send(b"GET /kept HTTP/1.0\r\nConnection: keep-alive\r\n\r\n");
    let response = client.response();
    assert_eq!(response.header("Connection"), Some("keep-alive"));
    client.send(b"GET /last HTTP/1.0\r\n\r\n");
    assert_eq!(client.response().body, "GET /last []");
    assert_eq!(client.rest(), b"");
}

#[test]
fn head_gets_the_head_and_no_body() {
    let server = Server::new(ECHO);
    let mut client = server.connect();
    client.send(b"HEAD /h HTTP/1.1\r\nHost: x\r\n\r\nGET /after HTTP/1.1\r\nHost: x\r\n\r\n");
    let (status, headers) = client.head();
    assert_eq!(status, 200);
    // The length of the body that a GET would have got.
    assert!(
        headers.iter().any(|line| line == "Content-Length: 10"),
        "{headers:?}"
    );
    // What follows the head is the next response, not a body.
    assert_eq!(client.response().body, "GET /after []");
}

#[test]
fn a_method_silt_has_no_variant_for_is_405() {
    let server = Server::new(ECHO);
    let mut client = server.connect();
    client.send(b"TRACE /t HTTP/1.1\r\nHost: x\r\nContent-Length: 2\r\n\r\nxx");
    let response = client.response();
    assert_eq!(
        (response.status, response.body.as_str()),
        (405, "Method Not Allowed")
    );
    assert_eq!(
        response.header("Allow"),
        Some("GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS")
    );
    // Its body was read: the connection goes on.
    client.send(b"OPTIONS /o HTTP/1.1\r\nHost: x\r\n\r\n");
    assert_eq!(client.response().body, "OPTIONS /o []");
}

/// A handler that fails answers 500 with nothing of the failure in it,
/// the failure is in the server's log, and the connection is as usable
/// as after any response.
#[test]
fn a_handler_that_fails_is_500_and_the_connection_goes_on() {
    let server = Server::new(ECHO);
    let mut client = server.connect();
    client.send(b"GET /fail HTTP/1.1\r\nHost: x\r\n\r\n");
    let failed = client.response();
    assert_eq!(
        (failed.status, failed.body.as_str()),
        (500, "Internal Server Error")
    );
    assert_eq!(failed.header("Connection"), None);
    client.send(b"GET /fail HTTP/1.1\r\nHost: x\r\n\r\nGET /fine HTTP/1.1\r\nHost: x\r\n\r\n");
    assert_eq!(client.response().status, 500);
    assert_eq!(client.response().body, "GET /fine []");
    // One that asked for the end of the connection gets it.
    client.send(b"GET /fail HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    let last = client.response();
    assert_eq!(last.status, 500);
    assert_eq!(last.header("Connection"), Some("close"));
    assert_eq!(client.rest(), b"");
    let log = server.err.contents();
    assert_eq!(log.matches("secret-detail").count(), 3, "{log}");
    drop(server);
}

/// What the handler returns must be a response a client can take: a
/// status that announces a response (1xx) or is no status is 500.
#[test]
fn a_status_that_is_none_is_500() {
    let server = Server::new(
        r#"match req.path {
        "/100" -> http.Response { status: 100, body: "", headers: #{} }
        "/0" -> http.Response { status: 0, body: "", headers: #{} }
        "/1000" -> http.Response { status: 1000, body: "", headers: #{} }
        "/204" -> http.Response { status: 204, body: "dropped", headers: #{} }
        _ -> http.Response { status: 599, body: "odd", headers: #{} }
      }"#,
    );
    let mut client = server.connect();
    for path in ["/100", "/0", "/1000"] {
        client.send(format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes());
        assert_eq!(client.response().status, 500, "{path}");
    }
    client.send(b"GET /599 HTTP/1.1\r\nHost: x\r\n\r\n");
    assert_eq!(client.response().status, 599);
    // 204 has no body and no length.
    client.send(b"GET /204 HTTP/1.1\r\nHost: x\r\n\r\nGET /599 HTTP/1.1\r\nHost: x\r\n\r\n");
    let (status, headers) = client.head();
    assert_eq!(status, 204);
    assert!(
        !headers.iter().any(|line| line.starts_with("Content-")),
        "{headers:?}"
    );
    assert_eq!(client.response().body, "odd");
    assert_eq!(server.err.contents().matches("out of range").count(), 3);
}

/// A connection whose requests are all there already does not keep
/// its worker: its task gives way after a few requests, whoever
/// answers them.
///
/// Shown by counting, not by a clock. 64 connections each have 200
/// requests waiting, and a task that is queued behind all of them
/// says when it runs. With turns of [`TURN`] requests it runs after
/// some 500 requests; if a connection kept its worker until the VM
/// took it away (after about 160 requests), it would run after
/// 10,000.
#[test]
fn a_connection_gives_way_after_a_few_requests() {
    /// The turn of a connection (`Conn::TURN`).
    const TURN: usize = 8;
    const CONNECTIONS: usize = 64;
    const REQUESTS: usize = 200;
    let source = format!(
        r#"
import channel
import http
import list
import task
import tcp

fn main() {{
  when let Ok(listener) = tcp.listen("127.0.0.1:0") else {{ panic("cannot listen") }}
  when let Ok(control) = tcp.listen("127.0.0.1:0") else {{ panic("cannot listen") }}
  let gate = channel.new(0)
  let at_the_gate = channel.new({CONNECTIONS})
  let log = channel.new(100000)
  let server = task.spawn {{ ->
    http.serve(listener) {{ req ->
      match req.path {{
        "/gate" -> {{
          channel.send(at_the_gate, ())
          let _ = channel.receive(gate)
        }}
        _ -> channel.send(log, "served")
      }}
      http.Response {{ status: 200, body: "ok", headers: #{{}} }}
    }}
  }}
  println("{{tcp.local_port(listener)}} {{tcp.local_port(control)}}")
  -- Every connection waits at the gate, its other requests behind it.
  1..{CONNECTIONS} |> list.each {{ _ ->
    let _ = channel.receive(at_the_gate)
  }}
  channel.close(gate)
  let _ = task.spawn {{ -> channel.send(log, "the other task") }}
  let before = loop served = 0 {{
    match channel.receive(log) {{
      channel.Message("served") -> loop(served + 1)
      _ -> served
    }}
  }}
  println("{{before}}")
  let _ = tcp.accept(control)
  task.cancel(server)
}}
"#
    );
    let server = Server::program(&source, None);
    let mut requests = b"GET /gate HTTP/1.1\r\nHost: x\r\n\r\n".to_vec();
    for _ in 0..REQUESTS {
        requests.extend_from_slice(b"GET /next HTTP/1.1\r\nHost: x\r\n\r\n");
    }
    let clients: Vec<Client> = (0..CONNECTIONS)
        .map(|_| {
            let mut client = server.connect();
            client.send(&requests);
            client
        })
        .collect();
    let limit = Instant::now() + PATIENCE;
    let served_before = loop {
        let printed = server.out.contents();
        let mut lines = printed.lines().skip(1);
        if let (Some(count), true) = (lines.next(), printed.ends_with('\n')) {
            break count.parse::<usize>().expect("a count");
        }
        assert!(Instant::now() < limit, "the other task never ran");
        thread::sleep(Duration::from_millis(2));
    };
    drop(clients);
    eprintln!("{served_before} requests were served before the other task ran");
    // A turn each for the connections ahead of it, and room for the
    // turns that other workers take meanwhile.
    assert!(
        served_before <= CONNECTIONS * TURN * 5,
        "{served_before} requests were served before another task ran"
    );
}

// ── Limits ──────────────────────────────────────────────────────────

/// The limits on a request, each from both sides: what is at the limit
/// is served, what is beyond it is refused with its status and the
/// connection closed.
#[test]
fn a_request_beyond_a_limit_is_refused() {
    let server = Server::new(
        r#"http.Response { status: 200, body: "{string.length(req.body)}", headers: #{} }"#,
    );
    let refused = |request: &[u8], status: u16| {
        let mut client = server.connect();
        client.send(request);
        let response = client.response();
        assert_eq!(response.status, status);
        assert_eq!(response.header("Connection"), Some("close"));
        assert_eq!(client.rest(), b"");
    };

    // The head.
    let head = |value: usize| {
        let mut request = b"GET / HTTP/1.1\r\nHost: x\r\nX: ".to_vec();
        request.extend(std::iter::repeat_n(b'a', value));
        request.extend_from_slice(b"\r\n\r\n");
        request
    };
    assert_eq!(server.ask(&head(HEAD_MAX - 100)).status, 200);
    refused(&head(HEAD_MAX), 431);
    // One that never ends is refused when it is too long, not read on.
    refused(&vec![b'A'; HEAD_MAX + 1][..], 400);
    let mut endless = b"GET / HTTP/1.1\r\nHost: x\r\n".to_vec();
    endless.extend(std::iter::repeat_n(b'a', HEAD_MAX));
    refused(&endless, 431);

    // The headers.
    let headers = |n: usize| {
        let mut request = b"GET / HTTP/1.1\r\nHost: x\r\n".to_vec();
        for i in 1..n {
            request.extend_from_slice(format!("H{i}: v\r\n").as_bytes());
        }
        request.extend_from_slice(b"\r\n");
        request
    };
    assert_eq!(server.ask(&headers(HEADERS_MAX)).status, 200);
    refused(&headers(HEADERS_MAX + 1), 431);

    // The body, by its declared length: refused before a byte of it
    // is sent.
    let declared = format!(
        "POST / HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n",
        BODY_MAX + 1
    );
    refused(declared.as_bytes(), 413);
    let mut exact =
        format!("POST / HTTP/1.1\r\nHost: x\r\nContent-Length: {BODY_MAX}\r\n\r\n").into_bytes();
    exact.extend(std::iter::repeat_n(b'b', BODY_MAX));
    assert_eq!(server.ask(&exact).body, BODY_MAX.to_string());

    // The body in chunks: refused when the chunk that goes beyond is
    // announced.
    let half = BODY_MAX / 2;
    let mut chunks = b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
    for _ in 0..2 {
        chunks.extend_from_slice(format!("{half:x}\r\n").as_bytes());
        chunks.extend(std::iter::repeat_n(b'c', half));
        chunks.extend_from_slice(b"\r\n");
    }
    let mut at_the_limit = chunks.clone();
    at_the_limit.extend_from_slice(b"0\r\n\r\n");
    assert_eq!(server.ask(&at_the_limit).body, BODY_MAX.to_string());
    chunks.extend_from_slice(b"1\r\n");
    refused(&chunks, 413);
}

/// The bodies that the server holds for requests that no handler has
/// yet have a bound together: a body that would go beyond it is
/// refused before the client is told to send it, and there is room
/// again when a body has been handed to its handler.
#[test]
fn the_bodies_that_wait_for_a_handler_are_bounded_together() {
    let server = Server::new(
        r#"http.Response { status: 200, body: "{string.length(req.body)}", headers: #{} }"#,
    );
    let largest = format!(
        "POST / HTTP/1.1\r\nHost: x\r\nExpect: 100-continue\r\nContent-Length: {BODY_MAX}\r\n\r\n"
    );
    // A client that is told to send its body has room for it.
    let told_to_go_on = |client: &mut Client| {
        let mut line = String::new();
        client.0.read_line(&mut line).expect("the interim response");
        assert_eq!(line, "HTTP/1.1 100 Continue\r\n");
        client.0.read_line(&mut line).expect("its end");
    };
    // As many bodies of the largest size as the bound holds, none of
    // them sent.
    let mut waiting: Vec<Client> = (0..BODIES_MAX / BODY_MAX)
        .map(|_| {
            let mut client = server.connect();
            client.send(largest.as_bytes());
            told_to_go_on(&mut client);
            client
        })
        .collect();
    // One more has no room, and is not told to send.
    let mut refused = server.connect();
    refused.send(largest.as_bytes());
    let response = refused.response();
    assert_eq!(response.status, 503);
    assert_eq!(response.header("Connection"), Some("close"));
    assert_eq!(refused.rest(), b"");
    // What is left of the room still serves a small one.
    let small = server.ask(b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\n\r\nhello");
    assert_eq!((small.status, small.body.as_str()), (200, "5"));
    // A body that has reached its handler makes room for another.
    let mut first = waiting.pop().expect("a waiting client");
    first.send(&vec![b'b'; BODY_MAX]);
    assert_eq!(first.response().body, BODY_MAX.to_string());
    let mut next = server.connect();
    next.send(largest.as_bytes());
    told_to_go_on(&mut next);
    // And so does a client that leaves without sending its body, once
    // the server has seen it leave.
    drop(waiting.pop());
    let limit = Instant::now() + PATIENCE;
    loop {
        let mut another = server.connect();
        another.send(largest.as_bytes());
        let mut line = String::new();
        another.0.read_line(&mut line).expect("an answer");
        if line == "HTTP/1.1 100 Continue\r\n" {
            break;
        }
        assert!(line.starts_with("HTTP/1.1 503 "), "{line:?}");
        assert!(
            Instant::now() < limit,
            "a client that left still has its room"
        );
        thread::sleep(Duration::from_millis(2));
    }
}

/// A request whose length two readers could take differently is
/// refused, the connection is closed, and what follows it on the
/// connection is not served as a request.
#[test]
fn a_request_of_uncertain_length_is_refused_and_nothing_after_it_is_served() {
    let server = Server::new(ECHO);
    let after: &[u8] = b"GET /smuggled HTTP/1.1\r\nHost: x\r\n\r\n";
    for request in [
        &b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 4\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n"[..],
        b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\nContent-Length: 4\r\n\r\n0\r\n\r\n",
        b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\nContent-Length: 26\r\n\r\n",
        b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 26\r\nContent-Length: 0\r\n\r\n",
        b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: -1\r\n\r\n",
        b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: +0\r\n\r\n",
        b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: zero\r\n\r\n",
        b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 0, 26\r\n\r\n",
        b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\nffffffffffffffffff\r\n",
        b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n0x0\r\n\r\n",
        b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n0\n\r\n",
        b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: xchunked\r\n\r\n0\r\n\r\n",
        b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked, identity\r\n\r\n0\r\n\r\n",
        b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n0;a=\x00\r\n\r\n",
        b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n0\r\nT: \x01\r\n\r\n",
        // Which host it is for is not certain either: none, or two.
        b"GET / HTTP/1.1\r\n\r\n",
        b"GET / HTTP/1.1\r\nHost: a\r\nHost: b\r\n\r\n",
        b"GET / HTTP/1.0\r\nHost: a\r\nhost: b\r\n\r\n",
        b"GET / HTTP/1.1\nHost: x\n\n",
        b"GET / HTTP/1.1\r\nHost: x\n\r\n",
        b"GET / HTTP/1.1\r\nHost: x\r\nX: a\rb\r\n\r\n",
        b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length : 0\r\n\r\n",
        b"POST / HTTP/1.1\r\nHost: x\r\nX: a\r\n Content-Length: 26\r\n\r\n",
    ] {
        let what = String::from_utf8_lossy(request).into_owned();
        let mut client = server.connect();
        client.send(&[request, after].concat());
        let response = client.response();
        assert_eq!(response.status, 400, "{what:?}");
        assert_eq!(response.header("Connection"), Some("close"), "{what:?}");
        assert_eq!(client.rest(), b"", "{what:?}");
    }
    // The server still serves.
    assert_eq!(
        server.ask(b"GET /fine HTTP/1.1\r\nHost: x\r\n\r\n").body,
        "GET /fine []"
    );
}

/// Bytes that are no request, and requests that stop anywhere before
/// their end: no handler is called for them, nothing fails, and the
/// server goes on serving.
#[test]
fn what_is_no_request_is_not_served() {
    let server = Server::new(
        r#"match req.path {
        "/fine" -> http.Response { status: 200, body: "fine", headers: #{} }
        _ -> panic("called for {req.path}")
      }"#,
    );
    // Cut short at every byte, and the connection closed.
    let whole: &[u8] =
        b"POST /cut HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n";
    for cut in 0..whole.len() {
        let mut client = server.connect();
        client.send(&whole[..cut]);
        client
            .0
            .get_ref()
            .shutdown(std::net::Shutdown::Write)
            .expect("shutdown");
        assert_eq!(client.rest(), b"", "cut at {cut}");
    }
    // Garbage: refused, or closed when it ends.
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut byte = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 24) as u8
    };
    for round in 0..200 {
        let garbage: Vec<u8> = (0..(round * 37) % 3000).map(|_| byte()).collect();
        let mut client = server.connect();
        // The server may close before it has taken all of it.
        let _ = client.0.get_mut().write_all(&garbage);
        let _ = client.0.get_ref().shutdown(std::net::Shutdown::Write);
        let answer = client.rest();
        assert!(
            answer.is_empty() || answer.starts_with(b"HTTP/1.1 4"),
            "{:?}",
            String::from_utf8_lossy(&answer)
        );
    }
    // What begins no request is refused without waiting for its end: a
    // TLS handshake sent to this port.
    let mut client = server.connect();
    client.send(b"\x16\x03\x01\x02\x00\x01\x00\x01\xfc\x03\x03");
    assert_eq!(client.response().status, 400);
    assert_eq!(client.rest(), b"");

    assert_eq!(
        server.ask(b"GET /fine HTTP/1.1\r\nHost: x\r\n\r\n").body,
        "fine"
    );
    assert_eq!(server.err.contents(), "");
}

// ── Time limits, on a clock the test moves ──────────────────────────

/// Move the clock on, a tenth of `limit` at a time, until `done`;
/// panics if it never is. (The moment at which the server starts a
/// wait is not the test's to know, so no single step is expected to
/// be the one.)
fn advance_until(clock: &TestClock, limit: Duration, what: &str, mut done: impl FnMut() -> bool) {
    let patience = Instant::now() + PATIENCE;
    while !done() {
        assert!(Instant::now() < patience, "{what}");
        clock.advance(limit / 10);
        thread::sleep(Duration::from_millis(2));
    }
}

/// A connection that is kept and sends nothing is closed when the time
/// for a request's head has passed, and not before.
#[test]
fn an_idle_connection_is_closed_after_the_time_for_a_head() {
    let clock = TestClock::default();
    let server = Server::on(ECHO, Some(clock.clone()));
    let mut client = server.connect();
    client.send(b"GET /1 HTTP/1.1\r\nHost: x\r\n\r\n");
    assert_eq!(client.response().body, "GET /1 []");
    // Not before: whenever the server began to wait, less than the
    // time has passed, and the connection still serves.
    clock.advance(REQUEST_TIME - Duration::from_secs(1));
    client.send(b"GET /2 HTTP/1.1\r\nHost: x\r\n\r\n");
    assert_eq!(client.response().body, "GET /2 []");
    advance_until(
        &clock,
        REQUEST_TIME,
        "an idle connection is never closed",
        || client.is_closed(),
    );
    // A connection that never sent anything likewise, and one that
    // sent an empty line and no request.
    let mut silent = server.connect();
    let mut empty = server.connect();
    empty.send(b"\r\n");
    advance_until(
        &clock,
        REQUEST_TIME,
        "a silent connection is never closed",
        || silent.is_closed() && empty.is_closed(),
    );
}

/// The time for a head is from when the server began to wait for it:
/// a head that trickles in does not get more by trickling. The client
/// is told (408), and the connection closed.
#[test]
fn a_head_that_trickles_in_does_not_get_more_time() {
    let clock = TestClock::default();
    let server = Server::on(ECHO, Some(clock.clone()));
    let mut client = server.connect();
    client.send(b"GET /slow HTTP/1.1\r\nHost: x\r\n");
    let mut sent = 0;
    let patience = Instant::now() + PATIENCE;
    while !client.has_word() {
        assert!(
            Instant::now() < patience,
            "a trickling head is waited for without end"
        );
        // Never as long as the limit without a byte.
        clock.advance(REQUEST_TIME * 2 / 3);
        // The answer to that step, if it is one, before more is sent.
        for _ in 0..20 {
            if client.has_word() {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        if !client.has_word() {
            client.send(format!("X-{sent}: v\r\n").as_bytes());
            sent += 1;
        }
    }
    assert_eq!(client.response().status, 408);
    assert_eq!(client.rest(), b"");
}

/// The body has its own time, longer than the head's and counted from
/// the head: a body that does not come in it is not waited for. The
/// client is told (408), and the connection closed.
#[test]
fn a_body_that_does_not_come_is_not_waited_for() {
    let clock = TestClock::default();
    let server = Server::on(ECHO, Some(clock.clone()));
    let mut client = server.connect();
    // The head has been read when the server says that the body may
    // come.
    let head_is_read = |client: &mut Client| {
        client.send(
            b"POST /body HTTP/1.1\r\nHost: x\r\nExpect: 100-continue\r\nContent-Length: 4\r\n\r\n",
        );
        let mut line = String::new();
        client.0.read_line(&mut line).expect("the interim response");
        assert_eq!(line, "HTTP/1.1 100 Continue\r\n");
        client.0.read_line(&mut line).expect("its end");
    };
    head_is_read(&mut client);
    // Longer than a head may take, and not as long as a body may: the
    // body is still taken.
    clock.advance(TRANSFER_TIME - Duration::from_secs(1));
    client.send(b"body");
    assert_eq!(client.response().body, "POST /body [body]");
    // Half a body, and no more.
    head_is_read(&mut client);
    client.send(b"bo");
    advance_until(
        &clock,
        TRANSFER_TIME,
        "a body that stops is waited for without end",
        || client.has_word(),
    );
    assert_eq!(client.response().status, 408);
    assert_eq!(client.rest(), b"");
}

/// A response that the client does not take is given up after the
/// time for it: the client never gets the whole of it.
#[test]
fn a_response_that_is_not_taken_is_given_up() {
    // More than the system takes in for a client that reads nothing.
    const LENGTH: usize = 64_000_000;
    let clock = TestClock::default();
    let server = Server::on(
        r#"{
        let part = string.repeat("x", 8000000)
        http.Response { status: 200, body: "{part}{part}{part}{part}{part}{part}{part}{part}", headers: #{} }
      }"#,
        Some(clock.clone()),
    );
    let mut client = server.connect();
    client.send(b"GET /big HTTP/1.1\r\nHost: x\r\n\r\n");
    let (status, headers) = client.head();
    assert_eq!(status, 200);
    assert!(headers.contains(&format!("Content-Length: {LENGTH}")));
    // Nearly as long as a response may take, the server still sends:
    // more comes than the system held when the client began to read.
    clock.advance(TRANSFER_TIME - Duration::from_secs(1));
    let mut part = vec![0u8; 1_000_000];
    for _ in 0..40 {
        client.0.read_exact(&mut part).expect("more of the body");
    }
    // The client takes no more. When the time has passed, the server
    // closes the connection: what the client then sends is refused by
    // the system.
    advance_until(
        &clock,
        TRANSFER_TIME,
        "a response nobody takes is sent for ever",
        || client.0.get_mut().write_all(b"x").is_err(),
    );
    let taken = 40 * part.len() + client.rest().len();
    assert!(taken < LENGTH, "the whole body came: {taken} bytes");
}

/// After a refusal the server reads what the client still sends, so
/// that the refusal reaches a client in the middle of sending, and
/// closes the connection when the client stops or the time for that
/// has passed.
#[test]
fn after_a_refusal_the_client_is_heard_out_for_a_time() {
    let clock = TestClock::default();
    let server = Server::on(ECHO, Some(clock.clone()));
    let mut client = server.connect();
    // Refused on its head; a body follows, more of it than the system
    // would hold for a server that reads nothing: the server takes it
    // although it has answered.
    client.send(b"POST /big HTTP/1.1\r\nHost: x\r\nContent-Length: 99999999999\r\n\r\n");
    let body = vec![b'x'; 64 * 1024];
    for _ in 0..512 {
        client.send(&body);
    }
    let response = client.response();
    assert_eq!(response.status, 413);
    // The server has said that nothing more comes from it.
    assert_eq!(client.rest(), b"");
    // Nearly as long as it hears a refused client out, the server
    // still takes what comes: more than the system would hold for a
    // server that reads nothing.
    clock.advance(REFUSAL_TIME - Duration::from_secs(1));
    for _ in 0..256 {
        client.send(&body);
    }
    // And when the time is over, it hears no more: what the client
    // sends then is refused by the system.
    advance_until(
        &clock,
        REFUSAL_TIME,
        "a refused client is heard for ever",
        || client.0.get_mut().write_all(&body).is_err(),
    );
    // One that stops sending is done with at once.
    let mut client = server.connect();
    client.send(b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: -1\r\n\r\n");
    assert_eq!(client.response().status, 400);
    assert_eq!(client.rest(), b"");
}

/// The server says its last word on a connection (408, or 503 when it
/// ends) while a thread of the pool is still reading that connection.
/// The write does not wait, and it does not disturb the read: after a
/// 408 the client is heard out as after any refusal, however the two
/// fall together.
///
/// Many connections at once whose bodies come in sixteen bytes at a
/// time, so that their readers are in and out of the read all the
/// while; the clock passes the time for a body once, and every
/// connection that was answered must still take what its client
/// sends. Then the server is cancelled under the same load.
#[test]
fn a_last_word_beside_a_read_does_not_end_the_read() {
    use std::sync::atomic::AtomicBool;
    const CONNECTIONS: usize = 32;
    let clock = TestClock::default();
    let server = Server::on(ECHO, Some(clock.clone()));
    // (Bodies that all of the connections have room for at a time,
    // and longer than what is sent of them.)
    let head = format!(
        "POST /slow HTTP/1.1\r\nHost: x\r\nExpect: 100-continue\r\nContent-Length: {}\r\n\r\n",
        2 * 1024 * 1024
    );
    // Connections whose heads are read, each with a thread that
    // sends its body in small pieces until told to stop.
    let trickling = |stop: &Arc<AtomicBool>| -> Vec<(Client, thread::JoinHandle<bool>)> {
        (0..CONNECTIONS)
            .map(|_| {
                let mut client = server.connect();
                client.send(head.as_bytes());
                let mut line = String::new();
                client.0.read_line(&mut line).expect("the interim response");
                assert_eq!(line, "HTTP/1.1 100 Continue\r\n");
                client.0.read_line(&mut line).expect("its end");
                let mut conn = client.0.get_ref().try_clone().expect("clone");
                let stop = stop.clone();
                let writer = thread::spawn(move || {
                    let mut sent = 0;
                    while !stop.load(Ordering::SeqCst) && sent < 256 * 1024 {
                        if conn.write_all(&[b'b'; 16]).is_err() {
                            return false;
                        }
                        sent += 16;
                    }
                    true
                });
                (client, writer)
            })
            .collect()
    };

    let mut answered = 0;
    for _ in 0..6 {
        let stop = Arc::new(AtomicBool::new(false));
        let mut clients = trickling(&stop);
        // The time for a body passes, once: the clock then stands, so
        // a connection that was answered is heard out without end.
        clock.advance(TRANSFER_TIME + Duration::from_secs(1));
        let patience = Instant::now() + Duration::from_secs(2);
        while Instant::now() < patience && !clients.iter_mut().all(|(client, _)| client.has_word())
        {
            thread::sleep(Duration::from_millis(2));
        }
        stop.store(true, Ordering::SeqCst);
        for (mut client, writer) in clients {
            let wrote = writer.join().expect("the writer");
            // (One whose wait began after the clock moved has all the
            // time yet: it is left alone.)
            if !client.has_word() {
                continue;
            }
            assert!(
                wrote,
                "the server stopped reading a body it had answered 408"
            );
            client.send(&vec![b'b'; 1024 * 1024]);
            assert_eq!(client.response().status, 408);
            assert_eq!(client.rest(), b"");
            answered += 1;
        }
    }
    eprintln!("{answered} connections were answered 408 beside their reads");
    assert!(answered > 0, "no connection reached its time limit");

    // The server ends with bodies half read and still coming: every
    // client is answered 503, or finds its connection closed.
    let stop = Arc::new(AtomicBool::new(false));
    let clients = trickling(&stop);
    drop(server);
    for (mut client, writer) in clients {
        let _ = writer.join();
        let last = client.rest();
        assert!(
            last.is_empty() || last.starts_with(b"HTTP/1.1 503 "),
            "{:?}",
            String::from_utf8_lossy(&last)
        );
    }
}
