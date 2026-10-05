//! The operator table of `docs/language/operators.md` against the
//! parser's precedence table, `silt::ast::prec`.
//!
//! Two things are compared, neither by reading Rust source:
//!
//! * the precedence number the page gives each operator is the binding
//!   power `prec` has for it, and the page lists every operator `prec`
//!   knows;
//! * for each pair of adjacent rows, a program that uses an operator of
//!   each row is parsed by the real parser, and the operator of the
//!   lower row is the root of the tree: the row order of the page is
//!   the order the parser binds in.

use silt::ast::{BinOp, Decl, Expr, ExprKind, Stmt, UnaryOp, prec};
use silt::lexer::Lexer;
use silt::parser::Parser;

/// A row of the page's table: its precedence and its operators, as the
/// page spells them (`-x`, `{ ... }`, `f(...)` included).
struct Row {
    precedence: u8,
    operators: Vec<String>,
}

fn doc_rows() -> Vec<Row> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/docs/language/operators.md");
    let page = std::fs::read_to_string(path).expect("docs/language/operators.md");
    let table: Vec<&str> = page
        .lines()
        .skip_while(|line| !line.starts_with("| Precedence"))
        .take_while(|line| line.starts_with('|'))
        .collect();
    let mut rows = Vec::new();
    // The header and the `|---|` line come first.
    for line in table.iter().skip(2) {
        // `\|` is a `|` inside a cell.
        let line = line.replace("\\|", "\u{1}");
        let cells: Vec<&str> = line.split('|').map(str::trim).collect();
        let precedence = cells[1].parse().expect("a precedence number");
        let operators: Vec<String> = cells[2]
            .split('`')
            .skip(1)
            .step_by(2)
            .map(|spelling| spelling.replace('\u{1}', "|"))
            .collect();
        assert!(!operators.is_empty(), "a row without an operator: {line}");
        rows.push(Row {
            precedence,
            operators,
        });
    }
    rows
}

const BINARY: [BinOp; 13] = [
    BinOp::Or,
    BinOp::And,
    BinOp::Eq,
    BinOp::Neq,
    BinOp::Lt,
    BinOp::Gt,
    BinOp::Leq,
    BinOp::Geq,
    BinOp::Add,
    BinOp::Sub,
    BinOp::Mul,
    BinOp::Div,
    BinOp::Mod,
];

/// Every operator of `prec` with the binding power the page shows (the
/// left one of an infix operator), spelled as the page spells it.
fn prec_table() -> Vec<(String, u8)> {
    let mut table: Vec<(String, u8)> = BINARY
        .iter()
        .map(|op| (op.to_string(), op.binding_power().0))
        .collect();
    for (spelling, power) in [
        ("|>", prec::PIPE.0),
        ("..", prec::RANGE.0),
        ("-x", prec::UNARY),
        ("!x", prec::UNARY),
        ("as", prec::AS),
        ("{ ... }", prec::TRAILING_CLOSURE),
        ("f(...)", prec::CALL),
        ("?", prec::CALL),
        (".", prec::FIELD),
    ] {
        table.push((spelling.to_string(), power));
    }
    table
}

/// How an operator of the page stands in an expression.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Infix,
    Prefix,
    /// `as`: infix, with a type on its right.
    As,
    Postfix,
}

fn kind(spelling: &str) -> Kind {
    match spelling {
        "-x" | "!x" => Kind::Prefix,
        "as" => Kind::As,
        "{ ... }" | "f(...)" | "?" | "." => Kind::Postfix,
        _ => Kind::Infix,
    }
}

/// `a` with the postfix operator applied.
fn postfix(spelling: &str) -> &'static str {
    match spelling {
        "{ ... }" => "a { x -> x }",
        "f(...)" => "a(b)",
        "?" => "a?",
        "." => "a.b",
        other => panic!("not a postfix operator: {other}"),
    }
}

/// The last expression of `fn main() { <body> }`, parsed.
fn parse(body: &str) -> Expr {
    let source = format!("fn main() {{\n  {body}\n}}\n");
    let tokens = Lexer::new(silt::source::FileId::default(), &source)
        .tokenize()
        .unwrap_or_else(|e| panic!("`{body}` does not lex: {e:?}"));
    let program = Parser::new(tokens, &source)
        .parse_program()
        .unwrap_or_else(|e| panic!("`{body}` does not parse: {e:?}"));
    let Some(Decl::Fn(main)) = program.decls.into_iter().next() else {
        panic!("`{body}`: no function");
    };
    let ExprKind::Block(mut stmts) = main.body.kind else {
        panic!("`{body}`: no block");
    };
    match stmts.pop() {
        Some(Stmt::Expr(expr)) if stmts.is_empty() => expr,
        _ => panic!("`{body}` is not one expression"),
    }
}

