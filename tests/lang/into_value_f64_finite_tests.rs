//! `IntoValue for f64` builds a `Float` only from a finite value.
//!
//! A silt `Float` is always finite and never `-0.0`. A foreign function
//! returning NaN or an infinity has no silt value to return, so the
//! conversion fails, and a function registered with `register_fn0/1/2`
//! raises a runtime error that names it.

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::value::{IntoValue, Value};
use silt::vm::Vm;
use std::sync::Arc;

#[test]
fn into_value_f64_finite_builds_float() {
    assert_eq!(1.5_f64.into_value(), Ok(Value::Float(1.5)));
    assert_eq!(f64::MAX.into_value(), Ok(Value::Float(f64::MAX)));
    match (-0.0_f64).into_value() {
        Ok(Value::Float(x)) => assert!(x.is_sign_positive(), "-0.0 must become 0.0"),
        other => panic!("expected Value::Float(0.0), got {other:?}"),
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

#[test]
fn foreign_function_returning_nan_raises() {
    let tokens = Lexer::new("fn main() { get_nan() }")
        .tokenize()
        .expect("lexer error");
    let program = Parser::new(tokens).parse_program().expect("parse error");
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    vm.register_fn0("get_nan", || f64::NAN).unwrap();
    let err = vm.run(script).expect_err("a NaN result must raise");
    assert!(
        err.to_string().contains("get_nan: non-finite float result"),
        "got: {err}"
    );
}
