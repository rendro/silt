//! Lock tests for recursive trait method dispatch — the cases where one
//! trait method's body calls another trait method (either a different
//! trait on the same receiver, or the same trait recursing through a
//! different impl, or both nested through a parameterized container).
//!
//! These shape the trait-system invariants that:
//!   - A trait method body can freely dispatch through `self` to OTHER
//!     traits implemented on the same type.
//!   - Same-named trait recursion through a different impl works (the
//!     classic shape used by container impls like `trait X for Box(a)`).
//!   - Nested parameterized containers — `Box(Box(Int))` — chain through
//!     impl-level where constraints all the way down.

use std::sync::Arc;

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::types::Severity;
use silt::value::Value;
use silt::vm::Vm;

// ── Helpers ─────────────────────────────────────────────────────────

fn type_errors(input: &str) -> Vec<String> {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    typechecker::check(&mut program)
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

fn run(input: &str) -> Value {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let errs = typechecker::check(&mut program);
    let fatal: Vec<_> = errs
        .iter()
        .filter(|e| e.severity == Severity::Error)
        .collect();
    assert!(fatal.is_empty(), "type errors: {fatal:?}");
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    vm.run(script).expect("runtime error")
}

// ── Cross-trait dispatch on the same receiver ───────────────────────

/// A trait method body invokes a method from a DIFFERENT trait on the
/// same receiver. Both traits must be implemented for the receiver type
/// for this to typecheck. Validates that intra-impl dispatch consults
/// the full method_table, not just the current trait's methods.
#[test]
fn test_cross_trait_method_call_on_self() {
    let v = run(r#"
trait Greet { fn greet(self) -> String }
trait Loud { fn loud(self) -> String }
trait Greet for Int { fn greet(self) -> String { "int" } }
trait Loud for Int { fn loud(self) -> String { "INT" } }

trait Combined { fn combined(self) -> String }
trait Combined for Int {
  fn combined(self) -> String { "{self.greet()}-{self.loud()}" }
}

fn main() -> String { (1).combined() }
"#);
    match v {
        Value::String(s) => assert_eq!(s.as_str(), "int-INT"),
        other => panic!("expected String, got {other:?}"),
    }
}

// ── Same-trait recursion through a different impl ───────────────────

/// `Box(a)` impl of Greet calls `inner.greet()` which dispatches into
/// the Greet impl for Int. This is the classic recursive trait pattern.
#[test]
fn test_same_trait_recursion_through_different_impl() {
    let v = run(r#"
type Box(T) { Box(T) }
trait Greet { fn greet(self) -> String }
trait Greet for Int { fn greet(self) -> String { "int-leaf" } }
trait Greet for Box(a) where a: Greet {
  fn greet(self) -> String {
    match self { Box(inner) -> "box({inner.greet()})" }
  }
}

fn main() -> String { Box(7).greet() }
"#);
    match v {
        Value::String(s) => assert_eq!(s.as_str(), "box(int-leaf)"),
        other => panic!("expected String, got {other:?}"),
    }
}

// ── Nested container: Box(Box(Int)) with chained constraints ────────

/// Two layers of Box: `Box(Box(Int))`. Each Greet call descends through
/// the impl-level where: Box(a) requires a: Greet, so the outer call
/// requires Box(Int) to impl Greet (which it does via the same Box(a)
/// impl with Int as the inner), which in turn requires Int to impl
/// Greet. The dispatch chain runs three levels deep at runtime.
#[test]
fn test_nested_box_dispatch_runtime() {
    let v = run(r#"
type Box(T) { Box(T) }
trait Greet { fn greet(self) -> String }
trait Greet for Int { fn greet(self) -> String { "leaf" } }
trait Greet for Box(a) where a: Greet {
  fn greet(self) -> String {
    match self { Box(inner) -> "[{inner.greet()}]" }
  }
}

fn main() -> String { Box(Box(99)).greet() }
"#);
    match v {
        Value::String(s) => assert_eq!(s.as_str(), "[[leaf]]"),
        other => panic!("expected String, got {other:?}"),
    }
}

/// Negative lock for the nested case — `Box(Box("x"))` must be rejected
/// because the innermost String doesn't implement Greet, and that
/// failure must propagate through both layers of the where-constraint
/// chain rather than silently dispatching to a non-existent method.
#[test]
fn test_nested_box_rejects_unconstrained_innermost() {
    let errs = type_errors(
        r#"
type Box(T) { Box(T) }
trait Greet { fn greet(self) -> String }
trait Greet for Int { fn greet(self) -> String { "leaf" } }
trait Greet for Box(a) where a: Greet {
  fn greet(self) -> String {
    match self { Box(inner) -> "[{inner.greet()}]" }
  }
}

fn main() {
  let b = Box(Box("x"))
  println(b.greet())
}
"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("does not implement trait 'Greet'")),
        "expected nested-where rejection, got: {errs:?}"
    );
}

// ── Cross-trait recursion through a chain ───────────────────────────

/// trait A's impl-for-Int body calls trait B on self; trait B is also
/// implemented for Int. Catches the case where method_table lookups
/// from inside another trait's impl body consult the SAME method_table
/// (not a per-trait private table).
#[test]
fn test_chained_cross_trait_calls() {
    let v = run(r#"
trait Halve { fn halve(self) -> Int }
trait Double { fn double(self) -> Int }
trait Quadruple { fn quad(self) -> Int }

trait Halve for Int { fn halve(self) -> Int { self / 2 } }
trait Double for Int { fn double(self) -> Int { self * 2 } }
trait Quadruple for Int { fn quad(self) -> Int { self.double().double() } }

fn main() -> Int { (5).quad() }
"#);
    match v {
        Value::Int(20) => {}
        other => panic!("expected Int(20), got {other:?}"),
    }
}
