//! Lock tests for Option-1 trait impl target type parameters:
//! `trait Show for Box(a) { ... }` where `a` is an impl-scoped type
//! variable visible to all methods.
//!
//! Each test was written to FAIL against the pre-feature codebase (bare
//! target only; parameterized targets either unparseable or rejected by
//! the unify arity mismatch when the impl body pattern-matched `self`)
//! and to PASS after the feature landed across `src/ast.rs`,
//! `src/parser.rs`, `src/typechecker/mod.rs::register_trait_impl`, and
//! `src/formatter.rs::format_trait_impl_with_comments`.

use std::sync::Arc;

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::types::Severity;
use silt::value::Value;
use silt::vm::Vm;

// ── Helpers ─────────────────────────────────────────────────────────

/// Attempt to parse. Return the first parse error message, or None on
/// success. Tests that expect parse errors use this helper directly.
fn parse_error(input: &str) -> Option<String> {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    match Parser::new(tokens).parse_program() {
        Err(e) => Some(e.message),
        Ok(_) => None,
    }
}

/// Typecheck-only: collect error messages. Used for wrong-arity and
/// other typechecker-level rejections that the parser accepts.
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
/// any error so tests pin the happy path. Returns the top-level `main`
/// return value.
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

// ── Parse rejection ─────────────────────────────────────────────────

#[test]
fn test_parse_rejects_trait_impl_target_with_concrete_type_arg() {
    // Specialization foreclosed by Option 1 — `trait X for Box(Int)`
    // would require specialization, which silt does not support.
    let err = parse_error(
        r#"
type Box(T) { Box(T) }
trait X { fn x(self) -> Int }
trait X for Box(Int) {
  fn x(self) -> Int { 1 }
}
"#,
    )
    .expect("expected parse error on concrete-typed impl target");
    assert!(
        err.contains("must be a lowercase type variable") && err.contains("Int"),
        "got: {err}"
    );
}

#[test]
fn test_parse_rejects_trait_impl_target_with_duplicate_binder() {
    let err = parse_error(
        r#"
type Pair(A, B) { Pair(A, B) }
trait X { fn x(self) -> Int }
trait X for Pair(a, a) {
  fn x(self) -> Int { 1 }
}
"#,
    )
    .expect("expected parse error on duplicate impl binder");
    assert!(
        err.contains("duplicate type variable") && err.contains("'a'"),
        "got: {err}"
    );
}

#[test]
fn test_parse_rejects_trait_impl_target_with_non_named_type() {
    // Tuple targets have no head symbol for method_table / qualified-
    // name keying, so the parser bails out rather than the typechecker
    // limping along with a degenerate TraitImpl.
    let err = parse_error(
        r#"
trait X { fn x(self) -> Int }
trait X for (Int, Int) {
  fn x(self) -> Int { 1 }
}
"#,
    )
    .expect("expected parse error on tuple impl target");
    assert!(err.contains("must be a named type"), "got: {err}");
}

// ── Typecheck rejection ─────────────────────────────────────────────

#[test]
fn test_typecheck_rejects_trait_impl_target_wrong_arity() {
    // `Box(T)` is a 1-param enum; passing two binders must error with
    // a clear arity-mismatch diagnostic keyed on the impl head.
    let errs = type_errors(
        r#"
type Box(T) { Box(T) }
trait X { fn x(self) -> Int }
trait X for Box(a, b) {
  fn x(self) -> Int { 1 }
}
"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("type argument count mismatch")
                && e.contains("'Box'")
                && e.contains("expected 1, got 2")),
        "got: {errs:?}"
    );
}

// ── Happy path: runtime behaviour ───────────────────────────────────

