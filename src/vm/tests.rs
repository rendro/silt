use super::*;
use crate::builtins::registry::{Body, Row, registry};
use crate::bytecode::{Asm, Emitter, Function, Segment};
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

/// The message of the error `build`'s script ends with.
fn script_error(build: impl FnOnce(&mut Emitter)) -> String {
    let script = make_function(build);
    let mut vm = Vm::new(crate::HostIo::process());
    vm.run(script).unwrap_err().message
}

#[test]
fn an_add_of_a_constant_refuses_what_an_add_refuses() {
    // (Fused: `AddConst`.)
    let message = script_error(|e| {
        let a = e.constant(Value::String("a".into()), span()).unwrap();
        let one = e.constant(Value::Int(1), span()).unwrap();
        e.emit(Asm::Constant { k: a }, span()).unwrap();
        e.emit(Asm::Constant { k: one }, span()).unwrap();
        e.emit(Asm::Add, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    assert_eq!(
        message,
        "cannot apply '+' to String and Int; build strings with interpolation, e.g. \"{a}{b}\""
    );
}

#[test]
fn a_comparison_and_its_jump_refuse_what_the_comparison_refuses() {
    for (compare, jump) in [(Asm::Lt, true), (Asm::Geq, false), (Asm::Eq, true)] {
        // (Fused: one `JumpIfNot..` instruction.)
        let message = script_error(|e| {
            let one = e.constant(Value::Int(1), span()).unwrap();
            let a = e.constant(Value::String("a".into()), span()).unwrap();
            let end = e.label();
            e.emit(Asm::Constant { k: one }, span()).unwrap();
            e.emit(Asm::Constant { k: a }, span()).unwrap();
            e.emit(compare, span()).unwrap();
            match jump {
                true => e.emit(Asm::JumpIfFalse { to: end }, span()).unwrap(),
                false => e.emit(Asm::JumpIfTrue { to: end }, span()).unwrap(),
            }
            e.bind(end, span()).unwrap();
            e.emit(Asm::Unit, span()).unwrap();
            e.emit(Asm::Return, span()).unwrap();
        });
        assert_eq!(
            message, "unsupported operation: cannot compare Int and String",
            "{compare:?}"
        );
    }
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
    vm.view.slots = Arc::from([std::sync::OnceLock::new()]);
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
    vm.view.slots = (0..3).map(|_| std::sync::OnceLock::new()).collect();
    assert!(vm.set_global(0, Value::Int(1)));
    let task = vm.spawn_child();
    assert!(Arc::ptr_eq(&vm.view.slots, &task.view.slots));
    // What is set later is the task's too.
    assert!(vm.set_global(1, Value::Int(2)));
    assert_eq!(task.global(1), Some(&Value::Int(2)));
    assert_eq!(task.global(2), None);
    assert!(!vm.set_global(3, Value::Int(0)));

    let again = super::slots_set_again();
    assert!(vm.set_global(0, Value::Int(10)));
    assert_eq!(super::slots_set_again(), again + 1);
    assert_eq!(vm.global(0), Some(&Value::Int(10)));
    assert_eq!(vm.global(1), Some(&Value::Int(2)));
    assert_eq!(task.global(0), Some(&Value::Int(1)));
    assert!(!Arc::ptr_eq(&vm.view.slots, &task.view.slots));
    // With no task holding them, the slots are set again where they
    // are.
    drop(task);
    let slots = Arc::as_ptr(&vm.view.slots);
    assert!(vm.set_global(1, Value::Int(20)));
    assert_eq!(vm.global(1), Some(&Value::Int(20)));
    assert!(std::ptr::eq(slots, Arc::as_ptr(&vm.view.slots)));
}

/// The VM of a task finds the slots of a program its VM took in after
/// the task was started (a later REPL entry's): where its own slots do
/// not have one, it looks in the newest. A slot that is declared and
/// not set is not set for the task either.
#[test]
fn the_vm_of_a_task_finds_the_slots_of_a_later_program() {
    fn publish(vm: &mut Vm) {
        let runtime = vm.runtime.clone();
        vm.publish(&mut runtime.newest.view.lock());
    }
    /// What a read of `slot` gives that found no value in the VM's
    /// own slots.
    fn again(vm: &mut Vm, slot: u16) -> Result<Value, String> {
        (vm.view)
            .global_of_newest(&mut vm.view_count, &vm.runtime.newest, slot)
            .map_err(|error| error.message)
    }
    let mut vm = Vm::new(crate::HostIo::process());
    vm.view.slots = (0..2).map(|_| std::sync::OnceLock::new()).collect();
    assert!(vm.set_global(0, Value::Int(1)));
    publish(&mut vm);
    let mut task = vm.spawn_child();
    // The task has the newest: a slot that is not set is not looked
    // for anywhere else.
    let unset = Err("the global '?' is read before it is set".to_string());
    assert_eq!(again(&mut task, 1), unset);
    assert!(!task.look_again());

    // A later program: the slots there were, and two more.
    vm.view.slots = (vm.view.slots.iter().cloned())
        .chain((0..2).map(|_| std::sync::OnceLock::new()))
        .collect();
    assert!(vm.set_global(2, Value::Int(3)));
    publish(&mut vm);
    assert_eq!(task.global(2), None);
    assert_eq!(again(&mut task, 2), Ok(Value::Int(3)));
    assert!(Arc::ptr_eq(&vm.view.slots, &task.view.slots));
    assert_eq!(task.global(0), Some(&Value::Int(1)));
    // Declared, not set: for the task as for its VM.
    assert_eq!((vm.global(3), again(&mut task, 3)), (None, unset));
    // Set from here on, the task has it without looking again.
    assert!(vm.set_global(3, Value::Int(4)));
    assert_eq!(task.global(3), Some(&Value::Int(4)));
}

/// A VM reads a copy of its own of a closure in a global slot: made
/// the first time it reads the slot, equal to the slot's closure, and
/// read from then on; the VM of a task makes a copy of its own. A slot
/// that is set again (a VM runs a second program) holds another
/// closure, which is copied in turn.
#[test]
fn a_vm_reads_its_own_copy_of_a_global_closure() {
    fn closure(name: &str, captured: i64) -> Arc<VmClosure> {
        Arc::new(VmClosure {
            function: Arc::new(Function::returning_unit(name.into(), 0)),
            upvalues: vec![Value::Int(captured)],
        })
    }
    /// What `GetGlobal` of slot 0 pushes.
    fn read(vm: &mut Vm) -> Arc<VmClosure> {
        let script = make_function(|e| {
            e.emit(Asm::GetGlobal { slot: 0 }, span()).unwrap();
            e.emit(Asm::Return, span()).unwrap();
        });
        match vm.run(script) {
            Ok(Value::VmClosure(closure)) => closure,
            other => panic!("not a closure: {other:?}"),
        }
    }
    let mut vm = Vm::new(crate::HostIo::process());
    vm.view.slots = Arc::from([std::sync::OnceLock::new()]);
    let shared = closure("f", 1);
    assert!(vm.set_global(0, Value::VmClosure(shared.clone())));

    let own = read(&mut vm);
    assert!(!Arc::ptr_eq(&own, &shared));
    assert!(Arc::ptr_eq(&own.function, &shared.function));
    assert_eq!(
        Value::VmClosure(own.clone()),
        Value::VmClosure(shared.clone())
    );
    assert!(Arc::ptr_eq(&read(&mut vm), &own));

    let mut task = vm.spawn_child();
    let task_own = read(&mut task);
    assert!(!Arc::ptr_eq(&task_own, &own) && !Arc::ptr_eq(&task_own, &shared));
    assert!(Arc::ptr_eq(&read(&mut task), &task_own));

    // Set again: the VM reads a copy of the new closure, the task goes
    // on with the slots it has.
    let other = closure("g", 2);
    assert!(vm.set_global(0, Value::VmClosure(other.clone())));
    let own_other = read(&mut vm);
    assert!(Arc::ptr_eq(&own_other.function, &other.function));
    assert_eq!(own_other.upvalues, [Value::Int(2)]);
    assert!(Arc::ptr_eq(&read(&mut task), &task_own));
    // The copy of the closure that was there is let go of.
    drop((own, task_own, task));
    assert_eq!(Arc::strong_count(&shared), 1);
}

/// A VM's copies take places as many as the slots it has read, not as
/// the highest of them; each slot gives the copy it has, whatever the
/// slots it shares places with; a slot that holds another closure, or
/// whose copy was let go of, is copied anew.
#[test]
fn the_copies_of_a_vm_are_as_many_as_the_slots_it_read() {
    fn closure(captured: i64) -> Arc<VmClosure> {
        Arc::new(VmClosure {
            function: Arc::new(Function::returning_unit("f".into(), 0)),
            upvalues: vec![Value::Int(captured)],
        })
    }
    let mut own = runtime::OwnClosures::default();
    assert_eq!(own.places(), 0);
    // Slots far apart, and slots that share a place in a small table.
    let slots: Vec<u16> = (0..40).map(|i| i * 1601).chain([8, 16, 24, 32]).collect();
    let shared: Vec<Arc<VmClosure>> = (0..slots.len()).map(|i| closure(i as i64)).collect();
    let copies: Vec<Arc<VmClosure>> = (slots.iter().zip(&shared))
        .map(|(&slot, shared)| own.of(slot, shared))
        .collect();
    assert!(own.places() <= 4 * slots.len(), "{} places", own.places());
    for ((&slot, shared), copy) in slots.iter().zip(&shared).zip(&copies) {
        assert!(!Arc::ptr_eq(copy, shared));
        assert_eq!(copy.upvalues, shared.upvalues);
        assert!(Arc::ptr_eq(&own.of(slot, shared), copy));
    }
    // Another closure in a slot: a copy of it, and the others stay.
    let other = closure(-1);
    let copy = own.of(slots[3], &other);
    assert_eq!(copy.upvalues, [Value::Int(-1)]);
    assert!(Arc::ptr_eq(&own.of(slots[3], &other), &copy));
    // A copy let go of: the slot is copied anew, the others stay.
    own.forget(slots[5]);
    assert!(!Arc::ptr_eq(&own.of(slots[5], &shared[5]), &copies[5]));
    for (i, ((&slot, shared), copy)) in slots.iter().zip(&shared).zip(&copies).enumerate() {
        if i != 3 && i != 5 {
            assert!(Arc::ptr_eq(&own.of(slot, shared), copy), "slot {slot}");
        }
    }
    // The table holds the closures it copied and no more.
    drop(own);
    assert!(shared.iter().all(|shared| Arc::strong_count(shared) == 1));
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
fn test_interpolate() {
    let script = make_function(|e| {
        let a = e.constant(Value::String("hello".into()), span()).unwrap();
        let b = e.constant(Value::String(" ".into()), span()).unwrap();
        let c = e.constant(Value::String("world".into()), span()).unwrap();
        e.emit(Asm::Constant { k: b }, span()).unwrap();
        let segments = [Segment::Literal(a), Segment::Value, Segment::Literal(c)];
        e.emit(
            Asm::Interpolate {
                segments: &segments,
            },
            span(),
        )
        .unwrap();
        e.emit(Asm::Return, span()).unwrap();
    });
    let mut vm = Vm::new(crate::HostIo::process());
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::String("hello world".into()));
}

/// `DisplayValue` leaves a String, an Int, a Float and a Bool as they
/// are, for `Interpolate` to write, and makes any other value its text.
#[test]
fn test_display_value() {
    let shown = |value: Value| {
        let script = make_function(|e| {
            let val = e.constant(value, span()).unwrap();
            e.emit(Asm::Constant { k: val }, span()).unwrap();
            e.emit(Asm::DisplayValue, span()).unwrap();
            e.emit(Asm::Return, span()).unwrap();
        });
        Vm::new(crate::HostIo::process()).run(script).unwrap()
    };
    for plain in [
        Value::Int(42),
        float(-0.0),
        Value::Bool(true),
        Value::String("s".into()),
    ] {
        assert_eq!(shown(plain.clone()), plain);
    }
    let list = Value::list(vec![Value::Int(1), Value::Int(2)]);
    assert_eq!(shown(list), Value::String("[1, 2]".into()));
}

/// Each kind `Interpolate` writes itself, written as `println` writes
/// it; and a lone segment that is a String is that String, not
/// a copy of it.
#[test]
fn test_interpolate_writes_plain_values() {
    let interpolated = |values: Vec<Value>| {
        let script = make_function(|e| {
            let dash = e.constant(Value::String("-".into()), span()).unwrap();
            let mut segments = vec![];
            for value in values {
                let k = e.constant(value, span()).unwrap();
                e.emit(Asm::Constant { k }, span()).unwrap();
                segments.extend([Segment::Value, Segment::Literal(dash)]);
            }
            segments.pop();
            e.emit(
                Asm::Interpolate {
                    segments: &segments,
                },
                span(),
            )
            .unwrap();
            e.emit(Asm::Return, span()).unwrap();
        });
        Vm::new(crate::HostIo::process()).run(script).unwrap()
    };
    let values = vec![
        Value::Int(i64::MIN),
        float(-0.0),
        float(1e20),
        float(1e-7),
        Value::Bool(false),
        Value::String("s".into()),
    ];
    let expected = "-9223372036854775808-0.0-100000000000000000000.0-0.0000001-false-s";
    assert_eq!(interpolated(values), Value::String(expected.into()));
    let alone: Arc<str> = "alone".into();
    let Value::String(result) = interpolated(vec![Value::String(alone.clone())]) else {
        panic!("a String")
    };
    assert!(Arc::ptr_eq(&result, &alone), "a lone String is copied");
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
                "{s}"
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
    assert_eq!(result, Value::variant(bv::SOME, vec![Value::Int(99)]));
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
        decoding: None,
        statement: false,
        connection: false,
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
            &format!("float overflow: {0}.0 + {0}.0", f64::MAX),
        );
        binary(
            float(f64::MAX),
            Asm::Mul,
            float(2.0),
            &format!("float overflow: {}.0 * 2.0", f64::MAX),
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
            "division by zero at 2:10; closure in make 2:10; main 8:11; <script> 4:1"
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
            "division by zero at 5:13; closure in main 5:13; main 4:12; <script> 2:1"
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
            "integer overflow: 4611686018427387904 * 2 at 3:3; step 3:3; closure in main 7:44; main 7:15; <script> 5:1"
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
      _ -> Step.go(self, k - 1)
    }
  }
}
fn main() {
  let c = C { n: 1 }
  println(Step.go(c, 5))
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
    fn in_a_tail_call_after_more_than_the_log_keeps() {
        // Two functions in turn, a cycle that divides the log's size:
        // however many tail calls there were, the latest 32 callers,
        // the latest first.
        for n in [34, 50, 100] {
            let source = format!(
                "\
fn ping(n: Int) -> Int {{
  match n <= 0 {{
    true -> 1 / n
    false -> pong(n - 1)
  }}
}}
fn pong(n: Int) -> Int {{
  ping(n - 1)
}}
fn main() {{
  println(ping({n}))
}}
"
            );
            let kept = "pong 8:3; ping 4:14; ".repeat(16);
            assert_eq!(
                stops(&source),
                format!("division by zero at 3:13; ping 3:13; {kept}main 11:11; <script> 10:1")
            );
        }
        // Three functions in turn, a cycle that does not divide it.
        for n in [11, 16, 17, 100] {
            let source = format!(
                "\
fn a(n: Int) -> Int {{
  match n <= 0 {{
    true -> 1 / n
    false -> b(n - 1)
  }}
}}
fn b(n: Int) -> Int {{
  c(n)
}}
fn c(n: Int) -> Int {{
  a(n)
}}
fn main() {{
  println(a({n}))
}}
"
            );
            let kept = "c 11:3; b 8:3; a 4:14; ".repeat(10) + "c 11:3; b 8:3";
            assert_eq!(
                stops(&source),
                format!("division by zero at 3:13; a 3:13; {kept}; main 14:11; <script> 13:1")
            );
        }
        // Three frames that have each made more tail calls than the
        // log keeps: each frame's latest 32 callers come after it.
        let source = "\
fn inner(n: Int) -> Int {
  match n {
    0 -> 1 / n
    _ -> inner(n - 1)
  }
}
fn mid(n: Int) -> Int {
  match n {
    0 -> 1 + inner(40)
    _ -> mid(n - 1)
  }
}
fn outer(n: Int) -> Int {
  match n {
    0 -> 1 + mid(50)
    _ -> outer(n - 1)
  }
}
fn main() {
  println(outer(60))
}
";
        assert_eq!(
            stops(source),
            "division by zero at 3:10; inner 3:10; inner 4:10 (32 times); mid 9:14; mid 10:10 (32 times); outer 15:14; outer 16:10 (32 times); main 20:11; <script> 19:1"
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
            "joined task failed: division by zero at 3:3; work 3:3; closure in main 6:27; main 7:11; <script> 5:1"
        );
    }

    #[test]
    fn the_join_of_a_cancelled_task() {
        let source = "\
import channel
import task
fn wait(ch: Channel(Int)) -> Int {
  match channel.receive(ch) {
    Some(n) -> n
    None -> 0
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
            "division by zero at 5:3; slow 5:3; closure in main 8:46; main 8:11; <script> 7:1"
        );
        // The same from a task: the join's error has the stack of
        // the function, of the task that set the deadline, and of the
        // join.
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
            "joined task failed: division by zero at 5:3; slow 5:3; closure in main 8:62; closure in main 8:27; main 9:11; <script> 7:1"
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
            decoding: None,
            statement: false,
            connection: false,
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
        log.log(3, &f, 10);
        log.log(3, &g, 20);
        log.log(3, &f, 30);
        assert!(!log.is_empty());
        assert_eq!(entries(&log), ["3 f 30", "3 g 20", "3 f 10"]);

        // One more than the log keeps: the first is gone, at every
        // place of the ring.
        for extra in 0..=2 * TCO_ELIDED_CAP {
            let mut log = TailCalls::default();
            let made = TCO_ELIDED_CAP + extra;
            for ip in 0..made {
                log.log(0, &f, ip);
            }
            let latest: Vec<String> = (made - TCO_ELIDED_CAP..made)
                .rev()
                .map(|ip| format!("0 f {ip}"))
                .collect();
            assert_eq!(entries(&log), latest);
        }
    }

    #[test]
    fn a_cycle_of_functions_keeps_the_latest_callers_of_the_cycle() {
        // A frame whose tail calls go through a cycle of `len`
        // functions, some lengths dividing the log's size and some
        // not: after each tail call the log holds the latest callers,
        // as a list of them all would.
        let names = ["f", "g", "h", "i", "j"];
        let functions: Vec<Arc<Function>> = names.into_iter().map(function).collect();
        for len in 1..=names.len() {
            let mut log = TailCalls::default();
            let mut all = Vec::new();
            for ip in 0..3 * TCO_ELIDED_CAP + 7 {
                log.log(2, &functions[ip % len], ip);
                all.push(format!("2 {} {ip}", names[ip % len]));
                let latest: Vec<String> = all.iter().rev().take(TCO_ELIDED_CAP).cloned().collect();
                assert_eq!(entries(&log), latest, "a cycle of {len}, after {ip}");
            }
        }
        // The logs are gone: the functions are held by the test alone.
        for f in &functions {
            assert_eq!(Arc::strong_count(f), 1);
        }
    }

    #[test]
    fn a_full_log_leaves_the_count_of_a_function_that_calls_itself_alone() {
        use std::sync::atomic::{AtomicBool, Ordering};

        // Once a frame has made as many tail calls as the log keeps, a
        // function that calls itself, or a cycle of functions whose
        // length divides the log's size, takes the place of its own
        // entry: the function's count, which every task running it
        // shares, stays as it is. Another thread reads the count all
        // along, so a clone and a drop between two reads would show.
        let (f, g) = (function("f"), function("g"));
        for cycle in [vec![&f], vec![&f, &g]] {
            let mut log = TailCalls::default();
            for ip in 0..TCO_ELIDED_CAP {
                log.log(0, cycle[ip % cycle.len()], ip);
            }
            let held = 1 + TCO_ELIDED_CAP / cycle.len();
            assert_eq!(Arc::strong_count(&f), held);
            let done = AtomicBool::new(false);
            let seen = std::thread::scope(|scope| {
                let reader = scope.spawn(|| {
                    let mut seen = Vec::new();
                    for _ in 0..1_000_000 {
                        let count = Arc::strong_count(&f);
                        if !seen.contains(&count) {
                            seen.push(count);
                        }
                    }
                    done.store(true, Ordering::Relaxed);
                    seen
                });
                let mut ip = TCO_ELIDED_CAP;
                while !done.load(Ordering::Relaxed) {
                    log.log(0, cycle[ip % cycle.len()], ip);
                    ip += 1;
                }
                reader.join().expect("the reader ends")
            });
            assert_eq!(seen, [held], "a cycle of {}", cycle.len());
            assert_eq!(Arc::strong_count(&f), held);
            // The entries are the latest tail calls all the same.
            assert_eq!(entries(&log).len(), TCO_ELIDED_CAP);
        }
        assert_eq!(Arc::strong_count(&f), 1);
        assert_eq!(Arc::strong_count(&g), 1);
    }

    #[test]
    fn each_frame_has_its_own_and_they_go_with_it() {
        let (f, g, h) = (function("f"), function("g"), function("h"));
        let mut log = TailCalls::default();
        log.log(1, &f, 1);
        log.log(1, &f, 2);
        // A frame above makes more than the log keeps.
        for ip in 0..TCO_ELIDED_CAP + 3 {
            log.log(4, &g, ip);
        }
        log.log(6, &h, 7);
        let all = entries(&log);
        assert_eq!(all.len(), 2 + TCO_ELIDED_CAP + 1);
        assert_eq!(all[0], "6 h 7");
        assert_eq!(all[1], format!("4 g {}", TCO_ELIDED_CAP + 2));
        assert_eq!(all[TCO_ELIDED_CAP], "4 g 3");
        assert_eq!(all[TCO_ELIDED_CAP + 1..], ["1 f 2", "1 f 1"]);

        // The frames at 5 and above go; the one at 4 goes on.
        log.prune(5);
        assert_eq!(entries(&log).len(), 2 + TCO_ELIDED_CAP);
        log.log(4, &h, 99);
        assert_eq!(
            entries(&log)[..2],
            ["4 h 99".to_string(), format!("4 g {}", TCO_ELIDED_CAP + 2)]
        );
        assert_eq!(entries(&log).len(), 2 + TCO_ELIDED_CAP);

        log.prune(4);
        assert_eq!(entries(&log), ["1 f 2", "1 f 1"]);
        // A new frame at the depth of one that is gone starts anew.
        log.log(4, &g, 5);
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
            "string interpolation requires string values, got List",
            |e| {
                push(e, string("a"));
                push(e, Value::list(vec![Value::Int(1)]));
                let segments = [Segment::Value, Segment::Value];
                e.emit(
                    Asm::Interpolate {
                        segments: &segments,
                    },
                    span(),
                )
                .unwrap();
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
                    at: 0,
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
            push(e, Value::map([]));
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

/// A push builds its value in its slot of the stack and makes room
/// first (`push_with` in run.rs). A VM's stack starts without room and
/// doubles, so a run of pushes of one form crosses its capacity
/// several times at a push of exactly that form; one that found no
/// room would panic.
mod pushes_that_make_room {
    use super::*;
    use crate::bytecode::VmClosure;

    /// More than the stack holds after eight doublings.
    const COUNT: usize = 300;

    fn closure() -> Value {
        Value::VmClosure(Arc::new(VmClosure {
            function: Arc::new(Function::returning_unit("f".to_string(), 0)),
            upvalues: vec![],
        }))
    }

    /// A value of every sort the instruction loop builds in place (a
    /// number, a Bool, unit, a closure), and three of the kinds it
    /// clones and moves.
    fn samples() -> Vec<Value> {
        vec![
            Value::Int(-7),
            float(2.5),
            Value::Bool(true),
            Value::Unit,
            closure(),
            Value::String("text".into()),
            Value::list(vec![Value::Int(1), Value::Unit]),
            Value::tuple(vec![float(0.5), Value::Bool(false)]),
        ]
    }

    /// Whether `a` is `b`: the same closure, or an equal value.
    fn same(a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::VmClosure(a), Value::VmClosure(b)) => Arc::ptr_eq(a, b),
            _ => a == b,
        }
    }

    /// Emit `before`, then `push` `COUNT` times, then a list of what
    /// was pushed, which the function returns.
    fn listing(e: &mut Emitter, before: impl FnOnce(&mut Emitter), push: impl Fn(&mut Emitter)) {
        before(e);
        for _ in 0..COUNT {
            push(e);
        }
        e.emit(Asm::MakeList { count: COUNT }, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
    }

    /// The values of the list `value`, which are `COUNT` of `expected`.
    fn all_are(value: Value, expected: &Value) {
        let Value::List(list) = value else {
            panic!("a list, not {value:?}")
        };
        assert_eq!(list.len(), COUNT);
        for pushed in list.iter() {
            assert!(same(&pushed, expected), "{pushed:?} is not {expected:?}");
        }
    }

    #[test]
    fn a_constant_of_every_kind() {
        for sample in samples() {
            let script = make_function(|e| {
                let k = e.constant(sample.clone(), span()).unwrap();
                listing(e, |_| {}, |e| e.emit(Asm::Constant { k }, span()).unwrap());
            });
            all_are(
                Vm::new(crate::HostIo::process()).run(script).unwrap(),
                &sample,
            );
        }
    }

    #[test]
    fn true_false_and_unit() {
        for (asm, value) in [
            (Asm::True, Value::Bool(true)),
            (Asm::False, Value::Bool(false)),
            (Asm::Unit, Value::Unit),
        ] {
            let script = make_function(|e| listing(e, |_| {}, |e| e.emit(asm, span()).unwrap()));
            all_are(
                Vm::new(crate::HostIo::process()).run(script).unwrap(),
                &value,
            );
        }
    }

    #[test]
    fn a_local_of_every_kind() {
        for sample in samples() {
            let script = make_function(|e| {
                let k = e.constant(sample.clone(), span()).unwrap();
                listing(
                    e,
                    |e| e.emit(Asm::Constant { k }, span()).unwrap(),
                    |e| e.emit(Asm::GetLocal { slot: 0 }, span()).unwrap(),
                );
            });
            all_are(
                Vm::new(crate::HostIo::process()).run(script).unwrap(),
                &sample,
            );
        }
    }

    #[test]
    fn a_global_of_every_kind() {
        for sample in samples() {
            let script = make_function(|e| {
                let k = e.constant(sample.clone(), span()).unwrap();
                listing(
                    e,
                    |e| {
                        e.emit(Asm::Constant { k }, span()).unwrap();
                        e.emit(Asm::SetGlobal { slot: 0 }, span()).unwrap();
                        e.emit(Asm::Pop, span()).unwrap();
                    },
                    |e| e.emit(Asm::GetGlobal { slot: 0 }, span()).unwrap(),
                );
            });
            let mut vm = Vm::new(crate::HostIo::process());
            vm.view.slots = Arc::from([std::sync::OnceLock::new()]);
            let pushed = vm.run(script).unwrap();
            match &sample {
                // A closure: the VM's own copy of it, one copy each time.
                Value::VmClosure(shared) => {
                    let own = vm.own.of(0, shared);
                    assert!(!Arc::ptr_eq(&own, shared));
                    all_are(pushed, &Value::VmClosure(own));
                }
                _ => all_are(pushed, &sample),
            }
        }
    }

    #[test]
    fn an_upvalue_of_every_kind() {
        for sample in samples() {
            let mut inner = Emitter::new("inner".to_string(), 0, span()).unwrap();
            listing(
                &mut inner,
                |_| {},
                |e| e.emit(Asm::GetUpvalue { index: 0 }, span()).unwrap(),
            );
            let inner = Value::VmClosure(Arc::new(VmClosure {
                function: Arc::new(inner.finish(1).unwrap_or_else(|e| panic!("{e:?}"))),
                upvalues: vec![sample.clone()],
            }));
            let script = make_function(|e| {
                let k = e.constant(inner, span()).unwrap();
                e.emit(Asm::Constant { k }, span()).unwrap();
                e.emit(Asm::Call { argc: 0 }, span()).unwrap();
                e.emit(Asm::Return, span()).unwrap();
            });
            all_are(
                Vm::new(crate::HostIo::process()).run(script).unwrap(),
                &sample,
            );
        }
    }

    /// The stack keeps what was pushed before it grew: values of every
    /// kind in turn, pushed past eight doublings, come back in order.
    #[test]
    fn what_was_pushed_before_the_stack_grew_is_kept() {
        let samples = samples();
        let script = make_function(|e| {
            let constants: Vec<_> = samples
                .iter()
                .map(|sample| e.constant(sample.clone(), span()).unwrap())
                .collect();
            for i in 0..COUNT {
                let k = constants[i % constants.len()];
                e.emit(Asm::Constant { k }, span()).unwrap();
            }
            e.emit(Asm::MakeList { count: COUNT }, span()).unwrap();
            e.emit(Asm::Return, span()).unwrap();
        });
        let Value::List(list) = Vm::new(crate::HostIo::process()).run(script).unwrap() else {
            panic!("a list")
        };
        assert_eq!(list.len(), COUNT);
        for (i, pushed) in list.iter().enumerate() {
            let expected = &samples[i % samples.len()];
            assert!(
                same(&pushed, expected),
                "{i}: {pushed:?} is not {expected:?}"
            );
        }
    }
}

/// Values whose end is seen, for the tests of what an instruction lets
/// go of and in which order.
mod seen_drops {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::runtime::sync::Channel;
    use crate::value::{HostFn, HostShape};

    /// The numbers of the probes that ended, in the order they ended.
    pub(super) type Log = Arc<Mutex<Vec<usize>>>;

    struct Probe(usize, Log);

    impl Drop for Probe {
        fn drop(&mut self) {
            self.1.lock().unwrap().push(self.0);
        }
    }

    /// A function that gives a new value at each call: the first is
    /// probe 1, the next probe 2. A probe is a value nothing else
    /// holds: when the VM lets go of it, its number is written to the
    /// log. (A constant would not do: the function's constants hold it
    /// too.)
    pub(super) fn maker() -> (Value, Log) {
        let log = Log::default();
        let (made, seen) = (AtomicUsize::new(0), log.clone());
        let make = move |_: &[Value]| {
            let probe = Probe(made.fetch_add(1, Ordering::SeqCst) + 1, seen.clone());
            Ok(Value::HostFn(Arc::new(HostFn {
                name: "probe".to_string(),
                call: Arc::new(move |_| {
                    let _ = &probe;
                    Ok(Value::Unit)
                }),
                returns: HostShape::Any,
            })))
        };
        let maker = Value::HostFn(Arc::new(HostFn {
            name: "maker".to_string(),
            call: Arc::new(make),
            returns: HostShape::Any,
        }));
        (maker, log)
    }

    /// Two functions that number their probes together: the first as
    /// [`maker`]'s, the second gives a new closure of `function` at
    /// each call, which captures a new probe and is held by nothing
    /// else (the function's one upvalue).
    pub(super) fn makers(function: Arc<Function>) -> (Value, Value, Log) {
        let log = Log::default();
        let made = Arc::new(AtomicUsize::new(0));
        let new_probe = {
            let (made, log) = (made.clone(), log.clone());
            move || {
                let probe = Probe(made.fetch_add(1, Ordering::SeqCst) + 1, log.clone());
                Value::HostFn(Arc::new(HostFn {
                    name: "probe".to_string(),
                    call: Arc::new(move |_| {
                        let _ = &probe;
                        Ok(Value::Unit)
                    }),
                    returns: HostShape::Any,
                }))
            }
        };
        let host = |name: &str, call: crate::value::HostImpl| {
            Value::HostFn(Arc::new(HostFn {
                name: name.to_string(),
                call,
                returns: HostShape::Any,
            }))
        };
        let probes = {
            let new_probe = new_probe.clone();
            host("maker", Arc::new(move |_| Ok(new_probe())))
        };
        let closures = host(
            "closures",
            Arc::new(move |_| {
                Ok(Value::VmClosure(Arc::new(crate::bytecode::VmClosure {
                    function: function.clone(),
                    upvalues: vec![new_probe()],
                })))
            }),
        );
        (probes, closures, log)
    }

    /// A function that gives a new channel at each call, and the
    /// channels it gave, each as long as something holds it. (A channel
    /// can be compared, which a function cannot; it does not say when
    /// it ended, only that it has.)
    pub(super) fn channels() -> (Value, Arc<Mutex<Vec<std::sync::Weak<Channel>>>>) {
        let given = Arc::new(Mutex::new(Vec::new()));
        let (made, seen) = (AtomicUsize::new(0), given.clone());
        let make = move |_: &[Value]| {
            let channel = Channel::new(made.fetch_add(1, Ordering::SeqCst), 0);
            seen.lock().unwrap().push(Arc::downgrade(&channel));
            Ok(Value::Channel(channel))
        };
        let maker = Value::HostFn(Arc::new(HostFn {
            name: "channels".to_string(),
            call: Arc::new(make),
            returns: HostShape::Any,
        }));
        (maker, given)
    }

    /// Emit the push of a new probe, made by the maker constant `k`.
    pub(super) fn probe(e: &mut Emitter, k: crate::bytecode::Const) {
        e.emit(Asm::Constant { k }, span()).unwrap();
        e.emit(Asm::Call { argc: 0 }, span()).unwrap();
    }

    /// What the log holds now.
    pub(super) fn ended(log: &Log) -> Vec<usize> {
        log.lock().unwrap().clone()
    }

    /// [`left_with`] the maker of probes: the VM, what the loop gave,
    /// and the log.
    pub(super) fn left_by(
        build: impl FnOnce(&mut Emitter, crate::bytecode::Const),
    ) -> (Vm, Result<Value, VmError>, Log) {
        let (maker, log) = maker();
        let (vm, run) = left_with(maker, build);
        (vm, run, log)
    }

    /// Run the script `build` emits, given the constant of `maker`,
    /// until it ends or fails, and give the VM as the instruction loop
    /// left it (nothing is cut back or unwound yet) and what the loop
    /// gave.
    pub(super) fn left_with(
        maker: Value,
        build: impl FnOnce(&mut Emitter, crate::bytecode::Const),
    ) -> (Vm, Result<Value, VmError>) {
        let script = make_function(|e| {
            let k = e.constant(maker, span()).unwrap();
            build(e, k);
        });
        let mut vm = Vm::new(crate::HostIo::process());
        let floor = vm.frames.len();
        vm.frames.push(Frame::Code(CallFrame {
            closure: Arc::new(crate::bytecode::VmClosure {
                function: script,
                upvalues: vec![],
            }),
            ip: 0,
            base_slot: 0,
        }));
        let run = vm.run_thread(floor, |vm| vm.run_frames(floor, usize::MAX));
        let run = run.map(|slice| match slice {
            crate::vm::run::Slice::Done(value) => value,
            _ => panic!("the script runs to its end or to an error"),
        });
        (vm, run)
    }
}

/// An instruction of two operands (arithmetic, a comparison, `==`,
/// `!=`) reads them where they lie on the stack and writes its result
/// over the first.
mod two_operands {
    use super::seen_drops::{channels, ended, left_by, left_with, probe};
    use super::*;

    fn string(s: &str) -> Value {
        Value::String(s.into())
    }

    /// Emit the push of `value`.
    fn push(e: &mut Emitter, value: Value) {
        let k = e.constant(value, span()).unwrap();
        e.emit(Asm::Constant { k }, span()).unwrap();
    }

    /// `a op b`, computed above a value that must stay what it is.
    fn result(a: Value, op: Asm<'static>, b: Value) -> Value {
        let below = string("below");
        let script = make_function(|e| {
            push(e, below.clone());
            push(e, a);
            push(e, b);
            e.emit(op, span()).unwrap();
            e.emit(Asm::MakeTuple { count: 2 }, span()).unwrap();
            e.emit(Asm::Return, span()).unwrap();
        });
        let Value::Tuple(pair) = Vm::new(crate::HostIo::process()).run(script).unwrap() else {
            panic!("a tuple")
        };
        let [kept, result] = &pair[..] else {
            panic!("a pair")
        };
        assert_eq!(kept, &below);
        result.clone()
    }

    #[test]
    fn two_ints() {
        let int = Value::Int;
        for (a, op, b, expected) in [
            (7, Asm::Add, 5, int(12)),
            (7, Asm::Sub, 5, int(2)),
            (7, Asm::Mul, -5, int(-35)),
            (-7, Asm::Div, 2, int(-3)),
            (-7, Asm::Mod, 2, int(-1)),
            (i64::MAX - 1, Asm::Add, 1, int(i64::MAX)),
            (i64::MIN + 1, Asm::Sub, 1, int(i64::MIN)),
            (7, Asm::Lt, 5, Value::Bool(false)),
            (7, Asm::Gt, 5, Value::Bool(true)),
            (5, Asm::Leq, 5, Value::Bool(true)),
            (5, Asm::Geq, 6, Value::Bool(false)),
            (5, Asm::Eq, 5, Value::Bool(true)),
            (5, Asm::Neq, 5, Value::Bool(false)),
        ] {
            assert_eq!(result(int(a), op, int(b)), expected, "{a} {op:?} {b}");
        }
    }

    #[test]
    fn two_floats() {
        for (a, op, b, expected) in [
            (7.5, Asm::Add, 0.25, float(7.75)),
            (7.5, Asm::Sub, 0.25, float(7.25)),
            (7.5, Asm::Mul, -2.0, float(-15.0)),
            (7.5, Asm::Div, 2.0, float(3.75)),
            (7.5, Asm::Mod, 2.0, float(1.5)),
            (7.5, Asm::Lt, 0.25, Value::Bool(false)),
            (7.5, Asm::Gt, 0.25, Value::Bool(true)),
            (0.25, Asm::Leq, 0.25, Value::Bool(true)),
            (0.25, Asm::Geq, 7.5, Value::Bool(false)),
            (0.25, Asm::Eq, 0.25, Value::Bool(true)),
            (0.25, Asm::Neq, 0.25, Value::Bool(false)),
        ] {
            assert_eq!(result(float(a), op, float(b)), expected, "{a} {op:?} {b}");
        }
    }

    /// A Float result that is zero is the one zero a Float has.
    #[test]
    fn a_float_result_of_zero_has_no_sign() {
        for (a, op, b) in [
            (0.0, Asm::Mul, -1.0),
            (-1.0, Asm::Mul, 0.0),
            (0.0, Asm::Div, -4.0),
            (-0.5, Asm::Add, 0.5),
            (-3.0, Asm::Mod, 3.0),
        ] {
            let Value::Float(zero) = result(float(a), op, float(b)) else {
                panic!("a Float")
            };
            assert_eq!(zero.get().to_bits(), 0.0_f64.to_bits(), "{a} {op:?} {b}");
        }
    }

    #[test]
    fn two_values_of_another_kind_are_compared() {
        let list = |items: &[i64]| Value::list(items.iter().copied().map(Value::Int).collect());
        for (a, op, b, expected) in [
            (string("a"), Asm::Lt, string("b"), true),
            (string("b"), Asm::Leq, string("a"), false),
            (string("a"), Asm::Eq, string("a"), true),
            (string("a"), Asm::Neq, string("a"), false),
            (list(&[1, 2]), Asm::Gt, list(&[1]), true),
            (list(&[1, 2]), Asm::Eq, list(&[1, 2]), true),
            (list(&[1, 2]), Asm::Neq, list(&[2, 1]), true),
            (Value::Bool(false), Asm::Lt, Value::Bool(true), true),
            (Value::Bool(true), Asm::Eq, Value::Bool(false), false),
            (Value::Unit, Asm::Eq, Value::Unit, true),
            (Value::Unit, Asm::Geq, Value::Unit, true),
        ] {
            let shown = format!("{a:?} {op:?} {b:?}");
            assert_eq!(result(a, op, b), Value::Bool(expected), "{shown}");
        }
    }

    /// The pairs of operands each instruction refuses, in every order.
    fn refused() -> Vec<(Value, Asm<'static>, Value)> {
        let int = Value::Int;
        let mut pairs = vec![];
        for op in [Asm::Add, Asm::Sub, Asm::Mul, Asm::Div, Asm::Mod] {
            for (a, b) in [
                (int(1), float(2.5)),
                (float(2.5), int(1)),
                (string("a"), string("b")),
                (string("a"), int(1)),
                (int(1), string("a")),
                (Value::Bool(true), Value::Bool(false)),
                (Value::Unit, int(1)),
                (float(1.5), Value::Unit),
            ] {
                pairs.push((a, op, b));
            }
        }
        pairs.extend([
            (int(i64::MAX), Asm::Add, int(1)),
            (int(i64::MIN), Asm::Sub, int(1)),
            (int(i64::MAX), Asm::Mul, int(2)),
            (int(i64::MIN), Asm::Div, int(-1)),
            (int(i64::MIN), Asm::Mod, int(-1)),
            (int(7), Asm::Div, int(0)),
            (int(7), Asm::Mod, int(0)),
            (float(7.0), Asm::Div, float(0.0)),
            (float(7.0), Asm::Mod, float(0.0)),
            (float(f64::MAX), Asm::Add, float(f64::MAX)),
            (float(f64::MAX), Asm::Mul, float(2.0)),
            (float(f64::MIN), Asm::Sub, float(f64::MAX)),
        ]);
        for op in [Asm::Lt, Asm::Gt, Asm::Leq, Asm::Geq, Asm::Eq, Asm::Neq] {
            for (a, b) in [
                (int(1), string("a")),
                (string("a"), int(1)),
                (int(1), float(1.0)),
                (Value::Bool(true), Value::Unit),
            ] {
                pairs.push((a, op, b));
            }
        }
        pairs
    }

    /// An instruction that refuses its operands has taken both off the
    /// stack when it fails, and nothing else: what is read of the
    /// stack after an error (the REPL goes on, an embedder runs the
    /// next program) finds it as it was below them.
    #[test]
    fn refused_operands_are_off_the_stack_at_the_error() {
        for (a, op, b) in refused() {
            let shown = format!("{a:?} {op:?} {b:?}");
            let (vm, run, _) = left_by(|e, _| {
                push(e, string("below"));
                e.emit(Asm::Unit, span()).unwrap();
                push(e, a);
                push(e, b);
                e.emit(op, span()).unwrap();
                e.emit(Asm::Return, span()).unwrap();
            });
            assert!(run.is_err(), "{shown} is refused");
            assert_eq!(vm.stack, [string("below"), Value::Unit], "{shown}");
        }
    }

    /// Operands that are refused are let go of the first before the
    /// second.
    #[test]
    fn refused_operands_end_the_first_before_the_second() {
        for op in [
            Asm::Add,
            Asm::Sub,
            Asm::Mul,
            Asm::Div,
            Asm::Mod,
            Asm::Lt,
            Asm::Gt,
            Asm::Leq,
            Asm::Geq,
            Asm::Eq,
            Asm::Neq,
        ] {
            let (vm, run, log) = left_by(|e, k| {
                probe(e, k);
                probe(e, k);
                e.emit(op, span()).unwrap();
                e.emit(Asm::Return, span()).unwrap();
            });
            assert!(run.is_err(), "{op:?} of two functions is refused");
            assert_eq!(ended(&log), [1, 2], "{op:?}");
            assert!(vm.stack.is_empty());
        }
    }

    /// Operands that `==` and `!=` accept are let go of by the
    /// instruction: nothing holds them when it is done. (No value that
    /// can be compared says in which order it ended: a probe is a
    /// function, and a function is refused, alone or in a list. And no
    /// value that `<` accepts has an end that is seen at all: it
    /// refuses a channel too.)
    #[test]
    fn compared_operands_are_let_go_of() {
        for op in [Asm::Eq, Asm::Neq] {
            let (maker, given) = channels();
            let (vm, run) = left_with(maker, |e, k| {
                probe(e, k);
                probe(e, k);
                e.emit(op, span()).unwrap();
                e.emit(Asm::Return, span()).unwrap();
            });
            assert!(matches!(run, Ok(Value::Bool(_))), "{op:?} of two channels");
            let given = given.lock().unwrap();
            assert_eq!(given.len(), 2);
            for channel in given.iter() {
                assert_eq!(channel.strong_count(), 0, "{op:?}");
            }
            drop(vm);
        }
    }
}

/// The instructions that test the value on top (`JumpIfFalse`,
/// `JumpIfTrue`), let go of it (`Pop`) or set a slot from another
/// (`SetLocal`, `Recur`) read and write the values where they lie.
mod slots_in_place {
    use super::seen_drops::{ended, left_by, probe};
    use super::*;
    use crate::bytecode::VmClosure;

    /// Emit the push of `value`.
    fn push(e: &mut Emitter, value: Value) {
        let k = e.constant(value, span()).unwrap();
        e.emit(Asm::Constant { k }, span()).unwrap();
    }

    fn closure() -> Value {
        Value::VmClosure(Arc::new(VmClosure {
            function: Arc::new(Function::returning_unit("f".to_string(), 0)),
            upvalues: vec![],
        }))
    }

    /// A value of every sort the instruction loop knows (`Sort` in
    /// run.rs) that can be compared with `==`.
    fn samples() -> Vec<Value> {
        vec![
            Value::Int(-7),
            float(2.5),
            Value::Bool(true),
            Value::Bool(false),
            Value::Unit,
            Value::String("text".into()),
            Value::list(vec![Value::Int(1), Value::Unit]),
        ]
    }

    /// `1` if the jump `jump` is taken on `value`, `2` if it is not.
    fn taken(value: Value, jump: fn(crate::bytecode::Label) -> Asm<'static>) -> Value {
        let script = make_function(|e| {
            push(e, value);
            let (there, end) = (e.label(), e.label());
            e.emit(jump(there), span()).unwrap();
            push(e, Value::Int(2));
            e.emit(Asm::Jump { to: end }, span()).unwrap();
            e.bind(there, span()).unwrap();
            push(e, Value::Int(1));
            e.bind(end, span()).unwrap();
            e.emit(Asm::Return, span()).unwrap();
        });
        Vm::new(crate::HostIo::process()).run(script).unwrap()
    }

    /// `false` and unit are false to a jump; a value of any other kind
    /// is true.
    #[test]
    fn a_jump_tests_the_value_on_top() {
        let (if_false, if_true): (fn(_) -> _, fn(_) -> _) =
            (|to| Asm::JumpIfFalse { to }, |to| Asm::JumpIfTrue { to });
        for value in samples().into_iter().chain([closure()]) {
            let falsy = matches!(value, Value::Bool(false) | Value::Unit);
            let (yes, no) = (Value::Int(1), Value::Int(2));
            let shown = format!("{value:?}");
            assert_eq!(
                taken(value.clone(), if_false),
                if falsy { yes.clone() } else { no.clone() },
                "JumpIfFalse on {shown}"
            );
            assert_eq!(
                taken(value, if_true),
                if falsy { no } else { yes },
                "JumpIfTrue on {shown}"
            );
        }
    }

    /// A jump and `Pop` let go of the value they take off the stack,
    /// then and once.
    #[test]
    fn a_jump_and_pop_let_go_of_the_value_on_top() {
        for take in [
            |e: &mut Emitter| e.emit(Asm::Pop, span()).unwrap(),
            |e: &mut Emitter| {
                let next = e.label();
                e.emit(Asm::JumpIfFalse { to: next }, span()).unwrap();
                e.bind(next, span()).unwrap();
            },
            |e: &mut Emitter| {
                let next = e.label();
                e.emit(Asm::JumpIfTrue { to: next }, span()).unwrap();
                e.bind(next, span()).unwrap();
            },
        ] {
            let (vm, run, log) = left_by(|e, k| {
                probe(e, k);
                probe(e, k);
                take(e);
                e.emit(Asm::Unit, span()).unwrap();
                e.emit(Asm::Return, span()).unwrap();
            });
            assert_eq!(run.unwrap(), Value::Unit);
            // The second probe was on top; the first was held until
            // the script's return cut the stack back.
            assert_eq!(ended(&log), [2, 1]);
            assert!(vm.stack.is_empty());
        }
    }

    /// `SetLocal` writes a copy of the value on top over a slot: the
    /// slot's old value ends, the value on top stays.
    #[test]
    fn set_local_of_every_kind_over_every_kind() {
        for new in samples() {
            for old in samples() {
                let script = make_function(|e| {
                    push(e, old.clone());
                    push(e, new.clone());
                    e.emit(Asm::SetLocal { slot: 0 }, span()).unwrap();
                    e.emit(Asm::GetLocal { slot: 0 }, span()).unwrap();
                    e.emit(Asm::MakeTuple { count: 2 }, span()).unwrap();
                    e.emit(Asm::Return, span()).unwrap();
                });
                assert_eq!(
                    Vm::new(crate::HostIo::process()).run(script).unwrap(),
                    Value::tuple(vec![new.clone(), new.clone()]),
                    "{new:?} over {old:?}"
                );
            }
        }
    }

    #[test]
    fn set_local_ends_the_value_that_was_in_the_slot() {
        let (vm, run, log) = left_by(|e, k| {
            probe(e, k);
            probe(e, k);
            e.emit(Asm::SetLocal { slot: 0 }, span()).unwrap();
            e.emit(Asm::Pop, span()).unwrap();
            e.emit(Asm::Unit, span()).unwrap();
            e.emit(Asm::Return, span()).unwrap();
        });
        assert_eq!(run.unwrap(), Value::Unit);
        // The first probe was in the slot; the second was there from
        // then on, until the script's return cut the stack back.
        assert_eq!(ended(&log), [1, 2]);
        assert!(vm.stack.is_empty());
    }

    /// `Recur` writes the values on top over the loop's bindings and
    /// cuts the stack back to them.
    #[test]
    fn recur_of_every_kind_over_every_kind() {
        let samples = samples();
        let count = samples.len();
        let script = make_function(|e| {
            // The bindings, then their next values: the same, turned
            // by one.
            for sample in &samples {
                push(e, sample.clone());
            }
            for i in 0..count {
                push(e, samples[(i + 1) % count].clone());
            }
            // (A jump to the next instruction.)
            let next = e.label();
            e.emit(
                Asm::Recur {
                    argc: count,
                    first: 0,
                    to: next,
                },
                span(),
            )
            .unwrap();
            e.bind(next, span()).unwrap();
            e.emit(Asm::MakeTuple { count }, span()).unwrap();
            e.emit(Asm::Return, span()).unwrap();
        });
        let mut turned = samples.clone();
        turned.rotate_left(1);
        assert_eq!(
            Vm::new(crate::HostIo::process()).run(script).unwrap(),
            Value::tuple(turned)
        );
    }

    /// The bindings' old values end in the bindings' order, at the
    /// `Recur`; the new ones are held by the bindings from then on,
    /// until the script's return cuts the stack back.
    #[test]
    fn recur_ends_the_old_values_of_the_bindings_in_order() {
        let (vm, run, log) = left_by(|e, k| {
            for _ in 0..6 {
                probe(e, k);
            }
            let next = e.label();
            e.emit(
                Asm::Recur {
                    argc: 3,
                    first: 0,
                    to: next,
                },
                span(),
            )
            .unwrap();
            e.bind(next, span()).unwrap();
            e.emit(Asm::Unit, span()).unwrap();
            e.emit(Asm::Return, span()).unwrap();
        });
        assert_eq!(run.unwrap(), Value::Unit);
        assert_eq!(ended(&log), [1, 2, 3, 4, 5, 6]);
        assert!(vm.stack.is_empty());
    }
}

/// A call of a closure enters it by its pointer, and a number, a Bool
/// or unit that a function returns to its caller is written from the
/// top slot over the callee's own slot. What a return lets go of, and
/// in which order, is one thing for every kind of result: the callee,
/// then its arguments and locals from the lowest slot up.
mod calls_and_returns {
    use super::seen_drops::{ended, makers};
    use super::*;
    use crate::bytecode::VmClosure;
    use crate::value::{HostFn, HostShape};

    /// Emit the push of `value`.
    fn push(e: &mut Emitter, value: Value) {
        let k = e.constant(value, span()).unwrap();
        e.emit(Asm::Constant { k }, span()).unwrap();
    }

    /// The results a function returns in the tests: a value of every
    /// sort the instruction loop knows (`Sort` in run.rs).
    fn results() -> Vec<Value> {
        vec![
            Value::Int(-7),
            float(2.5),
            Value::Bool(true),
            Value::Bool(false),
            Value::Unit,
            Value::String("text".into()),
            Value::list(vec![Value::Int(1), Value::Unit]),
            Value::tuple(vec![float(0.5), Value::Bool(false)]),
        ]
    }

    /// A function of two parameters that makes a probe its local and
    /// returns its result, called with two probes, a closure that owns
    /// a probe: when the function returns, the callee ends (probe 1,
    /// its upvalue), then the arguments (2, 3), then the local (4).
    /// The caller, a function's frame, gets the result and returns it
    /// with a probe of its own beside it (5), which ends last.
    #[test]
    fn a_return_ends_the_callee_then_its_slots_from_the_lowest_up() {
        for result in results() {
            returned(result);
        }
    }

    fn returned(result: Value) {
        // The callee: `fn inner(a, b) { let local = probe(); result }`,
        // where `probe` is global 0.
        let mut inner = Emitter::new("inner".to_string(), 2, span()).unwrap();
        inner.emit(Asm::GetGlobal { slot: 0 }, span()).unwrap();
        inner.emit(Asm::Call { argc: 0 }, span()).unwrap();
        push(&mut inner, result.clone());
        inner.emit(Asm::Return, span()).unwrap();
        let inner = Arc::new(inner.finish(1).unwrap_or_else(|e| panic!("{e:?}")));
        let (probes, closures, log) = makers(inner);
        // The caller: `closures()(probe(), probe())`, then a probe of
        // its own, and both in a tuple.
        let script = make_function(|e| {
            let probe = e.constant(probes.clone(), span()).unwrap();
            let closure = e.constant(closures, span()).unwrap();
            e.emit(Asm::Constant { k: probe }, span()).unwrap();
            e.emit(Asm::SetGlobal { slot: 0 }, span()).unwrap();
            e.emit(Asm::Pop, span()).unwrap();
            e.emit(Asm::Constant { k: closure }, span()).unwrap();
            e.emit(Asm::Call { argc: 0 }, span()).unwrap();
            for _ in 0..2 {
                e.emit(Asm::Constant { k: probe }, span()).unwrap();
                e.emit(Asm::Call { argc: 0 }, span()).unwrap();
            }
            e.emit(Asm::Call { argc: 2 }, span()).unwrap();
            e.emit(Asm::Constant { k: probe }, span()).unwrap();
            e.emit(Asm::Call { argc: 0 }, span()).unwrap();
            e.emit(Asm::MakeTuple { count: 2 }, span()).unwrap();
            e.emit(Asm::Return, span()).unwrap();
        });
        // (The script is called by an outer script, so that the inner
        // function returns to a function's frame that is not the
        // run's first.)
        let outer = make_function(|e| {
            push(
                e,
                Value::VmClosure(Arc::new(VmClosure {
                    function: script,
                    upvalues: vec![],
                })),
            );
            e.emit(Asm::Call { argc: 0 }, span()).unwrap();
            e.emit(Asm::Return, span()).unwrap();
        });
        let mut vm = Vm::new(crate::HostIo::process());
        vm.view.slots = Arc::from([std::sync::OnceLock::new()]);
        let Value::Tuple(pair) = vm.run(outer).unwrap() else {
            panic!("a tuple")
        };
        assert_eq!(pair[0], result, "the result");
        // The callee's probe, its two arguments, its local: all ended
        // at the return, in that order, before the caller made its own.
        assert_eq!(ended(&log), [1, 2, 3, 4], "returning {result:?}");
        drop(pair);
        assert_eq!(ended(&log), [1, 2, 3, 4, 5], "returning {result:?}");
    }

    /// A value of every kind that can be called is called by `Call`.
    #[test]
    fn a_call_of_every_kind_of_function() {
        let closure = Value::VmClosure(Arc::new(VmClosure {
            function: make_function_of(1, |e| {
                e.emit(Asm::GetLocal { slot: 0 }, span()).unwrap();
                e.emit(Asm::Return, span()).unwrap();
            }),
            upvalues: vec![],
        }));
        let host = Value::HostFn(Arc::new(HostFn {
            name: "same".to_string(),
            call: Arc::new(|args| Ok(args[0].clone())),
            returns: HostShape::Any,
        }));
        for callee in [closure, host] {
            for argument in results() {
                let script = make_function(|e| {
                    push(e, callee.clone());
                    push(e, argument.clone());
                    e.emit(Asm::Call { argc: 1 }, span()).unwrap();
                    e.emit(Asm::Return, span()).unwrap();
                });
                assert_eq!(
                    Vm::new(crate::HostIo::process()).run(script).unwrap(),
                    argument,
                    "{callee:?} of {argument:?}"
                );
            }
        }
    }

    /// The frames of a recursion 3,000 calls deep, which is past
    /// eleven doublings of the room for frames, each made at the push
    /// of a frame; and the values of the callers, which wait on the
    /// stack meanwhile, are what they were when the calls return.
    #[test]
    fn a_recursion_makes_room_for_its_frames() {
        for (base, step, expected) in [
            ("0", "1", "3000"),
            ("0.0", "0.5", "1500.0"),
            ("\"\"", "\".\"", "3000"),
        ] {
            let sum = if base.starts_with('"') {
                "\"{depth(n - 1)}{step()}\""
            } else {
                "step() + depth(n - 1)"
            };
            let shown = if base.starts_with('"') {
                "string.length(depth(3000))"
            } else {
                "depth(3000)"
            };
            let source = format!(
                "import string\nfn step() {{ {step} }}\nfn depth(n) {{\n  match n {{\n    0 -> {base}\n    _ -> {sum}\n  }}\n}}\nfn main() {{ \"{{{shown}}}\" }}"
            );
            assert_eq!(run_vm(&source), Value::String(expected.into()), "{source}");
        }
    }
}

/// A read of a part of a value (a record's field, a tuple's element,
/// what a variant holds, a map's value), `Dup`, and `?` on an `Ok` or a
/// `Some` copy the part where it goes: over the value on top, or above
/// it.
mod parts_in_place {
    use super::seen_drops::{ended, left_by, probe};
    use super::*;
    use crate::bytecode::{Const, VmClosure};
    use crate::value::{HostFn, HostShape};

    /// Emit the push of `value`.
    fn push(e: &mut Emitter, value: Value) {
        let k = e.constant(value, span()).unwrap();
        e.emit(Asm::Constant { k }, span()).unwrap();
    }

    fn name(e: &mut Emitter, s: &str) -> Const {
        e.constant(Value::String(s.into()), span()).unwrap()
    }

    /// A part of every sort the instruction loop knows (`Sort` in
    /// run.rs), and of several kinds that own something.
    fn samples() -> Vec<Value> {
        vec![
            Value::Int(-7),
            float(2.5),
            Value::Bool(true),
            Value::Bool(false),
            Value::Unit,
            Value::VmClosure(Arc::new(VmClosure {
                function: Arc::new(Function::returning_unit("f".to_string(), 0)),
                upvalues: vec![],
            })),
            Value::String("text".into()),
            Value::list(vec![Value::Int(1), Value::Unit]),
            Value::anon_record([("x", Value::Int(1))]),
            Value::variant(bv::SOME, vec![float(0.5)]),
        ]
    }

    /// Whether `a` is `b`: the same closure, or an equal value.
    fn same(a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::VmClosure(a), Value::VmClosure(b)) => Arc::ptr_eq(a, b),
            _ => a == b,
        }
    }

    /// The record `{ a: 5, b: part }`, and the place of `b` in it.
    fn record(part: &Value) -> (Value, usize) {
        let record = Value::anon_record([("a", Value::Int(5)), ("b", part.clone())]);
        let Value::Record(fields) = &record else {
            unreachable!()
        };
        let index = fields.named().position(|(name, _)| name == "b").unwrap();
        (record, index)
    }

    /// What the stack holds after the instruction `read` emits has run
    /// on `whole`, which lay above a value that must stay.
    fn read(whole: Value, read: impl FnOnce(&mut Emitter)) -> Vec<Value> {
        let (vm, run, _) = left_by(|e, _| {
            push(e, Value::Int(99));
            push(e, whole);
            read(e);
            // (The loop runs until the script would return; the stack
            // is looked at where an error of a last instruction leaves
            // it.)
            e.emit(Asm::Unit, span()).unwrap();
            e.emit(Asm::Unit, span()).unwrap();
            e.emit(Asm::Add, span()).unwrap();
            e.emit(Asm::Return, span()).unwrap();
        });
        run.expect_err("unit and unit are not added");
        vm.stack.clone()
    }

    /// The part alone, over the whole: the stack is the value below and
    /// the part.
    fn over(part: &Value, whole: Value, emit: impl FnOnce(&mut Emitter)) {
        let stack = read(whole.clone(), emit);
        assert_eq!(stack.len(), 2, "{part:?} of {whole:?}: {stack:?}");
        assert_eq!(stack[0], Value::Int(99));
        assert!(same(&stack[1], part), "{:?} is not {part:?}", stack[1]);
    }

    /// The part above the whole, which stays.
    fn above(part: &Value, whole: Value, emit: impl FnOnce(&mut Emitter)) {
        let stack = read(whole.clone(), emit);
        assert_eq!(stack.len(), 3, "{part:?} of {whole:?}: {stack:?}");
        assert_eq!(stack[0], Value::Int(99));
        assert_eq!(format!("{:?}", stack[1]), format!("{whole:?}"));
        assert!(same(&stack[2], part), "{:?} is not {part:?}", stack[2]);
    }

    #[test]
    fn a_field_of_every_kind_is_written_over_its_record() {
        for part in samples() {
            let (whole, index) = record(&part);
            over(&part, whole.clone(), |e| {
                e.emit(Asm::GetField { index }, span()).unwrap();
            });
            over(&part, whole, |e| {
                let b = name(e, "b");
                e.emit(Asm::GetFieldNamed { name: b }, span()).unwrap();
            });
        }
    }

    #[test]
    fn what_an_ok_or_a_some_holds_is_written_over_it() {
        for part in samples() {
            for tag in [bv::OK, bv::SOME] {
                over(&part, Value::variant(tag, vec![part.clone()]), |e| {
                    e.emit(Asm::QuestionMark, span()).unwrap();
                });
            }
        }
    }

    #[test]
    fn a_pattern_reads_a_part_of_every_kind_above_the_value() {
        for part in samples() {
            let (whole, index) = record(&part);
            above(&part, whole.clone(), |e| {
                e.emit(Asm::DestructRecordField { index }, span()).unwrap();
            });
            above(&part, whole, |e| {
                let b = name(e, "b");
                e.emit(Asm::DestructRecordFieldNamed { name: b }, span())
                    .unwrap();
            });
            let tuple = Value::tuple(vec![Value::Int(5), part.clone()]);
            above(&part, tuple, |e| {
                e.emit(Asm::DestructTuple { index: 1 }, span()).unwrap();
            });
            let variant = Value::variant(bv::ERR, vec![part.clone()]);
            above(&part, variant, |e| {
                e.emit(Asm::DestructVariant { index: 0 }, span()).unwrap();
            });
            let map = Value::map([
                (Value::String("j".into()), Value::Int(5)),
                (Value::String("k".into()), part.clone()),
            ]);
            above(&part, map, |e| {
                let k = name(e, "k");
                e.emit(Asm::DestructMapValue { key: k }, span()).unwrap();
            });
            above(&part, part.clone(), |e| {
                e.emit(Asm::Dup, span()).unwrap();
            });
        }
    }

    /// A function the VM calls with values and that gives what `make`
    /// makes of copies of them: a value that holds them and that
    /// nothing else holds.
    fn holder(name: &str, make: fn(&[Value]) -> Value) -> Value {
        Value::HostFn(Arc::new(HostFn {
            name: name.to_string(),
            call: Arc::new(move |parts| Ok(make(parts))),
            returns: HostShape::Any,
        }))
    }

    /// Emit the making of a value that holds `count` new probes, by
    /// the function `holder`; `k` is the maker of the probes.
    fn held(e: &mut Emitter, k: Const, holder: Value, count: usize) {
        push(e, holder);
        for _ in 0..count {
            probe(e, k);
        }
        e.emit(Asm::Call { argc: count }, span()).unwrap();
    }

    fn record_of(parts: &[Value]) -> Value {
        Value::anon_record([("a", parts[0].clone()), ("b", parts[1].clone())])
    }

    fn ok_of(parts: &[Value]) -> Value {
        Value::variant(bv::OK, parts.to_vec())
    }

    fn tuple_of(parts: &[Value]) -> Value {
        Value::tuple(parts.to_vec())
    }

    fn map_of(parts: &[Value]) -> Value {
        Value::map([
            (Value::String("j".into()), parts[0].clone()),
            (Value::String("k".into()), parts[1].clone()),
        ])
    }

    /// The probes that ended, in order, when the script has run: probe
    /// 1 lies below everything until the script returns; `whole` makes
    /// a value of the next probes; `read` reads a part of it; the two
    /// `Pop`s that follow take off what `read` left (a `Pop` of the
    /// unit pushed for it, where `read` left one value).
    fn ends(
        whole: impl FnOnce(&mut Emitter, Const),
        left: usize,
        read: Asm<'static>,
    ) -> Vec<usize> {
        let (vm, run, log) = left_by(|e, k| {
            probe(e, k);
            whole(e, k);
            e.emit(read, span()).unwrap();
            for _ in left..2 {
                e.emit(Asm::Unit, span()).unwrap();
            }
            e.emit(Asm::Pop, span()).unwrap();
            e.emit(Asm::Pop, span()).unwrap();
            e.emit(Asm::Pop, span()).unwrap();
            e.emit(Asm::Unit, span()).unwrap();
            e.emit(Asm::Return, span()).unwrap();
        });
        assert_eq!(run.unwrap(), Value::Unit);
        assert!(vm.stack.is_empty());
        ended(&log)
    }

    /// The record ends at the read of its field, with the field that
    /// was not read (3); the field that was read (2) lives on the
    /// stack until it is taken off.
    #[test]
    fn a_field_read_lets_go_of_the_record_and_keeps_the_field() {
        let whole = |e: &mut Emitter, k| held(e, k, holder("record", record_of), 2);
        let (_, a) = record(&Value::Unit);
        let a = if a == 0 { 1 } else { 0 };
        assert_eq!(ends(whole, 1, Asm::GetField { index: a }), [3, 2, 1]);
        let whole = |e: &mut Emitter, k| held(e, k, holder("record", record_of), 2);
        let (vm, run, log) = left_by(|e, k| {
            probe(e, k);
            whole(e, k);
            let b = name(e, "b");
            e.emit(Asm::GetFieldNamed { name: b }, span()).unwrap();
            e.emit(Asm::Pop, span()).unwrap();
            e.emit(Asm::Pop, span()).unwrap();
            e.emit(Asm::Unit, span()).unwrap();
            e.emit(Asm::Return, span()).unwrap();
        });
        assert_eq!(run.unwrap(), Value::Unit);
        assert!(vm.stack.is_empty());
        assert_eq!(ended(&log), [2, 3, 1]);
    }

    /// `?` lets go of the `Ok` and keeps what it held.
    #[test]
    fn a_question_mark_lets_go_of_the_ok_and_keeps_what_it_held() {
        let whole = |e: &mut Emitter, k| held(e, k, holder("ok", ok_of), 1);
        assert_eq!(ends(whole, 1, Asm::QuestionMark), [2, 1]);
    }

    /// A pattern's read and `Dup` keep the value: its parts end when
    /// it is taken off the stack, each once.
    #[test]
    fn a_pattern_read_holds_the_part_beside_the_value() {
        let (_, b) = record(&Value::Unit);
        let whole = |e: &mut Emitter, k| held(e, k, holder("record", record_of), 2);
        let read = Asm::DestructRecordField { index: b };
        assert_eq!(ends(whole, 2, read), [2, 3, 1]);
        let whole = |e: &mut Emitter, k| held(e, k, holder("tuple", tuple_of), 2);
        assert_eq!(ends(whole, 2, Asm::DestructTuple { index: 1 }), [2, 3, 1]);
        let whole = |e: &mut Emitter, k| held(e, k, holder("ok", ok_of), 1);
        let read = Asm::DestructVariant { index: 0 };
        assert_eq!(ends(whole, 2, read), [2, 1]);
        let whole = |e: &mut Emitter, k| held(e, k, holder("ok", ok_of), 1);
        assert_eq!(ends(whole, 2, Asm::Dup), [2, 1]);
        for named in ["record", "map"] {
            let (vm, run, log) = left_by(|e, k| {
                probe(e, k);
                let key = name(e, if named == "map" { "k" } else { "b" });
                if named == "map" {
                    held(e, k, holder("map", map_of), 2);
                    e.emit(Asm::DestructMapValue { key }, span()).unwrap();
                } else {
                    held(e, k, holder("record", record_of), 2);
                    e.emit(Asm::DestructRecordFieldNamed { name: key }, span())
                        .unwrap();
                }
                e.emit(Asm::Pop, span()).unwrap();
                e.emit(Asm::Pop, span()).unwrap();
                e.emit(Asm::Pop, span()).unwrap();
                e.emit(Asm::Unit, span()).unwrap();
                e.emit(Asm::Return, span()).unwrap();
            });
            assert_eq!(run.unwrap(), Value::Unit);
            assert!(vm.stack.is_empty());
            assert_eq!(ended(&log), [2, 3, 1], "{named}");
        }
    }

    /// A read that fails: a field read has taken the value off the
    /// stack, and it ends with the error; a pattern's read and `?`
    /// leave it where it was.
    #[test]
    fn a_read_that_fails_leaves_the_stack_as_it_was() {
        type Read = fn(&mut Emitter);
        let off: [Read; 2] = [
            |e| {
                e.emit(Asm::GetField { index: 0 }, span()).unwrap();
            },
            |e| {
                let b = name(e, "b");
                e.emit(Asm::GetFieldNamed { name: b }, span()).unwrap();
            },
        ];
        let kept: [Read; 6] = [
            |e| {
                e.emit(Asm::DestructRecordField { index: 0 }, span())
                    .unwrap();
            },
            |e| {
                let b = name(e, "b");
                e.emit(Asm::DestructRecordFieldNamed { name: b }, span())
                    .unwrap();
            },
            |e| {
                e.emit(Asm::DestructTuple { index: 0 }, span()).unwrap();
            },
            |e| {
                e.emit(Asm::DestructVariant { index: 0 }, span()).unwrap();
            },
            |e| {
                let k = name(e, "k");
                e.emit(Asm::DestructMapValue { key: k }, span()).unwrap();
            },
            |e| {
                e.emit(Asm::QuestionMark, span()).unwrap();
            },
        ];
        let failed = |read: Read| {
            // Probe 1 lies below; probe 2, a function, is what the
            // instruction is given: it is no record, tuple, variant or
            // map.
            let (vm, run, log) = left_by(|e, k| {
                probe(e, k);
                probe(e, k);
                read(e);
                e.emit(Asm::Return, span()).unwrap();
            });
            assert!(run.unwrap_err().type_confusion);
            (vm.stack.len(), ended(&log))
        };
        for read in off {
            assert_eq!(failed(read), (1, vec![2]));
        }
        for read in kept {
            assert_eq!(failed(read), (2, vec![]));
        }
    }
}

