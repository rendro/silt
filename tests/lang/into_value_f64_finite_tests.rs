//! `IntoValue for f64` builds a `Float` only from a finite value.
//!
//! A silt `Float` is always finite and never `-0.0`. A host function
//! returning NaN or an infinity has no silt value to return, so the
//! conversion fails (and the call raises a runtime error that names the
//! function: see `host_module_tests`).

use silt::value::{Float, IntoValue, Value};

#[test]
fn into_value_f64_finite_builds_float() {
    let float = |f: f64| Float::new(f).map(Value::Float).ok_or(String::new());
    assert_eq!(1.5_f64.into_value(), float(1.5));
    assert_eq!(f64::MAX.into_value(), float(f64::MAX));
    match (-0.0_f64).into_value() {
        Ok(Value::Float(x)) => assert!(x.get().is_sign_positive(), "-0.0 must become 0.0"),
        other => panic!("expected the Float 0.0, got {other:?}"),
    }
}

#[test]
fn into_value_f64_non_finite_fails() {
    for f in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let err = f
            .into_value()
            .expect_err("a non-finite f64 has no silt value");
        assert!(err.contains("non-finite float"), "got: {err}");
    }
}
