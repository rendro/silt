//! Round 93 — parser diagnostics and Pratt-loop dedup locks. The
//! Finding 2 (`//` hint) cases and the when-else / newline controls now
//! live in golden cases (tests/golden/frontend/{hints,parser}/round93_parser_hint__*.silt);
//! the AST-shape table below stays here because it inspects parser output.
//!
//! Finding 2 (GAP): a C-style `//` comment died with the bare
//! `expected expression, found /` (or `expected declaration, found /`
//! at top level). silt comments are `--`; newcomers hit this in minute
//! one. The parser now recognizes a parse error at a `/` immediately
//! followed (no gap, same line) by another `/` and emits a targeted
//! hint in the style of the G1 foreign-keyword hints.
//!
//! Finding 3 (cleanup lock): the eight infix-operator arms of
//! `parse_expr_bp_inner` shared a verbatim-copied tail
//! (`l_bp < min_bp → restore/break; advance; skip_nl;
//! parse_expr_bp(r_bp); rebuild node`). They now all route through one
//! helper (`parse_infix_rhs`). The precedence/associativity table
//! below pins the parsed structure of one expression per operator
//! family so any future drift in a single arm fails loudly.

use silt::ast::{Expr, ExprKind};
use silt::intern;
use silt::lexer::Lexer;
use silt::parser::Parser;

// ────────────────────────────────────────────────────────────────────
// Finding 3 lock: precedence/associativity table
// ────────────────────────────────────────────────────────────────────

/// Render the parsed structure of an expression fully parenthesized,
/// span-free, so two sources with the same shape compare equal and any
/// precedence/associativity drift in a single Pratt arm changes the
/// string.
fn shape(e: &Expr) -> String {
    match &e.kind {
        ExprKind::Int(n) => n.to_string(),
        ExprKind::Float(n) => format!("{n}"),
        ExprKind::Bool(b) => b.to_string(),
        ExprKind::Ident(s) => intern::resolve(*s),
        ExprKind::FieldAccess(b, f) => format!("{}.{}", shape(b), intern::resolve(*f)),
        ExprKind::Binary(l, op, r) => format!("({} {} {})", shape(l), op, shape(r)),
        ExprKind::Pipe(l, r) => format!("({} |> {})", shape(l), shape(r)),
        ExprKind::Range(l, r) => format!("({} .. {})", shape(l), shape(r)),
        ExprKind::QuestionMark(l) => format!("({}?)", shape(l)),
        ExprKind::Unary(op, v) => format!("({op:?} {})", shape(v)),
        ExprKind::Call(f, args) => format!(
            "{}({})",
            shape(f),
            args.iter().map(shape).collect::<Vec<_>>().join(", ")
        ),
        other => format!("<{other:?}>"),
    }
}

fn expr_shape(src: &str) -> String {
    let tokens = Lexer::new(src)
        .tokenize()
        .unwrap_or_else(|e| panic!("lex {src}: {e:?}"));
    let expr = Parser::new(tokens)
        .parse_expr()
        .unwrap_or_else(|e| panic!("parse {src}: {e:?}"));
    shape(&expr)
}

/// One expression per binary-operator family, pinning both precedence
/// and (left-)associativity. Any drift in one of the deduped Pratt
/// arms — a changed binding power, a broken `saved` restore, a
/// right-instead-of-left association — changes one of these strings.
#[test]
fn precedence_and_associativity_table() {
    let table: &[(&str, &str)] = &[
        // arithmetic: * / % bind tighter than + -, all left-assoc
        ("1 + 2 * 3 - 4", "((1 + (2 * 3)) - 4)"),
        ("10 - 3 - 2", "((10 - 3) - 2)"),
        ("100 / 5 / 2", "((100 / 5) / 2)"),
        ("10 % 3 + 7 / 2", "((10 % 3) + (7 / 2))"),
        ("1 + 2 + 3 * 4 % 5", "((1 + 2) + ((3 * 4) % 5))"),
        // comparison (50) binds tighter than equality (40)
        ("a < b == c > d", "((a < b) == (c > d))"),
        ("a <= b != c >= d", "((a <= b) != (c >= d))"),
        // equality (40) > && (30) > || (20); both boolean ops left-assoc
        ("a == b && c || d", "(((a == b) && c) || d)"),
        ("a || b && c", "(a || (b && c))"),
        ("a && b && c", "((a && b) && c)"),
        ("a || b || c", "((a || b) || c)"),
        // arithmetic > comparison > equality > boolean, mixed
        ("1 + 2 == 3 && 4 < 5", "(((1 + 2) == 3) && (4 < 5))"),
        // range (60) binds tighter than pipe (55)
        ("1 .. 10 |> f", "((1 .. 10) |> f)"),
        ("1 .. n + 1", "(1 .. (n + 1))"),
        // pipe (55) binds tighter than comparison/equality
        ("x |> f == y", "((x |> f) == y)"),
        ("x |> f |> g", "((x |> f) |> g)"),
        // `?` is a tight postfix, except that a trailing `?` applies to
        // the whole pipeline
        ("x |> f?", "((x |> f)?)"),
        ("x |> f |> g?", "(((x |> f) |> g)?)"),
        ("x + y?", "(x + (y?))"),
        ("f(a)? + f(b)?", "((f(a)?) + (f(b)?))"),
    ];
    for (src, expected) in table {
        let actual = expr_shape(src);
        assert_eq!(
            &actual, expected,
            "precedence/associativity drift for `{src}`: parsed as {actual}, \
             expected {expected}"
        );
    }
}

/// Equivalence pin: the explicitly parenthesized spelling of each table
/// row parses to the same shape — i.e. the unparenthesized source's
/// structure is exactly the one the parens claim.
#[test]
fn parenthesized_spelling_matches_table() {
    for (bare, parens) in [
        ("1 + 2 * 3 - 4", "(1 + (2 * 3)) - 4"),
        ("a == b && c || d", "((a == b) && c) || d"),
        ("x |> f == y", "(x |> f) == y"),
    ] {
        assert_eq!(
            expr_shape(bare),
            expr_shape(parens),
            "`{bare}` must parse with the structure of `{parens}`"
        );
    }
}
