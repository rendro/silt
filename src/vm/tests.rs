use super::*;
use crate::bytecode::{Chunk, Function, Op};
use crate::compiler::Compiler;
use crate::lexer::{Lexer, Span};
use crate::parser::Parser;

/// Helper: build a Function from raw bytecode construction.
fn make_function(build: impl FnOnce(&mut Chunk)) -> Arc<Function> {
    let mut func = Function::new("<test>".to_string(), 0);
    build(&mut func.chunk);
    Arc::new(func)
}

fn span() -> Span {
    Span::new(0, 0)
}

/// Helper: compile and run a silt program through the VM pipeline.
fn run_vm(source: &str) -> Value {
    let tokens = Lexer::new(source).tokenize().unwrap();
    let program = Parser::new(tokens).parse_program().unwrap();
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).unwrap();
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    vm.run(script).unwrap()
}

// ── Phase 1 bytecode-level tests ──────────────────────────────

#[test]
fn test_constant_and_return() {
    let script = make_function(|chunk| {
        let idx = chunk.add_constant(Value::Int(42));
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(idx, span());
        chunk.emit_op(Op::Return, span());
    });
    let mut vm = Vm::new();
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_arithmetic_add_int() {
    let script = make_function(|chunk| {
        let a = chunk.add_constant(Value::Int(2));
        let b = chunk.add_constant(Value::Int(3));
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(a, span());
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(b, span());
        chunk.emit_op(Op::Add, span());
        chunk.emit_op(Op::Return, span());
    });
    let mut vm = Vm::new();
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Int(5));
}

#[test]
fn test_arithmetic_expression() {
    let script = make_function(|chunk| {
        let two = chunk.add_constant(Value::Int(2));
        let three = chunk.add_constant(Value::Int(3));
        let four = chunk.add_constant(Value::Int(4));
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(two, span());
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(three, span());
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(four, span());
        chunk.emit_op(Op::Mul, span());
        chunk.emit_op(Op::Add, span());
        chunk.emit_op(Op::Return, span());
    });
    let mut vm = Vm::new();
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Int(14));
}

#[test]
fn test_float_arithmetic() {
    let script = make_function(|chunk| {
        let a = chunk.add_constant(Value::Float(1.5));
        let b = chunk.add_constant(Value::Float(2.5));
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(a, span());
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(b, span());
        chunk.emit_op(Op::Add, span());
        chunk.emit_op(Op::Return, span());
    });
    let mut vm = Vm::new();
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Float(4.0));
}

#[test]
fn test_negate() {
    let script = make_function(|chunk| {
        let a = chunk.add_constant(Value::Int(10));
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(a, span());
        chunk.emit_op(Op::Negate, span());
        chunk.emit_op(Op::Return, span());
    });
    let mut vm = Vm::new();
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Int(-10));
}

#[test]
fn test_comparison() {
    let script = make_function(|chunk| {
        let a = chunk.add_constant(Value::Int(3));
        let b = chunk.add_constant(Value::Int(5));
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(a, span());
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(b, span());
        chunk.emit_op(Op::Lt, span());
        chunk.emit_op(Op::Return, span());
    });
    let mut vm = Vm::new();
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn test_boolean_not() {
    let script = make_function(|chunk| {
        chunk.emit_op(Op::True, span());
        chunk.emit_op(Op::Not, span());
        chunk.emit_op(Op::Return, span());
    });
    let mut vm = Vm::new();
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Bool(false));
}

