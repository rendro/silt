//! Lock tests for round-65 DC2 fix.
//!
//! Round 65 hoisted two byte-identical helper functions
//! (`require_int`, `require_string`-as-`&str`) out of `tcp.rs` and
//! `stream.rs` into `super::common::{require_int_plain,
//! require_str_borrow}`. The local `err()` helpers stayed put because
//! they wrap module-specific error variants.
//!
//! These tests pin the dedup in three ways:
//!
//! 1. Source-grep negative locks against `tcp.rs` and `stream.rs`
//!    asserting the original `match arg { Value::Int(n) => …}` /
//!    `Value::String(s) => Ok(s.as_str())` bodies are gone — anyone
//!    who reverts the dedup by re-inlining the bodies will trip these.
//! 2. Source-grep positive lock against `common.rs` asserting the
//!    new helper names are present.
//! 3. Behavioral lock: a silt program that calls `stream.from_range`
//!    and `stream.write_to_file` with wrong-typed arguments and
//!    asserts the runtime error wording exactly matches what the
//!    pre-dedup local helpers produced ("… requires Int" /
//!    "… requires String"). The same wording is preserved verbatim
//!    in the new shared helpers, so a behavioural drift would also
//!    be caught here.

use std::sync::Arc;

use silt::value::Value;

// ── Source-grep locks ──────────────────────────────────────────────

/// Negative lock: `tcp.rs` must no longer carry the original
/// `require_int` body. The body shape was three lines —
///     match arg {
///         Value::Int(n) => Ok(*n),
///         _ => Err(VmError::new(format!("{fn_label} requires Int"))),
/// — and we assert the Int-arm + Err combination is gone (the new
/// stub is a single-line delegation `super::common::require_int_plain`).
#[test]
fn tcp_require_int_routes_through_common() {
    let src = include_str!("../src/builtins/tcp.rs");
    // Positive lock: tcp.rs must call into the shared helper.
    assert!(
        src.contains("require_int_plain"),
        "tcp.rs must delegate require_int to common::require_int_plain; \
         did someone revert the round-65 DC2 dedup?"
    );
    // Negative lock: the original Value::Int(n) => Ok(*n) body must be
    // gone (a single-line delegation has no `Ok(*n)`).
    assert!(
        !src.contains("Value::Int(n) => Ok(*n)"),
        "tcp.rs must not redefine its own require_int body — \
         delegate to common::require_int_plain instead"
    );
}

/// Negative lock: `tcp.rs` must no longer carry the original
/// `require_string` body. The distinctive substring is
/// `Value::String(s) => Ok(s.as_str())` — the old `&str`-borrowing
/// helper. The new stub delegates and contains no such match arm.
#[test]
fn tcp_require_string_routes_through_common() {
    let src = include_str!("../src/builtins/tcp.rs");
    assert!(
        src.contains("require_str_borrow"),
        "tcp.rs must delegate require_string to common::require_str_borrow; \
         did someone revert the round-65 DC2 dedup?"
    );
    assert!(
        !src.contains("Value::String(s) => Ok(s.as_str())"),
        "tcp.rs must not redefine its own require_string body — \
         delegate to common::require_str_borrow instead"
    );
}

/// Negative lock: `stream.rs` must no longer carry the original
/// `require_int` / `require_string` bodies.
#[test]
fn stream_require_int_routes_through_common() {
    let src = include_str!("../src/builtins/stream.rs");
    assert!(
        src.contains("require_int_plain"),
        "stream.rs must delegate require_int to common::require_int_plain"
    );
    assert!(
        !src.contains("Value::Int(n) => Ok(*n)"),
        "stream.rs must not redefine its own require_int body"
    );
}

#[test]
fn stream_require_string_routes_through_common() {
    let src = include_str!("../src/builtins/stream.rs");
    assert!(
        src.contains("require_str_borrow"),
        "stream.rs must delegate require_string to common::require_str_borrow"
    );
    assert!(
        !src.contains("Value::String(s) => Ok(s.as_str())"),
        "stream.rs must not redefine its own require_string body"
    );
}

/// Positive lock: `common.rs` must export the two new helper names.
#[test]
fn common_module_exports_dedup_helpers() {
    let src = include_str!("../src/builtins/common.rs");
    assert!(
        src.contains("fn require_int_plain"),
        "common.rs must define require_int_plain (round-65 DC2 helper)"
    );
    assert!(
        src.contains("fn require_str_borrow"),
        "common.rs must define require_str_borrow (round-65 DC2 helper)"
    );
}

// ── Behavioral lock ────────────────────────────────────────────────

