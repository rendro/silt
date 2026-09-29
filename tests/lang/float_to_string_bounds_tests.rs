//! Regression tests for `float.to_string(f, decimals)` bounds.
//!
//! Round 15 LATENT finding: Rust's `{:.prec$}` formatter backs precision
//! with a `u16`, so any `decimals > 65535` panics inside std with
//! "Formatting argument out of range". `catch_builtin_panic` converts
//! that panic into a `VmError` so the VM survives (hence LATENT), but
//! the panic still prints a noisy `thread 'main' panicked at ...` line
//! to stderr and the user-visible message is opaque. The fix in
//! `src/builtins/numeric.rs` adds an up-front `u16::try_from` check and
//! surfaces a clean error instead.
//!
//! These tests lock the new behavior:
//!  - Over-max precision is rejected with an exact, stable message.
//!  - Negative precision is rejected cleanly (pre-existing guard).
//!  - The happy path at `decimals = 0`, `decimals = 65535`, and a
//!    typical `decimals = 2` still works.
//!
//! The tests are careful to use `vm.run(...)` and match on the returned
//! `VmError` string — a panic would propagate through `expect_err`'s
//! path as an unwind and abort the test process, so a clean pass here
//! also asserts "no Rust panic escapes".

#![allow(clippy::mutable_key_type)]

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::value::Value;
use silt::vm::Vm;
use std::sync::Arc;

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

// ── Regression tests ────────────────────────────────────────────────

/// `float.to_string(1.5, 100000)` must not bubble up the raw Rust
/// "Formatting argument out of range" panic. After the fix it returns a
/// clean VmError with the exact phrase below. The very fact that this
/// test reaches `expect_err` (rather than aborting on an unwind) is
/// part of the assertion: a future revert of the guard would let the
/// panic escape and the test would fail with a panic-like signature.
#[test]
fn test_float_to_string_decimals_over_u16_max_rejected() {
    let err = run_err(
        r#"
        import float
        fn main() {
          let _ = float.to_string(1.5, 100000)
        }
        "#,
    );
    assert!(
        err.contains("float.to_string: decimals 100000 exceeds maximum precision of 65535"),
        "expected clean over-u16::MAX rejection, got: {err}"
    );
    // Defense-in-depth: make sure the raw Rust panic message never
    // surfaces in the error string. If this ever trips, the guard
    // regressed and we're back to the LATENT behavior.
    assert!(
        !err.contains("Formatting argument out of range"),
        "raw Rust fmt panic message leaked into VmError: {err}"
    );
}

/// `float.to_string(1.5, -1)` must reject negative precision cleanly.
#[test]
fn test_float_to_string_decimals_negative_rejected_or_handled() {
    let err = run_err(
        r#"
        import float
        fn main() {
          let _ = float.to_string(1.5, -1)
        }
        "#,
    );
    assert!(
        err.contains("float.to_string: decimals must be non-negative"),
        "expected clean negative rejection, got: {err}"
    );
}

/// `float.to_string(1.5, 0)` — Rust's formatter uses banker's rounding,
/// so `1.5` rounds to `2` (verified against rustc before locking).
#[test]
fn test_float_to_string_decimals_zero_ok() {
    let v = run(r#"
        import float
        fn main() -> string {
          return float.to_string(1.5, 0)
        }
        "#);
    assert_eq!(v, Value::String("2".into()));
}

/// `float.to_string(1.5, 65535)` — exactly at `u16::MAX`, the highest
/// precision Rust's fmt supports. Must succeed with a massive string
/// and (crucially) must NOT panic. The happy path is preserved by the
/// fix, which only rejects values that would have panicked anyway.
#[test]
fn test_float_to_string_decimals_at_u16_max_ok() {
    let v = run(r#"
        import float
        fn main() -> string {
          return float.to_string(1.5, 65535)
        }
        "#);
    match v {
        Value::String(s) => {
            // "1." plus 65535 fractional digits = 65537 chars total.
            assert_eq!(
                s.len(),
                65537,
                "expected 1 + 1 + 65535 = 65537 chars, got {}",
                s.len()
            );
            assert!(
                s.starts_with("1.5"),
                "expected leading '1.5', got: {}",
                &s[..8]
            );
        }
        other => panic!("expected String, got {other:?}"),
    }
}

/// Normal positive case: `float.to_string(3.14159, 2)` = `"3.14"`.
/// Guards against any regression in the happy path from the fix.
#[test]
fn test_float_to_string_normal_ok() {
    let v = run(r#"
        import float
        fn main() -> string {
          return float.to_string(3.14159, 2)
        }
        "#);
    assert_eq!(v, Value::String("3.14".into()));
}
