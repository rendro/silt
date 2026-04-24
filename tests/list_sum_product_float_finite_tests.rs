//! Regression test for audit GAP #3: `list.sum_float` / `list.product_float`
//! must not produce a non-finite `Value::Float`. The silt-wide invariant
//! (documented on `Value::Ord::cmp` in `src/value.rs`) is that every
//! `Value::Float` is finite (no NaN, no ±inf). `Vm::finite_float` enforces
//! this on arithmetic; prior to the fix in this change these two list
//! reducers accumulated into a raw `f64` and returned `Value::Float(total)`
//! unconditionally, which silently minted `Value::Float(inf)` on overflow
//! (e.g. `list.sum_float([f64::MAX, f64::MAX])`).
//!
//! The fix mirrors `float.min` / `float.max` / `float.clamp` in
//! `src/builtins/numeric.rs`: when the accumulator is not finite, widen to
//! `Value::ExtFloat(total)`.

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

/// `list.sum_float([f64::MAX, f64::MAX])` overflows to `+inf`. The fix must
/// widen this to `Value::ExtFloat(inf)` rather than minting a non-finite
/// `Value::Float`.
#[test]
fn list_sum_float_widens_to_extfloat_on_overflow() {
    let result = run(
        r#"
import list
import float
fn main() { list.sum_float([float.max_value, float.max_value]) }
"#,
    );
    assert_eq!(result, Value::ExtFloat(f64::INFINITY));
    // Defensive: pre-fix behavior would produce `Value::Float(inf)`. This
    // assertion locks against that regression.
    assert!(
        !matches!(result, Value::Float(_)),
        "list.sum_float must not return Value::Float for a non-finite accumulator; \
         got {result:?}"
    );
}

/// `list.product_float([f64::MAX, 2.0])` overflows to `+inf` in `f64`
/// multiplication. The fix must widen this to `Value::ExtFloat(inf)`.
#[test]
fn list_product_float_widens_to_extfloat_on_overflow() {
    let result = run(
        r#"
import list
import float
fn main() { list.product_float([float.max_value, 2.0]) }
"#,
    );
    assert_eq!(result, Value::ExtFloat(f64::INFINITY));
    assert!(
        !matches!(result, Value::Float(_)),
        "list.product_float must not return Value::Float for a non-finite accumulator; \
         got {result:?}"
    );
}

/// Sanity: the finite path still returns `Value::Float`. This ensures the
/// fix didn't over-widen (i.e. always return `ExtFloat`).
#[test]
fn sum_float_finite_stays_float() {
    let result = run(
        r#"
import list
fn main() { list.sum_float([1.0, 2.0, 3.0]) }
"#,
    );
    assert_eq!(result, Value::Float(6.0));
}

/// Same sanity check for `product_float`.
#[test]
fn product_float_finite_stays_float() {
    let result = run(
        r#"
import list
fn main() { list.product_float([2.0, 3.0, 4.0]) }
"#,
    );
    assert_eq!(result, Value::Float(24.0));
}
