//! Regression tests for `string.pad_left` / `string.pad_right` width bounds.
//!
//! Round 15 audit — BROKEN: previously, passing a huge width (near i64::MAX)
//! caused `collect::<String>()` to invoke `alloc::handle_alloc_error → abort()`,
//! bypassing `catch_builtin_panic` and bringing down the VM host process.
//!
//! These tests drive the VM via the library API (never via `silt run`), so
//! the whole process cannot be aborted during `cargo test`.

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::value::Value;
use silt::vm::Vm;
use std::sync::Arc;

fn run(input: &str) -> Value {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    vm.run(script).expect("runtime error")
}

fn run_err(input: &str) -> String {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = match compiler.compile_program(&program) {
        Ok(f) => f,
        Err(e) => return e.message,
    };
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    let err = vm.run(script).expect_err("expected runtime error");
    format!("{err}")
}

// ── The main BROKEN repros ─────────────────────────────────────────
//
// Before the fix these two tests would abort the `cargo test` process
// with `memory allocation of ... bytes failed` instead of returning a
// clean VmError. They MUST NOT abort.

#[test]
fn test_string_pad_left_huge_width_rejected() {
    let err = run_err(
        r#"
import string
fn main() {
  string.pad_left("hi", 9223372036854775800, " ")
}
        "#,
    );
    assert!(
        err.contains("pad_left"),
        "error should mention pad_left, got: {err}"
    );
    assert!(
        err.contains("exceeds maximum"),
        "error should mention the cap phrasing, got: {err}"
    );
}

#[test]
fn test_string_pad_right_huge_width_rejected() {
    let err = run_err(
        r#"
import string
fn main() {
  string.pad_right("hi", 9223372036854775800, " ")
}
        "#,
    );
    assert!(
        err.contains("pad_right"),
        "error should mention pad_right, got: {err}"
    );
    assert!(
        err.contains("exceeds maximum"),
        "error should mention the cap phrasing, got: {err}"
    );
}

// ── Boundary probe ─────────────────────────────────────────────────
//
// MAX_RANGE_MATERIALIZE = 10_000_000. At exactly the cap, pad_left
// should accept the width and produce a 10M-char string. One past
// the cap should be rejected with a clean VmError.

#[test]
fn test_string_pad_left_at_cap_ok_or_rejected() {
    // At cap: allowed, returns a string whose char count equals the cap.
    let result = run(r#"
import string
fn main() {
  string.length(string.pad_left("hi", 10000000, " "))
}
        "#);
    assert_eq!(result, Value::Int(10_000_000));

    // One past cap: clean VmError, not an abort.
    let err = run_err(
        r#"
import string
fn main() {
  string.pad_left("hi", 10000001, " ")
}
        "#,
    );
    assert!(
        err.contains("pad_left") && err.contains("exceeds maximum"),
        "error should mention pad_left and the cap, got: {err}"
    );
}

// ── Positive baseline cases (make sure the fix didn't regress) ─────

#[test]
fn test_string_pad_left_normal_width_ok() {
    let result = run(r#"
import string
fn main() {
  string.pad_left("hi", 5, " ")
}
        "#);
    assert_eq!(result, Value::String("   hi".into()));
}

#[test]
fn test_string_pad_right_normal_width_ok() {
    let result = run(r#"
import string
fn main() {
  string.pad_right("hi", 5, " ")
}
        "#);
    assert_eq!(result, Value::String("hi   ".into()));
}

#[test]
fn test_string_pad_left_width_zero_ok() {
    // Width less than the input's char count: original string preserved.
    let result = run(r#"
import string
fn main() {
  string.pad_left("hello", 2, " ")
}
        "#);
    assert_eq!(result, Value::String("hello".into()));
}

#[test]
fn test_string_pad_left_multibyte_pad_char() {
    // Non-ASCII pad char (U+00E9 'é' is 2 bytes UTF-8).
    let result = run(r#"
import string
fn main() {
  string.pad_left("hi", 5, "é")
}
        "#);
    assert_eq!(result, Value::String("éééhi".into()));
}

// ── Extra: negative width still rejected cleanly ───────────────────

#[test]
fn test_string_pad_left_negative_width_rejected() {
    let err = run_err(
        r#"
import string
fn main() {
  string.pad_left("hi", -1, " ")
}
        "#,
    );
    assert!(
        err.contains("pad_left") && err.contains("negative"),
        "error should mention pad_left and negative, got: {err}"
    );
}

#[test]
fn test_string_pad_right_negative_width_rejected() {
    let err = run_err(
        r#"
import string
fn main() {
  string.pad_right("hi", -1, " ")
}
        "#,
    );
    assert!(
        err.contains("pad_right") && err.contains("negative"),
        "error should mention pad_right and negative, got: {err}"
    );
}
