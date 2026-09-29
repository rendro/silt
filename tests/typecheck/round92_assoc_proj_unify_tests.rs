//! Round 92 BROKEN: `unify` had no AssocProj × AssocProj arm.
//!
//! Two *abstract* associated-type projections (receiver still a type
//! variable under a `where` bound) fell into the `_` catch-all of the
//! `unify` match and produced a self-identical mismatch like
//! `expected <_ as Stream>::Item, got <_ as Stream>::Item` — i.e. an
//! abstract projection failed to unify even with itself. The documented
//! contract (src/types/mod.rs, `Type::AssocProj`) says two abstract
//! projections unify iff they have the same receiver, trait_name, and
//! assoc_name.
//!
//! These tests lock:
//!   (a) typecheck-accepts for the broken shapes (projection return
//!       type, list of two identical projections, receiver-unifying
//!       projections across two type vars),
//!   (b) the full compile + RUN path through the VM via the in-process
//!       runner (typecheck-only locks for this bug class have produced
//!       runtime regressions in past rounds),
//!   (c) negatives: projections with a different assoc name, a
//!       different trait, or non-unifiable receivers are still
//!       rejected with a real (non-self-identical) mismatch.

use std::time::Duration;

use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::scheduler::test_support::InProcessRunner;
use silt::typechecker;
use silt::types::Severity;
use silt::value::Value;

