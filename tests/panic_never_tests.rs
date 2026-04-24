//! Regression tests for B3: panic() must return Type::Never so it is
//! accepted in diverging positions (when/when-let else bodies, match arms).

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::types::Severity;
use silt::value::Value;
use silt::vm::Vm;
use std::sync::Arc;

// ── Helpers ─────────────────────────────────────────────────────────

/// Return all hard type-checker errors for the given source.
fn type_errors(input: &str) -> Vec<String> {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let errors = typechecker::check(&mut program);
    errors
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

/// Assert zero hard type-checker errors.
fn assert_no_type_errors(input: &str) {
    let errs = type_errors(input);
    assert!(errs.is_empty(), "expected no type errors, got: {errs:?}");
}

/// Compile and run, returning the runtime error message.
fn run_err(input: &str) -> String {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = match compiler.compile_program(&program) {
        Ok(f) => f,
        Err(e) => return e.message,
    };
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    match vm.run(script) {
        Err(e) => format!("{e}"),
        Ok(v) => panic!("expected runtime error, got: {v:?}"),
    }
}

/// Compile and run, returning the value.
fn run(input: &str) -> Value {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    vm.run(script).expect("runtime error")
}

// ── Tests ───────────────────────────────────────────────────────────

/// `when let` else body containing `panic()` must pass the typechecker.
/// Before the fix, panic() returned a fresh type variable instead of Never,
/// so the divergence check rejected it despite the error message recommending it.
#[test]
fn test_panic_in_when_let_else_accepted() {
    let input = r#"
fn main() {
  when let Some(x) = Some(42) else { panic("no") }
  println(x)
}
"#;
    assert_no_type_errors(input);
}

/// `when <bool> else { panic(...) }` must also pass.
#[test]
fn test_panic_in_when_bool_else_accepted() {
    let input = r#"
fn main() {
  let ok = true
  when ok else { panic("no") }
  println("ok")
}
"#;
    assert_no_type_errors(input);
}

/// Sanity check: panic() still produces a runtime error when executed.
#[test]
fn test_panic_still_errors_on_wrong_usage() {
    let input = r#"
fn main() {
  panic("boom")
}
"#;
    let err = run_err(input);
    assert!(
        err.contains("boom"),
        "expected panic error containing 'boom', got: {err}"
    );
}

/// In a match expression, panic()'s Never type should unify with the other
/// arm's concrete type, so the overall match has a well-defined type.
#[test]
fn test_panic_in_match_arm_type_compatible() {
    let input = r#"
fn get(x) {
  match x {
    Some(v) -> v
    None -> panic("no value")
  }
}

fn main() {
  let result = get(Some(99))
  println(result)
}
"#;
    assert_no_type_errors(input);
    let result = run(input);
    assert_eq!(result, Value::Unit);
}
