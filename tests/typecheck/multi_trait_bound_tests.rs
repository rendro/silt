//! Lock tests for multi-trait bounds on generic functions.
//!
//! Two surface syntaxes flatten to the same constraint set:
//!   - `where a: MyEq + MyOrd`        (plus form)
//!   - `where a: MyEq, a: MyOrd`      (comma form)
//!
//! These tests confirm:
//!   - Both forms accept types that implement every listed trait.
//!   - Both forms reject types that implement only a subset.
//!   - The plus and comma forms are observably equivalent.
//!   - Three-trait bounds work the same as two-trait bounds.
//!
//! Together they pin the parser flattening of `+`-separated and
//! `,`-separated where clauses into a single `(tv, trait)` constraint
//! list, plus the call-site enforcement that consults every entry.
//!
//! Note on naming: silt auto-derives `Equal`/`Compare`/`Hash`/`Display`
//! for primitive and container types. To exercise multi-trait bounds
//! against an isolated implementation surface, these tests use distinct
//! user trait names (MyEq / MyOrd / MyShow) so the constraint check
//! exercises only the user impls and not the auto-derived ones.

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

// ── Plus form: positive ─────────────────────────────────────────────

/// `where a: MyEq + MyOrd` accepts Int because Int impls both.
#[test]
fn test_plus_form_accepts_type_implementing_both() {
    let v = run(r#"
trait MyEq { fn my_eq(self, other: Self) -> Bool }
trait MyOrd { fn my_cmp(self, other: Self) -> Int }
trait MyEq for Int { fn my_eq(self, other: Int) -> Bool { self == other } }
trait MyOrd for Int {
  fn my_cmp(self, other: Int) -> Int {
    match self < other {
      true -> -1
      false -> match self > other { true -> 1, false -> 0 }
    }
  }
}
fn use_both(x: a, y: a) -> Int where a: MyEq + MyOrd {
  match x.my_eq(y) { true -> 0, false -> x.my_cmp(y) }
}
fn main() -> Int { use_both(3, 7) }
"#);
    match v {
        Value::Int(-1) => {}
        other => panic!("expected Int(-1), got {other:?}"),
    }
}

// ── Plus form: negative (missing one trait) ─────────────────────────

/// String impls MyEq but NOT MyOrd — calling `use_both("a", "b")`
/// must be rejected against the MyOrd half of the bound.
#[test]
fn test_plus_form_rejects_type_implementing_only_one() {
    let errs = type_errors(
        r#"
trait MyEq { fn my_eq(self, other: Self) -> Bool }
trait MyOrd { fn my_cmp(self, other: Self) -> Int }
trait MyEq for String { fn my_eq(self, other: String) -> Bool { self == other } }
fn use_both(x: a, y: a) -> Int where a: MyEq + MyOrd {
  match x.my_eq(y) { true -> 0, false -> x.my_cmp(y) }
}
fn main() { println(use_both("a", "b")) }
"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("does not implement trait 'MyOrd'")),
        "expected MyOrd rejection, got: {errs:?}"
    );
}

/// Mirror of the previous test: a type implementing MyOrd but not MyEq
/// must be rejected against the MyEq half. Confirms both halves of the
/// bound are checked, not just whichever appears last.
#[test]
fn test_plus_form_rejects_when_first_trait_missing() {
    let errs = type_errors(
        r#"
trait MyEq { fn my_eq(self, other: Self) -> Bool }
trait MyOrd { fn my_cmp(self, other: Self) -> Int }
trait MyOrd for Float {
  fn my_cmp(self, other: Float) -> Int {
    match self < other {
      true -> -1
      false -> match self > other { true -> 1, false -> 0 }
    }
  }
}
fn use_both(x: a, y: a) -> Int where a: MyEq + MyOrd {
  match x.my_eq(y) { true -> 0, false -> x.my_cmp(y) }
}
fn main() { println(use_both(1.0, 2.0)) }
"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("does not implement trait 'MyEq'")),
        "expected MyEq rejection, got: {errs:?}"
    );
}

// ── Comma form: positive ────────────────────────────────────────────

/// `where a: MyEq, a: MyOrd` is the comma form. Must accept Int
/// identically to the plus form.
#[test]
fn test_comma_form_accepts_type_implementing_both() {
    let v = run(r#"
trait MyEq { fn my_eq(self, other: Self) -> Bool }
trait MyOrd { fn my_cmp(self, other: Self) -> Int }
trait MyEq for Int { fn my_eq(self, other: Int) -> Bool { self == other } }
trait MyOrd for Int {
  fn my_cmp(self, other: Int) -> Int {
    match self < other {
      true -> -1
      false -> match self > other { true -> 1, false -> 0 }
    }
  }
}
fn use_both(x: a, y: a) -> Int where a: MyEq, a: MyOrd {
  match x.my_eq(y) { true -> 0, false -> x.my_cmp(y) }
}
fn main() -> Int { use_both(5, 5) }
"#);
    match v {
        Value::Int(0) => {}
        other => panic!("expected Int(0), got {other:?}"),
    }
}

// ── Comma form: negative ────────────────────────────────────────────

/// Comma form must reject a type missing either constraint, matching
/// the plus-form behaviour exactly.
#[test]
fn test_comma_form_rejects_type_implementing_only_one() {
    let errs = type_errors(
        r#"
trait MyEq { fn my_eq(self, other: Self) -> Bool }
trait MyOrd { fn my_cmp(self, other: Self) -> Int }
trait MyEq for String { fn my_eq(self, other: String) -> Bool { self == other } }
fn use_both(x: a, y: a) -> Int where a: MyEq, a: MyOrd {
  match x.my_eq(y) { true -> 0, false -> x.my_cmp(y) }
}
fn main() { println(use_both("a", "b")) }
"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("does not implement trait 'MyOrd'")),
        "expected MyOrd rejection in comma form, got: {errs:?}"
    );
}

// ── Three-trait bound ───────────────────────────────────────────────

/// `where a: MyEq + MyOrd + MyShow` works the same way for three
/// traits. Catches a flattening bug that handles N=2 specially.
#[test]
fn test_three_trait_bound_accepts() {
    let v = run(r#"
trait MyEq { fn my_eq(self, other: Self) -> Bool }
trait MyOrd { fn my_cmp(self, other: Self) -> Int }
trait MyShow { fn my_show(self) -> String }
trait MyEq for Int { fn my_eq(self, other: Int) -> Bool { self == other } }
trait MyOrd for Int {
  fn my_cmp(self, other: Int) -> Int {
    match self < other {
      true -> -1
      false -> match self > other { true -> 1, false -> 0 }
    }
  }
}
trait MyShow for Int { fn my_show(self) -> String { "int" } }

fn use_three(x: a, y: a) -> String where a: MyEq + MyOrd + MyShow {
  match x.my_eq(y) { true -> x.my_show(), false -> "diff" }
}
fn main() -> String { use_three(2, 2) }
"#);
    match v {
        Value::String(s) => assert_eq!(s.as_str(), "int"),
        other => panic!("expected String(\"int\"), got {other:?}"),
    }
}

/// Three-trait bound rejects when the third trait is missing on the
/// argument type. Locks that the LAST entry in the conjunction is
/// checked, not silently dropped.
#[test]
fn test_three_trait_bound_rejects_when_last_missing() {
    let errs = type_errors(
        r#"
trait MyEq { fn my_eq(self, other: Self) -> Bool }
trait MyOrd { fn my_cmp(self, other: Self) -> Int }
trait MyShow { fn my_show(self) -> String }
trait MyEq for Int { fn my_eq(self, other: Int) -> Bool { self == other } }
trait MyOrd for Int { fn my_cmp(self, other: Int) -> Int { 0 } }
-- Note: Int does NOT implement MyShow in this test
fn use_three(x: a, y: a) -> String where a: MyEq + MyOrd + MyShow {
  match x.my_eq(y) { true -> x.my_show(), false -> "diff" }
}
fn main() { println(use_three(1, 1)) }
"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("does not implement trait 'MyShow'")),
        "expected MyShow rejection in three-bound, got: {errs:?}"
    );
}
