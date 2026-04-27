//! Regression test for audit finding BROKEN #1: user-defined
//! `type TypeOf(a)` collided with the internal type-descriptor sentinel.
//!
//! The typechecker uses `Type::Generic(intern("TypeOf"), [..])` as the
//! structural shape of a first-class type-descriptor value (what you get
//! when you pass `Employee` as a `type a` argument to `json.parse`). A
//! user-declared `type TypeOf(a) { Foo(a) }` used to bind `Foo` as a
//! constructor returning exactly that sentinel shape, so passing
//! `Foo(42)` into a type-descriptor slot typechecked silently and then
//! blew up at runtime with "type argument must be a record type".
//!
//! The fix is a front-of-function guard in
//! `TypeCheckContext::register_type_decl` that rejects the declaration
//! with a reserved-name diagnostic. This test locks that behavior.

use silt::parser::Parser;
use silt::typechecker;
use silt::types::Severity;

/// Run the type checker and return hard-error messages only.
fn type_errors(input: &str) -> Vec<String> {
    let tokens = silt::lexer::Lexer::new(input)
        .tokenize()
        .expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let errors = typechecker::check(&mut program);
    errors
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

#[test]
fn typeof_is_rejected_as_user_type_name() {
    // Exact reproducer from the audit. Before the fix this program
    // typechecked silently (constructor `Foo` was bound as
    // `forall a. a -> TypeOf(a)`, which unifies against the internal
    // type-descriptor slot used by e.g. `json.parse(..., T)`), and the
    // failure only surfaced as a VM runtime error later. After the fix
    // the declaration itself is rejected with a clear diagnostic.
    let src = r#"
type TypeOf(a) { Foo(a) }
fn main() {}
"#;
    let errs = type_errors(src);

    // Use a single, unique substring assertion (no OR-chain): the exact
    // phrase emitted by the reserved-name guard. If the guard is removed
    // or the message is changed, this test breaks loudly.
    assert!(
        errs.iter()
            .any(|e| e.contains("'TypeOf' is a reserved type name used by the type system")),
        "expected reserved-name diagnostic for `type TypeOf(..)`, got: {errs:?}"
    );
}

#[test]
fn typeof_rejected_also_without_params() {
    // Non-generic shape must be rejected too — the sentinel head is
    // just the name `TypeOf`, irrespective of arity. This locks that
    // the guard fires on the name match, not on the `Generic(..)` arity.
    let src = r#"
type TypeOf { Foo }
fn main() {}
"#;
    let errs = type_errors(src);
    assert!(
        errs.iter()
            .any(|e| e.contains("'TypeOf' is a reserved type name")),
        "expected reserved-name diagnostic for zero-arg `type TypeOf`, got: {errs:?}"
    );
}