/// A call of a method that is found where the code runs (`CallMethod`,
/// `TailCallMethod`): the method is entered by its closure's pointer,
/// and the slot its frame starts above is made where it goes.
mod methods_in_place {
    use std::sync::OnceLock;

    use super::seen_drops::{ended, maker, probe};
    use super::*;
    use crate::bytecode::{Const, VmClosure};
    use crate::value::{HostFn, HostShape};

    /// Emit the push of `value`.
    fn push(e: &mut Emitter, value: Value) {
        let k = e.constant(value, span()).unwrap();
        e.emit(Asm::Constant { k }, span()).unwrap();
    }

    fn closure(function: Arc<Function>) -> Value {
        Value::VmClosure(Arc::new(VmClosure {
            function,
            upvalues: vec![],
        }))
    }

    /// A function that gives the list of what it was called with.
    fn lister() -> Value {
        Value::HostFn(Arc::new(HostFn {
            name: "lister".to_string(),
            call: Arc::new(|args| Ok(Value::list(args.to_vec()))),
            returns: HostShape::Any,
        }))
    }

    /// A method of `arity` parameters that gives the list of them.
    fn listing(arity: u8) -> Value {
        closure(make_function_of(arity, |e| {
            for slot in 0..usize::from(arity) {
                e.emit(Asm::GetLocal { slot }, span()).unwrap();
            }
            let count = usize::from(arity);
            e.emit(Asm::MakeList { count }, span()).unwrap();
            e.emit(Asm::Return, span()).unwrap();
        }))
    }

