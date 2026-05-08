//! Round 76 BROKEN T2 lock: `validate_trait_impls` must substitute
//! the impl's `trait_args` into the trait method's signature template
//! before alpha-renaming the method-polymorphic vars.
//!
//! Pre-fix: `trait Foo(a) { fn produce(self) -> a }` plus
//! `trait Foo(Int) for String { fn produce(self) -> String = "haha" }`
//! typechecked silently. The trait's parameter tyvar `a` was
//! alpha-renamed to a fresh var (treated as method-polymorphic),
//! which then unified with the impl's `String`. The where-clause
//! `where a: Foo(Int)` site got back `String` for the produced type
//! and unified with the call-site's `Int` slot, all without an
//! error. Runtime printed the literal `"haha"` into an `Int` slot —
//! a soundness hole.
//!
//! Post-fix: pre-substituting `[a -> Int]` (from `Foo(Int)`) before
//! alpha-renaming pins the produced type to `Int`, so the impl's
//! `-> String` correctly fails to unify with the trait's `-> Int`,
//! emitting a typed "expected Int, got String" diagnostic.

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

const T2_BROKEN_PROGRAM: &str = r#"
trait Foo(a) { fn produce(self) -> a }
trait Foo(Int) for String { fn produce(self) -> String = "haha" }
fn use_foo(s: a) -> Int where a: Foo(Int) {
  s.produce()
}
fn main() {
  let s: String = "hi"
  let r: Int = use_foo(s)
  println(r)
}
"#;

/// The previously-broken program must now be REJECTED at typecheck.
/// We accept either the direct method-signature mismatch ("expected
/// Int, got String") or the equivalent "expected ... String, got ...
/// Int" depending on argument order — the lock is on REJECTION, not
/// the exact wording.
#[test]
fn t2_parametric_trait_impl_signature_mismatch_rejected() {
    let errs = type_errors(T2_BROKEN_PROGRAM);
    assert!(
        errs.iter().any(|m| {
            let has_kw = m.contains("type mismatch") || m.contains("expected");
            let mentions_both =
                (m.contains("Int") && m.contains("String")) || m.contains("produce");
            has_kw && mentions_both
        }),
        "expected a type-mismatch error linking Int and String (or method 'produce'); got:\n{}",
        errs.join("\n")
    );
}

/// A correctly-typed impl of the same parametric trait must still
/// typecheck — the fix should only narrow the signature check, not
/// reject programs that already correctly match.
#[test]
fn t2_parametric_trait_impl_matching_signature_typechecks() {
    let src = r#"
trait Foo(a) { fn produce(self) -> a }
trait Foo(Int) for String { fn produce(self) -> Int = 42 }
fn use_foo(s: a) -> Int where a: Foo(Int) {
  s.produce()
}
fn main() {
  let s: String = "hi"
  let r: Int = use_foo(s)
  println(r)
}
"#;
    let errs = type_errors(src);
    assert!(
        errs.is_empty(),
        "matching parametric impl should typecheck; got:\n{}",
        errs.join("\n")
    );
}

/// Runtime witness: the previously-broken program produced literal
/// `"haha"` (a String) into an Int-typed slot via the in-process
/// runner. Production CLI now refuses to run it (`silt run` exits 1
/// on typecheck errors via `compile_file_with_options`); this test
/// runs through the in-process runner — which deliberately ignores
/// typecheck errors to exercise the VM — so we instead assert the
/// stdout *no longer* contains the unsound "haha" output. With the
/// fix, the unification failure may either propagate into a
/// compile/VM error or short-circuit method dispatch; either way,
/// the literal string written by the impl must NOT appear in stdout
/// of a run that pretends the program is well-typed.
#[test]
fn t2_broken_program_does_not_silently_print_haha() {
    let runner = InProcessRunner::new(T2_BROKEN_PROGRAM).with_budget(Duration::from_secs(5));
    let outcome = runner.run_trial();
    assert!(
        !outcome.stdout.contains("haha"),
        "post-fix the unsound program must not write \"haha\" to stdout; got stdout={:?}, error={:?}",
        outcome.stdout,
        outcome.error_message
    );
}