#[test]
fn test_trait_impl_parameterized_enum_pattern_matches_self() {
    // Pre-feature: `match self { Box(inner) -> ... }` in the impl body
    // errored with "type argument count mismatch for Box: expected 0,
    // got 1" because type_from_name built Generic("Box", []) and the
    // match pattern expected Generic("Box", [elem]).
    let v = run(r#"
type Box(T) { Box(T) }
trait Wrap { fn unwrap(self) -> Int }
trait Wrap for Box(a) {
  fn unwrap(self) -> Int {
    match self { Box(inner) -> 42 }
  }
}
fn main() -> Int {
  let b = Box(99)
  b.unwrap()
}
"#);
    match v {
        Value::Int(42) => {}
        other => panic!("expected Int(42), got {other:?}"),
    }
}

#[test]
fn test_trait_impl_parameterized_enum_works_for_distinct_element_types() {
    // A single `trait X for Box(a)` impl must monomorph at each call
    // site: `Box(Int)` and `Box(String)` both dispatch to the same
    // method with no cross-pollution between their tyvars.
    let v = run(r#"
type Box(T) { Box(T) }
trait Unwrap { fn get(self) -> Int }
trait Unwrap for Box(a) {
  fn get(self) -> Int { match self { Box(inner) -> 1 } }
}
fn main() -> Int {
  let b = Box(99)
  let c = Box("hello")
  b.get() + c.get()
}
"#);
    match v {
        Value::Int(2) => {}
        other => panic!("expected Int(2), got {other:?}"),
    }
}

#[test]
fn test_trait_impl_parameterized_record_field_access_on_type_var() {
    // Pre-feature: `self.value` on a `trait X for Cell(a)` body failed
    // because self_type was Generic("Cell", []) and field lookup on a
    // zero-arity Generic of a 1-param record didn't unify.
    let v = run(r#"
type Cell(T) { value: T }
trait Peek { fn peek(self) -> Int }
trait Peek for Int { fn peek(self) -> Int { self } }
trait Peek for Cell(a) {
  fn peek(self) -> Int where a: Peek { self.value.peek() }
}
fn main() -> Int {
  let c = Cell { value: 42 }
  c.peek()
}
"#);
    match v {
        Value::Int(42) => {}
        other => panic!("expected Int(42), got {other:?}"),
    }
}

#[test]
fn test_trait_impl_parameterized_method_level_where_on_impl_binder() {
    // The critical recursive-trait-dispatch case: inner value is an
    // impl-bound tyvar, method body calls the same trait on it, and a
    // method-level `where a: Greet` makes the constraint check succeed
    // because `a` is visible in the method signature via the impl-level
    // param_map clone.
    let v = run(r#"
type Box(T) { Box(T) }
trait Greet { fn greet(self) -> String }
trait Greet for Int { fn greet(self) -> String { "int-greet" } }
trait Greet for Box(a) {
  fn greet(self) -> String where a: Greet {
    match self { Box(inner) -> inner.greet() }
  }
}
fn main() -> String {
  let b = Box(5)
  b.greet()
}
"#);
    match v {
        Value::String(s) => assert_eq!(s.as_str(), "int-greet"),
        other => panic!("expected String, got {other:?}"),
    }
}

// ── Backwards compatibility: bare-target impls ─────────────────────

#[test]
fn test_trait_impl_bare_target_still_typechecks_and_runs() {
    // Existing `trait X for Int { ... }` form must be unchanged —
    // no parser errors, no typecheck errors, runtime value correct.
    let v = run(r#"
trait Double { fn double(self) -> Int }
trait Double for Int { fn double(self) -> Int { self * 2 } }
fn main() -> Int { (5).double() }
"#);
    match v {
        Value::Int(10) => {}
        other => panic!("expected Int(10), got {other:?}"),
    }
}

#[test]
fn test_trait_impl_bare_target_on_parameterized_record_still_works() {
    // Pre-feature behaviour for impls that DON'T touch the element type
    // must be preserved — a bare `trait X for Box` on a parameterized
    // record (Generic("Box", [fresh_var]) via round-16 arity fix) still
    // compiles and runs when the method body never exposes the inner.
    let v = run(r#"
type Box(T) { value: T }
trait Tag { fn tag(self) -> String }
trait Tag for Box { fn tag(self) -> String { "box" } }
fn main() -> String {
  let b = Box { value: 42 }
  b.tag()
}
"#);
    match v {
        Value::String(s) => assert_eq!(s.as_str(), "box"),
        other => panic!("expected String, got {other:?}"),
    }
}

// ── Formatter round-trip ────────────────────────────────────────────

#[test]
fn test_format_preserves_parameterized_trait_impl_target() {
    // Source uses the `trait X for Box(a)` form; after silt fmt it
    // must still render with the `(a)` target args and remain
    // idempotent under a second pass.
    let src = "\
type Box(T) { Box(T) }

trait Wrap {
  fn unwrap(self) -> Int
}

trait Wrap for Box(a) {
  fn unwrap(self) -> Int {
    match self {
      Box(inner) -> 1
    }
  }
}
";
    let once = silt::formatter::format(src).expect("format error");
    assert!(
        once.contains("trait Wrap for Box(a)"),
        "formatter dropped target args: {once}"
    );
    let twice = silt::formatter::format(&once).expect("format error");
    assert_eq!(once, twice, "format not idempotent");
}

#[test]
fn test_format_preserves_multi_param_trait_impl_target() {
    // Two-param target — `Pair(a, b)` — round-trip through the
    // formatter with args in source order and a space after the comma.
    let src = "\
type Pair(A, B) { Pair(A, B) }

trait First {
  fn first(self) -> Int
}

trait First for Pair(a, b) {
  fn first(self) -> Int {
    match self {
      Pair(x, y) -> 1
    }
  }
}
";
    let once = silt::formatter::format(src).expect("format error");
    assert!(
        once.contains("trait First for Pair(a, b)"),
        "formatter mangled multi-param target: {once}"
    );
    let twice = silt::formatter::format(&once).expect("format error");
    assert_eq!(once, twice, "format not idempotent");
}

// ── Impl-level where clauses ────────────────────────────────────────

#[test]
fn test_trait_impl_level_where_clause_propagates_to_method_body() {
    // Impl-level `where a: Greet` must make the constraint visible
    // inside every method body in the impl, so `inner.greet()` on
    // `inner: a` dispatches via active_constraints without requiring
    // a method-level duplicate of the where clause.
    let v = run(r#"
type Box(T) { Box(T) }
trait Greet { fn greet(self) -> String }
trait Greet for Int { fn greet(self) -> String { "int-greet" } }
trait Greet for Box(a) where a: Greet {
  fn greet(self) -> String {
    match self { Box(inner) -> inner.greet() }
  }
}
fn main() -> String {
  Box(5).greet()
}
"#);
    match v {
        Value::String(s) => assert_eq!(s.as_str(), "int-greet"),
        other => panic!("expected String, got {other:?}"),
    }
}

#[test]
fn test_trait_impl_level_where_rejects_concrete_violator_at_call_site() {
    // `Box("hello").greet()` must be rejected because the impl-level
    // `where a: Greet` requires the element type to implement Greet,
    // and String does not. Before the method_table constraint plumbing
    // landed, this call was silently accepted because receiver-method
    // dispatch never consulted method_constraints — the constraint
    // only ran inside the method body, which was never re-checked per
    // call site.
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
        "expected rejection, got: {errs:?}"
    );
}

#[test]
fn test_trait_impl_level_where_accepts_conforming_call_site() {
    // Sanity check the positive path — `Box(5).greet()` passes the
    // where-clause check because Int impls Greet. This test catches
    // false positives from the call-site rejection path: a bug that
    // rejects all receiver-method calls instead of just the violating
    // ones would fail this test.
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
  Box(5).greet()
}
"#);
    match v {
        Value::String(s) => assert_eq!(s.as_str(), "int"),
        other => panic!("expected String, got {other:?}"),
    }
}

// ── Multi-trait bounds via `+` ──────────────────────────────────────

#[test]
fn test_trait_impl_level_where_with_multi_constraint_plus_syntax() {
    // `where a: Greet + Loud` must flatten to two separate (tv, trait)
    // entries sharing a type var, both propagating to method bodies
    // via active_constraints. Body uses BOTH `.greet()` and `.loud()`
    // on the inner value.
    let v = run(r#"
type Box(T) { Box(T) }
trait Greet { fn greet(self) -> String }
trait Loud { fn loud(self) -> String }
trait Greet for Int { fn greet(self) -> String { "int" } }
trait Loud for Int { fn loud(self) -> String { "INT" } }
trait Greet for Box(a) where a: Greet + Loud {
  fn greet(self) -> String {
    match self {
      Box(inner) -> "{inner.greet()}-{inner.loud()}"
    }
  }
}
fn main() -> String {
  Box(42).greet()
}
"#);
    match v {
        Value::String(s) => assert_eq!(s.as_str(), "int-INT"),
        other => panic!("expected String, got {other:?}"),
    }
}

#[test]
fn test_trait_impl_level_multi_constraint_rejects_partial_impl() {
    // If the caller's element type implements ONE of the two `+`-bound
    // traits but not the other, the call must be rejected at the
    // unsatisfied constraint. Here Int impls Greet but NOT Loud, so
    // the call should fail against the Loud half of `a: Greet + Loud`.
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
        "expected Loud rejection, got: {errs:?}"
    );
}

#[test]
fn test_trait_impl_level_where_with_comma_separated_clauses() {
    // Comma-separated form: `where a: Greet, a: Loud` must be
    // equivalent to `where a: Greet + Loud`. Both syntactic forms
    // flatten to the same (tv, trait) pairs in the parser.
    let v = run(r#"
type Box(T) { Box(T) }
trait Greet { fn greet(self) -> String }
trait Loud { fn loud(self) -> String }
trait Greet for Int { fn greet(self) -> String { "int" } }
trait Loud for Int { fn loud(self) -> String { "INT" } }
trait Greet for Box(a) where a: Greet, a: Loud {
  fn greet(self) -> String {
    match self {
      Box(inner) -> "{inner.greet()}/{inner.loud()}"
    }
  }
}
fn main() -> String {
  Box(7).greet()
}
"#);
    match v {
        Value::String(s) => assert_eq!(s.as_str(), "int/INT"),
        other => panic!("expected String, got {other:?}"),
    }
}

// ── Impl-level where clause validation ──────────────────────────────

#[test]
fn test_trait_impl_level_where_rejects_undeclared_binder() {
    // The type variable in an impl-level where clause must be declared
    // in the target type arguments. Referencing `b` when the target is
    // `Box(a)` is a hard error pointing the user at the target header.
    let errs = type_errors(
        r#"
type Box(T) { Box(T) }
trait Show { fn show(self) -> String }
trait Show for Box(a) where b: Show {
  fn show(self) -> String {
    match self { Box(inner) -> "x" }
  }
}
"#,
    );
    assert!(
        errs.iter().any(|e| {
            e.contains("'b'") && e.contains("not declared in the target type arguments")
        }),
        "expected undeclared-binder error, got: {errs:?}"
    );
}

#[test]
fn test_trait_impl_level_where_rejects_unknown_trait() {
    let errs = type_errors(
        r#"
type Box(T) { Box(T) }
trait Show { fn show(self) -> String }
trait Show for Box(a) where a: NotARealTrait {
  fn show(self) -> String {
    match self { Box(inner) -> "x" }
  }
}
"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("unknown trait 'NotARealTrait'")),
        "expected unknown-trait error, got: {errs:?}"
    );
}

// ── Method-level where on impl methods (previously silently ignored) ─

#[test]
fn test_method_level_where_on_trait_impl_method_now_enforced() {
    // Prior to this feature, register_trait_impl never consulted
    // method.where_clauses at all, so a method-level `where a: Greet`
    // was a no-op. If the method body then dispatched a method on `a`
    // that wasn't actually part of any trait, the typechecker would
    // emit a downstream error — but if the body WAS correct, the
    // constraint simply never took effect at call sites.
    //
    // After the fix, method-level where clauses attach to the method's
    // scheme and also flow through method_constraints for call-site
    // enforcement, so a call like `Box("hello").unwrap()` now fails
    // against the method-level `where a: Greet` requirement.
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
        "expected method-level where violation, got: {errs:?}"
    );
}
