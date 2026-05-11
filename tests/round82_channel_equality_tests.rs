//! Round 82 regression tests for `TYPE-LATENT-1` —
//! `Type::Channel(_)` was rejected by `is_valid_compare_operand` even
//! though `Value::Channel` implements identity-based equality at
//! runtime (see `src/value.rs:~1717`,
//! `(Value::Channel(a), Value::Channel(b)) => a.id == b.id`).
//!
//! The fix in `src/typechecker/inference.rs` adds a `Type::Channel(_)`
//! arm to `is_valid_compare_operand` that returns true for the
//! `is_equality` branch only — matching the `AnonRecord` / `Tuple` /
//! `Map` / `Set` / `Bool` / `Unit` treatment. Ordering (`<`, `<=`,
//! `>`, `>=`) is still rejected: channel ids are identity tokens, not
//! a meaningful well-order.
//!
//! Each test below FAILS on the unfixed code (typecheck rejects with
//! "operator '==' requires a comparable type, got 'Channel(Int)'") and
//! PASSES post-fix (typecheck accepts AND runtime returns the right
//! identity-based answer).
//!
//! Design rationale: silt's "one way to do things" principle — the
//! typecheck domain for `==` must agree with the runtime domain.
//! Diverging the two surfaces a phantom rejection on a well-defined
//! runtime operation.

use std::process::Command;

use silt::typechecker;
use silt::types::Severity;

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

/// Drive the full lex → parse → typecheck → compile → VM pipeline via
/// the `silt run` CLI and return (stdout, stderr, success).
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_round82_channel_eq_{label}.silt"));
    std::fs::write(&tmp, src).expect("write temp file");
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .arg("run")
        .arg(&tmp)
        .output()
        .expect("spawn silt run");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (stdout, stderr, out.status.success())
}

fn run_silt_ok(label: &str, src: &str) -> String {
    let (stdout, stderr, ok) = run_silt_raw(label, src);
    assert!(
        ok,
        "silt run should succeed for {label}; stdout={stdout}, stderr={stderr}"
    );
    stdout
}

// ── 1. Typecheck accepts `c1 == c2` on two channels ──────────────────

/// Two freshly created channels typecheck under `==`. Pre-fix this
/// failed at typecheck with
/// "operator '==' requires a comparable type, got 'Channel(Int)'".
#[test]
fn channel_eq_typechecks() {
    let errs = type_errors(
        r#"
import channel
fn main() {
    let c1 = channel.new(10)
    let c2 = channel.new(10)
    println(c1 == c2)
}
"#,
    );
    assert!(
        errs.is_empty(),
        "`==` on two Channel(_) values should typecheck; got: {errs:?}"
    );
}

// ── 2. Runtime identity-eq semantics ─────────────────────────────────

/// Two distinct channels have different identities — `c1 == c2`
/// returns `false` at runtime.
#[test]
fn channel_eq_distinct_runs_false() {
    let out = run_silt_ok(
        "distinct_eq_false",
        r#"
import channel
fn main() {
    let c1 = channel.new(10)
    let c2 = channel.new(10)
    println(c1 == c2)
}
"#,
    );
    assert_eq!(
        out.lines().next().unwrap_or("").trim(),
        "false",
        "expected two freshly-created channels to compare unequal; full out={out:?}"
    );
}

/// A channel compares equal to itself (`c1 == c1` returns `true`).
/// This is the reflexivity lock for `Value::Channel`'s identity-eq.
#[test]
fn channel_eq_self_runs_true() {
    let out = run_silt_ok(
        "self_eq_true",
        r#"
import channel
fn main() {
    let c1 = channel.new(10)
    println(c1 == c1)
}
"#,
    );
    assert_eq!(
        out.lines().next().unwrap_or("").trim(),
        "true",
        "expected a channel to be equal to itself; full out={out:?}"
    );
}

// ── 3. Typecheck rejects ordering ────────────────────────────────────

/// Ordering (`<`) on channels must still be rejected at typecheck —
/// channel ids are identity tokens with no meaningful well-order. The
/// diagnostic should reference "ordering" or "compare" (the
/// non-equality message uses "ordering comparison" in the operand-domain
/// path), NOT "equality" wording.
#[test]
fn channel_ordering_rejected_by_typecheck() {
    let errs = type_errors(
        r#"
import channel
fn main() {
    let c1 = channel.new(10)
    let c2 = channel.new(10)
    println(c1 < c2)
}
"#,
    );
    assert!(
        !errs.is_empty(),
        "`<` on Channel(_) values must be rejected at typecheck"
    );
    // The diagnostic mentions either the non-equality operand-domain
    // enumeration (which excludes Channel) or the operator itself.
    // It must NOT mention `==`/`!=`/"equality" — that would be a
    // mis-routed diagnostic.
    assert!(
        errs.iter().any(|e| {
            e.contains("<")
                || e.contains("Int, Float, ExtFloat, String, List, Range, Record, or Variant")
                || e.contains("Channel")
        }),
        "expected an ordering-domain diagnostic mentioning `<` or the ordering enumeration; got: {errs:?}"
    );
    assert!(
        !errs.iter().any(|e| e.contains("equality")),
        "ordering rejection must not be worded as an equality rejection; got: {errs:?}"
    );
}

// ── 4. `!=` direction (the dual of `==`) ─────────────────────────────

/// `!=` is the dual of `==`. Two distinct channels produce `true` for
/// `!=` and `c1 != c1` produces `false`.
#[test]
fn channel_neq_distinct_runs_true() {
    let errs = type_errors(
        r#"
import channel
fn main() {
    let c1 = channel.new(10)
    let c2 = channel.new(10)
    println(c1 != c2)
}
"#,
    );
    assert!(
        errs.is_empty(),
        "`!=` on two Channel(_) values should typecheck; got: {errs:?}"
    );

    let out = run_silt_ok(
        "neq_distinct_true",
        r#"
import channel
fn main() {
    let c1 = channel.new(10)
    let c2 = channel.new(10)
    println(c1 != c2)
}
"#,
    );
    assert_eq!(
        out.lines().next().unwrap_or("").trim(),
        "true",
        "expected `!=` on two distinct channels to return true; full out={out:?}"
    );
}
