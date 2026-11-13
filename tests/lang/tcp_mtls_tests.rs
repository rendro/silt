//! End-to-end tests of the TLS server of the `tcp-tls` feature with a
//! real TLS client: the mTLS variant (`tcp.tls_server_mtls`), and a
//! `tcp.tls_server` handshake per connection.
//!
//! Hermetic — generates a CA, server cert, and client cert in-process
//! using `rcgen` (already in `[dev-dependencies]`), runs a loopback
//! listener, and connects using a rustls `ClientConfig` we construct in
//! the test harness (since silt's `tcp.connect_tls` does not currently
//! supply client certificates). No external network access.
//!
//! The `tcp-tls` feature is opt-in; the entire file is gated so
//! `cargo test` without `--features tcp-tls` skips it.

#![cfg(all(feature = "tcp", feature = "tcp-tls"))]

use std::io::Write;
use std::net::TcpStream;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair, KeyUsagePurpose};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::{ClientConfig, ClientConnection, RootCertStore};

use silt::value::Value;

use crate::port_file::PortFile;

fn run(input: &str) -> Value {
    silt::session::testing::run_str(input).unwrap_or_else(|e| panic!("{e}"))
}

/// A full mTLS PKI generated for a single test.
struct MtlsPki {
    ca_cert_pem: String,
    server_cert_pem: String,
    server_key_pem: String,
    client_cert_pem: String,
    client_key_pem: String,
}

/// Generate an in-memory PKI: a CA, a server cert for `localhost`
/// signed by the CA, and a client cert also signed by the CA. Returns
/// PEM strings. Uses `rcgen` (already a dev-dependency).
fn generate_pki() -> MtlsPki {
    let ca_key = KeyPair::generate().expect("ca keypair");
    let mut ca_params =
        CertificateParams::new(vec!["silt mTLS test CA".to_string()]).expect("ca params");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let ca_cert = ca_params.self_signed(&ca_key).expect("self-sign CA");

    let server_key = KeyPair::generate().expect("server keypair");
    let server_params =
        CertificateParams::new(vec!["localhost".to_string(), "127.0.0.1".to_string()])
            .expect("server params");
    let server_cert = server_params
        .signed_by(&server_key, &ca_cert, &ca_key)
        .expect("sign server cert");

    let client_key = KeyPair::generate().expect("client keypair");
    let client_params =
        CertificateParams::new(vec!["silt-mtls-client".to_string()]).expect("client params");
    let client_cert = client_params
        .signed_by(&client_key, &ca_cert, &ca_key)
        .expect("sign client cert");

    MtlsPki {
        ca_cert_pem: ca_cert.pem(),
        server_cert_pem: server_cert.pem(),
        server_key_pem: server_key.serialize_pem(),
        client_cert_pem: client_cert.pem(),
        client_key_pem: client_key.serialize_pem(),
    }
}

fn pem_certs_to_der(pem: &str) -> Vec<CertificateDer<'static>> {
    CertificateDer::pem_slice_iter(pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .expect("parse cert pem")
}

fn pem_key_to_der(pem: &str) -> PrivateKeyDer<'static> {
    // rcgen emits a PKCS#8 key in PEM form. `PrivateKeyDer` reads
    // PKCS#8, PKCS#1 and SEC1 alike.
    PrivateKeyDer::from_pem_slice(pem.as_bytes()).expect("parse private key")
}

/// Build a rustls `ClientConfig` trusting `trust_ca_pem`. If
/// `client_identity` is `Some((cert_pem, key_pem))`, also present that
/// identity to the server during the handshake.
fn build_client_config(trust_ca_pem: &str, client_identity: Option<(&str, &str)>) -> ClientConfig {
    let mut roots = RootCertStore::empty();
    for cert in pem_certs_to_der(trust_ca_pem) {
        roots.add(cert).expect("add ca root");
    }
    let builder = ClientConfig::builder().with_root_certificates(roots);
    match client_identity {
        Some((cert_pem, key_pem)) => {
            let chain = pem_certs_to_der(cert_pem);
            let key = pem_key_to_der(key_pem);
            builder
                .with_client_auth_cert(chain, key)
                .expect("with_client_auth_cert")
        }
        None => builder.with_no_client_auth(),
    }
}

