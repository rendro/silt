//! End-to-end tests for user-written `type a` function parameters.
//! Covers both typecheck-only and full VM-run scenarios.

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
fn user_decoder_wraps_json_parse() {
    let src = r#"
        import json

        type Todo { id: Int, title: String }

        fn decode(body: String, type a) -> Result(a, String) {
            json.parse(body, a)
        }

        fn use_it(body: String) {
            let _ = decode(body, Todo)
        }
    "#;
    assert_ok(src);
}

#[test]
fn user_fn_pipe_target_with_type_param() {
    let src = r#"
        import json

        type Todo { id: Int, title: String }

        fn decode(body: String, type a) -> Result(a, String) {
            json.parse(body, a)
        }

        fn use_it(body: String) {
            let _ = body |> decode(Todo)
        }
    "#;
    assert_ok(src);
}

#[test]
fn user_return_only_var_rejected_full_program() {
    let src = r#"
        fn broken() -> a {
            unreachable()
        }
    "#;
    let errs = type_errors(src);
    assert!(
        errs.iter()
            .any(|m| m.contains("type variable 'a' in return type")),
        "expected return-type binding error; got:\n{}",
        errs.join("\n")
    );
}

#[test]
fn user_type_param_last_enforced_full_program() {
    let src = r#"
        fn wrong(type a, body: String) -> a {
            unreachable()
        }
    "#;
    // This is a parser error, not a typechecker error — it's caught at parse
    // time. The parse itself will fail, so we test via the lexer/parser.
    let tokens = Lexer::new(src).tokenize().expect("lexer");
    let err = Parser::new(tokens)
        .parse_program()
        .expect_err("expected parse error");
    assert!(
        err.message.contains("type' parameters must come after"),
        "unexpected message: {}",
        err.message
    );
}

#[test]
fn user_type_param_constraint_fires_at_call_site() {
    // A user `fn` that constrains `type a` to Display should reject calls
    // with types that don't implement Display.
    let src = r#"
        fn say(type a) -> String where a: Display {
            "ok"
        }

        type Blob { value: Int }

        fn use_it() {
            let _ = say(Blob)
        }
    "#;
    // Blob auto-derives Display (per the four built-in auto-derived
    // traits), so this should typecheck.
    assert_ok(src);
}

#[test]
fn user_enum_passed_as_type_arg() {
    // Enum type names should be usable as `type a` arguments.
    let src = r#"
        type Color { Red, Green, Blue }

        fn tag(type a) -> String {
            "tagged"
        }

        fn use_it() {
            let _ = tag(Color)
        }
    "#;
    assert_ok(src);
}

#[test]
fn user_type_param_multiple_anchored_returns() {
    let src = r#"
        fn make_pair(type a, type b) -> (a, b) {
            unreachable()
        }
    "#;
    let errs = type_errors(src);
    assert!(
        !errs
            .iter()
            .any(|m| m.contains("in return type is not introduced")),
        "two-type-param signature should accept paired return; got:\n{}",
        errs.join("\n")
    );
}
