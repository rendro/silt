use super::*;
use crate::builtins::registry::{Body, Row, registry};
use crate::bytecode::{Asm, Emitter, Function};
use crate::source::Span;
use crate::typeinfo::bv;

/// The Float `f`, a finite number, as a value.
fn float(f: f64) -> Value {
    Value::Float(crate::value::Float::new(f).expect("a finite number"))
}

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
        let a = e.constant(float(1.5), span()).unwrap();
        let b = e.constant(float(2.5), span()).unwrap();
        e.emit(Asm::Constant { k: a }, span()).unwrap();
        e.emit(Asm::Constant { k: b }, span()).unwrap();
        e.emit(Asm::Add, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script).unwrap();
    assert_eq!(result, float(4.0));
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
    vm.globals = Arc::from([std::sync::OnceLock::new()]);
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Int(42));
}

/// The VM of a task has the program's global slots, not a copy of
/// them; and a slot that is set again (a VM runs a second program)
/// gets its value in slots of that VM's own, while a task of the
/// first run keeps the ones it has.
#[test]
fn the_vm_of_a_task_shares_the_global_slots() {
    let mut vm = Vm::new(crate::HostIo::process());
    vm.globals = (0..3).map(|_| std::sync::OnceLock::new()).collect();
    assert!(vm.set_global(0, Value::Int(1)));
    let task = vm.spawn_child();
    assert!(Arc::ptr_eq(&vm.globals, &task.globals));
    // What is set later is the task's too.
    assert!(vm.set_global(1, Value::Int(2)));
    assert_eq!(task.global(1), Some(&Value::Int(2)));
    assert_eq!(task.global(2), None);
    assert!(!vm.set_global(3, Value::Int(0)));

    assert!(vm.set_global(0, Value::Int(10)));
    assert_eq!(vm.global(0), Some(&Value::Int(10)));
    assert_eq!(vm.global(1), Some(&Value::Int(2)));
    assert_eq!(task.global(0), Some(&Value::Int(1)));
    assert!(!Arc::ptr_eq(&vm.globals, &task.globals));
    // With no task holding them, the slots are set again where they
    // are.
    drop(task);
    let slots = Arc::as_ptr(&vm.globals);
    assert!(vm.set_global(1, Value::Int(20)));
    assert_eq!(vm.global(1), Some(&Value::Int(20)));
    assert!(std::ptr::eq(slots, Arc::as_ptr(&vm.globals)));
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
        let a = e.constant(Value::String("hello".into()), span()).unwrap();
        let b = e.constant(Value::String(" ".into()), span()).unwrap();
        let c = e.constant(Value::String("world".into()), span()).unwrap();
        e.emit(Asm::Constant { k: a }, span()).unwrap();
        e.emit(Asm::Constant { k: b }, span()).unwrap();
        e.emit(Asm::Constant { k: c }, span()).unwrap();
        e.emit(Asm::StringConcat { count: 3 }, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::String("hello world".into()));
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
    assert_eq!(result, Value::String("42".into()));
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
        let builtin = registry().named("println").expect("println").id;
        let val = e.constant(Value::Int(42), span()).unwrap();
        e.emit(Asm::Constant { k: val }, span()).unwrap();
        e.emit(Asm::CallBuiltin { builtin, argc: 1 }, span())
            .unwrap();
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
    assert_eq!(result, Value::tuple(vec![Value::Int(1), Value::Int(2)]));
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
    assert_eq!(result, Value::list(vec![Value::Int(10), Value::Int(20)]));
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
        let a = e.constant(float(5.5), span()).unwrap();
        let b = e.constant(float(2.25), span()).unwrap();
        e.emit(Asm::Constant { k: a }, span()).unwrap();
        e.emit(Asm::Constant { k: b }, span()).unwrap();
        e.emit(Asm::Sub, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script).unwrap();
    assert_eq!(result, float(3.25));
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
        let a = e.constant(float(5.5), span()).unwrap();
        let b = e.constant(float(2.0), span()).unwrap();
        e.emit(Asm::Constant { k: a }, span()).unwrap();
        e.emit(Asm::Constant { k: b }, span()).unwrap();
        e.emit(Asm::Mod, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script).unwrap();
    assert_eq!(result, float(1.5));
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
        let a = e.constant(float(1.5), span()).unwrap();
        let b = e.constant(float(2.5), span()).unwrap();
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
        let a = e.constant(float(4.0), span()).unwrap();
        let b = e.constant(float(4.5), span()).unwrap();
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
        let a = e.constant(float(2.5), span()).unwrap();
        let b = e.constant(float(1.5), span()).unwrap();
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
        let a = e.constant(float(3.5), span()).unwrap();
        e.emit(Asm::Constant { k: a }, span()).unwrap();
        e.emit(Asm::Negate, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script).unwrap();
    assert_eq!(result, float(-3.5));
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
fn test_e2e_string_from_int() {
    let result = run_vm(
        r#"
            import string

            fn main() {
                string.from(42)
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
        Value::list(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4)
        ])
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
        Value::list(vec![Value::Int(10), Value::Int(20), Value::Int(30)])
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
        Value::list(vec![Value::Int(10), Value::Int(20), Value::Int(30)])
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
    assert_eq!(result, Value::list(vec![Value::Int(4), Value::Int(5)]));
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
        Value::list(vec![Value::Int(10), Value::Int(20), Value::Int(30)])
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
    assert_eq!(result, Value::list(vec![Value::Int(4), Value::Int(5)]));
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
    assert_eq!(result, Value::list(vec![Value::Int(14), Value::Int(15)]));
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
    assert_eq!(result, Value::list(vec![Value::Int(20), Value::Int(30)]));
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
    assert_eq!(result, Value::list(vec![]));
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
    assert_eq!(result, float(5.0));
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
    assert_eq!(result, Value::String("Circle".into()));
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
                Value::Int(n) => n,
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
                let ch = channel.new(0)
                let _ = task.spawn({ -> channel.send(ch, 99) })
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
                let ch = channel.new(0)
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
        Value::list(vec![Value::Int(2), Value::Int(4), Value::Int(6),])
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
                let early_ok = regex.is_match("pat1", "pat1") == Ok(true)
                let mid_ok = regex.is_match("pat130", "pat130") == Ok(true)
                let late_ok = regex.is_match("pat259", "pat259") == Ok(true)
                let no_match = regex.is_match("pat2", "pat1") == Ok(true)
                [early_ok, mid_ok, late_ok, no_match]
            }
            "#,
    );
    assert_eq!(
        result,
        Value::list(vec![
            Value::Bool(true),
            Value::Bool(true),
            Value::Bool(true),
            Value::Bool(false),
        ])
    );
}