/// Drive a rustls client handshake against `addr`. Returns Ok(()) if
/// the TLS handshake succeeded (including mTLS cert presentation when
/// requested), Err(msg) otherwise.
fn rustls_client_handshake(
    addr: &str,
    server_name: &str,
    trust_ca_pem: &str,
    client_identity: Option<(&str, &str)>,
) -> Result<(), String> {
    let config = build_client_config(trust_ca_pem, client_identity);
    let server_name =
        ServerName::try_from(server_name.to_string()).map_err(|e| format!("server name: {e}"))?;
    let mut conn = ClientConnection::new(Arc::new(config), server_name)
        .map_err(|e| format!("client conn: {e}"))?;
    let mut sock = TcpStream::connect(addr).map_err(|e| format!("connect: {e}"))?;
    sock.set_read_timeout(Some(Duration::from_secs(5))).ok();
    sock.set_write_timeout(Some(Duration::from_secs(5))).ok();
    // Drive the handshake synchronously. `complete_io` runs both
    // directions of the handshake, surfacing any alert (bad_certificate,
    // certificate_required, etc) as an io::Error.
    while conn.is_handshaking() {
        conn.complete_io(&mut sock)
            .map_err(|e| format!("handshake: {e}"))?;
    }
    // Close cleanly so the server-side read doesn't stall.
    conn.send_close_notify();
    let _ = conn.complete_io(&mut sock);
    let _ = sock.flush();
    Ok(())
}

/// Spawn a silt `tcp.tls_server_mtls` server. It listens on a port the
/// system chooses and writes the port to `port_path`: the listener is
/// bound when the test has read it, so the one connection the test
/// makes is the mTLS peer (the server accepts exactly one). Returns a
/// JoinHandle; the caller joins to read the server's outcome.
fn spawn_mtls_server(
    port_path: String,
    server_cert_pem: String,
    server_key_pem: String,
    client_ca_pem: String,
) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        // Encode PEMs as hex so they can be embedded safely in silt
        // source (no escaping headaches around the BEGIN/END markers).
        let src = format!(
            r#"
import bytes
import io
import tcp

fn listen_and_tell(port_file) {{
  match tcp.listen("127.0.0.1:0") {{
    Ok(listener) -> {{
      when let Ok(_) = io.write_file(port_file, "{{tcp.local_port(listener)}}\n") else {{
        panic("cannot write the port")
      }}
      Ok(listener)
    }}
    Err(e) -> Err(e)
  }}
}}

fn main() {{
  host.give(match listen_and_tell("{port_path}") {{
    Ok(listener) -> match bytes.from_hex("{cert_hex}") {{
      Ok(cert) -> match bytes.from_hex("{key_hex}") {{
        Ok(key) -> match bytes.from_hex("{ca_hex}") {{
          Ok(ca) -> match tcp.accept(listener) {{
            Ok(conn) -> match tcp.tls_server_mtls(conn, cert, key, ca) {{
              Ok(_) -> "ok"
              Err(e) -> "err:{{Error.message(e)}}"
            }}
            Err(e) -> "accept:{{Error.message(e)}}"
          }}
          Err(e) -> "ca-parse:{{Error.message(e)}}"
        }}
        Err(e) -> "key-parse:{{Error.message(e)}}"
      }}
      Err(e) -> "cert-parse:{{Error.message(e)}}"
    }}
    Err(e) -> "listen:{{Error.message(e)}}"
  }})
}}
import host
"#,
            cert_hex = hex_encode(server_cert_pem.as_bytes()),
            key_hex = hex_encode(server_key_pem.as_bytes()),
            ca_hex = hex_encode(client_ca_pem.as_bytes()),
        );
        let v = run(&src);
        match v {
            Value::String(s, _) => s.to_string(),
            other => format!("unexpected:{other:?}"),
        }
    })
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(hex_char(b >> 4));
        out.push(hex_char(b & 0x0f));
    }
    out
}
fn hex_char(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        10..=15 => (b'a' + n - 10) as char,
        _ => unreachable!(),
    }
}

