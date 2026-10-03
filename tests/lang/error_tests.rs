//! Error tests that need the Rust API: an import of a missing builtin
//! item, checked through the session, and the `(break)` formatter round
//! trip. Every other error test is a golden case under
//! `tests/golden/lang/errors/error__*`.

use silt::lexer::Lexer;
use silt::parser::Parser;

/// Expect a parse error; returns the error message.
/// Uses parse_program which returns the first fatal error.
fn parse_errors(input: &str) -> Vec<String> {
    let tokens = Lexer::new(silt::source::FileId::default(), input)
        .tokenize()
        .expect("lexer error");
    match Parser::new(tokens, input).parse_program() {
        Err(e) => vec![e.message.clone()],
        Ok(_) => vec![], // no error; caller should check
    }
}

#[test]
fn test_import_nonexistent_builtin_item() {
    // An import of a name a builtin module does not have is rejected
    // when the program is checked, at the item.
    let source =
        "import list.{ nonexistent_function }\nfn main() { nonexistent_function([1, 2]) }\n";
    let errors: Vec<String> = silt::session::testing::analyze_str(source)
        .1
        .into_iter()
        .map(|d| d.message)
        .collect();
    assert!(
        errors
            .iter()
            .any(|m| m.contains("module 'list' has no member 'nonexistent_function'")),
        "got: {errors:?}"
    );
}

#[test]
fn test_parenthesized_break_parses_and_roundtrips_through_formatter() {
    // Regression lock against the formatter/parser roundtrip failure
    // that the removed G1 guard caused. `(break)` is syntactically a
    // paren expression wrapping an ident reference, and the formatter
    // strips redundant parens. The result must still parse — the G1
    // guard used to reject it with a fake "syntax error" even though
    // the token stream is perfectly valid ident-in-statement.
    let src = "fn main() {\n  (break)\n}\n";
    let formatted =
        silt::formatter::format(src).expect("paren-wrapped break must format without error");
    // parse_errors drops any typechecker diagnostics; we only care
    // that the *parser* accepts the formatter's output.
    let perrs = parse_errors(&formatted);
    assert!(
        perrs.is_empty(),
        "parser must accept formatter output for (break); formatted={formatted:?}, errors={perrs:?}"
    );
}
