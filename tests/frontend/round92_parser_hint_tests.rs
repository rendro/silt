//! Round 92 — parser diagnostic fixes. Finding 1 and the
//! statement-position control now live in golden cases
//! (tests/golden/frontend/hints/round92_parser_hint__*.silt); what stays
//! here tests the `fn f(..) = expr` body position, which stage 4 removes,
//! plus the example-corpus parse control.
//!
//! Finding 1 (GAP): the postfix-indexing diagnostic recommended
//! `string.char_at(s, i)`, a function that does not exist anywhere in
//! the registry (`src/typechecker/builtins/string.rs` has `chars`,
//! `char_code`, `slice`, ... but no `char_at`). The message now
//! recommends `string.slice(s, i, i + 1)`, which actually answers
//! "get the i-th character" and is verified to typecheck and run.
//!
//! Finding 2 (GAP): the G1 "silt has no 'if' keyword" hint fired only
//! at statement-start positions. In an expression-bodied function —
//! silt's signature style, and exactly where newcomers porting code
//! write `if` — the parser parsed `if` as an identifier body, returned
//! to declaration level, and emitted the baffling
//! `expected declaration, found n`. The hint now also fires after an
//! `=` body parses as a bare `if`/`while`/`for` identifier followed by
//! an expression-start token (a position that is a guaranteed parse
//! error, so accepted programs are byte-identical).

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

// ────────────────────────────────────────────────────────────────────
// Finding 2: no-'if' hint must fire in expression-bodied functions
// ────────────────────────────────────────────────────────────────────

/// The original repro: `if` as the start of an `=` function body used
/// to produce only `expected declaration, found n`.
#[test]
fn expr_bodied_fn_with_if_gets_no_if_hint() {
    let errs = parse_errors("fn f(n: Int) -> Int = if n == 0 { 1 } else { 2 }\n");
    let joined = errs.join("\n");
    assert!(
        errs.iter()
            .any(|e| e.contains("silt has no 'if' keyword") && e.contains("match cond")),
        "expression-bodied `= if ...` must trigger the no-'if' hint \
         pointing at 'match', got:\n{joined}"
    );
}

/// Same through the strict (non-recovering) parse entry point.
#[test]
fn expr_bodied_fn_with_if_hint_fires_in_strict_parse() {
    let err = parse_ok("fn f(n: Int) -> Int = if n == 0 { 1 } else { 2 }\n")
        .expect_err("the `= if ...` body must not parse");
    assert!(
        err.contains("silt has no 'if' keyword") && err.contains("match cond"),
        "strict parse must render the no-'if' hint, got:\n{err}"
    );
}

/// `while`/`for` get the matching loop hint in the same position.
#[test]
fn expr_bodied_fn_with_while_gets_loop_hint() {
    let errs = parse_errors("fn f(n: Int) -> Int = while n > 0 { 1 }\n");
    let joined = errs.join("\n");
    assert!(
        errs.iter()
            .any(|e| e.contains("silt has no 'while'/'for' keywords") && e.contains("loop")),
        "expression-bodied `= while ...` must trigger the loop hint, got:\n{joined}"
    );
}

/// End-to-end rendering through the binary: the user-facing stderr for
/// the repro must contain the hint, not `expected declaration`-only
/// noise.
#[test]
fn expr_bodied_if_repro_renders_hint_via_binary() {
    let dir = std::env::temp_dir().join("silt_round92_ifhint");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let main = dir.join("main.silt");
    std::fs::write(
        &main,
        "fn f(n: Int) -> Int = if n == 0 { 1 } else { 2 }\n\nfn main() {\n  ()\n}\n",
    )
    .unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_silt"))
        .args(["check", main.to_str().unwrap()])
        .output()
        .expect("silt check");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "`= if ...` must still be rejected; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("silt has no 'if' keyword") && stderr.contains("match cond"),
        "rendered diagnostic for the expression-bodied `if` repro must \
         contain the no-'if' hint; got:\n{stderr}"
    );
}

// ────────────────────────────────────────────────────────────────────
// Controls: accepted programs stay byte-identical
// ────────────────────────────────────────────────────────────────────

/// `if` is a legal identifier in silt. A variable genuinely named `if`
/// must keep parsing exactly as before in every touched position.
#[test]
fn variable_named_if_still_parses() {
    // (Statement position is locked by the golden case
    // tests/golden/frontend/hints/round92_parser_hint__variable_named_if_in_statement_position.silt.)
    // Expression-bodied fn whose body is a bare `if` reference followed
    // by a newline — NOT an expression-start token, so no hint fires
    // and the program still parses (name resolution is the
    // typechecker's job).
    parse_ok("fn f() = if\n\nfn main() {\n  ()\n}\n")
        .expect("bare `= if` body followed by newline must keep parsing");
    // Calling a variable named `if`: the body parses as a call, not a
    // bare identifier, so the hint cannot fire.
    parse_ok("fn f() = if(1)\n\nfn main() {\n  ()\n}\n")
        .expect("`= if(1)` call body must keep parsing");
}

/// Ordinary expression-bodied functions around the touched code path.
#[test]
fn ordinary_expression_bodies_still_parse() {
    parse_ok("fn square(x: Int) -> Int = x * x\n\nfn main() {\n  print(square(4))\n}\n")
        .expect("plain `=` bodies must keep parsing");
    parse_ok(
        "fn classify(n: Int) -> Int = match n {\n  0 -> 1,\n  _ -> 2,\n}\n\nfn main() {\n  ()\n}\n",
    )
    .expect("`= match ...` bodies must keep parsing");
}

/// Broader control: real example programs still parse cleanly through
/// the same public API.
#[test]
fn example_programs_still_parse() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for name in ["birthdays.silt", "calculator.silt", "budget.silt"] {
        let path = manifest.join("examples").join(name);
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        parse_ok(&src).unwrap_or_else(|e| panic!("examples/{name} must keep parsing: {e}"));
    }
}
