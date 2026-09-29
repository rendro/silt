//! Round 77 BLOAT-D2 lock test.
//!
//! `src/builtins/data.rs` previously contained six near-identical 12-line
//! arms for `time.hours`, `time.minutes`, `time.seconds`, `time.ms`,
//! `time.micros`, and `time.nanos`. Each block performed the same arity
//! check, the same `Value::Int` kind check, and the same checked-multiply
//! / overflow message — differing only in the multiplier and the unit
//! label. Round 77 collapsed the six bodies into a single
//! `duration_from_int(name, multiplier, args)` helper. Each arm is now a
//! one-liner.
//!
//! Per the audit-guide rule "dead-code fixes need a lock test proving the
//! deletion was semantically a no-op", the tests below pin the canonical
//! observable behaviour at every one of the six entry points along three
//! axes:
//!   1. **Normal value** — feed `5` and assert the produced
//!      `Duration { ns: 5 * multiplier }` record.
//!   2. **Overflow boundary** — feed `i64::MAX / multiplier + 1` (the
//!      smallest input that triggers `checked_mul` failure) and assert
//!      the diagnostic carries the exact pre-fix `"time arithmetic
//!      overflow: <name>(<n>) exceeds i64 nanoseconds"` template.
//!   3. **Kind mismatch** — feed a non-`Int` value (a string) and assert
//!      the diagnostic carries the exact pre-fix
//!      `"<name> requires Int, got String"` shape.
//!
//! For `time.nanos` the multiplier is `1`, so `checked_mul(1)` can never
//! overflow — there is no overflow boundary to exercise. The pre-refactor
//! `nanos` arm produced `Ok(make_duration(*n))` directly without any
//! checked-arithmetic step; the post-refactor helper produces the same
//! result via `n.checked_mul(1)`. The first and third axes are therefore
//! the load-bearing locks for that entry point.

use std::collections::BTreeMap;
use std::sync::Arc;

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::value::Value;
use silt::vm::Vm;

// ── Helpers ─────────────────────────────────────────────────────────

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

/// Materialise `Duration { ns: <ns> }` as a record value, mirroring the
/// `make_duration` helper in `src/builtins/data.rs`. Used as the
/// canonical "expected" shape for normal-input asserts.
fn duration_record(ns: i64) -> Value {
    let mut fields = BTreeMap::new();
    fields.insert("ns".to_string(), Value::Int(ns));
    Value::Record("Duration".into(), Arc::new(fields))
}

/// Run a tiny `time.<unit>(<n>)` program and return the resulting value.
fn call_unit(unit: &str, n: i64) -> Value {
    let src = format!(
        r#"
import time
fn main() = time.{unit}({n})
"#
    );
    run(&src)
}

/// Run a tiny `time.<unit>(<n>)` program and return the runtime error.
fn call_unit_err(unit: &str, n: i64) -> String {
    let src = format!(
        r#"
import time
fn main() = time.{unit}({n})
"#
    );
    run_err(&src)
}

/// Run a `time.<unit>("not an int")` program and return the runtime
/// error. Used to pin the kind-mismatch wording.
fn call_unit_kind_err(unit: &str) -> String {
    let src = format!(
        r#"
import time
fn main() = time.{unit}("not an int")
"#
    );
    run_err(&src)
}

// ── Normal-value lock (axis 1) ──────────────────────────────────────

/// `time.hours(5)` must produce `Duration {{ ns: 5 * 3_600_000_000_000 }}`,
/// matching the multiplier the pre-refactor arm used.
#[test]
fn time_hours_normal_value_produces_canonical_ns() {
    let got = call_unit("hours", 5);
    let expected = duration_record(5i64 * 3_600_000_000_000);
    assert_eq!(got, expected);
}

#[test]
fn time_minutes_normal_value_produces_canonical_ns() {
    let got = call_unit("minutes", 5);
    let expected = duration_record(5i64 * 60_000_000_000);
    assert_eq!(got, expected);
}

#[test]
fn time_seconds_normal_value_produces_canonical_ns() {
    let got = call_unit("seconds", 5);
    let expected = duration_record(5i64 * 1_000_000_000);
    assert_eq!(got, expected);
}

#[test]
fn time_ms_normal_value_produces_canonical_ns() {
    let got = call_unit("ms", 5);
    let expected = duration_record(5i64 * 1_000_000);
    assert_eq!(got, expected);
}

#[test]
fn time_micros_normal_value_produces_canonical_ns() {
    let got = call_unit("micros", 5);
    let expected = duration_record(5i64 * 1_000);
    assert_eq!(got, expected);
}

