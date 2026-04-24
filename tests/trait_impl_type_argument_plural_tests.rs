//! Round 61: `(s)` pluralization fix for the trait-impl arity diagnostic.
//!
//! `register_trait_impl` emits an error when the arity of
//! `trait Foo(a, b) for T` does not match the trait declaration's
//! parameter count. The pre-fix wording was
//!   "trait 'Foo' expects 2 type argument(s), got 1 in impl for 'T'"
//! which forced every N=1 case to read "1 type argument(s)". This
//! file locks the post-fix wording using the `plural()` helper — the
//! same helper already used by seven other typechecker arity sites
//! (round-17 F5) and the compiler site (round-18).
//!
//! Covers both singular (N=1) and plural (N>=2) forms at the impl
//! site. The where-clause bound site has its own lock in
//! `tests/where_clause_bound_arity_tests.rs`.

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

/// Impl supplies 0 args where the trait declares 1. Error uses
/// singular: "expects 1 type argument".
#[test]
fn test_impl_arity_mismatch_one_expected_uses_singular() {
    let errs = type_errors(
        r#"
trait Cast(to) {
  fn cast(self) -> to
}
trait Cast for Int {
  fn cast(self) -> Float { 1.0 }
}
fn main() { }
"#,
    );
    let joined = errs.join("\n");
    assert!(
        joined.contains("trait 'Cast' expects 1 type argument, got 0 in impl for 'Int'"),
        "expected singular form, got:\n{joined}"
    );
}

/// Impl supplies 0 args where the trait declares 2. Error uses
/// plural: "expects 2 type arguments".
#[test]
fn test_impl_arity_mismatch_two_expected_uses_plural() {
    let errs = type_errors(
        r#"
trait Convert(from, to) {
  fn convert(self) -> to
}
trait Convert for Int {
  fn convert(self) -> Float { 1.0 }
}
fn main() { }
"#,
    );
    let joined = errs.join("\n");
    assert!(
        joined.contains("trait 'Convert' expects 2 type arguments, got 0 in impl for 'Int'"),
        "expected plural form, got:\n{joined}"
    );
}
