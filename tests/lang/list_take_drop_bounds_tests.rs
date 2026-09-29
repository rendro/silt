//! Regression tests for `list.take` / `list.drop` bounds-handling corner
//! cases, especially around `i64::MIN` / `i64::MAX` range endpoints.
//!
//! The original BROKEN case: `list.take(range, 0)` with `lo == i64::MIN`
//! returned the full range because the internal `lo + n - 1` computation
//! underflowed on `i64::MIN - 1` and the `None` branch fell back to the
//! entire range. The fix short-circuits `n <= 0` to an empty result.

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

// ── BROKEN regression: take(range, 0) at i64::MIN ──────────────────────
//
// Previously: `lo.checked_add(0).and_then(|v| v.checked_sub(1))` yielded
// `None` on `lo == i64::MIN`, then the code fell back to returning the
// whole range. Correct behavior: taking zero elements always yields an
// empty result.
#[test]
fn test_list_take_range_at_i64_min_zero_count_returns_empty() {
    let result = run(r#"
import list
fn main() -> Int {
  let lo = -9223372036854775807 - 1
  let r = lo..(lo + 8)
  let t = list.take(r, 0)
  list.length(t)
}
"#);
    assert_eq!(result, Value::Int(0));
}

// Non-zero count at i64::MIN must still produce the correct prefix:
// `list.take(lo..(lo+8), 3)` → `Range(lo, lo+2)` with 3 elements.
#[test]
fn test_list_take_range_at_i64_min_nonzero_count_ok() {
    let result = run(r#"
import list
fn main() -> Int {
  let lo = -9223372036854775807 - 1
  let r = lo..(lo + 8)
  let t = list.take(r, 3)
  list.length(t)
}
"#);
    assert_eq!(result, Value::Int(3));
}

// Sanity check: the list path (distinct code path from the range path)
// still returns empty on zero count.
#[test]
fn test_list_take_list_zero_count_returns_empty() {
    let result = run(r#"
import list
fn main() { list.take([1, 2, 3], 0) }
"#);
    assert_eq!(result, Value::List(Arc::new(vec![])));
}

// Oversized count on a small range should yield the full range (3
// elements) rather than panicking or returning something surprising.
// `5..8` in silt is inclusive on both ends → [5, 6, 7, 8]. Taking 100
// caps at the 4 elements in the range.
#[test]
fn test_list_take_range_oversized_count_returns_full() {
    let result = run(r#"
import list
fn main() -> Int {
  let r = 5..8
  list.length(list.take(r, 100))
}
"#);
    assert_eq!(result, Value::Int(4));
}

// `list.drop(range, 0)` at `lo == i64::MIN` must return the full range
// (9 elements for `lo..(lo+8)`). This is the symmetric corner case to
// the BROKEN take bug and is currently correct — lock it with a test.
#[test]
fn test_list_drop_range_at_i64_min_zero_count_returns_full() {
    let result = run(r#"
import list
fn main() -> Int {
  let lo = -9223372036854775807 - 1
  let r = lo..(lo + 8)
  let d = list.drop(r, 0)
  list.length(d)
}
"#);
    assert_eq!(result, Value::Int(9));
}

// Dropping more than the range contains yields an empty list, even at
// `lo == i64::MIN` (where `lo + n` could overflow).
#[test]
fn test_list_drop_range_at_i64_min_count_larger_than_range_returns_empty() {
    let result = run(r#"
import list
fn main() -> Int {
  let lo = -9223372036854775807 - 1
  let r = lo..(lo + 8)
  let d = list.drop(r, 100)
  list.length(d)
}
"#);
    assert_eq!(result, Value::Int(0));
}

// Negative count is rejected cleanly by `list.take` at the top of the
// builtin (before the range-specific path). Lock the existing semantic:
// a clean VmError, not silently-empty and not a panic.
#[test]
fn test_list_take_range_negative_count_returns_empty() {
    let err = run_err(
        r#"
import list
fn main() -> Int {
  let r = 1..10
  list.length(list.take(r, -1))
}
"#,
    );
    assert!(
        err.contains("list.take") && err.contains("negative"),
        "expected negative-index error from list.take, got: {err}"
    );
}
