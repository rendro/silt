//! Parser tests for the marker `deep` of `deep fn`: a word of its own
//! only right before `fn` at the top level, a name everywhere else.
//!
//! What the recursion check does with it is in the golden cases
//! tests/golden/lang/recursion/deep__*.silt.

use silt::testing::ast::{Decl, ExprKind, FnDecl, PatternKind, Stmt};
use silt::testing::lexer::Lexer;
use silt::testing::parser::Parser;

fn parse_ok(src: &str) -> Vec<Decl> {
    let tokens = Lexer::new(silt::testing::source::FileId::default(), src)
        .tokenize()
        .checked()
        .expect("lexer");
    let program = Parser::new(tokens, src).parse_program().expect("parse");
    program.decls
}

fn fns(decls: &[Decl]) -> Vec<&FnDecl> {
    decls
        .iter()
        .filter_map(|d| if let Decl::Fn(f) = d { Some(f) } else { None })
        .collect()
}

#[test]
fn deep_before_fn_marks_the_function_and_starts_its_span() {
    let src = "deep fn walk(xs) { xs }\npub deep fn other(n) { n }\nfn plain(n) { n }\n";
    let decls = parse_ok(src);
    let fns = fns(&decls);
    assert_eq!(fns.len(), 3);
    let deep = fns[0].deep.expect("walk is deep");
    assert_eq!(&src[deep.start as usize..deep.end as usize], "deep");
    assert_eq!(fns[0].span.start, 0, "the declaration starts at `deep`");
    let deep = fns[1].deep.expect("other is deep");
    assert_eq!(&src[deep.start as usize..deep.end as usize], "deep");
    assert!(fns[1].is_pub);
    assert!(fns[2].deep.is_none());
}

#[test]
fn deep_is_a_name_everywhere_else() {
    let src = "fn deep(deep) { let deep = deep\n deep }\nlet deep_value = 1\n";
    let decls = parse_ok(src);
    let f = fns(&decls)[0];
    assert_eq!(f.name.to_string(), "deep");
    assert!(f.deep.is_none());
    assert!(
        matches!(&f.params[0].pattern.kind, PatternKind::Ident(name) if name.to_string() == "deep")
    );
    let ExprKind::Block(stmts) = &f.body.kind else {
        panic!("a block body");
    };
    assert!(
        matches!(&stmts[0], Stmt::Let { pattern, .. } if matches!(&pattern.kind, PatternKind::Ident(name) if name.to_string() == "deep"))
    );
}

#[test]
fn the_formatter_keeps_deep() {
    let src = "deep   fn walk(xs: List(Int)) -> Int {\n  0\n}\n\npub  deep fn other(n: Int) -> Int {\n  n\n}\n";
    let formatted = silt::testing::format::format(silt::testing::source::FileId::default(), src)
        .expect("formats");
    assert_eq!(
        formatted,
        "deep fn walk(xs: List(Int)) -> Int {\n  0\n}\n\npub deep fn other(n: Int) -> Int {\n  n\n}\n"
    );
}
