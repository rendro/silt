//! Regression tests for HTTP builtin security hardening.
//!
//! Locks four fixes in `src/builtins/data.rs`:
//!
//! - HIGH-1: `http.serve` caps request bodies at 10 MiB and returns 413
//!   Payload Too Large for anything larger (or for a Content-Length that
//!   advertises more than the cap).
//! - HIGH-2: `http.serve` uses `recv_timeout` so the accept loop unblocks
//!   periodically, and bounds concurrent handler threads so a slowloris-style
//!   burst cannot force unbounded thread spawning. (Library limitation:
//!   tiny_http 0.12 does not expose per-connection socket timeouts, so a
//!   partial-headers client will be held open by tiny_http's internal pool
//!   — the test here instead exercises that a *legitimate* request still
//!   gets a response in bounded time, and that a follow-up request on a
//!   fresh connection after the partial-headers attacker still works.)
//! - HIGH-3: `http.get` / `http.request` configure `timeout_connect` and
//!   `timeout_global` on the ureq Agent so a bogus / black-holed peer
//!   does not hang forever.
//! - MED-1: When a handler returns a VmError, the 500 response body is a
//!   generic "Internal Server Error" — the VmError details (message, line
//!   number, call stack) are logged to stderr but never leaked to the
//!   client.
//!
//! All tests bind to `127.0.0.1` and use an OS-assigned port.

#![cfg(feature = "http")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

fn silt_bin() -> PathBuf {
    if let Ok(p) = std::env::var("CARGO_BIN_EXE_silt") {
        return PathBuf::from(p);
    }
    let mut p = std::env::current_exe().unwrap();
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    p.push("silt");
    p
}

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn tmp_silt_file(stem: &str, src: &str) -> PathBuf {
    let n = TMP_COUNTER.fetch_add(1, Ordering::SeqCst);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let tmp = std::env::temp_dir().join(format!(
        "silt_http_hardening_{}_{}_{}_{}.silt",
        stem,
        std::process::id(),
        ts,
        n
    ));
    std::fs::write(&tmp, src).unwrap();
    tmp
}

/// Grab an ephemeral port from the OS, drop the listener, hand the number
/// to the silt subprocess.
fn pick_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = l.local_addr().unwrap().port();
    drop(l);
    port
}