// ── Audit regression: builtin panic is caught (V1) ───────────────────

#[test]
fn test_builtin_panic_converted_to_vm_error() {
    // Locks V1: a panic inside a builtin must be caught where the
    // builtin is called (`Vm::enter_row`) and converted to a clean
    // `VmError`. A panicking builtin would otherwise tear down the
    // current scheduler worker thread, stalling every other task on
    // that worker.
    //
    // No row of the registry panics, so the test calls a row of its
    // own, as a call of a builtin does. If the wrapper is removed,
    // this test unwinds and fails; with the wrapper, it returns a
    // VmError naming the module and the panic payload.
    fn boom(_vm: &mut Vm, _args: &[Value]) -> crate::builtins::typed::Called {
        panic!("synthetic builtin panic for test")
    }
    let println = registry().named("println").expect("println");
    let row = Row {
        signature: "fn boom() -> ()",
        summary: "panics",
        name: "boom",
        module: "test",
        params: Vec::new(),
        feature: None,
        enabled: true,
        id: println.id,
        body: Body::Typed(boom),
    };
    let mut vm = Vm::new(crate::HostIo::process());
    let err = vm
        .enter_row(&row, &[])
        .err()
        .expect("expected VmError from panicking builtin");
    let msg = format!("{err}");
    assert!(
        msg.contains("builtin module 'test' panicked"),
        "expected wrapper message, got: {msg}"
    );
    assert!(
        msg.contains("synthetic builtin panic for test"),
        "expected panic payload to be preserved, got: {msg}"
    );
}

// ── println/print: a call with other arguments than one ─────────────

#[test]
fn test_println_rejects_wrong_arity() {
    // The checker rejects a call of `println` or `print` with other
    // than one argument, so no program makes one. A call that is made
    // all the same (a compiler or emitter bug that mis-emits argc) is
    // the one error of arguments that do not fit a builtin.
    let mut vm = Vm::new(crate::HostIo::process());
    for (name, args, said) in [
        (
            "println",
            vec![],
            "println takes (value: a), but was called with ()",
        ),
        (
            "println",
            vec![Value::Int(1), Value::Int(2)],
            "println takes (value: a), but was called with (Int, Int)",
        ),
        (
            "print",
            vec![],
            "print takes (value: a), but was called with ()",
        ),
        (
            "print",
            vec![Value::Int(1), Value::Int(2)],
            "print takes (value: a), but was called with (Int, Int)",
        ),
    ] {
        let id = registry().named(name).expect(name).id;
        let err = vm
            .enter_builtin(id, &args)
            .err()
            .unwrap_or_else(|| panic!("{said}"));
        assert_eq!(err.message, said);
        assert!(err.type_confusion);
    }
}

// ── Where an error is ───────────────────────────────────────────────
//
// An error's place and its call stack are read off the frames a run
// leaves (`Vm::enrich_error`): off each frame's instruction pointer,
// which the instruction loop keeps in a variable of its own and writes
// back to the frame when it leaves, or calls. Each test stops code with
// an error and says where the error must be: in a script and in a
// function the script calls, and whether the code runs in one piece or
// is stopped and resumed after every instruction.
mod error_places {
    use std::cell::Cell;

    use super::*;
    use crate::bytecode::VmClosure;
    use crate::source::FileId;
    use crate::{Buffer, HostIo};

    thread_local! {
        static NEXT_SPAN: Cell<u32> = const { Cell::new(0) };
    }

    /// A span of its own at each call: the instruction emitted with it
    /// is known by it.
    pub(super) fn span() -> Span {
        let at = NEXT_SPAN.with(|next| next.replace(next.get() + 1));
        Span::point(FileId::default(), at)
    }

    /// Emit the push of `value`.
    fn push(e: &mut Emitter, value: Value) {
        let k = e.constant(value, span()).unwrap();
        e.emit(Asm::Constant { k }, span()).unwrap();
    }

    /// Run the code `build` emits, which must stop with an error of its
    /// last instruction, and give the error. The code is run as a
    /// script and as a function a script calls, each in one piece and
    /// in slices of one step: every run must stop at that instruction,
    /// with the call stack of the frames that are there.
    pub(super) fn stopped(build: impl FnOnce(&mut Emitter)) -> VmError {
        let inner = make_function(|e| {
            build(e);
            e.emit(Asm::Return, span()).unwrap();
        });
        // The last instruction of `build`: the one before the `Return`.
        let place = inner.chunk().span_at(inner.chunk().len() - 2);
        let call = span();
        let outer = make_function(|e| {
            push(
                e,
                Value::VmClosure(Arc::new(VmClosure {
                    function: inner.clone(),
                    upvalues: vec![],
                })),
            );
            e.emit(Asm::Call { argc: 0 }, call).unwrap();
            e.emit(Asm::Return, span()).unwrap();
        });
        let mut errors = Vec::new();
        for (script, stack) in [(inner.clone(), vec![place]), (outer, vec![place, call])] {
            for slice in [None, Some(1)] {
                let mut vm = Vm::new(HostIo::process());
                if let Some(steps) = slice {
                    vm.set_time_slice(steps);
                }
                let err = vm
                    .run(script.clone())
                    .expect_err("the code stops with an error");
                assert_eq!(err.span, Some(place), "the place of: {err}");
                let frames: Vec<Span> = err.call_stack.iter().map(|(_, span)| *span).collect();
                assert_eq!(frames, stack, "the call stack of: {err}");
                errors.push(err);
            }
        }
        let error = errors.pop().expect("four runs");
        for other in &errors {
            assert_eq!(other.message, error.message);
        }
        error
    }

    /// `a op b` stops with `message`.
    fn binary(a: Value, op: Asm<'static>, b: Value, message: &str) {
        let err = stopped(|e| {
            push(e, a);
            push(e, b);
            e.emit(op, span()).unwrap();
        });
        assert_eq!(err.message, message);
        assert!(!err.type_confusion);
    }

    #[test]
    fn an_integer_overflow() {
        let int = Value::Int;
        binary(
            int(i64::MAX),
            Asm::Add,
            int(1),
            "integer overflow: 9223372036854775807 + 1",
        );
        binary(
            int(i64::MIN),
            Asm::Sub,
            int(1),
            "integer overflow: -9223372036854775808 - 1",
        );
        binary(
            int(i64::MAX),
            Asm::Mul,
            int(2),
            "integer overflow: 9223372036854775807 * 2",
        );
        binary(
            int(i64::MIN),
            Asm::Div,
            int(-1),
            "integer overflow: -9223372036854775808 / -1",
        );
        binary(
            int(i64::MIN),
            Asm::Mod,
            int(-1),
            "integer overflow: -9223372036854775808 % -1",
        );
        let err = stopped(|e| {
            push(e, int(i64::MIN));
            e.emit(Asm::Negate, span()).unwrap();
        });
        assert_eq!(err.message, "integer overflow: negate -9223372036854775808");
    }

