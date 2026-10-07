use super::*;
use crate::bytecode::{Asm, Emitter, Function};
use crate::source::Span;
use crate::typeinfo::bv;

/// Helper: build a function of `arity` parameters with the emitter.
fn make_function_of(arity: u8, build: impl FnOnce(&mut Emitter)) -> Arc<Function> {
    let mut emitter = Emitter::new("<test>".to_string(), usize::from(arity), span()).unwrap();
    build(&mut emitter);
    Arc::new(emitter.finish(0).unwrap_or_else(|e| panic!("{e:?}")))
}

/// Helper: build a script with the emitter.
fn make_function(build: impl FnOnce(&mut Emitter)) -> Arc<Function> {
    make_function_of(0, build)
}

fn span() -> Span {
    Span::BUILTIN
}

/// Helper: compile and run a silt program through a session.
fn run_vm(source: &str) -> Value {
    run_vm_result(source).unwrap()
}

/// Helper: compile a silt program through a session and run it.
fn run_vm_result(source: &str) -> Result<Value, VmError> {
    let program = crate::session::testing::compile_str(source).unwrap_or_else(|e| panic!("{e:?}"));
    Vm::new(crate::HostIo::process()).run_program(&program)
}

// ── Phase 1 bytecode-level tests ──────────────────────────────

