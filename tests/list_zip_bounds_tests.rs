//! Regression tests for `list.zip` result-length bounds (L1).
//!
//! Before the fix, `list.zip` computed its result capacity via
//! `ValueIter::len()` which used `size_hint` saturating to `usize::MAX`
//! for huge ranges like `0..i64::MAX`. The subsequent
//! `Vec::with_capacity(usize::MAX)` then panicked opaquely, surfacing
//! as "builtin module 'list' panicked". The fix validates both input
//! lengths (via `checked_range_len` semantics for ranges) against
//! `MAX_RANGE_MATERIALIZE` and returns a clean `VmError` on overflow.
//!
//! These tests drive the VM via the library API so a regression
//! surfaces as a panic or test-runner abort rather than a noisy
//! OOM kill.

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

// ── Huge range pair ─────────────────────────────────────────────────
//
// 20_000_000 elements exceeds the 10_000_000-element
// `MAX_RANGE_MATERIALIZE` cap. Both inputs are ranges, so the expected
// length is min(20M, 20M) = 20M, which must be rejected with an exact
// phrase pin.

#[test]
fn test_list_zip_range_range_huge_rejected() {
    let err = run_err(
        r#"
import list
fn main() {
  list.zip(0..19999999, 0..19999999)
}
        "#,
    );
    assert!(
        err.contains(
            "list.zip: result length 20000000 exceeds maximum materialized length 10000000"
        ),
        "error should contain the exact cap phrasing, got: {err}"
    );
}

// ── Exactly at the cap: must pass ───────────────────────────────────
//
// `MAX_RANGE_MATERIALIZE` is 10_000_000. The inclusive range `0..9999999`
// yields exactly 10_000_000 elements. Zipping two such ranges must
// produce a 10_000_000-element list without hitting the cap.

#[test]
fn test_list_zip_range_range_at_cap_ok() {
    let result = run(r#"
import list
fn main() {
  list.length(list.zip(0..9999999, 0..9999999))
}
        "#);
    assert_eq!(result, Value::Int(10_000_000));
}

// ── Just over the cap: must be rejected with exact phrase ───────────
//
// The inclusive range `0..10000000` yields 10_000_001 elements, which
// is one over the cap.

#[test]
fn test_list_zip_range_range_just_over_cap_rejected() {
    let err = run_err(
        r#"
import list
fn main() {
  list.zip(0..10000000, 0..10000000)
}
        "#,
    );
    assert!(
        err.contains(
            "list.zip: result length 10000001 exceeds maximum materialized length 10000000"
        ),
        "error should contain the exact cap phrasing, got: {err}"
    );
}

// ── `i64::MAX` pair: must reject cleanly, not panic ─────────────────
//
// Before the fix, `Vec::with_capacity(usize::MAX)` panicked opaquely
// as "builtin module 'list' panicked". With the fix, the huge expected
// length is detected via `u128` arithmetic and rejected with a clean
// VmError.

#[test]
fn test_list_zip_i64_max_range_rejected() {
    let err = run_err(
        r#"
import list
fn main() {
  list.zip(0..9223372036854775807, 0..9223372036854775807)
}
        "#,
    );
    assert!(
        err.contains("list.zip: result length"),
        "error should mention list.zip: result length, got: {err}"
    );
    assert!(
        err.contains("exceeds maximum materialized length 10000000"),
        "error should contain the exact cap phrasing, got: {err}"
    );
    // Regression pin: the error must not be the opaque panic surface.
    assert!(
        !err.contains("panic"),
        "error must not be a panic surface, got: {err}"
    );
}

// ── Positive case: small ranges zip correctly ───────────────────────
//
// `list.zip(0..4, 10..14)` — both are 5-element inclusive ranges, so
// the result must be `[(0, 10), (1, 11), (2, 12), (3, 13), (4, 14)]`.

#[test]
fn test_list_zip_small_ranges_ok() {
    let result = run(r#"
import list
fn main() {
  list.zip(0..4, 10..14)
}
        "#);
    let expected = Value::List(Arc::new(vec![
        Value::Tuple(vec![Value::Int(0), Value::Int(10)]),
        Value::Tuple(vec![Value::Int(1), Value::Int(11)]),
        Value::Tuple(vec![Value::Int(2), Value::Int(12)]),
        Value::Tuple(vec![Value::Int(3), Value::Int(13)]),
        Value::Tuple(vec![Value::Int(4), Value::Int(14)]),
    ]));
    assert_eq!(result, expected);
}