    #[test]
    fn a_division_by_zero() {
        binary(Value::Int(7), Asm::Div, Value::Int(0), "division by zero");
        binary(Value::Int(7), Asm::Mod, Value::Int(0), "modulo by zero");
        binary(float(7.0), Asm::Div, float(0.0), "float division by zero");
        binary(float(7.0), Asm::Mod, float(0.0), "modulo by zero");
    }

    #[test]
    fn a_float_overflow() {
        binary(
            float(f64::MAX),
            Asm::Add,
            float(f64::MAX),
            &format!("float overflow: {0} + {0}", f64::MAX),
        );
        binary(
            float(f64::MAX),
            Asm::Mul,
            float(2.0),
            &format!("float overflow: {} * 2", f64::MAX),
        );
    }

    /// The error the program `source` stops with: its message, its
    /// place and its call stack, the innermost function first, each
    /// place as `line:column` (`-` for one in no file; a function that
    /// is there `n` times in a row at one place is `(n times)`). The
    /// program is run in one piece and in slices of one step, and must
    /// stop with the same error both times.
    fn stops(source: &str) -> String {
        stops_within(source, None)
    }

    /// [`stops`] for a program that may run `budget` steps.
    fn stops_within(source: &str, budget: Option<u64>) -> String {
        let program =
            crate::session::testing::compile_str(source).unwrap_or_else(|e| panic!("{e:?}"));
        let at = |span: Span| {
            if !span.is_in_source() {
                return "-".to_string();
            }
            let before = &source[..span.start_offset()];
            let line = before.matches('\n').count() + 1;
            let column = before.len() - before.rfind('\n').map_or(0, |at| at + 1) + 1;
            format!("{line}:{column}")
        };
        let shown = |err: &VmError| {
            let mut text = format!("{} at {}", err.message, err.span.map_or("-".into(), at));
            let mut frames = err.call_stack.iter().peekable();
            while let Some((name, span)) = frames.next() {
                let mut times = 1;
                while frames
                    .next_if(|next| next.0 == *name && next.1 == *span)
                    .is_some()
                {
                    times += 1;
                }
                text += &format!("; {name} {}", at(*span));
                if times > 1 {
                    text += &format!(" ({times} times)");
                }
            }
            text
        };
        let run = |slice: Option<usize>| {
            let mut vm = Vm::new(HostIo::buffer(&Buffer::new()));
            if let Some(steps) = slice {
                vm.set_time_slice(steps);
            }
            if let Some(steps) = budget {
                vm.set_step_budget(steps);
            }
            let err = vm
                .run_program(&program)
                .expect_err("the program stops with an error");
            vm.stop_tasks();
            shown(&err)
        };
        let sliced = run(Some(1));
        // (A budget is counted exactly at slice 1 only.)
        if budget.is_none() {
            assert_eq!(run(None), sliced);
        }
        sliced
    }

