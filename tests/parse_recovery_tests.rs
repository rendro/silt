//! Regression tests for LATENT parser-recovery cascade fix (Option B).
//!
//! Scenario: a malformed `fn f(...)` body makes the parser drop the entire
//! declaration. Every later reference to `f` then produces "undefined
//! variable 'f'" — one parse error cascades into N bogus type errors.
//!
//! Fix: when parser recovery fires inside a `fn` declaration, salvage
//! whatever header (name, params, return type) parsed cleanly and emit a
//! recovery-stub `FnDecl` (marked with `is_recovery_stub = true`). The
//! typechecker binds the stub name as a normal function but:
//!   * does not type-check its (empty) body,
//!   * at call sites, returns a fresh type variable without emitting
//!     arity / arg-type cascade errors.
//!
//! These tests lock the following guarantees:
//!   * downstream references to a recovered `fn` do NOT produce "undefined
//!     variable" errors,
//!   * the stub's empty body does NOT generate "missing return" /
//!     "unreachable" / "unused binding" errors,
//!   * malformed `fn` with no name does NOT create a stub (nothing to
//!     suppress — there's no usable name),
//!   * nested parser recoveries do not cascade stubs,
//!   * unrelated (non-cascade) type errors in the same file still fire.

use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::types::Severity;

/// Parse (recovering) + typecheck and return (parse_error_messages,
/// type_error_messages). Only hard errors are returned from the typechecker.
fn parse_recover_and_type(input: &str) -> (Vec<String>, Vec<String>) {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let (mut program, parse_errors) = Parser::new(tokens).parse_program_recovering();
    let parse_msgs: Vec<String> = parse_errors.into_iter().map(|e| e.message).collect();
    let type_errors = typechecker::check(&mut program);
    let type_msgs: Vec<String> = type_errors
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect();
    (parse_msgs, type_msgs)
}

#[test]
fn test_parse_recovery_preserves_downstream_references() {
    // Malformed body for `f` — `1 +` is an incomplete expression. Without
    // the fix this drops the whole `fn f` and then `f(1, 2)` in `main`
    // produces "undefined variable 'f'".
    let src = r#"
fn f(a, b) {
  let x = 1 +
  let y = 2
  x + y
}

fn main() {
  f(1, 2)
}
"#;
    let (parse_msgs, type_msgs) = parse_recover_and_type(src);

    // At least one parse error should be reported (the malformed body).
    assert!(
        !parse_msgs.is_empty(),
        "expected at least one parse error, got none"
    );

    // ZERO "undefined variable 'f'" type errors — this is the cascade we fix.
    let undefined_f = type_msgs
        .iter()
        .filter(|m| m.contains("undefined variable 'f'"))
        .count();
    assert_eq!(
        undefined_f, 0,
        "expected 0 'undefined variable f' errors, got {undefined_f}: parse={parse_msgs:?}, type={type_msgs:?}"
    );
}

#[test]
fn test_parse_recovery_stub_does_not_generate_body_errors() {
    // With a recovery stub, the synthetic empty body must NOT trip
    // "missing return", "unreachable", "return type mismatch",
    // "unused binding", etc.
    let src = r#"
fn f(a, b) -> Int {
  let x = 1 +
}

fn main() {
  f(1, 2)
}
"#;
    let (_parse_msgs, type_msgs) = parse_recover_and_type(src);

    for msg in &type_msgs {
        assert!(
            !msg.contains("return type mismatch"),
            "stub body produced return type mismatch: {msg}"
        );
        assert!(
            !msg.contains("unreachable"),
            "stub body produced unreachable: {msg}"
        );
        assert!(
            !msg.contains("missing return"),
            "stub body produced missing return: {msg}"
        );
        assert!(
            !msg.contains("undefined variable 'f'"),
            "stub body still cascades: {msg}"
        );
    }
}

#[test]
fn test_parse_recovery_unnamed_fn_does_not_create_stub() {
    // `fn (bad)` — no identifier — should NOT create a stub (there's no
    // name to bind). We verify by ensuring the subsequent valid `fn` still
    // typechecks cleanly and no "undefined variable 'g'" for a name that
    // was never declared slips through.
    let src = r#"
fn (bad) {
  42
}

fn g() -> Int {
  1
}

fn main() {
  g()
}
"#;
    let (parse_msgs, type_msgs) = parse_recover_and_type(src);
    assert!(
        !parse_msgs.is_empty(),
        "expected parse error for unnamed fn, got none"
    );

    // `g` is a normal fn — must not be affected.
    for msg in &type_msgs {
        assert!(
            !msg.contains("undefined variable 'g'"),
            "subsequent valid fn broken: {msg}"
        );
    }
}