// ── Tests ──────────────────────────────────────────────────────────────

// Quarantine lifted (round 92). The d17ff85 quarantine blamed
// "rustls 0.23's stricter client-auth defaults", but the fixtures and
// the product mTLS path were both fine. The real bug was a startup
// race in this harness: the tests slept a fixed 150ms after spawning
// the silt server, but the server needs ~0.5s (debug build) to compile
// its 30KB source and bind. The client's connect was refused; the
// valid-cert test then failed its assert, and the two reject tests —
// which discard the client result — left the server blocked forever in
// `accept()` (the "hang"). Fixed by retrying the handshake connection
// until the listener is up (`connect_with_retry`).
#[test]
fn mtls_accept_rejects_client_without_cert() {
    // rustls 0.23 defaults `WebPkiClientVerifier::builder(...).build()`
    // to *requiring* client auth. If the client does not present a
    // cert, the server aborts the handshake with `certificate_required`
    // and `tls_server_mtls` returns `Err`.
    let pki = generate_pki();
    let port_file = PortFile::new();
    let server = spawn_mtls_server(
        port_file.path(),
        pki.server_cert_pem.clone(),
        pki.server_key_pem.clone(),
        pki.ca_cert_pem.clone(),
    );
    let addr = format!("127.0.0.1:{}", port_file.wait());
    // Client trusts the server CA but presents *no* client cert. The
    // server-side outcome is what this test locks down: silt returns
    // `Err(_)` because the handshake fails with `certificate_required`
    // (or similar). In TLS 1.3 the client can legitimately finish its
    // end of the handshake before the server's alert arrives, so we do
    // not assert on the client-side outcome — only the server's.
    let _ = rustls_client_handshake(&addr, "localhost", &pki.ca_cert_pem, None);
    let server_result = server.join().expect("server thread");
    assert!(
        server_result.starts_with("err:"),
        "expected server Err(...), got: {server_result:?}"
    );
}

#[test]
fn mtls_accept_accepts_client_with_valid_cert() {
    // Happy path: client presents a cert signed by the CA the server
    // trusts. Handshake succeeds on both sides.
    let pki = generate_pki();
    let port_file = PortFile::new();
    let server = spawn_mtls_server(
        port_file.path(),
        pki.server_cert_pem.clone(),
        pki.server_key_pem.clone(),
        pki.ca_cert_pem.clone(),
    );
    let addr = format!("127.0.0.1:{}", port_file.wait());
    let client_result = rustls_client_handshake(
        &addr,
        "localhost",
        &pki.ca_cert_pem,
        Some((&pki.client_cert_pem, &pki.client_key_pem)),
    );
    assert!(
        client_result.is_ok(),
        "expected client handshake success, got: {client_result:?}"
    );
    let server_result = server.join().expect("server thread");
    assert_eq!(
        server_result, "ok",
        "expected server Ok, got: {server_result:?}"
    );
}

#[test]
fn mtls_accept_rejects_client_with_wrong_ca_cert() {
    // Client presents a cert signed by a *different* CA. The server's
    // client verifier rejects the chain and the handshake fails.
    let server_pki = generate_pki();
    let other_pki = generate_pki();
    let port_file = PortFile::new();
    let server = spawn_mtls_server(
        port_file.path(),
        server_pki.server_cert_pem.clone(),
        server_pki.server_key_pem.clone(),
        server_pki.ca_cert_pem.clone(), // server trusts only server_pki's CA
    );
    let addr = format!("127.0.0.1:{}", port_file.wait());
    // The client trusts the server CA (so server-side cert verifies
    // fine), but its own client cert is signed by a foreign CA. That
    // must be rejected by the server's `WebPkiClientVerifier`. As with
    // the "no client cert" test, the server outcome is authoritative —
    // the client side can legitimately Finish before the fatal alert
    // arrives in TLS 1.3.
    let _ = rustls_client_handshake(
        &addr,
        "localhost",
        &server_pki.ca_cert_pem,
        Some((&other_pki.client_cert_pem, &other_pki.client_key_pem)),
    );
    let server_result = server.join().expect("server thread");
    assert!(
        server_result.starts_with("err:"),
        "expected server Err(...), got: {server_result:?}"
    );
}