    #[test]
    fn in_main_after_a_call_has_returned() {
        let source = "\
fn id(x: Int) -> Int {
  x
}
fn main() {
  let big = id(9223372036854775807)
  let one = id(1)
  let sum = big + one
  println(sum)
}
";
        assert_eq!(
            stops(source),
            "integer overflow: 9223372036854775807 + 1 at 7:13; main 7:13; <script> 4:1"
        );
    }

    #[test]
    fn in_a_called_function() {
        let source = "\
fn inner(a: Int, b: Int) -> Int {
  let r = a % b
  r
}
fn outer(a: Int) -> Int {
  let v = inner(a, 0)
  v * 2
}
fn main() {
  println(outer(7))
}
";
        assert_eq!(
            stops(source),
            "modulo by zero at 2:11; inner 2:11; outer 6:11; main 10:11; <script> 9:1"
        );
        let source = "\
fn mul(a: Int, b: Int) -> Int {
  a * b
}
fn sub(a: Int, b: Int) -> Int {
  let d = a - b
  d
}
fn main() {
  let a = match mul(3, 4) {
    12 -> sub(0 - 9223372036854775807, 2)
    _ -> 0
  }
  println(a)
}
";
        assert_eq!(
            stops(source),
            "integer overflow: -9223372036854775807 - 2 at 5:11; sub 5:11; main 10:11; <script> 8:1"
        );
        let source = "\
fn neg(x: Int) -> Int {
  let y = -x
  y
}
fn main() {
  let m = 0 - 9223372036854775807 - 1
  println(neg(m))
}
";
        assert_eq!(
            stops(source),
            "integer overflow: negate -9223372036854775808 at 2:11; neg 2:11; main 7:11; <script> 5:1"
        );
        let source = "\
fn ratio(x: Float, by: Float) -> Float {
  x / by
}
fn main() {
  let z = ratio(1.0, 0.0)
  println(z)
}
";
        assert_eq!(
            stops(source),
            "float division by zero at 2:3; ratio 2:3; main 5:11; <script> 4:1"
        );
    }

    #[test]
    fn in_a_closure_and_in_a_loop() {
        let source = "\
fn make(d: Int) -> Fn(Int) -> Int {
  { x -> x / d }
}
fn main() {
  let f = make(0)
  let g = make(2)
  println(g(8))
  println(f(8))
}
";
        assert_eq!(
            stops(source),
            "division by zero at 2:10; <lambda> 2:10; main 8:11; <script> 4:1"
        );
        let source = "\
fn main() {
  let r = loop i = 5, acc = 1 {
    match i < 0 {
      true -> acc
      false -> loop(i - 1, acc + 100 / i)
    }
  }
  println(r)
}
";
        assert_eq!(
            stops(source),
            "division by zero at 5:34; main 5:34; <script> 1:1"
        );
    }

    #[test]
    fn in_an_arm_of_a_match_on_literals() {
        let pick = "\
fn pick(b: Bool, d: Int) -> Int {
  match b {
    true -> 10 / d
    false -> 20 % d
  }
}
";
        let source = format!("{pick}fn main() {{\n  println(pick(true, 0))\n}}\n");
        assert_eq!(
            stops(&source),
            "division by zero at 3:13; pick 3:13; main 8:11; <script> 7:1"
        );
        let source = format!("{pick}fn main() {{\n  println(pick(false, 0))\n}}\n");
        assert_eq!(
            stops(&source),
            "modulo by zero at 4:14; pick 4:14; main 8:11; <script> 7:1"
        );
        let step = "\
fn step(n: Int, d: Int) -> Int {
  let v = match n {
    0 -> 1 / d
    1 -> 2
    other -> other % d
  }
  v + 9223372036854775807
}
";
        for (n, error) in [
            (
                0,
                "division by zero at 3:10; step 3:10; main 10:11; <script> 9:1",
            ),
            (
                1,
                "integer overflow: 2 + 9223372036854775807 at 7:3; step 7:3; main 10:11; <script> 9:1",
            ),
            (
                2,
                "modulo by zero at 5:14; step 5:14; main 10:11; <script> 9:1",
            ),
        ] {
            let source = format!("{step}fn main() {{\n  println(step({n}, 0))\n}}\n");
            assert_eq!(stops(&source), error);
        }
    }

    #[test]
    fn in_the_callback_of_a_builtin() {
        let source = "\
import list
fn main() {
  let xs = [3, 2, 1, 0]
  let ys = list.map(xs) { x ->
    let q = 12 / x
    q + 1
  }
  println(ys)
}
";
        assert_eq!(
            stops(source),
            "division by zero at 5:13; <lambda> 5:13; main 4:12; <script> 2:1"
        );
        let source = "\
import list
fn step(acc: Int, x: Int) -> Int {
  acc * x
}
fn main() {
  let xs = [4611686018427387904, 2, 2]
  let total = list.fold(xs, 1) { acc, x -> step(acc, x) }
  println(total)
}
";
        assert_eq!(
            stops(source),
            "integer overflow: 4611686018427387904 * 2 at 3:3; step 3:3; <lambda> 7:44; main 7:15; <script> 5:1"
        );
    }

    #[test]
    fn in_a_display_impl() {
        let source = "\
type P {
  x: Int,
}
trait Display for P {
  fn display(self) -> String {
    let d = 10 / self.x
    \"P({d})\"
  }
}
fn main() {
  let p = P { x: 0 }
  println(\"got {p}\")
}
";
        assert_eq!(
            stops(source),
            "division by zero at 6:13; P.display 6:13; main 12:11; <script> 10:1"
        );
        let source = "\
type P {
  x: Int,
}
trait Display for P {
  fn display(self) -> String {
    let d = 10 % self.x
    \"P({d})\"
  }
}
fn main() {
  let ps = [P { x: 1 }, P { x: 0 }]
  println(ps)
}
";
        assert_eq!(
            stops(source),
            "modulo by zero at 6:13; P.display 6:13; main 12:3; <script> 10:1"
        );
    }

    #[test]
    fn behind_a_question_mark() {
        let source = "\
fn parse(n: Int) -> Result(Int, String) {
  match n > 0 {
    true -> Ok(100 / (n - 1))
    false -> Err(\"neg\")
  }
}
fn run(n: Int) -> Result(Int, String) {
  let a = parse(n + 1)?
  let b = parse(n)?
  Ok(a + b)
}
fn main() {
  match run(1) {
    Ok(v) -> println(v)
    Err(e) -> println(e)
  }
}
";
        assert_eq!(
            stops(source),
            "division by zero at 3:16; parse 3:16; run 9:11; main 13:9; <script> 12:1"
        );
        // After a `?` that returned early.
        let source = "\
fn parse(n: Int) -> Result(Int, String) {
  match n > 0 {
    true -> Ok(n)
    false -> Err(\"neg\")
  }
}
fn run(n: Int) -> Result(Int, String) {
  let a = parse(n)?
  Ok(a)
}
fn main() {
  let r = run(0)
  let k = match r {
    Ok(v) -> v
    Err(_) -> 0
  }
  println(7 / k)
}
";
        assert_eq!(
            stops(source),
            "division by zero at 17:11; main 17:11; <script> 11:1"
        );
    }

    #[test]
    fn in_a_tail_call() {
        let source = "\
fn c(n: Int) -> Int {
  100 / n
}
fn b(n: Int) -> Int {
  c(n - 1)
}
fn a(n: Int) -> Int {
  b(n - 1)
}
fn main() {
  println(a(2))
}
";
        assert_eq!(
            stops(source),
            "division by zero at 2:3; c 2:3; b 5:3; a 8:3; main 11:11; <script> 10:1"
        );
        // More tail calls than the log keeps.
        let source = "\
fn count(i: Int, acc: Int) -> Int {
  match i {
    0 -> acc / i
    _ -> count(i - 1, acc + i)
  }
}
fn main() {
  println(count(100, 0))
}
";
        assert_eq!(
            stops(source),
            "division by zero at 3:10; count 3:10; count 4:10 (32 times); main 8:11; <script> 7:1"
        );
        let source = "\
type C {
  n: Int,
}
trait Step {
  fn go(self, k: Int) -> Int
}
trait Step for C {
  fn go(self, k: Int) -> Int {
    match k {
      0 -> 1 / k
      _ -> self.go(k - 1)
    }
  }
}
fn main() {
  let c = C { n: 1 }
  println(c.go(5))
}
";
        assert_eq!(
            stops(source),
            "division by zero at 10:12; C.go 10:12; C.go 11:12 (5 times); main 17:11; <script> 15:1"
        );
    }

    #[test]
    fn in_a_tail_call_among_other_tail_calls() {
        // More tail calls than the log keeps, of three functions in
        // turn: the latest 32 callers, the latest first.
        let source = "\
fn a(n: Int) -> Int {
  match n <= 0 {
    true -> 1 / n
    false -> b(n - 1)
  }
}
fn b(n: Int) -> Int {
  c(n)
}
fn c(n: Int) -> Int {
  a(n)
}
fn main() {
  println(a(15))
}
";
        let kept = "c 11:3; b 8:3; a 4:14; ".repeat(10) + "c 11:3; b 8:3";
        assert_eq!(
            stops(source),
            format!("division by zero at 3:13; a 3:13; {kept}; main 14:11; <script> 13:1")
        );
        // Two frames that have each made tail calls: each frame's
        // callers come after it.
        let source = "\
fn inner(n: Int) -> Int {
  match n {
    0 -> 1 / n
    _ -> inner(n - 1)
  }
}
fn mid(n: Int) -> Int {
  1 + inner(n)
}
fn outer(n: Int) -> Int {
  match n {
    0 -> mid(3)
    _ -> outer(n - 1)
  }
}
fn main() {
  println(outer(2))
}
";
        assert_eq!(
            stops(source),
            "division by zero at 3:10; inner 3:10; inner 4:10 (3 times); mid 8:7; outer 12:10; outer 13:10 (2 times); main 17:11; <script> 16:1"
        );
        // A frame's callers go with the frame: the second call of
        // `inner` shows its own tail calls only.
        let source = "\
fn inner(n: Int, d: Int) -> Int {
  match n {
    0 -> 6 / d
    _ -> inner(n - 1, d)
  }
}
fn main() {
  let first = inner(5, 1)
  let second = inner(2, 0)
  println(first + second)
}
";
        assert_eq!(
            stops(source),
            "division by zero at 3:10; inner 3:10; inner 4:10 (2 times); main 9:16; <script> 7:1"
        );
    }

    #[test]
    fn a_panic_and_an_error_of_a_builtin() {
        let source = "\
fn boom(n: Int) -> Int {
  match n {
    0 -> panic(\"zero at {n}\")
    _ -> n
  }
}
fn main() {
  let a = boom(1)
  println(a + boom(0))
}
";
        assert_eq!(
            stops(source),
            "panic: zero at 0 at 3:10; boom 3:10; main 9:15; <script> 7:1"
        );
        let source = "\
fn span(a: Int, b: Int) -> List(Int) {
  a..b
}
fn main() {
  let xs = span(0, 9000000000000000000)
  println(xs)
}
";
        assert_eq!(
            stops(source),
            "range 0..9000000000000000000 has 9000000000000000001 elements; materializing more than 10000000 is not allowed at 6:3; main 6:3; <script> 4:1"
        );
    }

    #[test]
    fn in_a_task_that_is_joined() {
        let source = "\
import task
fn work(n: Int) -> Int {
  10 / n
}
fn main() {
  let h = task.spawn({ -> work(0) })
  let v = task.join(h)
  println(v)
}
";
        assert_eq!(
            stops(source),
            "joined task failed: division by zero at 3:3; work 3:3; <lambda> 6:27"
        );
    }

    #[test]
    fn the_join_of_a_cancelled_task() {
        let source = "\
import channel
import task
fn wait(ch: Channel(Int)) -> Int {
  match channel.receive(ch) {
    channel.Message(n) -> n
    _ -> 0
  }
}
fn main() {
  let ch = channel.new(0)
  let h = task.spawn({ -> wait(ch) })
  task.cancel(h)
  let v = task.join(h)
  println(v)
}
";
        assert_eq!(
            stops(source),
            "joined task failed: cancelled at 13:11; main 13:11; <script> 9:1"
        );
    }

    #[test]
    fn under_a_deadline() {
        // In the callback of `task.deadline`, after a wait.
        let source = "\
import task
import time
fn slow(n: Int) -> Int {
  time.sleep(time.ms(1))
  10 / n
}
fn main() {
  let r = task.deadline(time.ms(60000), { -> slow(0) })
  println(r)
}
";
        assert_eq!(
            stops(source),
            "division by zero at 5:3; slow 5:3; <lambda> 8:46; main 8:11; <script> 7:1"
        );
        // The same from a task: the join's error has the stack of
        // the function and of the task that set the deadline.
        let source = "\
import task
import time
fn slow(n: Int) -> Int {
  time.sleep(time.ms(1))
  10 / n
}
fn main() {
  let h = task.spawn({ -> task.deadline(time.ms(60000), { -> slow(0) }) })
  println(task.join(h))
}
";
        assert_eq!(
            stops(source),
            "joined task failed: division by zero at 5:3; slow 5:3; <lambda> 8:62; <lambda> 8:27"
        );
    }

    #[test]
    fn at_the_frame_limit() {
        let source = "\
fn deep(n: Int) -> Int {
  1 + deep(n + 1)
}
fn main() {
  println(deep(0))
}
";
        assert_eq!(
            stops(source),
            "stack overflow: recursion depth exceeded 100000 frames at 2:7; deep 2:7 (99998 times); main 5:11; <script> 4:1"
        );
    }

    #[test]
    fn at_the_step_budget() {
        let source = "\
fn id(x: Int) -> Int {
  x
}
fn twice(x: Int) -> Int {
  let a = id(x)
  let b = id(a)
  a + b
}
fn main() {
  let n = twice(1)
  let m = twice(n)
  println(n + m)
}
";
        let ends: Vec<String> = (0..40)
            .map(|steps| stops_within(source, Some(steps)))
            .collect();
        assert_eq!(
            ends.join("\n"),
            "\
the step budget is used up at 1:1; <script> 1:1\n\
the step budget is used up at 1:1; <script> 1:1\n\
the step budget is used up at 1:1; <script> 1:1\n\
the step budget is used up at 1:1; <script> 1:1\n\
the step budget is used up at 4:1; <script> 4:1\n\
the step budget is used up at 4:1; <script> 4:1\n\
the step budget is used up at 4:1; <script> 4:1\n\
the step budget is used up at 9:1; <script> 9:1\n\
the step budget is used up at 9:1; <script> 9:1\n\
the step budget is used up at 9:1; <script> 9:1\n\
the step budget is used up at 9:1; <script> 9:1\n\
the step budget is used up at 10:11; main 10:11; <script> 9:1\n\
the step budget is used up at 10:11; main 10:11; <script> 9:1\n\
the step budget is used up at 10:17; main 10:17; <script> 9:1\n\
the step budget is used up at 5:11; twice 5:11; main 10:11; <script> 9:1\n\
the step budget is used up at 5:11; twice 5:11; main 10:11; <script> 9:1\n\
the step budget is used up at 5:14; twice 5:14; main 10:11; <script> 9:1\n\
the step budget is used up at 2:3; id 2:3; twice 5:11; main 10:11; <script> 9:1\n\
the step budget is used up at 2:3; id 2:3; twice 5:11; main 10:11; <script> 9:1\n\
the step budget is used up at 5:11; twice 5:11; main 10:11; <script> 9:1\n\
the step budget is used up at 5:11; twice 5:11; main 10:11; <script> 9:1\n\
the step budget is used up at 6:11; twice 6:11; main 10:11; <script> 9:1\n\
the step budget is used up at 6:14; twice 6:14; main 10:11; <script> 9:1\n\
the step budget is used up at 2:3; id 2:3; twice 6:11; main 10:11; <script> 9:1\n\
the step budget is used up at 2:3; id 2:3; twice 6:11; main 10:11; <script> 9:1\n\
the step budget is used up at 6:11; twice 6:11; main 10:11; <script> 9:1\n\
the step budget is used up at 6:11; twice 6:11; main 10:11; <script> 9:1\n\
the step budget is used up at 7:3; twice 7:3; main 10:11; <script> 9:1\n\
the step budget is used up at 7:7; twice 7:7; main 10:11; <script> 9:1\n\
the step budget is used up at 7:3; twice 7:3; main 10:11; <script> 9:1\n\
the step budget is used up at 10:11; main 10:11; <script> 9:1\n\
the step budget is used up at 10:11; main 10:11; <script> 9:1\n\
the step budget is used up at 11:11; main 11:11; <script> 9:1\n\
the step budget is used up at 11:17; main 11:17; <script> 9:1\n\
the step budget is used up at 5:11; twice 5:11; main 11:11; <script> 9:1\n\
the step budget is used up at 5:11; twice 5:11; main 11:11; <script> 9:1\n\
the step budget is used up at 5:14; twice 5:14; main 11:11; <script> 9:1\n\
the step budget is used up at 2:3; id 2:3; twice 5:11; main 11:11; <script> 9:1\n\
the step budget is used up at 2:3; id 2:3; twice 5:11; main 11:11; <script> 9:1\n\
the step budget is used up at 5:11; twice 5:11; main 11:11; <script> 9:1"
        );
    }
}

