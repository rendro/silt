//! Round 79 (F5 BLOAT/parity) — `tcp.rs` `require_*` helpers use the
//! canonical `"<fn> requires <Kind>, got <kind>"` shape.
//!
//! Behavioural lock: `tcp.accept(id(123))` is ill-typed (the typechecker
//! rejects it); the typechecker's verdict is ignored here so the runtime
//! `require_listener` defence is reached, and its error must name the
//! offending kind.

#![cfg(feature = "tcp")]

use std::sync::Arc;

// ── Test 2: behavioural — runtime path emits canonical shape ────────

/// End-to-end: route a non-listener through a generic `id` function so
/// the typechecker can't reject the wrong-kind argument at the call
/// site, then call `tcp.accept(id(123))`. The call reaches the runtime
/// and trips `require_listener`. The error must contain both
/// `"requires TcpListener"` (the helper's invariant) and `"got "` (the
/// canonical-shape tail naming the offending kind).
///
/// We use `tcp.accept` rather than `tcp.set_nodelay` (which exercises
/// `require_bool`) because `set_nodelay` requires a real `TcpStream` as
/// arg 0 — reaching its `require_bool` on arg 1 would need a full
/// `listen → accept → connect` sequence with cross-task coordination,
/// purely to set up a single error-message check. `tcp.accept` only
/// requires one argument and the `require_listener` path covers the
/// same canonical-shape invariant.
#[test]
fn tcp_accept_with_non_listener_emits_canonical_shape() {
    let src = r#"
import tcp

-- Generic identity function: returns whatever was passed in. The
-- typechecker infers a fresh type variable for `x`, so the call site
-- below can pass `id(123)` (an Int) where `tcp.accept` expects a
-- TcpListener and the typechecker can't catch the mismatch — the
-- error surfaces at runtime through `require_listener`.
fn id(x) { x }

fn main() {
  tcp.accept(id(123))
}
"#;
    let tokens = silt::lexer::Lexer::new(silt::source::FileId::default(), src)
        .tokenize()
        .expect("lex error");
    let mut program = silt::parser::Parser::new(tokens, src)
        .parse_program()
        .expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = silt::compiler::Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = silt::vm::Vm::new();
    let err = vm.run(script).expect_err("expected runtime error");
    let msg = format!("{err}");
    assert!(
        msg.contains("requires TcpListener"),
        "expected canonical `\"<fn> requires TcpListener\"` substring, got: {msg}"
    );
    assert!(
        msg.contains("got "),
        "expected canonical `\", got <kind>\"` tail naming the \
         offending kind, got: {msg}"
    );
    assert!(
        msg.contains("got Int"),
        "expected the offending kind to surface as `Int` (the laundered \
         value was `123: Int`), got: {msg}"
    );
}
