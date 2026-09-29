//! Parser tests for `type a` function parameters.
//!
//! Verifies:
//!   * `type a` parses as a `ParamKind::Type` param with the correct name.
//!   * Multiple `type a` params are accepted contiguously at the end.
//!   * Mixing `Data` and `Type` params with Type last is accepted.
//!   * `type a` followed by any Data param is rejected (type-last rule).
//!   * `type a: T` (annotation) is rejected.
//!   * Trait methods accept `type a`.
//!   * Formatter round-trips `type a` params.

use silt::ast::{Decl, FnDecl, ParamKind, PatternKind};
use silt::formatter::format as format_source;
use silt::lexer::Lexer;
use silt::parser::Parser;

fn parse_ok(src: &str) -> Vec<Decl> {
    let tokens = Lexer::new(src).tokenize().expect("lexer");
    let program = Parser::new(tokens).parse_program().expect("parse");
    program.decls
}

fn parse_err(src: &str) -> String {
    let tokens = Lexer::new(src).tokenize().expect("lexer");
    Parser::new(tokens)
        .parse_program()
        .err()
        .map(|e| e.message)
        .expect("expected parse error")
}

fn first_fn(decls: &[Decl]) -> &FnDecl {
    decls
        .iter()
        .find_map(|d| if let Decl::Fn(f) = d { Some(f) } else { None })
        .expect("expected fn decl")
}

#[test]
fn parses_single_type_param() {
    let decls = parse_ok("fn default(type a) -> a { a }");
    let f = first_fn(&decls);
    assert_eq!(f.params.len(), 1);
    assert_eq!(f.params[0].kind, ParamKind::Type);
    assert!(matches!(&f.params[0].pattern.kind, PatternKind::Ident(_)));
    assert!(f.params[0].ty.is_none());
}

#[test]
fn parses_data_then_type_param() {
    let decls = parse_ok("fn parse(body: String, type a) -> a { a }");
    let f = first_fn(&decls);
    assert_eq!(f.params.len(), 2);
    assert_eq!(f.params[0].kind, ParamKind::Data);
    assert_eq!(f.params[1].kind, ParamKind::Type);
}

#[test]
fn parses_multiple_type_params_contiguous() {
    let decls = parse_ok("fn convert(x: a, type b, type c) -> b { x }");
    let f = first_fn(&decls);
    assert_eq!(f.params.len(), 3);
    let kinds: Vec<_> = f.params.iter().map(|p| p.kind.clone()).collect();
    assert_eq!(
        kinds,
        vec![ParamKind::Data, ParamKind::Type, ParamKind::Type]
    );
}

#[test]
fn rejects_type_before_data() {
    let msg = parse_err("fn bad(type a, body: String) -> a { a }");
    assert!(
        msg.contains("type' parameters must come after"),
        "unexpected message: {msg}"
    );
}

#[test]
fn rejects_type_then_data_then_type() {
    let msg = parse_err("fn bad(type a, x: Int, type b) -> b { b }");
    assert!(
        msg.contains("type' parameters must come after"),
        "unexpected message: {msg}"
    );
}

#[test]
fn rejects_type_with_annotation() {
    let msg = parse_err("fn bad(type a: Int) -> a { a }");
    assert!(
        msg.contains("cannot carry a type annotation"),
        "unexpected message: {msg}"
    );
}

#[test]
fn parses_in_trait_method() {
    let src = "trait Make {\n  fn make(type a) -> a\n}\n";
    let decls = parse_ok(src);
    let trait_decl = decls
        .iter()
        .find_map(|d| {
            if let Decl::Trait(t) = d {
                Some(t)
            } else {
                None
            }
        })
        .expect("trait decl");
    assert_eq!(trait_decl.methods.len(), 1);
    let m = &trait_decl.methods[0];
    assert_eq!(m.params.len(), 1);
    assert_eq!(m.params[0].kind, ParamKind::Type);
}

#[test]
fn formatter_roundtrips_type_param() {
    let src = "fn default(type a) -> a {\n  a\n}\n";
    let formatted = format_source(src).expect("format");
    assert!(
        formatted.contains("type a"),
        "formatted output missing `type a`: {formatted}"
    );
}

#[test]
fn formatter_roundtrips_mixed_params() {
    let src = "fn parse(body: String, type a) -> a {\n  a\n}\n";
    let formatted = format_source(src).expect("format");
    assert!(
        formatted.contains("body: String"),
        "missing body param: {formatted}"
    );
    assert!(formatted.contains("type a"), "missing type a: {formatted}");
    let body_pos = formatted.find("body:").unwrap();
    let type_pos = formatted.find("type a").unwrap();
    assert!(
        body_pos < type_pos,
        "type a should come after data params: {formatted}"
    );
}

#[test]
fn formatter_idempotent_multiline_type_params() {
    // A long signature spanning multiple lines, mixing data and type
    // params, must round-trip cleanly — format(format(src)) == format(src).
    let src = "fn pipeline(\n    src: String,\n    parse: Fn(String) -> Int,\n    type a,\n    type b,\n) -> (a, b) {\n    unreachable()\n}\n";
    let first = format_source(src).expect("format 1");
    let second = format_source(&first).expect("format 2");
    assert_eq!(
        first, second,
        "formatter not idempotent on multi-line `type a` signatures.\n\
         first pass:\n{first}\n\n\
         second pass:\n{second}"
    );
    // And the two `type` params must remain contiguous at the end.
    let a_pos = first.find("type a").expect("type a missing");
    let b_pos = first.find("type b").expect("type b missing");
    let src_pos = first.find("src:").expect("src: missing");
    assert!(
        src_pos < a_pos && a_pos < b_pos,
        "expected order src, type a, type b; got:\n{first}"
    );
}