/// `time.nanos(5)` is the identity case (multiplier = 1). The
/// pre-refactor arm returned `Ok(make_duration(*n))` directly with no
/// multiplication; the post-refactor helper produces the same value via
/// `checked_mul(1)`.
#[test]
fn time_nanos_normal_value_produces_canonical_ns() {
    let got = call_unit("nanos", 5);
    let expected = duration_record(5);
    assert_eq!(got, expected);
}

// ── Overflow-boundary lock (axis 2) ─────────────────────────────────
//
// For each unit, the smallest input that triggers `checked_mul` failure
// is `i64::MAX / multiplier + 1`. The diagnostic carries the exact
// pre-fix template:
//   "time arithmetic overflow: time.<unit>(<n>) exceeds i64 nanoseconds"

fn assert_overflow_message(err: &str, unit: &str, n: i64) {
    let prefix = format!("time arithmetic overflow: time.{unit}({n}) exceeds i64 nanoseconds");
    assert!(
        err.contains(&prefix),
        "expected overflow message containing {prefix:?}, got: {err}"
    );
}

#[test]
fn time_hours_overflow_boundary_emits_canonical_message() {
    let mult = 3_600_000_000_000i64;
    let n = i64::MAX / mult + 1;
    let err = call_unit_err("hours", n);
    assert_overflow_message(&err, "hours", n);
}

#[test]
fn time_minutes_overflow_boundary_emits_canonical_message() {
    let mult = 60_000_000_000i64;
    let n = i64::MAX / mult + 1;
    let err = call_unit_err("minutes", n);
    assert_overflow_message(&err, "minutes", n);
}

#[test]
fn time_seconds_overflow_boundary_emits_canonical_message() {
    let mult = 1_000_000_000i64;
    let n = i64::MAX / mult + 1;
    let err = call_unit_err("seconds", n);
    assert_overflow_message(&err, "seconds", n);
}

#[test]
fn time_ms_overflow_boundary_emits_canonical_message() {
    let mult = 1_000_000i64;
    let n = i64::MAX / mult + 1;
    let err = call_unit_err("ms", n);
    assert_overflow_message(&err, "ms", n);
}

#[test]
fn time_micros_overflow_boundary_emits_canonical_message() {
    let mult = 1_000i64;
    let n = i64::MAX / mult + 1;
    let err = call_unit_err("micros", n);
    assert_overflow_message(&err, "micros", n);
}

// `time.nanos` has multiplier = 1, so `checked_mul(1)` cannot overflow
// for any `i64` input. The pre-refactor arm matched this — it never
// produced an overflow error. We pin that contract by asserting the
// extreme positive input `i64::MAX` round-trips without error, and the
// most-negative literal silt can lex (`i64::MIN + 1` — silt's lexer
// parses unary-minus + positive literal, and `-i64::MIN` overflows the
// positive-literal half) also round-trips. Together these confirm the
// multiplier-1 fast path was preserved by the refactor.
#[test]
fn time_nanos_extremes_never_overflow() {
    let max = call_unit("nanos", i64::MAX);
    assert_eq!(max, duration_record(i64::MAX));
    let near_min = call_unit("nanos", i64::MIN + 1);
    assert_eq!(near_min, duration_record(i64::MIN + 1));
}

// ── Kind-mismatch lock (axis 3) ─────────────────────────────────────
//
// Feed a non-`Int` value (a `String`) to each unit and assert the
// diagnostic carries the exact pre-fix `"<name> requires Int, got
// String"` shape. This pins the message wording the audit-round-75
// canonical-form lock also encodes — but here we exercise the runtime
// surface, not just the source string.

fn assert_kind_message(err: &str, unit: &str) {
    let needle = format!("time.{unit} requires Int, got String");
    assert!(
        err.contains(&needle),
        "expected kind-mismatch message containing {needle:?}, got: {err}"
    );
}

#[test]
fn time_hours_non_int_emits_canonical_kind_message() {
    let err = call_unit_kind_err("hours");
    assert_kind_message(&err, "hours");
}

#[test]
fn time_minutes_non_int_emits_canonical_kind_message() {
    let err = call_unit_kind_err("minutes");
    assert_kind_message(&err, "minutes");
}

#[test]
fn time_seconds_non_int_emits_canonical_kind_message() {
    let err = call_unit_kind_err("seconds");
    assert_kind_message(&err, "seconds");
}

#[test]
fn time_ms_non_int_emits_canonical_kind_message() {
    let err = call_unit_kind_err("ms");
    assert_kind_message(&err, "ms");
}

#[test]
fn time_micros_non_int_emits_canonical_kind_message() {
    let err = call_unit_kind_err("micros");
    assert_kind_message(&err, "micros");
}

#[test]
fn time_nanos_non_int_emits_canonical_kind_message() {
    let err = call_unit_kind_err("nanos");
    assert_kind_message(&err, "nanos");
}
