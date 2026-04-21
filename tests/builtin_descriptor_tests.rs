//! Tests for descriptor dispatch on builtin container types
//! (`List`, `Map`, `Set`, `Channel`).

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
    assert!(
        errs.is_empty(),
        "expected no errors, got:\n{}",
        errs.join("\n")
    );
}

#[test]
fn list_as_type_arg() {
    assert_ok(
        r#"
        trait Empty {
            fn empty() -> Self
        }

        trait Empty for List(a) {
            fn empty() -> Self { [] }
        }

        fn make(type t) -> t where t: Empty {
            t.empty()
        }

        fn use_it() {
            let _: List(Int) = make(List)
        }
        "#,
    );
}

#[test]
fn map_as_type_arg() {
    assert_ok(
        r#"
        trait Empty {
            fn empty() -> Self
        }

        trait Empty for Map(k, v) {
            fn empty() -> Self { #{} }
        }

        fn make(type t) -> t where t: Empty {
            t.empty()
        }

        fn use_it() {
            let _: Map(String, Int) = make(Map)
        }
        "#,
    );
}

#[test]
fn set_as_type_arg() {
    assert_ok(
        r#"
        trait Empty {
            fn empty() -> Self
        }

        trait Empty for Set(a) {
            fn empty() -> Self { #[] }
        }

        fn make(type t) -> t where t: Empty {
            t.empty()
        }

        fn use_it() {
            let _: Set(Int) = make(Set)
        }
        "#,
    );
}

#[test]
fn list_concrete_type_path() {
    // `List.empty()` already worked via module-call dispatch on the
    // qualified name; lock it in.
    assert_ok(
        r#"
        trait Empty {
            fn empty() -> Self
        }

        trait Empty for List(a) {
            fn empty() -> Self { [] }
        }

        fn use_it() {
            let _: List(Int) = List.empty()
        }
        "#,
    );
}
