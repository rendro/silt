//! Round 101 lock: `Value::Ord` for ExtFloat must be sign-aware.
//!
//! Pre-fix bug (`src/value.rs`, `impl Ord for Value`, ExtFloat/ExtFloat
//! arm): the arm compared raw bit patterns unsigned —
//! `a.to_bits().cmp(&b.to_bits())` — so the IEEE-754 sign bit made every
//! negative ExtFloat sort ABOVE every positive one (and reversed the
//! order among negatives):
//!
//!   * `list.sort([3.0/1.0, -2.0/1.0, 1.0/1.0])` printed `[1, 3, -2]`
//!     although the docs promise natural ascending order.
//!   * `(-2.0/1.0) < (1.0/1.0)` was `true` (scalar `<` uses IEEE
//!     `partial_cmp` in `src/vm/arithmetic.rs`) while
//!     `[-2.0/1.0] < [1.0/1.0]` was `false` (container compare routes
//!     through `Value::cmp`) — the same comparison, opposite answers.
//!   * Same root cause corrupted `list.sort_by` / `list.min_by` with
//!     ExtFloat keys and `BTreeSet` (`set.from_list`) iteration order.
//!
//! Post-fix: the arm uses `f64::total_cmp` (IEEE totalOrder), which is
//! sign-aware AND returns `Equal` iff the bit patterns are identical, so
//! the bitwise `PartialEq` (NaN self-equal, required for container keys)
//! and `Hash` contracts are preserved. The mixed Float/ExtFloat NaN
//! fallbacks switched from bit compare to `total_cmp` for the same
//! reason.

use std::cmp::Ordering;
use std::process::Command;

use silt::value::Value;

/// Run a Silt source program and return (stdout, stderr, success).
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_round101_extfloat_ord_{label}.silt"));
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

// ── 1. list.sort on ExtFloat: natural ascending order ──────────────

#[test]
fn list_sort_extfloat_ascending() {
    let out = run_silt_ok(
        "sort_ascending",
        r#"
import list
fn main() {
  println("{list.sort([3.0/1.0, -2.0/1.0, 1.0/1.0])}")
}
"#,
    );
    assert_eq!(
        out.trim(),
        "[-2, 1, 3]",
        "list.sort must order ExtFloats numerically ascending \
         (pre-fix bitwise Ord printed [1, 3, -2]); got {out:?}"
    );
}

// ── 2. Scalar `<` and container `<` must agree ──────────────────────

#[test]
fn scalar_and_container_lt_parity() {
    let out = run_silt_ok(
        "lt_parity",
        r#"
fn main() {
  println("{(-2.0/1.0) < (1.0/1.0)}")
  println("{[-2.0/1.0] < [1.0/1.0]}")
}
"#,
    );
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(
        lines,
        vec!["true", "true"],
        "scalar `<` (IEEE partial_cmp) and container `<` (Value::cmp) \
         must give the same answer for -2.0 < 1.0 as ExtFloat \
         (pre-fix the container compare said false); got {out:?}"
    );
}

// ── 3. sort_by / min_by with ExtFloat keys ──────────────────────────

#[test]
fn sort_by_and_min_by_extfloat_keys() {
    let out = run_silt_ok(
        "sort_by_min_by",
        r#"
import list
fn main() {
  let xs = [3.0/1.0, -2.0/1.0, 1.0/1.0]
  let sorted = list.sort_by(xs) { x -> x }
  println("{sorted}")
  let m = list.min_by(xs) { x -> x }
  match m {
    Some(v) -> println("{v}")
    None -> println("none")
  }
}
"#,
    );
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(
        lines,
        vec!["[-2, 1, 3]", "-2"],
        "sort_by/min_by with ExtFloat keys must pick numeric order \
         (pre-fix min_by returned 1, the bitwise minimum); got {out:?}"
    );
}

// ── 4. set.from_list: BTreeSet iteration order is ascending ─────────

