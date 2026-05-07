//! Round 76 BROKEN T1 lock: self-referencing assoc-type binding must
//! be rejected with a clear typechecker error and must NOT cause a
//! stack overflow at compile time.
//!
//! Pre-fix: the binding `type Item = <Int as Container>::Item` stored
//! a self-referential `AssocProj` in the assoc-binding registry. Any
//! later canonicalisation of `<Int as Container>::Item` would look up
//! the binding, recurse on the same `AssocProj`, look up again, and
//! recurse forever — `cargo run -- check` exited with "fatal runtime
//! error: stack overflow, aborting".
//!
//! Post-fix: `Resolver::register_assoc_binding` walks the
//! canonicalised RHS for any `AssocProj` that closes back on the
//! triple under registration (or another already-registered triple)
//! and refuses insertion, returning an `AssocBindingCycle` the
//! typechecker turns into a "self-referential" error. The runtime
//! path also stays alive: a subprocess `silt check` must exit
//! cleanly (non-stack-overflow) on the broken input.

use std::time::Duration;

use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::scheduler::test_support::InProcessRunner;
use silt::typechecker;
use silt::types::Severity;

fn type_errors(input: &str) -> Vec<String> {
    let tokens = Lexer::new(input).tokenize().expect("lexer");
    let mut program = Parser::new(tokens).parse_program().expect("parse");
    typechecker::check(&mut program)
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

const T1_DIRECT_CYCLE: &str = r#"
trait Container {
  type Item
  fn unwrap(self) -> Self::Item
}

trait Container for Int {
  type Item = <Int as Container>::Item
  fn unwrap(self) -> <Int as Container>::Item = self
}

fn main() {
  let x: Int = 7
  println(x.unwrap())
}
"#;

/// Direct self-reference: typechecker must reject with a
/// "self-referential" error. Must NOT loop / stack-overflow.
#[test]
fn t1_direct_self_reference_rejected_with_error() {
    let errs = type_errors(T1_DIRECT_CYCLE);
    assert!(
        errs.iter().any(|m| m.contains("self-referential")
            && m.contains("Container")
            && m.contains("Item")),
        "expected self-referential assoc-type error mentioning Container::Item; got:\n{}",
        errs.join("\n")
    );
}

/// Same input run through the in-process runner: the program must
/// fail to typecheck (so the run does not actually execute), but the
/// process must NOT stack-overflow. Pre-fix this aborted with
/// "fatal runtime error: stack overflow".
#[test]
fn t1_direct_self_reference_no_stack_overflow_at_runtime_path() {
    let runner = InProcessRunner::new(T1_DIRECT_CYCLE).with_budget(Duration::from_secs(5));
    let outcome = runner.run_trial();
    // We don't assert success — typecheck rejects it. We assert the
    // runner returned a verdict at all (i.e. did not abort the
    // process). The Outcome being constructible is the witness; if
    // the run aborted via stack overflow the test process would
    // crash before reaching here.
    let _ = outcome;
}

/// Mutual cycle through two distinct trait/assoc pairs:
/// `Foo::T = <Int as Bar>::S; Bar::S = <Int as Foo>::T`. The cycle
/// detector must catch the second registration even though neither
/// binding is directly self-referential.
#[test]
fn t1_mutual_cycle_through_assoc_projections_rejected() {
    let src = r#"
trait Foo {
  type T
}

trait Bar {
  type S
}

trait Bar for Int {
  type S = <Int as Foo>::T
}

trait Foo for Int {
  type T = <Int as Bar>::S
}

fn main() {
  println("ok")
}
"#;
    let errs = type_errors(src);
    assert!(
        errs.iter().any(|m| m.contains("self-referential")),
        "expected mutual-cycle assoc-type error; got:\n{}",
        errs.join("\n")
    );
}
