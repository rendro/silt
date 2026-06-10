//! Round 93 — qualified record construction must be a parse error, not
//! a silent misparse.
//!
//! Finding (BROKEN — silent wrong values): `util.Pt { x: 1, y: 2 }`
//! used to parse as the field access `util.Pt` (a type descriptor at
//! runtime) followed by a SEPARATE, silently-discarded anonymous-record
//! statement. `silt check` exited 0, the literal's fields vanished, and
//! `a == b` compared descriptors (printing `true` for records with
//! different fields). First-user trap: qualified ENUM constructors
//! (`shapes.Circle(2.0)`) work, so users naturally try the record form.
//!
//! Adding qualified record construction is a deferred design decision
//! ("no new syntax without a design decision"), so the fix emits a
//! clean parse error with an actionable hint instead. The guard is
//! conservative: it fires only on a Capitalized field access followed
//! on the SAME line by `{` whose contents start like record fields
//! (`ident :`) — trailing closures, blocks, match bodies, and
//! next-line braces are untouched.

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

/// Create a fresh temp project dir with the given (name, contents)
/// files and return the dir path.
fn temp_project(tag: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("silt_round93_qualrec_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (name, contents) in files {
        std::fs::write(dir.join(name), contents).unwrap();
    }
    dir
}

fn run_silt(args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_silt"))
        .args(args)
        .output()
        .expect("silt binary runs")
}

// ────────────────────────────────────────────────────────────────────
// The fix: qualified record construction is now a parse error
// ────────────────────────────────────────────────────────────────────

/// The original repro, at parser level: must produce the hint, naming
/// both the qualified form the user wrote and the import-based fix.
#[test]
fn qualified_record_literal_is_a_parse_error_with_hint() {
    let errs = parse_errors("fn main() {\n  let a = util.Pt { x: 1, y: 2 }\n  ()\n}\n");
    let joined = errs.join("\n");
    assert!(
        errs.iter()
            .any(|e| e.contains("qualified record construction")
                && e.contains("util.Pt { ... }")
                && e.contains("import util.{ Pt }")),
        "qualified record construction must die with the import hint, got:\n{joined}"
    );
}

/// Same through the strict (non-recovering) entry point.
#[test]
fn qualified_record_literal_strict_parse_errors() {
    let err = parse_ok("fn main() {\n  let a = util.Pt { x: 1, y: 2 }\n  ()\n}\n")
        .expect_err("qualified record construction must not parse");
    assert!(
        err.contains("qualified record construction") && err.contains("import util.{ Pt }"),
        "strict parse must render the hint, got:\n{err}"
    );
}

/// Deeper dotted path: the message echoes the full path the user wrote.
#[test]
fn qualified_record_literal_deep_path_echoes_path() {
    let errs = parse_errors("fn main() {\n  let a = a.b.Pt { x: 1 }\n  ()\n}\n");
    let joined = errs.join("\n");
    assert!(
        errs.iter()
            .any(|e| e.contains("qualified record construction") && e.contains("a.b.Pt { ... }")),
        "deep dotted qualified construction must echo the path, got:\n{joined}"
    );
}

/// End-to-end fail-before/pass-after: this exact project used to pass
/// `silt check` with exit 0 and print `true` for `a == b` on records
/// with different fields. It must now be a parse error (nonzero exit)
/// whose stderr carries the hint.
#[test]
fn qualified_record_literal_rejected_via_binary() {
    let dir = temp_project(
        "binary",
        &[
            ("util.silt", "pub type Pt { x: Int, y: Int }\n"),
            (
                "main.silt",
                "import util\n\nfn main() {\n  let a = util.Pt { x: 1, y: 2 }\n  let b = util.Pt { x: 9, y: 9 }\n  print(a == b)\n}\n",
            ),
        ],
    );
    let output = run_silt(&["check", dir.join("main.silt").to_str().unwrap()]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "qualified record construction must fail `silt check` (used to exit 0 \
         and silently discard the literal); stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("qualified record construction") && stderr.contains("import util.{ Pt }"),
        "rendered diagnostic must carry the import hint; got:\n{stderr}"
    );
}

/// The hint's own recommendation must be a working program: import the
/// type, construct it bare, and equality now sees the real fields.
#[test]
fn hinted_fix_actually_works() {
    let dir = temp_project(
        "fixed",
        &[
            ("util.silt", "pub type Pt { x: Int, y: Int }\n"),
            (
                "main.silt",
                "import util.{ Pt }\n\nfn main() {\n  let a = Pt { x: 1, y: 2 }\n  let b = Pt { x: 9, y: 9 }\n  print(a == b)\n  print(a == Pt { x: 1, y: 2 })\n}\n",
            ),
        ],
    );
    let output = run_silt(&["run", dir.join("main.silt").to_str().unwrap()]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "the hint's recommended fix must run; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert_eq!(
        stdout.replace(char::is_whitespace, ""),
        "falsetrue",
        "imported record construction must compare real field values"
    );
}