/// The handshake runs in the connection's own task: a client that
/// connects and sends nothing holds up that task only. The accept loop
/// takes the next client at once, and that client's handshake
/// completes while the silent one's still waits. (With the handshake
/// inside the accept, the second client waited behind the first.) A
/// connection that speaks TLS already is refused by `tcp.tls_server`
/// and stays usable.
#[test]
fn a_silent_client_does_not_hold_up_the_next_handshake() {
    let pki = generate_pki();
    let port_file = PortFile::new();
    let port_path = port_file.path();
    let server = thread::spawn(move || {
        let src = format!(
            r#"
import bytes
import channel
import io
import task
import tcp

fn serve(conn, cert, key, done) {{
  match tcp.tls_server(conn, cert, key) {{
    Ok(tls) -> {{
      -- A TLS connection is not handed to tls_server again.
      let again = match tcp.tls_server(tls, cert, key) {{
        Ok(_) -> "a second handshake"
        Err(e) -> Error.message(e)
      }}
      let _ = tcp.write(tls, bytes.from_string("hello over TLS"))
      tcp.close(tls)
      channel.send(done, "served; {{again}}")
    }}
    Err(e) -> channel.send(done, "err:{{Error.message(e)}}")
  }}
}}

fn main() {{
  when let Ok(listener) = tcp.listen("127.0.0.1:0") else {{ panic("cannot listen") }}
  when let Ok(_) = io.write_file("{port_path}", "{{tcp.local_port(listener)}}\n") else {{
    panic("cannot write the port")
  }}
  when let Ok(cert) = bytes.from_hex("{cert_hex}") else {{ panic("no cert") }}
  when let Ok(key) = bytes.from_hex("{key_hex}") else {{ panic("no key") }}
  let done = channel.new(2)
  -- The silent client first, then the real one: each gets a task.
  when let Ok(silent) = tcp.accept(listener) else {{ panic("no first client") }}
  let first = task.spawn {{ -> serve(silent, cert, key, done) }}
  when let Ok(real) = tcp.accept(listener) else {{ panic("no second client") }}
  let _ = task.spawn {{ -> serve(real, cert, key, done) }}
  -- The real client's handshake ends while the silent one's waits.
  let outcome = match channel.receive(done) {{
    Some(what) -> what
    None -> "closed"
  }}
  task.cancel(first)
  host.give(outcome)
}}
import host
"#,
            cert_hex = hex_encode(pki.server_cert_pem.as_bytes()),
            key_hex = hex_encode(pki.server_key_pem.as_bytes()),
        );
        match run(&src) {
            Value::String(s, _) => s.to_string(),
            other => format!("unexpected:{other:?}"),
        }
    });
    let addr = format!("127.0.0.1:{}", port_file.wait());
    // Connected and silent until the test ends.
    let silent = TcpStream::connect(&addr).expect("the silent client connects");
    let config = build_client_config(&pki.ca_cert_pem, None);
    let name = ServerName::try_from("localhost".to_string()).expect("server name");
    let mut conn = ClientConnection::new(Arc::new(config), name).expect("client connection");
    let mut sock = TcpStream::connect(&addr).expect("the real client connects");
    sock.set_read_timeout(Some(Duration::from_secs(30))).ok();
    let mut tls = rustls::Stream::new(&mut conn, &mut sock);
    let mut greeting = [0u8; 14];
    let read = std::io::Read::read_exact(&mut tls, &mut greeting);
    assert!(
        read.is_ok(),
        "the real client's handshake waited behind the silent client: {read:?}"
    );
    assert_eq!(&greeting, b"hello over TLS");
    assert_eq!(
        server.join().expect("server thread"),
        "served; tcp TLS error: the connection speaks TLS already"
    );
    drop(silent);
}