    /// A VM whose program has one impl: of a trait for the type of
    /// `receiver`, with the one method `method`. The call's operands
    /// (the method's name, the trait) are 0 and 0.
    fn vm_with(receiver: &Value, method: Value) -> Vm {
        let mut globals = crate::bytecode::Globals::default();
        let t = crate::defs::builtin_trait_id("Display").unwrap();
        let ty = crate::types::canonical::dispatch_type_for_value(receiver);
        assert_eq!(globals.trait_index(t, "T".to_string()), Some(0));
        let slot = globals.add_method(t, ty, "m", "T.m".to_string()).unwrap();
        assert_eq!(globals.method_id("m"), Some(0));
        let table = Arc::new(globals);
        let mut vm = Vm::new(crate::HostIo::process());
        vm.view.slots = std::iter::repeat_with(OnceLock::new)
            .take(table.len())
            .collect();
        vm.view.entry = table.clone();
        vm.view.table = table;
        assert!(vm.set_global(slot, method));
        vm
    }

    /// Run the script `build` emits on `vm` until it ends or fails,
    /// and give the VM as the instruction loop left it (nothing is cut
    /// back or unwound yet) and what the loop gave.
    fn left(mut vm: Vm, build: impl FnOnce(&mut Emitter)) -> (Vm, Result<Value, VmError>) {
        let script = make_function(build);
        let floor = vm.frames.len();
        vm.frames.push(Frame::Code(CallFrame {
            closure: Arc::new(VmClosure {
                function: script,
                upvalues: vec![],
            }),
            ip: 0,
            base_slot: 0,
        }));
        let run = vm.run_thread(floor, |vm| vm.run_frames(floor, usize::MAX));
        let run = run.map(|slice| match slice {
            crate::vm::run::Slice::Done(value) => value,
            _ => panic!("the script runs to its end or to an error"),
        });
        (vm, run)
    }