// ── The arguments of a builtin ──────────────────────────────────────
//
// A builtin's arguments leave the stack before its body runs, into the
// VM's one list for them (`Vm::with_args`): no call has a list of its
// own, and no body sees a slot of the stack.
mod builtin_args {
    use super::*;

    fn ints(values: &[i64]) -> Vec<Value> {
        values.iter().copied().map(Value::Int).collect()
    }

    #[test]
    fn the_arguments_are_off_the_stack_and_the_list_is_used_again() {
        let mut vm = Vm::new(crate::HostIo::process());
        vm.stack = ints(&[1, 2, 3]);
        let seen = vm.with_args(1, |vm, args| {
            assert_eq!(vm.stack, ints(&[1]));
            // What a body pushes is not among its arguments.
            vm.stack.push(Value::Int(9));
            args.to_vec()
        });
        assert_eq!(seen, ints(&[2, 3]));
        assert_eq!(vm.stack, ints(&[1, 9]));
        assert!(vm.builtin_args.is_empty());
        let capacity = vm.builtin_args.capacity();
        assert!(capacity >= 2);

        // A call of no more arguments fits the list as it is.
        for _ in 0..100 {
            vm.stack.extend(ints(&[4, 5]));
            vm.with_args(2, |_, args| assert_eq!(args, ints(&[4, 5])));
            assert_eq!(vm.stack, ints(&[1, 9]));
            assert!(vm.builtin_args.is_empty());
            assert_eq!(vm.builtin_args.capacity(), capacity);
        }
        // No arguments: nothing leaves the stack.
        vm.with_args(2, |vm, args| {
            assert!(args.is_empty());
            assert_eq!(vm.stack, ints(&[1, 9]));
        });
    }

