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
    let result = run(r#"
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
"#);
    assert_eq!(result, Value::Bool(true));
}

/// Baseline: epoch_ns == 0 still round-trips through to_datetime.
#[test]
fn test_time_to_datetime_exact_epoch_zero_ok() {
    let result = run(r#"
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
"#);
    assert_eq!(result, Value::Bool(true));
}

/// A negative `epoch_ns` whose magnitude spans multiple seconds.
/// `-1_500_000_000 ns` == 1.5s before epoch == 1969-12-31 23:59:58.500.
#[test]
fn test_time_to_datetime_negative_integer_seconds_ok() {
    let result = run(r#"
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
"#);
    assert_eq!(result, Value::Bool(true));
}

/// Mirror test for `time.to_utc`: same B3 signed-remainder bug. Pre-fix
/// this returns `Err("instant out of range")`; post-fix it resolves to
/// 1969-12-31 23:59:59.500.
#[test]
fn test_time_to_utc_negative_epoch_ns_subsecond() {
    let result = run(r#"
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
"#);
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

/// Load-bearing L1 lock for `time.to_datetime` (round 17 F20 upgrade).
///
/// Earlier rounds documented this test as a "structural guard, not a
/// bug-specific lock": the `+ offset` panic path was thought to be
/// unreachable from Silt user code because `Instant.epoch_ns` is an
/// `i64` and so the combined epoch + offset stayed well inside
/// chrono's ±262_143-year `NaiveDateTime` window.
///
/// Round 17 re-analysis showed that was wrong — `offset_min` is
/// destructured from `Value::Int` which is an `i64`, not an `i32`.
/// `chrono::Duration::try_minutes` accepts up to ≈1.5e14 minutes
/// (≈292M years) before the `checked_mul(60)` inside it overflows, so
/// a `150_000_000_000` minute offset (≈285_388 years) from a
/// `epoch_ns: 0` base yields a `Duration` that, when added with the
/// bare `+` operator, pushes `NaiveDateTime` past its 262_143-year
/// max and triggers chrono's:
///
///     expect("`NaiveDateTime + TimeDelta` overflowed")
///
/// panic. The `checked_add_signed` fix in `src/builtins/data.rs`
/// returns `None` for this case and surfaces a clean
/// `"time.to_datetime: datetime + offset out of range"` VmError.
///
/// This test drives that exact offset and asserts:
///  - the builtin returns a clean `VmError`, not an `Ok`,
///  - the error message does NOT contain `"panicked"` or chrono's
///    `"overflowed"` expect string, and
///  - the error message IS tagged with `time.to_datetime`.
///
/// Mutation verification: reverting `.checked_add_signed(offset)` back
/// to `+ offset` in src/builtins/data.rs would now make this test fail
/// with a "builtin module 'time' panicked: `NaiveDateTime + TimeDelta`
/// overflowed" VmError — caught by the `overflowed` negative
/// assertion below.
#[test]
fn test_time_to_datetime_offset_causes_date_overflow_returns_clean_error() {
    // 150_000_000_000 minutes ≈ 285_388 years forward from 1970-01-01
    // → year ≈ 287358, which exceeds chrono's 262143 max year.
    // `Duration::try_minutes(150_000_000_000)` is valid (well under
    // the ~1.5e14-minute try_minutes ceiling), so we proceed to the
    // `checked_add_signed` / `+` call site — exactly the path the
    // fix guards.
    let err = run_err(
        r#"
import time
fn main() -> Int {
  let inst = Instant { epoch_ns: 0 }
  let dt = time.to_datetime(inst, 150000000000)
  dt.date.year
}
"#,
    );
    assert!(
        err.contains("time.to_datetime"),
        "error should be tagged with time.to_datetime, got: {err}"
    );
    assert!(
        err.to_lowercase().contains("out of range"),
        "expected clean out-of-range error, got: {err}"
    );
    assert!(
        !err.contains("panicked"),
        "error should not mention 'panicked' (fix regression!): {err}"
    );
    assert!(
        !err.to_lowercase().contains("overflowed"),
        "error should not surface chrono's 'overflowed' expect message \
         (fix regression!): {err}"
    );

    // Defence-in-depth extremal-input smoke test: the earlier probe
    // matrix still has value as a fuzzer against future changes, so
    // keep it — but now the primary assertion above is the real lock.
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

// ── F19 locks: strftime pattern / receiver-type mismatch ───────────
//
// Round 17 finding: `validate_strftime_pattern` only rejected
// `Item::Error` (truly bogus tokens like `%Q`). A valid specifier
// whose semantics don't match the receiver type — `%H` on a
// `NaiveDate` (no time component), or `%z` on a `NaiveDateTime`
// (naive = no TZ) — still reached chrono's `format()` call, which
// panics with "a Display implementation returned an error
// unexpectedly". `catch_builtin_panic` converts that into a
// `VmError`, BUT Rust's default panic hook still writes a 3-line
// `thread 'main' panicked at ...` notice to stderr before the
// recovery, leaking internal panic text to the user.
//
// The fix extends `validate_strftime_pattern` to classify each parsed
// `Item::Numeric` / `Item::Fixed` variant against the receiver type
// (`Date` vs `DateTime`) and reject incompatible specifiers up-front
// with a clean `VmError`. Because the panic path is never reached,
// stderr stays quiet.
//
// Each lock below has two assertions:
//   1. Library-level: `run_err` returns a clean `VmError` whose
//      message mentions "specifier" and names the receiver type.
//      A regression that removes the up-front reject would leak
//      chrono's "a Display implementation returned an error
//      unexpectedly" panic message into the VmError.
//   2. Subprocess-level: the silt binary run against the same
//      program must not print "panicked at" to stderr. This catches
//      any future regression where the panic escapes (even if the
//      VmError looks clean) because the default hook still writes
//      before `catch_builtin_panic` recovers.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static F19_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn silt_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

fn f19_temp_silt_file(content: &str) -> PathBuf {
    let n = F19_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join("silt_f19_strftime_tests");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("probe_{n}.silt"));
    std::fs::write(&path, content).unwrap();
    path
}

/// Run the given silt source as a subprocess and assert that stderr
/// never contains "panicked at". The subprocess exit code is
/// ignored — runtime errors are expected. This is the real F19
/// "no stderr panic leak" check.
fn assert_subprocess_no_panic_leak(src: &str) {
    let path = f19_temp_silt_file(src);
    let output = silt_bin()
        .arg(&path)
        .output()
        .expect("failed to spawn silt binary");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked at"),
        "silt subprocess leaked a Rust panic notice to stderr:\n{stderr}"
    );
    // Also guard against chrono's specific panic text surfacing via
    // the panic message (even if the "thread 'main' panicked at"
    // header ever gets suppressed, the body should not leak).
    assert!(
        !stderr.contains("a Display implementation returned an error"),
        "silt subprocess leaked chrono's Display-impl panic text:\n{stderr}"
    );
}

/// F19 lock #1: `time.format_date(d, "%H")` — a valid time-only
/// specifier on a `NaiveDate` receiver.
#[test]
fn test_time_format_date_rejects_time_only_specifier() {
    let src = r#"
import time
fn main() -> String {
  match time.date(2024, 6, 15) {
    Ok(d) -> time.format_date(d, "%H")
    Err(_) -> "date err"
  }
}
"#;
    let err = run_err(src);
    assert!(
        err.contains("time.format_date"),
        "error should be tagged time.format_date, got: {err}"
    );
    assert!(
        err.contains("specifier"),
        "error should mention 'specifier', got: {err}"
    );
    assert!(
        err.contains("Date"),
        "error should name the 'Date' receiver type, got: {err}"
    );
    assert!(
        !err.contains("panicked"),
        "error leaked 'panicked' text — chrono format panic escaped: {err}"
    );
    assert!(
        !err.contains("Display implementation"),
        "error leaked chrono's Display-impl panic text: {err}"
    );

    // Subprocess check: no `thread 'main' panicked at ...` line in
    // stderr. This is the canonical F19 stderr-leak lock.
    assert_subprocess_no_panic_leak(src);
}

/// F19 lock #2: `time.format_date(d, "%z")` — a timezone specifier
/// on a `NaiveDate` receiver. Date has neither a time nor a TZ.
#[test]
fn test_time_format_date_rejects_timezone_specifier() {
    let src = r#"
import time
fn main() -> String {
  match time.date(2024, 6, 15) {
    Ok(d) -> time.format_date(d, "%z")
    Err(_) -> "date err"
  }
}
"#;
    let err = run_err(src);
    assert!(
        err.contains("time.format_date"),
        "error should be tagged time.format_date, got: {err}"
    );
    assert!(
        err.to_lowercase().contains("timezone") || err.to_lowercase().contains("time specifier"),
        "error should mention 'timezone' or 'time specifier', got: {err}"
    );
    assert!(
        !err.contains("panicked"),
        "error leaked 'panicked' text: {err}"
    );
    assert_subprocess_no_panic_leak(src);
}

/// F19 lock #3: `time.format(dt, "%z")` — timezone specifier on a
/// `NaiveDateTime` receiver. Silt DateTimes are naive (no TZ).
#[test]
fn test_time_format_rejects_timezone_specifier_for_naive_datetime() {
    let src = r#"
import time
fn main() -> String {
  match time.date(2024, 6, 15) {
    Ok(d) -> match time.time(10, 30, 0) {
      Ok(t) -> time.format(time.datetime(d, t), "%z")
      Err(_) -> "time err"
    }
    Err(_) -> "date err"
  }
}
"#;
    let err = run_err(src);
    assert!(
        err.contains("time.format"),
        "error should be tagged time.format, got: {err}"
    );
    assert!(
        err.to_lowercase().contains("timezone"),
        "error should mention 'timezone', got: {err}"
    );
    assert!(
        err.to_lowercase().contains("naive"),
        "error should mention 'naive' (the receiver type), got: {err}"
    );
    assert!(
        !err.contains("panicked"),
        "error leaked 'panicked' text: {err}"
    );
    assert_subprocess_no_panic_leak(src);
}

/// F19 positive control: `time.format_date(d, "%Y-%m-%d")` — a
/// pure-date specifier on a Date receiver. Must still work.
#[test]
fn test_time_format_date_happy_path_still_works() {
    let v = run(r#"
import time
fn main() -> String {
  match time.date(2024, 6, 15) {
    Ok(d) -> time.format_date(d, "%Y-%m-%d")
    Err(_) -> "err"
  }
}
"#);
    assert_eq!(v, Value::String("2024-06-15".into()));
}

/// F19 positive control: `time.format(dt, "%H:%M:%S")` — time
/// specifiers on a DateTime. Must still work (fix only rejects TZ
/// specifiers on DateTime, not time specifiers).
#[test]
fn test_time_format_datetime_with_time_specifiers_still_works() {
    let v = run(r#"
import time
fn main() -> String {
  match time.date(2024, 6, 15) {
    Ok(d) -> match time.time(10, 30, 45) {
      Ok(t) -> time.format(time.datetime(d, t), "%H:%M:%S")
      Err(_) -> "time err"
    }
    Err(_) -> "date err"
  }
}
"#);
    assert_eq!(v, Value::String("10:30:45".into()));
}

/// F19 positive control: previous round's `%Q` (bogus specifier)
/// rejection is preserved by the new validator. A regression that
/// narrowed the validator to only handle Fixed/Numeric and dropped
/// the `Item::Error` arm would fail this test.
#[test]
fn test_time_format_still_rejects_bogus_specifier() {
    let err = run_err(
        r#"
import time
fn main() -> String {
  match time.date(2024, 6, 15) {
    Ok(d) -> time.format_date(d, "%Q")
    Err(_) -> "err"
  }
}
"#,
    );
    assert!(
        err.contains("invalid format specifier"),
        "expected 'invalid format specifier' rejection for '%Q', got: {err}"
    );
}
