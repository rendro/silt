//! Round 15 audit locks for `time.to_datetime` / `time.to_utc` /
//! `time.to_instant`.
//!
//! Two findings:
//!
//! * **B3 (BROKEN)** — `time.to_datetime` and `time.to_utc` computed the
//!   subsecond nanosecond remainder with `(epoch_ns % 1_000_000_000) as u32`.
//!   Rust's signed `%` returns a negative remainder for negative dividends
//!   whose magnitude isn't a multiple of 1e9. `as u32` then wraps that
//!   negative value into a huge `u32`, which chrono rejects as
//!   `nsec >= 2_000_000_000`, surfacing as "instant out of range" even
//!   though the instant is perfectly valid.
//!
//! * **L1 (LATENT)** — `to_datetime` computes `utc_dt + offset` and
//!   `to_instant` computes `dt - offset`. chrono's `NaiveDateTime +/- Duration`
//!   panics on overflow. Round 13's `try_minutes` fix guards against
//!   absurd offset magnitudes, but a perfectly valid offset can still
//!   push a near-extremal `NaiveDateTime` beyond chrono's ±262_143-year
//!   range and panic. The fix uses `checked_add_signed` /
//!   `checked_sub_signed` and maps `None` to a clean `VmError`.
//!
//! These tests lock both fixes.

#![allow(clippy::mutable_key_type)]

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

// ── B3 locks: negative epoch_ns with sub-second remainder ──────────

/// The load-bearing B3 lock. Reverting `div_euclid`/`rem_euclid` back to
/// signed `%` + `as u32` will either bubble up "instant out of range"
/// (the pre-fix symptom) or, if chrono ever grew a wider nsec range,
/// produce the wrong subsecond component.
#[test]
fn test_time_to_datetime_negative_epoch_ns_subsecond_resolves_correctly() {
    // epoch_ns = -500_000_000 is 500ms before 1970-01-01T00:00:00 UTC,
    // which is 1969-12-31T23:59:59.500 UTC.
    let result = run(
        r#"
import time
fn main() -> Bool {
  let inst = Instant { epoch_ns: -500000000 }
  let dt = time.to_datetime(inst, 0)
  dt.date.year == 1969
    && dt.date.month == 12
    && dt.date.day == 31
    && dt.time.hour == 23
    && dt.time.minute == 59
    && dt.time.second == 59
    && dt.time.ns == 500000000
}
"#,
    );
    assert_eq!(result, Value::Bool(true));
}

/// Baseline: epoch_ns == 0 still round-trips through to_datetime.
#[test]
fn test_time_to_datetime_exact_epoch_zero_ok() {
    let result = run(
        r#"
import time
fn main() -> Bool {
  let inst = Instant { epoch_ns: 0 }
  let dt = time.to_datetime(inst, 0)
  dt.date.year == 1970
    && dt.date.month == 1
    && dt.date.day == 1
    && dt.time.hour == 0
    && dt.time.minute == 0
    && dt.time.second == 0
    && dt.time.ns == 0
}
"#,
    );
    assert_eq!(result, Value::Bool(true));
}

/// A negative `epoch_ns` whose magnitude spans multiple seconds.
/// `-1_500_000_000 ns` == 1.5s before epoch == 1969-12-31 23:59:58.500.
#[test]
fn test_time_to_datetime_negative_integer_seconds_ok() {
    let result = run(
        r#"
import time
fn main() -> Bool {
  let inst = Instant { epoch_ns: -1500000000 }
  let dt = time.to_datetime(inst, 0)
  dt.date.year == 1969
    && dt.date.month == 12
    && dt.date.day == 31
    && dt.time.hour == 23
    && dt.time.minute == 59
    && dt.time.second == 58
    && dt.time.ns == 500000000
}
"#,
    );
    assert_eq!(result, Value::Bool(true));
}

/// Mirror test for `time.to_utc`: same B3 signed-remainder bug. Pre-fix
/// this returns `Err("instant out of range")`; post-fix it resolves to
/// 1969-12-31 23:59:59.500.
#[test]
fn test_time_to_utc_negative_epoch_ns_subsecond() {
    let result = run(
        r#"
import time
fn main() -> Bool {
  let inst = Instant { epoch_ns: -500000000 }
  let dt = time.to_utc(inst)
  dt.date.year == 1969
    && dt.date.month == 12
    && dt.date.day == 31
    && dt.time.hour == 23
    && dt.time.minute == 59
    && dt.time.second == 59
    && dt.time.ns == 500000000
}
"#,
    );
    assert_eq!(result, Value::Bool(true));
}

// ── L1 locks: near-chrono-range datetimes + valid offsets ──────────

