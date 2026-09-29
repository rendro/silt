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
    // Three positive locks, in decreasing order of "directness":
    //
    // 1. The runner returned a verdict at all — reaching this line at
    //    all is the primary witness that the process didn't abort via
    //    `fatal runtime error: stack overflow`, which is the
    //    regression we're guarding.
    // 2. The trial did not time out. Pre-fix, the typecheck step
    //    looped forever on the self-referential `AssocProj`; a
    //    timeout here would mean we re-introduced the looping shape
    //    (just not the stack-overflow variant).
    // 3. If the runner produced any error message, it must not be a
    //    stack-overflow-ish string. The in-process runner catches VM
    //    panics into `error_message` — a recursive shape that
    //    overflowed inside the worker thread would surface here as a
    //    `panic in vm thread: ...stack overflow...` rather than
    //    aborting the test process. Either form (clean error,
    //    successful Value, or non-overflow error) is acceptable; the
    //    regression we lock against is specifically the recursive
    //    blow-up.
    assert!(
        !outcome.timed_out,
        "self-referential assoc-type compile path timed out — \
         the typecheck/compile path may have re-introduced a \
         non-terminating recursion (pre-fix shape, just without \
         the explicit stack overflow). elapsed={:?}",
        outcome.elapsed
    );
    if let Some(msg) = &outcome.error_message {
        let lower = msg.to_ascii_lowercase();
        assert!(
            !lower.contains("stack overflow"),
            "self-referential assoc-type lowering caught a panic \
             whose message mentions stack overflow — the recursive \
             AssocProj shape is back. error_message={msg}"
        );
    }
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
