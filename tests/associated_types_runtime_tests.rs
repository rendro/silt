//! Runtime locks for associated-types end-to-end.
//!
//! The typechecker tests in `tests/associated_types_tests.rs` cover the
//! pure type-level surface: declarations, bindings, projection
//! resolution, bound enforcement, supertrait inheritance.
//!
//! These runtime tests drive the compiled program through the VM so we
//! also lock the dispatch path: when a method's return type is
//! `Self::Item` (which the typechecker reduces to `Int` at impl
//! registration), the VM must call the correct method body and the
//! returned value must round-trip back through silt's value model.

use std::time::Duration;

use silt::scheduler::test_support::InProcessRunner;
use silt::value::Value;

fn run(src: &str) -> Option<Value> {
    let runner = InProcessRunner::new(src).with_budget(Duration::from_secs(5));
    let outcome = runner.run_trial();
    assert!(outcome.ok(), "run should succeed: {outcome:?}");
    outcome.result
}

/// End-to-end: trait declares `type Item`, impl binds `type Item = Int`,
/// the impl method returns `Self::Item`. At runtime the method must
/// produce the bound int.
#[test]
fn assoc_type_method_returns_bound_int() {
    let src = r#"
trait Stream {
  type Item
  fn first(self) -> Self::Item
}

type Wrap { v: Int }

trait Stream for Wrap {
  type Item = Int

  fn first(self) -> Int {
    self.v
  }
}

fn main() {
  let w = Wrap { v: 7 }
  w.first()
}
"#;
    assert_eq!(run(src), Some(Value::Int(7)));
}

/// Multiple associated types per trait round-trip through the VM.
#[test]
fn multiple_assoc_types_runtime() {
    let src = r#"
trait Pair {
  type First
  type Second
  fn first(self) -> Self::First
  fn second(self) -> Self::Second
}

type IntStringPair { a: Int, b: String }

trait Pair for IntStringPair {
  type First = Int
  type Second = String

  fn first(self) -> Int { self.a }
  fn second(self) -> String { self.b }
}

fn main() {
  let p = IntStringPair { a: 13, b: "ok" }
  p.first()
}
"#;
    assert_eq!(run(src), Some(Value::Int(13)));
}

/// Supertrait inheritance: `Sub: Super` references `Self::Item`
/// declared on Super; the impl Super for T binds it; the impl Sub for
/// T's method must dispatch correctly and return the bound type.
#[test]
fn supertrait_assoc_type_runtime() {
    let src = r#"
trait Super {
  type Item
  fn one(self) -> Self::Item
}

trait Sub: Super {
  fn first(self) -> Self::Item
}

type Wrap { v: Int }

trait Super for Wrap {
  type Item = Int
  fn one(self) -> Int { self.v }
}

trait Sub for Wrap {
  fn first(self) -> Int { self.v + 1 }
}

fn main() {
  let w = Wrap { v: 41 }
  w.first()
}
"#;
    assert_eq!(run(src), Some(Value::Int(42)));
}

/// Two impls of the same trait on different types each bind a different
/// concrete type for the assoc-type. The VM dispatches each receiver to
/// the corresponding impl's method body.
#[test]
fn distinct_assoc_bindings_per_impl_runtime() {
    let src = r#"
trait Stream {
  type Item
  fn first(self) -> Self::Item
}

type A { x: Int }
type B { y: String }

trait Stream for A {
  type Item = Int
  fn first(self) -> Int { self.x }
}

trait Stream for B {
  type Item = String
  fn first(self) -> String { self.y }
}

fn main() {
  let a = A { x: 9 }
  a.first()
}
"#;
    assert_eq!(run(src), Some(Value::Int(9)));
}