    #[test]
    fn a_call_inside_a_call_has_arguments_of_its_own() {
        let mut vm = Vm::new(crate::HostIo::process());
        vm.stack = ints(&[1, 2]);
        vm.with_args(0, |vm, outer| {
            vm.stack = ints(&[7, 8, 9]);
            let inner = vm.with_args(1, |_, inner| inner.to_vec());
            assert_eq!(inner, ints(&[8, 9]));
            assert_eq!(outer, ints(&[1, 2]));
            assert_eq!(vm.stack, ints(&[7]));
        });
        assert!(vm.builtin_args.is_empty());
        vm.stack.push(Value::Int(3));
        vm.with_args(0, |_, args| assert_eq!(args, ints(&[7, 3])));
    }

    /// A row of the test's own, with `body`.
    fn row(body: crate::builtins::typed::TypedCall) -> Row {
        Row {
            signature: "fn probe(a: Bytes) -> ()",
            summary: "a probe",
            name: "probe",
            module: "test",
            params: Vec::new(),
            feature: None,
            enabled: true,
            id: registry().named("println").expect("println").id,
            body: Body::Typed(body),
        }
    }

    /// The arguments are dropped when the body has returned, whatever
    /// it returned: before its value is anybody's, before the frame it
    /// goes on as is resumed for the first time, before its error is
    /// looked at. (As when every call had a list of its own, which went
    /// when the call was made.)
    #[test]
    fn the_arguments_are_dropped_when_the_body_has_returned() {
        use crate::vm::calls::Entered;

        struct Frame;
        impl Native for Frame {
            fn name(&self) -> &str {
                "test.probe"
            }
            fn resume(&mut self, _: &mut Vm, _: Value) -> Result<Step, VmError> {
                Ok(Step::Done(Value::Unit))
            }
        }
        fn value(_: &mut Vm, _: &[Value]) -> crate::builtins::typed::Called {
            Some(Ok(Step::Done(Value::Unit)))
        }
        fn frame(_: &mut Vm, _: &[Value]) -> crate::builtins::typed::Called {
            Some(Ok(Step::Run(Box::new(Frame))))
        }
        fn fails(_: &mut Vm, _: &[Value]) -> crate::builtins::typed::Called {
            Some(Err(VmError::new("the probe failed".into())))
        }
        fn misfit(_: &mut Vm, _: &[Value]) -> crate::builtins::typed::Called {
            None
        }

        // What the test and the argument hold: the bytes are held twice
        // while the argument is there.
        let held = Arc::new(vec![7u8]);
        for (body, frames) in [
            (value as crate::builtins::typed::TypedCall, 0),
            (frame, 1),
            (fails, 0),
            (misfit, 0),
        ] {
            let row = row(body);
            let mut vm = Vm::new(crate::HostIo::process());
            vm.stack = vec![Value::Bytes(held.clone())];
            let entered = vm.with_args(0, |vm, args| {
                assert_eq!(Arc::strong_count(&held), 2);
                vm.enter_row(&row, args)
            });
            assert_eq!(Arc::strong_count(&held), 1);
            // (The frame is there, and has not been resumed.)
            assert_eq!(vm.frames.len(), frames);
            assert_eq!(matches!(entered, Ok(Entered::Native)), frames == 1);
            assert!(vm.stack.is_empty());
        }
    }

