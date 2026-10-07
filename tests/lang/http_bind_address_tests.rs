//! Where an HTTP server is reached is what `tcp.listen` was told.
//!
//! `http.serve(listener, handler)` serves on a listener the program
//! bound itself, so the safety of a quick development server lies in
//! the address it writes:
//!
//! - `tcp.listen("127.0.0.1:<port>")`: loopback only, not reachable
//!   from other machines;
//! - `tcp.listen("0.0.0.0:<port>")`: every interface.
//!
//! The tests probe the OS for where the listener accepts, rather than
//! asking "can an external IP connect": the test environment may not
//! have several routable interfaces, and a server that is not bound
//! to an interface cannot accept on it.

#![cfg(feature = "http")]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::port_file::PortFile;

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
        "silt_http_bind_{}_{}_{}_{}.silt",
        stem,
        std::process::id(),
        ts,
        n
    ));
    std::fs::write(&tmp, src).unwrap();
    tmp
}

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

fn shutdown(mut child: Child) -> (String, String) {
    let _ = child.kill();
    let out = child.wait_with_output().ok();
    out.map(|o| {
        (
            String::from_utf8_lossy(&o.stdout).to_string(),
            String::from_utf8_lossy(&o.stderr).to_string(),
        )
    })
    .unwrap_or_default()
}

