//! Regression tests for round-74 audit fix: `Value::Hash` violated the
//! Hash/Eq contract for cross-discriminant equal pairs (Float↔ExtFloat,
//! List↔Range), and `Value::Ord` short-circuited on different
//! discriminants for Float↔ExtFloat — both incompatible with `PartialEq`,
//! which considers `Float(x) == ExtFloat(x)` (widened to f64) and
//! `List([1,2,3]) == Range(1,3)`.
//!
//! Rust's contract: `a == b` ⇒ `hash(a) == hash(b)` AND `a.cmp(&b) ==
//! Equal`. This test pins down each cross-discriminant equal pair and
//! asserts all three predicates.
//!
//! End-to-end probe: the language-level `.hash()` builtin
//! (`src/vm/dispatch.rs:447-474`) routes through `impl Hash for Value`,
//! so a silt program asserting `a.hash() == b.hash()` exercises the
//! same code path.

use std::cmp::Ordering;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use silt::value::Value;

fn hash_of(v: &Value) -> u64 {
    let mut h = DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

// ── Float ↔ ExtFloat ───────────────────────────────────────────────

#[test]
fn float_extfloat_equal_finite_values_hash_equal() {
    let a = Value::Float(1.0);
    let b = Value::ExtFloat(1.0);
    assert_eq!(a, b, "PartialEq widens both to f64; 1.0 == 1.0 must hold");
    assert_eq!(
        hash_of(&a),
        hash_of(&b),
        "Hash/Eq contract: equal Values must hash equal across Float/ExtFloat"
    );
    assert_eq!(
        a.cmp(&b),
        Ordering::Equal,
        "Ord/Eq contract: a == b ⇒ a.cmp(&b) == Equal"
    );
    assert_eq!(b.cmp(&a), Ordering::Equal);
}

#[test]
fn float_extfloat_zero_and_neg_zero_hash_equal() {
    // f64 PartialEq says 0.0 == -0.0, and PartialEq widens Float/ExtFloat
    // to f64 (line ~1705), so cross-pair 0.0/-0.0 must compare equal
    // and hash equal regardless of which side carries the negative.
    let f_pos = Value::Float(0.0);
    let xf_neg = Value::ExtFloat(-0.0);
    assert_eq!(f_pos, xf_neg);
    assert_eq!(hash_of(&f_pos), hash_of(&xf_neg));
    assert_eq!(f_pos.cmp(&xf_neg), Ordering::Equal);

    let f_neg = Value::Float(-0.0);
    let xf_pos = Value::ExtFloat(0.0);
    assert_eq!(f_neg, xf_pos);
    assert_eq!(hash_of(&f_neg), hash_of(&xf_pos));
    assert_eq!(f_neg.cmp(&xf_pos), Ordering::Equal);
}

#[test]
fn float_extfloat_unequal_values_ord_consistent() {
    // 1.0 vs 2.0: PartialEq says false; Ord must say Less/Greater
    // (NEVER Equal) — directionally consistent.
    let a = Value::Float(1.0);
    let b = Value::ExtFloat(2.0);
    assert_ne!(a, b);
    assert_eq!(a.cmp(&b), Ordering::Less);
    assert_eq!(b.cmp(&a), Ordering::Greater);
}

#[test]
fn float_extfloat_nan_ord_distinct() {
    // ExtFloat NaN: PartialEq with Float widens to f64 where NaN != x for
    // all x, so the pair is unequal — Ord must NOT return Equal.
    let f = Value::Float(1.0);
    let nan = Value::ExtFloat(f64::NAN);
    assert_ne!(f, nan);
    assert_ne!(f.cmp(&nan), Ordering::Equal);
    assert_ne!(nan.cmp(&f), Ordering::Equal);
}

// ── List ↔ Range ───────────────────────────────────────────────────

#[test]
fn list_range_equal_pairs_hash_equal() {
    // Range(1,3) materializes to [1,2,3]; PartialEq returns true (line
    // ~1729-1730), so Hash and Ord must agree.
    let list = Value::List(Arc::new(vec![
        Value::Int(1),
        Value::Int(2),
        Value::Int(3),
    ]));
    let range = Value::Range(1, 3);
    assert_eq!(list, range);
    assert_eq!(range, list);
    assert_eq!(hash_of(&list), hash_of(&range));
    assert_eq!(list.cmp(&range), Ordering::Equal);
    assert_eq!(range.cmp(&list), Ordering::Equal);
}

#[test]
fn list_range_empty_hash_equal() {
    // Empty range (lo > hi) equals empty list per PartialEq.
    let list = Value::List(Arc::new(vec![]));
    let range = Value::Range(5, 4);
    assert_eq!(list, range);
    assert_eq!(hash_of(&list), hash_of(&range));
    assert_eq!(list.cmp(&range), Ordering::Equal);
}

#[test]
fn list_range_single_element_hash_equal() {
    let list = Value::List(Arc::new(vec![Value::Int(42)]));
    let range = Value::Range(42, 42);
    assert_eq!(list, range);
    assert_eq!(hash_of(&list), hash_of(&range));
    assert_eq!(list.cmp(&range), Ordering::Equal);
}

#[test]
fn list_range_unequal_pairs_ord_consistent() {
    // List doesn't materialize to range: should NOT be Equal.
    let list = Value::List(Arc::new(vec![
        Value::Int(1),
        Value::Int(3),
        Value::Int(2), // out of order — not a range
    ]));
    let range = Value::Range(1, 3);
    assert_ne!(list, range);
    assert_ne!(list.cmp(&range), Ordering::Equal);
}

// ── HashMap / HashSet round-trip ───────────────────────────────────

/// Inserting an equal-but-cross-discriminant pair into a HashSet must
/// recognize the duplicate. Pre-fix, the discriminant difference made
/// the second insert hash to a different bucket and slip through.
#[test]
fn hashset_dedup_across_float_extfloat() {
    use std::collections::HashSet;
    let mut s: HashSet<Value> = HashSet::new();
    s.insert(Value::Float(7.5));
    s.insert(Value::ExtFloat(7.5));
    assert_eq!(
        s.len(),
        1,
        "HashSet must dedup Float(7.5) and ExtFloat(7.5) — they're equal"
    );
}

#[test]
fn hashset_dedup_across_list_range() {
    use std::collections::HashSet;
    let mut s: HashSet<Value> = HashSet::new();
    s.insert(Value::List(Arc::new(vec![
        Value::Int(1),
        Value::Int(2),
        Value::Int(3),
    ])));
    s.insert(Value::Range(1, 3));
    assert_eq!(
        s.len(),
        1,
        "HashSet must dedup List([1,2,3]) and Range(1,3) — they're equal"
    );
}

// ── End-to-end silt binary: language-level .hash() builtin ─────────

fn silt_bin() -> PathBuf {
    if let Ok(p) = std::env::var("CARGO_BIN_EXE_silt") {
        return PathBuf::from(p);
    }
    let mut p = std::env::current_exe().unwrap();
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    p.push("silt");
    p
}

fn run_silt(src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!(
        "silt_round74_hash_eq_{}_{}.silt",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(&tmp, src).unwrap();
    let output = Command::new(silt_bin())
        .arg("run")
        .arg(&tmp)
        .output()
        .expect("failed to run silt binary");
    let _ = std::fs::remove_file(&tmp);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    (stdout, stderr, output.status.success())
}

/// End-to-end silt-level repro from the audit finding:
/// ```
/// let a: Float = 1.0
/// let b: ExtFloat = 1.0 / 1.0
/// println(a == b)            -- true
/// println(a.hash() == b.hash()) -- must be true (was false)
/// ```
#[test]
fn silt_level_float_extfloat_hash_eq() {
    let src = r#"
fn main() {
  let a: Float = 1.0
  let b: ExtFloat = 1.0 / 1.0
  println(a == b)
  println(a.hash() == b.hash())
}
"#;
    let (stdout, stderr, ok) = run_silt(src);
    assert!(ok, "silt run failed; stderr={stderr}; stdout={stdout}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(
        lines.iter().filter(|l| **l == "true").count() >= 2,
        "expected two `true` lines (a == b AND a.hash() == b.hash()); got stdout={stdout:?}"
    );
}

/// End-to-end silt-level: a List literal and the matching Range must
/// hash equal at the language level.
#[test]
fn silt_level_list_range_hash_eq() {
    let src = r#"
fn main() {
  let a: List(Int) = [1, 2, 3]
  let b: List(Int) = 1..3
  println(a == b)
  println(a.hash() == b.hash())
}
"#;
    let (stdout, stderr, ok) = run_silt(src);
    assert!(ok, "silt run failed; stderr={stderr}; stdout={stdout}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(
        lines.iter().filter(|l| **l == "true").count() >= 2,
        "expected two `true` lines (a == b AND a.hash() == b.hash()); got stdout={stdout:?}"
    );
}