    fn call(argc: u8, tail: bool) -> Asm<'static> {
        let (method, argc, of, at) = (0, usize::from(argc), 0, 0);
        match tail {
            true => Asm::TailCallMethod {
                method,
                argc,
                of,
                at,
            },
            false => Asm::CallMethod {
                method,
                argc,
                of,
                at,
            },
        }
    }

    /// The values of the list `value`.
    fn items(value: &Value) -> Vec<Value> {
        let Value::List(list) = value else {
            panic!("a list, not {value:?}")
        };
        list.iter().collect()
    }

    /// Whether `a` is `b`: the same closure, or an equal value.
    fn same(a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::VmClosure(a), Value::VmClosure(b)) => Arc::ptr_eq(a, b),
            _ => a == b,
        }
    }

    /// A receiver or an argument of every sort the instruction loop
    /// knows (`Sort` in run.rs).
    fn samples() -> Vec<Value> {
        vec![
            Value::Int(-7),
            float(2.5),
            Value::Bool(true),
            Value::Unit,
            closure(Arc::new(Function::returning_unit("f".to_string(), 0))),
            Value::String("text".into()),
            Value::list(vec![Value::Int(1), Value::Unit]),
            Value::anon_record([("x", Value::Int(1))]),
        ]
    }

    /// The method gets the receiver and the arguments, in order, and
    /// its result lies where the receiver lay; the value below is
    /// untouched. In tail position its result is the caller's.
    #[test]
    fn a_method_gets_its_receiver_and_its_arguments() {
        for method in [listing as fn(u8) -> Value, |_| lister()] {
            for receiver in samples() {
                for argument in samples() {
                    for extra in [0u8, 1, 3] {
                        for tail in [false, true] {
                            let vm = vm_with(&receiver, method(1 + extra));
                            let (vm, run) = left(vm, |e| {
                                push(e, Value::Int(99));
                                push(e, receiver.clone());
                                for _ in 0..extra {
                                    push(e, argument.clone());
                                }
                                e.emit(call(1 + extra, tail), span()).unwrap();
                                if !tail {
                                    e.emit(Asm::MakeList { count: 2 }, span()).unwrap();
                                }
                                e.emit(Asm::Return, span()).unwrap();
                            });
                            let result = run.unwrap();
                            let got = match tail {
                                true => result,
                                false => {
                                    let both = items(&result);
                                    assert_eq!(both[0], Value::Int(99));
                                    both[1].clone()
                                }
                            };
                            let got = items(&got);
                            assert_eq!(got.len(), usize::from(1 + extra));
                            assert!(same(&got[0], &receiver), "{got:?}");
                            for given in &got[1..] {
                                assert!(same(given, &argument), "{got:?}");
                            }
                            assert!(vm.stack.is_empty());
                        }
                    }
                }
            }
        }
    }

    /// A type named as the receiver chooses the method and is no
    /// argument of it.
    #[test]
    fn a_type_as_the_receiver_is_not_an_argument() {
        for tail in [false, true] {
            let receiver = Value::PrimitiveDescriptor("Int");
            let vm = vm_with(&receiver, listing(2));
            let (vm, run) = left(vm, |e| {
                push(e, Value::Int(99));
                push(e, receiver.clone());
                push(e, Value::Int(1));
                push(e, Value::String("two".into()));
                e.emit(call(3, tail), span()).unwrap();
                if !tail {
                    e.emit(Asm::MakeList { count: 2 }, span()).unwrap();
                }
                e.emit(Asm::Return, span()).unwrap();
            });
            let result = run.unwrap();
            let given = Value::list(vec![Value::Int(1), Value::String("two".into())]);
            match tail {
                true => assert_eq!(result, given),
                false => assert_eq!(result, Value::list(vec![Value::Int(99), given])),
            }
            assert!(vm.stack.is_empty());
        }
    }

    /// The slot a method's frame starts above is one more value on the
    /// stack: with every number of values below, the stack's room is
    /// used up exactly at that slot several times.
    #[test]
    fn a_method_call_makes_room_for_its_slot() {
        for below in 0..70usize {
            let receiver = Value::String("r".into());
            let vm = vm_with(&receiver, listing(1));
            let (vm, run) = left(vm, |e| {
                for n in 0..below {
                    push(e, Value::Int(n as i64));
                }
                push(e, receiver.clone());
                e.emit(call(1, false), span()).unwrap();
                e.emit(Asm::MakeList { count: below + 1 }, span()).unwrap();
                e.emit(Asm::Return, span()).unwrap();
            });
            let mut expected: Vec<Value> = (0..below).map(|n| Value::Int(n as i64)).collect();
            expected.push(Value::list(vec![receiver]));
            assert_eq!(run.unwrap(), Value::list(expected), "{below} below");
            assert!(vm.stack.is_empty());
        }
    }

    /// Run `build` on a VM whose method, of `arity` parameters, gives
    /// unit, for a receiver that is a probe; `build` gets the maker of
    /// probes. The VM as the loop left it, what the loop gave, and the
    /// probes that ended.
    fn with_probes(
        arity: u8,
        build: impl FnOnce(&mut Emitter, Const),
    ) -> (Vm, Result<Value, VmError>, Vec<usize>) {
        let (maker, log) = maker();
        let method = closure(Arc::new(Function::returning_unit("m".to_string(), arity)));
        let vm = vm_with(&lister(), method);
        let (vm, run) = left(vm, |e| {
            let k = e.constant(maker, span()).unwrap();
            build(e, k);
        });
        let ended = ended(&log);
        (vm, run, ended)
    }

    /// The receiver and the arguments are the method's: they end when
    /// it returns, the receiver first; the value below ends when the
    /// caller is done.
    #[test]
    fn a_method_call_ends_what_it_was_given_at_the_return() {
        let (vm, run, ended) = with_probes(3, |e, k| {
            probe(e, k);
            probe(e, k);
            probe(e, k);
            probe(e, k);
            e.emit(call(3, false), span()).unwrap();
            e.emit(Asm::Pop, span()).unwrap();
            e.emit(Asm::Unit, span()).unwrap();
            e.emit(Asm::Return, span()).unwrap();
        });
        assert_eq!(run.unwrap(), Value::Unit);
        assert_eq!(ended, [2, 3, 4, 1]);
        assert!(vm.stack.is_empty());
    }

    /// A call with the wrong number of arguments (which no checked
    /// program makes) fails with the slot of the method's frame made
    /// and nothing let go of; in tail position, with the stack as it
    /// was.
    #[test]
    fn a_method_call_that_fails_leaves_the_stack_as_it_was() {
        for (tail, left) in [(false, 4), (true, 3)] {
            let (vm, run, ended) = with_probes(3, |e, k| {
                probe(e, k);
                probe(e, k);
                probe(e, k);
                e.emit(call(2, tail), span()).unwrap();
                e.emit(Asm::Return, span()).unwrap();
            });
            let err = run.unwrap_err();
            assert!(err.type_confusion);
            assert_eq!(err.message, "function 'm' expects 3 arguments, got 2");
            assert_eq!(vm.stack.len(), left, "tail: {tail}");
            assert_eq!(ended, [0; 0]);
            if !tail {
                assert_eq!(vm.stack[1], Value::Unit);
            }
        }
    }

    /// A method that is no closure and cannot be called: the receiver
    /// and its slot are off the stack at the error.
    #[test]
    fn a_method_that_cannot_be_called_takes_the_receiver_off() {
        let receiver = Value::Int(7);
        let vm = vm_with(&receiver, Value::Int(3));
        let (vm, run) = left(vm, |e| {
            push(e, Value::Int(99));
            push(e, receiver.clone());
            e.emit(call(1, false), span()).unwrap();
            e.emit(Asm::Return, span()).unwrap();
        });
        let err = run.unwrap_err();
        assert!(err.type_confusion);
        assert_eq!(err.message, "cannot call value of type Int");
        assert_eq!(vm.stack, [Value::Int(99)]);
    }
}

