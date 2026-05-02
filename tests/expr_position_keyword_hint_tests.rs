//! Round 67 GAP regression lock — F12.
//!
//! In statement position, `parser.rs` G1 emits actionable error messages
//! when the user types `if`, `while`, or `for` (silt has none of these
//! keywords). In expression position the parser parses these as bare
//! identifiers and the typechecker emits "undefined variable 'if'" with
//! NO hint. The doc-comment on `format_undefined_variable_message` says
//! the typechecker is meant to be the unified home for these hints.
//!
//! Fix: extend the `foreign_keyword_hint` table in
//! `src/typechecker/inference.rs` to cover `if`, `while`, `for` with
//! the SAME wording the parser uses at parser.rs:2084-2118. We assert
//! the hint mentions `match` (silt's `if`-replacement) for `if`, and
//! mentions `loop` / `list.each` / `list.map` for `while`/`for`.

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

#[test]
fn test_if_in_expression_position_hints_match() {
    let errs = type_errors(
        r#"
fn main() {
    let x: Bool = true
    let r = if x { 1 } else { 0 }
    println(r)
}
"#,
    );
    // Must reference the undefined identifier
    assert!(
        errs.iter().any(|e| e.contains("undefined variable 'if'")),
        "expected `undefined variable 'if'` diagnostic, got:\n{}",
        errs.join("\n")
    );
    // Must include the "use 'match' ..." advice
    assert!(
        errs.iter()
            .any(|e| e.contains("silt has no 'if' keyword") && e.contains("match cond")),
        "expected the parser-style hint mentioning `match cond` for the `if` mistake, got:\n{}",
        errs.join("\n")
    );
}

#[test]
fn test_while_in_expression_position_hints_loop() {
    let errs = type_errors(
        r#"
fn main() {
    let x: Bool = true
    let r = while x { 1 }
    println(r)
}
"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("undefined variable 'while'")),
        "expected `undefined variable 'while'` diagnostic, got:\n{}",
        errs.join("\n")
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("silt has no 'while'/'for' keywords")
                && (e.contains("loop") || e.contains("list.each") || e.contains("list.map"))),
        "expected the parser-style hint mentioning loop/list.each/list.map for `while`, got:\n{}",
        errs.join("\n")
    );
}

#[test]
fn test_for_in_expression_position_hints_iterators() {
    let errs = type_errors(
        r#"
fn main() {
    let r = for x { 1 }
    println(r)
}
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains("undefined variable 'for'")),
        "expected `undefined variable 'for'` diagnostic, got:\n{}",
        errs.join("\n")
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("silt has no 'while'/'for' keywords")
                && (e.contains("loop") || e.contains("list.each") || e.contains("list.map"))),
        "expected the parser-style hint mentioning loop/list.each/list.map for `for`, got:\n{}",
        errs.join("\n")
    );
}
