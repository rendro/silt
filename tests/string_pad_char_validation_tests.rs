//! Regression tests for `string.pad_left` / `string.pad_right` pad-string
//! validation.
//!
//! Round 60 audit — LATENT L2: previously, the pad argument was silently
//! coerced. An empty pad `""` was substituted with a space, and a multi-char
//! pad like `"ab"` silently kept only the first char. Both shapes now error
//! cleanly, matching silt's strict-validation discipline elsewhere in the
//! string builtins.

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

// ── Rejection cases ────────────────────────────────────────────────

#[test]
fn pad_left_rejects_empty_pad() {
    let err = run_err(
        r#"
import string
fn main() {
  string.pad_left("x", 5, "")
}
        "#,
    );
    assert!(
        err.contains("pad_left") && err.contains("non-empty") && err.contains("1-character"),
        "error should mention pad_left, non-empty, and 1-character; got: {err}"
    );
}

#[test]
fn pad_left_rejects_multi_char_pad() {
    let err = run_err(
        r#"
import string
fn main() {
  string.pad_left("x", 5, "ab")
}
        "#,
    );
    assert!(
        err.contains("pad_left") && err.contains("1-character") && err.contains("\"ab\""),
        "error should mention pad_left, 1-character, and the offending pad; got: {err}"
    );
    assert!(
        err.contains("2 characters"),
        "error should report the actual character count; got: {err}"
    );
}

#[test]
fn pad_right_rejects_empty_pad() {
    let err = run_err(
        r#"
import string
fn main() {
  string.pad_right("x", 5, "")
}
        "#,
    );
    assert!(
        err.contains("pad_right") && err.contains("non-empty") && err.contains("1-character"),
        "error should mention pad_right, non-empty, and 1-character; got: {err}"
    );
}

#[test]
fn pad_right_rejects_multi_char_pad() {
    let err = run_err(
        r#"
import string
fn main() {
  string.pad_right("x", 5, "ab")
}
        "#,
    );
    assert!(
        err.contains("pad_right") && err.contains("1-character") && err.contains("\"ab\""),
        "error should mention pad_right, 1-character, and the offending pad; got: {err}"
    );
    assert!(
        err.contains("2 characters"),
        "error should report the actual character count; got: {err}"
    );
}

// ── Positive guards (don't overreach) ──────────────────────────────

#[test]
fn pad_left_accepts_single_char_pad() {
    let result = run(r#"
import string
fn main() {
  string.pad_left("42", 5, "0")
}
        "#);
    assert_eq!(result, Value::String("00042".into()));
}

#[test]
fn pad_right_accepts_single_char_pad() {
    let result = run(r#"
import string
fn main() {
  string.pad_right("hi", 5, ".")
}
        "#);
    assert_eq!(result, Value::String("hi...".into()));
}

// Multibyte single-char pad should still be accepted (UTF-8 length != char count).
#[test]
fn pad_left_accepts_multibyte_single_char_pad() {
    let result = run(r#"
import string
fn main() {
  string.pad_left("hi", 5, "é")
}
        "#);
    assert_eq!(result, Value::String("éééhi".into()));
}