// ────────────────────────────────────────────────────────────────────
// Conservatism controls: everything that parsed before still parses
// ────────────────────────────────────────────────────────────────────

/// Trailing closures on module functions are the bread-and-butter form
/// the guard must never touch — `{ x -> ... }` has `ident ->`, not
/// `ident :`, after the brace.
#[test]
fn trailing_closure_on_module_function_still_parses() {
    parse_ok("import list\n\nfn main() {\n  let doubled = [1, 2, 3] |> list.map { x -> x * 2 }\n  print(doubled)\n}\n")
        .expect("pipe + trailing closure must keep parsing");
    parse_ok("fn main() {\n  let r = util.f { x -> x }\n  ()\n}\n")
        .expect("direct trailing closure on a module function must keep parsing");
}

/// Even a CAPITALIZED field access followed by a trailing closure stays
/// untouched (the brace contents are `ident ->`, not `ident :`).
#[test]
fn trailing_closure_on_capitalized_field_still_parses() {
    parse_ok("fn main() {\n  let r = util.Make { x -> x }\n  ()\n}\n")
        .expect("trailing closure after a capitalized field access must keep parsing");
}

/// A capitalized field access with the brace on the NEXT line is a
/// legitimate "field access, then separate statement" sequence and must
/// keep parsing — the guard is same-line only.
#[test]
fn capitalized_field_access_then_brace_on_next_line_still_parses() {
    parse_ok("fn main() {\n  let t = util.Pt\n  let r = { x: 1, y: 2 }\n  ()\n}\n")
        .expect("next-line anon record after a capitalized field access must keep parsing");
    parse_ok("fn main() {\n  let t = util.Pt\n  { x: 1, y: 2 }\n  ()\n}\n")
        .expect("next-line brace statement after a capitalized field access must keep parsing");
}

/// Match on a capitalized field-access scrutinee: the match body
/// `{ pattern -> ... }` must not be mistaken for record fields.
#[test]
fn match_on_capitalized_field_access_scrutinee_still_parses() {
    parse_ok("fn main() {\n  let r = match util.Pt {\n    _ -> 1,\n  }\n  print(r)\n}\n")
        .expect("match body after a capitalized field-access scrutinee must keep parsing");
}

/// Bare (unqualified) record construction is the supported form and
/// must keep working end-to-end: construct, access fields, compare.
#[test]
fn bare_record_create_still_works_runtime() {
    let dir = temp_project(
        "bare",
        &[(
            "main.silt",
            "type Pt { x: Int, y: Int }\n\nfn main() {\n  let p = Pt { x: 1, y: 2 }\n  print(p.x)\n  print(p.y)\n  print(p == Pt { x: 1, y: 2 })\n}\n",
        )],
    );
    let output = run_silt(&["run", dir.join("main.silt").to_str().unwrap()]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "bare record construction must keep running; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert_eq!(
        stdout.replace(char::is_whitespace, ""),
        "12true",
        "bare record construction must build a real record"
    );
}

/// Qualified ENUM constructors are supported and must keep working —
/// they are exactly why users expect the record form to work too.
#[test]
fn qualified_enum_constructor_still_works_runtime() {
    let dir = temp_project(
        "enum",
        &[
            (
                "shapes.silt",
                "pub type Shape {\n  Circle(Float),\n  Rect(Float, Float)\n}\n",
            ),
            (
                "main.silt",
                "import shapes\n\nfn main() {\n  let c = shapes.Circle(2.0)\n  print(c == shapes.Circle(2.0))\n  print(c == shapes.Rect(1.0, 2.0))\n}\n",
            ),
        ],
    );
    let output = run_silt(&["run", dir.join("main.silt").to_str().unwrap()]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "qualified enum constructors must keep running; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert_eq!(
        stdout.replace(char::is_whitespace, ""),
        "truefalse",
        "qualified enum constructors must build real variants"
    );
}

/// Record update syntax `expr.{ field: value }` shares the Dot arm and
/// must keep parsing.
#[test]
fn record_update_still_parses() {
    parse_ok("type Pt { x: Int, y: Int }\n\nfn main() {\n  let p = Pt { x: 1, y: 2 }\n  let q = p.{ x: 9 }\n  print(q.x)\n}\n")
        .expect("record update must keep parsing");
}

/// Lowercase field access followed by a same-line record-ish brace is
/// NOT the qualified-construction shape; it keeps its previous
/// behavior (whatever that was — here, a parse error in an arg list,
/// but specifically NOT the qualified-record hint).
#[test]
fn lowercase_field_access_never_gets_qualified_record_hint() {
    let errs = parse_errors("fn main() {\n  let a = util.pt { x: 1 }\n  ()\n}\n");
    let joined = errs.join("\n");
    assert!(
        !joined.contains("qualified record construction"),
        "guard must require a Capitalized field, got:\n{joined}"
    );
}
