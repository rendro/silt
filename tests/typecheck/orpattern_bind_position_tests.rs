//! Regression tests: or-pattern bindings must extract from the structural
//! position of whichever alternative actually matched at runtime — not from
//! the first alternative's positions.
//!
//! This is a codegen bug, so every test compiles AND RUNS the program and
//! asserts the produced value. A typecheck-only test would be a hollow lock:
//! the types are identical across alternatives, so only execution can prove
//! the right field/element is read.
//!
//! Bug: in `compile_pattern_bind`'s `PatternKind::Or` arm, the bind sequence
//! was emitted from the FIRST alternative only, while the test side routed
//! every matching alternative to that single shared bind block. When a shared
//! binding sat at a different position across alternatives (e.g. `A(x)` binds
//! field 0 but `B(_, x)` binds field 1), the runtime always extracted from the
//! first alternative's position.
//!
//! Fix: each alternative is re-tested and binds from its OWN positions, with
//! all alternatives funnelling results into one shared set of result slots.

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

/// Primary repro from the bug report. `A(x)` binds field 0; `B(_, x)` binds
/// field 1. Reading from field 0 for a `B` value would yield 99 instead of 20.
#[test]
fn test_constructor_different_position_bind() {
    // pick(A(10)) must be 10.
    let a = run(r#"
type E { A(Int), B(Int, Int) }
fn pick(e: E) -> Int {
  match e {
    A(x) | B(_, x) -> x
  }
}
fn main() -> Int { pick(A(10)) }
    "#);
    assert_eq!(a, Value::Int(10), "pick(A(10)) should bind field 0");

    // pick(B(99, 20)) must be 20 (field 1), NOT 99 (field 0).
    let b = run(r#"
type E { A(Int), B(Int, Int) }
fn pick(e: E) -> Int {
  match e {
    A(x) | B(_, x) -> x
  }
}
fn main() -> Int { pick(B(99, 20)) }
    "#);
    assert_eq!(
        b,
        Value::Int(20),
        "pick(B(99, 20)) should bind field 1, not 99"
    );
}

/// Control case from the report: same position across alternatives must
/// still be correct (this case worked before the fix and must stay correct).
#[test]
fn test_constructor_same_position_bind_control() {
    let b = run(r#"
type E { A(Int), B(Int, Int) }
fn pick(e: E) -> Int {
  match e {
    A(x) | B(x, _) -> x
  }
}
fn main() -> Int { pick(B(7, 99)) }
    "#);
    assert_eq!(b, Value::Int(7));
}

/// Two distinct constructors, the shared `x` at opposite positions:
/// `P(x, _)` (field 0) and `Q(_, x)` (field 1).
#[test]
fn test_two_constructors_opposite_positions() {
    let p = run(r#"
type T { P(Int, Int), Q(Int, Int) }
fn get(t: T) -> Int {
  match t {
    P(x, _) | Q(_, x) -> x
  }
}
fn main() -> Int { get(P(1, 2)) }
    "#);
    assert_eq!(p, Value::Int(1), "P(1,2) binds field 0");

    let q = run(r#"
type T { P(Int, Int), Q(Int, Int) }
fn get(t: T) -> Int {
  match t {
    P(x, _) | Q(_, x) -> x
  }
}
fn main() -> Int { get(Q(1, 2)) }
    "#);
    assert_eq!(q, Value::Int(2), "Q(1,2) binds field 1, not field 0");
}

/// List patterns: `[x]` binds index 0 of a 1-element list; `[_, x]` binds
/// index 1 of a 2-element list.
#[test]
fn test_list_different_position_bind() {
    let single = run(r#"
fn pick(xs: List(Int)) -> Int {
  match xs {
    [x] | [_, x] -> x
    _ -> -1
  }
}
fn main() -> Int { pick([5]) }
    "#);
    assert_eq!(single, Value::Int(5), "[5] binds index 0");

    let pair = run(r#"
fn pick(xs: List(Int)) -> Int {
  match xs {
    [x] | [_, x] -> x
    _ -> -1
  }
}
fn main() -> Int { pick([5, 9]) }
    "#);
    assert_eq!(pair, Value::Int(9), "[5,9] binds index 1, not index 0");
}

/// Or-pattern combined with a guard. The guard evaluates against the bound
/// value; if the binding read from the wrong position the guard would route
/// to the wrong branch. `B(99, 20)` binds x=20, so `x > 50` is false and we
/// fall to the second arm returning x*2 = 40.
#[test]
fn test_or_pattern_with_guard() {
    let r = run(r#"
type E { A(Int), B(Int, Int) }
fn classify(e: E) -> Int {
  match e {
    A(x) | B(_, x) when x > 50 -> x
    A(x) | B(_, x) -> x * 2
  }
}
fn main() -> Int { classify(B(99, 20)) }
    "#);
    assert_eq!(
        r,
        Value::Int(40),
        "x must be 20 (field 1): 20>50 false, so 20*2=40"
    );

    // And when the correctly-bound value DOES pass the guard:
    let hi = run(r#"
type E { A(Int), B(Int, Int) }
fn classify(e: E) -> Int {
  match e {
    A(x) | B(_, x) when x > 50 -> x
    A(x) | B(_, x) -> x * 2
  }
}
fn main() -> Int { classify(B(1, 99)) }
    "#);
    assert_eq!(hi, Value::Int(99), "x must be 99 (field 1): 99>50 true");
}

/// Nested or-pattern: the shared binding lives inside a tuple, and the inner
/// alternatives place it at different positions.
#[test]
fn test_nested_or_pattern_bind_position() {
    let left = run(r#"
type E { A(Int), B(Int, Int) }
fn pick(p: (Int, E)) -> Int {
  match p {
    (_, A(x)) | (_, B(_, x)) -> x
  }
}
fn main() -> Int { pick((0, A(11))) }
    "#);
    assert_eq!(left, Value::Int(11));

    let right = run(r#"
type E { A(Int), B(Int, Int) }
fn pick(p: (Int, E)) -> Int {
  match p {
    (_, A(x)) | (_, B(_, x)) -> x
  }
}
fn main() -> Int { pick((0, B(99, 22))) }
    "#);
    assert_eq!(
        right,
        Value::Int(22),
        "nested B binds inner field 1, not 99"
    );
}

/// Multiple shared bindings at swapped positions across alternatives.
#[test]
fn test_multiple_bindings_swapped() {
    let p = run(r#"
type T { P(Int, Int), Q(Int, Int) }
fn diff(t: T) -> Int {
  match t {
    P(a, b) | Q(b, a) -> a - b
  }
}
fn main() -> Int { diff(P(10, 3)) }
    "#);
    assert_eq!(p, Value::Int(7), "P(10,3): a=10, b=3 -> 7");

    let q = run(r#"
type T { P(Int, Int), Q(Int, Int) }
fn diff(t: T) -> Int {
  match t {
    P(a, b) | Q(b, a) -> a - b
  }
}
fn main() -> Int { diff(Q(10, 3)) }
    "#);
    // Q(b, a): b=10, a=3 -> a - b = 3 - 10 = -7
    assert_eq!(q, Value::Int(-7), "Q(10,3): a=3, b=10 -> -7");
}
