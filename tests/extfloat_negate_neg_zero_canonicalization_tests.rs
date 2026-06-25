//! Lock: negating an `ExtFloat` must canonicalize `-0.0 -> +0.0`, the same
//! as every other `ExtFloat` producer (`Div` in `src/vm/arithmetic.rs`,
//! `NarrowFloat` and the numeric builtins).
//!
//! Why it matters — silt runs two equality notions over floats:
//!   - the user-facing `==` operator widens to IEEE, so `-0.0 == +0.0` is true;
//!   - container keys (`Value::Set` is a `BTreeSet<Value>`, `Value::Map` a
//!     `BTreeMap`) use the *bitwise* `Ord`/`Hash` in `src/value.rs`, under
//!     which `-0.0` and `+0.0` are DISTINCT.
//! `-0.0` is the only finite value where the two disagree, so the codebase
//! keeps the invariant "an `ExtFloat` never holds `-0.0`" by canonicalizing at
//! every producer. `Op::Negate`'s `ExtFloat` arm (`src/vm/execute.rs`) was the
//! lone hole: `-(0.0 / 1.0)` yielded `ExtFloat(-0.0)` while `0.0 / 1.0` yields
//! `ExtFloat(+0.0)` — `==`-equal yet distinct set keys, so a set built from
//! both held TWO elements. Pre-fix this test sees length 2; post-fix, 1.
//!
//! Drives the `silt` CLI end-to-end (lexer -> parser -> typechecker ->
//! compiler -> VM) so the lock exercises the real runtime path, not a
//! typecheck-only tautology.

use std::process::Command;

const EXECUTE_RS: &str = include_str!("../src/vm/execute.rs");

fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_extfloat_negzero_{label}.silt"));
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

/// Behavioral lock: a negated zero and a plain zero are `==`-equal AND collapse
/// to a single set element. `0.0 / 1.0` widens `Float / Float` to `ExtFloat`,
/// and `-pos` walks `Op::Negate`'s `ExtFloat` arm at runtime (no const-folding
/// across the `let`). Both `set.from_list` keying and `==` must agree.
#[test]
fn negated_extfloat_zero_is_a_single_set_key() {
    let out = run_silt_ok(
        "set_collapse",
        r#"
import set

fn main() {
  let pos = 0.0 / 1.0
  let neg = -pos
  println(pos == neg)
  let s = set.from_list([pos, neg])
  println(set.length(s))
}
"#,
    );
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines.len() >= 2, "expected 2 lines; got {out:?}");
    assert_eq!(
        lines[0], "true",
        "`-0.0 == +0.0` must hold under the IEEE `==` operator; got {:?}",
        lines[0]
    );
    assert_eq!(
        lines[1], "1",
        "a set of `==`-equal floats must hold ONE element — the negated zero \
         leaked in as a distinct bitwise key (ExtFloat(-0.0) vs ExtFloat(+0.0)); \
         got {:?}",
        lines[1]
    );
}

/// `insert`-then-`insert` of the two zeros must likewise dedup, exercising the
/// `BTreeSet::insert` path rather than the bulk `from_list` collect.
#[test]
fn inserting_both_zero_signs_dedups() {
    let out = run_silt_ok(
        "set_insert",
        r#"
import set

fn main() {
  let pos = 0.0 / 1.0
  let neg = -pos
  let s0 = set.new()
  let s1 = set.insert(s0, pos)
  let s2 = set.insert(s1, neg)
  println(set.length(s2))
  println(set.contains(s2, neg))
}
"#,
    );
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines.len() >= 2, "expected 2 lines; got {out:?}");
    assert_eq!(
        lines[0], "1",
        "negated zero must dedup against +0.0 on insert"
    );
    assert_eq!(
        lines[1], "true",
        "the negated zero must be found in the set"
    );
}

/// Source-grep guard: the `ExtFloat` negate arm must keep canonicalizing zero.
/// A bare `Value::ExtFloat(-n)` (no `== 0.0` fold) reintroduces the bug.
#[test]
fn execute_rs_canonicalizes_negated_extfloat_zero() {
    // Find the ExtFloat arm of Op::Negate and assert it guards on `== 0.0`.
    let negate_idx = EXECUTE_RS
        .find("Op::Negate")
        .expect("Op::Negate handler present in execute.rs");
    let ext_rel = EXECUTE_RS[negate_idx..]
        .find("Value::ExtFloat(n) =>")
        .expect("ExtFloat arm present under Op::Negate");
    let ext_idx = negate_idx + ext_rel;
    // Scan from the arm header to the next match arm (`other =>`), so the
    // guard check is scoped to this arm's body regardless of comment length.
    let body = &EXECUTE_RS[ext_idx..];
    let arm = &body[..body.find("other =>").unwrap_or(body.len().min(800))];
    assert!(
        arm.contains("== 0.0"),
        "Op::Negate's ExtFloat arm no longer canonicalizes -0.0 -> +0.0 \
         (missing the `== 0.0` guard). A raw `ExtFloat(-n)` lets a negated \
         zero become a distinct bitwise set/map key — see this test's module \
         doc."
    );
}
