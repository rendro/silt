//! Lock tests for impl-level and method-level `where` clause enforcement
//! at call sites.
//!
//! These tests pin the end-to-end behaviour established by the round-22
//! follow-up that wired `method_constraints` from
//! `register_trait_impl` through to receiver-method dispatch and through
//! the where-aware generalize path. A regression that severs any link in
//! that chain (impl-level constraints not flowing into method_table,
//! method-level where being silently dropped, or call-site dispatch not
//! consulting the recorded constraints) would cause one or more of these
//! to fail.

use std::sync::Arc;

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::types::Severity;
use silt::value::Value;
use silt::vm::Vm;

// ── Helpers ─────────────────────────────────────────────────────────

/// Typecheck-only: collect hard-error messages.
fn type_errors(input: &str) -> Vec<String> {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    typechecker::check(&mut program)
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

/// Full pipeline: lex → parse → typecheck → compile → run. Panics on
/// any error so positive tests pin the happy path.
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

// ── Impl-level where: positive lock ─────────────────────────────────

/// Sanity-check the happy path: `Box(42).greet()` typechecks AND runs
/// because Int implements Greet, so the impl-level `where a: Greet` is
/// satisfied at the call site.
#[test]
fn test_impl_where_accepts_int_inner() {
    let v = run(r#"
type Box(T) { Box(T) }
trait Greet { fn greet(self) -> String }
trait Greet for Int { fn greet(self) -> String { "int" } }
trait Greet for Box(a) where a: Greet {
  fn greet(self) -> String {
    match self { Box(inner) -> inner.greet() }
  }
}
fn main() -> String {
  Box(42).greet()
}
"#);
    match v {
        Value::String(s) => assert_eq!(s.as_str(), "int"),
        other => panic!("expected String(\"int\"), got {other:?}"),
    }
}

// ── Impl-level where: negative lock ─────────────────────────────────

/// `Box("hello").greet()` must be rejected at the call site because
/// String does not implement Greet, so the impl-level constraint
/// `where a: Greet` is unsatisfied. Before the round-22 fix this was
/// silently accepted (the constraint only ran inside the method body,
/// which was never re-checked per call site).
#[test]
fn test_impl_where_rejects_string_inner() {
    let errs = type_errors(
        r#"
type Box(T) { Box(T) }
trait Greet { fn greet(self) -> String }
trait Greet for Int { fn greet(self) -> String { "int" } }
trait Greet for Box(a) where a: Greet {
  fn greet(self) -> String {
    match self { Box(inner) -> inner.greet() }
  }
}
fn main() {
  let b = Box("hello")
  println(b.greet())
}
"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("type 'String' does not implement trait 'Greet'")),
        "expected impl-level where rejection for String inner, got: {errs:?}"
    );
}

// ── Method-level where: enforced at call site ───────────────────────

/// Method-level where (declared on the method, not the impl header) must
/// also be enforced at call sites. Previously these were silently
/// ignored — now they flow into method_constraints alongside impl-level
/// ones.
#[test]
fn test_method_level_where_rejects_at_call_site() {
    let errs = type_errors(
        r#"
type Box(T) { Box(T) }
trait Greet { fn greet(self) -> String }
trait Greet for Int { fn greet(self) -> String { "int" } }
trait Wrap { fn unwrap(self) -> String }
trait Wrap for Box(a) {
  fn unwrap(self) -> String where a: Greet {
    match self { Box(inner) -> inner.greet() }
  }
}
fn main() {
  let b = Box("hello")
  println(b.unwrap())
}
"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("type 'String' does not implement trait 'Greet'")),
        "expected method-level where rejection for String inner, got: {errs:?}"
    );
}

/// Method-level where positive case must run: Box(42).unwrap() with an
/// Int inner satisfies `where a: Greet` and dispatches through.
#[test]
fn test_method_level_where_accepts_satisfying_call() {
    let v = run(r#"
type Box(T) { Box(T) }
trait Greet { fn greet(self) -> String }
trait Greet for Int { fn greet(self) -> String { "int" } }
trait Wrap { fn unwrap(self) -> String }
trait Wrap for Box(a) {
  fn unwrap(self) -> String where a: Greet {
    match self { Box(inner) -> inner.greet() }
  }
}
fn main() -> String {
  Box(42).unwrap()
}
"#);
    match v {
        Value::String(s) => assert_eq!(s.as_str(), "int"),
        other => panic!("expected String(\"int\"), got {other:?}"),
    }
}

// ── Multi-trait impl-level where ────────────────────────────────────

/// Impl-level `where a: Greet + Loud` requires both traits on the inner
/// type. Calling with an inner that implements only Greet must be
/// rejected at the Loud half.
#[test]
fn test_impl_where_multi_constraint_rejects_partial_impl() {
    let errs = type_errors(
        r#"
type Box(T) { Box(T) }
trait Greet { fn greet(self) -> String }
trait Loud { fn loud(self) -> String }
trait Greet for Int { fn greet(self) -> String { "int" } }
trait Greet for Box(a) where a: Greet + Loud {
  fn greet(self) -> String {
    match self { Box(inner) -> inner.greet() }
  }
}
fn main() {
  let b = Box(5)
  println(b.greet())
}
"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("does not implement trait 'Loud'")),
        "expected Loud rejection from multi-constraint, got: {errs:?}"
    );
}

/// Inner type implements only the second of two `+`-bound traits — the
/// Greet half must produce the rejection. Mirror of the previous test
/// to confirm both halves of the conjunction are checked.
#[test]
fn test_impl_where_multi_constraint_rejects_other_partial_impl() {
    let errs = type_errors(
        r#"
type Box(T) { Box(T) }
trait Greet { fn greet(self) -> String }
trait Loud { fn loud(self) -> String }
trait Loud for Int { fn loud(self) -> String { "INT" } }
trait Greet for Box(a) where a: Greet + Loud {
  fn greet(self) -> String {
    match self { Box(inner) -> inner.loud() }
  }
}
fn main() {
  let b = Box(5)
  println(b.greet())
}
"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("does not implement trait 'Greet'")),
        "expected Greet rejection from multi-constraint, got: {errs:?}"
    );
}

// ── Generic fn calling where-constrained trait method ───────────────

/// A generic function that calls a where-constrained trait method must
/// propagate the constraint requirement up to its own callers — i.e.
/// the caller must satisfy the constraint when invoking the wrapper, not
/// just when invoking the trait method directly.
#[test]
fn test_generic_fn_propagates_constraint_requirement() {
    let errs = type_errors(
        r#"
type Box(T) { Box(T) }
trait Greet { fn greet(self) -> String }
trait Greet for Int { fn greet(self) -> String { "int" } }
trait Greet for Box(a) where a: Greet {
  fn greet(self) -> String {
    match self { Box(inner) -> inner.greet() }
  }
}
fn use_box(b: Box(a)) -> String where a: Greet { b.greet() }
fn main() {
  let bad = Box("hello")
  println(use_box(bad))
}
"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("does not implement trait 'Greet'")),
        "expected propagated rejection through wrapper fn, got: {errs:?}"
    );
}

/// Sanity check positive path of the propagation lock above — calling
/// the wrapper with a satisfying inner must succeed. Without this the
/// negative test could spuriously pass by always rejecting.
#[test]
fn test_generic_fn_propagates_constraint_positive_path() {
    let v = run(r#"
type Box(T) { Box(T) }
trait Greet { fn greet(self) -> String }
trait Greet for Int { fn greet(self) -> String { "int" } }
trait Greet for Box(a) where a: Greet {
  fn greet(self) -> String {
    match self { Box(inner) -> inner.greet() }
  }
}
fn use_box(b: Box(a)) -> String where a: Greet { b.greet() }
fn main() -> String { use_box(Box(99)) }
"#);
    match v {
        Value::String(s) => assert_eq!(s.as_str(), "int"),
        other => panic!("expected String(\"int\"), got {other:?}"),
    }
}