/// `Negate` and `Not` write their result over their operand, and a
/// pattern's test pushes its Bool above the value it tests: each is
/// built where it goes.
mod results_in_place {
    use super::seen_drops::{ended, left_by, probe};
    use super::*;

    /// Emit the push of `value`.
    fn push(e: &mut Emitter, value: Value) {
        let k = e.constant(value, span()).unwrap();
        e.emit(Asm::Constant { k }, span()).unwrap();
    }

    fn string(s: &str) -> Value {
        Value::String(s.into())
    }

    /// The stack after `op` has run on `operand`, which lay above a
    /// value that must stay, or the error with the stack as the loop
    /// left it.
    fn one(operand: Value, op: Asm<'static>) -> (Vec<Value>, Option<VmError>) {
        let (vm, run, _) = left_by(|e, _| {
            push(e, Value::Int(99));
            push(e, operand);
            e.emit(op, span()).unwrap();
            e.emit(Asm::MakeList { count: 2 }, span()).unwrap();
            e.emit(Asm::Return, span()).unwrap();
        });
        match run {
            Ok(Value::List(list)) => (list.iter().collect(), None),
            Ok(other) => panic!("a list, not {other:?}"),
            Err(error) => (vm.stack.clone(), Some(error)),
        }
    }

    #[test]
    fn negate_and_not_give_their_result_where_the_operand_was() {
        let half = crate::value::Float::new(0.5).unwrap();
        let zero = crate::value::Float::new(0.0).unwrap();
        for (operand, op, result) in [
            (Value::Int(5), Asm::Negate, Value::Int(-5)),
            (Value::Int(0), Asm::Negate, Value::Int(0)),
            (Value::Int(i64::MIN + 1), Asm::Negate, Value::Int(i64::MAX)),
            (Value::Float(half), Asm::Negate, Value::Float(-half)),
            (Value::Float(-half), Asm::Negate, Value::Float(half)),
            (Value::Float(zero), Asm::Negate, Value::Float(-zero)),
            (Value::Bool(true), Asm::Not, Value::Bool(false)),
            (Value::Bool(false), Asm::Not, Value::Bool(true)),
        ] {
            let shown = format!("{operand:?}");
            let (stack, error) = one(operand, op);
            assert!(error.is_none(), "{shown}: {error:?}");
            assert_eq!(stack, [Value::Int(99), result], "{shown}");
        }
    }