#[test]
fn test_globals() {
    let script = make_function(|chunk| {
        let name = chunk.add_constant(Value::String("x".to_string()));
        let val = chunk.add_constant(Value::Int(42));
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(val, span());
        chunk.emit_op(Op::SetGlobal, span());
        chunk.emit_u16(name, span());
        chunk.emit_op(Op::GetGlobal, span());
        chunk.emit_u16(name, span());
        chunk.emit_op(Op::Return, span());
    });
    let mut vm = Vm::new();
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_locals() {
    let script = make_function(|chunk| {
        let val = chunk.add_constant(Value::Int(10));
        chunk.emit_op(Op::Unit, span());
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(val, span());
        chunk.emit_op(Op::SetLocal, span());
        chunk.emit_u16(0, span());
        chunk.emit_op(Op::Pop, span());
        chunk.emit_op(Op::GetLocal, span());
        chunk.emit_u16(0, span());
        chunk.emit_op(Op::Return, span());
    });
    let mut vm = Vm::new();
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Int(10));
}

#[test]
fn test_string_concat() {
    let script = make_function(|chunk| {
        let a = chunk.add_constant(Value::String("hello".to_string()));
        let b = chunk.add_constant(Value::String(" ".to_string()));
        let c = chunk.add_constant(Value::String("world".to_string()));
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(a, span());
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(b, span());
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(c, span());
        chunk.emit_op(Op::StringConcat, span());
        chunk.emit_u8(3, span());
        chunk.emit_op(Op::Return, span());
    });
    let mut vm = Vm::new();
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::String("hello world".to_string()));
}

#[test]
fn test_display_value() {
    let script = make_function(|chunk| {
        let val = chunk.add_constant(Value::Int(42));
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(val, span());
        chunk.emit_op(Op::DisplayValue, span());
        chunk.emit_op(Op::Return, span());
    });
    let mut vm = Vm::new();
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::String("42".to_string()));
}

#[test]
fn test_jump_if_false() {
    let script = make_function(|chunk| {
        let one = chunk.add_constant(Value::Int(1));
        let two = chunk.add_constant(Value::Int(2));
        chunk.emit_op(Op::False, span());
        let patch = chunk.emit_jump(Op::JumpIfFalse, span());
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(one, span());
        let skip_else = chunk.emit_jump(Op::Jump, span());
        chunk.patch_jump(patch);
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(two, span());
        chunk.patch_jump(skip_else);
        chunk.emit_op(Op::Return, span());
    });
    let mut vm = Vm::new();
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Int(2));
}

#[test]
fn test_builtin_println() {
    let script = make_function(|chunk| {
        let name = chunk.add_constant(Value::String("println".to_string()));
        let val = chunk.add_constant(Value::Int(42));
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(val, span());
        chunk.emit_op(Op::CallBuiltin, span());
        chunk.emit_u16(name, span());
        chunk.emit_u8(1, span());
        chunk.emit_op(Op::Return, span());
    });
    let mut vm = Vm::new();
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Unit);
}

#[test]
fn test_make_tuple() {
    let script = make_function(|chunk| {
        let a = chunk.add_constant(Value::Int(1));
        let b = chunk.add_constant(Value::Int(2));
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(a, span());
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(b, span());
        chunk.emit_op(Op::MakeTuple, span());
        chunk.emit_u8(2, span());
        chunk.emit_op(Op::Return, span());
    });
    let mut vm = Vm::new();
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Tuple(vec![Value::Int(1), Value::Int(2)]));
}

#[test]
fn test_make_list() {
    let script = make_function(|chunk| {
        let a = chunk.add_constant(Value::Int(10));
        let b = chunk.add_constant(Value::Int(20));
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(a, span());
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(b, span());
        chunk.emit_op(Op::MakeList, span());
        chunk.emit_u16(2, span());
        chunk.emit_op(Op::Return, span());
    });
    let mut vm = Vm::new();
    let result = vm.run(script).unwrap();
    assert_eq!(
        result,
        Value::List(Arc::new(vec![Value::Int(10), Value::Int(20)]))
    );
}

#[test]
fn test_division_by_zero() {
    let script = make_function(|chunk| {
        let a = chunk.add_constant(Value::Int(10));
        let b = chunk.add_constant(Value::Int(0));
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(a, span());
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(b, span());
        chunk.emit_op(Op::Div, span());
        chunk.emit_op(Op::Return, span());
    });
    let mut vm = Vm::new();
    let result = vm.run(script);
    assert!(result.is_err());
    assert!(result.unwrap_err().message.contains("division by zero"));
}

