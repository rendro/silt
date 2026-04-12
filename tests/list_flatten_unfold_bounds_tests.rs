//! Regression tests for `list.flatten` and `list.unfold` accumulation caps.
//!
//! Round 19 audit -- LATENT: both `list.flatten` and `list.unfold` could
//! accumulate unbounded results without ever checking `MAX_RANGE_MATERIALIZE`.
//! Every other collection-building builtin caps output at 10,000,000 elements.
//! These tests verify that both functions now reject over-cap results with a
//! clean `VmError` and that small inputs still work correctly.

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

// ── list.flatten ───────────────────────────────────────────────────────

/// Flatten a list of sub-ranges whose combined length exceeds the cap.
/// Two ranges of 6,000,000 elements each = 12,000,000 total > 10,000,000.
#[test]
fn test_list_flatten_accumulated_over_cap_rejected() {
    let err = run_err(
        r#"
import list
fn main() {
  list.flatten([0..5999999, 0..5999999])
}
        "#,
    );
    assert!(
        err.contains("list.flatten"),
        "error should mention list.flatten by name, got: {err}"
    );
    assert!(
        err.contains("exceeds maximum list length"),
        "error should mention exceeds maximum list length, got: {err}"
    );
}

/// Flatten a small list of sub-lists -- must still work correctly.
#[test]
fn test_list_flatten_small_ok() {
    let result = run(
        r#"
import list
fn main() {
  list.flatten([[1, 2], [3, 4], [5]])
}
        "#,
    );
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4),
            Value::Int(5),
        ]))
    );
}

// ── list.unfold ────────────────────────────────────────────────────────

/// Unfold that would generate more than MAX_RANGE_MATERIALIZE elements.
/// The callback never returns None, so without a cap it would loop forever.
/// With the cap it should error after 10,000,001 elements.
#[test]
fn test_list_unfold_over_cap_rejected() {
    let err = run_err(
        r#"
import list
fn main() {
  list.unfold(0) { n -> Some((n, n + 1)) }
}
        "#,
    );
    assert!(
        err.contains("list.unfold"),
        "error should mention list.unfold by name, got: {err}"
    );
    assert!(
        err.contains("exceeds maximum list length"),
        "error should mention exceeds maximum list length, got: {err}"
    );
}

/// Unfold that generates a small list -- must still work correctly.
#[test]
fn test_list_unfold_small_ok() {
    let result = run(
        r#"
import list
fn main() {
  list.unfold(1) { n ->
    match n > 5 {
      true -> None
      _ -> Some((n, n + 1))
    }
  }
}
        "#,
    );
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4),
            Value::Int(5),
        ]))
    );
}
