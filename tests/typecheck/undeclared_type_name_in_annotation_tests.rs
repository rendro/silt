//! Round 60 B3 regression lock.
//!
//! `resolve_type_expr`'s uppercase-unknown-name arm previously returned
//! `Type::Generic(name, vec![])` as a ghost type when the annotation
//! referenced something that was neither a declared record nor a
//! declared enum. That ghost type then cascaded into
//! `"does not implement Display"` and `"type mismatch: expected
//! Frobnitz, got Int"` diagnostics far from the annotation site.
//!
//! The round-23 fix (see `trait_impl_undeclared_target_tests.rs`)
//! applied a whitelist at trait-impl targets only; fn params, fn
//! returns, let ascriptions, and expression ascriptions all used the
//! same `resolve_type_expr` path and still silently accepted ghost
//! types. This lock mirrors the whitelist into `resolve_type_expr` so
//! every type-annotation position rejects unknown uppercase names at
//! the annotation's own span.

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

/// Fn parameter: `fn foo(x: Frobnitz)` where `Frobnitz` is undeclared.
#[test]
fn test_unknown_type_in_fn_param_annotation() {
    let errs = type_errors(
        r#"
fn foo(x: Frobnitz) -> Int { 0 }
fn main() { }
"#,
    );
    let joined = errs.join("\n");
    assert!(
        joined.contains("unknown type 'Frobnitz'"),
        "expected 'unknown type Frobnitz' diagnostic, got:\n{joined}"
    );
    assert!(
        !joined.contains("does not implement"),
        "should not cascade into 'does not implement' noise, got:\n{joined}"
    );
}

/// Fn return type: `fn foo() -> Frobnitz`.
#[test]
fn test_unknown_type_in_fn_return_annotation() {
    let errs = type_errors(
        r#"
fn foo() -> Frobnitz { 0 }
fn main() { }
"#,
    );
    let joined = errs.join("\n");
    assert!(
        joined.contains("unknown type 'Frobnitz'"),
        "expected 'unknown type Frobnitz' diagnostic, got:\n{joined}"
    );
}

/// `let x: Frobnitz = ...`.
#[test]
fn test_unknown_type_in_let_ascription() {
    let errs = type_errors(
        r#"
fn main() {
    let x: Frobnitz = 1
}
"#,
    );
    let joined = errs.join("\n");
    assert!(
        joined.contains("unknown type 'Frobnitz'"),
        "expected 'unknown type Frobnitz' diagnostic, got:\n{joined}"
    );
    assert!(
        !joined.contains("does not implement"),
        "should not cascade into 'does not implement' noise, got:\n{joined}"
    );
}

/// Generic form `fn foo(x: Frobnitz(Int))` (undeclared with type args).
#[test]
fn test_unknown_generic_type_in_annotation() {
    let errs = type_errors(
        r#"
fn foo(x: Frobnitz(Int)) -> Int { 0 }
fn main() { }
"#,
    );
    let joined = errs.join("\n");
    assert!(
        joined.contains("unknown type 'Frobnitz'"),
        "expected 'unknown type Frobnitz' diagnostic, got:\n{joined}"
    );
}

/// Sanity: a declared record must continue to work in param annotations
/// (the whitelist accepts user records).
#[test]
fn test_declared_record_in_fn_param_still_works() {
    let errs = type_errors(
        r#"
type Point { x: Int, y: Int }
fn read(p: Point) -> Int { p.x }
fn main() { }
"#,
    );
    assert!(
        errs.is_empty(),
        "declared record in param annotation should typecheck, got:\n{}",
        errs.join("\n")
    );
}
