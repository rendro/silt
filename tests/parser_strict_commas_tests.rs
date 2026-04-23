//! Regression tests for the strict-comma rule.
//!
//! silt's parser requires explicit commas between elements in every
//! delimited list-style construct. Previously the parser tolerated
//! whitespace/newline-only separation (`fn f(a b c)`, `[1 2 3]`); now
//! those forms must produce a parse error that mentions both the
//! construct name and the missing comma.
//!
//! Each construct has two tests:
//!   * positive — valid comma-separated form parses,
//!   * negative — the comma-less form is rejected with a helpful
//!     message containing `','` and (where feasible) the construct name.
//!
//! Trailing commas remain permitted and are covered by a separate
//! parametrized test at the bottom.
//!
//! Tests use `silt::lexer::Lexer + silt::parser::Parser` directly,
//! matching the pattern established by `tests/parse_recovery_tests.rs`.

use silt::lexer::Lexer;
use silt::parser::Parser;

fn parse(src: &str) -> Result<(), String> {
    let tokens = Lexer::new(src)
        .tokenize()
        .map_err(|e| format!("lex error: {e}"))?;
    Parser::new(tokens)
        .parse_program()
        .map(|_| ())
        .map_err(|e| e.message)
}

/// Assert that `src` produces a parse error whose message contains both
/// `','` (the literal comma reminder) and some fragment that identifies
/// the construct (e.g. "list literal", "function parameter list").
fn assert_missing_comma(src: &str, construct_hint: &str) {
    match parse(src) {
        Ok(()) => panic!(
            "expected parse error for {src:?}, got Ok (construct hint: {construct_hint:?})"
        ),
        Err(msg) => {
            assert!(
                msg.contains("','"),
                "expected error to mention `','`, got: {msg}"
            );
            assert!(
                msg.contains(construct_hint),
                "expected error to mention construct {construct_hint:?}, got: {msg}"
            );
        }
    }
}

// --------------------------------------------------------------------
// 1. fn params
// --------------------------------------------------------------------

#[test]
fn fn_params_require_commas() {
    assert_missing_comma("fn f(a b c) {}\n", "function parameter list");
}

#[test]
fn fn_params_with_commas_parse() {
    parse("fn f(a, b, c) {}\n").expect("valid fn params must parse");
}

// --------------------------------------------------------------------
// 2. list literal
// --------------------------------------------------------------------

#[test]
fn list_literal_requires_commas() {
    assert_missing_comma("fn main() { [1 2 3] }\n", "list literal");
}

#[test]
fn list_literal_with_commas_parses() {
    parse("fn main() { [1, 2, 3] }\n").expect("valid list literal must parse");
}

// --------------------------------------------------------------------
// 3. tuple literal (needs at least 2 elements — `(e)` is parenthesized)
// --------------------------------------------------------------------

#[test]
fn tuple_literal_requires_commas() {
    // Using three elements so the first comma disambiguates the tuple;
    // the missing comma between `2` and `3` then triggers the error.
    assert_missing_comma("fn main() { (1, 2 3) }\n", "tuple literal");
}

#[test]
fn tuple_literal_with_commas_parses() {
    parse("fn main() { (1, 2, 3) }\n").expect("valid tuple literal must parse");
}

// --------------------------------------------------------------------
// 4. map literal
// --------------------------------------------------------------------

#[test]
fn map_literal_requires_commas() {
    assert_missing_comma(
        "fn main() { #{\"a\": 1 \"b\": 2} }\n",
        "map literal",
    );
}

#[test]
fn map_literal_with_commas_parses() {
    parse("fn main() { #{\"a\": 1, \"b\": 2} }\n").expect("valid map literal must parse");
}

// --------------------------------------------------------------------
// 5. set literal
// --------------------------------------------------------------------

#[test]
fn set_literal_requires_commas() {
    assert_missing_comma("fn main() { #[1 2 3] }\n", "set literal");
}

#[test]
fn set_literal_with_commas_parses() {
    parse("fn main() { #[1, 2, 3] }\n").expect("valid set literal must parse");
}

// --------------------------------------------------------------------
// 6. call args
// --------------------------------------------------------------------

#[test]
fn call_args_require_commas() {
    assert_missing_comma(
        "fn foo(a, b, c) {}\nfn main() { foo(1 2 3) }\n",
        "function call argument list",
    );
}

#[test]
fn call_args_with_commas_parse() {
    parse("fn foo(a, b, c) {}\nfn main() { foo(1, 2, 3) }\n")
        .expect("valid call args must parse");
}

// --------------------------------------------------------------------
// 7. record literal fields
// --------------------------------------------------------------------

#[test]
fn record_literal_requires_commas() {
    let src = "type User { name: String, age: Int }\n\
               fn main() { User { name: \"a\" age: 30 } }\n";
    assert_missing_comma(src, "record literal fields");
}

#[test]
fn record_literal_with_commas_parses() {
    let src = "type User { name: String, age: Int }\n\
               fn main() { User { name: \"a\", age: 30 } }\n";
    parse(src).expect("valid record literal must parse");
}

// --------------------------------------------------------------------
// 8. record field type decl
// --------------------------------------------------------------------

#[test]
fn record_type_decl_requires_commas() {
    assert_missing_comma("type P { x: Int y: Int }\n", "record type fields");
}

#[test]
fn record_type_decl_with_commas_parses() {
    parse("type P { x: Int, y: Int }\n").expect("valid record type decl must parse");
}

// --------------------------------------------------------------------
// 9. enum variant decl
// --------------------------------------------------------------------

#[test]
fn enum_variant_fields_require_commas() {
    assert_missing_comma("type S { Foo(Int Int) }\n", "enum variant fields");
}

#[test]
fn enum_variant_fields_with_commas_parse() {
    parse("type S { Foo(Int, Int) }\n").expect("valid enum variant must parse");
}

// --------------------------------------------------------------------
// 10. selective import
// --------------------------------------------------------------------

#[test]
fn selective_import_requires_commas() {
    assert_missing_comma("import list.{ map filter }\n", "selective import list");
}

#[test]
fn selective_import_with_commas_parses() {
    parse("import list.{ map, filter }\n").expect("valid selective import must parse");
}

// --------------------------------------------------------------------
// 11. trailing commas remain permitted
// --------------------------------------------------------------------

#[test]
fn trailing_commas_are_allowed() {
    parse("fn f(a, b,) {}\n").expect("fn trailing comma must parse");
    parse("fn main() { [1, 2, 3,] }\n").expect("list trailing comma must parse");
    parse("fn main() { (1, 2, 3,) }\n").expect("tuple trailing comma must parse");
    parse("fn main() { #{\"a\": 1,} }\n").expect("map trailing comma must parse");
    parse("fn main() { #[1, 2, 3,] }\n").expect("set trailing comma must parse");
    parse(
        "fn foo(a, b, c) {}\nfn main() { foo(1, 2, 3,) }\n",
    )
    .expect("call-args trailing comma must parse");
    parse("type P { x: Int, y: Int, }\n").expect("record-type trailing comma must parse");
    parse("type S { Foo(Int, Int,), Bar, }\n")
        .expect("enum-variant trailing comma must parse");
    parse("import list.{ map, filter, }\n")
        .expect("selective-import trailing comma must parse");
}
