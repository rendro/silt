//! Parameterized trait declarations: `trait TryInto(b) { ... }`.
//! Covers: decl + impl with concrete args, concrete method call, generic
//! use via where-clause-with-args, arity mismatch, param binding.

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

fn assert_rejected(src: &str, needle: &str) {
    let errs = type_errors(src);
    assert!(
        errs.iter().any(|m| m.contains(needle)),
        "expected error containing {needle:?}; got:\n{}",
        errs.join("\n")
    );
}

#[test]
fn decl_with_single_param_typechecks() {
    assert_ok(
        r#"
        trait Convertible(b) {
            fn convert(self) -> b
        }
        "#,
    );
}

#[test]
fn decl_with_multiple_params_typechecks() {
    assert_ok(
        r#"
        trait Pair(a, b) {
            fn first(self) -> a
            fn second(self) -> b
        }
        "#,
    );
}

#[test]
fn decl_rejects_uppercase_param_name() {
    // Trait parameters must be lowercase, matching silt's tyvar convention.
    let tokens = Lexer::new(
        r#"
        trait Bad(B) {
            fn m(self) -> B
        }
        "#,
    )
    .tokenize()
    .expect("lexer");
    let err = Parser::new(tokens)
        .parse_program()
        .err()
        .expect("expected parse error");
    assert!(
        err.message.contains("lowercase"),
        "unexpected: {}",
        err.message
    );
}

#[test]
fn impl_with_concrete_args_typechecks() {
    assert_ok(
        r#"
        import int

        trait Convertible(b) {
            fn convert(self) -> b
        }

        trait Convertible(Int) for String {
            fn convert(self) -> Int {
                match int.parse(self) {
                    Ok(n) -> n
                    Err(_) -> 0
                }
            }
        }
        "#,
    );
}

#[test]
fn impl_arity_mismatch_rejected() {
    assert_rejected(
        r#"
        trait TwoArgs(a, b) {
            fn noop(self)
        }

        trait TwoArgs(Int) for String {
            fn noop(self) { () }
        }
        "#,
        "expects 2 type argument",
    );
}

#[test]
fn concrete_method_call_returns_impl_type() {
    // Call the impl method directly on a String instance; the compiler
    // should resolve `"x".convert()` to return Int.
    assert_ok(
        r#"
        import int

        trait Convertible(b) {
            fn convert(self) -> b
        }

        trait Convertible(Int) for String {
            fn convert(self) -> Int {
                match int.parse(self) {
                    Ok(n) -> n
                    Err(_) -> 0
                }
            }
        }

        fn use_it() {
            let n: Int = "42".convert()
        }
        "#,
    );
}

#[test]
fn generic_fn_with_parameterized_trait_constraint() {
    // The core motivating example: a generic fn whose body uses a
    // parameterized-trait method and relies on the where clause to
    // carry the trait's type argument.
    assert_ok(
        r#"
        import int

        trait Convertible(b) {
            fn convert(self) -> b
        }

        trait Convertible(Int) for String {
            fn convert(self) -> Int {
                match int.parse(self) {
                    Ok(n) -> n
                    Err(_) -> 0
                }
            }
        }

        fn unpack(x: a, type b) -> b where a: Convertible(b) {
            x.convert()
        }

        fn use_it() {
            let n: Int = unpack("42", Int)
        }
        "#,
    );
}

#[test]
fn multiple_impls_of_parameterized_trait() {
    // Different types can implement Convertible with different target args.
    // This is exactly what `trait TryInto<T>` enables in Rust.
    assert_ok(
        r#"
        trait Wrap(b) {
            fn wrap(self) -> b
        }

        trait Wrap(Int) for Int {
            fn wrap(self) -> Int { self }
        }

        trait Wrap(String) for String {
            fn wrap(self) -> String { self }
        }

        fn use_it() {
            let a: Int = 1.wrap()
            let b: String = "x".wrap()
        }
        "#,
    );
}

#[test]
fn parameterized_trait_param_is_in_scope_in_methods() {
    // The trait param `b` must be visible in the method signature's
    // return type. This is a spot check that registration wires params
    // into the method's param_map.
    assert_ok(
        r#"
        trait Box(b) {
            fn get(self) -> b
            fn set(self, x: b) -> Self
        }
        "#,
    );
}