/// A TLS client that goes away while a server written in silt writes to
/// it does not end the server: the writes fail with an error. (The TLS
/// library wrote with `writev`, which raises SIGPIPE, and the `silt`
/// command, which sets that signal to its default for stdout, died by
/// it: status 141, no message.) Run as the `silt` command runs programs.
#[test]
#[cfg(unix)]
fn a_tls_peer_that_goes_away_does_not_end_the_program() {
    let pki = generate_pki();
    let port_file = PortFile::new();
    let dir = std::env::temp_dir().join(format!("silt_tls_sigpipe_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.silt");
    std::fs::write(
        &file,
        format!(
            r#"import bytes
import io
import tcp
import time

fn write_until_it_fails(tls: tcp.TcpStream, n: Int) -> String {{
  match n > 0 {{
    true -> match tcp.write(tls, bytes.from_string("data that nobody reads any more\n")) {{
      Ok(_) -> {{
        time.sleep(time.ms(20))
        write_until_it_fails(tls, n - 1)
      }}
      Err(e) -> "a write failed: {{Error.message(e)}}"
    }}
    false -> "every write went"
  }}
}}

fn main() {{
  when let Ok(listener) = tcp.listen("127.0.0.1:0") else {{ panic("cannot listen") }}
  when let Ok(_) = io.write_file("{port_path}", "{{tcp.local_port(listener)}}\n") else {{
    panic("cannot write the port")
  }}
  when let Ok(cert) = bytes.from_hex("{cert_hex}") else {{ panic("no cert") }}
  when let Ok(key) = bytes.from_hex("{key_hex}") else {{ panic("no key") }}
  when let Ok(conn) = tcp.accept(listener) else {{ panic("no client") }}
  when let Ok(tls) = tcp.tls_server(conn, cert, key) else {{ panic("no handshake") }}
  time.sleep(time.ms(200))
  println(write_until_it_fails(tls, 200))
  println("the server is still running")
}}
"#,
            port_path = port_file.path(),
            cert_hex = hex_encode(pki.server_cert_pem.as_bytes()),
            key_hex = hex_encode(pki.server_key_pem.as_bytes()),
        ),
    )
    .unwrap();
    let server = std::process::Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("run")
        .arg(&file)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the server starts");
    let addr = format!("127.0.0.1:{}", port_file.wait());
    // The handshake, then the client goes away at once.
    rustls_client_handshake(&addr, "localhost", &pki.ca_cert_pem, None).expect("handshake");
    let output = server.wait_with_output().expect("the server ends");
    let _ = std::fs::remove_dir_all(&dir);
    let out = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {out}; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(out.starts_with("a write failed: "), "{out}");
    assert!(out.ends_with("the server is still running\n"), "{out}");
}

/// The TLS server of the tests below: it writes its port to
/// `port_path`, accepts `n` clients, makes each a TLS connection, and
/// runs `handle(conn, i)` for each in a task of its own; it gives
/// `after` once every handler has returned.
fn tls_handlers_src(port_path: &str, pki: &MtlsPki, n: usize, handle: &str, after: &str) -> String {
    format!(
        r#"
import bytes
import channel
import io
import list
import result
import stream
import task
import tcp
import time

fn head(conn: tcp.TcpStream) -> List(String) {{
  let lines = stream.tcp_lines(conn)
  loop got = [] {{
    match channel.receive(lines) {{
      Some(Ok("")) -> got
      Some(Ok(line)) -> loop(list.append(got, line))
      _ -> got
    }}
  }}
}}

{handle}

fn main() {{
  when let Ok(listener) = tcp.listen("127.0.0.1:0") else {{ panic("cannot listen") }}
  when let Ok(_) = io.write_file("{port_path}", "{{tcp.local_port(listener)}}\n") else {{
    panic("cannot write the port")
  }}
  when let Ok(cert) = bytes.from_hex("{cert_hex}") else {{ panic("no cert") }}
  when let Ok(key) = bytes.from_hex("{key_hex}") else {{ panic("no key") }}
  let handlers = list.map(1..{n}) {{ i ->
    when let Ok(raw) = tcp.accept(listener) else {{ panic("no client") }}
    task.spawn {{ ->
      when let Ok(conn) = tcp.tls_server(raw, cert, key) else {{ panic("no handshake") }}
      handle(conn, i)
    }}
  }}
  list.each(handlers) {{ h -> task.join(h) }}
  host.give({after})
}}
import host
"#,
        cert_hex = hex_encode(pki.server_cert_pem.as_bytes()),
        key_hex = hex_encode(pki.server_key_pem.as_bytes()),
    )
}

/// A rustls client of `addr`, its handshake done.
fn tls_client(addr: &str, pki: &MtlsPki) -> (ClientConnection, TcpStream) {
    let config = build_client_config(&pki.ca_cert_pem, None);
    let name = ServerName::try_from("localhost".to_string()).expect("server name");
    let mut conn = ClientConnection::new(Arc::new(config), name).expect("client connection");
    let mut sock = TcpStream::connect(addr).expect("the client connects");
    sock.set_read_timeout(Some(Duration::from_secs(20))).ok();
    while conn.is_handshaking() {
        conn.complete_io(&mut sock).expect("handshake");
    }
    (conn, sock)
}

/// STR-2 on TLS: a helper reads the head of a request through
/// `stream.tcp_lines(conn)` and returns; the handler then answers on
/// `conn`, while the client waits for the answer and sends nothing
/// more. The stream's read in flight is woken when the stream is left,
/// so the answer does not wait behind it. (It waited until the client
/// sent again: the read held the connection's lock.)
#[test]
fn a_tls_stream_left_by_a_helper_leaves_the_connection_to_its_handler() {
    let pki = generate_pki();
    let port_file = PortFile::new();
    let src = tls_handlers_src(
        &port_file.path(),
        &pki,
        1,
        r#"fn handle(conn: tcp.TcpStream, _i: Int) -> () {
  let got = head(conn)
  let _ = tcp.write(conn, bytes.from_string("got {list.length(got)} lines\n"))
}"#,
        r#""answered""#,
    );
    let server = thread::spawn(move || match run(&src) {
        Value::String(s, _) => s.to_string(),
        other => format!("unexpected:{other:?}"),
    });
    let addr = format!("127.0.0.1:{}", port_file.wait());
    let (mut conn, mut sock) = tls_client(&addr, &pki);
    let mut tls = rustls::Stream::new(&mut conn, &mut sock);
    tls.write_all(b"a\nb\n\n").expect("the request");
    let mut answer = [0u8; 12];
    let read = std::io::Read::read_exact(&mut tls, &mut answer);
    assert!(read.is_ok(), "the answer did not come: {read:?}");
    assert_eq!(&answer, b"got 2 lines\n");
    assert_eq!(server.join().expect("server thread"), "answered");
}

