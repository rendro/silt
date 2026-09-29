//! Regression tests for B2: Or-pattern with compound alternatives
//! silently fails to match.
//!
//! When an or-pattern alternative contains a compound pattern (tuple,
//! constructor, list) with a refutable sub-pattern, and that alternative
//! partially matches (structural test passes but a nested sub-test
//! fails), DestructTuple/DestructVariant/DestructList values were left
//! on the stack.  The JumpIfFalse from the failing sub-test jumped
//! directly to the next alternative's code, bypassing the cleanup Pop.
//! The next alternative then peeked TOS expecting the original scrutinee
//! but instead saw the stale destructured value.
//!
//! Fix: `compile_pattern_test_tracked` tracks the destruct depth at
//! each failure point and emits Pop trampolines so the stack is clean
//! when the next alternative's test begins.

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

/// B2 primary repro: tuple or-pattern where the first alternative
/// partially matches (tuple length OK but element test fails).
/// Before the fix this returned "nope"; must return "found".
#[test]
fn test_or_pattern_tuple_compound_match() {
    let result = run(r#"
fn main() {
  match (5, 2) {
    (1, _) | (_, 2) -> "found"
    _ -> "nope"
  }
}
    "#);
    assert_eq!(result, Value::String("found".into()));
}

/// B2 list variant: list or-pattern where the first alternative
/// partially matches (list length OK but element test fails).
/// Before the fix this returned "nope"; must return "found".
#[test]
fn test_or_pattern_list_compound_match() {
    let result = run(r#"
fn main() {
  match [5, 2] {
    [1, 2] | [5, 2] -> "found"
    _ -> "nope"
  }
}
    "#);
    assert_eq!(result, Value::String("found".into()));
}

/// B2 constructor variant: constructor or-pattern where the first
/// alternative partially matches (tag OK but field test fails).
#[test]
fn test_or_pattern_constructor_compound_match() {
    let result = run(r#"
fn main() {
  match Some(42) {
    Some(1) | Some(42) -> "found"
    _ -> "nope"
  }
}
    "#);
    assert_eq!(result, Value::String("found".into()));
}

/// Positive lock: simple (non-compound) or-patterns must still work.
#[test]
fn test_or_pattern_simple_still_works() {
    let result = run(r#"
fn main() {
  match 3 {
    1 | 2 | 3 -> "low"
    _ -> "high"
  }
}
    "#);
    assert_eq!(result, Value::String("low".into()));
}

/// Positive lock: compound or-pattern where the first alternative
/// matches (no cleanup needed).
#[test]
fn test_or_pattern_first_alt_matches() {
    let result = run(r#"
fn main() {
  match (1, 2) {
    (1, _) | (_, 2) -> "found"
    _ -> "nope"
  }
}
    "#);
    assert_eq!(result, Value::String("found".into()));
}
