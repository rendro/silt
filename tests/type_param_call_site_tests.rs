//! End-to-end tests confirming json/toml call sites use type-last order
//! and compose with `|>`.

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

// Small helper: assemble a string variable "body"/"content" in silt without
// needing escaped quotes inside the Rust source. Typechecker only cares about
// the type — the content is irrelevant for these tests.

#[test]
fn json_parse_new_order_typechecks() {
    let src = r#"
        type Todo { id: Int, title: String, done: Bool }

        fn use_body(body: String) {
            let _ = json.parse(body, Todo)
        }
    "#;
    let errs = type_errors(src);
    assert!(
        errs.is_empty(),
        "expected no errors, got:\n{}",
        errs.join("\n")
    );
}

#[test]
fn json_parse_old_order_rejected() {
    let src = r#"
        type Todo { id: Int, title: String, done: Bool }

        fn use_body(body: String) {
            let _ = json.parse(Todo, body)
        }
    "#;
    let errs = type_errors(src);
    assert!(
        !errs.is_empty(),
        "old `json.parse(Todo, body)` order should fail typecheck now"
    );
}

#[test]
fn json_parse_pipe_composes() {
    let src = r#"
        type Todo { id: Int, title: String, done: Bool }

        fn use_body(body: String) {
            let _ = body |> json.parse(Todo)
        }
    "#;
    let errs = type_errors(src);
    assert!(
        errs.is_empty(),
        "pipe should compose cleanly with type-last order; errors:\n{}",
        errs.join("\n")
    );
}

#[test]
fn json_parse_list_new_order() {
    let src = r#"
        type Todo { id: Int, title: String, done: Bool }

        fn use_body(body: String) {
            let _ = json.parse_list(body, Todo)
        }
    "#;
    let errs = type_errors(src);
    assert!(errs.is_empty(), "errors:\n{}", errs.join("\n"));
}

#[test]
fn json_parse_map_new_order() {
    let src = r#"
        fn use_body(body: String) {
            let _ = json.parse_map(body, Int)
        }
    "#;
    let errs = type_errors(src);
    assert!(errs.is_empty(), "errors:\n{}", errs.join("\n"));
}

#[test]
fn toml_parse_new_order_typechecks() {
    let src = r#"
        type Config { name: String }

        fn use_content(content: String) {
            let _ = toml.parse(content, Config)
        }
    "#;
    let errs = type_errors(src);
    assert!(errs.is_empty(), "errors:\n{}", errs.join("\n"));
}

#[test]
fn toml_parse_pipe_composes() {
    let src = r#"
        type Config { name: String }

        fn use_content(content: String) {
            let _ = content |> toml.parse(Config)
        }
    "#;
    let errs = type_errors(src);
    assert!(errs.is_empty(), "errors:\n{}", errs.join("\n"));
}