    /// An operand that is refused is off the stack at the error, and
    /// the value below it is where it was.
    #[test]
    fn a_refused_operand_is_off_the_stack_at_the_error() {
        let (stack, error) = one(Value::Int(i64::MIN), Asm::Negate);
        let error = error.unwrap();
        assert_eq!(
            error.message,
            "integer overflow: negate -9223372036854775808"
        );
        assert!(!error.type_confusion);
        assert_eq!(stack, [Value::Int(99)]);
        for (operand, op, message) in [
            (string("a"), Asm::Negate, "cannot negate String"),
            (Value::Bool(true), Asm::Negate, "cannot negate Bool"),
            (Value::Unit, Asm::Negate, "cannot negate Unit"),
            (Value::Int(1), Asm::Not, "cannot apply 'not' to Int"),
            (float(0.5), Asm::Not, "cannot apply 'not' to Float"),
            (string("a"), Asm::Not, "cannot apply 'not' to String"),
        ] {
            let (stack, error) = one(operand, op);
            let error = error.unwrap();
            assert_eq!(error.message, message);
            assert!(error.type_confusion);
            assert_eq!(stack, [Value::Int(99)], "{message}");
        }
    }

    /// A refused operand that owns something ends with the error; the
    /// value below it lives on.
    #[test]
    fn a_refused_operand_ends_with_the_error() {
        for op in [Asm::Negate, Asm::Not] {
            let (vm, run, log) = left_by(|e, k| {
                probe(e, k);
                probe(e, k);
                e.emit(op, span()).unwrap();
                e.emit(Asm::Return, span()).unwrap();
            });
            assert!(run.unwrap_err().type_confusion);
            assert_eq!(ended(&log), [2]);
            assert_eq!(vm.stack.len(), 1);
        }
    }