/// `time.to_instant` with a near-max year and a large negative offset
/// forces `naive_dt - offset` beyond chrono's ±262_143-year window.
/// Pre-fix: chrono's `Sub<Duration>` impl panics with
/// ``expect("`NaiveDateTime - TimeDelta` overflowed")``. Post-fix:
/// `checked_sub_signed` returns None and the builtin surfaces a clean
/// VmError.
#[test]
fn test_time_to_instant_offset_causes_date_overflow_returns_clean_error() {
    let err = run_err(
        r#"
import time
fn main() -> Instant {
  let dr = time.date(262142, 12, 31)
  match dr {
    Ok(d) -> {
      let tr = time.time(23, 59, 59)
      match tr {
        Ok(t) -> {
          let dt = time.datetime(d, t)
          -- try_minutes allows this magnitude; the naive +/- then
          -- overflows chrono's valid range.
          time.to_instant(dt, -2147483647)
        }
        Err(_) -> Instant { epoch_ns: 0 }
      }
    }
    Err(_) -> Instant { epoch_ns: 0 }
  }
}
"#,
    );
    assert!(
        err.to_lowercase().contains("out of range"),
        "expected clean out-of-range error from time.to_instant, got: {err}"
    );
    assert!(
        err.contains("time.to_instant"),
        "error should be tagged with the builtin name, got: {err}"
    );
    assert!(
        !err.contains("panicked"),
        "error should not mention 'panicked': {err}"
    );
    assert!(
        !err.to_lowercase().contains("overflowed"),
        "error should not surface chrono's 'overflowed' expect message: {err}"
    );
}

/// Defence-in-depth L1 check for `time.to_datetime`.
///
/// The `to_datetime` `+ offset` panic path is NOT reachable from Silt
/// user code today: `Instant.epoch_ns` is an `i64`, so the combined
/// `epoch_ns + i32::MAX-minute-offset` range stays well inside chrono's
/// ±262143-year NaiveDateTime window (max reachable is about year 6348).
/// We nevertheless run an extremal-input smoke test so that:
///
///  1. Any future widening of `Instant` (e.g. to i128) or chrono's
///     NaiveDateTime representation will be caught by this test if the
///     checked-add guard has been removed.
///  2. An extremal input NEVER surfaces a chrono `expect("... overflowed")`
///     string or a "builtin panicked" message — only clean `VmError`s
///     or valid values. Reverting `checked_add_signed` back to `+` and
///     then later widening Instant would silently re-introduce the
///     panic; this test documents the invariant.
///
/// Mutation note: since the panic path is unreachable from Silt today,
/// this test ALSO passes when `checked_add_signed` is reverted to `+`.
/// The B3 mutation verification for to_datetime (test 1) and the L1
/// mutation verification for to_instant (test 5) together lock both
/// fixes; this test is a structural guard, not a bug-specific lock.
#[test]
fn test_time_to_datetime_offset_causes_date_overflow_returns_clean_error() {
    // Try large/small representable epoch_ns combined with the
    // largest/smallest offsets. None of these should panic; all should
    // either return a DateTime or a clean VmError. We stay a hair
    // inside `i64::MAX`/`i64::MIN` because the Silt lexer rejects
    // the full-range literals as "number too large".
    let probes: &[(&str, i64, i64)] = &[
        (
            "near-max epoch_ns, i32::MAX offset",
            9_000_000_000_000_000_000,
            2_147_483_647,
        ),
        (
            "near-min epoch_ns, i32::MIN offset",
            -9_000_000_000_000_000_000,
            -2_147_483_647,
        ),
        (
            "near-max epoch_ns, i32::MIN offset",
            9_000_000_000_000_000_000,
            -2_147_483_647,
        ),
        (
            "near-min epoch_ns, i32::MAX offset",
            -9_000_000_000_000_000_000,
            2_147_483_647,
        ),
    ];

    for (label, epoch_ns, offset) in probes {
        let src = format!(
            r#"
import time
fn main() -> Int {{
  let inst = Instant {{ epoch_ns: {epoch_ns} }}
  let dt = time.to_datetime(inst, {offset})
  dt.date.year
}}
"#
        );
        // Use the error path (this may succeed or fail; we just care
        // that no panic noise surfaces). Drive the VM manually so we
        // tolerate both success and VmError outcomes.
        let tokens = silt::lexer::Lexer::new(&src)
            .tokenize()
            .expect("lexer error");
        let mut program = silt::parser::Parser::new(tokens)
            .parse_program()
            .expect("parse error");
        let _ = silt::typechecker::check(&mut program);
        let mut compiler = silt::compiler::Compiler::new();
        let functions = compiler.compile_program(&program).expect("compile error");
        let script = Arc::new(functions.into_iter().next().unwrap());
        let mut vm = Vm::new();
        match vm.run(script) {
            Ok(_) => { /* valid datetime — fine */ }
            Err(e) => {
                let msg = format!("{e}");
                assert!(
                    !msg.contains("panicked"),
                    "{label}: to_datetime surfaced a panic string: {msg}"
                );
                assert!(
                    !msg.to_lowercase().contains("overflowed"),
                    "{label}: to_datetime surfaced chrono's 'overflowed' \
                     expect message: {msg}"
                );
            }
        }
    }
}
