//! Regression tests for `Op::CallMethod` yield handling and `Op::ListConcat`
//! combined-size pre-check.
//!
//! Bug 1 (BROKEN): When `Op::CallMethod` dispatches a user-defined trait method
//! via `invoke_callable`, and that method internally yields (e.g. `io.read_file`
//! inside a `task.spawn`), the stack was left inconsistent because the receiver
//! and args were truncated before the yield propagated.  The fix re-pushes the
//! saved args (or receiver + extra_args for the record-field callable path) so
//! the instruction can re-execute cleanly after resume.
//!
//! Bug 2 (LATENT): `resume_suspended_invoke` omitted `prune_tco_elided` calls
//! in its Return and EarlyReturn arms (unlike `invoke_callable` which has them).
//! This only affects diagnostic output, so a direct unit test is impractical --
//! the fix mirrors invoke_callable and is verified by code inspection.
//!
//! Bug 3 (LATENT): `Op::ListConcat` validated each operand individually (max
//! 10M) but only checked the combined size after materializing both.  Two 9.9M
//! ranges would cause ~800MB allocation before being rejected.  The fix adds a
//! pre-check on `result.len() + b_len` before extending.

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

fn run_err(input: &str) -> String {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = match compiler.compile_program(&program) {
        Ok(f) => f,
        Err(e) => return e.message,
    };
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    let err = vm.run(script).expect_err("expected runtime error");
    format!("{err}")
}

// ── CallMethod yield: trait method with io.read_file in task.spawn ────

/// Core regression: a user-defined trait method that internally calls a
/// yielding builtin (io.read_file) inside a spawned task must survive the
/// yield/resume cycle without corrupting the CallMethod stack layout.
#[test]
fn test_call_method_yield_trait_method_in_spawned_task() {
    let tmp = std::env::temp_dir().join("silt_test_call_method_yield.txt");
    let tmp_str = tmp.to_str().unwrap().replace('\\', "/");

    std::fs::write(&tmp, "hello from task").unwrap();

    let input = format!(
        r#"
import io
import task

type Greeter {{ path: String }}

trait Greet {{ fn greet(self) -> String }}

trait Greet for Greeter {{
  fn greet(self) -> String {{
    match io.read_file(self.path) {{
      Ok(s) -> s
      Err(_) -> "failed"
    }}
  }}
}}

fn main() {{
  let g = Greeter {{ path: "{tmp_str}" }}
  let handle = task.spawn(fn() {{
    let msg = g.greet()
    msg
  }})
  task.join(handle)
}}
"#
    );
    let result = run(&input);
    assert_eq!(
        result,
        Value::String("hello from task".into()),
        "expected trait method to return file contents via task.spawn, got {result:?}"
    );

    let _ = std::fs::remove_file(&tmp);
}

// ── CallMethod: non-yielding trait method (control case) ──────────────

/// Verify that non-yielding CallMethod dispatch still works correctly
/// after the yield-handling changes.
#[test]
fn test_call_method_non_yielding_trait_method() {
    let result = run(r#"
type Counter { n: Int }

trait Describe { fn describe(self) -> String }

trait Describe for Counter {
  fn describe(self) -> String {
    "count={self.n}"
  }
}

fn main() {
  let c = Counter { n: 42 }
  c.describe()
}
"#);
    assert_eq!(result, Value::String("count=42".into()));
}

// ── ListConcat combined-size pre-check ────────────────────────────────

/// Two ranges whose individual sizes are under the 10M cap but whose
/// combined size exceeds it must be rejected WITHOUT materializing both.
/// Before the fix, this would allocate ~800MB before failing.
/// Op::ListConcat is emitted for list-spread syntax `[..a, ..b]`.
#[test]
fn test_list_concat_combined_size_rejected_before_materialize() {
    let err = run_err(
        r#"
fn main() {
  let a = 1..6_000_000
  let b = 1..6_000_000
  [..a, ..b]
}
"#,
    );
    assert!(
        err.contains("concatenated list exceeds maximum size"),
        "expected combined-size rejection, got: {err}"
    );
}
