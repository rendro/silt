//! Lock: the mixed `Float`/`ExtFloat` arithmetic arm in
//! `src/vm/arithmetic.rs` must canonicalize `-0.0 -> +0.0`, the same as the
//! dedicated `(Float, Float)` `Div` arm, `Op::Negate`, `NarrowFloat`, and the
//! numeric builtins.
//!
//! Why it matters — silt runs two equality notions over floats:
//!   - the user-facing `==` operator widens to IEEE, so `-0.0 == +0.0` is true;
//!   - container keys (`Value::Set` is a `BTreeSet<Value>`, `Value::Map` a
//!     `BTreeMap`) use the *bitwise* `Ord`/`Hash` in `src/value.rs`, under
//!     which `-0.0` and `+0.0` are DISTINCT.
//! `-0.0` is the only finite value where the two disagree, so the codebase
//! keeps the invariant "an `ExtFloat` never holds `-0.0`" by canonicalizing at
//! every producer. Commit f2c4462 closed the `Op::Negate` hole and claimed it
//! was the lone one — but the collapsed mixed `(Float | ExtFloat, Float |
//! ExtFloat)` arm (Add/Sub/Mul/Div/Mod) still pushed a raw `ExtFloat(a OP b)`
//! with no `== 0.0` fold. `(-1.0) * (0.0 / 1.0)` and `(-1.0) / (1.0 / 0.0)`
//! both yield `ExtFloat(-0.0)`, which is `==`-equal to `+0.0` yet a distinct
//! set key. Pre-fix the sets below hold TWO elements; post-fix, ONE.
//!
//! Drives the `silt` CLI end-to-end (lexer -> parser -> typechecker ->
//! compiler -> VM) so the lock exercises the real runtime path, not a
//! typecheck-only tautology.

use std::process::Command;

const ARITHMETIC_RS: &str = include_str!("../src/vm/arithmetic.rs");

fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_extfloat_mixed_negzero_{label}.silt"));
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

/// `Mul` in the mixed arm: `(-1.0) * (0.0 / 1.0)` produces `-0.0`. The right
/// operand `0.0 / 1.0` widens `Float / Float` to `ExtFloat(+0.0)`, so the
/// multiply walks the mixed `(Float, ExtFloat)` arm. Result must canonicalize
/// and dedup against a plain `+0.0` set key.
#[test]
fn mixed_mul_negative_zero_is_a_single_set_key() {
    let out = run_silt_ok(
        "mul",
        r#"
import set

fn main() {
  let zero = 0.0 / 1.0
  let neg = (-1.0) * zero
  let pos = 1.0 / 1.0 / 1.0 * 0.0
  println(pos == neg)
  println(set.length(set.from_list([pos, neg])))
}
"#,
    );
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines.len() >= 2, "expected 2 lines; got {out:?}");
    assert_eq!(lines[0], "true", "`-0.0 == +0.0` under IEEE `==`");
    assert_eq!(
        lines[1], "1",
        "mixed-arm Mul `-0.0` leaked in as a distinct bitwise set key \
         (ExtFloat(-0.0) vs ExtFloat(+0.0)); got {:?}",
        lines[1]
    );
}

/// `Div` in the mixed arm: `(-1.0) / (1.0 / 0.0)` = `(-1.0) / +inf` = `-0.0`.
/// The right operand is `ExtFloat(+inf)`, so the divide walks the mixed arm.
#[test]
fn mixed_div_negative_zero_dedups_on_insert() {
    let out = run_silt_ok(
        "div",
        r#"
import set

fn main() {
  let inf = 1.0 / 0.0
  let neg = (-1.0) / inf
  let pos = 1.0 / inf
  let s0 = set.new()
  let s1 = set.insert(s0, pos)
  let s2 = set.insert(s1, neg)
  println(neg == pos)
  println(set.length(s2))
  println(set.contains(s2, neg))
}
"#,
    );
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines.len() >= 3, "expected 3 lines; got {out:?}");
    assert_eq!(lines[0], "true", "`-0.0 == +0.0` under IEEE `==`");
    assert_eq!(
        lines[1], "1",
        "mixed-arm Div `-0.0` must dedup against +0.0 on insert; got {:?}",
        lines[1]
    );
    assert_eq!(
        lines[2], "true",
        "the negated zero must be found in the set"
    );
}

/// Source-grep guard: the mixed arithmetic arm must keep canonicalizing zero.
/// A bare `Value::ExtFloat(a OP b)` (no `== 0.0` fold) reintroduces the bug.
#[test]
fn arithmetic_rs_canonicalizes_mixed_arm_zero() {
    // Locate the collapsed mixed Float/ExtFloat arm and assert its body folds
    // `-0.0` via the `== 0.0` guard, scoped from the arm header to the next
    // arm (the `String`/`String` `Add` arm) so the check stays local.
    let arm_idx = ARITHMETIC_RS
        .find("Value::Float(a) | Value::ExtFloat(a), Value::Float(b) | Value::ExtFloat(b)")
        .expect("mixed Float/ExtFloat arithmetic arm present in arithmetic.rs");
    let body = &ARITHMETIC_RS[arm_idx..];
    let arm = &body[..body
        .find("Value::String(a), Value::String(b)")
        .expect("String/String arm follows the mixed float arm")];
    assert!(
        arm.contains("== 0.0"),
        "the mixed Float/ExtFloat arithmetic arm no longer canonicalizes \
         -0.0 -> +0.0 (missing the `== 0.0` guard). A raw `ExtFloat(a OP b)` \
         lets a computed negative zero become a distinct bitwise set/map key \
         — see this test's module doc."
    );
}