/// The operator at the root of `expr`, as the page spells it.
fn root(expr: &Expr) -> String {
    match &expr.kind {
        ExprKind::Binary(_, op, _) => op.to_string(),
        ExprKind::Pipe(..) => "|>".to_string(),
        ExprKind::Range(..) => "..".to_string(),
        ExprKind::Unary(UnaryOp::Neg, _) => "-x".to_string(),
        ExprKind::Unary(UnaryOp::Not, _) => "!x".to_string(),
        ExprKind::Ascription(..) => "as".to_string(),
        ExprKind::Call(..) => "call".to_string(),
        ExprKind::QuestionMark(..) => "?".to_string(),
        ExprKind::FieldAccess(..) => ".".to_string(),
        _ => "operand".to_string(),
    }
}

fn assert_root(body: &str, operator: &str) {
    let tree = parse(body);
    assert_eq!(
        root(&tree),
        operator,
        "`{body}`: docs/language/operators.md says `{operator}` binds loosest here"
    );
}

#[test]
fn the_page_has_the_binding_powers_of_prec() {
    let mut documented: Vec<(String, u8)> = doc_rows()
        .into_iter()
        .flat_map(|row| {
            let precedence = row.precedence;
            row.operators.into_iter().map(move |op| (op, precedence))
        })
        .collect();
    let mut table = prec_table();
    documented.sort();
    table.sort();
    assert_eq!(
        documented, table,
        "the operator table of docs/language/operators.md (left) and silt::ast::prec (right) differ"
    );
}

#[test]
fn the_rows_of_the_page_are_in_binding_order() {
    let rows = doc_rows();
    assert!(
        rows.windows(2).all(|w| w[0].precedence <= w[1].precedence),
        "the rows are listed from the lowest precedence to the highest"
    );
    let mut probes = 0;
    for pair in rows.windows(2) {
        let (lower, higher) = (&pair[0], &pair[1]);
        for low in &lower.operators {
            for high in &higher.operators {
                // The spelling of a prefix operator without its `x`.
                let sign = |spelling: &str| spelling.trim_end_matches('x').to_string();
                match (kind(low), kind(high)) {
                    (Kind::Infix, Kind::Infix) => {
                        assert_root(&format!("a {low} b {high} c"), low);
                        assert_root(&format!("a {high} b {low} c"), low);
                    }
                    (Kind::Infix, Kind::Prefix) => {
                        assert_root(&format!("{}a {low} b", sign(high)), low);
                    }
                    (Kind::Prefix, Kind::As) => {
                        assert_root(&format!("{}a as T", sign(low)), low);
                    }
                    (Kind::As, Kind::Postfix) => {
                        assert_root(&format!("{} as T", postfix(high)), low);
                    }
                    // Postfix operators apply in the order they are
                    // written; there is no program that tells their
                    // precedences apart.
                    (Kind::Postfix, Kind::Postfix) => continue,
                    other => {
                        panic!("no probe for the adjacent rows `{low}` and `{high}`: {other:?}")
                    }
                }
                probes += 1;
            }
        }
    }
    assert!(probes >= 30, "only {probes} pairs were probed");
}

#[test]
fn every_postfix_operator_binds_tighter_than_a_prefix_operator() {
    for row in doc_rows() {
        for op in &row.operators {
            if kind(op) == Kind::Postfix {
                assert_root(&format!("-{}", postfix(op)), "-x");
                assert_root(&format!("!{}", postfix(op)), "!x");
            }
        }
    }
}

#[test]
fn every_infix_operator_is_left_associative() {
    for row in doc_rows() {
        for op in &row.operators {
            if kind(op) != Kind::Infix {
                continue;
            }
            let body = format!("a {op} b {op} c");
            let tree = parse(&body);
            let (ExprKind::Binary(left, ..) | ExprKind::Pipe(left, _) | ExprKind::Range(left, _)) =
                &tree.kind
            else {
                panic!("`{body}` is not an infix expression");
            };
            assert_eq!(root(&tree), *op, "`{body}`");
            assert_eq!(root(left), *op, "`{body}` is `(a {op} b) {op} c`");
        }
    }
}
