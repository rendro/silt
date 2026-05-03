//! Runtime regression tests for anonymous record `...rest` destructuring.
//!
//! BROKEN finding (pre-fix):
//!   `match p { {age: a, ...rest} -> ... }` (and let-destructure with
//!   `...rest`) crashed at runtime with
//!     `error[runtime]: record rest destructure: expected record, got Int`
//!   (or `String`, `Float`, etc., depending on the type of the last
//!   bound field).
//!
//! Root cause: in `compile_compound_bind` (src/compiler/patterns.rs),
//! per-field destructures leave the *last* sub-value on TOS — not the
//! parent record. The original AnonRecord handler then emitted
//! `Op::Dup` + `Op::DestructRecordRest` against TOS, so the rest
//! opcode received a field's value (Int / String / …) and reported its
//! type as the "type that isn't a record".
//!
//! Fix: route the rest capture through `compile_compound_bind` itself
//! via a new `BindDestructKind::RecordRest(Vec<Symbol>)` arm, so it
//! sees the same `__bind_parent__` slot used by every per-field
//! destructure and always operates on the actual parent record.
//!
//! These tests exercise the runtime path end-to-end via the `silt`
//! CLI — they fail before the patterns.rs / compiler/mod.rs fix and
//! pass after.

use std::process::Command;

/// Run a Silt source program and return (stdout, stderr, success).
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_anon_rest_rt_{label}.silt"));
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

/// Run and assert success; return stdout.
fn run_silt_ok(label: &str, src: &str) -> String {
    let (stdout, stderr, ok) = run_silt_raw(label, src);
    assert!(
        ok,
        "silt run should succeed for {label}; stdout={stdout}, stderr={stderr}"
    );
    stdout
}

/// Canonical repro from the bug report: a two-field anon record,
/// destructured into one named field plus `...rest`. Pre-fix this
/// crashes with `expected record, got Int`.
#[test]
fn anon_record_rest_pattern_binds_remaining_fields() {
    let out = run_silt_ok(
        "rest_basic",
        r#"
fn main() {
  let p = {age: 30, city: "X"}
  match p {
    {age: a, ...rest} -> {
      println(a)
      println(rest.city)
    }
  }
}
"#,
    );
    let mut lines = out.lines();
    assert_eq!(lines.next().unwrap_or("").trim(), "30");
    assert_eq!(lines.next().unwrap_or("").trim(), "X");
}

/// More than one named field plus a rest binding. Exercises that
/// every named-field destruct still resolves against the parent
/// record (not against any intermediate sub-value), and that the
/// rest record contains every non-listed field.
#[test]
fn anon_record_rest_with_multiple_named_fields() {
    let out = run_silt_ok(
        "rest_multi",
        r#"
fn main() {
  let p = {a: 1, b: 2, c: 3}
  match p {
    {a: x, ...r} -> println(r.b + r.c)
  }
}
"#,
    );
    assert_eq!(out.lines().next().unwrap_or("").trim(), "5");
}

/// Zero named fields, only `...rest` — the rest must capture every
/// field. Pre-fix this branch was protected because there was no
/// "last sub-value" to confuse with the parent, but it's worth
/// pinning to make sure the new dispatch through
/// `compile_compound_bind` doesn't regress the empty-fields case.
#[test]
fn anon_record_rest_with_zero_named_fields() {
    let out = run_silt_ok(
        "rest_zero",
        r#"
fn main() {
  let p = {a: 1}
  match p {
    {...r} -> println(r.a)
  }
}
"#,
    );
    assert_eq!(out.lines().next().unwrap_or("").trim(), "1");
}
