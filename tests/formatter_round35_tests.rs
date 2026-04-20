//! Round 35 formatter regression tests.
//!
//! F1: Formatter dropped `{- ... -}` block comments that appeared inside
//! string-interpolation expressions such as `"x={foo() {- note -}}"`.
//! Root cause was the two-state scanner in `collect_source_block_comments`
//! failing to re-enter code mode inside `{...}`.
//!
//! F2: `format_expr_with_parens` only considered Binary-within-Binary
//! precedence. Low-bp child constructs (FloatElse, Range, Ascription,
//! QuestionMark) emitted as children of Binary/Range/Ascription/
//! QuestionMark dropped required parens, causing `parse(fmt(src))` to
//! produce a different AST than `parse(src)`.
//!
//! Each test below asserts AST equivalence (modulo span/ty metadata)
//! between source and formatter output.

use silt::ast::{Decl, Expr, ExprKind, Program, StringPart};
use silt::formatter::format;
use silt::lexer::Lexer;
use silt::parser::Parser;

fn parse_program(src: &str) -> Program {
    let tokens = Lexer::new(src)
        .tokenize()
        .unwrap_or_else(|e| panic!("lex failed: {e:?}\nsrc:\n{src}"));
    Parser::new(tokens)
        .parse_program()
        .unwrap_or_else(|e| panic!("parse failed: {e:?}\nsrc:\n{src}"))
}

/// Structural equality for Program — spans, types, and trivia-free.
/// Walks the AST recursively to produce a normalized string that omits
/// the `span`/`ty` fields. Sufficient for our round-trip checks.
fn structural_debug(p: &Program) -> String {
    let mut s = String::new();
    for d in &p.decls {
        decl_dbg(d, &mut s);
        s.push('\n');
    }
    s
}

fn decl_dbg(d: &Decl, out: &mut String) {
    match d {
        Decl::Fn(fn_decl) => {
            out.push_str(&format!("Fn(name={:?}, body=", fn_decl.name));
            expr_dbg(&fn_decl.body, out);
            out.push(')');
        }
        Decl::Let { pattern, value, .. } => {
            out.push_str(&format!("Let(pattern={pattern:?}, value="));
            expr_dbg(value, out);
            out.push(')');
        }
        other => out.push_str(&format!("{other:?}")),
    }
}

fn expr_dbg(e: &Expr, out: &mut String) {
    match &e.kind {
        ExprKind::Int(n) => out.push_str(&format!("Int({n})")),
        ExprKind::Float(f) => out.push_str(&format!("Float({f})")),
        ExprKind::Bool(b) => out.push_str(&format!("Bool({b})")),
        ExprKind::StringLit(s, triple) => out.push_str(&format!("Str({s:?},triple={triple})")),
        ExprKind::StringInterp(parts) => {
            out.push_str("Interp[");
            for (i, p) in parts.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                match p {
                    StringPart::Literal(s) => out.push_str(&format!("Lit({s:?})")),
                    StringPart::Expr(ex) => {
                        out.push_str("E(");
                        expr_dbg(ex, out);
                        out.push(')');
                    }
                }
            }
            out.push(']');
        }
        ExprKind::Ident(sym) => out.push_str(&format!("Ident({sym:?})")),
        ExprKind::Binary(l, op, r) => {
            out.push_str(&format!("Bin({op:?},"));
            expr_dbg(l, out);
            out.push(',');
            expr_dbg(r, out);
            out.push(')');
        }
        ExprKind::Unary(op, ex) => {
            out.push_str(&format!("Un({op:?},"));
            expr_dbg(ex, out);
            out.push(')');
        }
        ExprKind::Pipe(l, r) => {
            out.push_str("Pipe(");
            expr_dbg(l, out);
            out.push(',');
            expr_dbg(r, out);
            out.push(')');
        }
        ExprKind::Range(l, r) => {
            out.push_str("Range(");
            expr_dbg(l, out);
            out.push(',');
            expr_dbg(r, out);
            out.push(')');
        }
        ExprKind::FloatElse(l, r) => {
            out.push_str("FloatElse(");
            expr_dbg(l, out);
            out.push(',');
            expr_dbg(r, out);
            out.push(')');
        }
        ExprKind::Ascription(ex, ty) => {
            out.push_str(&format!("Asc(ty={ty:?},"));
            expr_dbg(ex, out);
            out.push(')');
        }
        ExprKind::QuestionMark(ex) => {
            out.push_str("Q(");
            expr_dbg(ex, out);
            out.push(')');
        }
        ExprKind::Call(c, args) => {
            out.push_str("Call(");
            expr_dbg(c, out);
            out.push_str(",[");
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                expr_dbg(a, out);
            }
            out.push_str("])");
        }
        ExprKind::Block(stmts) => {
            out.push_str(&format!("Block({stmts:?})"));
        }
        other => out.push_str(&format!("{other:?}")),
    }
}

