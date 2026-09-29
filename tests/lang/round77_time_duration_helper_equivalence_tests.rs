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
//!
//! Axes 1 and 2 are golden cases in
//! tests/golden/lang/stdlib/round77_time_duration_helper_equivalence__*.silt.
//! Axis 3 stays here: `silt check` rejects a String argument, so the
//! runtime kind check is only reachable past the typechecker.

use std::sync::Arc;

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::vm::Vm;

// ── Helpers ─────────────────────────────────────────────────────────

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