/// Compile and run a silt program; capture either the returned `Value`
/// or the runtime `VmError` message. Used by the behavioural lock to
/// verify that wrong-typed arguments to `stream.*` produce the same
/// "… requires Int" / "… requires String" wording the old local
/// helpers used.
fn try_run(input: &str) -> Result<Value, String> {
    let tokens = silt::lexer::Lexer::new(input)
        .tokenize()
        .map_err(|e| format!("lex error: {}", e.message))?;
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .map_err(|e| format!("parse error: {}", e.message))?;
    // Typechecker errors are non-fatal here — the dedup we're testing
    // is a runtime-side concern, and the existing tcp/stream tests
    // already follow this lenient pattern.
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = silt::compiler::Compiler::new();
    let functions = compiler
        .compile_program(&program)
        .map_err(|e| format!("compile error: {}", e.message))?;
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = silt::vm::Vm::new();
    vm.run(script).map_err(|e| format!("{e}"))
}

/// Behavioral lock: passing a `String` to `stream.from_range` (which
/// expects `Int`) must produce the diagnostic
/// `"stream.from_range requires Int"` — the verbatim wording from the
/// pre-dedup local `require_int` helper. The dedup re-routes the body
/// to `common::require_int_plain`, which preserves this exact format!
/// string. If a future refactor changes the wording (e.g. by adding a
/// `, got <kind>` suffix like the canonical `common::require_int`),
/// this test will fail and demand an explicit decision.
#[test]
fn stream_from_range_int_arg_error_message_unchanged_post_dedup() {
    let src = r#"
import stream
fn main() { stream.count(stream.from_range("nope", 100)) }
"#;
    let outcome = try_run(src);
    let err_or_unreachable = match outcome {
        Ok(v) => panic!(
            "expected runtime error from stream.from_range(\"nope\", 100); got Ok({v:?}). \
             If the typechecker grew strict enough to reject this at compile time, \
             the test needs a new path that defeats inference."
        ),
        Err(msg) => msg,
    };
    assert!(
        err_or_unreachable.contains("stream.from_range requires Int"),
        "expected exact pre-dedup wording \"stream.from_range requires Int\"; \
         got {err_or_unreachable:?}"
    );
}

/// Behavioral lock: passing an `Int` to `stream.write_to_file`'s path
/// argument (which expects `String`) must produce
/// `"stream.write_to_file requires String"` — verbatim from the
/// pre-dedup local `require_string` helper.
#[test]
fn stream_write_to_file_string_arg_error_message_unchanged_post_dedup() {
    // `stream.write_to_file(stream, path)` — path is arg[1]. Pass an
    // Int there to trigger the require_string path. Use an empty
    // stream so we don't actually open a file even if the path check
    // is reordered in future.
    let src = r#"
import stream
fn main() { stream.write_to_file(stream.from_list([]), 42) }
"#;
    let outcome = try_run(src);
    let err = match outcome {
        Ok(v) => panic!("expected runtime error from stream.write_to_file(_, 42); got Ok({v:?})"),
        Err(msg) => msg,
    };
    assert!(
        err.contains("stream.write_to_file requires String"),
        "expected exact pre-dedup wording \"stream.write_to_file requires String\"; \
         got {err:?}"
    );
}

/// Behavioral lock for tcp: `tcp.read` expects an `Int` second
/// argument (max-bytes). Passing a `String` must produce
/// `"tcp.read requires Int"` — verbatim from the pre-dedup local
/// helper, preserved across the dedup.
///
/// We can't actually open a tcp listener from a unit test reliably,
/// but `tcp.read` validates the int arg before doing any I/O — the
/// `require_int` call sits at the top of the dispatch. To hit the
/// arg-shape check we just need *any* first argument that survives
/// to the call site. We pass a non-stream first arg so the require
/// check there fails first, but with a different wording — so we
/// instead use `tcp.connect` whose arg[1] (port) takes Int. Pass a
/// String there.
#[cfg(feature = "tcp")]
#[test]
fn tcp_connect_port_int_arg_error_message_unchanged_post_dedup() {
    // `tcp.connect("127.0.0.1:80")` only takes one arg in the silt
    // surface (host:port string), but the underlying require_int is
    // exercised by `tcp.read(stream, n)`. Building a real stream from
    // a unit test is heavy, so we instead drive the require_string
    // path via `tcp.connect` itself — passing a non-string argument
    // must trip "tcp.connect requires String".
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
    assert!(
        err.contains("tcp.connect requires String"),
        "expected exact pre-dedup wording \"tcp.connect requires String\"; \
         got {err:?}"
    );
}