/// Poll-connect until the silt subprocess has bound the port, or time out.
fn wait_for_bind(port: u16, max_wait: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < max_wait {
        if let Ok(s) = TcpStream::connect_timeout(
            &format!("127.0.0.1:{port}").parse().unwrap(),
            Duration::from_millis(200),
        ) {
            drop(s);
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Connect to the silt subprocess with a retry loop. `wait_for_bind`
/// confirms the listener is up via a probe-and-drop cycle, but the
/// follow-up `TcpStream::connect` from a test can still race the silt
/// subprocess's accept loop on slow CI runners (Linux occasionally
/// returned `ECONNREFUSED` between the bind probe drop and the real
/// connect — see ci flake against med1_handler_error_does_not_leak_vm_error_details).
/// Retry up to ~2s before giving up, treating `ConnectionRefused` as
/// transient. Any other error is surfaced immediately so we don't
/// mask real bugs.
fn connect_with_retry(port: u16) -> TcpStream {
    let addr = format!("127.0.0.1:{port}");
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut backoff = Duration::from_millis(20);
    loop {
        match TcpStream::connect(&addr) {
            Ok(s) => return s,
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                if Instant::now() >= deadline {
                    panic!("connect to {addr}: exhausted retries; last error: {e}");
                }
                std::thread::sleep(backoff);
                backoff = std::cmp::min(backoff * 2, Duration::from_millis(200));
            }
            Err(e) => panic!("connect to {addr}: {e}"),
        }
    }
}

/// Spawn `silt run <tmp>` with stdout/stderr piped.
fn spawn_silt(tmp: &PathBuf) -> Child {
    Command::new(silt_bin())
        .arg("run")
        .arg(tmp)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn silt")
}

/// Kill + wait; collect stdout/stderr. Used for all test teardown.
fn shutdown(mut child: Child) -> (String, String) {
    let _ = child.kill();
    let out = child.wait_with_output().ok();
    let (stdout, stderr) = out
        .map(|o| {
            (
                String::from_utf8_lossy(&o.stdout).to_string(),
                String::from_utf8_lossy(&o.stderr).to_string(),
            )
        })
        .unwrap_or_default();
    (stdout, stderr)
}

/// Minimal echo-ish silt server: always returns 200 OK with a short body,
/// regardless of input. Used by HIGH-1 and HIGH-2 tests.
fn echo_server_src(port: u16) -> String {
    format!(
        r#"
import http
fn main() {{
  http.serve({port}) {{ _req ->
    Response {{ status: 200, body: "ok", headers: #{{}} }}
  }}
}}
"#
    )
}

// ────────────────────────────────────────────────────────────────────────
// HIGH-1: body cap — server rejects oversized uploads with 413.
// ────────────────────────────────────────────────────────────────────────

#[test]
fn high1_http_serve_rejects_oversized_body_with_413() {
    let port = pick_port();
    let tmp = tmp_silt_file("high1_body_cap", &echo_server_src(port));
    let child = spawn_silt(&tmp);

    assert!(
        wait_for_bind(port, Duration::from_secs(10)),
        "silt http.serve failed to bind 127.0.0.1:{port}"
    );

    // 50 MiB body, well above the 10 MiB cap. We send a real (honest)
    // Content-Length so the server knows to reject up front, but we also
    // start writing the body — the server should close the socket or
    // respond 413 quickly, long before we finish streaming 50 MiB.
    let body_len: usize = 50 * 1024 * 1024;
    let mut sock = connect_with_retry(port);
    sock.set_write_timeout(Some(Duration::from_secs(10))).ok();
    sock.set_read_timeout(Some(Duration::from_secs(15))).ok();

    let req_head = format!(
        "POST / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: {body_len}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n"
    );
    sock.write_all(req_head.as_bytes()).expect("write headers");

    // Stream body in chunks; tolerate ErrorKind::BrokenPipe / ConnectionReset
    // — the server is allowed to close on us once it decides to 413.
    let start = Instant::now();
    let chunk = vec![b'A'; 64 * 1024];
    let mut written: usize = 0;
    while written < body_len && start.elapsed() < Duration::from_secs(15) {
        let take = std::cmp::min(chunk.len(), body_len - written);
        match sock.write_all(&chunk[..take]) {
            Ok(()) => written += take,
            Err(_) => break, // server closed
        }
    }
    // Read whatever the server sent back (if anything). A 413 is the
    // expected response; connection reset is also acceptable (some
    // implementations drop instead of responding cleanly when they
    // already know the request is bad).
    let mut resp = Vec::new();
    let _ = sock.read_to_end(&mut resp);
    let resp_str = String::from_utf8_lossy(&resp);
    let elapsed = start.elapsed();

    let (_stdout, _stderr) = shutdown(child);
    let _ = std::fs::remove_file(&tmp);

    // Must not take anywhere near the full 50 MiB upload time.
    assert!(
        elapsed < Duration::from_secs(15),
        "oversized POST took {elapsed:?} — server did not short-circuit"
    );
    // Either we got a 413, OR the server reset the connection (also fine).
    let got_413 = resp_str.starts_with("HTTP/1.1 413")
        || resp_str.starts_with("HTTP/1.0 413")
        || resp_str.contains(" 413 ");
    let got_close = resp.is_empty();
    assert!(
        got_413 || got_close,
        "expected 413 or connection close; got:\n{resp_str}"
    );
}

// ────────────────────────────────────────────────────────────────────────
// HIGH-2: accept loop is bounded + legitimate requests still work even
// when an attacker is sitting on a partial-headers connection.
// ────────────────────────────────────────────────────────────────────────

#[test]
fn high2_http_serve_legitimate_request_works_alongside_slow_attacker() {
    // tiny_http 0.12 does not expose per-connection socket read timeouts,
    // so an attacker who writes `GET / HTTP/1.1\r\n` then stops is held
    // open inside tiny_http's internal pool regardless of what we do on
    // our side. What we *can* verify is:
    //   1. The accept loop keeps running (recv_timeout, not
    //      indefinite blocking).
    //   2. A legitimate follow-up request on a fresh connection still
    //      gets a response in bounded time.
    // This guards against a future regression where the slowloris
    // connection would wedge the *user-visible* accept pipeline.

    let port = pick_port();
    let tmp = tmp_silt_file("high2_slowloris", &echo_server_src(port));
    let child = spawn_silt(&tmp);

    assert!(
        wait_for_bind(port, Duration::from_secs(10)),
        "silt http.serve failed to bind 127.0.0.1:{port}"
    );

    // Attacker: open a connection, send partial headers, then sit there.
    let mut attacker = connect_with_retry(port);
    attacker
        .write_all(b"GET / HTTP/1.1\r\nHost: x\r\n")
        .expect("attacker partial write");
    // DO NOT send the terminating \r\n — just hold the connection.

    // Legitimate client: full request on a new connection, expect 200 fast.
    let start = Instant::now();
    let mut client = connect_with_retry(port);
    client.set_read_timeout(Some(Duration::from_secs(10))).ok();
    client
        .write_all(
            format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .expect("client write");

    let mut buf = Vec::new();
    let _ = client.read_to_end(&mut buf);
    let elapsed = start.elapsed();
    let resp = String::from_utf8_lossy(&buf);

    // Drop attacker connection (clean shutdown of the test).
    drop(attacker);

    let (_stdout, _stderr) = shutdown(child);
    let _ = std::fs::remove_file(&tmp);

    assert!(
        elapsed < Duration::from_secs(10),
        "legitimate request took {elapsed:?} — accept loop may be wedged by slowloris"
    );
    assert!(
        resp.starts_with("HTTP/1.1 200") || resp.contains(" 200 "),
        "expected 200 OK for legitimate request; got:\n{resp}"
    );
}

// ────────────────────────────────────────────────────────────────────────
// HIGH-3: http.get returns Err bounded in time for a black-holed peer.
// ────────────────────────────────────────────────────────────────────────

#[test]
fn high3_http_get_bogus_address_returns_err_in_bounded_time() {
    // Pick an OS-assigned port, then drop the listener. No one is
    // listening on it. Connecting should fail fast (ECONNREFUSED on
    // loopback). The key guard is that the silt subprocess exits within
    // the 10s timeout_connect budget we set — not that it hangs forever.
    let port = pick_port();
    let src = format!(
        r#"
import http
fn main() {{
  match http.get("http://127.0.0.1:{port}/") {{
    Ok(_) -> println("unexpected-ok")
    Err(_) -> println("expected-err")
  }}
}}
"#
    );
    let tmp = tmp_silt_file("high3_bogus_addr", &src);

    let start = Instant::now();
    let output = Command::new(silt_bin())
        .arg("run")
        .arg(&tmp)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn silt");
    let elapsed = start.elapsed();
    let _ = std::fs::remove_file(&tmp);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Must return Err (not hang) in far less than the 60s global cap.
    assert!(
        elapsed < Duration::from_secs(15),
        "http.get took {elapsed:?} against unreachable peer — timeouts not applied\n\
         stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("expected-err"),
        "expected http.get to return Err(_); got\nstdout: {stdout}\nstderr: {stderr}"
    );
}

// ────────────────────────────────────────────────────────────────────────
// MED-1: 500 response body does not leak VmError contents.
// ────────────────────────────────────────────────────────────────────────

#[test]
fn med1_handler_error_does_not_leak_vm_error_details() {
    // Handler calls test.assert(false, "secret-internal-info") inside a
    // named helper so VmError carries both a distinctive message AND a
    // call stack. We assert:
    //   - response body is generic ("Internal Server Error"),
    //   - response body does NOT contain "secret-internal-info",
    //     "call stack", or "at line",
    //   - silt stderr DOES contain the VmError detail (so ops can
    //     debug — the info isn't silently swallowed).
    let port = pick_port();
    let src = format!(
        r#"
import http
import test

fn private_helper() {{
  test.assert(false, "secret-internal-info")
}}

fn main() {{
  http.serve({port}) {{ _req ->
    private_helper()
    Response {{ status: 200, body: "never", headers: #{{}} }}
  }}
}}
"#
    );
    let tmp = tmp_silt_file("med1_info_leak", &src);
    let child = spawn_silt(&tmp);

    assert!(
        wait_for_bind(port, Duration::from_secs(10)),
        "silt http.serve failed to bind 127.0.0.1:{port}"
    );

    // Poke the server; read whatever 500 it returns.
    let mut sock = connect_with_retry(port);
    sock.set_read_timeout(Some(Duration::from_secs(10))).ok();
    sock.write_all(
        format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n").as_bytes(),
    )
    .expect("write");
    let mut resp = Vec::new();
    let _ = sock.read_to_end(&mut resp);
    let resp_str = String::from_utf8_lossy(&resp).to_string();

    // Give the server a moment to flush stderr before we kill it.
    std::thread::sleep(Duration::from_millis(200));

    let (_stdout, stderr) = shutdown(child);
    let _ = std::fs::remove_file(&tmp);

    // Response must be 500 and must NOT leak VmError internals.
    assert!(
        resp_str.starts_with("HTTP/1.1 500") || resp_str.contains(" 500 "),
        "expected 500 status; got:\n{resp_str}"
    );
    assert!(
        resp_str.contains("Internal Server Error"),
        "expected generic 500 body; got:\n{resp_str}"
    );
    for leak in &["secret-internal-info", "call stack", "at line", "VM error"] {
        assert!(
            !resp_str.contains(leak),
            "response leaks {leak:?} to client; full response:\n{resp_str}"
        );
    }

    // Operator-side logging must still contain the details. The Display
    // impl of VmError starts with "VM error:" and includes the message.
    assert!(
        stderr.contains("secret-internal-info"),
        "expected VmError detail on stderr for operator debugging; got stderr:\n{stderr}"
    );
}

// ────────────────────────────────────────────────────────────────────────
// Source-grep lock: every test-side TCP connect against the silt
// subprocess goes through `connect_with_retry`, never raw
// `TcpStream::connect`. The bare connect is racy on Linux CI runners
// (see ci flake against med1_handler_error_does_not_leak_vm_error_details).
// The retry helper masks the brief window between `wait_for_bind` probe
// teardown and the silt accept loop being ready for the real connect.
// ────────────────────────────────────────────────────────────────────────

#[test]
fn http_test_call_sites_use_connect_with_retry_not_bare_tcpstream_connect() {
    let src = std::fs::read_to_string(file!()).expect("read own source");
    // Strip line comments crudely so we only inspect executable Rust.
    let mut code = String::new();
    for line in src.lines() {
        let stripped = match line.find("//") {
            Some(i) => &line[..i],
            None => line,
        };
        code.push_str(stripped);
        code.push('\n');
    }
    // Build the forbidden pattern at runtime so this test's source
    // doesn't itself match the literal it greps for.
    let forbidden = format!("TcpStream{}connect(", "::");
    // Whitelist: `connect_timeout` inside `wait_for_bind` (polled), and
    // the `connect_with_retry` body which calls `TcpStream::connect(&addr)`
    // intentionally. Everything else is a regression.
    for (lineno, line) in code.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.contains("connect_timeout") {
            continue;
        }
        if trimmed.contains("connect(&addr)") {
            continue;
        }
        assert!(
            !trimmed.contains(&forbidden),
            "line {} uses raw {} — must go through connect_with_retry: {}",
            lineno + 1,
            forbidden,
            trimmed,
        );
    }
}