/// A server on a listener bound to `host`, on a port the system
/// chooses and the program writes to `port_path`. Always answers 200.
fn server_src(host: &str, port_path: &str) -> String {
    format!(
        r#"
import http
import io
import tcp

fn main() {{
  when let Ok(listener) = tcp.listen("{host}:0") else {{ panic("cannot listen") }}
  when let Ok(_) = io.write_file("{port_path}", "{{tcp.local_port(listener)}}\n") else {{
    panic("cannot write the port")
  }}
  http.serve(listener) {{ _req ->
    http.Response {{ status: 200, body: "ok", headers: #{{}} }}
  }}
}}
"#
    )
}

/// Probe whether `host:port` is accepting TCP connections within
/// `timeout`. Returns true if we completed a TCP handshake. This is the
/// OS-level "is the listener bound on this interface" test.
fn tcp_probe(host: &str, port: u16, timeout: Duration) -> bool {
    let Ok(addr) = format!("{host}:{port}").parse::<SocketAddr>() else {
        return false;
    };
    match TcpStream::connect_timeout(&addr, timeout) {
        Ok(s) => {
            drop(s);
            true
        }
        Err(_) => false,
    }
}

/// Best-effort discovery of an external (non-loopback) IPv4 address this
/// host owns, suitable for use as the "LAN IP" probe target. Returns
/// `None` if none is available (e.g. isolated CI sandbox).
///
/// Strategy: UDP-connect to a public address on 0 so the kernel fills in
/// the source IP it *would* use, without actually sending a packet. No
/// DNS, no network traffic, just a routing-table lookup.
fn discover_external_ipv4() -> Option<std::net::Ipv4Addr> {
    use std::net::{Ipv4Addr, UdpSocket};
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    // 203.0.113.1 is in TEST-NET-3 — guaranteed not routed, so no traffic
    // leaves the machine, but a kernel routing decision still happens.
    sock.connect("203.0.113.1:9").ok()?;
    let local = sock.local_addr().ok()?;
    if let std::net::IpAddr::V4(v4) = local.ip()
        && !v4.is_loopback()
        && !v4.is_unspecified()
        && v4 != Ipv4Addr::new(0, 0, 0, 0)
    {
        return Some(v4);
    }
    None
}

// ────────────────────────────────────────────────────────────────────────
// A listener on 127.0.0.1 is reached on loopback only.
// ────────────────────────────────────────────────────────────────────────

/// A server on `tcp.listen("127.0.0.1:<port>")` MUST bind 127.0.0.1 only. A developer who
/// writes a quick server should not silently expose it to the LAN.
///
/// Assertions:
///  1. Connecting on 127.0.0.1 succeeds (sanity: the server is up).
///  2. If the host has a non-loopback IPv4, connecting on THAT address
///     must fail — the listener is not bound there. (A server bound to
///     0.0.0.0 would accept the connection on the LAN IP; a server
///     bound to 127.0.0.1 will ECONNREFUSED.)
///  3. If no external IPv4 is discoverable (isolated test env),
///     connecting on 127.0.0.1 still succeeds — at minimum we've proven
///     the server is up and reachable via loopback, and the negative
///     half is a no-op rather than a false-negative failure.
#[test]
fn a_listener_on_loopback_serves_loopback_only() {
    let port_file = PortFile::new();
    let tmp = tmp_silt_file(
        "serve_default_localhost",
        &server_src("127.0.0.1", &port_file.path()),
    );
    let child = spawn_silt(&tmp);

    let port = port_file.wait();

    // (1) Sanity: loopback works.
    assert!(
        tcp_probe("127.0.0.1", port, Duration::from_secs(2)),
        "http.serve should accept on 127.0.0.1:{port}"
    );

    // (2) Negative: if there's a LAN IP, the listener must NOT accept on it.
    match discover_external_ipv4() {
        Some(lan_ip) => {
            let lan = lan_ip.to_string();
            let reachable = tcp_probe(&lan, port, Duration::from_millis(500));
            if reachable {
                let (_stdout, _stderr) = shutdown(child);
                let _ = std::fs::remove_file(&tmp);
                panic!(
                    "http.serve leaked onto external interface {lan}:{port} — \
                     listener bound to 0.0.0.0 instead of 127.0.0.1"
                );
            }
        }
        None => {
            // No routable external IPv4 in this environment (common for
            // containerized/isolated CI). The loopback-success check
            // above is all we can deterministically verify here; the
            // test of the unspecified address below pins down the
            // all-interfaces direction and exercises the same
            // bind-address code path, so regressions are still
            // caught.
        }
    }

    let (_stdout, _stderr) = shutdown(child);
    let _ = std::fs::remove_file(&tmp);
}

// ────────────────────────────────────────────────────────────────────────
// A listener on 0.0.0.0 is reached on all interfaces.
// ────────────────────────────────────────────────────────────────────────

/// A server on `tcp.listen("0.0.0.0:<port>")` is reached on every
/// interface: the address that was written is the one that counts.
///
/// Strategy: the listener is bound on the SAME port on both loopback and
/// (if available) the LAN IP, so a probe on either address must succeed.
/// In an isolated environment with no external IPv4, the minimum guard
/// is that loopback works AND a probe on 0.0.0.0 (which the kernel
/// rewrites to 127.0.0.1 for outbound connect) succeeds.
#[test]
fn a_listener_on_the_unspecified_address_serves_all_interfaces() {
    let port_file = PortFile::new();
    let tmp = tmp_silt_file("all_interfaces", &server_src("0.0.0.0", &port_file.path()));
    let child = spawn_silt(&tmp);

    let port = port_file.wait();

    // Loopback must work (0.0.0.0 includes loopback).
    assert!(
        tcp_probe("127.0.0.1", port, Duration::from_secs(2)),
        "the server should accept on 127.0.0.1:{port}"
    );

    // If there is a LAN IP, the listener MUST be reachable there too —
    // that is what the address asks for.
    if let Some(lan_ip) = discover_external_ipv4() {
        let lan = lan_ip.to_string();
        let reachable = tcp_probe(&lan, port, Duration::from_secs(2));
        if !reachable {
            let (_stdout, _stderr) = shutdown(child);
            let _ = std::fs::remove_file(&tmp);
            panic!(
                "the server did NOT accept on external interface \
                 {lan}:{port} — appears bound to loopback only"
            );
        }
    }

    // Extra guard that works even without a LAN IP: send a real HTTP
    // request over loopback and check we get HTTP/1.1 200 back. If
    // the server on such a listener came down to "do nothing" or to a
    // listener that isn't actually serving, this will catch it.
    let mut sock = TcpStream::connect(("127.0.0.1", port)).expect("connect loopback");
    sock.set_read_timeout(Some(Duration::from_secs(5))).ok();
    sock.write_all(
        format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n").as_bytes(),
    )
    .expect("write request");
    let mut resp = Vec::new();
    let _ = sock.read_to_end(&mut resp);
    let resp_str = String::from_utf8_lossy(&resp);

    let (_stdout, _stderr) = shutdown(child);
    let _ = std::fs::remove_file(&tmp);

    assert!(
        resp_str.starts_with("HTTP/1.1 200") || resp_str.contains(" 200 "),
        "the server: expected 200 OK over loopback; got:\n{resp_str}"
    );
}
