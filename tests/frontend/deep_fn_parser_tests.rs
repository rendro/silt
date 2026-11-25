//! Parser tests for the marker `deep` of `deep fn`: a word of its own
//! only right before `fn` at the top level, a name everywhere else.
//!
//! What the recursion check does with it is in the golden cases
//! tests/golden/lang/recursion/deep__*.silt. Also: the formatter keeps
//! what the check asks a loop of tail calls to write at its ends.

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

fn format(src: &str) -> String {
    silt::testing::format::format(silt::testing::source::FileId::default(), src).expect("formats")
}

/// The errors `silt check` gives `src`.
fn errors(src: &str) -> Vec<String> {
    silt::testing::check_str(src)
        .iter()
        .filter(|d| d.is_error())
        .map(|d| d.message().to_string())
        .collect()
}

/// Each shape an ending of a `()` loop of tail calls is written in to
/// say that it ends (E0346) is accepted, and still is once formatted: on
/// the last line of a function that may reach itself, the formatter
/// keeps the `return` of a call and the `()` behind a `let`.
#[test]
fn the_formatter_keeps_what_says_where_a_loop_ends() {
    let shapes = [
        // A `return` of a call on the last line.
        "fn f(n: Int) {\n  when n == 0 else {\n    return f(n - 1)\n  }\n  return println(\"end\")\n}\n",
        // A `()` behind a `let` on the last line.
        "fn g(n: Int) {\n  when n == 0 else {\n    return g(n - 1)\n  }\n  let _done = println(\"end\")\n  ()\n}\n",
        // A hand-off to another function, written as a `return`.
        "fn h(n: Int) {\n  when n == 0 else {\n    return h(n - 1)\n  }\n  return other(n)\n}\n\nfn other(n: Int) {\n  println(n)\n}\n",
        // Two functions that call each other.
        "fn a(n: Int) {\n  when n == 0 else {\n    return b(n - 1)\n  }\n  return println(\"a\")\n}\n\nfn b(n: Int) {\n  a(n)\n}\n",
        // In an arm.
        "fn s(n: Int) {\n  match n {\n    0 -> return println(\"end\")\n    _ -> s(n - 1)\n  }\n}\n",
    ];
    for shape in shapes {
        let src = format!("{shape}\nfn main() {{\n  println(1)\n}}\n");
        assert_eq!(errors(&src), Vec::<String>::new(), "{src}");
        let formatted = format(&src);
        assert_eq!(formatted, src, "the formatter changed it");
        assert_eq!(errors(&formatted), Vec::<String>::new(), "{formatted}");
    }
    // Without them each is refused: the test above is no test otherwise.
    let bare = "fn f(n: Int) {\n  when n == 0 else {\n    return f(n - 1)\n  }\n  println(\"end\")\n}\n\nfn main() {\n  f(1)\n}\n";
    assert!(
        errors(bare).iter().any(|e| e.contains("does not say so")),
        "{:?}",
        errors(bare)
    );
    // A function that does not reach itself loses its `return` as before.
    assert_eq!(
        format("fn k(n: Int) -> Int {\n  return n + 1\n}\n"),
        "fn k(n: Int) -> Int {\n  n + 1\n}\n"
    );
}

/// `deep fn` and `deep` as a name of each kind format as written and
/// are accepted.
#[test]
fn deep_formats_and_checks_as_a_marker_and_as_a_name() {
    let src = "import list\n\ndeep fn walk(xs: List(Int)) -> Int {\n  match xs {\n    [] -> 0\n    [x, ..rest] -> x + walk(list.filter(rest) { y -> y > x })\n  }\n}\n\nfn deep(deep: Int) -> Int {\n  let deep = deep + 1\n  deep\n}\n\nfn main() {\n  println(walk([1, 2]) + deep(1))\n}\n";
    assert_eq!(format(src), src);
    assert_eq!(errors(src), Vec::<String>::new());
}
