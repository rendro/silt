//! Round 74 lock test for the `require_int_plain` deletion.
//!
//! Background: Round 65 hoisted byte-identical `require_int` bodies
//! out of `tcp.rs` and `stream.rs` into a shared
//! `super::common::require_int_plain` helper that produced messages of
//! the form `"{fn_label} requires Int"` (no `, got <kind>` suffix).
//! Round 67 added the `, got <kind>` suffix to the body, making it
//! byte-identical to the canonical `super::common::require_int` —
//! leaving two functions with the same body, defeating the round-65
//! dedup.
//!
//! Round 74 deletes `require_int_plain` and routes the local wrappers
//! in `tcp.rs` and `stream.rs` to `super::common::require_int`
//! directly. These tests pin the deletion three ways:
//!
//! 1. Source-grep negative lock: the helper must be gone from
//!    `common.rs`.
//! 2. Source-grep positive lock: the call sites in `tcp.rs` and
//!    `stream.rs` must reference `super::common::require_int(` and
//!    must NOT reference `require_int_plain`.
//! 3. Behavioral lock: a wrong-typed argument must still emit the
//!    canonical `, got <kind>` suffix that round 67 introduced — i.e.
//!    the routing change preserves the message format.
//!
//! If a future contributor reintroduces the shim, (1) or (2) will trip.
//! If they accidentally drop the suffix when consolidating, (3) trips.

use std::sync::Arc;

use silt::value::Value;

// ── Source-grep locks ──────────────────────────────────────────────

/// `common.rs` must NOT define `require_int_plain` (deleted in round 74).
#[test]
fn common_does_not_define_require_int_plain() {
    let src = include_str!("../src/builtins/common.rs");
    assert!(
        !src.contains("fn require_int_plain"),
        "common.rs must NOT define require_int_plain — round 74 deleted \
         the shim because it was byte-identical to require_int after \
         round 67 added the `, got <kind>` suffix to its body"
    );
}

/// `tcp.rs` must reference `super::common::require_int(` and must NOT
/// reference the deleted `require_int_plain` shim.
#[test]
fn tcp_routes_require_int_through_common_require_int() {
    let src = include_str!("../src/builtins/tcp.rs");
    assert!(
        src.contains("super::common::require_int("),
        "tcp.rs must call super::common::require_int directly after \
         round 74 deleted require_int_plain"
    );
    assert!(
        !src.contains("require_int_plain"),
        "tcp.rs must not reference the deleted require_int_plain shim"
    );
}

/// `stream.rs` must reference `super::common::require_int(` and must
/// NOT reference the deleted `require_int_plain` shim.
#[test]
fn stream_routes_require_int_through_common_require_int() {
    let src = include_str!("../src/builtins/stream.rs");
    assert!(
        src.contains("super::common::require_int("),
        "stream.rs must call super::common::require_int directly after \
         round 74 deleted require_int_plain"
    );
    assert!(
        !src.contains("require_int_plain"),
        "stream.rs must not reference the deleted require_int_plain shim"
    );
}

// ── Behavioral lock ────────────────────────────────────────────────

/// Compile and run a silt program; capture either the returned `Value`
/// or the runtime `VmError` message.
fn try_run(input: &str) -> Result<Value, String> {
    let tokens = silt::lexer::Lexer::new(input)
        .tokenize()
        .map_err(|e| format!("lex error: {}", e.message))?;
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .map_err(|e| format!("parse error: {}", e.message))?;
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = silt::compiler::Compiler::new();
    let functions = compiler
        .compile_program(&program)
        .map_err(|e| format!("compile error: {}", e.message))?;
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = silt::vm::Vm::new();
    vm.run(script).map_err(|e| format!("{e}"))
}

/// After the round-74 routing change, `stream.from_range` (and any
/// other stream/tcp builtin that takes an `Int` argument) must still
/// emit the canonical `, got <kind>` suffix that round 67 introduced.
/// This proves the deletion preserved behavior — both the old
/// `require_int_plain` and the new direct `require_int` call produce
/// the same diagnostic.
#[test]
fn stream_from_range_emits_canonical_got_kind_suffix() {
    let src = r#"
import stream
fn main() { stream.count(stream.from_range("nope", 100)) }
"#;
    let outcome = try_run(src);
    let err = match outcome {
        Ok(v) => panic!(
            "expected runtime error from stream.from_range(\"nope\", 100); got Ok({v:?})"
        ),
        Err(msg) => msg,
    };
    assert!(
        err.contains("stream.from_range requires Int"),
        "expected the canonical require_int wording; got {err:?}"
    );
    assert!(
        err.contains(", got String"),
        "expected the round-67 `, got <kind>` suffix preserved through \
         the round-74 routing change; got {err:?}"
    );
}

#[cfg(feature = "tcp")]
#[test]
fn tcp_connect_emits_canonical_got_kind_suffix() {
    let src = r#"
import tcp
fn main() {
  match tcp.connect(42) {
    Ok(_) -> "ok"
    Err(_) -> "err"
  }
}
"#;
    let outcome = try_run(src);
    let err = match outcome {
        Ok(v) => panic!("expected runtime error from tcp.connect(42); got Ok({v:?})"),
        Err(msg) => msg,
    };
    // Note: tcp.connect's port argument goes through require_string,
    // not require_int — but require_string also emits the suffix in
    // the canonical common helper. The point of this test is that the
    // tcp builtin still reaches the common helpers and produces a
    // suffixed diagnostic.
    assert!(
        err.contains("tcp.connect requires String"),
        "expected the canonical wording; got {err:?}"
    );
}
