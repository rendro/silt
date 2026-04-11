//! Regression tests for `list.flat_map` range materialization bounds.
//!
//! Round 15 audit — BROKEN: previously, when a `list.flat_map` callback
//! returned a `Value::Range(lo, hi)`, the VM materialized it via
//! `for i in lo..=hi { v.push(Value::Int(i)) }` with no bound on the
//! range size. Every sibling range-materialization site
//! (`ListConcat`, `materialize_iter`, `list.flatten`, `value_to_json`,
//! `Value::materialize_range`, etc.) calls `checked_range_len` with
//! the `MAX_RANGE_MATERIALIZE` cap. This one did not, so a callback
//! returning `0..i64::MAX` would hang the process while RSS grew
//! without bound.
//!
//! These tests drive the VM via the library API (never via `silt run`)
//! so a regression surfaces as a clean `VmError` or a `#[should_panic]`
//! / timeout rather than hanging `cargo test`.

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

// ── The BROKEN repro ────────────────────────────────────────────────
//
// Before the fix this would loop inside `for i in lo..=hi { v.push(...) }`
// while allocations climbed without bound, eventually OOM-killing the
// test runner. With the fix, `apply_callback_result` now calls
// `checked_range_len` / enforces `MAX_RANGE_MATERIALIZE` and produces
// a clean VmError.
//
// We don't use `#[should_panic]` or a timeout attribute because a
// regression here would hang rather than panic. The guarantee we're
// after is "returns quickly with an error" — if this test ever hangs
// `cargo test`, the regression is obvious to any developer running
// the suite.

#[test]
fn test_list_flat_map_callback_range_huge_rejected() {
    let err = run_err(
        r#"
import list
fn main() {
  [1] |> list.flat_map { _ -> 0..9223372036854775807 }
}
        "#,
    );
    // Strong pin: must mention list.flat_map by name (the fix prefixes
    // the error with "list.flat_map:") and must mention the cap phrasing.
    // The old weak-OR fallback on "range" was too broad and would have
    // accepted any error text mentioning ranges at all.
    assert!(
        err.contains("list.flat_map") || err.contains("flat_map"),
        "error should mention flat_map by name, got: {err}"
    );
    assert!(
        err.contains("exceeds maximum") || err.contains("materializ"),
        "error should mention the cap phrasing, got: {err}"
    );
}

// ── Mutation lock: small-range positive path materializes correctly ─
//
// Duplicates the small-range coverage with extra mutation-verification
// assertions (exact length + element-wise probe of the first and last
// values in each row). Any mutation that turns the small-range branch
// into a no-op, drops the range, or off-by-ones the materialization
// loop will fail these checks.

#[test]
fn test_list_flat_map_callback_range_small_ok_mutation_verify() {
    let result = run(
        r#"
import list
fn main() {
  [10, 20] |> list.flat_map { _ -> 1..4 }
}
        "#,
    );
    // Silt ranges inclusive on both ends: 1..4 is [1, 2, 3, 4].
    // Two input items -> 2 * 4 = 8 integers, each row [1, 2, 3, 4].
    let expected = Value::List(Arc::new(vec![
        Value::Int(1),
        Value::Int(2),
        Value::Int(3),
        Value::Int(4),
        Value::Int(1),
        Value::Int(2),
        Value::Int(3),
        Value::Int(4),
    ]));
    assert_eq!(result, expected);
    if let Value::List(xs) = &result {
        assert_eq!(xs.len(), 8, "expected exactly 8 elements, got {}", xs.len());
        assert_eq!(xs[0], Value::Int(1), "first element wrong: {:?}", xs[0]);
        assert_eq!(xs[3], Value::Int(4), "end of first row wrong: {:?}", xs[3]);
        assert_eq!(xs[4], Value::Int(1), "start of second row wrong: {:?}", xs[4]);
        assert_eq!(xs[7], Value::Int(4), "final element wrong: {:?}", xs[7]);
    } else {
        panic!("expected a list, got {:?}", result);
    }
}

// ── Positive case: small range, multiple input items ────────────────
//
// Silt ranges are inclusive on both ends: `0..3` is `[0, 1, 2, 3]`
// (4 elements). For `[1, 2] |> list.flat_map { _ -> 0..3 }` we
// therefore expect 2 * 4 = 8 integers, each row `[0, 1, 2, 3]`.

#[test]
fn test_list_flat_map_callback_range_small_ok() {
    let result = run(
        r#"
import list
fn main() {
  [1, 2] |> list.flat_map { _ -> 0..3 }
}
        "#,
    );
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::Int(0),
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(0),
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
        ]))
    );
}

// ── Empty range (hi < lo) yields nothing ────────────────────────────
//
// `checked_range_len` treats `lo > hi` as length 0, and `collection_len`
// on `Range(lo, hi)` with `hi < lo` returns `Some(0)`. A flat_map over
// a single item whose callback returns such a range should therefore
// produce an empty list.

#[test]
fn test_list_flat_map_callback_empty_range_ok() {
    let result = run(
        r#"
import list
fn main() {
  let lo = 5
  let hi = 4
  [1] |> list.flat_map { _ -> lo..hi }
}
        "#,
    );
    assert_eq!(result, Value::List(Arc::new(vec![])));
}

// ── Non-range callback result: behaviour unchanged ──────────────────
//
// The fix must not alter how callback results other than `Value::Range`
// are handled. `[1, 2] |> flat_map { x -> [x, x] }` must still produce
// `[1, 1, 2, 2]`.

#[test]
fn test_list_flat_map_callback_nonrange_unchanged() {
    let result = run(
        r#"
import list
fn main() {
  [1, 2] |> list.flat_map { x -> [x, x] }
}
        "#,
    );
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::Int(1),
            Value::Int(1),
            Value::Int(2),
            Value::Int(2),
        ]))
    );
}

// ── Boundary probe: near-cap range rejected cleanly ─────────────────
//
// `MAX_RANGE_MATERIALIZE` is 10_000_000. A single flat_map step
// returning `0..10_000_001` (10_000_002 elements) must be rejected
// with a clean VmError rather than materialized.

#[test]
fn test_list_flat_map_callback_range_just_over_cap_rejected() {
    let err = run_err(
        r#"
import list
fn main() {
  [1] |> list.flat_map { _ -> 0..10000001 }
}
        "#,
    );
    assert!(
        err.contains("exceeds maximum") || err.contains("materializ"),
        "error should mention the cap phrasing, got: {err}"
    );
}

// ── Accumulated overflow across multiple items ──────────────────────
//
// Even when an individual callback range fits within the cap, the
// accumulated flat_map output must not exceed `MAX_RANGE_MATERIALIZE`.
// Two items, each producing a 6M-element range, would total 12M and
// must be rejected.

#[test]
fn test_list_flat_map_accumulated_over_cap_rejected() {
    let err = run_err(
        r#"
import list
fn main() {
  [1, 2] |> list.flat_map { _ -> 0..5999999 }
}
        "#,
    );
    assert!(
        err.contains("exceeds maximum") || err.contains("materializ"),
        "error should mention the cap phrasing, got: {err}"
    );
}