    /// The nine tests of a pattern, each with a value it holds for and
    /// one it does not.
    fn tests(e: &mut Emitter) -> Vec<(Asm<'static>, Value, Value)> {
        let mut constant = |value: Value| e.constant(value, span()).unwrap();
        let some = Value::variant(bv::SOME, vec![Value::Int(1)]);
        let record = Value::anon_record([("x", Value::Int(1))]);
        let Value::Record(fields) = &record else {
            unreachable!()
        };
        let anon = Value::TypeDescriptor(fields.ty().clone());
        let pair = Value::tuple(vec![Value::Int(1), Value::Int(2)]);
        let triple = Value::list(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
        let map = Value::map([(string("k"), Value::Int(1))]);
        vec![
            (
                Asm::TestTag {
                    tag: constant(Value::VariantConstructor(bv::SOME.tag())),
                },
                some.clone(),
                Value::variant(bv::NONE, vec![]),
            ),
            (
                Asm::TestEqual {
                    k: constant(string("a")),
                },
                string("a"),
                string("b"),
            ),
            (Asm::TestTupleLen { len: 2 }, pair.clone(), Value::Int(1)),
            (
                Asm::TestListMin { len: 3 },
                triple.clone(),
                Value::list(vec![]),
            ),
            (Asm::TestListExact { len: 3 }, triple, Value::list(vec![])),
            (
                Asm::TestIntRange {
                    lo: constant(Value::Int(1)),
                    hi: constant(Value::Int(5)),
                },
                Value::Int(5),
                Value::Int(6),
            ),
            (
                Asm::TestFloatRange {
                    lo: constant(float(0.5)),
                    hi: constant(float(1.5)),
                },
                float(1.0),
                float(2.0),
            ),
            (Asm::TestRecordTag { ty: constant(anon) }, record, some),
            (
                Asm::TestMapHasKey {
                    key: constant(string("k")),
                },
                map,
                Value::map([]),
            ),
        ]
    }

    /// Each test pushes `true` or `false` above the value, which stays;
    /// with every number of values below, so that the stack's room is
    /// used up exactly at the test's push several times.
    #[test]
    fn a_pattern_test_pushes_its_bool_above_the_value() {
        for which in 0..9 {
            for holds in [true, false] {
                for below in 0..70usize {
                    let mut tested = Value::Unit;
                    let (vm, run, _) = left_by(|e, _| {
                        let (test, yes, no) = tests(e).swap_remove(which);
                        tested = if holds { yes } else { no };
                        for n in 0..below {
                            push(e, Value::Int(n as i64));
                        }
                        push(e, tested.clone());
                        e.emit(test, span()).unwrap();
                        e.emit(Asm::MakeList { count: below + 2 }, span()).unwrap();
                        e.emit(Asm::Return, span()).unwrap();
                    });
                    let mut expected: Vec<Value> =
                        (0..below).map(|n| Value::Int(n as i64)).collect();
                    expected.push(tested);
                    expected.push(Value::Bool(holds));
                    assert_eq!(
                        run.unwrap(),
                        Value::list(expected),
                        "test {which}, {holds}, {below} below"
                    );
                    assert!(vm.stack.is_empty());
                }
            }
        }
    }
}

/// `Slide` keeps the value on top and cuts the frame back below it:
/// the value is written where it goes, and the values that go end from
/// the lowest up.
mod slide_in_place {
    use super::seen_drops::{ended, left_by, probe};
    use super::*;
    use crate::bytecode::VmClosure;

    /// Emit the push of `value`.
    fn push(e: &mut Emitter, value: Value) {
        let k = e.constant(value, span()).unwrap();
        e.emit(Asm::Constant { k }, span()).unwrap();
    }

    /// A value of every sort the instruction loop knows (`Sort` in
    /// run.rs).
    fn samples() -> Vec<Value> {
        vec![
            Value::Int(-7),
            float(2.5),
            Value::Bool(true),
            Value::Unit,
            Value::VmClosure(Arc::new(VmClosure {
                function: Arc::new(Function::returning_unit("f".to_string(), 0)),
                upvalues: vec![],
            })),
            Value::String("text".into()),
            Value::list(vec![Value::Int(1), Value::Unit]),
        ]
    }

    /// Whether `a` is `b`: the same closure, or an equal value.
    fn same(a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::VmClosure(a), Value::VmClosure(b)) => Arc::ptr_eq(a, b),
            _ => a == b,
        }
    }

    /// A value of every sort slides over values of every sort, over
    /// none, one or three of them; what lies below stays.
    #[test]
    fn a_value_of_every_kind_slides_over_every_kind() {
        for top in samples() {
            for under in samples() {
                for cut in [0, 1, 3] {
                    let (vm, run, _) = left_by(|e, _| {
                        push(e, Value::Int(99));
                        for _ in 0..cut {
                            push(e, under.clone());
                        }
                        push(e, top.clone());
                        e.emit(Asm::Slide { slot: 1 }, span()).unwrap();
                        // (The stack is looked at where an error of
                        // the next instruction leaves it.)
                        e.emit(Asm::Unit, span()).unwrap();
                        e.emit(Asm::Negate, span()).unwrap();
                        e.emit(Asm::Return, span()).unwrap();
                    });
                    run.expect_err("unit has no negative");
                    let shown = format!("{top:?} over {cut} of {under:?}");
                    assert_eq!(vm.stack.len(), 2, "{shown}: {:?}", vm.stack);
                    assert_eq!(vm.stack[0], Value::Int(99), "{shown}");
                    assert!(same(&vm.stack[1], &top), "{shown}: {:?}", vm.stack);
                }
            }
        }
    }

    /// Probe 1 lies below and stays; probes 2, 3 and 4 are slid over
    /// and end in that order, at the `Slide`; the value on top, probe 5
    /// or a number, lives until it is taken off.
    #[test]
    fn a_slide_ends_the_values_under_the_top_from_the_lowest_up() {
        for owns in [true, false] {
            let (vm, run, log) = left_by(|e, k| {
                for _ in 0..4 {
                    probe(e, k);
                }
                match owns {
                    true => probe(e, k),
                    false => push(e, Value::Int(7)),
                }
                e.emit(Asm::Slide { slot: 1 }, span()).unwrap();
                // A sixth probe, made after the slide: it ends before
                // the value that slid.
                probe(e, k);
                e.emit(Asm::Pop, span()).unwrap();
                e.emit(Asm::Pop, span()).unwrap();
                e.emit(Asm::Unit, span()).unwrap();
                e.emit(Asm::Return, span()).unwrap();
            });
            assert_eq!(run.unwrap(), Value::Unit);
            let (after, top): (usize, &[usize]) = if owns { (6, &[5]) } else { (5, &[]) };
            let mut expected = vec![2, 3, 4, after];
            expected.extend_from_slice(top);
            expected.push(1);
            assert_eq!(ended(&log), expected, "the top owns something: {owns}");
            assert!(vm.stack.is_empty());
        }
    }
}

