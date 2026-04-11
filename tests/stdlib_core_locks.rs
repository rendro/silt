//! Regression locks for stdlib core builtins (`option.*`, `result.*`) and
//! concurrency / env safety rails that previously had zero or one-branch
//! coverage. See audit round 15 GAP: untested stdlib builtins and safety
//! rails.
//!
//! Each test pins a specific code path to its observable silt-level
//! behavior. If a refactor changes the return shape or error phrase,
//! these tests should fail loudly so the audit record stays honest.

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::value::Value;
use silt::vm::Vm;
use std::sync::Arc;

// ── Helpers (mirrors tests/integration.rs) ──────────────────────────

fn run(input: &str) -> Value {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    vm.run(script).expect("runtime error")
}

fn run_err(input: &str) -> String {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = typechecker::check(&mut program);
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

// ════════════════════════════════════════════════════════════════════
// option.map — src/builtins/core.rs:145-157
// ════════════════════════════════════════════════════════════════════

#[test]
fn test_option_map_some_applies_callback() {
    // Locks src/builtins/core.rs:150-154 — Some branch applies the
    // closure and rewraps the result as Some(new_val).
    let result = run(
        r#"
import option
fn main() { option.map(Some(5), { n -> n * 2 }) }
    "#,
    );
    assert_eq!(result, Value::Variant("Some".into(), vec![Value::Int(10)]));
}

#[test]
fn test_option_map_none_propagates() {
    // Locks src/builtins/core.rs:155 — None propagates without
    // invoking the callback.
    let result = run(
        r#"
import option
fn main() { option.map(None, { n -> n * 2 }) }
    "#,
    );
    assert_eq!(result, Value::Variant("None".into(), Vec::new()));
}

// ════════════════════════════════════════════════════════════════════
// option.to_result — src/builtins/core.rs:131-144
// ════════════════════════════════════════════════════════════════════

#[test]
fn test_option_to_result_some_becomes_ok() {
    // Locks src/builtins/core.rs:136-138 — Some(v) becomes Ok(v),
    // discarding the provided err value.
    let result = run(
        r#"
import option
fn main() { option.to_result(Some(42), "missing") }
    "#,
    );
    assert_eq!(result, Value::Variant("Ok".into(), vec![Value::Int(42)]));
}

#[test]
fn test_option_to_result_none_becomes_err_with_value() {
    // Locks src/builtins/core.rs:139-141 — None becomes
    // Err(err_value), propagating the user-supplied error payload.
    let result = run(
        r#"
import option
fn main() { option.to_result(None, "missing") }
    "#,
    );
    assert_eq!(
        result,
        Value::Variant("Err".into(), vec![Value::String("missing".into())])
    );
}

// ════════════════════════════════════════════════════════════════════
// option.flat_map — src/builtins/core.rs:159-170
// ════════════════════════════════════════════════════════════════════

#[test]
fn test_option_flat_map_some_applies_callback() {
    // Locks src/builtins/core.rs:163-166 — Some(v) invokes the
    // callback with v and returns its Option result directly (no
    // extra wrapping). The None branch is already covered by
    // test_option_flat_map_none in tests/integration.rs.
    let result = run(
        r#"
import option
fn main() { option.flat_map(Some(3), { n -> Some(n + 10) }) }
    "#,
    );
    assert_eq!(result, Value::Variant("Some".into(), vec![Value::Int(13)]));
}

// ════════════════════════════════════════════════════════════════════
// result.unwrap_or — src/builtins/core.rs:9-20
// ════════════════════════════════════════════════════════════════════

#[test]
fn test_result_unwrap_or_ok_returns_value() {
    // Locks src/builtins/core.rs:14-16 — Ok(v) branch returns the
    // inner value and ignores the default.
    let result = run(
        r#"
import result
fn main() { result.unwrap_or(Ok(7), 99) }
    "#,
    );
    assert_eq!(result, Value::Int(7));
}

#[test]
fn test_result_unwrap_or_err_returns_default() {
    // Locks src/builtins/core.rs:17 — Err branch returns the
    // user-supplied default, dropping the error payload.
    let result = run(
        r#"
import result
fn main() { result.unwrap_or(Err("boom"), 99) }
    "#,
    );
    assert_eq!(result, Value::Int(99));
}

// ════════════════════════════════════════════════════════════════════
// result.flat_map — src/builtins/core.rs:84-95
// ════════════════════════════════════════════════════════════════════

#[test]
fn test_result_flat_map_ok_applies_callback() {
    // Locks src/builtins/core.rs:89-91 — Ok(v) invokes callback with
    // v; callback's Result is returned directly (no extra wrapping).
    let result = run(
        r#"
import result
fn main() { result.flat_map(Ok(4), { n -> Ok(n * n) }) }
    "#,
    );
    assert_eq!(result, Value::Variant("Ok".into(), vec![Value::Int(16)]));
}

#[test]
fn test_result_flat_map_err_propagates() {
    // Locks src/builtins/core.rs:92 — Err is passed through unchanged
    // and the callback is never invoked.
    let result = run(
        r#"
import result
fn main() { result.flat_map(Err("fail"), { n -> Ok(n * n) }) }
    "#,
    );
    assert_eq!(
        result,
        Value::Variant("Err".into(), vec![Value::String("fail".into())])
    );
}

// ════════════════════════════════════════════════════════════════════
// result.map_err — src/builtins/core.rs:51-63 (Err branch)
// ════════════════════════════════════════════════════════════════════

#[test]
fn test_result_map_err_on_err_applies_callback() {
    // Locks src/builtins/core.rs:57-61 — Err(e) invokes callback
    // with e and rewraps as Err(new_e). The Ok passthrough is
    // already covered by test_result_map_err_on_ok in integration.rs.
    let result = run(
        r#"
import result
fn main() { result.map_err(Err(3), { e -> e + 100 }) }
    "#,
    );
    assert_eq!(result, Value::Variant("Err".into(), vec![Value::Int(103)]));
}

// ════════════════════════════════════════════════════════════════════
// result.map_ok — src/builtins/core.rs:37-49 (Ok branch)
// ════════════════════════════════════════════════════════════════════

#[test]
fn test_result_map_ok_on_ok_applies_callback() {
    // Locks src/builtins/core.rs:42-46 — Ok(v) invokes callback with
    // v and rewraps as Ok(new_v). The Err passthrough is already
    // covered by test_result_map_ok_on_err in integration.rs.
    let result = run(
        r#"
import result
fn main() { result.map_ok(Ok(6), { v -> v * 7 }) }
    "#,
    );
    assert_eq!(result, Value::Variant("Ok".into(), vec![Value::Int(42)]));
}

// ════════════════════════════════════════════════════════════════════
// env.set spawn-thread guard — src/builtins/io.rs:338-342
// ════════════════════════════════════════════════════════════════════

#[test]
fn test_env_set_from_spawned_task_rejected() {
    // Locks src/builtins/io.rs:338-342 — env.set refuses to run from
    // a scheduled task (prevents std::env::set_var UB on worker
    // threads). The error from the task propagates through task.join
    // as "joined task failed: <original>" (src/builtins/concurrency.rs
    // lines 418-421, 438-441).
    let err = run_err(
        r#"
import env
import task
fn main() {
    let h = task.spawn(fn() {
        env.set("SILT_LOCK_TEST_SPAWN", "nope")
    })
    task.join(h)
}
    "#,
    );
    assert!(
        err.contains("env.set cannot be called from a spawned task"),
        "expected env.set spawn rejection, got: {err}"
    );
}

// ════════════════════════════════════════════════════════════════════
// channel.new(-1) negative-capacity guard
// src/builtins/concurrency.rs:16-21
// ════════════════════════════════════════════════════════════════════

#[test]
fn test_channel_new_negative_capacity_rejected() {
    // Locks src/builtins/concurrency.rs:17-20 — negative capacities
    // are rejected with a clean VmError, preventing a silent cast
    // to a giant usize.
    let err = run_err(
        r#"
import channel
fn main() { channel.new(-1) }
    "#,
    );
    assert!(
        err.contains("channel.new capacity must be a non-negative integer"),
        "expected negative-capacity rejection, got: {err}"
    );
}

// ════════════════════════════════════════════════════════════════════
// channel.timeout(-1) negative-duration guard
// src/builtins/concurrency.rs:227-230
// ════════════════════════════════════════════════════════════════════

#[test]
fn test_channel_timeout_negative_duration_rejected_or_latent() {
    // Locks src/builtins/concurrency.rs:227-231 — negative durations
    // are rejected with a clean VmError instead of wrapping via
    // `as u64` into a near-infinite delay.
    let err = run_err(
        r#"
import channel
fn main() { channel.timeout(-1) }
    "#,
    );
    assert!(
        err.contains("channel.timeout duration must be non-negative"),
        "expected negative-duration rejection, got: {err}"
    );
}
