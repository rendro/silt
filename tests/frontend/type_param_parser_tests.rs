//! Parser tests for `type a` function parameters.
//!
//! Verifies:
//!   * `type a` parses as a `ParamKind::Type` param with the correct name.
//!   * Multiple `type a` params are accepted contiguously at the end.
//!   * Mixing `Data` and `Type` params with Type last is accepted.
//!   * Trait methods accept `type a`.
//!   * The formatter is idempotent on multi-line `type a` signatures.
//!
//! The rejection diagnostics and the canonical-format checks live in
//! golden cases: tests/golden/frontend/{parser,fmt}/type_param_parser__*.silt.

use silt::ast::{Decl, FnDecl, ParamKind, PatternKind};
fn format_source(source: &str) -> Result<String, silt::diagnostic::Diagnostic> {
    silt::format::format(silt::source::FileId::default(), source)
}
use silt::lexer::Lexer;
use silt::parser::Parser;

fn parse_ok(src: &str) -> Vec<Decl> {
    let tokens = Lexer::new(silt::source::FileId::default(), src)
        .tokenize()
        .checked()
        .expect("lexer");
    let program = Parser::new(tokens, src).parse_program().expect("parse");
    program.decls
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
