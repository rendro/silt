//! Round 67 LATENT lock — `IntoValue for f64` must canonicalize non-finite
//! values to `Value::ExtFloat`.
//!
//! The `Value::Float`/`Value::ExtFloat` split is a load-bearing invariant:
//! `Value::Float` carries finite f64 only, non-finite (NaN, ±∞) goes in
//! `Value::ExtFloat`. The invariant is documented at
//! `src/vm/mod.rs::Vm::finite_float`, `src/value.rs` PartialEq/Ord arms,
//! and `tests/list_sum_product_float_finite_tests.rs`.
//!
//! Pre-fix, `impl IntoValue for f64 { Value::Float(self) }` would happily
//! construct a `Value::Float(NaN)` via `register_fn0("get_nan", || ->
//! f64 { f64::NAN })` from an embedder. That breaks every downstream
//! container/dedup path that assumes `Value::Float` is finite. Mirror the
//! canonicalization at `src/builtins/numeric.rs::clamp` (round-the-result
//! → ExtFloat for non-finite).

use silt::value::{IntoValue, Value};

#[test]
fn into_value_f64_finite_routes_to_float() {
    match 1.5_f64.into_value() {
        Value::Float(x) => assert_eq!(x, 1.5),
        other => panic!("expected Value::Float(1.5), got {other:?}"),
    }
    match 0.0_f64.into_value() {
        Value::Float(x) => assert_eq!(x, 0.0),
        other => panic!("expected Value::Float(0.0), got {other:?}"),
    }
    match (-0.0_f64).into_value() {
        Value::Float(x) => assert_eq!(x, -0.0),
        other => panic!("expected Value::Float(-0.0), got {other:?}"),
    }
    match f64::MAX.into_value() {
        Value::Float(x) => assert_eq!(x, f64::MAX),
        other => panic!("expected Value::Float(MAX), got {other:?}"),
    }
}

#[test]
fn into_value_f64_nan_routes_to_extfloat() {
    match f64::NAN.into_value() {
        Value::ExtFloat(x) => assert!(x.is_nan(), "expected NaN inside ExtFloat, got {x}"),
        other => panic!(
            "Value::Float invariant violated — NaN must canonicalize to ExtFloat, got {other:?}"
        ),
    }
}

#[test]
fn into_value_f64_pos_inf_routes_to_extfloat() {
    match f64::INFINITY.into_value() {
        Value::ExtFloat(x) => assert_eq!(x, f64::INFINITY),
        other => panic!(
            "Value::Float invariant violated — +inf must canonicalize to ExtFloat, got {other:?}"
        ),
    }
}

#[test]
fn into_value_f64_neg_inf_routes_to_extfloat() {
    match (-f64::INFINITY).into_value() {
        Value::ExtFloat(x) => assert_eq!(x, f64::NEG_INFINITY),
        other => panic!(
            "Value::Float invariant violated — -inf must canonicalize to ExtFloat, got {other:?}"
        ),
    }
}
