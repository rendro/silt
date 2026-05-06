//! Round 74 Fix #3 lock: an alias target containing a free type
//! variable that is not declared in the alias header is rejected at
//! typecheck with a clear, identifier-named diagnostic.
//!
//! Pre-fix bug (`src/typechecker/mod.rs::register_type_alias` at line
//! ~3923-3993):
//!   The function did not validate that every lowercase tyvar in the
//!   resolved `target_ty` appeared in `td.params`. `resolve_type_expr_inner`
//!   lazily allocates one fresh `Type::Var` per encountered lowercase
//!   identifier (and inserts into `param_vars`). For
//!   `type AnyList = List(a)` (no `(a)` after `AnyList`), the `a`
//!   was a free tyvar — and the alias stored it as part of its
//!   target. Every use site (`let xs: AnyList = ...`) produced
//!   `List(<the-shared-tyvar>)`. Two distinct uses then unified the
//!   shared tyvar with two distinct element types, surfacing
//!   "type mismatch: expected Int, got String" on the second use —
//!   far from the actual bug (the missing `(a)` on the alias header).
//!
//! Post-fix: a snapshot of declared param names is taken before
//! resolving `target_te`. After resolution, any names `param_vars`
//! gained that aren't in the snapshot are reported as undeclared
//! type parameters, with a hint suggesting the corrected header.

use silt::types::Severity;

fn type_errors(input: &str) -> Vec<String> {
    let tokens = silt::lexer::Lexer::new(input)
        .tokenize()
        .expect("lexer error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    let errors = silt::typechecker::check(&mut program);
    errors
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

/// `type AnyList = List(a)` — `a` is undeclared. Pre-fix: silently
/// accepted with a shared tyvar leaking across uses. Post-fix:
/// rejected with a clear diagnostic naming `a` and suggesting the
/// fix.
#[test]
fn alias_with_undeclared_free_tyvar_is_rejected() {
    let errs = type_errors(
        r#"
type AnyList = List(a)
fn main() {
    let xs: AnyList = [1, 2, 3]
    let ys: AnyList = ["a", "b"]
    println(xs)
    println(ys)
}
"#,
    );
    assert!(
        !errs.is_empty(),
        "expected `type AnyList = List(a)` (undeclared `a`) to be rejected at typecheck; \
         got no errors"
    );
    let has_undeclared = errs.iter().any(|m| {
        m.contains("undeclared type parameter") && m.contains('a') && m.contains("AnyList")
    });
    assert!(
        has_undeclared,
        "expected an `undeclared type parameter 'a' in alias target` diagnostic naming \
         `AnyList`; got: {errs:?}"
    );
}

/// Multiple undeclared tyvars produce one diagnostic each (sorted for
/// stable output).
#[test]
fn alias_with_two_undeclared_free_tyvars_emits_two_diagnostics() {
    let errs = type_errors(
        r#"
type Pairish = Map(k, v)
fn main() {}
"#,
    );
    let undeclared_count = errs
        .iter()
        .filter(|m| m.contains("undeclared type parameter"))
        .count();
    assert!(
        undeclared_count >= 2,
        "expected two `undeclared type parameter` diagnostics (one per missing name); \
         got {undeclared_count}: {errs:?}"
    );
}

/// Properly declared tyvars are accepted (sanity check that the fix
/// doesn't break the happy path).
#[test]
fn alias_with_declared_tyvar_typechecks() {
    let errs = type_errors(
        r#"
type AnyList(a) = List(a)
fn main() {
    let xs: AnyList(Int) = [1, 2, 3]
    println(xs)
}
"#,
    );
    assert!(
        errs.is_empty(),
        "properly declared `type AnyList(a) = List(a)` should typecheck; got: {errs:?}"
    );
}
