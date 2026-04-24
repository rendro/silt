//! Round 60 L12 lock: `IntoValue` impls for `Vec<Value>`, `Option<T>`,
//! and `Result<T, String>` (src/value.rs:1709, :1715, :1724) have no
//! in-tree callers but ARE intentional embedder-support surface — FFI
//! users returning these from `register_fn0`/`register_fn1`/`register_fn2`
//! closures rely on them for automatic marshalling back into silt
//! `Value`s.
//!
//! Prior precedent at tests/dead_code_lock_tests.rs:349 deleted
//! `register_fn3` under similar "no callers" conditions, but the
//! generic return marshalling differs: the embedder-facing surface
//! stays (Option/Result/Vec returns from FFI closures are a common
//! shape), so we LOCK rather than delete.
//!
//! This file exercises each trait via a tiny silt program running
//! against a `Vm` with three registered FFI functions.

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::value::Value;
use silt::vm::Vm;
use std::sync::Arc;

fn compile_and_run(vm: &mut Vm, src: &str) -> Value {
    let tokens = Lexer::new(src).tokenize().expect("lexer");
    let mut program = Parser::new(tokens).parse_program().expect("parser");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile");
    let script = Arc::new(functions.into_iter().next().unwrap());
    vm.run(script).expect("runtime")
}

#[test]
fn ffi_returning_option_marshals_to_variant() {
    // Exercises `impl<T: IntoValue> IntoValue for Option<T>` at
    // src/value.rs:1715. `Some(x)` must round-trip to
    // `Value::Variant("Some", [x])`; `None` to
    // `Value::Variant("None", [])`.
    let mut vm = Vm::new();
    vm.register_fn1("maybe_positive", |x: i64| -> Option<i64> {
        if x > 0 { Some(x) } else { None }
    })
    .unwrap();

    let got = compile_and_run(&mut vm, "fn main() { maybe_positive(7) }");
    assert_eq!(got, Value::Variant("Some".into(), vec![Value::Int(7)]));

    let got = compile_and_run(&mut vm, "fn main() { maybe_positive(-3) }");
    assert_eq!(got, Value::Variant("None".into(), vec![]));
}

#[test]
fn ffi_returning_result_marshals_to_variant() {
    // Exercises `impl<T: IntoValue> IntoValue for Result<T, String>`
    // at src/value.rs:1724. `Ok(x)` → `Variant("Ok", [x])`;
    // `Err(msg)` → `Variant("Err", [String(msg)])`.
    let mut vm = Vm::new();
    vm.register_fn1("safe_reciprocal", |x: i64| -> Result<i64, String> {
        if x != 0 {
            Ok(100 / x)
        } else {
            Err("div by zero".to_string())
        }
    })
    .unwrap();

    let got = compile_and_run(&mut vm, "fn main() { safe_reciprocal(5) }");
    assert_eq!(got, Value::Variant("Ok".into(), vec![Value::Int(20)]));

    let got = compile_and_run(&mut vm, "fn main() { safe_reciprocal(0) }");
    assert_eq!(
        got,
        Value::Variant("Err".into(), vec![Value::String("div by zero".into())])
    );
}

#[test]
fn ffi_returning_vec_marshals_to_list() {
    // Exercises `impl IntoValue for Vec<Value>` at src/value.rs:1709.
    // The returned Vec should come back as `Value::List(Arc<Vec<…>>)`.
    let mut vm = Vm::new();
    vm.register_fn1("range_up_to", |n: i64| -> Vec<Value> {
        (0..n).map(Value::Int).collect()
    })
    .unwrap();

    let got = compile_and_run(&mut vm, "fn main() { range_up_to(4) }");
    assert_eq!(
        got,
        Value::List(Arc::new(vec![
            Value::Int(0),
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
        ]))
    );

    let got = compile_and_run(&mut vm, "fn main() { range_up_to(0) }");
    assert_eq!(got, Value::List(Arc::new(vec![])));
}
