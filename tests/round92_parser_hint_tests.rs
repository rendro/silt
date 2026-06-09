//! Round 92 — parser diagnostic fixes.
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
// Finding 1: postfix-indexing diagnostic must name only real functions
// ────────────────────────────────────────────────────────────────────

/// Source-grep lock: `char_at` must not reappear anywhere in the
/// parser. (The registry has no such function; recommending it sends
/// users to a second error.)
#[test]
fn parser_source_never_mentions_char_at() {
    assert!(
        !include_str!("../src/parser.rs").contains("char_at"),
        "src/parser.rs must not mention `char_at` — no such builtin exists; \
         the indexing diagnostic should point at string.slice(s, i, i + 1)"
    );
}

/// Belt-and-suspenders: the recommended replacement must actually be a
/// registered builtin, so the diagnostic can never dangle again.
#[test]
fn recommended_replacements_exist_in_string_registry() {
    let registry = include_str!("../src/typechecker/builtins/string.rs");
    assert!(
        registry.contains("string.slice"),
        "string.slice must exist in the builtin registry — it is what the \
         postfix-indexing diagnostic recommends"
    );
    assert!(
        !registry.contains("char_at"),
        "if string.char_at is ever added, restore it to the indexing \
         diagnostic and update these locks"
    );
}

/// Real-path lock: parse `xs[0]` through the public API and check the
/// rendered error.
#[test]
fn indexing_error_recommends_string_slice_not_char_at() {
    let errs = parse_errors("fn main() {\n  let xs = [1, 2, 3]\n  let _r = xs[0]\n  ()\n}\n");
    let joined = errs.join("\n");
    assert!(
        errs.iter()
            .any(|e| e.contains("postfix indexing is not supported")),
        "expected the postfix-indexing diagnostic, got:\n{joined}"
    );
    assert!(
        errs.iter().any(|e| e.contains("list.get(xs, i)")
            && e.contains("map.get(m, k)")
            && e.contains("string.slice(s, i, i + 1)")),
        "indexing diagnostic must recommend list.get / map.get / \
         string.slice(s, i, i + 1), got:\n{joined}"
    );
    assert!(
        !joined.contains("char_at"),
        "indexing diagnostic must not mention the nonexistent \
         string.char_at, got:\n{joined}"
    );
}

/// The recommendation must itself be a working program: getting the
/// i-th character via `string.slice(s, i, i + 1)` parses, typechecks,
/// and runs (verified end-to-end through the binary).
#[test]
fn recommended_slice_call_runs() {
    let dir = std::env::temp_dir().join("silt_round92_slice");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let main = dir.join("main.silt");
    std::fs::write(
        &main,
        "import string\n\nfn main() {\n  let s = \"hello\"\n  let i = 1\n  print(string.slice(s, i, i + 1))\n}\n",
    )
    .unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_silt"))
        .args(["run", main.to_str().unwrap()])
        .output()
        .expect("silt run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "the diagnostic's recommended replacement must run; \
         stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert_eq!(
        stdout.trim(),
        "e",
        "string.slice(s, i, i + 1) should yield the i-th character"
    );
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
    // Statement position: bind and use.
    parse_ok("fn main() {\n  let if = 3\n  print(if + 1)\n}\n")
        .expect("`if` as a let-bound variable must keep parsing");
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