#[test]
fn test_constant_and_return() {
    let script = make_function(|e| {
        let idx = e.constant(Value::Int(42), span()).unwrap();
        e.emit(Asm::Constant { k: idx }, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_arithmetic_add_int() {
    let script = make_function(|e| {
        let a = e.constant(Value::Int(2), span()).unwrap();
        let b = e.constant(Value::Int(3), span()).unwrap();
        e.emit(Asm::Constant { k: a }, span()).unwrap();
        e.emit(Asm::Constant { k: b }, span()).unwrap();
        e.emit(Asm::Add, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Int(5));
}

#[test]
fn test_arithmetic_expression() {
    let script = make_function(|e| {
        let two = e.constant(Value::Int(2), span()).unwrap();
        let three = e.constant(Value::Int(3), span()).unwrap();
        let four = e.constant(Value::Int(4), span()).unwrap();
        e.emit(Asm::Constant { k: two }, span()).unwrap();
        e.emit(Asm::Constant { k: three }, span()).unwrap();
        e.emit(Asm::Constant { k: four }, span()).unwrap();
        e.emit(Asm::Mul, span()).unwrap();
        e.emit(Asm::Add, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Int(14));
}

#[test]
fn test_float_arithmetic() {
    let script = make_function(|e| {
        let a = e.constant(Value::Float(1.5), span()).unwrap();
        let b = e.constant(Value::Float(2.5), span()).unwrap();
        e.emit(Asm::Constant { k: a }, span()).unwrap();
        e.emit(Asm::Constant { k: b }, span()).unwrap();
        e.emit(Asm::Add, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Float(4.0));
}

#[test]
fn test_negate() {
    let script = make_function(|e| {
        let a = e.constant(Value::Int(10), span()).unwrap();
        e.emit(Asm::Constant { k: a }, span()).unwrap();
        e.emit(Asm::Negate, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Int(-10));
}

#[test]
fn test_comparison() {
    let script = make_function(|e| {
        let a = e.constant(Value::Int(3), span()).unwrap();
        let b = e.constant(Value::Int(5), span()).unwrap();
        e.emit(Asm::Constant { k: a }, span()).unwrap();
        e.emit(Asm::Constant { k: b }, span()).unwrap();
        e.emit(Asm::Lt, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn test_boolean_not() {
    let script = make_function(|e| {
        e.emit(Asm::True, span()).unwrap();
        e.emit(Asm::Not, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Bool(false));
}

#[test]
fn test_globals() {
    let script = make_function(|e| {
        let val = e.constant(Value::Int(42), span()).unwrap();
        e.emit(Asm::Constant { k: val }, span()).unwrap();
        e.emit(Asm::SetGlobal { slot: 0 }, span()).unwrap();
        e.emit(Asm::GetGlobal { slot: 0 }, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    vm.globals.push(None);
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_locals() {
    let script = make_function(|e| {
        let val = e.constant(Value::Int(10), span()).unwrap();
        e.emit(Asm::Unit, span()).unwrap();
        e.emit(Asm::Constant { k: val }, span()).unwrap();
        e.emit(Asm::SetLocal { slot: 0 }, span()).unwrap();
        e.emit(Asm::Pop, span()).unwrap();
        e.emit(Asm::GetLocal { slot: 0 }, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Int(10));
}

#[test]
fn test_string_concat() {
    let script = make_function(|e| {
        let a = e
            .constant(Value::String("hello".to_string()), span())
            .unwrap();
        let b = e.constant(Value::String(" ".to_string()), span()).unwrap();
        let c = e
            .constant(Value::String("world".to_string()), span())
            .unwrap();
        e.emit(Asm::Constant { k: a }, span()).unwrap();
        e.emit(Asm::Constant { k: b }, span()).unwrap();
        e.emit(Asm::Constant { k: c }, span()).unwrap();
        e.emit(Asm::StringConcat { count: 3 }, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::String("hello world".to_string()));
}

#[test]
fn test_display_value() {
    let script = make_function(|e| {
        let val = e.constant(Value::Int(42), span()).unwrap();
        e.emit(Asm::Constant { k: val }, span()).unwrap();
        e.emit(Asm::DisplayValue, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::String("42".to_string()));
}

#[test]
fn test_jump_if_false() {
    let script = make_function(|e| {
        let one = e.constant(Value::Int(1), span()).unwrap();
        let two = e.constant(Value::Int(2), span()).unwrap();
        e.emit(Asm::False, span()).unwrap();
        let other = e.label();
        let end = e.label();
        e.emit(Asm::JumpIfFalse { to: other }, span()).unwrap();
        e.emit(Asm::Constant { k: one }, span()).unwrap();
        e.emit(Asm::Jump { to: end }, span()).unwrap();
        e.bind(other, span()).unwrap();
        e.emit(Asm::Constant { k: two }, span()).unwrap();
        e.bind(end, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Int(2));
}

#[test]
fn test_builtin_println() {
    let script = make_function(|e| {
        let name = e
            .constant(Value::String("println".to_string()), span())
            .unwrap();
        let val = e.constant(Value::Int(42), span()).unwrap();
        e.emit(Asm::Constant { k: val }, span()).unwrap();
        e.emit(Asm::CallBuiltin { name, argc: 1 }, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Unit);
}

#[test]
fn test_make_tuple() {
    let script = make_function(|e| {
        let a = e.constant(Value::Int(1), span()).unwrap();
        let b = e.constant(Value::Int(2), span()).unwrap();
        e.emit(Asm::Constant { k: a }, span()).unwrap();
        e.emit(Asm::Constant { k: b }, span()).unwrap();
        e.emit(Asm::MakeTuple { count: 2 }, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Tuple(vec![Value::Int(1), Value::Int(2)]));
}

#[test]
fn test_make_list() {
    let script = make_function(|e| {
        let a = e.constant(Value::Int(10), span()).unwrap();
        let b = e.constant(Value::Int(20), span()).unwrap();
        e.emit(Asm::Constant { k: a }, span()).unwrap();
        e.emit(Asm::Constant { k: b }, span()).unwrap();
        e.emit(Asm::MakeList { count: 2 }, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script).unwrap();
    assert_eq!(
        result,
        Value::List(Arc::new(vec![Value::Int(10), Value::Int(20)]))
    );
}

#[test]
fn test_division_by_zero() {
    let script = make_function(|e| {
        let a = e.constant(Value::Int(10), span()).unwrap();
        let b = e.constant(Value::Int(0), span()).unwrap();
        e.emit(Asm::Constant { k: a }, span()).unwrap();
        e.emit(Asm::Constant { k: b }, span()).unwrap();
        e.emit(Asm::Div, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script);
    assert!(result.is_err());
    assert!(result.unwrap_err().message.contains("division by zero"));
}

#[test]
fn test_unit_and_pop() {
    let script = make_function(|e| {
        let val = e.constant(Value::Int(99), span()).unwrap();
        e.emit(Asm::Unit, span()).unwrap();
        e.emit(Asm::Pop, span()).unwrap();
        e.emit(Asm::Constant { k: val }, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Int(99));
}

#[test]
fn test_dup() {
    let script = make_function(|e| {
        let val = e.constant(Value::Int(5), span()).unwrap();
        e.emit(Asm::Constant { k: val }, span()).unwrap();
        e.emit(Asm::Dup, span()).unwrap();
        e.emit(Asm::Add, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Int(10));
}

#[test]
fn test_eq_neq() {
    let script = make_function(|e| {
        let val = e.constant(Value::Int(5), span()).unwrap();
        e.emit(Asm::Constant { k: val }, span()).unwrap();
        e.emit(Asm::Constant { k: val }, span()).unwrap();
        e.emit(Asm::Eq, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    assert_eq!(vm.run(script).unwrap(), Value::Bool(true));

    let script2 = make_function(|e| {
        let a = e.constant(Value::Int(5), span()).unwrap();
        let b = e.constant(Value::Int(3), span()).unwrap();
        e.emit(Asm::Constant { k: a }, span()).unwrap();
        e.emit(Asm::Constant { k: b }, span()).unwrap();
        e.emit(Asm::Neq, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm2 = Vm::new(crate::HostIo::process());
    assert_eq!(vm2.run(script2).unwrap(), Value::Bool(true));
}

#[test]
fn test_sub_int() {
    let script = make_function(|e| {
        let a = e.constant(Value::Int(10), span()).unwrap();
        let b = e.constant(Value::Int(3), span()).unwrap();
        e.emit(Asm::Constant { k: a }, span()).unwrap();
        e.emit(Asm::Constant { k: b }, span()).unwrap();
        e.emit(Asm::Sub, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Int(7));
}

#[test]
fn test_sub_int_underflow() {
    let script = make_function(|e| {
        let a = e.constant(Value::Int(i64::MIN), span()).unwrap();
        let b = e.constant(Value::Int(1), span()).unwrap();
        e.emit(Asm::Constant { k: a }, span()).unwrap();
        e.emit(Asm::Constant { k: b }, span()).unwrap();
        e.emit(Asm::Sub, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script);
    assert!(result.is_err());
    assert!(result.unwrap_err().message.contains("integer overflow"));
}

#[test]
fn test_sub_float() {
    let script = make_function(|e| {
        let a = e.constant(Value::Float(5.5), span()).unwrap();
        let b = e.constant(Value::Float(2.25), span()).unwrap();
        e.emit(Asm::Constant { k: a }, span()).unwrap();
        e.emit(Asm::Constant { k: b }, span()).unwrap();
        e.emit(Asm::Sub, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Float(3.25));
}

#[test]
fn test_mod_int() {
    let script = make_function(|e| {
        let a = e.constant(Value::Int(10), span()).unwrap();
        let b = e.constant(Value::Int(3), span()).unwrap();
        e.emit(Asm::Constant { k: a }, span()).unwrap();
        e.emit(Asm::Constant { k: b }, span()).unwrap();
        e.emit(Asm::Mod, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Int(1));
}

#[test]
fn test_mod_int_by_zero() {
    let script = make_function(|e| {
        let a = e.constant(Value::Int(10), span()).unwrap();
        let b = e.constant(Value::Int(0), span()).unwrap();
        e.emit(Asm::Constant { k: a }, span()).unwrap();
        e.emit(Asm::Constant { k: b }, span()).unwrap();
        e.emit(Asm::Mod, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script);
    assert!(result.is_err());
    assert!(result.unwrap_err().message.contains("modulo by zero"));
}

#[test]
fn test_mod_float() {
    let script = make_function(|e| {
        let a = e.constant(Value::Float(5.5), span()).unwrap();
        let b = e.constant(Value::Float(2.0), span()).unwrap();
        e.emit(Asm::Constant { k: a }, span()).unwrap();
        e.emit(Asm::Constant { k: b }, span()).unwrap();
        e.emit(Asm::Mod, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Float(1.5));
}

#[test]
fn test_gt_int() {
    let script = make_function(|e| {
        let a = e.constant(Value::Int(7), span()).unwrap();
        let b = e.constant(Value::Int(3), span()).unwrap();
        e.emit(Asm::Constant { k: a }, span()).unwrap();
        e.emit(Asm::Constant { k: b }, span()).unwrap();
        e.emit(Asm::Gt, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn test_gt_float() {
    let script = make_function(|e| {
        let a = e.constant(Value::Float(1.5), span()).unwrap();
        let b = e.constant(Value::Float(2.5), span()).unwrap();
        e.emit(Asm::Constant { k: a }, span()).unwrap();
        e.emit(Asm::Constant { k: b }, span()).unwrap();
        e.emit(Asm::Gt, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Bool(false));
}

#[test]
fn test_geq_int() {
    let script = make_function(|e| {
        let a = e.constant(Value::Int(5), span()).unwrap();
        let b = e.constant(Value::Int(5), span()).unwrap();
        e.emit(Asm::Constant { k: a }, span()).unwrap();
        e.emit(Asm::Constant { k: b }, span()).unwrap();
        e.emit(Asm::Geq, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn test_geq_float() {
    let script = make_function(|e| {
        let a = e.constant(Value::Float(4.0), span()).unwrap();
        let b = e.constant(Value::Float(4.5), span()).unwrap();
        e.emit(Asm::Constant { k: a }, span()).unwrap();
        e.emit(Asm::Constant { k: b }, span()).unwrap();
        e.emit(Asm::Geq, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Bool(false));
}

#[test]
fn test_leq_int() {
    let script = make_function(|e| {
        let a = e.constant(Value::Int(3), span()).unwrap();
        let b = e.constant(Value::Int(3), span()).unwrap();
        e.emit(Asm::Constant { k: a }, span()).unwrap();
        e.emit(Asm::Constant { k: b }, span()).unwrap();
        e.emit(Asm::Leq, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn test_leq_float() {
    let script = make_function(|e| {
        let a = e.constant(Value::Float(2.5), span()).unwrap();
        let b = e.constant(Value::Float(1.5), span()).unwrap();
        e.emit(Asm::Constant { k: a }, span()).unwrap();
        e.emit(Asm::Constant { k: b }, span()).unwrap();
        e.emit(Asm::Leq, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Bool(false));
}

#[test]
fn test_negate_float() {
    let script = make_function(|e| {
        let a = e.constant(Value::Float(3.5), span()).unwrap();
        e.emit(Asm::Constant { k: a }, span()).unwrap();
        e.emit(Asm::Negate, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Float(-3.5));
}

#[test]
fn test_negate_int_overflow() {
    let script = make_function(|e| {
        let a = e.constant(Value::Int(i64::MIN), span()).unwrap();
        e.emit(Asm::Constant { k: a }, span()).unwrap();
        e.emit(Asm::Negate, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script);
    assert!(result.is_err());
    assert!(result.unwrap_err().message.contains("integer overflow"));
}

// ── Phase 2 end-to-end tests ──────────────────────────────────

#[test]
fn test_e2e_hello_world() {
    run_vm(r#"fn main() { println("hello") }"#);
}

#[test]
fn test_e2e_arithmetic() {
    let result = run_vm(r#"fn main() { 2 + 3 * 4 }"#);
    assert_eq!(result, Value::Int(14));
}

#[test]
fn test_e2e_function_call() {
    let result = run_vm(
        r#"
            fn add(a, b) { a + b }
            fn main() { add(10, 20) }
        "#,
    );
    assert_eq!(result, Value::Int(30));
}

#[test]
fn test_e2e_let_binding() {
    let result = run_vm(
        r#"
            fn main() {
                let x = 42
                x
            }
        "#,
    );
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_e2e_let_and_string_interp() {
    run_vm(
        r#"
            fn main() {
                let x = 42
                println("x = {x}")
            }
        "#,
    );
}

#[test]
fn test_e2e_multiple_functions() {
    let result = run_vm(
        r#"
            fn double(n) { n * 2 }
            fn add_one(n) { n + 1 }
            fn main() { add_one(double(5)) }
        "#,
    );
    assert_eq!(result, Value::Int(11));
}

#[test]
fn test_e2e_recursion() {
    let result = run_vm(
        r#"
            fn factorial(n) {
                match n {
                    0 -> 1
                    _ -> n * factorial(n - 1)
                }
            }
            fn main() { factorial(5) }
        "#,
    );
    assert_eq!(result, Value::Int(120));
}

#[test]
fn test_e2e_string_operations() {
    let result = run_vm(
        r#"
            import string

            fn main() {
                let s = "hello, world"
                string.length(s)
            }
        "#,
    );
    assert_eq!(result, Value::Int(12));
}

#[test]
fn test_e2e_list_operations() {
    let result = run_vm(
        r#"
            import list

            fn main() {
                let xs = [1, 2, 3, 4, 5]
                list.length(xs)
            }
        "#,
    );
    assert_eq!(result, Value::Int(5));
}

#[test]
fn test_e2e_test_assert() {
    run_vm(
        r#"
            import test

            fn main() {
                test.assert_eq(2 + 2, 4)
            }
        "#,
    );
}

#[test]
fn test_e2e_nested_calls() {
    let result = run_vm(
        r#"
            fn f(x) { x + 1 }
            fn g(x) { f(x) * 2 }
            fn main() { g(10) }
        "#,
    );
    assert_eq!(result, Value::Int(22));
}

#[test]
fn test_e2e_match_int() {
    let result = run_vm(
        r#"
            fn classify(n) {
                match n {
                    0 -> "zero"
                    1 -> "one"
                    _ -> "other"
                }
            }
            fn main() { classify(1) }
        "#,
    );
    assert_eq!(result, Value::String("one".into()));
}

#[test]
fn test_e2e_boolean_logic() {
    let result = run_vm(
        r#"
            fn main() {
                let a = true
                let b = false
                a && !b
            }
        "#,
    );
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn test_e2e_builtin_println_call() {
    // Test that println works when called as a regular function via globals
    run_vm(
        r#"
            fn main() {
                println("testing 1 2 3")
            }
        "#,
    );
}

#[test]
fn test_e2e_variant_constructor() {
    let result = run_vm(
        r#"
            fn main() {
                let x = Some(42)
                x
            }
        "#,
    );
    assert_eq!(result, Value::variant(bv::SOME, vec![Value::Int(42)]));
}

#[test]
fn test_e2e_int_to_string() {
    let result = run_vm(
        r#"
            import int

            fn main() {
                int.to_string(42)
            }
        "#,
    );
    assert_eq!(result, Value::String("42".into()));
}

#[test]
fn test_e2e_list_append() {
    let result = run_vm(
        r#"
            import list

            fn main() {
                let xs = [1, 2, 3]
                list.append(xs, 4)
            }
        "#,
    );
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4)
        ]))
    );
}

// ── Phase 3: Closures and upvalue capture ────────────────────────

#[test]
fn test_closure_capture() {
    let result = run_vm(
        r#"
            fn make_adder(n) {
                { x -> x + n }
            }
            fn main() {
                let add5 = make_adder(5)
                add5(10)
            }
        "#,
    );
    assert_eq!(result, Value::Int(15));
}

#[test]
fn test_closure_in_map() {
    let result = run_vm(
        r#"
            import list

            fn main() {
                let factor = 10
                [1, 2, 3] |> list.map({ x -> x * factor })
            }
        "#,
    );
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::Int(10),
            Value::Int(20),
            Value::Int(30)
        ]))
    );
}

#[test]
fn test_higher_order() {
    let result = run_vm(
        r#"
            fn apply_twice(f, x) {
                f(f(x))
            }
            fn main() {
                let double = { x -> x * 2 }
                apply_twice(double, 3)
            }
        "#,
    );
    assert_eq!(result, Value::Int(12));
}

#[test]
fn test_closure_counter() {
    // Tests that closures capture values, not references
    let result = run_vm(
        r#"
            import list

            fn main() {
                let fns = [1, 2, 3] |> list.map({ n ->
                    { -> n * 10 }
                })
                fns |> list.map({ f -> f() })
            }
        "#,
    );
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::Int(10),
            Value::Int(20),
            Value::Int(30)
        ]))
    );
}

#[test]
fn test_closure_multiple_captures() {
    let result = run_vm(
        r#"
            fn make_linear(a, b) {
                { x -> a * x + b }
            }
            fn main() {
                let f = make_linear(3, 7)
                f(10)
            }
        "#,
    );
    assert_eq!(result, Value::Int(37));
}

#[test]
fn test_closure_transitive_capture() {
    // outer -> middle -> inner: transitive upvalue chaining
    let result = run_vm(
        r#"
            fn outer(x) {
                let make_inner = { ->
                    { -> x }
                }
                make_inner()
            }
            fn main() {
                let f = outer(42)
                f()
            }
        "#,
    );
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_closure_no_capture() {
    // Lambda that doesn't capture anything (no upvalues needed)
    let result = run_vm(
        r#"
            fn main() {
                let f = { x -> x + 1 }
                f(10)
            }
        "#,
    );
    assert_eq!(result, Value::Int(11));
}

#[test]
fn test_closure_with_filter() {
    let result = run_vm(
        r#"
            import list

            fn main() {
                let threshold = 3
                [1, 2, 3, 4, 5] |> list.filter({ x -> x > threshold })
            }
        "#,
    );
    assert_eq!(
        result,
        Value::List(Arc::new(vec![Value::Int(4), Value::Int(5)]))
    );
}

#[test]
fn test_closure_with_fold() {
    let result = run_vm(
        r#"
            import list

            fn main() {
                let offset = 100
                [1, 2, 3] |> list.fold(offset, { acc, x -> acc + x })
            }
        "#,
    );
    assert_eq!(result, Value::Int(106));
}

#[test]
fn test_let_tuple_destructure() {
    let result = run_vm(
        r#"
            fn main() {
                let (a, b) = (10, 20)
                a + b
            }
        "#,
    );
    assert_eq!(result, Value::Int(30));
}

#[test]
fn test_let_tuple_destructure_three() {
    let result = run_vm(
        r#"
            fn main() {
                let (a, b, c) = (1, 2, 3)
                a * 100 + b * 10 + c
            }
        "#,
    );
    assert_eq!(result, Value::Int(123));
}

#[test]
fn test_closure_returned_from_fn() {
    // A named function returns a closure that captures a parameter
    let result = run_vm(
        r#"
            fn multiplier(factor) {
                { x -> x * factor }
            }
            fn main() {
                let times3 = multiplier(3)
                let times7 = multiplier(7)
                times3(10) + times7(5)
            }
        "#,
    );
    assert_eq!(result, Value::Int(65));
}

#[test]
fn test_trailing_closure_with_capture() {
    // Pipe with trailing closure syntax { x -> ... }
    let result = run_vm(
        r#"
            import list

            fn main() {
                let factor = 10
                [1, 2, 3] |> list.map { x -> x * factor }
            }
        "#,
    );
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::Int(10),
            Value::Int(20),
            Value::Int(30)
        ]))
    );
}

#[test]
fn test_trailing_closure_filter_with_capture() {
    let result = run_vm(
        r#"
            import list

            fn main() {
                let limit = 3
                [1, 2, 3, 4, 5] |> list.filter { x -> x > limit }
            }
        "#,
    );
    assert_eq!(
        result,
        Value::List(Arc::new(vec![Value::Int(4), Value::Int(5)]))
    );
}

#[test]
fn test_chained_pipes_with_closures() {
    let result = run_vm(
        r#"
            import list

            fn main() {
                let offset = 10
                let cutoff = 13
                [1, 2, 3, 4, 5]
                    |> list.map({ x -> x + offset })
                    |> list.filter({ x -> x > cutoff })
            }
        "#,
    );
    assert_eq!(
        result,
        Value::List(Arc::new(vec![Value::Int(14), Value::Int(15)]))
    );
}

// ── Phase 4: Full pattern matching ──────────────────────────────

#[test]
fn test_match_int_literal() {
    let result = run_vm(
        r#"
            fn main() { match 42 {
              42 -> "yes"
              _ -> "no"
            } }
        "#,
    );
    assert_eq!(result, Value::String("yes".into()));
}

#[test]
fn test_match_int_fallthrough() {
    let result = run_vm(
        r#"
            fn main() { match 99 {
              42 -> "yes"
              _ -> "no"
            } }
        "#,
    );
    assert_eq!(result, Value::String("no".into()));
}

#[test]
fn test_match_string_literal() {
    let result = run_vm(
        r#"
            fn main() { match "hello" {
              "hello" -> 1
              _ -> 0
            } }
        "#,
    );
    assert_eq!(result, Value::Int(1));
}

#[test]
fn test_match_bool_literal() {
    let result = run_vm(
        r#"
            fn main() { match true {
              true -> "yes"
              false -> "no"
            } }
        "#,
    );
    assert_eq!(result, Value::String("yes".into()));
}

#[test]
fn test_match_float_literal() {
    let result = run_vm(
        r#"
            fn main() { match 3.14 {
              3.14 -> "pi"
              _ -> "other"
            } }
        "#,
    );
    assert_eq!(result, Value::String("pi".into()));
}

#[test]
fn test_match_tuple() {
    let result = run_vm(
        r#"
            fn main() {
                match (1, 2) {
                  (1, y) -> y * 10
                  _ -> 0
                }
            }
        "#,
    );
    assert_eq!(result, Value::Int(20));
}

#[test]
fn test_match_tuple_wildcard() {
    let result = run_vm(
        r#"
            fn main() {
                match (1, 2) {
                  (_, y) -> y + 100
                  _ -> 0
                }
            }
        "#,
    );
    assert_eq!(result, Value::Int(102));
}

#[test]
fn test_match_list_exact() {
    let result = run_vm(
        r#"
            fn main() {
                match [1, 2, 3] {
                  [a, b, c] -> a + b + c
                  _ -> 0
                }
            }
        "#,
    );
    assert_eq!(result, Value::Int(6));
}

#[test]
fn test_match_list_exact_mismatch() {
    let result = run_vm(
        r#"
            fn main() {
                match [1, 2] {
                  [a, b, c] -> a + b + c
                  _ -> 99
                }
            }
        "#,
    );
    assert_eq!(result, Value::Int(99));
}

#[test]
fn test_match_list_head_rest() {
    let result = run_vm(
        r#"
            fn main() {
                match [10, 20, 30] {
                  [h, ..t] -> h
                  _ -> 0
                }
            }
        "#,
    );
    assert_eq!(result, Value::Int(10));
}

#[test]
fn test_match_list_rest_value() {
    let result = run_vm(
        r#"
            fn main() {
                match [10, 20, 30] {
                  [_, ..t] -> t
                  _ -> []
                }
            }
        "#,
    );
    assert_eq!(
        result,
        Value::List(Arc::new(vec![Value::Int(20), Value::Int(30)]))
    );
}

#[test]
fn test_match_list_empty_rest() {
    let result = run_vm(
        r#"
            fn main() {
                match [10] {
                  [h, ..t] -> t
                  _ -> [99]
                }
            }
        "#,
    );
    assert_eq!(result, Value::List(Arc::new(vec![])));
}

#[test]
fn test_match_constructor_simple() {
    let result = run_vm(
        r#"
            fn main() {
                match Some(42) {
                  Some(n) -> n
                  None -> 0
                }
            }
        "#,
    );
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_match_constructor_none() {
    let result = run_vm(
        r#"
            fn main() {
                match None {
                  Some(n) -> n
                  None -> 0
                }
            }
        "#,
    );
    assert_eq!(result, Value::Int(0));
}

#[test]
fn test_match_constructor_ok_err() {
    let result = run_vm(
        r#"
            fn main() {
                let v = Ok(42)
                match v {
                  Ok(n) -> n
                  Err(_) -> -1
                }
            }
        "#,
    );
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_match_nested_constructor_tuple() {
    let result = run_vm(
        r#"
            fn main() {
                match Some((1, 2)) {
                  Some((a, b)) -> a + b
                  None -> 0
                }
            }
        "#,
    );
    assert_eq!(result, Value::Int(3));
}

#[test]
fn test_match_nested_constructor_list() {
    let result = run_vm(
        r#"
            fn main() {
                match Some([10, 20]) {
                    Some([h, ..t]) -> h
                    _ -> 0
                }
            }
        "#,
    );
    assert_eq!(result, Value::Int(10));
}

#[test]
fn test_match_or_pattern() {
    let result = run_vm(
        r#"
            fn main() {
                match 2 {
                  1 | 2 | 3 -> "small"
                  _ -> "big"
                }
            }
        "#,
    );
    assert_eq!(result, Value::String("small".into()));
}

#[test]
fn test_match_or_pattern_no_match() {
    let result = run_vm(
        r#"
            fn main() {
                match 5 {
                  1 | 2 | 3 -> "small"
                  _ -> "big"
                }
            }
        "#,
    );
    assert_eq!(result, Value::String("big".into()));
}

#[test]
fn test_match_guard() {
    let result = run_vm(
        r#"
            fn main() {
                match 42 {
                    n when n > 100 -> "big"
                    n when n > 0 -> "positive"
                    _ -> "other"
                }
            }
        "#,
    );
    assert_eq!(result, Value::String("positive".into()));
}

#[test]
fn test_match_guard_all_fail() {
    let result = run_vm(
        r#"
            fn main() {
                match -5 {
                    n when n > 100 -> "big"
                    n when n > 0 -> "positive"
                    _ -> "other"
                }
            }
        "#,
    );
    assert_eq!(result, Value::String("other".into()));
}

#[test]
fn test_match_range() {
    let result = run_vm(
        r#"
            fn main() {
                match 5 {
                  1..10 -> "in range"
                  _ -> "out"
                }
            }
        "#,
    );
    assert_eq!(result, Value::String("in range".into()));
}

#[test]
fn test_match_range_boundary() {
    let result = run_vm(
        r#"
            fn main() {
                match 10 {
                  1..10 -> "in range"
                  _ -> "out"
                }
            }
        "#,
    );
    assert_eq!(result, Value::String("in range".into()));
}

#[test]
fn test_match_range_out() {
    let result = run_vm(
        r#"
            fn main() {
                match 11 {
                  1..10 -> "in range"
                  _ -> "out"
                }
            }
        "#,
    );
    assert_eq!(result, Value::String("out".into()));
}

#[test]
fn test_guardless_match() {
    let result = run_vm(
        r#"
            fn main() {
                let x = 5
                match {
                  x > 10 -> "big"
                  x > 0 -> "positive"
                  _ -> "other"
                }
            }
        "#,
    );
    assert_eq!(result, Value::String("positive".into()));
}

#[test]
fn test_guardless_match_default() {
    let result = run_vm(
        r#"
            fn main() {
                let x = -5
                match {
                  x > 10 -> "big"
                  x > 0 -> "positive"
                  _ -> "other"
                }
            }
        "#,
    );
    assert_eq!(result, Value::String("other".into()));
}

#[test]
fn test_let_tuple_destructure_nested() {
    let result = run_vm(
        r#"
            fn main() {
                let (a, (b, c)) = (1, (2, 3))
                a + b + c
            }
        "#,
    );
    assert_eq!(result, Value::Int(6));
}

#[test]
fn test_match_multiple_arms() {
    let result = run_vm(
        r#"
            fn classify(n) {
                match n {
                    0 -> "zero"
                    1 -> "one"
                    2 -> "two"
                    _ -> "many"
                }
            }
            fn main() {
                classify(2)
            }
        "#,
    );
    assert_eq!(result, Value::String("two".into()));
}

#[test]
fn test_match_ident_binding() {
    let result = run_vm(
        r#"
            fn main() {
                match 42 { x -> x + 1 }
            }
        "#,
    );
    assert_eq!(result, Value::Int(43));
}

#[test]
fn test_match_wildcard() {
    let result = run_vm(
        r#"
            fn main() {
                match 42 { _ -> "matched" }
            }
        "#,
    );
    assert_eq!(result, Value::String("matched".into()));
}

#[test]
fn test_match_constructor_with_guard() {
    let result = run_vm(
        r#"
            fn main() {
                match Some(5) {
                    Some(n) when n > 10 -> 0
                    Some(n) -> n * 2
                    None -> 0
                }
            }
        "#,
    );
    assert_eq!(result, Value::Int(10));
}

#[test]
fn test_when_bool_guard() {
    let result = run_vm(
        r#"
            fn safe_div(a, b) {
                when b != 0 else { return Err("div by zero") }
                Ok(a / b)
            }
            fn main() {
                match safe_div(10, 2) {
                  Ok(n) -> n
                  Err(_) -> -1
                }
            }
        "#,
    );
    assert_eq!(result, Value::Int(5));
}

#[test]
fn test_when_bool_guard_fails() {
    let result = run_vm(
        r#"
            fn safe_div(a, b) {
                when b != 0 else { return Err("div by zero") }
                Ok(a / b)
            }
            fn main() {
                match safe_div(10, 0) {
                  Ok(n) -> n
                  Err(_) -> -1
                }
            }
        "#,
    );
    assert_eq!(result, Value::Int(-1));
}

#[test]
fn test_match_list_two_elems_with_rest() {
    let result = run_vm(
        r#"
            fn main() {
                match [1, 2, 3, 4, 5] {
                    [a, b, ..rest] -> a + b
                    _ -> 0
                }
            }
        "#,
    );
    assert_eq!(result, Value::Int(3));
}

#[test]
fn test_match_tuple_three() {
    let result = run_vm(
        r#"
            fn main() {
                match (10, 20, 30) {
                    (a, b, c) -> a + b + c
                    _ -> 0
                }
            }
        "#,
    );
    assert_eq!(result, Value::Int(60));
}

#[test]
fn test_match_nested_tuple_in_list() {
    // Match a list where elements are extracted as simple ints
    let result = run_vm(
        r#"
            fn main() {
                match [1, 2] {
                    [a, b] -> a * 100 + b
                    _ -> 0
                }
            }
        "#,
    );
    assert_eq!(result, Value::Int(102));
}

#[test]
fn test_match_constructor_wildcard_field() {
    let result = run_vm(
        r#"
            fn main() {
                match Ok(42) {
                  Ok(_) -> "is ok"
                  Err(_) -> "is err"
                }
            }
        "#,
    );
    assert_eq!(result, Value::String("is ok".into()));
}

#[test]
fn test_match_or_pattern_constructor() {
    let result = run_vm(
        r#"
            fn main() {
                match None {
                  Some(_) -> "has value"
                  None -> "empty"
                }
            }
        "#,
    );
    assert_eq!(result, Value::String("empty".into()));
}

#[test]
fn test_match_deeply_nested() {
    // Some((a, [h, ..t]))
    let result = run_vm(
        r#"
            fn main() {
                match Some((1, [10, 20, 30])) {
                    Some((a, [h, ..t])) -> a + h
                    _ -> 0
                }
            }
        "#,
    );
    assert_eq!(result, Value::Int(11));
}

#[test]
fn test_guardless_match_first_branch() {
    let result = run_vm(
        r#"
            fn main() {
                let x = 50
                match {
                  x > 10 -> "big"
                  x > 0 -> "positive"
                  _ -> "other"
                }
            }
        "#,
    );
    assert_eq!(result, Value::String("big".into()));
}

#[test]
fn test_match_in_function() {
    let result = run_vm(
        r#"
            fn describe(opt) {
                match opt {
                    Some(n) when n > 0 -> "positive"
                    Some(0) -> "zero"
                    Some(_) -> "negative"
                    None -> "nothing"
                }
            }
            fn main() {
                describe(Some(0))
            }
        "#,
    );
    assert_eq!(result, Value::String("zero".into()));
}

#[test]
fn test_match_float_range() {
    let result = run_vm(
        r#"
            fn main() {
                match 3.14 {
                    0.0..1.0 -> "small"
                    1.0..5.0 -> "medium"
                    _ -> "large"
                }
            }
        "#,
    );
    assert_eq!(result, Value::String("medium".into()));
}

#[test]
fn test_match_float_range_out() {
    let result = run_vm(
        r#"
            fn main() {
                match 10.0 {
                    0.0..1.0 -> "small"
                    1.0..5.0 -> "medium"
                    _ -> "large"
                }
            }
        "#,
    );
    assert_eq!(result, Value::String("large".into()));
}

#[test]
fn test_match_recursive_list_sum() {
    // Use match to destructure a list recursively
    let result = run_vm(
        r#"
            fn sum(xs) {
                match xs {
                    [] -> 0
                    [h, ..t] -> h + sum(t)
                }
            }
            fn main() {
                sum([1, 2, 3, 4, 5])
            }
        "#,
    );
    assert_eq!(result, Value::Int(15));
}

#[test]
fn test_match_map_pattern() {
    let result = run_vm(
        r#"
            fn main() {
                let m = #{"name": "Alice", "age": "30"}
                match m {
                    #{"name": n} -> n
                    _ -> "unknown"
                }
            }
        "#,
    );
    assert_eq!(result, Value::String("Alice".into()));
}

#[test]
fn test_match_constructor_nested_or() {
    let result = run_vm(
        r#"
            fn main() {
                match 42 {
                    1 | 2 | 3 -> "tiny"
                    n when n > 40 -> "big"
                    _ -> "other"
                }
            }
        "#,
    );
    assert_eq!(result, Value::String("big".into()));
}

#[test]
fn test_match_tuple_nested_wildcard() {
    let result = run_vm(
        r#"
            fn main() {
                match (1, (2, 3)) {
                    (1, (_, c)) -> c * 10
                    _ -> 0
                }
            }
        "#,
    );
    assert_eq!(result, Value::Int(30));
}

#[test]
fn test_match_list_empty() {
    let result = run_vm(
        r#"
            fn main() {
                match [] {
                    [] -> "empty"
                    _ -> "not empty"
                }
            }
        "#,
    );
    assert_eq!(result, Value::String("empty".into()));
}

#[test]
fn test_let_constructor_destructure() {
    let result = run_vm(
        r#"
            fn main() {
                let x = Ok(42)
                match x {
                  Ok(n) -> n
                  Err(_) -> 0
                }
            }
        "#,
    );
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_match_multiple_constructors_sequence() {
    let result = run_vm(
        r#"
            fn process(items) {
                match items {
                    [] -> 0
                    [h, ..t] -> h + process(t)
                }
            }
            fn main() {
                process([10, 20, 30])
            }
        "#,
    );
    assert_eq!(result, Value::Int(60));
}

#[test]
fn test_match_pin_pattern() {
    let result = run_vm(
        r#"
            fn main() {
                let expected = 42
                match 42 {
                    ^expected -> "matched"
                    _ -> "nope"
                }
            }
        "#,
    );
    assert_eq!(result, Value::String("matched".into()));
}

#[test]
fn test_match_pin_pattern_no_match() {
    let result = run_vm(
        r#"
            fn main() {
                let expected = 42
                match 99 {
                    ^expected -> "matched"
                    _ -> "nope"
                }
            }
        "#,
    );
    assert_eq!(result, Value::String("nope".into()));
}

#[test]
fn test_when_pattern_match() {
    let result = run_vm(
        r#"
            fn extract(val) {
                when let Some(n) = val else { return -1 }
                n
            }
            fn main() {
                extract(Some(42))
            }
        "#,
    );
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_when_pattern_match_fails() {
    let result = run_vm(
        r#"
            fn extract(val) {
                when let Some(n) = val else { return -1 }
                n
            }
            fn main() {
                extract(None)
            }
        "#,
    );
    assert_eq!(result, Value::Int(-1));
}

#[test]
fn test_match_or_pattern_with_binding() {
    // Or-patterns where each alt binds the same variable
    let result = run_vm(
        r#"
            fn main() {
                match Some(5) {
                    Some(n) -> n * 2
                    None -> 0
                }
            }
        "#,
    );
    assert_eq!(result, Value::Int(10));
}

#[test]
fn test_match_guard_with_tuple() {
    let result = run_vm(
        r#"
            fn main() {
                match (3, 4) {
                    (a, b) when a + b > 10 -> 0
                    (a, b) -> a + b
                }
            }
        "#,
    );
    assert_eq!(result, Value::Int(7));
}

// ── Phase 5 tests ──────────────────────────────────────────

#[test]
fn test_loop_sum() {
    let result = run_vm(
        r#"
            fn main() {
                loop x = 0, sum = 0 {
                    match x >= 10 {
                        true -> sum
                        _ -> loop(x + 1, sum + x)
                    }
                }
            }
        "#,
    );
    assert_eq!(result, Value::Int(45));
}

#[test]
fn test_loop_factorial() {
    let result = run_vm(
        r#"
            fn main() {
                loop n = 10, acc = 1 {
                    match n <= 1 {
                        true -> acc
                        _ -> loop(n - 1, acc * n)
                    }
                }
            }
        "#,
    );
    assert_eq!(result, Value::Int(3628800));
}

#[test]
fn test_record_create_and_access() {
    let result = run_vm(
        r#"
            type User { name: String, age: Int }
            fn main() {
                let u = User { name: "Alice", age: 30 }
                u.age
            }
        "#,
    );
    assert_eq!(result, Value::Int(30));
}

#[test]
fn test_record_update() {
    let result = run_vm(
        r#"
            type User { name: String, age: Int }
            fn main() {
                let u = User { name: "Alice", age: 30 }
                let u2 = u.{ age: 31 }
                u2.age
            }
        "#,
    );
    assert_eq!(result, Value::Int(31));
}

#[test]
fn test_range_expression() {
    // 1..5 inclusive = [1, 2, 3, 4, 5], sum = 15
    let result = run_vm(
        r#"
            import list

            fn main() {
                let nums = 1..5
                nums |> list.fold(0) { acc, n -> acc + n }
            }
        "#,
    );
    assert_eq!(result, Value::Int(15));
}

#[test]
fn test_set_literal() {
    let result = run_vm(
        r#"
            import set

            fn main() {
                let s = #[1, 2, 3, 2, 1]
                set.length(s)
            }
        "#,
    );
    assert_eq!(result, Value::Int(3));
}

#[test]
fn test_question_mark_ok() {
    let result = run_vm(
        r#"
            import int

            fn parse_add(a, b) {
                let x = int.parse(a)?
                let y = int.parse(b)?
                Ok(x + y)
            }
            fn main() {
                match parse_add("10", "20") {
                    Ok(n) -> n
                    Err(_) -> -1
                }
            }
        "#,
    );
    assert_eq!(result, Value::Int(30));
}

#[test]
fn test_question_mark_err() {
    let result = run_vm(
        r#"
            import int

            fn parse_add(a, b) {
                let x = int.parse(a)?
                let y = int.parse(b)?
                Ok(x + y)
            }
            fn main() {
                match parse_add("10", "abc") {
                    Ok(n) -> n
                    Err(_) -> -1
                }
            }
        "#,
    );
    assert_eq!(result, Value::Int(-1));
}

#[test]
fn test_type_decl_variant_constructors() {
    let result = run_vm(
        r#"
            type Color { Red, Green, Blue }
            fn main() {
                let c = Red
                match c {
                  Red -> 1
                  Green -> 2
                  Blue -> 3
                }
            }
        "#,
    );
    assert_eq!(result, Value::Int(1));
}

#[test]
fn test_type_decl_variant_with_fields() {
    let result = run_vm(
        r#"
            type Shape { Circle(Float), Rect(Float, Float) }
            fn main() {
                let s = Circle(5.0)
                match s {
                    Circle(r) -> r
                    Rect(w, h) -> w + h
                }
            }
        "#,
    );
    assert_eq!(result, Value::Float(5.0));
}

#[test]
fn test_custom_display_trait() {
    let result = run_vm(
        r#"
            type Shape { Circle(Float), Rect(Float, Float) }
            trait Display for Shape {
                fn display(self) -> String {
                    match self {
                        Circle(r) -> "Circle"
                        Rect(w, h) -> "Rect"
                    }
                }
            }
            fn main() {
                let s = Circle(5.0)
                s.display()
            }
        "#,
    );
    assert_eq!(result, Value::String("Circle".to_string()));
}

#[test]
fn test_recursive_variant_eval() {
    let result = run_vm(
        r#"
            type Expr { Num(Int), Add(Expr, Expr) }
            fn eval(expr) {
                match expr {
                    Num(n) -> n
                    Add(l, r) -> eval(l) + eval(r)
                }
            }
            fn main() {
                eval(Add(Num(3), Num(5)))
            }
        "#,
    );
    assert_eq!(result, Value::Int(8));
}

#[test]
fn test_loop_in_function() {
    let result = run_vm(
        r#"
            fn sum_to(n) {
                loop i = 0, acc = 0 {
                    match i > n {
                        true -> acc
                        _ -> loop(i + 1, acc + i)
                    }
                }
            }
            fn main() {
                sum_to(100)
            }
        "#,
    );
    assert_eq!(result, Value::Int(5050));
}

// ── Concurrency tests ────────────────────────────────────────────

#[test]
fn test_spawn_join() {
    let result = run_vm(
        r#"
            import task

            fn main() {
                let t = task.spawn({ -> 42 })
                task.join(t)
            }
        "#,
    );
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_spawn_join_already_completed() {
    // Ensure task.join works when the fiber has already completed
    // before join is called (the original deadlock scenario).
    let result = run_vm(
        r#"
            import channel
            import task

            fn main() {
                let ch = channel.new(1)
                let t = task.spawn({ ->
                    channel.send(ch, "done")
                    99
                })
                -- Wait for the message, ensuring the fiber runs to completion
                let _ = channel.receive(ch)
                -- Now the fiber should already be completed
                task.join(t)
            }
        "#,
    );
    assert_eq!(result, Value::Int(99));
}

#[test]
fn test_spawn_join_multiple_completed() {
    // Multiple fibers that complete before join is called
    let result = run_vm(
        r#"
            import channel
            import task

            fn main() {
                let ch = channel.new(10)
                let t1 = task.spawn({ ->
                    channel.send(ch, 1)
                    10
                })
                let t2 = task.spawn({ ->
                    channel.send(ch, 2)
                    20
                })
                let t3 = task.spawn({ ->
                    channel.send(ch, 3)
                    30
                })
                -- Drain all messages so fibers complete
                let _ = channel.receive(ch)
                let _ = channel.receive(ch)
                let _ = channel.receive(ch)
                -- All fibers should be done; join should not deadlock
                let a = task.join(t1)
                let b = task.join(t2)
                let c = task.join(t3)
                a + b + c
            }
        "#,
    );
    assert_eq!(result, Value::Int(60));
}

// ── Scheduler integration tests ──────────────────────────────

#[test]
fn test_scheduler_task_completes() {
    // task.join returns the value directly on success
    let result = run_vm(
        r#"
            import task
            fn main() {
                let t = task.spawn({ -> 42 })
                task.join(t)
            }
            "#,
    );
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_scheduler_multiple_tasks() {
    let result = run_vm(
        r#"
            import task
            import list
            fn main() {
                let tasks = [1, 2, 3] |> list.map({ n -> task.spawn({ -> n * 10 }) })
                tasks |> list.map({ t -> task.join(t) })
            }
            "#,
    );
    if let Value::List(items) = &result {
        assert_eq!(items.len(), 3);
        // Values are returned directly (10, 20, 30) — order may vary
        let mut vals: Vec<i64> = items
            .iter()
            .map(|v| match v {
                Value::Int(n) => *n,
                other => panic!("expected Int, got {:?}", other),
            })
            .collect();
        vals.sort();
        assert_eq!(vals, vec![10, 20, 30]);
    } else {
        panic!("expected list, got {:?}", result);
    }
}

#[test]
fn test_scheduler_channel_communication() {
    // channel.receive wraps value in Message variant
    let result = run_vm(
        r#"
            import task
            import channel
            fn main() {
                let ch = channel.new()
                task.spawn({ -> channel.send(ch, 99) })
                channel.receive(ch)
            }
            "#,
    );
    assert_eq!(result, Value::variant(bv::MESSAGE, vec![Value::Int(99)]));
}

#[test]
fn test_scheduler_deadlock_detection() {
    // Deadlock: task.join propagates as a VmError
    let err = run_vm_result(
        r#"
            import task
            import channel
            fn main() {
                let ch = channel.new()
                let t = task.spawn({ -> channel.receive(ch) })
                task.join(t)
            }
            "#,
    )
    .unwrap_err();
    assert!(
        err.message.contains("deadlock"),
        "expected deadlock error, got: {}",
        err.message
    );
}

#[test]
fn test_scheduler_task_failure_propagates() {
    // task.join on a failed task propagates as a VmError
    let err = run_vm_result(
        r#"
            import task
            fn main() {
                let t = task.spawn({ -> 1 / 0 })
                task.join(t)
            }
            "#,
    )
    .unwrap_err();
    // Production message from src/vm/task.rs: the join-site wraps the
    // inner VmError as "joined task failed: <inner>".
    assert!(
        err.message == "joined task failed: division by zero",
        "expected exact join-site division error, got: {}",
        err.message
    );
}

// ── Higher-order builtin suspension tests (G4) ───────────────────────
//
// These tests exercise the iteration frame of the higher-order builtins
// (`vm::iterate`) when the task parks inside the user callback (via
// `task.join(task.spawn(...))`). They verify that the frame's state —
// especially a fold's running value — is the same when the task goes
// on.

#[test]
fn test_scheduler_list_fold_with_yielding_callback() {
    // list.fold's accumulator must round-trip across a suspension. If it
    // were lost, the sum would be wrong (likely 0 or a partial value).
    let result = run_vm(
        r#"
            import task
            import list
            fn main() {
                [1, 2, 3, 4, 5] |> list.fold(0, { acc, n ->
                    task.join(task.spawn({ -> acc + n }))
                })
            }
            "#,
    );
    assert_eq!(result, Value::Int(15));
}

#[test]
fn test_scheduler_list_fold_with_yielding_callback_string_acc() {
    // A second fold test that uses a non-integer accumulator to ensure
    // the suspension path preserves arbitrary Value types in the
    // accumulator (not just small Ints that might be copied trivially).
    let result = run_vm(
        r#"
            import task
            import list
            fn main() {
                ["a", "b", "c"] |> list.fold("", { acc, s ->
                    task.join(task.spawn({ -> "{acc}{s}" }))
                })
            }
            "#,
    );
    assert_eq!(result, Value::String("abc".into()));
}

#[test]
fn test_scheduler_list_filter_with_yielding_predicate() {
    // list.filter with a yielding predicate. Verifies both the count and
    // the order of surviving elements (filter must preserve source
    // order across suspensions).
    let result = run_vm(
        r#"
            import task
            import list
            fn main() {
                [1, 2, 3, 4, 5, 6] |> list.filter({ n ->
                    task.join(task.spawn({ -> n % 2 == 0 }))
                })
            }
            "#,
    );
    assert_eq!(
        result,
        Value::List(Arc::new(vec![Value::Int(2), Value::Int(4), Value::Int(6),]))
    );
}

// ── Regex cache eviction test (L5) ───────────────────────────────────
//
// The LRU cache in src/vm/runtime.rs holds at most 256 compiled regex
// patterns and evicts the oldest 64 when full. This test compiles more
// than 256 distinct patterns from silt code and then verifies that
// patterns compiled both before and after the eviction threshold still
// produce correct match results.

// ── Audit regression: TailCall arity check (3a4edd6 L1) ────────────

#[test]
fn test_tail_call_rejects_arity_mismatch() {
    // Locks in 3a4edd6 L1: `TailCall` must verify that the caller
    // pushed exactly `arity` arguments before mutating the current
    // frame. In well-typed silt programs the type checker always
    // matches arity, so this is reachable only when the checker is
    // wrong: a type-confusion error.
    //
    // The test bypasses the compiler by building a function with
    // arity 2 and a script that pushes only ONE argument before
    // emitting `TailCall 1`.
    use crate::bytecode::VmClosure;

    // Callee: expects 2 arguments.
    let closure = Arc::new(VmClosure {
        function: Arc::new(Function::returning_unit("two_arg".to_string(), 2)),
        upvalues: vec![],
    });

    // Script: push the closure, push ONE int, then TailCall argc=1.
    // This simulates a buggy emitter sending the wrong argc for a
    // known 2-arg function.
    let script = make_function(|e| {
        let closure_idx = e.constant(Value::VmClosure(closure), span()).unwrap();
        let arg_idx = e.constant(Value::Int(1), span()).unwrap();
        e.emit(Asm::Constant { k: closure_idx }, span()).unwrap();
        e.emit(Asm::Constant { k: arg_idx }, span()).unwrap();
        e.emit(Asm::TailCall { argc: 1 }, span()).unwrap(); // wrong argc
        e.emit(Asm::Return, span()).unwrap();
    });

    let mut vm = Vm::new(crate::HostIo::process());
    let err = vm.run(script).expect_err("expected arity-mismatch error");
    assert!(err.type_confusion);
    let msg = format!("{err}");
    assert!(
        msg.contains("two_arg") && msg.contains("expects 2") && msg.contains("got 1"),
        "expected arity error naming the callee and counts, got: {msg}"
    );
}

// ── Audit regression: shared channel/task ID counters (3a4edd6 L3) ──

#[test]
fn test_spawn_child_shares_channel_id_counter() {
    // Locks in 3a4edd6 L3: when a VM spawns a child (for task.spawn), the
    // channel ID counter must be shared via Arc so IDs remain globally
    // unique across parent and child. Before the fix, each VM had its
    // own `AtomicU64`, so a channel created in a spawned task could
    // collide with a channel created in the parent, leading to
    // scheduler confusion.
    let mut parent = Vm::new(crate::HostIo::process());
    let id0 = parent.next_channel_id();
    assert_eq!(id0, 0);

    let mut child = parent.spawn_child();
    // Child must observe the counter advanced by the parent — so its
    // next ID is 1, not 0. If the Arc were cloned-per-VM as a fresh
    // AtomicU64 (the bug), child would also return 0.
    let id1 = child.next_channel_id();
    assert_eq!(id1, 1, "child VM must share channel ID counter with parent");

    // Allocating again from the parent must see the child's advance.
    let id2 = parent.next_channel_id();
    assert_eq!(id2, 2, "parent must observe child's channel ID advance");
}

#[test]
fn test_spawn_child_shares_task_id_counter() {
    // Companion to the channel test: same invariant for task IDs.
    let mut parent = Vm::new(crate::HostIo::process());
    let id0 = parent.next_task_id();
    assert_eq!(id0, 0);

    let mut child = parent.spawn_child();
    let id1 = child.next_task_id();
    assert_eq!(id1, 1, "child VM must share task ID counter with parent");

    let id2 = parent.next_task_id();
    assert_eq!(id2, 2, "parent must observe child's task ID advance");
}

#[test]
fn test_regex_cache_eviction_correctness() {
    // Compile 260 distinct regex patterns (>256 MAX_ENTRIES), each
    // matched against a string that should succeed. Then verify that
    // both an "early" pattern (likely evicted and recompiled) and a
    // "late" pattern (still cached) still match correctly, and that
    // a non-matching case also works.
    //
    // The patterns are of the form "pat<n>" where <n> varies, making
    // each pattern a distinct literal that matches itself.
    let result = run_vm(
        r#"
            import list
            import regex
            fn main() {
                -- Force compilation of 260 distinct patterns.
                1..260 |> list.each({ n ->
                    let _ = regex.is_match("pat{n}", "pat{n}")
                })
                -- After eviction, verify correct match results on
                -- patterns spanning the full range:
                --   pat1:   likely evicted, must recompile cleanly
                --   pat130: middle, possibly cached
                --   pat259: newest, definitely cached
                --   "pat1" vs "pat2": must NOT match
                let early_ok = regex.is_match("pat1", "pat1")
                let mid_ok = regex.is_match("pat130", "pat130")
                let late_ok = regex.is_match("pat259", "pat259")
                let no_match = regex.is_match("pat1", "pat2")
                [early_ok, mid_ok, late_ok, no_match]
            }
            "#,
    );
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::Bool(true),
            Value::Bool(true),
            Value::Bool(true),
            Value::Bool(false),
        ]))
    );
}

// ── Audit regression: builtin panic is caught (V1) ───────────────────

#[test]
fn test_builtin_panic_converted_to_vm_error() {
    // Locks V1: panics inside builtin modules must be caught by
    // `catch_builtin_panic` and converted to a clean `VmError`. A
    // panicking builtin would otherwise tear down the current scheduler
    // worker thread, stalling every other task on that worker.
    //
    // We exercise this by routing through a `#[cfg(test)]`-only
    // "__test_panic_builtin" arm in `dispatch_builtin` that always
    // panics. If the wrapper is removed, this test unwinds and fails
    // the whole test process; with the wrapper, it returns a VmError
    // naming the module and the panic payload.
    let mut vm = Vm::new(crate::HostIo::process());
    let err = vm
        .dispatch_builtin("__test_panic_builtin.boom", &[])
        .err()
        .expect("expected VmError from panicking builtin");
    let msg = format!("{err}");
    assert!(
        msg.contains("builtin module") && msg.contains("panicked"),
        "expected wrapper message, got: {msg}"
    );
    assert!(
        msg.contains("synthetic builtin panic for test"),
        "expected panic payload to be preserved, got: {msg}"
    );
}

// ── Audit regression: println/print runtime arity guard ─────────────

#[test]
fn test_println_rejects_wrong_arity() {
    // Locks the defence-in-depth arity checks in `dispatch_builtin` for
    // the built-in `println` and `print` functions. In well-typed silt
    // programs the type checker (see `typechecker/builtin_env.rs`)
    // rejects wrong-arity calls to both with arity 1, so these runtime
    // guards exist purely to catch a hypothetical compiler or emitter
    // bug that mis-emits argc. Mirrors `test_tail_call_rejects_arity_mismatch`
    // in spirit: bypass the compiler by calling `dispatch_builtin`
    // directly with the wrong number of arguments and assert we get a
    // clean VmError naming the builtin and the actual count.
    //
    // A silent revert of either `args.len() != 1` check would otherwise
    // slip through unnoticed.
    let mut vm = Vm::new(crate::HostIo::process());

    // println with 0 args
    let err = vm
        .dispatch_builtin("println", &[])
        .err()
        .expect("expected VmError for println with 0 args");
    let msg = format!("{err}");
    assert!(
        msg.contains("println takes 1 argument, got 0"),
        "expected println 0-arg guard message, got: {msg}"
    );

    // println with 2 args
    let err = vm
        .dispatch_builtin("println", &[Value::Int(1), Value::Int(2)])
        .err()
        .expect("expected VmError for println with 2 args");
    let msg = format!("{err}");
    assert!(
        msg.contains("println takes 1 argument, got 2"),
        "expected println 2-arg guard message, got: {msg}"
    );

    // print with 0 args
    let err = vm
        .dispatch_builtin("print", &[])
        .err()
        .expect("expected VmError for print with 0 args");
    let msg = format!("{err}");
    assert!(
        msg.contains("print takes 1 argument, got 0"),
        "expected print 0-arg guard message, got: {msg}"
    );

    // print with 2 args
    let err = vm
        .dispatch_builtin("print", &[Value::Int(1), Value::Int(2)])
        .err()
        .expect("expected VmError for print with 2 args");
    let msg = format!("{err}");
    assert!(
        msg.contains("print takes 1 argument, got 2"),
        "expected print 2-arg guard message, got: {msg}"
    );
}

// ── Type confusion: one test per guard family ───────────────────────
//
// The code a VM runs is verified, so an instruction always finds its
// operands; what the verifier cannot know is their kind. The typechecker
// claims it, and where the claim is wrong the instruction stops with a
// `VmError::type_confusion` error (code E0704). No well-typed program
// reaches these once the checker is sound, so each test hands the VM
// code no compiler emits for a checked program: well-formed, built with
// the emitter, and wrong about a value.
mod type_confusion {
    use std::collections::BTreeMap;

    use super::*;
    use crate::bytecode::VmClosure;
    use crate::diagnostic::Code;

    /// Emit the push of `value`.
    fn push(e: &mut Emitter, value: Value) {
        let k = e.constant(value, span()).unwrap();
        e.emit(Asm::Constant { k }, span()).unwrap();
    }

    fn string(s: &str) -> Value {
        Value::String(s.to_string())
    }

    fn name(e: &mut Emitter, s: &str) -> crate::bytecode::Const {
        e.constant(string(s), span()).unwrap()
    }

    fn record(fields: &[(&str, Value)]) -> Value {
        let fields: BTreeMap<String, Value> = fields
            .iter()
            .map(|(name, value)| (name.to_string(), value.clone()))
            .collect();
        Value::builtin_record(crate::typeinfo::ty::ANON_RECORD, fields)
    }

    fn function(arity: u8) -> Value {
        Value::VmClosure(Arc::new(VmClosure {
            function: Arc::new(Function::returning_unit("f".to_string(), arity)),
            upvalues: vec![],
        }))
    }

    /// Run the script `build` emits (followed by a `Return`), which
    /// must stop with a type-confusion error whose message contains
    /// `phrase`.
    fn confused(phrase: &str, build: impl FnOnce(&mut Emitter)) {
        let script = make_function(|e| {
            build(e);
            e.emit(Asm::Return, span()).unwrap();
        });
        let err = Vm::new(crate::HostIo::process())
            .run(script)
            .expect_err("an instruction that meets a value of the wrong kind stops");
        assert!(
            err.message.contains(phrase),
            "expected {phrase:?} in: {}",
            err.message
        );
        assert!(err.type_confusion, "not a type-confusion error: {err}");
        assert_eq!(err.to_diagnostic().code, Code::TypeConfusion);
    }

    #[test]
    fn arithmetic_on_a_value_that_is_no_number() {
        confused("cannot apply '+' to String and Int", |e| {
            push(e, string("a"));
            push(e, Value::Int(1));
            e.emit(Asm::Add, span()).unwrap();
        });
        confused("cannot apply '*' to Bool and Bool", |e| {
            e.emit(Asm::True, span()).unwrap();
            e.emit(Asm::False, span()).unwrap();
            e.emit(Asm::Mul, span()).unwrap();
        });
    }

    #[test]
    fn arithmetic_on_an_int_and_a_float() {
        confused("cannot mix Int and Float", |e| {
            push(e, Value::Int(1));
            push(e, Value::Float(2.5));
            e.emit(Asm::Sub, span()).unwrap();
        });
    }

    #[test]
    fn negation_of_a_value_that_is_no_number() {
        confused("cannot negate String", |e| {
            push(e, string("a"));
            e.emit(Asm::Negate, span()).unwrap();
        });
    }

    #[test]
    fn not_of_a_value_that_is_no_bool() {
        confused("cannot apply 'not' to Int", |e| {
            push(e, Value::Int(1));
            e.emit(Asm::Not, span()).unwrap();
        });
    }

    #[test]
    fn ordering_of_values_of_different_types() {
        confused("cannot compare Int and String", |e| {
            push(e, Value::Int(1));
            push(e, string("a"));
            e.emit(Asm::Lt, span()).unwrap();
        });
    }

    #[test]
    fn ordering_of_functions() {
        confused("type 'Fn' does not implement Compare", |e| {
            push(e, Value::List(Arc::new(vec![function(0)])));
            push(e, Value::List(Arc::new(vec![function(0)])));
            e.emit(Asm::Geq, span()).unwrap();
        });
    }

    #[test]
    fn equality_of_values_of_different_types() {
        confused("cannot compare Int and String", |e| {
            push(e, Value::Int(1));
            push(e, string("a"));
            e.emit(Asm::Eq, span()).unwrap();
        });
        confused("cannot compare Bool and Unit", |e| {
            e.emit(Asm::True, span()).unwrap();
            e.emit(Asm::Unit, span()).unwrap();
            e.emit(Asm::Neq, span()).unwrap();
        });
    }

    #[test]
    fn equality_of_functions() {
        confused("type 'Fn' does not implement Equal", |e| {
            push(e, function(0));
            push(e, function(0));
            e.emit(Asm::Eq, span()).unwrap();
        });
    }

    #[test]
    fn interpolation_of_a_part_that_is_no_string() {
        confused(
            "string interpolation requires string values, got Int",
            |e| {
                push(e, string("a"));
                push(e, Value::Int(1));
                e.emit(Asm::StringConcat { count: 2 }, span()).unwrap();
            },
        );
    }

    #[test]
    fn display_of_a_value_without_display() {
        confused("type 'Fn' does not implement Display", |e| {
            push(e, function(0));
            e.emit(Asm::DisplayValue, span()).unwrap();
        });
    }

    #[test]
    fn call_of_a_value_that_is_no_function() {
        confused("cannot call value of type Int", |e| {
            push(e, Value::Int(1));
            push(e, Value::Int(2));
            e.emit(Asm::Call { argc: 1 }, span()).unwrap();
        });
    }

    #[test]
    fn call_with_the_wrong_number_of_arguments() {
        confused("function 'f' expects 2 arguments, got 1", |e| {
            push(e, function(2));
            push(e, Value::Int(1));
            e.emit(Asm::Call { argc: 1 }, span()).unwrap();
        });
        confused("function 'f' expects 2 arguments, got 0", |e| {
            push(e, function(2));
            e.emit(Asm::TailCall { argc: 0 }, span()).unwrap();
        });
        confused(
            "variant constructor 'Some' expects 1 arguments, got 2",
            |e| {
                push(e, Value::VariantConstructor(bv::SOME.tag()));
                push(e, Value::Int(1));
                push(e, Value::Int(2));
                e.emit(Asm::Call { argc: 2 }, span()).unwrap();
            },
        );
    }

    #[test]
    fn method_call_that_names_no_trait_of_the_program() {
        confused("names a trait the program does not have", |e| {
            push(e, Value::Int(1));
            let method = name(e, "nope");
            e.emit(
                Asm::CallMethod {
                    method,
                    argc: 1,
                    of: 0,
                },
                span(),
            )
            .unwrap();
        });
    }

    #[test]
    fn record_update_of_a_value_that_is_no_record() {
        confused("requires a record, got Int", |e| {
            push(e, Value::Int(1));
            push(e, Value::Int(2));
            let x = name(e, "x");
            e.emit(Asm::RecordUpdate { fields: &[x] }, span()).unwrap();
        });
    }

    #[test]
    fn range_of_values_that_are_no_ints() {
        confused("requires two Int operands, got String and Int", |e| {
            push(e, string("a"));
            push(e, Value::Int(2));
            e.emit(Asm::MakeRange, span()).unwrap();
        });
    }

    #[test]
    fn spread_of_a_value_that_is_no_list() {
        confused("left operand is not a list or range", |e| {
            push(e, Value::Int(1));
            push(e, Value::List(Arc::new(vec![])));
            e.emit(Asm::ListConcat, span()).unwrap();
        });
        confused("right operand is not a list or range", |e| {
            push(e, Value::List(Arc::new(vec![])));
            push(e, Value::Int(1));
            e.emit(Asm::ListConcat, span()).unwrap();
        });
    }

    #[test]
    fn field_of_a_value_without_it() {
        confused("cannot access field 'x' on Int", |e| {
            push(e, Value::Int(1));
            let x = name(e, "x");
            e.emit(Asm::GetField { name: x }, span()).unwrap();
        });
        confused("record has no field 'x'", |e| {
            push(e, record(&[("y", Value::Int(1))]));
            let x = name(e, "x");
            e.emit(Asm::GetField { name: x }, span()).unwrap();
        });
    }

    #[test]
    fn tuple_pattern_on_a_value_of_another_shape() {
        confused("tuple destructure: expected tuple, got Int", |e| {
            push(e, Value::Int(1));
            e.emit(Asm::DestructTuple { index: 0 }, span()).unwrap();
        });
        confused(
            "tuple destructure: expected at least 3 elements, got 1",
            |e| {
                push(e, Value::Tuple(vec![Value::Int(1)]));
                e.emit(Asm::DestructTuple { index: 2 }, span()).unwrap();
            },
        );
    }

    #[test]
    fn variant_pattern_on_a_value_of_another_shape() {
        confused("variant destructure: expected variant, got Int", |e| {
            push(e, Value::Int(1));
            e.emit(Asm::DestructVariant { index: 0 }, span()).unwrap();
        });
        confused("variant destructure: field index 1 out of bounds", |e| {
            push(e, Value::variant(bv::SOME, vec![Value::Int(1)]));
            e.emit(Asm::DestructVariant { index: 1 }, span()).unwrap();
        });
    }

    #[test]
    fn list_pattern_on_a_value_of_another_shape() {
        confused("list destructure: expected list, got Int", |e| {
            push(e, Value::Int(1));
            e.emit(Asm::DestructList { index: 0 }, span()).unwrap();
        });
        confused(
            "list destructure: expected at least 2 elements, got 1",
            |e| {
                push(e, Value::List(Arc::new(vec![Value::Int(1)])));
                e.emit(Asm::DestructList { index: 1 }, span()).unwrap();
            },
        );
        confused("range index out of bounds", |e| {
            push(e, Value::Range(1, 2));
            e.emit(Asm::DestructList { index: 5 }, span()).unwrap();
        });
        confused("list destructure: expected list, got Int", |e| {
            push(e, Value::Int(1));
            e.emit(Asm::DestructListRest { start: 0 }, span()).unwrap();
        });
        confused("rest pattern start 3 exceeds list length 1", |e| {
            push(e, Value::List(Arc::new(vec![Value::Int(1)])));
            e.emit(Asm::DestructListRest { start: 3 }, span()).unwrap();
        });
    }

    #[test]
    fn record_pattern_on_a_value_of_another_shape() {
        confused("record destructure: expected record, got Int", |e| {
            push(e, Value::Int(1));
            let x = name(e, "x");
            e.emit(Asm::DestructRecordField { name: x }, span())
                .unwrap();
        });
        confused("record has no field 'x'", |e| {
            push(e, record(&[("y", Value::Int(1))]));
            let x = name(e, "x");
            e.emit(Asm::DestructRecordField { name: x }, span())
                .unwrap();
        });
        confused("record rest destructure: expected record, got Int", |e| {
            push(e, Value::Int(1));
            let x = name(e, "x");
            e.emit(Asm::DestructRecordRest { excluded: &[x] }, span())
                .unwrap();
        });
    }

    #[test]
    fn map_pattern_on_a_value_of_another_shape() {
        confused("map destructure: expected map, got Int", |e| {
            push(e, Value::Int(1));
            let k = name(e, "k");
            e.emit(Asm::DestructMapValue { key: k }, span()).unwrap();
        });
        confused("map has no key 'k'", |e| {
            push(e, Value::Map(Arc::new(BTreeMap::new())));
            let k = name(e, "k");
            e.emit(Asm::DestructMapValue { key: k }, span()).unwrap();
        });
    }

    #[test]
    fn question_mark_on_a_value_that_is_no_result_or_option() {
        confused("`?` applies only to Result or Option; got Int", |e| {
            push(e, Value::Int(1));
            e.emit(Asm::QuestionMark, span()).unwrap();
        });
        confused("`?` applies only to Result or Option; got variant", |e| {
            push(e, Value::variant(bv::MESSAGE, vec![Value::Int(1)]));
            e.emit(Asm::QuestionMark, span()).unwrap();
        });
    }

    /// The other kind of runtime error keeps its code: a program can
    /// reach it with every value of the type the checker gave it.
    #[test]
    fn an_error_a_well_typed_program_reaches_is_no_type_confusion() {
        let script = make_function(|e| {
            push(e, Value::Int(1));
            push(e, Value::Int(0));
            e.emit(Asm::Div, span()).unwrap();
            e.emit(Asm::Return, span()).unwrap();
        });
        let err = Vm::new(crate::HostIo::process())
            .run(script)
            .expect_err("division by zero stops the program");
        assert!(!err.type_confusion);
        assert_eq!(err.to_diagnostic().code, Code::RuntimeError);
    }
}

// ── A builtin's frame is told when it is dropped ─────────────────────
//
// `Native::abandon` is how `task.deadline` restores the outer deadline
// and `postgres.transact` rolls back. It must be called exactly once on
// every way a VM's calls end without the frame finishing.

mod abandon {
    use super::*;
    use crate::runtime::handle::TaskHandle;
    use crate::runtime::sync::{Arm, Channel, Wait};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    /// A frame that fails, or parks on a channel nobody sends to, and
    /// counts how often it is abandoned.
    struct Probe {
        abandoned: Arc<AtomicUsize>,
        park_on: Option<Arc<Channel>>,
    }

    impl Native for Probe {
        fn name(&self) -> &str {
            "test.probe"
        }

        fn resume(&mut self, _vm: &mut Vm, _input: Value) -> Result<Step, VmError> {
            match &self.park_on {
                Some(ch) => Ok(Step::Park(Wait::new(vec![Arm::Recv(ch.clone())]))),
                None => Err(VmError::new("the probe failed".into())),
            }
        }

        fn abandon(&mut self, _vm: &mut Vm) {
            self.abandoned.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// A VM whose one call in progress is a probe's.
    fn with_probe(mut vm: Vm, park_on: Option<Arc<Channel>>) -> (Vm, Arc<AtomicUsize>) {
        let abandoned = Arc::new(AtomicUsize::new(0));
        vm.push_native_frame(Box::new(Probe {
            abandoned: abandoned.clone(),
            park_on,
        }));
        (vm, abandoned)
    }

    /// The task of `main` that runs a probe, and its handle.
    fn spawn_probe(
        main: &mut Vm,
        park_on: Option<Arc<Channel>>,
    ) -> (Arc<TaskHandle>, Arc<AtomicUsize>) {
        let (mut vm, abandoned) = with_probe(main.spawn_child(), park_on);
        vm.spawned = true;
        let id = main.next_task_id();
        let handle = Arc::new(TaskHandle::new(id));
        main.scheduler().submit(id, vm, handle.clone()).unwrap();
        (handle, abandoned)
    }

    /// Wait until the probe has been abandoned, then long enough for a
    /// second time to show, and give the count.
    fn settled(abandoned: &AtomicUsize) -> usize {
        let limit = Instant::now() + Duration::from_secs(10);
        while abandoned.load(Ordering::SeqCst) == 0 && Instant::now() < limit {
            std::thread::sleep(Duration::from_millis(2));
        }
        std::thread::sleep(Duration::from_millis(50));
        abandoned.load(Ordering::SeqCst)
    }

    /// Wait until the task of a parking probe is parked.
    fn parked(ch: &Channel) {
        let limit = Instant::now() + Duration::from_secs(10);
        while ch.waiting().0 == 0 && Instant::now() < limit {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(ch.waiting().0 > 0, "the task did not park");
    }

    #[test]
    fn an_error_on_the_main_thread_abandons_the_frame_once() {
        let (mut vm, abandoned) = with_probe(Vm::new(crate::HostIo::process()), None);
        let run = vm.run_frames(0, usize::MAX);
        let err = vm.finish_run(run, 0, 0).unwrap_err();
        assert_eq!(err.message, "the probe failed");
        assert_eq!(abandoned.load(Ordering::SeqCst), 1);
        drop(vm);
        assert_eq!(abandoned.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn an_error_in_a_task_abandons_the_frame_once() {
        let mut main = Vm::new(crate::HostIo::process());
        let (handle, abandoned) = spawn_probe(&mut main, None);
        let wait = Wait::new(vec![Arm::Cell(handle.done())]);
        main.scheduler().block_thread(wait, false).unwrap();
        let err = handle.try_get().unwrap().unwrap_err();
        handle.mark_joined();
        assert_eq!(err.message, "the probe failed");
        assert_eq!(settled(&abandoned), 1);
        drop(main);
        assert_eq!(settled(&abandoned), 1);
    }

    #[test]
    fn a_cancel_of_a_parked_task_abandons_the_frame_once() {
        let mut main = Vm::new(crate::HostIo::process());
        let ch = Channel::new(main.next_channel_id(), 0);
        let (handle, abandoned) = spawn_probe(&mut main, Some(ch.clone()));
        parked(&ch);
        assert_eq!(abandoned.load(Ordering::SeqCst), 0);
        // What `task.cancel` does.
        main.scheduler().cancel(&handle);
        assert_eq!(settled(&abandoned), 1);
        drop(main);
        assert_eq!(settled(&abandoned), 1);
    }

    #[test]
    fn the_end_of_the_program_abandons_the_frame_of_a_parked_task_once() {
        let mut main = Vm::new(crate::HostIo::process());
        let ch = Channel::new(main.next_channel_id(), 0);
        let (handle, abandoned) = spawn_probe(&mut main, Some(ch.clone()));
        parked(&ch);
        assert_eq!(abandoned.load(Ordering::SeqCst), 0);
        drop(main);
        assert_eq!(settled(&abandoned), 1);
        // The channel and the handle outlive the task: it was not
        // their end that dropped it.
        drop((ch, handle));
        assert_eq!(abandoned.load(Ordering::SeqCst), 1);
    }
}
