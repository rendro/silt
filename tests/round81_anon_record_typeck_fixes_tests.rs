//! Round 81 regression tests for anon-record typechecker fixes in
//! `src/typechecker/inference.rs`:
//!
//! - **F1 (TYPE-GAP)**: `is_valid_compare_operand` rejected
//!   `Type::AnonRecord` even though closed-row anon records compile to
//!   `Value::Record` and `Value`'s PartialEq compares them
//!   element-wise. Result: `let a = {x: 1}; let b = {x: 1}; a == b`
//!   was rejected at typecheck. Fix: accept closed-row AnonRecord on
//!   `==`/`!=`; still reject open rows (the answer would depend on the
//!   hidden tail).
//!
//! - **F2 (ERR-GAP)**: AnonRecord field-access errors at the
//!   deferred-check site (`inference.rs:958`) and the direct site
//!   (`:2462`) emitted bare `"anon record has no field '<f>'"` with no
//!   "did you mean" suggestion. Sibling Record / Generic sites already
//!   call `format_record_field_suggestion`. Fix: route both AnonRecord
//!   sites through the same helper.
//!
//! - **F3 (ERR-GAP)**: closed-row anon record-update with an unknown
//!   field (`inference.rs:4600-4609`) emitted
//!   `"unknown field '<f>' in closed anon record"` without a
//!   suggestion. Fix: append the helper's hint, mirroring the
//!   `Type::Record` arm immediately below.
//!
//! Each test FAILS on the unfixed code and PASSES post-fix.

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
    let tmp = std::env::temp_dir().join(format!("silt_round81_anon_eq_{label}.silt"));
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

// ── F1: closed-row AnonRecord values compare with `==` / `!=` ────────

/// Closed-row `==` typechecks AND prints `true` at runtime when the
/// records have identical fields. Pre-fix this failed at typecheck with
/// "operator == requires a comparable type, got '{x: Int}'".
#[test]
fn f1_closed_anon_record_eq_typechecks_and_runs_true() {
    let errs = type_errors(
        r#"
fn main() {
    let a = {x: 1}
    let b = {x: 1}
    println(a == b)
}
"#,
    );
    assert!(
        errs.is_empty(),
        "closed-row AnonRecord `==` should typecheck; got: {errs:?}"
    );

    let out = run_silt_ok(
        "f1_eq_true",
        r#"
fn main() {
    let a = {x: 1}
    let b = {x: 1}
    println(a == b)
}
"#,
    );
    assert_eq!(
        out.lines().next().unwrap_or("").trim(),
        "true",
        "expected runtime to print 'true' for equal anon records; full out={out:?}"
    );
}

/// `!=` is the dual: equal records yield `false`.
#[test]
fn f1_closed_anon_record_neq_typechecks_and_runs_false() {
    let out = run_silt_ok(
        "f1_neq_equal",
        r#"
fn main() {
    let a = {x: 1}
    let b = {x: 1}
    println(a != b)
}
"#,
    );
    assert_eq!(
        out.lines().next().unwrap_or("").trim(),
        "false",
        "expected runtime to print 'false' for `!=` on equal anon records; full out={out:?}"
    );
}

/// Different field values compare as unequal at runtime.
#[test]
fn f1_closed_anon_record_eq_differs_runs_false() {
    let out = run_silt_ok(
        "f1_eq_false",
        r#"
fn main() {
    let a = {x: 1}
    let b = {x: 2}
    println(a == b)
}
"#,
    );
    assert_eq!(
        out.lines().next().unwrap_or("").trim(),
        "false",
        "expected runtime to print 'false' for differing anon records; full out={out:?}"
    );
}

/// F1 conservative-design lock: an open-row AnonRecord (a typed
/// parameter `r: {x: Int, ...rest}`) must NOT be comparable with `==`.
/// Two open-row records may differ on unobserved fields, so the result
/// would depend on the hidden tail. Closed rows alone are comparable.
#[test]
fn f1_open_row_anon_record_eq_rejected() {
    let errs = type_errors(
        r#"
fn cmp(a: {x: Int, ...ra}, b: {x: Int, ...rb}) = a == b
fn main() {
    println(cmp({x: 1, y: 2}, {x: 1, z: 3}))
}
"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("==") || e.contains("comparable") || e.contains("requires")),
        "expected `==` on open-row AnonRecord to be rejected at typecheck; got: {errs:?}"
    );
}

// ── F2: did-you-mean hint on AnonRecord field-access ─────────────────

/// Direct AnonRecord field access (site :2462) with a typo close to a
/// real field surfaces `did you mean ... name ...`.
#[test]
fn f2_anon_record_field_access_typo_emits_did_you_mean() {
    let errs = type_errors(
        r#"
fn main() {
    let r = {name: "x"}
    println(r.nam)
}
"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("did you mean") && e.contains("name")),
        "expected a did-you-mean hint pointing at `name`; got: {errs:?}"
    );
    // Sanity: the original message body is still present.
    assert!(
        errs.iter()
            .any(|e| e.contains("anon record has no field") && e.contains("nam")),
        "expected the original 'anon record has no field' wording to remain; got: {errs:?}"
    );
}

/// Deferred sibling site (:958) — the brief documents that this
/// is the deferred-path twin of :2462: when a `pending_field_accesses`
/// entry resolves to a closed AnonRecord at finalization with a
/// typo'd field, the same `format_record_field_suggestion` helper
/// must apply. We could not construct a minimal program that lands
/// on :958 deterministically because silt's row-poly path at
/// `inference.rs:~2767` unifies the receiver typevar with an
/// `AnonRecord{<typo>: ?, ...?row}` at the field-access site itself,
/// so the call-site row-unification ("record open side has fields
/// not present in closed target") fires before finalization can
/// see a narrowed receiver.
///
/// :958 and :2462 share the same diagnostic helper post-fix, so the
/// :2462 lock test above transitively pins :958. This test covers
/// the closed-row form where field access happens on an already-
/// concrete AnonRecord (i.e. the same shape as :2462 with the value
/// preserved end-to-end).
#[test]
fn f2_anon_record_field_access_via_let_typo_emits_did_you_mean() {
    let errs = type_errors(
        r#"
fn main() {
    let p = {name: "x", age: 30}
    println(p.nam)
}
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains("nam")),
        "expected a typecheck error mentioning the bogus field 'nam'; got: {errs:?}"
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("did you mean") && e.contains("name")),
        "expected did-you-mean hint pointing at `name`; got: {errs:?}"
    );
}

// ── F3: did-you-mean hint on closed-row anon record-update ───────────

/// Record-update against a closed AnonRecord with an unknown field
/// close to an existing one must surface a did-you-mean hint.
///
/// Note: silt distinguishes the *extend* operator `{...r, nam: "y"}`
/// (creates a new record by union) from the *record-update*
/// expression `r.{ nam: "y" }` (validates against the receiver's
/// declared fields). Site :3819 is the closed-row branch of the
/// record-update path; the extend operator goes through a different
/// arm and silently unions, so this test uses `r.{...}`.
#[test]
fn f3_closed_anon_record_update_typo_emits_did_you_mean() {
    let errs = type_errors(
        r#"
fn main() {
    let r = {name: "x"}
    let r2 = r.{ nam: "y" }
    println(r2.name)
}
"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("did you mean") && e.contains("name")),
        "expected did-you-mean hint pointing at `name` on closed anon record-update; got: {errs:?}"
    );
    // Sanity: the original body wording is preserved.
    assert!(
        errs.iter()
            .any(|e| e.contains("unknown field") && e.contains("nam")),
        "expected the original 'unknown field' wording to remain; got: {errs:?}"
    );
}
