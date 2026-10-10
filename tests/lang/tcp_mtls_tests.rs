//! End-to-end tests for the mTLS variant of the `tcp-tls` feature
//! (`tcp.accept_tls_mtls`).
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
    let mut reader = std::io::BufReader::new(pem.as_bytes());
    rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .expect("parse cert pem")
}

fn pem_key_to_der(pem: &str) -> PrivateKeyDer<'static> {
    // rcgen emits a PKCS#8 key in PEM form. `private_key` transparently
    // handles PKCS#8 / PKCS#1 / SEC1 and returns a `PrivateKeyDer`.
    let mut reader = std::io::BufReader::new(pem.as_bytes());
    rustls_pemfile::private_key(&mut reader)
        .expect("parse private key")
        .expect("at least one private key")
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

/// Spawn a silt `tcp.accept_tls_mtls` server. It listens on a port the
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

fn main() -> String {{
  match listen_and_tell("{port_path}") {{
    Ok(listener) -> match bytes.from_hex("{cert_hex}") {{
      Ok(cert) -> match bytes.from_hex("{key_hex}") {{
        Ok(key) -> match bytes.from_hex("{ca_hex}") {{
          Ok(ca) -> match tcp.accept_tls_mtls(listener, cert, key, ca) {{
            Ok(_) -> "ok"
            Err(e) -> "err:{{e.message()}}"
          }}
          Err(e) -> "ca-parse:{{e.message()}}"
        }}
        Err(e) -> "key-parse:{{e.message()}}"
      }}
      Err(e) -> "cert-parse:{{e.message()}}"
    }}
    Err(e) -> "listen:{{e.message()}}"
  }}
}}
"#,
            cert_hex = hex_encode(server_cert_pem.as_bytes()),
            key_hex = hex_encode(server_key_pem.as_bytes()),
            ca_hex = hex_encode(client_ca_pem.as_bytes()),
        );
        let v = run(&src);
        match v {
            Value::String(s) => s.to_string(),
            other => format!("unexpected:{other:?}").into(),
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
    // and `accept_tls_mtls` returns `Err`.
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