/// STR-2 on TLS: handlers that give up reading the head of silent
/// clients (a deadline around the helper) and return leave nothing
/// behind: each stream's read is woken and ends, so the connections,
/// held by nothing any more, close, and the I/O pool is free: a file
/// read after them runs.
/// (The reads ran on, held their connections and counted among the
/// operations that cannot be interrupted: 70 such clients made every
/// I/O operation fail.)
#[test]
fn tls_handlers_that_give_up_on_silent_clients_leave_nothing_behind() {
    let n = 70;
    let pki = generate_pki();
    let port_file = PortFile::new();
    let read_back = port_file.path();
    let src = tls_handlers_src(
        &port_file.path(),
        &pki,
        n,
        r#"fn handle(conn: tcp.TcpStream, _i: Int) -> () {
  let _ = task.deadline(time.ms(200)) { -> head(conn) }
}"#,
        &format!(
            r#"io.read_file("{read_back}") |> result.map {{ _ -> "a file read after them" }} |> result.unwrap_or("no file read")"#
        ),
    );
    let server = thread::spawn(move || match run(&src) {
        Value::String(s, _) => s.to_string(),
        other => format!("unexpected:{other:?}"),
    });
    let addr = format!("127.0.0.1:{}", port_file.wait());
    // The clients stay, silent, until the server has said how it went.
    let clients: Vec<(ClientConnection, TcpStream)> =
        (0..n).map(|_| tls_client(&addr, &pki)).collect();
    assert_eq!(
        server.join().expect("server thread"),
        "a file read after them"
    );
    drop(clients);
}
