//! Round 93 — parser diagnostics and Pratt-loop dedup locks.
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

/// Parse a source string with the recovering entry point and return
/// every collected parse-error message.
fn parse_errors(input: &str) -> Vec<String> {
    let tokens = Lexer::new(input).tokenize().expect("lexer");
    let (_program, errors) = Parser::new(tokens).parse_program_recovering();
    errors.into_iter().map(|e| e.message).collect()
}

/// Parse a source string with the strict entry point; Ok(()) when the
/// whole program parses cleanly.
fn parse_ok(input: &str) -> Result<(), String> {
    let tokens = Lexer::new(input).tokenize().map_err(|e| format!("{e:?}"))?;
    Parser::new(tokens)
        .parse_program()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn run_silt(args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_silt"))
        .args(args)
        .output()
        .expect("silt binary runs")
}

const HINT: &str = "silt line comments use '--', not '//'";

// ────────────────────────────────────────────────────────────────────
// Finding 2: `//` gets the comment hint
// ────────────────────────────────────────────────────────────────────

/// Top level (declaration position): `// hello` before the first decl.
#[test]
fn double_slash_at_top_level_gets_hint() {
    let errs = parse_errors("// hello\nfn main() {\n  ()\n}\n");
    let joined = errs.join("\n");
    assert!(
        errs.iter().any(|e| e.contains(HINT)),
        "top-level `//` must get the comment hint, got:\n{joined}"
    );
}

/// Inside a function body (expression position).
#[test]
fn double_slash_inside_fn_gets_hint() {
    let errs = parse_errors("fn main() {\n  // hello\n  ()\n}\n");
    let joined = errs.join("\n");
    assert!(
        errs.iter().any(|e| e.contains(HINT)),
        "in-function `//` must get the comment hint, got:\n{joined}"
    );
}

/// Trailing comment (`let a = 1 // hello`): the first `/` is consumed
/// as division and the parse error lands on the SECOND slash; the
/// backward-adjacency check still fires the hint.
#[test]
fn trailing_double_slash_gets_hint() {
    let errs = parse_errors("fn main() {\n  let a = 1 // hello\n  ()\n}\n");
    let joined = errs.join("\n");
    assert!(
        errs.iter().any(|e| e.contains(HINT)),
        "trailing `//` comment must get the comment hint, got:\n{joined}"
    );
}

/// Strict entry point renders the same hint.
#[test]
fn double_slash_strict_parse_renders_hint() {
    let err = parse_ok("// hello\nfn main() {\n  ()\n}\n").expect_err("`//` must not parse");
    assert!(
        err.contains(HINT),
        "strict parse must render the comment hint, got:\n{err}"
    );
}

/// End-to-end rendering through the binary.
#[test]
fn double_slash_renders_hint_via_binary() {
    let dir = std::env::temp_dir().join("silt_round93_slashhint");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let main = dir.join("main.silt");
    std::fs::write(&main, "// hello\nfn main() {\n  ()\n}\n").unwrap();

    let output = run_silt(&["check", main.to_str().unwrap()]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "`//` must still be rejected; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains(HINT),
        "rendered diagnostic must contain the comment hint; got:\n{stderr}"
    );
}

// ────────────────────────────────────────────────────────────────────
// Finding 2 controls: `--` comments and division are untouched
// ────────────────────────────────────────────────────────────────────

/// Real silt comments (`--` line and `{- -}` block) keep parsing.
#[test]
fn dash_comments_unaffected() {
    parse_ok("-- hello\nfn main() {\n  -- inner\n  ()\n}\n").expect("`--` comments must parse");
    parse_ok("{- block\n   comment -}\nfn main() {\n  ()\n}\n")
        .expect("`{- -}` comments must parse");
}

/// Division — spaced and unspaced — keeps parsing and running.
#[test]
fn division_unaffected() {
    parse_ok("fn main() {\n  let a = 10\n  let b = 2\n  print(a / b)\n  print(a/b)\n}\n")
        .expect("division must keep parsing");

    let dir = std::env::temp_dir().join("silt_round93_division");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let main = dir.join("main.silt");
    std::fs::write(
        &main,
        "fn main() {\n  let a = 10\n  let b = 2\n  print(a / b)\n  print(a/b)\n}\n",
    )
    .unwrap();
    let output = run_silt(&["run", main.to_str().unwrap()]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "division must keep running; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert_eq!(stdout.replace(char::is_whitespace, ""), "55");
}

/// Adjacency lock: SPACED slashes (`/ /`) are not a comment attempt and
/// keep the generic error, not the hint.
#[test]
fn spaced_slashes_keep_generic_error() {
    let errs = parse_errors("fn main() {\n  / / x\n  ()\n}\n");
    let joined = errs.join("\n");
    assert!(
        !joined.contains(HINT),
        "spaced `/ /` must NOT get the comment hint, got:\n{joined}"
    );
    assert!(
        errs.iter().any(|e| e.contains("expected expression")),
        "spaced `/ /` keeps the generic expression error, got:\n{joined}"
    );
}

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
        ExprKind::FloatElse(l, r) => format!("({} else {})", shape(l), shape(r)),
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
        // `?` (54) applies to the whole pipe (55/56), not the RHS call
        ("x |> f?", "((x |> f)?)"),
        ("x + y?", "((x + y)?)"),
        // float-else is the loosest (10)
        ("1 + 2 else 3", "((1 + 2) else 3)"),
        ("a else b + c", "(a else (b + c))"),
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

/// min_bp gating still works across the deduped arms: an operator that
/// must NOT be consumed at a given binding power stays unconsumed and
/// the parser state is restored (`when cond else { ... }` relies on
/// `else` NOT being eaten as the FloatElse infix at min_bp 11).
#[test]
fn when_else_not_consumed_as_float_else() {
    parse_ok("fn f(n: Int) -> Int {\n  when n > 0 else {\n    return 0\n  }\n  n\n}\n")
        .expect("`when ... else { }` must keep parsing — `else` must not be eaten as FloatElse");
}

/// Newline sensitivity preserved through the dedup: `+`/`-` after a
/// newline terminate the expression (unary-at-statement-start
/// ambiguity), while `&&` may cross newlines.
#[test]
fn newline_sensitivity_preserved() {
    // `- 1` on its own line is a new statement, not a subtraction.
    parse_ok("fn main() {\n  let a = 1\n  - a\n  ()\n}\n")
        .expect("newline before `-` must terminate the previous expression");
    // `&&` may cross a newline boundary.
    parse_ok("fn main() {\n  let a = true\n  let b = a\n    && a\n  print(b)\n}\n")
        .expect("`&&` must still be allowed across newlines");
}
