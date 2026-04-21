//! Tests for the new `list` stdlib operations added in this change:
//! `index_of`, `remove_at`, `min_by`, `max_by`, `sum`, `sum_float`,
//! `product`, `product_float`, `scan`, and `intersperse`.
//!
//! Scan convention: the result length is `length(xs) + 1` and the
//! initial accumulator appears at the head. This matches Haskell's
//! `scanl`.

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

fn some_int(n: i64) -> Value {
    Value::Variant("Some".into(), vec![Value::Int(n)])
}

fn some_str(s: &str) -> Value {
    Value::Variant("Some".into(), vec![Value::String(s.to_string())])
}

fn none() -> Value {
    Value::Variant("None".into(), Vec::new())
}

fn int_list(xs: &[i64]) -> Value {
    Value::List(Arc::new(xs.iter().copied().map(Value::Int).collect()))
}

// ── list.index_of ──────────────────────────────────────────────────

#[test]
fn test_index_of_found_first() {
    let result = run(r#"
import list
fn main() { list.index_of([10, 20, 30, 20], 20) }
"#);
    assert_eq!(result, some_int(1));
}

#[test]
fn test_index_of_not_found() {
    let result = run(r#"
import list
fn main() { list.index_of([10, 20, 30], 99) }
"#);
    assert_eq!(result, none());
}

#[test]
fn test_index_of_empty() {
    let result = run(r#"
import list
fn main() { list.index_of([], 1) }
"#);
    assert_eq!(result, none());
}

// ── list.remove_at ─────────────────────────────────────────────────

#[test]
fn test_remove_at_middle() {
    let result = run(r#"
import list
fn main() { list.remove_at([10, 20, 30, 40], 1) }
"#);
    assert_eq!(result, int_list(&[10, 30, 40]));
}

#[test]
fn test_remove_at_first() {
    let result = run(r#"
import list
fn main() { list.remove_at([1, 2, 3], 0) }
"#);
    assert_eq!(result, int_list(&[2, 3]));
}

#[test]
fn test_remove_at_last() {
    let result = run(r#"
import list
fn main() { list.remove_at([1, 2, 3], 2) }
"#);
    assert_eq!(result, int_list(&[1, 2]));
}

#[test]
fn test_remove_at_out_of_bounds_errors() {
    let err = run_err(
        r#"
import list
fn main() { list.remove_at([1, 2], 5) }
"#,
    );
    assert!(
        err.contains("list.remove_at") && err.contains("out of bounds"),
        "expected out-of-bounds error, got: {err}"
    );
}

#[test]
fn test_remove_at_negative_errors() {
    let err = run_err(
        r#"
import list
fn main() { list.remove_at([1, 2], -1) }
"#,
    );
    assert!(
        err.contains("list.remove_at") && err.contains("negative"),
        "expected negative-index error, got: {err}"
    );
}

// ── list.min_by / list.max_by ──────────────────────────────────────

#[test]
fn test_min_by_shortest_word() {
    let result = run(r#"
import list
import string
fn main() {
  list.min_by(["banana", "fig", "apple"]) { w -> string.length(w) }
}
"#);
    assert_eq!(result, some_str("fig"));
}

#[test]
fn test_max_by_longest_word() {
    let result = run(r#"
import list
import string
fn main() {
  list.max_by(["banana", "fig", "apple"]) { w -> string.length(w) }
}
"#);
    assert_eq!(result, some_str("banana"));
}

#[test]
fn test_min_by_empty_is_none() {
    let result = run(r#"
import list
fn main() { list.min_by([]) { x -> x } }
"#);
    assert_eq!(result, none());
}

#[test]
fn test_max_by_empty_is_none() {
    let result = run(r#"
import list
fn main() { list.max_by([]) { x -> x } }
"#);
    assert_eq!(result, none());
}

#[test]
fn test_min_by_ties_return_first() {
    let result = run(r#"
import list
fn main() {
  -- 10 and 20 both map to 0 under x % 2; first one wins.
  list.min_by([10, 20, 30, 40]) { x -> x % 2 }
}
"#);
    assert_eq!(result, some_int(10));
}

// ── list.sum / list.product ────────────────────────────────────────

#[test]
fn test_sum_basic() {
    let result = run(r#"
import list
fn main() { list.sum([1, 2, 3, 4]) }
"#);
    assert_eq!(result, Value::Int(10));
}

#[test]
fn test_sum_empty_is_zero() {
    let result = run(r#"
import list
fn main() { list.sum([]) }
"#);
    assert_eq!(result, Value::Int(0));
}

#[test]
fn test_sum_float_basic() {
    let result = run(r#"
import list
fn main() { list.sum_float([0.5, 1.5, 2.0]) }
"#);
    assert_eq!(result, Value::Float(4.0));
}

#[test]
fn test_sum_float_empty_is_zero() {
    let result = run(r#"
import list
fn main() { list.sum_float([]) }
"#);
    assert_eq!(result, Value::Float(0.0));
}

#[test]
fn test_product_basic() {
    let result = run(r#"
import list
fn main() { list.product([1, 2, 3, 4]) }
"#);
    assert_eq!(result, Value::Int(24));
}

#[test]
fn test_product_empty_is_one() {
    let result = run(r#"
import list
fn main() { list.product([]) }
"#);
    assert_eq!(result, Value::Int(1));
}

#[test]
fn test_product_float_basic() {
    let result = run(r#"
import list
fn main() { list.product_float([1.5, 2.0, 4.0]) }
"#);
    assert_eq!(result, Value::Float(12.0));
}

#[test]
fn test_product_float_empty_is_one() {
    let result = run(r#"
import list
fn main() { list.product_float([]) }
"#);
    assert_eq!(result, Value::Float(1.0));
}

// Overflow is surfaced as a clean runtime error (not a panic).
#[test]
fn test_sum_overflow_errors() {
    let err = run_err(
        r#"
import list
fn main() {
  let big = 9223372036854775807  -- i64::MAX
  list.sum([big, 1])
}
"#,
    );
    assert!(
        err.contains("list.sum") && err.contains("overflow"),
        "expected overflow error, got: {err}"
    );
}

// ── list.scan ──────────────────────────────────────────────────────
//
// Convention: result length is N+1, with `init` at the head.

#[test]
fn test_scan_prefix_sums() {
    let result = run(r#"
import list
fn main() {
  list.scan([1, 2, 3, 4], 0) { acc, x -> acc + x }
}
"#);
    // [0, 0+1, 0+1+2, 0+1+2+3, 0+1+2+3+4] = [0, 1, 3, 6, 10]
    assert_eq!(result, int_list(&[0, 1, 3, 6, 10]));
}

#[test]
fn test_scan_empty_returns_just_init() {
    let result = run(r#"
import list
fn main() {
  list.scan([], 42) { acc, x -> acc + x }
}
"#);
    assert_eq!(result, int_list(&[42]));
}

#[test]
fn test_scan_length_is_n_plus_1() {
    let result = run(r#"
import list
fn main() -> Int {
  let s = list.scan([10, 20, 30], 0) { acc, x -> acc + x }
  list.length(s)
}
"#);
    assert_eq!(result, Value::Int(4));
}

// ── list.intersperse ───────────────────────────────────────────────

#[test]
fn test_intersperse_basic() {
    let result = run(r#"
import list
fn main() { list.intersperse([1, 2, 3], 0) }
"#);
    assert_eq!(result, int_list(&[1, 0, 2, 0, 3]));
}

#[test]
fn test_intersperse_empty_unchanged() {
    let result = run(r#"
import list
fn main() { list.intersperse([], 0) }
"#);
    assert_eq!(result, int_list(&[]));
}

#[test]
fn test_intersperse_single_unchanged() {
    let result = run(r#"
import list
fn main() { list.intersperse([42], 0) }
"#);
    assert_eq!(result, int_list(&[42]));
}

#[test]
fn test_intersperse_two_elements() {
    let result = run(r#"
import list
fn main() { list.intersperse([1, 2], 99) }
"#);
    assert_eq!(result, int_list(&[1, 99, 2]));
}