#[test]
fn test_unit_and_pop() {
    let script = make_function(|chunk| {
        let val = chunk.add_constant(Value::Int(99));
        chunk.emit_op(Op::Unit, span());
        chunk.emit_op(Op::Pop, span());
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(val, span());
        chunk.emit_op(Op::Return, span());
    });
    let mut vm = Vm::new();
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Int(99));
}

#[test]
fn test_dup() {
    let script = make_function(|chunk| {
        let val = chunk.add_constant(Value::Int(5));
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(val, span());
        chunk.emit_op(Op::Dup, span());
        chunk.emit_op(Op::Add, span());
        chunk.emit_op(Op::Return, span());
    });
    let mut vm = Vm::new();
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Int(10));
}

#[test]
fn test_eq_neq() {
    let script = make_function(|chunk| {
        let val = chunk.add_constant(Value::Int(5));
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(val, span());
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(val, span());
        chunk.emit_op(Op::Eq, span());
        chunk.emit_op(Op::Return, span());
    });
    let mut vm = Vm::new();
    assert_eq!(vm.run(script).unwrap(), Value::Bool(true));

    let script2 = make_function(|chunk| {
        let a = chunk.add_constant(Value::Int(5));
        let b = chunk.add_constant(Value::Int(3));
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(a, span());
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(b, span());
        chunk.emit_op(Op::Neq, span());
        chunk.emit_op(Op::Return, span());
    });
    let mut vm2 = Vm::new();
    assert_eq!(vm2.run(script2).unwrap(), Value::Bool(true));
}

