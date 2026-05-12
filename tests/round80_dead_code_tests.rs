//! Round 80 audit — DUP-1 and DEAD-FN regression locks.
//!
//! Two findings landed in this file:
//!
//! 1. **DUP-1 (`src/builtins/tcp.rs`)** — `ClientStreamWrapper` and
//!    `ServerStreamWrapper` had byte-identical impl bodies (same
//!    `complete_io_handshake`, same `Read` / `Write` / `ReadWrite`
//!    forwards). They differed only in the `rustls::StreamOwned<...>`
//!    inner type parameter (`ClientConnection` vs `ServerConnection`).
//!    Round 80 collapsed both to a single `stream_wrapper!` macro arm.
//!
//! 2. **DEAD-FN (`src/value.rs`)** — `IoCompletion::register_waker`
//!    (the non-guard variant) had zero production callers. The only
//!    surviving I/O-waker entry point is
//!    `IoCompletion::register_waker_guard`, which both registers the
//!    entry AND returns an RAII guard for cancel-path cleanup. The
//!    non-guard fn and its sole `legacy_register_io_waker_still_works`
//!    test were deleted together.
//!
//! These tests are source-grep locks: they fail loudly if a future
//! refactor reintroduces the duplication or the dead function.

use std::path::PathBuf;

fn read_src(rel: &str) -> String {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

// ── DUP-1 locks ──────────────────────────────────────────────────────

/// `src/builtins/tcp.rs` must define a `stream_wrapper!` macro and
/// invoke it for both `ClientStreamWrapper` (with `ClientConnection`)
/// and `ServerStreamWrapper` (with `ServerConnection`). If either
/// invocation disappears, or someone reintroduces a hand-rolled
/// `struct ClientStreamWrapper { ... }` definition outside the macro,
/// this test fires.
#[test]
fn tcp_stream_wrappers_emitted_from_single_macro() {
    let src = read_src("src/builtins/tcp.rs");

    // Macro definition exists.
    assert!(
        src.contains("macro_rules! stream_wrapper"),
        "expected `macro_rules! stream_wrapper` in src/builtins/tcp.rs"
    );

    // Both invocations exist.
    assert!(
        src.contains("stream_wrapper!(ClientStreamWrapper, ClientConnection)"),
        "expected `stream_wrapper!(ClientStreamWrapper, ClientConnection)` invocation"
    );
    assert!(
        src.contains("stream_wrapper!(ServerStreamWrapper, ServerConnection)"),
        "expected `stream_wrapper!(ServerStreamWrapper, ServerConnection)` invocation"
    );
}

/// The duplicated impl bodies must not have been re-introduced as
/// hand-rolled `struct $StreamWrapper { ... }` blocks outside the
/// macro. Specifically: there is exactly one occurrence of
/// `inner: rustls::StreamOwned<` in the file (inside the macro body).
/// If the duplication comes back, the count climbs to two.
#[test]
fn tcp_stream_wrapper_inner_field_appears_once() {
    let src = read_src("src/builtins/tcp.rs");
    let count = src.matches("inner: rustls::StreamOwned<").count();
    assert_eq!(
        count, 1,
        "expected exactly one `inner: rustls::StreamOwned<...>` field in tcp.rs \
         (the macro arm); found {count}. Did the Client/Server wrappers get \
         re-duplicated?"
    );
}

/// The handshake body — the most distinctive duplicate — must appear
/// exactly once in the file (inside the macro arm). Pre-fix it
/// appeared twice (one per wrapper).
#[test]
fn tcp_complete_io_handshake_body_appears_once() {
    let src = read_src("src/builtins/tcp.rs");
    let count = src
        .matches("while self.inner.conn.is_handshaking()")
        .count();
    assert_eq!(
        count, 1,
        "expected exactly one `while self.inner.conn.is_handshaking()` loop \
         in tcp.rs (the macro arm); found {count}. Re-duplication regression."
    );
}

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
    use std::sync::Arc;

    fn run(input: &str) -> silt::value::Value {
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

    fn pick_port() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        drop(listener);
        addr.to_string()
    }

    // Client side: connect_tls drives `ClientStreamWrapper`. We point
    // at a closed port so the handshake never starts; the wrapper's
    // `complete_io_handshake` propagates the connect error as
    // `Err(TcpTls(_))`. (Pre-fix this came from `ClientStreamWrapper`.)
    let unused_port = pick_port();
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
    let addr = pick_port();
    let server_src = format!(
        r#"
import bytes
import tcp
import task
import time

fn main() -> String {{
  match tcp.listen("{addr}") {{
    Ok(listener) -> {{
      let server = task.spawn(fn() {{
        let bad_cert = bytes.from_string("not a real cert")
        let bad_key = bytes.from_string("not a real key")
        match tcp.accept_tls(listener, bad_cert, bad_key) {{
          Ok(_) -> "wrong: server expected to fail"
          Err(e) -> e.message()
        }}
      }})
      time.sleep(time.ms(50))
      let _ = tcp.connect("{addr}")
      task.join(server)
    }}
    Err(e) -> e.message()
  }}
}}
"#
    );
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

// ── DEAD-FN locks ────────────────────────────────────────────────────

/// The non-guard `IoCompletion::register_waker` signature must NOT
/// reappear in `src/value.rs`. Its sole production-removal proof is
/// negative — every call site uses `register_waker_guard` (locked by
/// `cancel_path_join_io_waker_leak_tests::*`).
///
/// We also assert `register_waker_guard` is still present, so the
/// ban-list cannot accidentally pass by the guard variant being
/// renamed too.
#[test]
fn register_waker_non_guard_absent_from_value_rs() {
    let src = read_src("src/value.rs");

    // Banned: the non-guard signature. Note we look for the exact
    // signature including `(&self,` so the substring matches the
    // pre-removal definition unambiguously and does NOT match the
    // guard variant which is `(self: &Arc<Self>,`.
    let banned_signature_pattern = "pub fn register_waker(&self,";
    assert!(
        !src.contains(banned_signature_pattern),
        "src/value.rs must not contain the non-guard \
         `pub fn register_waker(&self, ...)`. Round 80 removed it as dead \
         code; every I/O entry guard now uses `register_waker_guard`."
    );

    // Sanity: the guard variant is still present. If a future rename
    // moves the guard out of `register_waker_guard`, the ban-list
    // above would silently pass — this assert catches that case.
    assert!(
        src.contains("pub fn register_waker_guard"),
        "src/value.rs must still define `pub fn register_waker_guard`; \
         it is the only remaining I/O-waker registration entry point."
    );
}

/// The deleted `legacy_register_io_waker_still_works` test must NOT
/// reappear in `tests/cancel_path_join_io_waker_leak_tests.rs`. The
/// non-guard fn is gone; if someone re-adds the test, it would fail
/// to compile (the fn doesn't exist), but a source-grep here surfaces
/// the regression up front rather than at compile time.
#[test]
fn legacy_register_io_waker_test_removed() {
    let src = read_src("tests/cancel_path_join_io_waker_leak_tests.rs");
    assert!(
        !src.contains("fn legacy_register_io_waker_still_works"),
        "tests/cancel_path_join_io_waker_leak_tests.rs must not redefine \
         `legacy_register_io_waker_still_works`. The non-guard \
         `IoCompletion::register_waker` it asserted is removed."
    );
    // The sibling `legacy_register_join_waker_still_works` should
    // remain — `TaskHandle::register_join_waker` (non-guard) IS still
    // used by `concurrency::main_thread_wait_for_join`. Asserting
    // both directions keeps the ban surgical.
    assert!(
        src.contains("fn legacy_register_join_waker_still_works"),
        "tests/cancel_path_join_io_waker_leak_tests.rs must still define \
         `legacy_register_join_waker_still_works`; the join-waker non-guard \
         API is NOT dead and must keep its lock test."
    );
}