/// String interpolation: a literal segment is read where it is, in the
/// constants of the code, and the values come off the stack in order.
mod interpolation {
    use std::sync::Mutex;

    use super::*;
    use crate::value::{HostFn, HostShape};

    fn text(value: Value) -> String {
        match value {
            Value::String(s) => s.to_string(),
            other => panic!("a String, not {other:?}"),
        }
    }

    /// Run the emitted script, which leaves a String.
    fn run_script(build: impl FnOnce(&mut Emitter)) -> String {
        let script = make_function(|e| {
            build(e);
            e.emit(Asm::Return, span()).unwrap();
        });
        text(Vm::new(crate::HostIo::process()).run(script).unwrap())
    }

    #[test]
    fn literals_first_last_between_and_none() {
        let result = run_vm(
            r#"
                fn main() {
                    let x = 7
                    let y = "b"
                    let first = "v{x}"
                    let last = "{x}v"
                    let alone = "{x}"
                    let between = "a{x}b{y}c"
                    let adjacent = "{x}{y}{x}"
                    let unicode = "é{y}✓{x}ß"
                    "{first}|{last}|{alone}|{between}|{adjacent}|{unicode}"
                }
            "#,
        );
        assert_eq!(text(result), "v7|7v|7|a7bbc|7b7|éb✓7ß");
    }

    /// The parser makes no interpolation of literals only, nor an empty
    /// literal segment; the instruction takes them all the same.
    #[test]
    fn literals_only_and_no_segment() {
        let result = run_script(|e| {
            let a = e.constant(Value::String("ab".into()), span()).unwrap();
            let empty = e.constant(Value::String("".into()), span()).unwrap();
            let c = e.constant(Value::String("c".into()), span()).unwrap();
            let segments = [
                Segment::Literal(a),
                Segment::Literal(empty),
                Segment::Literal(c),
                Segment::Literal(a),
            ];
            e.emit(
                Asm::Interpolate {
                    segments: &segments,
                },
                span(),
            )
            .unwrap();
        });
        assert_eq!(result, "abcab");
        // A literal at the last constant the pool holds: both bytes of
        // its index are read.
        let result = run_script(|e| {
            for n in 0..65535 {
                e.constant(Value::Int(n), span()).unwrap();
            }
            let last = e.constant(Value::String("last".into()), span()).unwrap();
            assert_eq!(last.index(), 65535);
            let segments = [Segment::Literal(last), Segment::Literal(last)];
            e.emit(
                Asm::Interpolate {
                    segments: &segments,
                },
                span(),
            )
            .unwrap();
        });
        assert_eq!(result, "lastlast");
        let result = run_script(|e| {
            e.emit(Asm::Interpolate { segments: &[] }, span()).unwrap();
        });
        assert_eq!(result, "");
    }

    /// The values come off the stack in order, below what the
    /// instruction does not take.
    #[test]
    fn values_come_off_the_stack_in_order() {
        let script = make_function(|e| {
            let s = |e: &mut Emitter, text: &str| {
                let k = e.constant(Value::String(text.into()), span()).unwrap();
                e.emit(Asm::Constant { k }, span()).unwrap();
                k
            };
            s(e, "under");
            s(e, "1");
            let dash = s(e, "-");
            s(e, "2");
            let segments = [
                Segment::Value,
                Segment::Literal(dash),
                Segment::Value,
                Segment::Value,
            ];
            e.emit(
                Asm::Interpolate {
                    segments: &segments,
                },
                span(),
            )
            .unwrap();
            e.emit(Asm::MakeTuple { count: 2 }, span()).unwrap();
            e.emit(Asm::Return, span()).unwrap();
        });
        let result = Vm::new(crate::HostIo::process()).run(script).unwrap();
        let Value::Tuple(pair) = result else {
            panic!("a tuple, not {result:?}")
        };
        assert_eq!(text(pair[0].clone()), "under");
        assert_eq!(text(pair[1].clone()), "1--2");
    }

    /// 255 segments, the most an interpolation has (one more is a
    /// compile error, tests/golden/lang/limits/round92_*).
    #[test]
    fn the_most_segments_an_interpolation_has() {
        let mut interpolation = String::from("{x}");
        let mut expected = String::from("7");
        for k in 0..127 {
            interpolation.push_str(&format!("<{k}>{{x}}"));
            expected.push_str(&format!("<{k}>7"));
        }
        let source = format!("fn main() {{\n  let x = 7\n  \"{interpolation}\"\n}}\n");
        assert_eq!(text(run_vm(&source)), expected);
    }

    /// A Display impl written in silt runs as a frame between the
    /// segments; the values and literals around it are not disturbed.
    #[test]
    fn a_display_impl_runs_between_the_segments() {
        let result = run_vm(
            r#"
                type Celsius {
                    c: Float,
                }

                trait Display for Celsius {
                    fn display(self) -> String {
                        "{self.c}°C"
                    }
                }

                fn main() {
                    let t = Celsius { c: 20.5 }
                    let n = 3
                    "t={t}, n={n}, again {t}!"
                }
            "#,
        );
        assert_eq!(text(result), "t=20.5°C, n=3, again 20.5°C!");
    }

    #[test]
    fn interpolations_in_tasks() {
        let result = run_vm(
            r#"
                import list
                import task

                fn main() {
                    let tasks = list.map([1, 2, 3, 4]) { i ->
                        task.spawn({ ->
                            list.fold(1..50, "") { acc, k -> "{acc}v{k * i};" }
                        })
                    }
                    let results = list.map(tasks) { t -> task.join(t) }
                    list.fold(results, "") { acc, r -> "{acc}[{r}]" }
                }
            "#,
        );
        let mut expected = String::new();
        for i in 1..=4 {
            expected.push('[');
            for k in 1..=50 {
                expected.push_str(&format!("v{};", k * i));
            }
            expected.push(']');
        }
        assert_eq!(text(result), expected);
    }

    /// An interpolation reads its literals where they are: the count of
    /// a literal's `Arc<str>`, which every task running the code would
    /// share, is not touched, not even while a value of the
    /// interpolation is being made, or after.
    #[test]
    fn an_interpolation_does_not_count_its_literals() {
        let literal: Arc<str> = "spin".into();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let probe = {
            let (literal, seen) = (literal.clone(), seen.clone());
            Value::HostFn(Arc::new(HostFn {
                name: "probe".to_string(),
                call: Arc::new(move |_| {
                    seen.lock().unwrap().push(Arc::strong_count(&literal));
                    Ok(Value::String("x".into()))
                }),
                returns: HostShape::Any,
            }))
        };
        let script = make_function(|e| {
            let k = e.constant(Value::String(literal.clone()), span()).unwrap();
            let p = e.constant(probe, span()).unwrap();
            e.emit(Asm::Constant { k: p }, span()).unwrap();
            e.emit(Asm::Call { argc: 0 }, span()).unwrap();
            let segments = [Segment::Literal(k), Segment::Value, Segment::Literal(k)];
            e.emit(
                Asm::Interpolate {
                    segments: &segments,
                },
                span(),
            )
            .unwrap();
            e.emit(Asm::Constant { k: p }, span()).unwrap();
            e.emit(Asm::Call { argc: 0 }, span()).unwrap();
            e.emit(Asm::Pop, span()).unwrap();
            e.emit(Asm::Return, span()).unwrap();
        });
        let before = Arc::strong_count(&literal);
        let result = Vm::new(crate::HostIo::process()).run(script).unwrap();
        assert_eq!(text(result), "spinxspin");
        // While the value was made, and after the instruction ran.
        assert_eq!(*seen.lock().unwrap(), [before, before]);
    }
}

/// An instruction that fuses two costs the steps of the two, and a
/// slice ends between its parts where it ended between the two.
mod fused_steps {
    use super::*;
    use crate::bytecode::{Op, VmClosure};
    use crate::vm::run::Slice;
    use crate::vm::runtime::CallFrame;

    /// Where `fuse` is false, a label between two instructions keeps
    /// them apart.
    fn apart(e: &mut Emitter, fuse: bool) {
        if !fuse {
            let between = e.label();
            e.bind(between, span()).unwrap();
        }
    }

    /// `loop i = 0 { match i <cmp> 5 { true -> loop(i + 1) false -> i } }`
    /// for `<` or `!=`, with its pairs fused or kept apart.
    fn count_to_five(compare: Asm<'static>, fuse: bool) -> Arc<Function> {
        make_function(|e| {
            let zero = e.constant(Value::Int(0), span()).unwrap();
            let five = e.constant(Value::Int(5), span()).unwrap();
            let one = e.constant(Value::Int(1), span()).unwrap();
            let (start, end) = (e.label(), e.label());
            e.emit(Asm::Constant { k: zero }, span()).unwrap();
            e.bind(start, span()).unwrap();
            e.emit(Asm::GetLocal { slot: 0 }, span()).unwrap();
            e.emit(Asm::Constant { k: five }, span()).unwrap();
            e.emit(compare, span()).unwrap();
            apart(e, fuse);
            e.emit(Asm::JumpIfFalse { to: end }, span()).unwrap();
            e.emit(Asm::GetLocal { slot: 0 }, span()).unwrap();
            e.emit(Asm::Constant { k: one }, span()).unwrap();
            apart(e, fuse);
            e.emit(Asm::Add, span()).unwrap();
            // (`Recur` is always the two, `Recur` and the jump back:
            // the count at slice 1 below holds it to two steps.)
            e.emit(
                Asm::Recur {
                    argc: 1,
                    first: 0,
                    to: start,
                },
                span(),
            )
            .unwrap();
            e.bind(end, span()).unwrap();
            e.emit(Asm::GetLocal { slot: 0 }, span()).unwrap();
            e.emit(Asm::Return, span()).unwrap();
        })
    }

    /// The ops of `script`.
    fn ops(script: &Function) -> Vec<Op> {
        script
            .chunk()
            .instrs()
            .map(|(_, instr)| instr.op())
            .collect()
    }

    /// Run `script` in slices of `slice` steps: what it ends with, and
    /// how many slices ran out of steps.
    fn sliced(script: Arc<Function>, slice: usize) -> (Result<Value, String>, usize) {
        let mut vm = Vm::new(crate::HostIo::process());
        vm.frames.push(Frame::Code(CallFrame {
            closure: Arc::new(VmClosure {
                function: script,
                upvalues: vec![],
            }),
            ip: 0,
            base_slot: 0,
        }));
        let mut slices = 0;
        loop {
            match vm.run_frames(0, slice) {
                Ok(Slice::OutOfBudget) => slices += 1,
                Ok(Slice::Done(value)) => return (Ok(value), slices),
                Ok(_) => panic!("the script neither waits nor gives way"),
                Err(error) => return (Err(error.message), slices),
            }
        }
    }

    #[test]
    fn a_fused_instruction_costs_the_steps_of_the_two_it_fuses() {
        for compare in [Asm::Lt, Asm::Neq] {
            let fused = count_to_five(compare, true);
            let apart = count_to_five(compare, false);
            assert!(ops(&fused).contains(&Op::AddConst), "{:?}", ops(&fused));
            assert!(!ops(&apart).contains(&Op::AddConst), "{:?}", ops(&apart));
            for slice in [1, 2, 3, 7, 1000] {
                let (fused_end, fused_slices) = sliced(fused.clone(), slice);
                let (apart_end, apart_slices) = sliced(apart.clone(), slice);
                assert_eq!(fused_end, Ok(Value::Int(5)), "{compare:?} at slice {slice}");
                assert_eq!(apart_end, fused_end);
                assert_eq!(fused_slices, apart_slices, "{compare:?} at slice {slice}");
            }
            // At slice 1 a slice is a step: the 52 steps of the two
            // instructions before the loop, five rounds of 9, the last
            // test of 4 and the 2 after it, all but the last running out.
            assert_eq!(sliced(fused, 1).1, 1 + 5 * 9 + 4 + 2 - 1);
        }
    }

    #[test]
    fn the_second_part_of_a_fused_instruction_fails_as_its_instruction() {
        let overflow = |fuse| {
            make_function(|e| {
                let most = e.constant(Value::Int(i64::MAX), span()).unwrap();
                let one = e.constant(Value::Int(1), span()).unwrap();
                e.emit(Asm::Constant { k: most }, span()).unwrap();
                e.emit(Asm::Constant { k: one }, span()).unwrap();
                apart(e, fuse);
                e.emit(Asm::Add, span()).unwrap();
                e.emit(Asm::Return, span()).unwrap();
            })
        };
        let message = format!("integer overflow: {} + 1", i64::MAX);
        for slice in [1, 2, 3] {
            assert_eq!(
                sliced(overflow(true), slice),
                (Err(message.clone()), slice_count(slice))
            );
            assert_eq!(sliced(overflow(false), slice).0, Err(message.clone()));
        }
    }

    /// The slices that run out before the `Add` of three steps fails in
    /// slices of `slice`.
    fn slice_count(slice: usize) -> usize {
        2 / slice
    }
}