#[test]
fn set_from_list_extfloat_iterates_ascending() {
    let out = run_silt_ok(
        "set_iteration",
        r#"
import set
fn main() {
  println("{set.to_list(set.from_list([1.0/1.0, -2.0/1.0, 3.0/1.0]))}")
}
"#,
    );
    assert_eq!(
        out.trim(),
        "[-2, 1, 3]",
        "BTreeSet iteration over ExtFloat elements must be numerically \
         ascending (pre-fix negatives sorted last); got {out:?}"
    );
}

// ── 5. Regression guards: NaN keys still dedup and stay orderable ───

#[test]
fn set_extfloat_nan_still_dedups() {
    let out = run_silt_ok(
        "nan_dedup",
        r#"
import set
fn main() {
  let s = set.from_list([0.0/0.0, 0.0/0.0])
  println("{set.length(s)}")
  println("{set.to_list(set.from_list([0.0/0.0, -2.0/1.0]))}")
}
"#,
    );
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines.len(), 2, "expected two output lines; got {out:?}");
    assert_eq!(
        lines[0], "1",
        "total_cmp must keep NaN self-equal (dedup to one element); got {out:?}"
    );
    // NaN's position under totalOrder depends on its SIGN bit, and the
    // sign of the NaN produced by `0.0/0.0` is platform-dependent
    // (negative quiet NaN on x86-64 SSE, positive on aarch64). Assert
    // only that NaN is a valid BTreeSet key coexisting with a finite
    // element — not a specific side of it.
    assert!(
        lines[1] == "[-2, NaN]" || lines[1] == "[NaN, -2]",
        "NaN must be a valid BTreeSet key alongside finite values \
         (either totalOrder side, sign-dependent); got {out:?}"
    );
}

// ── 6. Rust-level Ord contract on Value directly ────────────────────

#[test]
fn value_ord_extfloat_sign_aware_total_order() {
    // Pre-fix: bit compare put the sign bit at the top, so every
    // negative sorted Greater than every positive.
    assert_eq!(
        Value::ExtFloat(-2.0).cmp(&Value::ExtFloat(1.0)),
        Ordering::Less,
        "-2.0 must sort below 1.0"
    );
    // Pre-fix: order among negatives was reversed (larger magnitude =
    // larger unsigned bits).
    assert_eq!(
        Value::ExtFloat(-3.0).cmp(&Value::ExtFloat(-2.0)),
        Ordering::Less,
        "-3.0 must sort below -2.0"
    );
    assert_eq!(
        Value::ExtFloat(f64::NEG_INFINITY).cmp(&Value::ExtFloat(-1e308)),
        Ordering::Less,
        "-inf must sort below every finite"
    );
    assert_eq!(
        Value::ExtFloat(f64::INFINITY).cmp(&Value::ExtFloat(1e308)),
        Ordering::Greater,
        "+inf must sort above every finite"
    );
    // Eq/Ord contract: bitwise-equal values compare Equal.
    assert_eq!(
        Value::ExtFloat(f64::NAN).cmp(&Value::ExtFloat(f64::NAN)),
        Ordering::Equal,
        "NaN must stay self-equal under Ord (container-key reflexivity)"
    );
    assert_eq!(
        Value::ExtFloat(1.0).cmp(&Value::ExtFloat(1.0)),
        Ordering::Equal
    );
}

#[test]
fn value_ord_mixed_float_extfloat_nan_fallback() {
    // Float is always finite; the only cross-arm partial_cmp miss is an
    // ExtFloat NaN. PartialEq says unequal there, so Ord must not say
    // Equal — and the two directions must be antisymmetric.
    let f = Value::Float(1.0);
    let nan = Value::ExtFloat(f64::NAN);
    let a = f.cmp(&nan);
    let b = nan.cmp(&f);
    assert_ne!(a, Ordering::Equal, "finite Float != ExtFloat NaN");
    assert_ne!(b, Ordering::Equal, "ExtFloat NaN != finite Float");
    assert_eq!(
        a,
        b.reverse(),
        "cross-arm NaN ordering must be antisymmetric"
    );
}
