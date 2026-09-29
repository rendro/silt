// ════════════════════════════════════════════════════════════════════
// AUDIT ROUND-23 FOLLOW-UP: match-scrutinee record-literal disambiguation
//
// Before this fix, `match Pair { left: 1, right: "hi" } { Pair { .. } -> ... }`
// failed to parse. The parser's `in_match_scrutinee` flag suppressed
// *all* `{` consumption inside the scrutinee so the match-body `{`
// wasn't accidentally consumed — but that also blocked record literals.
// Users had to parenthesize (`match (Pair { ... }) { ... }`) or
// name-bind first (`let p = Pair { ... }; match p { ... }`).
//
// Fix: bounded lookahead in `src/parser.rs::scrutinee_lbrace_is_record_literal`
// distinguishes the two cases by peeking past the `{`:
//   - `{ Ident Colon ...`  → record literal (allow)
//   - `{ Pattern Arrow ...` → match body (suppress)
//
// These tests lock each disambiguation case end-to-end and guard
// against regressions that would either re-break record-literal
// scrutinees or accidentally let match-body `{` be consumed.
// ════════════════════════════════════════════════════════════════════

use silt::typechecker;
use silt::types::Severity;
use std::io::Write;
use std::process::Command;

fn parse_ok(input: &str) -> Result<(), String> {
    let tokens = silt::lexer::Lexer::new(input)
        .tokenize()
        .map_err(|e| format!("lex: {e:?}"))?;
    silt::parser::Parser::new(tokens)
        .parse_program()
        .map(|_| ())
        .map_err(|e| format!("parse: {e:?}"))
}

fn type_errors(input: &str) -> Vec<String> {
    let tokens = silt::lexer::Lexer::new(input)
        .tokenize()
        .expect("lexer error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    let errors = typechecker::check(&mut program);
    errors
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

fn run_silt(src: &str) -> (String, String, i32) {
    // Write to a temp file rather than piping to `/dev/stdin` — the
    // latter doesn't exist on Windows. Unique-per-test-process filename
    // so parallel test runs don't collide.
    let path = std::env::temp_dir().join(format!(
        "silt_match_scrutinee_test_{}_{}.silt",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::File::create(&path)
        .expect("create temp file")
        .write_all(src.as_bytes())
        .expect("write temp file");
    let out = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("run")
        .arg(&path)
        .output()
        .expect("spawn silt");
    let _ = std::fs::remove_file(&path);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

#[test]
fn test_record_literal_as_match_scrutinee_parses() {
    // The repro from the round-23 audit finding. Before the lookahead
    // fix this failed with "expected ->, found :" on the `left:` colon.
    parse_ok(
        r#"
type Pair(a, b) { left: a, right: b }
fn demo() -> Int {
  match Pair { left: 1, right: "hi" } {
    Pair { left: x, right: y } -> x
  }
}
"#,
    )
    .expect("record literal in match scrutinee must parse");
}

#[test]
fn test_record_literal_scrutinee_runs_end_to_end() {
    // Full compile+run path: parser accepts the scrutinee, typechecker
    // accepts the record-pattern arm, VM produces the expected value.
    let (stdout, stderr, code) = run_silt(
        r#"
type Pair(a, b) { left: a, right: b }
fn main() {
  match Pair { left: 1, right: "hi" } {
    Pair { left: x, right: y } -> println(x)
  }
}
"#,
    );
    assert_eq!(code, 0, "silt should exit 0; stderr={stderr:?}");
    assert_eq!(stdout.trim(), "1", "expected '1', got {stdout:?}");
}

#[test]
fn test_record_literal_on_comparison_rhs_in_scrutinee() {
    // Comparison operators in scrutinee already work (cf. round-20
    // comparison-operator fix). Here the RHS is a record literal, so
    // the helper must fire on the RHS's primary parse as well.
    parse_ok(
        r#"
type Pair(a, b) { left: a, right: b }
fn demo(p) -> String {
  match p == Pair { left: 1, right: "x" } {
    true -> "equal"
    false -> "not"
  }
}
"#,
    )
    .expect("record literal on comparison RHS in scrutinee must parse");
}

#[test]
fn test_nested_match_record_scrutinee() {
    // Inner match scrutinee also uses a record literal. The
    // in_match_scrutinee flag is save/restored around nested matches
    // (src/parser.rs parse_match_expr), so the helper fires correctly
    // at each nesting level.
    parse_ok(
        r#"
type Pair(a, b) { left: a, right: b }
fn demo() -> Int {
  match (match Pair { left: 1, right: 2 } { Pair { left: x, right: y } -> x + y }) {
    n -> n + 10
  }
}
"#,
    )
    .expect("nested match with record-literal inner scrutinee must parse");
}

#[test]
fn test_bare_ident_scrutinee_still_works() {
    // Regression lock: lowercase/plain-ident scrutinee never enters the
    // constructor branch (Ident name matching `is_constructor`), so the
    // helper can't fire. Still-working case must stay working.
    parse_ok(
        r#"
fn demo(x: Int) -> String {
  match x {
    0 -> "zero"
    _ -> "other"
  }
}
"#,
    )
    .expect("bare-ident scrutinee must still parse");
}

#[test]
fn test_bare_constructor_scrutinee_still_works() {
    // Regression lock: bare constructor (no record/call args) as
    // scrutinee. Helper sees `{` is followed by `Pattern Arrow` (not
    // `Ident Colon`) → returns false → match body consumes the `{`.
    parse_ok(
        r#"
type Status { On, Off }
fn demo(s: Status) -> Int {
  match s {
    On -> 1
    Off -> 0
  }
}
"#,
    )
    .expect("bare constructor scrutinee must still parse");
}

#[test]
fn test_match_body_not_misidentified_as_record() {
    // Regression lock: the classic `match Ctor { x -> x }` (bare ctor
    // scrutinee, match body with ident pattern). Helper must see
    // `{ x -> ...` → second meaningful token is `Arrow`, not `Colon`
    // → returns false → match body consumes the `{`.
    parse_ok(
        r#"
type Thing { Foo, Bar }
fn demo(t: Thing) -> Int {
  match t {
    x -> 1
  }
}
"#,
    )
    .expect("match body with ident pattern must not be misidentified");
}

#[test]
fn test_record_literal_scrutinee_typechecks() {
    // Round trip: parser accepts record literal in scrutinee; typechecker
    // verifies no spurious errors. Tests the seam between parser and
    // typechecker when the scrutinee evaluates to a record value.
    let errs = type_errors(
        r#"
type Pair(a, b) { left: a, right: b }
fn demo() -> Int {
  match Pair { left: 1, right: 2 } {
    Pair { left: x, right: y } -> x + y
  }
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected zero type errors for record-literal scrutinee, got: {errs:?}"
    );
}
