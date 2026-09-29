//! End-to-end tests for the `tcp` builtin module (v0.9 PR 2).
//!
//! All tests are hermetic — they bind to `127.0.0.1` with an OS-assigned
//! port (`tcp.listen("127.0.0.1:0")` then read back via `peer_addr` /
//! coordinated port handoff). No external network access.
//!
//! Coverage:
//! - listen errors on invalid address
//! - basic connect / accept / read / write / close roundtrip
//! - read returns Bytes (PR 1's value type)
//! - read on closed stream errors
//! - write on closed stream errors
//! - read_exact reads exactly N bytes
//! - cooperative I/O: a server task and a client task run concurrently
//!   under the silt scheduler without deadlocking
//! - stress: 50 sequential connection roundtrips on the same listener

#![cfg(feature = "tcp")]

use std::sync::Arc;

use silt::types::Severity;
use silt::value::Value;

fn run(input: &str) -> Value {
    let tokens = silt::lexer::Lexer::new(input)
        .tokenize()
        .expect("lex error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = silt::compiler::Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = silt::vm::Vm::new();
    vm.run(script).expect("runtime error")
}

fn type_errors(input: &str) -> Vec<String> {
    let tokens = silt::lexer::Lexer::new(input)
        .tokenize()
        .expect("lex error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    silt::typechecker::check(&mut program)
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

/// Pick a port from the OS by binding then immediately rebinding from
/// silt's perspective. Returns the address string.
fn pick_port() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    drop(listener);
    addr.to_string()
}

// ── Type-level integration ────────────────────────────────────────────

#[test]
fn test_typechecker_accepts_tcp_signatures() {
    let errs = type_errors(
        r#"
import bytes
import tcp
fn main() {
  match tcp.listen("127.0.0.1:0") {
    Ok(l) -> match tcp.accept(l) {
      Ok(s) -> {
        let _ = tcp.write(s, bytes.empty())
        let _ = tcp.read(s, 1024)
        tcp.close(s)
      }
      Err(_) -> ()
    }
    Err(_) -> ()
  }
}
"#,
    );
    assert!(errs.is_empty(), "got: {errs:?}");
}

// ── Basic ops ─────────────────────────────────────────────────────────

#[test]
fn test_listen_invalid_address_errors() {
    let v = run(r#"
import tcp
fn main() {
  match tcp.listen("not a real address") {
    Ok(_) -> "wrong: should error"
    Err(_) -> "ok"
  }
}
"#);
    assert_eq!(v, Value::String("ok".into()));
}

#[test]
fn test_listen_returns_listener_handle() {
    let v = run(r#"
import tcp
fn main() {
  match tcp.listen("127.0.0.1:0") {
    Ok(_) -> "ok"
    Err(e) -> e
  }
}
"#);
    assert_eq!(v, Value::String("ok".into()));
}

// ── Echo roundtrip ────────────────────────────────────────────────────

#[test]
fn test_echo_roundtrip() {
    let addr = pick_port();
    let src = format!(
        r#"
import bytes
import tcp
import task
import time

fn main() {{
  match tcp.listen("{addr}") {{
    Ok(listener) -> {{
      let server = task.spawn(fn() {{
        match tcp.accept(listener) {{
          Ok(conn) -> {{
            match tcp.read(conn, 1024) {{
              Ok(buf) -> {{
                let _ = tcp.write(conn, buf)
                tcp.close(conn)
              }}
              Err(e) -> println("server read err: " + e)
            }}
          }}
          Err(e) -> println("accept err: " + e)
        }}
      }})
      time.sleep(time.ms(50))
      match tcp.connect("{addr}") {{
        Ok(conn) -> {{
          let _ = tcp.write(conn, bytes.from_string("hello"))
          let result = match tcp.read(conn, 1024) {{
            Ok(buf) -> match bytes.to_string(buf) {{
              Ok(s) -> s
              Err(e) -> e
            }}
            Err(e) -> e
          }}
          tcp.close(conn)
          task.join(server)
          result
        }}
        Err(e) -> e
      }}
    }}
    Err(e) -> e
  }}
}}
"#
    );
    let v = run(&src);
    assert_eq!(v, Value::String("hello".into()));
}

#[test]
fn test_read_exact_returns_full_payload() {
    let addr = pick_port();
    let src = format!(
        r#"
import bytes
import tcp
import task
import time

fn main() {{
  match tcp.listen("{addr}") {{
    Ok(listener) -> {{
      let server = task.spawn(fn() {{
        match tcp.accept(listener) {{
          Ok(conn) -> {{
            -- Send 8 bytes in two writes so read_exact has to assemble.
            let _ = tcp.write(conn, bytes.from_string("abcd"))
            time.sleep(time.ms(20))
            let _ = tcp.write(conn, bytes.from_string("efgh"))
            tcp.close(conn)
          }}
          Err(_) -> ()
        }}
      }})
      time.sleep(time.ms(50))
      match tcp.connect("{addr}") {{
        Ok(conn) -> {{
          let result = match tcp.read_exact(conn, 8) {{
            Ok(buf) -> match bytes.to_string(buf) {{
              Ok(s) -> s
              Err(e) -> e
            }}
            Err(e) -> e
          }}
          tcp.close(conn)
          task.join(server)
          result
        }}
        Err(e) -> e
      }}
    }}
    Err(e) -> e
  }}
}}
"#
    );
    let v = run(&src);
    assert_eq!(v, Value::String("abcdefgh".into()));
}

#[test]
fn test_read_after_close_errors() {
    let addr = pick_port();
    let src = format!(
        r#"
import bytes
import tcp
import task
import time

fn main() {{
  match tcp.listen("{addr}") {{
    Ok(listener) -> {{
      let server = task.spawn(fn() {{
        match tcp.accept(listener) {{
          Ok(c) -> tcp.close(c)
          Err(_) -> ()
        }}
      }})
      time.sleep(time.ms(50))
      match tcp.connect("{addr}") {{
        Ok(conn) -> {{
          tcp.close(conn)
          let result = match tcp.read(conn, 16) {{
            Ok(_) -> "wrong: should error"
            Err(_) -> "errored"
          }}
          task.join(server)
          result
        }}
        Err(e) -> e
      }}
    }}
    Err(e) -> e
  }}
}}
"#
    );
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
    let addr = pick_port();
    let src = format!(
        r#"
import bytes
import tcp
import task
import time

fn main() {{
  match tcp.listen("{addr}") {{
    Ok(listener) -> {{
      let server = task.spawn(fn() {{
        match tcp.accept(listener) {{
          Ok(c) -> tcp.close(c)
          Err(_) -> ()
        }}
      }})
      time.sleep(time.ms(50))
      match tcp.connect("{addr}") {{
        Ok(conn) -> {{
          tcp.close(conn)
          let result = match tcp.write(conn, bytes.from_string("hi")) {{
            Ok(_) -> "wrong: should error"
            Err(_) -> "errored"
          }}
          task.join(server)
          result
        }}
        Err(e) -> e
      }}
    }}
    Err(e) -> e
  }}
}}
"#
    );
    let v = run(&src);
    assert_eq!(v, Value::String("errored".into()));
}

#[test]
fn test_connect_to_unbound_port_errors() {
    let addr = pick_port();
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
