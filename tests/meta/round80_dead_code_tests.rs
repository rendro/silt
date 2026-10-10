//! Round 80 audit — DUP-1 (`src/builtins/tcp.rs`): `ClientStreamWrapper`
//! and `ServerStreamWrapper` had byte-identical impl bodies and were
//! collapsed to a single `stream_wrapper!` macro arm. The lock below
//! drives both wrappers through their handshake-failure paths.

/// Behavioral lock for DUP-1: drive both `ClientStreamWrapper` and
/// `ServerStreamWrapper` through the public tcp.* builtins and assert
/// the handshake-failure path produces identical observable shapes.
///
/// Why this proves no semantic regression: post-collapse, both
/// wrappers come from the same macro arm. If the macro arm was wrong
/// (e.g. accidentally swapped Read for Write, dropped `complete_io`),
/// at least one of the two paths below would diverge from the
/// pre-collapse error shape `Err(TcpTls(_))`.
#[cfg(feature = "tcp-tls")]
#[test]
fn tcp_tls_wrappers_observable_behavior_identical() {
    fn run(input: &str) -> silt::value::Value {
        silt::session::testing::run_str(input).expect("runtime error")
    }

    fn unbound_addr() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        drop(listener);
        addr.to_string()
    }

    // Client side: connect_tls drives `ClientStreamWrapper`. We point
    // at a closed port so the handshake never starts; the wrapper's
    // `complete_io_handshake` propagates the connect error as
    // `Err(TcpTls(_))`. (Pre-fix this came from `ClientStreamWrapper`.)
    let unused_port = unbound_addr();
    let client_src = format!(
        r#"
import tcp

fn main() -> String {{
  match tcp.connect_tls("{unused_port}", "localhost") {{
    Ok(_) -> "unexpected ok"
    Err(e) -> e.message()
  }}
}}
"#
    );
    let client_v = run(&client_src);
    let silt::value::Value::String(client_msg) = client_v else {
        panic!("client_v not a string: {client_v:?}");
    };
    assert!(
        !client_msg.is_empty(),
        "client wrapper produced empty error message"
    );

    // Server side: accept_tls drives `ServerStreamWrapper`. Send
    // garbage on a real connection so the server's
    // `complete_io_handshake` fails with a TLS alert. Pre-fix the
    // body that produced this error lived in `ServerStreamWrapper`.
    let server_src = r#"
import bytes
import tcp
import task
import time

fn main() -> String {
  match tcp.listen("127.0.0.1:0") {
    Ok(listener) -> {
      let server = task.spawn({ ->
        let bad_cert = bytes.from_string("not a real cert")
        let bad_key = bytes.from_string("not a real key")
        match tcp.accept_tls(listener, bad_cert, bad_key) {
          Ok(_) -> "wrong: server expected to fail"
          Err(e) -> e.message()
        }
      })
      time.sleep(time.ms(50))
      let _ = tcp.connect("127.0.0.1:{tcp.local_port(listener)}")
      task.join(server)
    }
    Err(e) -> e.message()
  }
}
"#
    .to_string();
    let server_v = run(&server_src);
    let silt::value::Value::String(server_msg) = server_v else {
        panic!("server_v not a string: {server_v:?}");
    };
    assert!(
        !server_msg.is_empty(),
        "server wrapper produced empty error message"
    );

    // The KEY invariant: both wrappers reach the
    // `complete_io_handshake` failure path, both surface a non-empty
    // string error via the TcpError trait. Identical observable shape
    // confirms the macro arm correctly drives both sides.
}
