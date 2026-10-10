//! Which comments are a declaration's documentation, and that `silt
//! fmt` does not change the answer.

use silt::ast::Decl;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::source::FileId;

/// The documentation of each function of `source`, in order.
fn docs(source: &str) -> Vec<Option<String>> {
    let lexed = Lexer::new(FileId::default(), source)
        .tokenize()
        .checked()
        .expect("lexes");
    let program = Parser::new(lexed, source)
        .with_docs()
        .parse_program()
        .expect("parses");
    program
        .decls
        .iter()
        .filter_map(|decl| match decl {
            Decl::Fn(f) => Some(f.doc.clone()),
            _ => None,
        })
        .collect()
}

/// The documentation of the functions of `source`, which is the same
/// once `silt fmt` has written the file its way.
fn docs_before_and_after_fmt(source: &str) -> Vec<Option<String>> {
    let before = docs(source);
    let formatted = silt::format::format(FileId::default(), source).expect("formats");
    assert_eq!(
        before,
        docs(&formatted),
        "`silt fmt` changes the documentation:\n{source}\n---\n{formatted}"
    );
    before
}

fn doc(text: &str) -> Option<String> {
    Some(text.to_string())
}

#[test]
fn the_block_of_comments_above_a_declaration_documents_it() {
    assert_eq!(
        docs_before_and_after_fmt("-- one\n-- two\nfn f() { 1 }\n\nfn g() { 2 }\n"),
        [doc("one\ntwo"), None]
    );
    // An empty line cuts the block off; a comment behind code is that
    // code's.
    assert_eq!(
        docs_before_and_after_fmt("-- far\n\nfn f() { 1 } -- behind\nfn g() { 2 }\n"),
        [None, None]
    );
    assert_eq!(
        docs_before_and_after_fmt("{-\n  a block\n    indented\n-}\nfn f() { 1 }\n"),
        [doc("a block\n  indented")]
    );
}

#[test]
fn a_comment_on_the_declaration_s_line_does_not_cut_the_documentation_off() {
    assert_eq!(
        docs_before_and_after_fmt("-- doc of f\n{- inline -} fn f() { 1 }\n"),
        [doc("doc of f")]
    );
    // Without a block above, a comment on the line is no documentation.
    assert_eq!(
        docs_before_and_after_fmt("{- inline -} fn f() { 1 }\n"),
        [None]
    );
}

#[test]
fn comments_on_one_line_above_a_declaration_are_two_lines_of_documentation() {
    // `silt fmt` puts each on a line of its own; the documentation is
    // the same before and after.
    let both = docs_before_and_after_fmt("{- one -} {- two -}\nfn f() { 1 }\n");
    let lines: Vec<&str> = both[0]
        .as_deref()
        .expect("both comments are documentation")
        .lines()
        .map(str::trim)
        .collect();
    assert_eq!(lines, ["one", "two"]);
}