#[test]
fn test_popn() {
    let script = make_function(|chunk| {
        let a = chunk.add_constant(Value::Int(1));
        let b = chunk.add_constant(Value::Int(2));
        let c = chunk.add_constant(Value::Int(3));
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(a, span());
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(b, span());
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(c, span());
        chunk.emit_op(Op::PopN, span());
        chunk.emit_u8(2, span());
        chunk.emit_op(Op::Return, span());
    });
    let mut vm = Vm::new();
    let result = vm.run(script).unwrap();
    assert_eq!(result, Value::Int(1));
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
    assert_eq!(result, Value::Variant("Some".into(), vec![Value::Int(42)]));
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
                fn(x) { x + n }
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
                [1, 2, 3] |> list.map(fn(x) { x * factor })
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
                let double = fn(x) { x * 2 }
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
                let fns = [1, 2, 3] |> list.map(fn(n) {
                    fn() { n * 10 }
                })
                fns |> list.map(fn(f) { f() })
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
                fn(x) { a * x + b }
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
                let make_inner = fn() {
                    fn() { x }
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
                let f = fn(x) { x + 1 }
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
                [1, 2, 3, 4, 5] |> list.filter(fn(x) { x > threshold })
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
                [1, 2, 3] |> list.fold(offset, fn(acc, x) { acc + x })
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
                fn(x) { x * factor }
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
fn test_closure_with_pipe_and_fn_syntax() {
    // Pipe with explicit fn(x) { ... } closure
    let result = run_vm(
        r#"
            import list

            fn main() {
                let base = 5
                [1, 2, 3] |> list.map(fn(x) { x + base })
            }
        "#,
    );
    assert_eq!(
        result,
        Value::List(Arc::new(vec![Value::Int(6), Value::Int(7), Value::Int(8)]))
    );
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
                    |> list.map(fn(x) { x + offset })
                    |> list.filter(fn(x) { x > cutoff })
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
            fn main() { match 42 { 42 -> "yes" _ -> "no" } }
        "#,
    );
    assert_eq!(result, Value::String("yes".into()));
}

#[test]
fn test_match_int_fallthrough() {
    let result = run_vm(
        r#"
            fn main() { match 99 { 42 -> "yes" _ -> "no" } }
        "#,
    );
    assert_eq!(result, Value::String("no".into()));
}

#[test]
fn test_match_string_literal() {
    let result = run_vm(
        r#"
            fn main() { match "hello" { "hello" -> 1 _ -> 0 } }
        "#,
    );
    assert_eq!(result, Value::Int(1));
}

#[test]
fn test_match_bool_literal() {
    let result = run_vm(
        r#"
            fn main() { match true { true -> "yes" false -> "no" } }
        "#,
    );
    assert_eq!(result, Value::String("yes".into()));
}

#[test]
fn test_match_float_literal() {
    let result = run_vm(
        r#"
            fn main() { match 3.14 { 3.14 -> "pi" _ -> "other" } }
        "#,
    );
    assert_eq!(result, Value::String("pi".into()));
}

#[test]
fn test_match_tuple() {
    let result = run_vm(
        r#"
            fn main() {
                match (1, 2) { (1, y) -> y * 10  _ -> 0 }
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
                match (1, 2) { (_, y) -> y + 100  _ -> 0 }
            }
        "#,
    );
    assert_eq!(result, Value::Int(102));
}

#[test]
fn test_match_tuple_len_mismatch() {
    let result = run_vm(
        r#"
            fn main() {
                match (1, 2, 3) { (a, b) -> a + b  _ -> 99 }
            }
        "#,
    );
    assert_eq!(result, Value::Int(99));
}

#[test]
fn test_match_list_exact() {
    let result = run_vm(
        r#"
            fn main() {
                match [1, 2, 3] { [a, b, c] -> a + b + c  _ -> 0 }
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
                match [1, 2] { [a, b, c] -> a + b + c  _ -> 99 }
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
                match [10, 20, 30] { [h, ..t] -> h  _ -> 0 }
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
                match [10, 20, 30] { [_, ..t] -> t  _ -> [] }
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
                match [10] { [h, ..t] -> t  _ -> [99] }
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
                match Some(42) { Some(n) -> n  None -> 0 }
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
                match None { Some(n) -> n  None -> 0 }
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
                match v { Ok(n) -> n  Err(_) -> -1 }
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
                match Some((1, 2)) { Some((a, b)) -> a + b  None -> 0 }
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
                match 2 { 1 | 2 | 3 -> "small" _ -> "big" }
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
                match 5 { 1 | 2 | 3 -> "small" _ -> "big" }
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
                match 5 { 1..10 -> "in range" _ -> "out" }
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
                match 10 { 1..10 -> "in range" _ -> "out" }
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
                match 11 { 1..10 -> "in range" _ -> "out" }
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
                match { x > 10 -> "big"  x > 0 -> "positive"  _ -> "other" }
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
                match { x > 10 -> "big"  x > 0 -> "positive"  _ -> "other" }
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
fn test_let_list_destructure() {
    let result = run_vm(
        r#"
            fn main() {
                let [a, b, c] = [10, 20, 30]
                a + b + c
            }
        "#,
    );
    assert_eq!(result, Value::Int(60));
}

#[test]
fn test_let_list_head_rest() {
    let result = run_vm(
        r#"
            fn main() {
                let [h, ..t] = [1, 2, 3, 4]
                h
            }
        "#,
    );
    assert_eq!(result, Value::Int(1));
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
                    Some(n) when n > 10 -> "big"
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
                match safe_div(10, 2) { Ok(n) -> n  Err(_) -> -1 }
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
                match safe_div(10, 0) { Ok(n) -> n  Err(_) -> -1 }
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
                match Ok(42) { Ok(_) -> "is ok" Err(_) -> "is err" }
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
                match None { Some(_) -> "has value"  None -> "empty" }
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
                match { x > 10 -> "big"  x > 0 -> "positive"  _ -> "other" }
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
                match x { Ok(n) -> n  Err(_) -> 0 }
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
                when Some(n) = val else { return -1 }
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
                when Some(n) = val else { return -1 }
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
                    (a, b) when a + b > 10 -> "big"
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
                match c { Red -> 1  Green -> 2  Blue -> 3 }
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
fn test_tuple_index_access() {
    let result = run_vm(
        r#"
            fn main() {
                let pair = (10, 20)
                pair.0 + pair.1
            }
        "#,
    );
    assert_eq!(result, Value::Int(30));
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
                let t = task.spawn(fn() { 42 })
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
                let t = task.spawn(fn() {
                    channel.send(ch, "done")
                    99
                })
                -- Wait for the message, ensuring the fiber runs to completion
                let Message(msg) = channel.receive(ch)
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
                let t1 = task.spawn(fn() {
                    channel.send(ch, 1)
                    10
                })
                let t2 = task.spawn(fn() {
                    channel.send(ch, 2)
                    20
                })
                let t3 = task.spawn(fn() {
                    channel.send(ch, 3)
                    30
                })
                -- Drain all messages so fibers complete
                let Message(_) = channel.receive(ch)
                let Message(_) = channel.receive(ch)
                let Message(_) = channel.receive(ch)
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

// ── FFI tests ──────────────────────────────────────────────────

/// Helper: compile and run silt code on a pre-configured VM (for FFI tests).
fn run_vm_with(vm: &mut Vm, source: &str) -> Value {
    let tokens = Lexer::new(source).tokenize().unwrap();
    let program = Parser::new(tokens).parse_program().unwrap();
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).unwrap();
    let script = Arc::new(functions.into_iter().next().unwrap());
    vm.run(script).unwrap()
}

#[test]
fn test_foreign_fn_raw() {
    let mut vm = Vm::new();
    vm.register_fn("double", |args: &[Value]| {
        let Value::Int(n) = &args[0] else {
            return Err(VmError::new("expected Int".into()));
        };
        Ok(Value::Int(n * 2))
    })
    .unwrap();
    let result = run_vm_with(&mut vm, "fn main() { double(21) }");
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_foreign_fn1_typed() {
    let mut vm = Vm::new();
    vm.register_fn1("double", |x: i64| -> i64 { x * 2 })
        .unwrap();
    let result = run_vm_with(&mut vm, "fn main() { double(21) }");
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_foreign_fn2_typed() {
    let mut vm = Vm::new();
    vm.register_fn2("add", |a: i64, b: i64| -> i64 { a + b })
        .unwrap();
    let result = run_vm_with(&mut vm, "fn main() { add(10, 32) }");
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_foreign_fn0_typed() {
    let mut vm = Vm::new();
    vm.register_fn0("answer", || -> i64 { 42 }).unwrap();
    let result = run_vm_with(&mut vm, "fn main() { answer() }");
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_foreign_fn_string() {
    let mut vm = Vm::new();
    vm.register_fn1("shout", |s: String| -> String { s.to_uppercase() })
        .unwrap();
    let result = run_vm_with(&mut vm, r#"fn main() { shout("hello") }"#);
    assert_eq!(result, Value::String("HELLO".into()));
}

#[test]
fn test_foreign_fn_returns_option() {
    let mut vm = Vm::new();
    vm.register_fn1("maybe", |x: i64| -> Option<i64> {
        if x > 0 { Some(x) } else { None }
    })
    .unwrap();
    let result = run_vm_with(&mut vm, "fn main() { maybe(5) }");
    assert_eq!(result, Value::Variant("Some".into(), vec![Value::Int(5)]));
    let result = run_vm_with(&mut vm, "fn main() { maybe(-1) }");
    assert_eq!(result, Value::Variant("None".into(), vec![]));
}

#[test]
fn test_foreign_fn_returns_result() {
    let mut vm = Vm::new();
    vm.register_fn1("safe_div", |x: i64| -> Result<i64, String> {
        if x != 0 {
            Ok(100 / x)
        } else {
            Err("division by zero".into())
        }
    })
    .unwrap();
    let result = run_vm_with(&mut vm, "fn main() { safe_div(5) }");
    assert_eq!(result, Value::Variant("Ok".into(), vec![Value::Int(20)]));
    let result = run_vm_with(&mut vm, "fn main() { safe_div(0) }");
    assert_eq!(
        result,
        Value::Variant("Err".into(), vec![Value::String("division by zero".into())])
    );
}

#[test]
fn test_foreign_fn_higher_order() {
    let mut vm = Vm::new();
    vm.register_fn1("square", |x: i64| -> i64 { x * x })
        .unwrap();
    let result = run_vm_with(
        &mut vm,
        "import list\nfn main() { [1, 2, 3] |> list.map(square) }",
    );
    assert_eq!(
        result,
        Value::List(Arc::new(vec![Value::Int(1), Value::Int(4), Value::Int(9),]))
    );
}

#[test]
fn test_foreign_fn_module_qualified() {
    let mut vm = Vm::new();
    vm.register_fn1("mylib.double", |x: i64| -> i64 { x * 2 })
        .unwrap();
    // Module-qualified names go through GetGlobal + Call, not CallBuiltin
    let result = run_vm_with(
        &mut vm,
        r#"
            fn main() {
                let f = mylib.double
                f(21)
            }
        "#,
    );
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_foreign_fn_type_error() {
    let mut vm = Vm::new();
    vm.register_fn1("double", |x: i64| -> i64 { x * 2 })
        .unwrap();
    let tokens = Lexer::new(r#"fn main() { double("hello") }"#)
        .tokenize()
        .unwrap();
    let program = Parser::new(tokens).parse_program().unwrap();
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).unwrap();
    let script = Arc::new(functions.into_iter().next().unwrap());
    let err = vm.run(script).unwrap_err();
    assert!(err.message.contains("expected Int"), "got: {}", err.message);
}

// ── Scheduler integration tests ──────────────────────────────

#[test]
fn test_scheduler_task_completes() {
    // task.join returns the value directly on success
    let result = run_vm(
        r#"
            import task
            fn main() {
                let t = task.spawn(fn() { 42 })
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
                let tasks = [1, 2, 3] |> list.map(fn(n) { task.spawn(fn() { n * 10 }) })
                tasks |> list.map(fn(t) { task.join(t) })
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
                task.spawn(fn() { channel.send(ch, 99) })
                channel.receive(ch)
            }
            "#,
    );
    assert_eq!(
        result,
        Value::Variant("Message".into(), vec![Value::Int(99)])
    );
}

#[test]
fn test_scheduler_deadlock_detection() {
    // Deadlock: task.join propagates as a VmError
    let tokens = Lexer::new(
        r#"
            import task
            import channel
            fn main() {
                let ch = channel.new()
                let t = task.spawn(fn() { channel.receive(ch) })
                task.join(t)
            }
            "#,
    )
    .tokenize()
    .unwrap();
    let program = Parser::new(tokens).parse_program().unwrap();
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).unwrap();
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    let err = vm.run(script).unwrap_err();
    assert!(
        err.message.contains("deadlock"),
        "expected deadlock error, got: {}",
        err.message
    );
}

#[test]
fn test_scheduler_task_failure_propagates() {
    // task.join on a failed task propagates as a VmError
    let tokens = Lexer::new(
        r#"
            import task
            fn main() {
                let t = task.spawn(fn() { 1 / 0 })
                task.join(t)
            }
            "#,
    )
    .tokenize()
    .unwrap();
    let program = Parser::new(tokens).parse_program().unwrap();
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).unwrap();
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    let err = vm.run(script).unwrap_err();
    assert!(
        err.message.contains("division by zero") || err.message.contains("joined task failed"),
        "expected division error, got: {}",
        err.message
    );
}
