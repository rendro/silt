//! Regression tests for the "did you mean ...?" suggestion hint on
//! type-checker diagnostics. Closes round-17 deferred finding #4.
//!
//! The type checker should append `help: did you mean \`<cand>\`?` to:
//!   - undefined variable '<typo>'
//!   - unknown function '<typo>' on module '<mod>'
//!
//! whenever a close edit-distance candidate exists in scope. When no
//! candidate is close enough, the bare error survives unchanged.
//!
//! These tests assert on the RAW TypeError message (not the rendered
//! SourceError) because SourceError rendering is covered by
//! `tests/cli_test_rendering_tests.rs`. The convention the message
//! embeds is `\nhelp: did you mean \`<cand>\`?` — the newline triggers
//! the multi-line body path in `src/errors.rs::Display`, which then
//! renders the second line as `= help: did you mean \`<cand>\`?`.

use silt::typechecker;
use silt::types::Severity;

// ── Helpers ─────────────────────────────────────────────────────────

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

fn first_error_containing(errs: &[String], needle: &str) -> Option<String> {
    errs.iter().find(|e| e.contains(needle)).cloned()
}

// ── Undefined variable hints ───────────────────────────────────────

#[test]
fn test_undefined_variable_suggests_close_match() {
    // `pintln` is 1 edit away from the stdlib `println` — the
    // typechecker should offer it as a hint. Mutation-verified: comment
    // out the suggestion append in `format_undefined_variable_message`
    // and this test fails on the `did you mean` assertion.
    let errs = type_errors(r#"fn main() { pintln("hello") }"#);
    let msg = first_error_containing(&errs, "undefined variable 'pintln'")
        .expect("expected undefined variable error");
    assert!(
        msg.contains("did you mean"),
        "expected 'did you mean' hint, got: {msg:?}"
    );
    assert!(
        msg.contains("`println`"),
        "expected suggestion to name `println`, got: {msg:?}"
    );
    // The hint must land on its own line so SourceError renders it as
    // a `= help:` continuation below the caret.
    assert!(
        msg.contains('\n'),
        "expected newline between header and hint, got: {msg:?}"
    );
    assert!(
        msg.contains("help: did you mean `println`?"),
        "expected exact `help: did you mean` body line, got: {msg:?}"
    );
}

#[test]
fn test_undefined_variable_omits_suggestion_when_too_far() {
    // `xyzzy_completely_unrelated` is far enough from every in-scope
    // name that no candidate passes the edit-distance threshold. The
    // plain error must survive without a misleading hint.
    let errs = type_errors(r#"fn main() { xyzzy_completely_unrelated() }"#);
    let msg = first_error_containing(&errs, "undefined variable")
        .expect("expected undefined variable error");
    assert!(
        !msg.contains("did you mean"),
        "did not expect 'did you mean' hint, got: {msg:?}"
    );
}

#[test]
fn test_undefined_variable_never_suggests_the_typo_itself() {
    // Guard against a regression where `suggest_similar` stopped
    // filtering exact matches of the typo against the candidate set —
    // e.g. a scope-walk that found the typo name in an outer scope
    // would otherwise suggest it back to itself.
    //
    // We can't easily construct a scope where the typo resolves but
    // lookup fails, so we use the closest repro: a typo whose only
    // "close" candidate set IS the typo itself via a lambda param.
    // The important assertion is that the hint (if emitted) is not
    // the exact typo string.
    let errs = type_errors(
        r#"
fn main() {
  let zz = 1
  zzzz
}
"#,
    );
    let msg = first_error_containing(&errs, "undefined variable 'zzzz'")
        .expect("expected undefined variable error");
    if let Some(idx) = msg.find("did you mean") {
        let tail = &msg[idx..];
        assert!(
            !tail.contains("`zzzz`"),
            "suggestion must never echo the typo itself, got: {msg:?}"
        );
    }
}

#[test]
fn test_suggestion_uses_locally_scoped_variable_name() {
    // Local let-bindings must appear in the candidate set — not only
    // top-level decls and stdlib builtins. Here `banana` is a local,
    // and `bananna` is 1 edit away.
    let errs = type_errors(
        r#"
fn main() {
  let banana = 1
  bananna
}
"#,
    );
    let msg = first_error_containing(&errs, "undefined variable 'bananna'")
        .expect("expected undefined variable error");
    assert!(
        msg.contains("did you mean"),
        "expected 'did you mean' hint for local var typo, got: {msg:?}"
    );
    assert!(
        msg.contains("`banana`"),
        "expected suggestion to name the local `banana`, got: {msg:?}"
    );
}

#[test]
fn test_suggestion_uses_function_parameter_name() {
    // Fn parameters land in the inner scope's env chain. Typoing a
    // parameter name inside the body must still suggest it.
    let errs = type_errors(
        r#"
fn greet(name: String) {
  println(namee)
}
fn main() { greet("hi") }
"#,
    );
    let msg = first_error_containing(&errs, "undefined variable 'namee'")
        .expect("expected undefined variable error");
    assert!(
        msg.contains("did you mean"),
        "expected 'did you mean' hint for fn param typo, got: {msg:?}"
    );
    assert!(
        msg.contains("`name`"),
        "expected suggestion to name the parameter `name`, got: {msg:?}"
    );
}

// ── Module function hints ──────────────────────────────────────────

#[test]
fn test_unknown_module_function_suggests_close_match() {
    // `list.lenght` → `list.length`, edit distance 2.
    let errs = type_errors(
        r#"
import list
fn main() {
  let xs = [1, 2, 3]
  list.lenght(xs)
}
"#,
    );
    let msg = first_error_containing(&errs, "unknown function 'lenght'")
        .expect("expected unknown module function error");
    assert!(
        msg.contains("did you mean"),
        "expected 'did you mean' hint, got: {msg:?}"
    );
    assert!(
        msg.contains("`length`"),
        "expected suggestion to name `length`, got: {msg:?}"
    );
}

#[test]
fn test_unknown_module_function_omits_suggestion_when_too_far() {
    // `list.zzzzzzzzz` has no close match in the list module.
    let errs = type_errors(
        r#"
import list
fn main() {
  let xs = [1]
  list.zzzzzzzzz(xs)
}
"#,
    );
    let msg = first_error_containing(&errs, "unknown function 'zzzzzzzzz'")
        .expect("expected unknown module function error");
    assert!(
        !msg.contains("did you mean"),
        "did not expect 'did you mean' hint, got: {msg:?}"
    );
}

#[test]
fn test_unknown_module_function_suggests_across_modules() {
    // Tune-check: a string-module typo should get a string-module
    // suggestion, not something from list (which has different
    // functions). `string.toupper` → `string.to_upper`.
    let errs = type_errors(
        r#"
import string
fn main() {
  string.toupper("hi")
}
"#,
    );
    let msg = first_error_containing(&errs, "unknown function 'toupper'")
        .expect("expected unknown module function error");
    assert!(
        msg.contains("did you mean"),
        "expected 'did you mean' hint, got: {msg:?}"
    );
    assert!(
        msg.contains("`to_upper`"),
        "expected suggestion to name `to_upper`, got: {msg:?}"
    );
}

// ── Rendered diagnostic end-to-end ─────────────────────────────────

#[test]
fn test_rendered_undefined_variable_emits_help_continuation_line() {
    // End-to-end: the rendered SourceError (via `silt check` path)
    // must carry a `= help:` line below the caret for the typo case.
    // This locks the errors.rs render path: `help: ` body-line prefix
    // becomes `= help:` rather than `= note: help:`.
    use silt::errors::SourceError;
    use silt::lexer::Lexer;
    use silt::parser::Parser;

    let source = r#"fn main() { pintln("hi") }"#;
    let tokens = Lexer::new(source).tokenize().expect("lex");
    let mut program = Parser::new(tokens).parse_program().expect("parse");
    let errors = typechecker::check(&mut program);
    let err = errors
        .iter()
        .find(|e| e.message.contains("undefined variable 'pintln'"))
        .expect("typecheck should flag pintln");
    let rendered = format!(
        "{}",
        SourceError::from_type_error(err, source, "t.silt")
    );
    assert!(
        rendered.contains("= help:"),
        "expected rendered '= help:' continuation, got:\n{rendered}"
    );
    assert!(
        rendered.contains("did you mean `println`?"),
        "expected rendered 'did you mean' hint, got:\n{rendered}"
    );
    // Ordering: caret line comes before the help line.
    let caret_idx = rendered
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains('^') && l.contains("undefined variable"))
        .map(|(i, _)| i)
        .expect("caret line missing");
    let help_idx = rendered
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains("= help:"))
        .map(|(i, _)| i)
        .expect("help line missing");
    assert!(
        caret_idx < help_idx,
        "caret must come before = help:, got:\n{rendered}"
    );
}
