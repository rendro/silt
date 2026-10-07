//! End-to-end tests for the `tcp` builtin module (v0.9 PR 2).
//!
//! All tests are hermetic — they bind to `127.0.0.1` with an OS-assigned
//! port (`tcp.listen("127.0.0.1:0")` then read back via `peer_addr` /
//! coordinated port handoff). No external network access.
//!
//! Coverage:
//! - basic connect / accept / read / write / close roundtrip
//! - read returns Bytes (PR 1's value type)
//! - read on closed stream errors
//! - write on closed stream errors
//! - read_exact reads exactly N bytes
//! - cooperative I/O: a server task and a client task run concurrently
//!   under the silt scheduler without deadlocking
//! - stress: 50 sequential connection roundtrips on the same listener
//!
//! The socket-free cases (signature typecheck, invalid listen address)
//! are golden cases `tests/golden/lang/tcp/tcp_module__*` (feature `tcp`).

#![cfg(feature = "tcp")]

use silt::value::Value;

fn run(input: &str) -> Value {
    silt::session::testing::run_str(input).unwrap_or_else(|e| panic!("{e}"))
}

/// An address that nobody listens on: a port the OS gave and that was
/// given back. (The tests that listen ask for port 0 themselves and
/// read the port with `tcp.local_port`.)
fn unbound_addr() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    drop(listener);
    addr.to_string()
}

// ── Basic ops ─────────────────────────────────────────────────────────

#[test]
fn test_listen_returns_listener_handle() {
    let v = run(r#"
import tcp
fn main() {
  match tcp.listen("127.0.0.1:0") {
    Ok(_) -> "ok"
    Err(e) -> e.message()
  }
}
"#);
    assert_eq!(v, Value::String("ok".into()));
}

// ── Echo roundtrip ────────────────────────────────────────────────────

#[test]
fn test_echo_roundtrip() {
    let src = r#"
import bytes
import tcp
import task
import time

fn main() {
  match tcp.listen("127.0.0.1:0") {
    Ok(listener) -> {
      let server = task.spawn({ ->
        match tcp.accept(listener) {
          Ok(conn) -> {
            match tcp.read(conn, 1024) {
              Ok(buf) -> {
                let _ = tcp.write(conn, buf)
                tcp.close(conn)
              }
              Err(e) -> println("server read err: {e}")
            }
          }
          Err(e) -> println("accept err: {e}")
        }
      })
      time.sleep(time.ms(50))
      match tcp.connect("127.0.0.1:{tcp.local_port(listener)}") {
        Ok(conn) -> {
          let _ = tcp.write(conn, bytes.from_string("hello"))
          let result = match tcp.read(conn, 1024) {
            Ok(buf) -> match bytes.to_string(buf) {
              Ok(s) -> s
              Err(e) -> e.message()
            }
            Err(e) -> e.message()
          }
          tcp.close(conn)
          task.join(server)
          result
        }
        Err(e) -> e.message()
      }
    }
    Err(e) -> e.message()
  }
}
"#
    .to_string();
    let v = run(&src);
    assert_eq!(v, Value::String("hello".into()));
}

#[test]
fn test_read_exact_returns_full_payload() {
    let src = r#"
import bytes
import tcp
import task
import time

fn main() {
  match tcp.listen("127.0.0.1:0") {
    Ok(listener) -> {
      let server = task.spawn({ ->
        match tcp.accept(listener) {
          Ok(conn) -> {
            -- Send 8 bytes in two writes so read_exact has to assemble.
            let _ = tcp.write(conn, bytes.from_string("abcd"))
            time.sleep(time.ms(20))
            let _ = tcp.write(conn, bytes.from_string("efgh"))
            tcp.close(conn)
          }
          Err(_) -> ()
        }
      })
      time.sleep(time.ms(50))
      match tcp.connect("127.0.0.1:{tcp.local_port(listener)}") {
        Ok(conn) -> {
          let result = match tcp.read_exact(conn, 8) {
            Ok(buf) -> match bytes.to_string(buf) {
              Ok(s) -> s
              Err(e) -> e.message()
            }
            Err(e) -> e.message()
          }
          tcp.close(conn)
          task.join(server)
          result
        }
        Err(e) -> e.message()
      }
    }
    Err(e) -> e.message()
  }
}
"#
    .to_string();
    let v = run(&src);
    assert_eq!(v, Value::String("abcdefgh".into()));
}

#[test]
fn test_read_after_close_errors() {
    let src = r#"
import bytes
import tcp
import task
import time

fn main() {
  match tcp.listen("127.0.0.1:0") {
    Ok(listener) -> {
      let server = task.spawn({ ->
        match tcp.accept(listener) {
          Ok(c) -> tcp.close(c)
          Err(_) -> ()
        }
      })
      time.sleep(time.ms(50))
      match tcp.connect("127.0.0.1:{tcp.local_port(listener)}") {
        Ok(conn) -> {
          tcp.close(conn)
          let result = match tcp.read(conn, 16) {
            Ok(_) -> "wrong: should error"
            Err(_) -> "errored"
          }
          task.join(server)
          result
        }
        Err(e) -> e.message()
      }
    }
    Err(e) -> e.message()
  }
}
"#
    .to_string();
    let v = run(&src);
    // Lock the exact Err-branch string. The previous `contains("error")`
    // was satisfied by the Ok-branch sentinel "wrong: should error" too
    // (both contained "error"), so the assertion could never fail.
    // Sibling tests (`test_write_after_close_errors`,
    // `test_connect_to_unbound_port_errors`) already use this shape.
    assert_eq!(v, Value::String("errored".into()));
}

#[test]
fn test_write_after_close_errors() {
    let src = r#"
import bytes
import tcp
import task
import time

fn main() {
  match tcp.listen("127.0.0.1:0") {
    Ok(listener) -> {
      let server = task.spawn({ ->
        match tcp.accept(listener) {
          Ok(c) -> tcp.close(c)
          Err(_) -> ()
        }
      })
      time.sleep(time.ms(50))
      match tcp.connect("127.0.0.1:{tcp.local_port(listener)}") {
        Ok(conn) -> {
          tcp.close(conn)
          let result = match tcp.write(conn, bytes.from_string("hi")) {
            Ok(_) -> "wrong: should error"
            Err(_) -> "errored"
          }
          task.join(server)
          result
        }
        Err(e) -> e.message()
      }
    }
    Err(e) -> e.message()
  }
}
"#
    .to_string();
    let v = run(&src);
    assert_eq!(v, Value::String("errored".into()));
}

#[test]
fn test_connect_to_unbound_port_errors() {
    let addr = unbound_addr();
    let src = format!(
        r#"
import tcp
fn main() {{
  match tcp.connect("{addr}") {{
    Ok(_) -> "wrong: should error"
    Err(_) -> "errored"
  }}
}}
"#
    );
    let v = run(&src);
    assert_eq!(v, Value::String("errored".into()));
}