    /// A VM keeps the list from one call to the next, and gives it up
    /// where it waits: a waiting task holds none.
    #[test]
    fn a_vm_that_waits_gives_the_list_up() {
        use crate::vm::run::Slice;

        let program = crate::session::testing::compile_str(
            "\
import channel
import int
fn main() {
  let nobody = channel.new(0)
  let n = int.abs(-1) + int.abs(-2)
  channel.receive(nobody)
}
",
        )
        .unwrap_or_else(|e| panic!("{e:?}"));
        let mut vm = Vm::new(crate::HostIo::process());
        vm.load(&program);
        vm.frames.push(Frame::Code(CallFrame {
            closure: Arc::new(VmClosure {
                function: Arc::new(program.functions[0].clone()),
                upvalues: vec![],
            }),
            ip: 0,
            base_slot: 0,
        }));
        // Step by step up to the wait.
        let mut kept = 0;
        loop {
            match vm.run_frames(0, 1).expect("the program runs") {
                Slice::OutOfBudget => kept = kept.max(vm.builtin_args.capacity()),
                Slice::Parked(_) => break,
                _ => panic!("the program waits"),
            }
            assert!(vm.builtin_args.is_empty());
        }
        assert!(kept >= 1, "the list was kept between the calls");
        assert_eq!(vm.builtin_args.capacity(), 0);
    }

    /// Run `main`'s code and give the VM back with what the run came
    /// to.
    fn ran(source: &str) -> (Vm, Result<Value, VmError>) {
        let program =
            crate::session::testing::compile_str(source).unwrap_or_else(|e| panic!("{e:?}"));
        let mut vm = Vm::new(crate::HostIo::buffer(&crate::Buffer::new()));
        let result = vm.run_program(&program);
        (vm, result)
    }