fn type_errors(input: &str) -> Vec<String> {
    let tokens = Lexer::new(input).tokenize().expect("lexer");
    let mut program = Parser::new(tokens).parse_program().expect("parse");
    typechecker::check(&mut program)
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

fn assert_ok(src: &str) {
    let errs = type_errors(src);
    assert!(
        errs.is_empty(),
        "expected no errors, got:\n{}",
        errs.join("\n")
    );
}

fn assert_rejected(src: &str, needle: &str) {
    let errs = type_errors(src);
    assert!(
        errs.iter().any(|m| m.contains(needle)),
        "expected error containing {needle:?}; got:\n{}",
        errs.join("\n")
    );
}

fn run(src: &str) -> Option<Value> {
    let runner = InProcessRunner::new(src).with_budget(Duration::from_secs(5));
    let outcome = runner.run_trial();
    assert!(outcome.ok(), "run should succeed: {outcome:?}");
    outcome.result
}

/// Shared program prelude: a trait with one associated type and a
/// concrete impl binding it to Int.
const STREAM_PRELUDE: &str = r#"
trait Stream {
  type Item
  fn first(self) -> Self::Item
}

type Wrap { v: Int }

trait Stream for Wrap {
  type Item = Int
  fn first(self) -> Int { self.v }
}
"#;

// ── (a) Typecheck-accepts ────────────────────────────────────────────

/// The primary broken shape: a generic fn whose declared return type is
/// the abstract projection `<a as Stream>::Item` and whose body returns
/// a method call typed as that same projection. Before the fix the two
/// identical projections failed to unify with each other
/// (`expected <_ as Stream>::Item, got <_ as Stream>::Item`).
#[test]
fn abstract_projection_unifies_with_itself_in_return_position() {
    assert_ok(&format!(
        "{STREAM_PRELUDE}
fn grab(s: a) -> <a as Stream>::Item where a: Stream {{
  s.first()
}}

fn main() {{
  println(grab(Wrap {{ v: 5 }}))
}}
"
    ));
}

/// Two calls producing the same abstract projection in one list
/// literal: the element-type unification goes projection-vs-projection.
#[test]
fn identical_projections_unify_in_list_literal() {
    assert_ok(&format!(
        "{STREAM_PRELUDE}
fn pair(s: a) where a: Stream {{
  let xs = [s.first(), s.first()]
  xs
}}

fn main() {{
  println(pair(Wrap {{ v: 3 }}))
}}
"
    ));
}

/// Receivers may themselves need unification: projections over two
/// *different* type variables (`a` and `b`, both Stream-bound) unify by
/// unifying the receivers, not by structural identity.
#[test]
fn projections_over_distinct_tyvars_unify_receivers() {
    assert_ok(&format!(
        "{STREAM_PRELUDE}
fn pick(x: a, y: b) where a: Stream, b: Stream {{
  let xs = [x.first(), y.first()]
  xs
}}

fn main() {{
  println(pick(Wrap {{ v: 1 }}, Wrap {{ v: 2 }}))
}}
"
    ));
}

// ── (b) Full compile + RUN path ──────────────────────────────────────

/// End-to-end: the generic projection-returning fn must not just
/// typecheck — the compiler must bind it and the VM must dispatch
/// `first` through the Wrap impl and hand back the bound Int.
#[test]
fn projection_return_runs_through_vm() {
    let src = format!(
        "{STREAM_PRELUDE}
fn grab(s: a) -> <a as Stream>::Item where a: Stream {{
  s.first()
}}

fn main() {{
  grab(Wrap {{ v: 5 }})
}}
"
    );
    assert_eq!(run(&src), Some(Value::Int(5)));
}

/// End-to-end: the list-of-projections shape runs and produces the
/// expected concrete list at runtime.
#[test]
fn projection_list_runs_through_vm() {
    let src = format!(
        "{STREAM_PRELUDE}
fn pair(s: a) where a: Stream {{
  [s.first(), s.first()]
}}

fn main() {{
  pair(Wrap {{ v: 3 }})
}}
"
    );
    match run(&src) {
        Some(Value::List(items)) => {
            assert_eq!(items.as_slice(), &[Value::Int(3), Value::Int(3)]);
        }
        other => panic!("expected List([3, 3]), got {other:?}"),
    }
}

// ── (c) Negatives: real mismatches still rejected ────────────────────

/// Same trait, different assoc names: `<a as Pair>::First` vs
/// `<a as Pair>::Second` is a genuine mismatch.
#[test]
fn different_assoc_names_rejected() {
    assert_rejected(
        r#"
trait Pair {
  type First
  type Second
  fn first(self) -> Self::First
  fn second(self) -> Self::Second
}

fn bad(s: a) -> <a as Pair>::First where a: Pair {
  s.second()
}

fn main() {
  println("x")
}
"#,
        "type mismatch: expected <_ as Pair>::First, got <_ as Pair>::Second",
    );
}

/// Same assoc name through different traits: `<a as A>::Item` vs
/// `<a as B>::Item` is a genuine mismatch.
#[test]
fn different_trait_names_rejected() {
    assert_rejected(
        r#"
trait A {
  type Item
  fn ai(self) -> Self::Item
}

trait B {
  type Item
  fn bi(self) -> Self::Item
}

fn bad(s: a) -> <a as A>::Item where a: A + B {
  s.bi()
}

fn main() {
  println("x")
}
"#,
        "type mismatch: expected <_ as A>::Item, got <_ as B>::Item",
    );
}

/// Non-unifiable receivers: nested projections whose receivers are
/// themselves distinct abstract projections (`<a as Meta>::T` vs
/// `<a as Meta>::U`). The arm recurses into the receivers, which
/// mismatch on assoc name.
#[test]
fn non_unifiable_receivers_rejected() {
    assert_rejected(
        r#"
trait Meta {
  type T
  type U
  fn t(self) -> Self::T
  fn u(self) -> Self::U
}

trait Stream {
  type Item
  fn first(self) -> Self::Item
}

fn bad(x: <<a as Meta>::T as Stream>::Item) -> <<a as Meta>::U as Stream>::Item where a: Meta {
  x
}

fn main() {
  println("x")
}
"#,
        "type mismatch: expected <_ as Meta>::U, got <_ as Meta>::T",
    );
}

// ── (d) Operand-validity parity (top-level round-92 follow-up) ───────

/// `==` on two abstract projections: after the unify fix the two sides
/// unify, but `is_valid_compare_operand` / `is_valid_arith_operand`
/// (src/typechecker/inference.rs) also had to learn that an abstract
/// `AssocProj` is "maybe valid" like a `Type::Var` — otherwise the
/// deferred operator check rejected it with "operator '==' requires a
/// comparable type, got '<_ as Stream>::Item'". Full run-path lock.
#[test]
fn projection_equality_runs_through_vm() {
    let result = run(&format!(
        "{STREAM_PRELUDE}
fn same(s: a) -> Bool where a: Stream {{
  let x = s.first()
  let y = s.first()
  x == y
}}

fn main() {{
  same(Wrap {{ v: 3 }})
}}
"
    ));
    assert_eq!(result, Some(Value::Bool(true)));
}

/// Arithmetic parity: `+` on two abstract projections defers the same
/// way and the concrete check happens at the instantiation boundary.
#[test]
fn projection_arithmetic_runs_through_vm() {
    let result = run(&format!(
        "{STREAM_PRELUDE}
fn add_two(s: a) where a: Stream {{
  s.first() + s.first()
}}

fn main() {{
  add_two(Wrap {{ v: 4 }})
}}
"
    ));
    assert_eq!(result, Some(Value::Int(8)));
}
