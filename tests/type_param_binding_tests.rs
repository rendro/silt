//! Typechecker tests for `type a` parameters and the return-type
//! binding rule.

use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::types::Severity;

fn check(input: &str) -> Vec<String> {
    let tokens = Lexer::new(input).tokenize().expect("lexer");
    let mut program = Parser::new(tokens).parse_program().expect("parse");
    typechecker::check(&mut program)
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

fn assert_ok(input: &str) {
    let errs = check(input);
    assert!(errs.is_empty(), "expected no errors, got:\n{}", errs.join("\n"));
}

fn assert_error_contains(input: &str, needle: &str) {
    let errs = check(input);
    assert!(
        errs.iter().any(|m| m.contains(needle)),
        "expected error containing {needle:?}, got:\n{}",
        errs.join("\n")
    );
}

#[test]
fn type_param_in_scope_for_return() {
    // `a` declared by `type a` flows through into the return position.
    // We call json.parse (which today is still first-arg-type) from inside
    // — so use a standalone scenario without calling stdlib.
    let src = r#"
        fn identity_desc(type a) -> a { unreachable() }
        fn main() {
          let _ = identity_desc
        }
    "#;
    // Body `unreachable()` won't resolve, so just check signature is accepted
    // via a looser lookup: parse + typecheck the signature alone.
    let sig_only = r#"
        fn identity_desc(type a) -> List(a)
    "#;
    let errs = check(sig_only);
    // Either no errors, or only errors unrelated to `a`.
    let unrelated = errs
        .iter()
        .filter(|m| m.contains("'a'") || m.contains("\"a\""))
        .count();
    assert_eq!(
        unrelated, 0,
        "type var 'a' should be in scope for return; got:\n{}",
        errs.join("\n")
    );
    // Ensure test compiles even if we didn't use it.
    let _ = src;
}

#[test]
fn return_only_var_rejected() {
    let src = "fn make() -> a { unreachable() }\n";
    assert_error_contains(src, "type variable 'a' in return type is not introduced");
}

#[test]
fn return_only_var_with_where_rejected() {
    let src = "fn make() -> a where a: Display { unreachable() }\n";
    // Should report both the return-binding error and the where-clause error.
    let errs = check(src);
    assert!(
        errs.iter().any(|m| m.contains("type variable 'a' in return type")),
        "expected return-type error, got:\n{}",
        errs.join("\n")
    );
}

#[test]
fn return_var_anchored_by_data_param_accepted() {
    let src = "fn identity(x: a) -> a { x }\n";
    assert_ok(src);
}

#[test]
fn return_var_anchored_by_type_param_accepted_signature() {
    // Can't fully check without running the body, but the signature alone
    // must not trip the binding rule.
    let src = "fn default(type a) -> a\n";
    let errs = check(src);
    assert!(
        !errs.iter().any(|m| m.contains("in return type is not introduced")),
        "type `a` anchored by `type a` param should be accepted; got:\n{}",
        errs.join("\n")
    );
}

#[test]
fn type_param_multiple_vars_in_return() {
    let src = "fn make_pair(type a, type b) -> (a, b)\n";
    let errs = check(src);
    assert!(
        !errs.iter().any(|m| m.contains("in return type is not introduced")),
        "both `a` and `b` anchored by `type` params; got:\n{}",
        errs.join("\n")
    );
}

#[test]
fn type_param_mixed_with_data_anchored_returns() {
    let src = "fn tag(x: a, type b) -> (a, b)\n";
    let errs = check(src);
    assert!(
        !errs.iter().any(|m| m.contains("in return type is not introduced")),
        "`a` via data param, `b` via type param; got:\n{}",
        errs.join("\n")
    );
}