    #[test]
    fn every_way_to_call_a_builtin_leaves_the_list_empty() {
        // By its instruction, as a value, as the callback of a builtin,
        // and with a callback of its own that calls builtins.
        let (vm, result) = ran("\
import int
import list
import string
fn apply(f: Fn(Int) -> Int, x: Int) -> Int {
  f(x)
}
fn main() {
  let direct = int.abs(-3) + string.length(\"four\")
  let as_value = apply(int.abs, -5)
  let callback = list.map([-1, -2], int.abs)
  let nested = list.fold([1, 2, 3], 0) { acc, x -> acc + int.max(x, string.length(\"ab\")) }
  println(callback)
  direct + as_value + list.length(callback) + nested
}
");
        assert_eq!(result.unwrap(), Value::Int(7 + 5 + 2 + 7));
        assert!(vm.builtin_args.is_empty());
        assert!(vm.builtin_args.capacity() >= 2);
    }

    #[test]
    fn a_builtin_that_fails_leaves_the_list_empty() {
        let (vm, result) = ran("\
import list
fn main() {
  let xs = list.map(1..4) { x -> 10 / (x - 2) }
  list.length(xs)
}
");
        assert_eq!(result.unwrap_err().message, "division by zero");
        assert!(vm.builtin_args.is_empty());

        // A body that panics: the panic is an error of the call, and
        // the list is the VM's again.
        fn boom(_vm: &mut Vm, _args: &[Value]) -> crate::builtins::typed::Called {
            panic!("a panic in a builtin")
        }
        let row = row(boom);
        let mut vm = Vm::new(crate::HostIo::process());
        vm.stack = ints(&[0, 1, 2]);
        let err = (vm.with_args(1, |vm, args| vm.enter_row(&row, args)).err())
            .expect("the panic is an error");
        assert!(err.message.contains("a panic in a builtin"), "{err}");
        assert_eq!(vm.stack, ints(&[0]));
        assert!(vm.builtin_args.is_empty());
        assert!(vm.builtin_args.capacity() >= 2);
    }
}

// ── The log of tail calls ───────────────────────────────────────────
mod tail_calls {
    use super::*;
    use crate::vm::runtime::{TCO_ELIDED_CAP, TailCalls};

    fn function(name: &str) -> Arc<Function> {
        Arc::new(Function::returning_unit(name.to_string(), 0))
    }

    /// The log's entries as `depth name ip`, the innermost frame's
    /// first and a frame's latest first.
    fn entries(log: &TailCalls) -> Vec<String> {
        log.newest_first()
            .map(|(depth, function, ip)| format!("{depth} {} {ip}", function.name()))
            .collect()
    }

    #[test]
    fn a_frame_keeps_its_latest_tail_calls_the_latest_first() {
        let (f, g) = (function("f"), function("g"));
        let mut log = TailCalls::default();
        assert!(log.is_empty());
        log.log(3, f.clone(), 10);
        log.log(3, g.clone(), 20);
        log.log(3, f.clone(), 30);
        assert!(!log.is_empty());
        assert_eq!(entries(&log), ["3 f 30", "3 g 20", "3 f 10"]);

        // One more than the log keeps: the first is gone, at every
        // place of the ring.
        for extra in 0..=2 * TCO_ELIDED_CAP {
            let mut log = TailCalls::default();
            let made = TCO_ELIDED_CAP + extra;
            for ip in 0..made {
                log.log(0, f.clone(), ip);
            }
            let latest: Vec<String> = (made - TCO_ELIDED_CAP..made)
                .rev()
                .map(|ip| format!("0 f {ip}"))
                .collect();
            assert_eq!(entries(&log), latest);
        }
    }

    #[test]
    fn each_frame_has_its_own_and_they_go_with_it() {
        let (f, g, h) = (function("f"), function("g"), function("h"));
        let mut log = TailCalls::default();
        log.log(1, f.clone(), 1);
        log.log(1, f.clone(), 2);
        // A frame above makes more than the log keeps.
        for ip in 0..TCO_ELIDED_CAP + 3 {
            log.log(4, g.clone(), ip);
        }
        log.log(6, h.clone(), 7);
        let all = entries(&log);
        assert_eq!(all.len(), 2 + TCO_ELIDED_CAP + 1);
        assert_eq!(all[0], "6 h 7");
        assert_eq!(all[1], format!("4 g {}", TCO_ELIDED_CAP + 2));
        assert_eq!(all[TCO_ELIDED_CAP], "4 g 3");
        assert_eq!(all[TCO_ELIDED_CAP + 1..], ["1 f 2", "1 f 1"]);

        // The frames at 5 and above go; the one at 4 goes on.
        log.prune(5);
        assert_eq!(entries(&log).len(), 2 + TCO_ELIDED_CAP);
        log.log(4, h.clone(), 99);
        assert_eq!(
            entries(&log)[..2],
            ["4 h 99".to_string(), format!("4 g {}", TCO_ELIDED_CAP + 2)]
        );
        assert_eq!(entries(&log).len(), 2 + TCO_ELIDED_CAP);

        log.prune(4);
        assert_eq!(entries(&log), ["1 f 2", "1 f 1"]);
        // A new frame at the depth of one that is gone starts anew.
        log.log(4, g.clone(), 5);
        assert_eq!(entries(&log), ["4 g 5", "1 f 2", "1 f 1"]);
        log.prune(0);
        assert!(log.is_empty());
        assert_eq!(entries(&log), [""; 0]);
        // The functions are held by the test alone again.
        assert_eq!(Arc::strong_count(&f), 1);
        assert_eq!(Arc::strong_count(&g), 1);
        assert_eq!(Arc::strong_count(&h), 1);
    }
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

    use super::error_places::{span, stopped};
    use super::*;
    use crate::bytecode::VmClosure;
    use crate::diagnostic::Code;

    /// Emit the push of `value`.
    fn push(e: &mut Emitter, value: Value) {
        let k = e.constant(value, span()).unwrap();
        e.emit(Asm::Constant { k }, span()).unwrap();
    }

    fn string(s: &str) -> Value {
        Value::String(s.into())
    }

    fn name(e: &mut Emitter, s: &str) -> crate::bytecode::Const {
        e.constant(string(s), span()).unwrap()
    }

    fn record(fields: &[(&str, Value)]) -> Value {
        Value::anon_record(fields.iter().cloned())
    }

    fn function(arity: u8) -> Value {
        Value::VmClosure(Arc::new(VmClosure {
            function: Arc::new(Function::returning_unit("f".to_string(), arity)),
            upvalues: vec![],
        }))
    }

    /// Run the code `build` emits, which must stop at its last
    /// instruction with a type-confusion error whose message contains
    /// `phrase`: as a script and as a called function, in one piece
    /// and in slices of one step (`error_places::stopped`).
    fn confused(phrase: &str, build: impl FnOnce(&mut Emitter)) {
        let err = stopped(build);
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
            push(e, float(2.5));
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
            push(e, Value::list(vec![function(0)]));
            push(e, Value::list(vec![function(0)]));
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

    /// To a program every function is an `Fn`: a closure against a
    /// builtin function, or against a variant's constructor, is refused
    /// with the words two closures are refused with.
    #[test]
    fn functions_of_different_kinds_are_functions() {
        let builtin = || Value::BuiltinFn(crate::builtins::registry::BuiltinId(0));
        let constructor = || Value::VariantConstructor(bv::SOME.tag());
        for other in [builtin, constructor] {
            confused("type 'Fn' does not implement Equal", |e| {
                push(e, function(1));
                push(e, other());
                e.emit(Asm::Eq, span()).unwrap();
            });
            confused("type 'Fn' does not implement Equal", |e| {
                push(e, other());
                push(e, function(1));
                e.emit(Asm::Neq, span()).unwrap();
            });
            confused("type 'Fn' does not implement Compare", |e| {
                push(e, function(1));
                push(e, other());
                e.emit(Asm::Lt, span()).unwrap();
            });
        }
        confused("type 'Fn' does not implement Compare", |e| {
            push(e, function(1));
            push(e, function(1));
            e.emit(Asm::Lt, span()).unwrap();
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
    fn method_call_that_names_no_method_of_the_program() {
        confused("names a method the program does not have", |e| {
            push(e, Value::Int(1));
            e.emit(
                Asm::CallMethod {
                    method: 0,
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
        confused("ListConcat: an operand is not a list", |e| {
            push(e, Value::Int(1));
            push(e, Value::list(vec![]));
            e.emit(Asm::ListConcat, span()).unwrap();
        });
        confused("ListConcat: an operand is not a list", |e| {
            push(e, Value::list(vec![]));
            push(e, Value::Int(1));
            e.emit(Asm::ListConcat, span()).unwrap();
        });
    }

    #[test]
    fn field_of_a_value_without_it() {
        confused("cannot access field 'x' on Int", |e| {
            push(e, Value::Int(1));
            let x = name(e, "x");
            e.emit(Asm::GetFieldNamed { name: x }, span()).unwrap();
        });
        confused("record has no field 'x'", |e| {
            push(e, record(&[("y", Value::Int(1))]));
            let x = name(e, "x");
            e.emit(Asm::GetFieldNamed { name: x }, span()).unwrap();
        });
        confused("cannot access field 0 of Int", |e| {
            push(e, Value::Int(1));
            e.emit(Asm::GetField { index: 0 }, span()).unwrap();
        });
        confused("cannot access field 1 of an anonymous record", |e| {
            push(e, record(&[("y", Value::Int(1))]));
            e.emit(Asm::GetField { index: 1 }, span()).unwrap();
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
                push(e, Value::tuple(vec![Value::Int(1)]));
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
                push(e, Value::list(vec![Value::Int(1)]));
                e.emit(Asm::DestructList { index: 1 }, span()).unwrap();
            },
        );
        confused(
            "list destructure: expected at least 6 elements, got 2",
            |e| {
                push(e, Value::List(crate::value::List::ints(1, 2).unwrap()));
                e.emit(Asm::DestructList { index: 5 }, span()).unwrap();
            },
        );
        confused("list destructure: expected list, got Int", |e| {
            push(e, Value::Int(1));
            e.emit(Asm::DestructListRest { start: 0 }, span()).unwrap();
        });
        confused("rest pattern start 3 exceeds list length 1", |e| {
            push(e, Value::list(vec![Value::Int(1)]));
            e.emit(Asm::DestructListRest { start: 3 }, span()).unwrap();
        });
    }

    #[test]
    fn record_pattern_on_a_value_of_another_shape() {
        confused("record destructure: expected record, got Int", |e| {
            push(e, Value::Int(1));
            let x = name(e, "x");
            e.emit(Asm::DestructRecordFieldNamed { name: x }, span())
                .unwrap();
        });
        confused("record has no field 'x'", |e| {
            push(e, record(&[("y", Value::Int(1))]));
            let x = name(e, "x");
            e.emit(Asm::DestructRecordFieldNamed { name: x }, span())
                .unwrap();
        });
        confused("record destructure: no field 0 in Int", |e| {
            push(e, Value::Int(1));
            e.emit(Asm::DestructRecordField { index: 0 }, span())
                .unwrap();
        });
        confused(
            "record destructure: no field 1 in an anonymous record",
            |e| {
                push(e, record(&[("y", Value::Int(1))]));
                e.emit(Asm::DestructRecordField { index: 1 }, span())
                    .unwrap();
            },
        );
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
            push(e, Value::map(BTreeMap::new()));
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
        main.scheduler()
            .block_thread(wait, crate::scheduler::Blocks::Thread)
            .unwrap();
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