#[test]
fn test_parse_recovery_nested_recovery_does_not_cascade() {
    // Two back-to-back malformed fns. Both should get stubs so that
    // neither `a` nor `b` produces "undefined variable" at the call sites.
    // The depth guard ensures we never stub-inside-a-stub: if recovery
    // fires while already inside the recovery path, we bail.
    let src = r#"
fn a(x) {
  let p = 1 +
}

fn b(y) {
  let q = 2 +
}

fn main() {
  let r = a(1)
  let s = b(2)
  r
}
"#;
    let (parse_msgs, type_msgs) = parse_recover_and_type(src);
    assert!(
        !parse_msgs.is_empty(),
        "expected parse errors for both malformed fns"
    );

    // No cascade for either name.
    for name in ["a", "b"] {
        let needle = format!("undefined variable '{name}'");
        let count = type_msgs.iter().filter(|m| m.contains(&needle)).count();
        assert_eq!(
            count, 0,
            "{name} cascaded: parse={parse_msgs:?}, type={type_msgs:?}"
        );
    }
}

#[test]
fn test_parse_recovery_stub_suppresses_arity_mismatch_at_call_site() {
    // The stub was recovered with two params. A call site passes one
    // argument — this is a cascade error (the user's real problem is the
    // malformed body, not the call) and must be suppressed. Similarly,
    // passing too many arguments must not fire either.
    let src = r#"
fn f(a, b) {
  let x = 1 +
}

fn main() {
  f(1)
  f(1, 2, 3)
  f("not", "int")
}
"#;
    let (parse_msgs, type_msgs) = parse_recover_and_type(src);
    assert!(!parse_msgs.is_empty(), "expected parse errors");

    // No cascaded arity/type errors from calls to the stub.
    for msg in &type_msgs {
        assert!(
            !msg.contains("undefined variable 'f'"),
            "cascade to f: {msg}"
        );
        assert!(
            !msg.contains("function expects"),
            "stub call produced arity error: {msg}"
        );
        assert!(
            !msg.contains("function arity mismatch"),
            "stub call produced unify-arity error: {msg}"
        );
    }
}

#[test]
fn test_parse_recovery_stub_empty_body_does_not_trip_unit_mismatch() {
    // With a declared return type of `Int` and an empty stub body (which
    // would otherwise be inferred as `Unit`), the typechecker must not
    // emit a Unit-vs-Int mismatch for the synthetic body.
    let src = r#"
fn f(a, b) -> Int {
  let bad = 1 +
}

fn main() -> Int {
  f(1, 2)
}
"#;
    let (_parse_msgs, type_msgs) = parse_recover_and_type(src);
    // The stub body must NOT be type-checked, so no `type mismatch:
    // expected Int, got ()` (Unit is rendered `()` by the Type Display impl).
    for msg in &type_msgs {
        assert!(
            !(msg.contains("type mismatch") && msg.contains("Int") && msg.contains("()")),
            "stub body Unit leaked to declared Int: {msg}"
        );
    }
    // And as always, no cascade.
    for msg in &type_msgs {
        assert!(
            !msg.contains("undefined variable 'f'"),
            "cascade: {msg}"
        );
    }
}

#[test]
fn test_parse_recovery_real_type_errors_still_fire() {
    // A file with both a parse-recovered `fn` AND a genuine type error
    // elsewhere. The genuine type error must still be reported — we
    // only suppress errors that involve stubs.
    let src = r#"
fn broken(x) {
  let p = 1 +
}

fn good() -> Int {
  let v: Int = "hi"
  v
}

fn main() {
  good()
}
"#;
    let (parse_msgs, type_msgs) = parse_recover_and_type(src);
    assert!(!parse_msgs.is_empty(), "expected parse error");

    // The `let v: Int = "hi"` must still produce a type mismatch.
    // The exact phrase originates from src/typechecker/mod.rs:464
    // (Typechecker::unify _ arm) — `type mismatch: expected Int, got String`.
    let has_mismatch = type_msgs
        .iter()
        .any(|m| m.contains("type mismatch: expected Int, got String"));
    assert!(
        has_mismatch,
        "expected exact type-mismatch phrase from typechecker/mod.rs:464, got: {type_msgs:?}"
    );
}
