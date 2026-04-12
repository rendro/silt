//! Regression tests for indirect calls to yielding builtins in spawned tasks.
//!
//! When a `BuiltinFn` (e.g. `io.read_file`) is captured in a variable and
//! called via `Op::Call` (not `Op::CallBuiltin`) inside a spawned task, the
//! yield/resume cycle must preserve the function value on the stack so the
//! opcode can locate it upon re-execution.

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

/// Core regression: an IO builtin captured in a variable and called indirectly
/// from a spawned task must survive the yield/resume cycle without corrupting
/// the stack.
#[test]
fn test_indirect_io_builtin_call_in_spawned_task() {
    let tmp = std::env::temp_dir().join("silt_test_indirect_builtin.txt");
    let tmp_str = tmp.to_str().unwrap().replace('\\', "/");

    // Write a known payload so we can verify the read.
    std::fs::write(&tmp, "hello from indirect").unwrap();

    let input = format!(
        r#"
import io
import task

fn main() {{
  let read = io.read_file
  let h = task.spawn(fn() {{ read("{tmp_str}") }})
  task.join(h)
}}
"#
    );
    let result = run(&input);
    assert!(
        matches!(
            result,
            Value::Variant(ref tag, ref args)
                if tag == "Ok" && args[0] == Value::String("hello from indirect".into())
        ),
        "expected Ok(\"hello from indirect\"), got {result:?}"
    );

    // Clean up
    let _ = std::fs::remove_file(&tmp);
}

/// Same idea but with io.write_file captured in a variable — another yielding
/// IO builtin called indirectly from a spawned task.
#[test]
fn test_indirect_io_write_builtin_in_spawned_task() {
    let tmp = std::env::temp_dir().join("silt_test_indirect_write_builtin.txt");
    let tmp_str = tmp.to_str().unwrap().replace('\\', "/");

    // Remove any leftover from a previous run.
    let _ = std::fs::remove_file(&tmp);

    let input = format!(
        r#"
import io
import task

fn main() {{
  let write = io.write_file
  let h = task.spawn(fn() {{ write("{tmp_str}", "written indirectly") }})
  task.join(h)
}}
"#
    );
    let result = run(&input);
    // write_file returns Ok(()) on success
    assert!(
        matches!(result, Value::Variant(ref tag, _) if tag == "Ok"),
        "expected Ok variant, got {result:?}"
    );

    let contents = std::fs::read_to_string(&tmp).unwrap();
    assert_eq!(contents, "written indirectly");

    // Clean up
    let _ = std::fs::remove_file(&tmp);
}

/// Non-yielding builtins called indirectly in a spawned task must still work
/// correctly (the fix must not break the non-yield path).
#[test]
fn test_indirect_non_yielding_builtin_in_spawned_task() {
    let result = run(
        r#"
import list
import task

fn main() {
  let len = list.length
  let h = task.spawn(fn() { len([10, 20, 30]) })
  task.join(h)
}
"#,
    );
    assert_eq!(result, Value::Int(3));
}

/// Verify that a direct call to a yielding builtin in a spawned task still
/// works (this path goes through Op::CallBuiltin, not Op::Call).
#[test]
fn test_direct_io_builtin_in_spawned_task_still_works() {
    let tmp = std::env::temp_dir().join("silt_test_direct_builtin.txt");
    let tmp_str = tmp.to_str().unwrap().replace('\\', "/");
    std::fs::write(&tmp, "direct call").unwrap();

    let input = format!(
        r#"
import io
import task

fn main() {{
  let h = task.spawn(fn() {{ io.read_file("{tmp_str}") }})
  task.join(h)
}}
"#
    );
    let result = run(&input);
    assert!(
        matches!(
            result,
            Value::Variant(ref tag, ref args)
                if tag == "Ok" && args[0] == Value::String("direct call".into())
        ),
        "expected Ok(\"direct call\"), got {result:?}"
    );

    let _ = std::fs::remove_file(&tmp);
}