fn assert_ast_preserved(src: &str) {
    let a1 = parse_program(src);
    let formatted = format(src).unwrap_or_else(|e| panic!("format failed: {e:?}\nsrc:\n{src}"));
    let a2 = parse_program(&formatted);
    let d1 = structural_debug(&a1);
    let d2 = structural_debug(&a2);
    assert_eq!(
        d1, d2,
        "AST must be preserved across formatting.\n--- src ---\n{src}\n--- formatted ---\n{formatted}\n--- ast(src) ---\n{d1}\n--- ast(fmt) ---\n{d2}"
    );
}

// ─── F1: string-interpolation block comments ──────────────────────────

#[test]
fn test_round35_f1_block_comment_inside_interp_preserved() {
    // The `{- note -}` lives inside a string-interpolation expression.
    // Prior to the fix, `collect_source_block_comments` was still in
    // "in_string" mode when it reached `{- note -}` and skipped it,
    // so the formatter lost the comment on output.
    let src = "fn main() {\n  println(\"x={foo() {- note -}}\")\n}\n";
    let formatted = format(src).unwrap_or_else(|e| panic!("format failed: {e:?}"));
    assert!(
        formatted.contains("{- note -}"),
        "block comment inside string interpolation must be preserved.\n--- formatted ---\n{formatted}"
    );
}

#[test]
fn test_round35_f1_block_comment_interp_round_trip() {
    // Round-trip: format again; must be idempotent and still contain the
    // comment. Also the parser must accept the formatted output.
    let src = "fn main() {\n  println(\"x={foo() {- note -}}\")\n}\n";
    let fmt1 = format(src).unwrap();
    let fmt2 = format(&fmt1).unwrap();
    assert_eq!(fmt1, fmt2, "formatter must be idempotent");
    // The formatted output must still lex+parse.
    let tokens = Lexer::new(&fmt1)
        .tokenize()
        .unwrap_or_else(|e| panic!("lex failed: {e:?}\nfmt:\n{fmt1}"));
    Parser::new(tokens)
        .parse_program()
        .unwrap_or_else(|e| panic!("parse failed: {e:?}\nfmt:\n{fmt1}"));
    assert!(fmt1.contains("{- note -}"));
}

// ─── F2: precedence-inverting parens ──────────────────────────────────

#[test]
fn test_round35_f2_range_containing_floatelse() {
    // `(a else b)..n` must keep parens. Otherwise `a else b..n` re-parses
    // as `FloatElse(a, Range(b, n))`.
    let src = "fn main() = (1.0 / 2.0 else 0.0)..10\n";
    assert_ast_preserved(src);
}

#[test]
fn test_round35_f2_ascription_containing_floatelse() {
    // `(x else 0.0) as Int` must keep parens — Ascription bp (95) is
    // higher than FloatElse bp (10), so omitting parens reparses as
    // `FloatElse(x, Ascription(0.0, Int))`.
    let src = "fn f(x) = (x else 0.0) as Int\n";
    assert_ast_preserved(src);
}

#[test]
fn test_round35_f2_binary_containing_floatelse() {
    // `(x else 0.0) + 1.0` must keep parens — `+` (70) is tighter than
    // `else` (10), so `x else 0.0 + 1.0` reparses as
    // `FloatElse(x, Add(0.0, 1.0))`.
    let src = "fn f(x) = (x else 0.0) + 1.0\n";
    assert_ast_preserved(src);
}

#[test]
fn test_round35_f2_questionmark_containing_floatelse() {
    // `(x else y)?` must keep parens — QuestionMark bp (110) is way
    // tighter than FloatElse (10).
    let src = "fn f(x, y) = (x else y)?\n";
    assert_ast_preserved(src);
}

#[test]
fn test_round35_f2_binary_containing_range() {
    // `(a..n) + 1` — Add (70) is tighter than Range (60). Dropping parens
    // would re-parse as `Range(a, Add(n, 1))`.
    let src = "fn f(a, n) = (a..n) + 1\n";
    assert_ast_preserved(src);
}

#[test]
fn test_round35_f2_nested_floatelse_left() {
    // `(a else b) else c` — FloatElse is right-associative (10, 11).
    // Without parens on the left, `a else b else c` parses as
    // `FloatElse(a, FloatElse(b, c))`, flipping associativity.
    let src = "fn f(a, b, c) = (a else b) else c\n";
    assert_ast_preserved(src);
}
