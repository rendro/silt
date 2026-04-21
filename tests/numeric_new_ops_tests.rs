//! Tests for `int.clamp`, `float.clamp`, and the `float.is_*` predicates.
//!
//! Covers:
//!   * Happy paths at the low edge, high edge, and inside `[lo, hi]`.
//!   * Invalid bounds (`lo > hi`) surface a clean runtime error, not a
//!     Rust panic.
//!   * `float.is_finite` / `is_infinite` / `is_nan` give the correct
//!     answer for `float.infinity`, `float.neg_infinity`, and
//!     `float.nan` (the only way in silt to produce an `ExtFloat` we can
//!     pass to these predicates without going through `math.sqrt` etc).
//!
//! Rationale: the predicates take `ExtFloat` intentionally — `Float` is
//! guaranteed finite per `docs/language/types.md`, so defining a `Float`
//! overload would be misleading (the answer is always `false` / `true` /
//! `false` respectively). These tests therefore feed them values sourced
//! from the non-finite `float.*` constants.

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

// ── int.clamp ───────────────────────────────────────────────────────

#[test]
fn int_clamp_inside_range_returns_value() {
    let v = run(r#"
        import int
        fn main() -> int {
          return int.clamp(5, 0, 10)
        }
        "#);
    assert_eq!(v, Value::Int(5));
}

#[test]
fn int_clamp_below_lo_returns_lo() {
    let v = run(r#"
        import int
        fn main() -> int {
          return int.clamp(-3, 0, 10)
        }
        "#);
    assert_eq!(v, Value::Int(0));
}

#[test]
fn int_clamp_above_hi_returns_hi() {
    let v = run(r#"
        import int
        fn main() -> int {
          return int.clamp(42, 0, 10)
        }
        "#);
    assert_eq!(v, Value::Int(10));
}

#[test]
fn int_clamp_equal_bounds_degenerate_range() {
    // lo == hi is a valid (degenerate) range: every input clamps to that
    // single value. Pin this behavior because the obvious `lo > hi`
    // rejection could accidentally swallow `lo == hi` if someone
    // tightens the guard.
    let v = run(r#"
        import int
        fn main() -> int {
          return int.clamp(100, 7, 7)
        }
        "#);
    assert_eq!(v, Value::Int(7));
}

#[test]
fn int_clamp_invalid_bounds_panics_cleanly() {
    let err = run_err(
        r#"
        import int
        fn main() {
          let _ = int.clamp(5, 10, 0)
        }
        "#,
    );
    assert!(
        err.contains("int.clamp: invalid bounds"),
        "expected clean invalid-bounds error, got: {err}"
    );
}

// ── float.clamp ─────────────────────────────────────────────────────

#[test]
fn float_clamp_inside_range_returns_value() {
    let v = run(r#"
        import float
        fn main() -> float {
          return float.clamp(0.5, 0.0, 1.0)
        }
        "#);
    assert_eq!(v, Value::Float(0.5));
}

#[test]
fn float_clamp_below_lo_returns_lo() {
    let v = run(r#"
        import float
        fn main() -> float {
          return float.clamp(-0.2, 0.0, 1.0)
        }
        "#);
    assert_eq!(v, Value::Float(0.0));
}

#[test]
fn float_clamp_above_hi_returns_hi() {
    let v = run(r#"
        import float
        fn main() -> float {
          return float.clamp(1.5, 0.0, 1.0)
        }
        "#);
    assert_eq!(v, Value::Float(1.0));
}

#[test]
fn float_clamp_invalid_bounds_panics_cleanly() {
    let err = run_err(
        r#"
        import float
        fn main() {
          let _ = float.clamp(0.5, 1.0, 0.0)
        }
        "#,
    );
    assert!(
        err.contains("float.clamp: invalid bounds"),
        "expected clean invalid-bounds error, got: {err}"
    );
}

// ── float.is_finite / is_infinite / is_nan ──────────────────────────
//
// These take `ExtFloat`. The only values in scope that are typed
// `ExtFloat` without going through another builtin are the three
// non-finite `float.*` constants, plus finite values produced by e.g.
// `math.sqrt(4.0)` (whose return type is ExtFloat even though the
// result is finite). That combination is enough to exercise every arm.

#[test]
fn is_finite_true_for_finite_extfloat() {
    // `math.sqrt(4.0)` returns `ExtFloat` (per the typechecker; sqrt of a
    // negative would be NaN, so the return type is widened), but the
    // runtime value here is finite.
    let v = run(r#"
        import math
        import float
        fn main() -> bool {
          return float.is_finite(math.sqrt(4.0))
        }
        "#);
    assert_eq!(v, Value::Bool(true));
}

#[test]
fn is_finite_false_for_infinity() {
    let v = run(r#"
        import float
        fn main() -> bool {
          return float.is_finite(float.infinity)
        }
        "#);
    assert_eq!(v, Value::Bool(false));
}

#[test]
fn is_finite_false_for_nan() {
    let v = run(r#"
        import float
        fn main() -> bool {
          return float.is_finite(float.nan)
        }
        "#);
    assert_eq!(v, Value::Bool(false));
}

#[test]
fn is_infinite_true_for_pos_infinity() {
    let v = run(r#"
        import float
        fn main() -> bool {
          return float.is_infinite(float.infinity)
        }
        "#);
    assert_eq!(v, Value::Bool(true));
}

#[test]
fn is_infinite_true_for_neg_infinity() {
    let v = run(r#"
        import float
        fn main() -> bool {
          return float.is_infinite(float.neg_infinity)
        }
        "#);
    assert_eq!(v, Value::Bool(true));
}

#[test]
fn is_infinite_false_for_nan() {
    // NaN is not infinite — this is the usual IEEE 754 gotcha and worth
    // pinning explicitly.
    let v = run(r#"
        import float
        fn main() -> bool {
          return float.is_infinite(float.nan)
        }
        "#);
    assert_eq!(v, Value::Bool(false));
}

#[test]
fn is_infinite_false_for_finite_extfloat() {
    let v = run(r#"
        import math
        import float
        fn main() -> bool {
          return float.is_infinite(math.sqrt(4.0))
        }
        "#);
    assert_eq!(v, Value::Bool(false));
}

#[test]
fn is_nan_true_for_nan() {
    let v = run(r#"
        import float
        fn main() -> bool {
          return float.is_nan(float.nan)
        }
        "#);
    assert_eq!(v, Value::Bool(true));
}

#[test]
fn is_nan_false_for_infinity() {
    let v = run(r#"
        import float
        fn main() -> bool {
          return float.is_nan(float.infinity)
        }
        "#);
    assert_eq!(v, Value::Bool(false));
}

#[test]
fn is_nan_false_for_finite_extfloat() {
    let v = run(r#"
        import math
        import float
        fn main() -> bool {
          return float.is_nan(math.sqrt(4.0))
        }
        "#);
    assert_eq!(v, Value::Bool(false));
}
