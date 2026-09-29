//! Regression tests for the "trait args verification" soundness fix
//! (round 58). Before the fix, `where a: TryInto(Int)` verification in
//! `src/typechecker/mod.rs::verify_trait_obligation` took only
//! `trait_name: Symbol` — the trait arguments carried by the bound were
//! dropped on the floor. As a result, `where a: TryInto(Int)` against a
//! `trait TryInto(Float) for String` impl silently typechecked, and
//! `convert("hi")` with signature `-> Result(Int, String)` returned a
//! `Result(Float, String)` at runtime — a soundness hole.
//!
//! After the fix, the bound's trait args are threaded through
//! `verify_trait_obligation` and compared positionally against the
//! matched impl's registered trait args (via a new `impl_trait_args`
//! map). Concrete mismatches reject with a diagnostic that names both
//! the bound and the matched impl.
//!
//! Lives in its own file to avoid edit collisions with the broader test
//! coverage work happening in parallel.

use silt::typechecker;
use silt::types::Severity;

fn type_errors(input: &str) -> Vec<String> {
    let tokens = silt::lexer::Lexer::new(input)
        .tokenize()
        .expect("lexer error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    let errors = typechecker::check(&mut program);
    errors
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

/// The repro that pins the soundness fix. The impl is `TryInto(Float) for
/// String` — it produces `Result(Float, String)`. The bound is
/// `where a: TryInto(Int)` — it promises `Result(Int, String)`. Unifying
/// `a = String` finds a matching `(TryInto, String)` impl but its args
/// don't match. Must reject.
#[test]
fn where_clause_trait_args_must_match_impl_trait_args() {
    let src = r#"
trait TryInto(b) { fn try_into(self) -> Result(b, String) }
trait TryInto(Float) for String { fn try_into(self) = Ok(1.5) }
fn convert(s: a) -> Result(Int, String) where a: TryInto(Int) { s.try_into() }
fn main() { println(convert("hi")) }
"#;
    let errs = type_errors(src);
    // Must reject with a diagnostic that mentions either TryInto(Int)
    // or the "does not implement" fragment. Both are acceptable — the
    // key property is that no clean typecheck passes.
    assert!(
        !errs.is_empty(),
        "trait args mismatch MUST reject; got no errors"
    );
    assert!(
        errs.iter().any(|e| {
            (e.contains("TryInto(Int)") || e.contains("TryInto") && e.contains("Int"))
                && (e.contains("does not implement") || e.contains("TryInto(Float)"))
        }),
        "expected 'does not implement TryInto(Int)' (or similar) citing \
         both the bound's args and the impl's args; got: {errs:?}"
    );
}

/// Matching trait args must still typecheck cleanly. The bound is
/// `where a: TryInto(Float)` and the impl is `TryInto(Float) for String`
/// — args agree, obligation satisfied.
#[test]
fn matching_trait_args_typechecks_cleanly() {
    let src = r#"
trait TryInto(b) { fn try_into(self) -> Result(b, String) }
trait TryInto(Float) for String { fn try_into(self) = Ok(1.5) }
fn convert(s: a) -> Result(Float, String) where a: TryInto(Float) { s.try_into() }
fn main() { println(convert("hi")) }
"#;
    let errs = type_errors(src);
    assert!(
        errs.is_empty(),
        "matching trait args must typecheck cleanly, got: {errs:?}"
    );
}

/// Parameterless traits keep the fast path — an empty bound_trait_args
/// slice skips the impl_trait_args check. This pins that behaviour so
/// the round 58 fix doesn't accidentally break ordinary `where a: Display`.
#[test]
fn parameterless_where_clause_fast_path_unchanged() {
    let src = r#"
trait MyDisplay { fn my_display(self) -> String }
trait MyDisplay for Int { fn my_display(self) -> String { "int" } }
fn show(x: a) -> String where a: MyDisplay { x.my_display() }
fn main() { println(show(42)) }
"#;
    let errs = type_errors(src);
    assert!(
        errs.is_empty(),
        "parameterless where-clause must still typecheck cleanly, got: {errs:?}"
    );
}
