//! Round 60 G1 regression lock.
//!
//! `verify_trait_obligation` fast-paths the empty-args case at
//! `mod.rs:1043` because bare-name supertrait sub-obligations
//! legitimately pass `&[]`. But this allowed a user-written where
//! clause like `where a: Cast` (for a `trait Cast(to)`) to silently
//! typecheck — the arity mismatch was swallowed by the fast path.
//!
//! The fix detects this at the where-clause registration site
//! (`inference.rs:474`-ish, `check_fn_body_with_name`): when the
//! trait has params but the bound supplies zero trait_args, emit a
//! specific "expects N type argument(s) in bound, got 0" error.
//!
//! Round 61: the `(s)` pluralization was swapped to the `plural()`
//! helper so the diagnostic reads "1 type argument" / "2 type
//! arguments" instead of "1 type argument(s)".

use silt::lexer::Lexer;
use silt::parser::Parser;
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

/// Repro: `where a: Cast` (bare, no args) against a parameterised
/// `trait Cast(to)` used to typecheck silently. Post-fix: arity error.
#[test]
fn test_where_clause_bare_bound_on_parameterized_trait_rejected() {
    let errs = type_errors(
        r#"
trait Cast(to) {
  fn cast(self) -> to
}
fn g(x: a) -> Float where a: Cast { 1.0 }
fn main() { }
"#,
    );
    let joined = errs.join("\n");
    assert!(
        joined.contains("trait 'Cast' expects 1 type argument in bound, got 0"),
        "expected arity-in-bound error (singular), got:\n{joined}"
    );
}

/// Counterpart: matching arity in bound still typechecks.
#[test]
fn test_where_clause_matching_arity_accepted() {
    let errs = type_errors(
        r#"
trait Cast(to) {
  fn cast(self) -> to
}
trait Cast(Float) for Int {
  fn cast(self) -> Float { 1.0 }
}
fn g(x: a) -> Float where a: Cast(Float) { x.cast() }
fn main() {
  let _ = g(42)
}
"#,
    );
    assert!(
        errs.is_empty(),
        "matching-arity where bound should typecheck, got:\n{}",
        errs.join("\n")
    );
}

/// Plural form: a two-parameter trait referenced bare in a where
/// clause renders "2 type arguments" (not "2 type argument(s)").
#[test]
fn test_where_clause_bare_bound_two_param_trait_uses_plural() {
    let errs = type_errors(
        r#"
trait Convert(from, to) {
  fn convert(self) -> to
}
fn g(x: a) -> Float where a: Convert { 1.0 }
fn main() { }
"#,
    );
    let joined = errs.join("\n");
    assert!(
        joined.contains("trait 'Convert' expects 2 type arguments in bound, got 0"),
        "expected arity-in-bound error (plural), got:\n{joined}"
    );
}

/// Parameterless traits in where clauses remain unaffected.
#[test]
fn test_where_clause_parameterless_trait_unaffected() {
    let errs = type_errors(
        r#"
trait Display2 {
  fn display2(self) -> String
}
fn g(x: a) -> String where a: Display2 { x.display2() }
fn main() { }
"#,
    );
    assert!(
        errs.is_empty(),
        "parameterless trait in where bound should not trigger the arity check, got:\n{}",
        errs.join("\n")
    );
}
