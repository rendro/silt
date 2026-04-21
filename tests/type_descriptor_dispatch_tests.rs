//! Trait-method dispatch on type descriptors (`Int.default()`,
//! `a.default()` where `a: TypeOf(_)`).

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

fn assert_ok(src: &str) {
    let errs = type_errors(src);
    assert!(errs.is_empty(), "expected no errors, got:\n{}", errs.join("\n"));
}

#[test]
fn default_via_type_descriptor_generic() {
    // The aspirational example from generics.md — `default` as a real,
    // user-writable function.
    let src = r#"
        trait Default {
            fn default() -> Self
        }

        trait Default for Int {
            fn default() -> Self { 0 }
        }

        fn default(type a) -> a where a: Default {
            a.default()
        }

        fn use_it() {
            let _ = default(Int)
        }
    "#;
    assert_ok(src);
}

#[test]
fn default_via_type_descriptor_concrete() {
    // Direct `Int.default()` with no generic wrapping.
    let src = r#"
        trait Default {
            fn default() -> Self
        }

        trait Default for Int {
            fn default() -> Self { 0 }
        }

        fn use_it() {
            let _ = Int.default()
        }
    "#;
    assert_ok(src);
}

#[test]
fn monoid_combine_dispatches_via_type() {
    // Multi-arg trait method invoked on a descriptor. Neither Self-typed
    // parameter is a value-receiver; both come from the explicit args.
    let src = r#"
        trait Monoid {
            fn empty() -> Self
            fn combine(a: Self, b: Self) -> Self
        }

        trait Monoid for Int {
            fn empty() -> Self { 0 }
            fn combine(a: Self, b: Self) -> Self { a + b }
        }

        fn use_it() {
            let e = Int.empty()
            let c = Int.combine(1, 2)
        }
    "#;
    assert_ok(src);
}

#[test]
fn missing_where_clause_rejects_descriptor_method() {
    // `a.default()` requires `where a: Default`. Without it, the method
    // isn't in scope.
    let src = r#"
        trait Default {
            fn default() -> Self
        }

        fn broken(type a) -> a {
            a.default()
        }
    "#;
    let errs = type_errors(src);
    assert!(
        errs.iter().any(|m| m.contains("no method 'default'")),
        "expected 'no method' diagnostic, got:\n{}",
        errs.join("\n")
    );
}

#[test]
fn ambiguous_method_across_traits_on_descriptor() {
    // Two traits both provide `.make()`; calling via a generic `type a`
    // constrained to both should report ambiguity.
    let src = r#"
        trait A {
            fn make() -> Self
        }

        trait B {
            fn make() -> Self
        }

        fn broken(type x) -> x where x: A + B {
            x.make()
        }
    "#;
    let errs = type_errors(src);
    assert!(
        errs.iter().any(|m| m.contains("ambiguous")),
        "expected ambiguity diagnostic, got:\n{}",
        errs.join("\n")
    );
}

#[test]
fn supertrait_method_visible_on_descriptor() {
    // `where a: Sub` should also make `Super`'s methods callable on a
    // descriptor — same expansion rule as value-dispatch.
    let src = r#"
        trait Parent {
            fn parent() -> Self
        }

        trait Child: Parent {
            fn child() -> Self
        }

        trait Parent for Int {
            fn parent() -> Self { 1 }
        }

        trait Child for Int {
            fn child() -> Self { 2 }
        }

        fn call_parent(type a) -> a where a: Child {
            a.parent()
        }

        fn use_it() {
            let _ = call_parent(Int)
        }
    "#;
    assert_ok(src);
}

#[test]
fn user_record_type_dispatches_via_descriptor() {
    let src = r#"
        trait Make {
            fn make() -> Self
        }

        type Point { x: Int, y: Int }

        trait Make for Point {
            fn make() -> Self { Point { x: 0, y: 0 } }
        }

        fn use_it() {
            let _ = Point.make()
        }
    "#;
    assert_ok(src);
}

#[test]
fn generic_fn_body_uses_descriptor_method_with_constraint() {
    // A generic fn that uses a descriptor method inside its body relies
    // on the where clause propagating the constraint. Verify that
    // calling the generic fn from a context where the constraint is
    // satisfied works.
    let src = r#"
        import list

        trait Monoid {
            fn empty() -> Self
            fn combine(a: Self, b: Self) -> Self
        }

        trait Monoid for Int {
            fn empty() -> Self { 0 }
            fn combine(a: Self, b: Self) -> Self { a + b }
        }

        fn reduce(xs: List(a), type a) -> a where a: Monoid {
            list.fold(xs, a.empty()) { acc, x -> a.combine(acc, x) }
        }

        fn use_it() {
            let _ = reduce([1, 2, 3], Int)
        }
    "#;
    assert_ok(src);
}
