//! Round 85 regression tests.
//!
//! ── REGRESSION(2f4aa6a): Eq/Hash/Ord contract violation ──────────────
//!
//! Round 84 patched `Value::PartialEq` for `Value::Record` to treat
//! `type_name == "<anon>"` as a wildcard (src/value.rs ~1729): when
//! either side carries `<anon>`, compare fields-only. That fixed
//! `==` for anon-typed nominals (the `unify_anon_nominal` widening
//! at type-check time without runtime rebrand).
//!
//! But `Value::Ord` and `Value::Hash` were NOT updated. The Rust
//! contracts require:
//!   - `a == b ⇒ cmp(a, b) == Ordering::Equal` (src/value.rs ~1751-56,
//!      ~1921-28 comment blocks reaffirm this).
//!   - `a == b ⇒ hash(a) == hash(b)` (standard `Hash`/`Eq` contract).
//!
//! Concretely:
//!   - `Ord` still did `na.cmp(nb).then_with(...)` so anon and nominal
//!     ordered apart even though PartialEq says they're equal.
//!     ⇒ `BTreeSet` silently kept BOTH as distinct entries.
//!   - `Hash` still did `name.hash(state)` so anon and nominal hashed
//!     to different buckets even though PartialEq says they're equal.
//!     ⇒ `HashSet`/`HashMap` returned wrong `contains` results.
//!
//! Round 85 propagates the `<anon>`-wildcard logic to BOTH sites:
//!   - `Ord` arm at ~line 1875: when either side carries `<anon>`,
//!     fall through directly to field comparison (skip name.cmp).
//!   - `Hash` arm at ~line 2213: hash field contents only (drop the
//!     name from the hash entirely). Two distinct concrete nominals
//!     (`Person{x:1}` vs `Car{x:1}`) still compare unequal via
//!     PartialEq's `na == nb && fa == fb` branch — they just hash-
//!     collide and disambiguate via Eq, which is normal hash-collision
//!     behavior, not a bug.
//!
//! Also fixed: `src/vm/arithmetic.rs:144` cmp dispatch guard
//! `if na == nb` widened to admit `<anon>` on either side — same
//! wildcard logic.

use std::process::Command;

/// Drive the full lex → parse → typecheck → compile → VM pipeline via
/// the `silt run` CLI and return (stdout, stderr, success).
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_round85_{label}.silt"));
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

// ── User-visible silt-level repros ───────────────────────────────────

/// `set.contains(s, r)` must return `true` when `s` was built from a
/// nominal value `p` and `r` is an anon-typed value PartialEq-equal to
/// `p`. Without the Hash fix, the anon and nominal land in different
/// hash buckets and `contains` returns `false`.
#[test]
fn set_contains_anon_typed_when_nominal_inserted() {
    let src = r#"
import set
type P { x: Int }
fn main() {
  let p: P = P { x: 1 }
  let r: { x: Int } = { x: 1 }
  let s = set.from_list([p])
  println(set.contains(s, r))
}
"#;
    let out = run_silt_ok("set_contains_anon_after_nominal", src);
    let line = out.lines().next().unwrap_or("").trim_end();
    assert_eq!(
        line, "true",
        "set.contains must agree with == — anon-typed value PartialEq-equal to a nominal must hash to the same bucket; got: {out:?}"
    );
}

/// Inserting an anon-typed value that PartialEq-equals an already-
/// present nominal must dedup (set size stays at 1). Without the Hash
/// fix, the set grows to 2.
#[test]
fn set_dedup_when_inserting_anon_after_nominal() {
    let src = r#"
import set
type P { x: Int }
fn main() {
  let p: P = P { x: 1 }
  let r: { x: Int } = { x: 1 }
  let s = set.from_list([p])
  let s2 = set.insert(s, r)
  println(set.length(s2))
}
"#;
    let out = run_silt_ok("set_dedup_anon_after_nominal", src);
    let line = out.lines().next().unwrap_or("").trim_end();
    assert_eq!(
        line, "1",
        "set.insert(s, r) where r PartialEq-equals an existing element must dedup; got: {out:?}"
    );
}

// ── Rust-level contract enforcement ─────────────────────────────────

/// `Hash` contract: PartialEq-equal values must hash equal.
/// `Value::Record("P", {x:1})` and `Value::Record("<anon>", {x:1})`
/// are equal under round-84's `<anon>`-wildcard PartialEq, so their
/// hashes MUST match.
#[test]
fn hash_of_nominal_equals_hash_of_anon_when_eq() {
    use silt::Value;
    use std::collections::BTreeMap;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::sync::Arc;

    let mut fa = BTreeMap::new();
    fa.insert("x".to_string(), Value::Int(1));
    let p = Value::Record("P".to_string(), Arc::new(fa));

    let mut fb = BTreeMap::new();
    fb.insert("x".to_string(), Value::Int(1));
    let r = Value::Record("<anon>".to_string(), Arc::new(fb));

    // Sanity: PartialEq agrees they're equal (round-84 lock).
    assert_eq!(p, r, "round-84 PartialEq: anon-wildcard collapses name");

    let mut ha = DefaultHasher::new();
    p.hash(&mut ha);
    let mut hb = DefaultHasher::new();
    r.hash(&mut hb);
    assert_eq!(
        ha.finish(),
        hb.finish(),
        "Hash contract: a == b ⇒ hash(a) == hash(b)"
    );
}

/// `Ord` contract: PartialEq-equal values must compare `Equal`.
#[test]
fn cmp_of_nominal_and_anon_returns_equal_when_eq() {
    use silt::Value;
    use std::cmp::Ordering;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    let mut fa = BTreeMap::new();
    fa.insert("x".to_string(), Value::Int(1));
    let p = Value::Record("P".to_string(), Arc::new(fa));

    let mut fb = BTreeMap::new();
    fb.insert("x".to_string(), Value::Int(1));
    let r = Value::Record("<anon>".to_string(), Arc::new(fb));

    assert_eq!(p, r, "round-84 PartialEq: anon-wildcard collapses name");
    assert_eq!(
        p.cmp(&r),
        Ordering::Equal,
        "Ord contract: a == b ⇒ cmp(a, b) == Equal"
    );
    assert_eq!(
        r.cmp(&p),
        Ordering::Equal,
        "Ord contract is symmetric: cmp(b, a) == Equal too"
    );
}

/// `BTreeSet` dedup proves the Ord-vs-PartialEq agreement at the
/// container level — `BTreeSet::insert` returns `false` for a value
/// that compares Ordering::Equal with an existing element.
#[test]
fn btreeset_dedups_anon_and_nominal() {
    use silt::Value;
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;

    let mut fa = BTreeMap::new();
    fa.insert("x".to_string(), Value::Int(1));
    let p = Value::Record("P".to_string(), Arc::new(fa));

    let mut fb = BTreeMap::new();
    fb.insert("x".to_string(), Value::Int(1));
    let r = Value::Record("<anon>".to_string(), Arc::new(fb));

    let mut s: BTreeSet<Value> = BTreeSet::new();
    s.insert(p);
    s.insert(r);
    assert_eq!(
        s.len(),
        1,
        "BTreeSet must dedup PartialEq-equal anon and nominal records"
    );
}

/// `HashSet` dedup proves the Hash-vs-PartialEq agreement at the
/// container level — `HashSet::insert` returns `false` for a value
/// whose hash + Eq collide with an existing element.
#[test]
fn hashset_dedups_anon_and_nominal() {
    use silt::Value;
    use std::collections::{BTreeMap, HashSet};
    use std::sync::Arc;

    let mut fa = BTreeMap::new();
    fa.insert("x".to_string(), Value::Int(1));
    let p = Value::Record("P".to_string(), Arc::new(fa));

    let mut fb = BTreeMap::new();
    fb.insert("x".to_string(), Value::Int(1));
    let r = Value::Record("<anon>".to_string(), Arc::new(fb));

    let mut s: HashSet<Value> = HashSet::new();
    s.insert(p);
    s.insert(r);
    assert_eq!(
        s.len(),
        1,
        "HashSet must dedup PartialEq-equal anon and nominal records"
    );
}

// ── Regression: distinct nominals stay distinct ──────────────────────

/// CRITICAL: two distinct concrete nominals (NEITHER `<anon>`) with
/// the same fields must remain distinct under all three contracts.
/// They hash-collide (the round-85 Hash fix drops the name from the
/// hash) but disambiguate via Eq — which is normal hash-collision
/// behavior. They must NOT dedup in a set.
#[test]
fn two_distinct_nominals_still_distinct_in_set() {
    use silt::Value;
    use std::collections::{BTreeMap, BTreeSet, HashSet};
    use std::sync::Arc;

    let mut fa = BTreeMap::new();
    fa.insert("x".to_string(), Value::Int(1));
    let person = Value::Record("Person".to_string(), Arc::new(fa));

    let mut fb = BTreeMap::new();
    fb.insert("x".to_string(), Value::Int(1));
    let car = Value::Record("Car".to_string(), Arc::new(fb));

    // PartialEq: still unequal (neither carries `<anon>`).
    assert_ne!(person, car, "distinct nominals must remain unequal");

    // BTreeSet: keeps both (Ord must not return Equal).
    let mut bts: BTreeSet<Value> = BTreeSet::new();
    bts.insert(person.clone());
    bts.insert(car.clone());
    assert_eq!(
        bts.len(),
        2,
        "BTreeSet must keep distinct nominals separate"
    );

    // HashSet: keeps both (Eq must disambiguate even on hash collision).
    let mut hs: HashSet<Value> = HashSet::new();
    hs.insert(person);
    hs.insert(car);
    assert_eq!(hs.len(), 2, "HashSet must keep distinct nominals separate");
}

// ── Regression: round-84 PartialEq lock intact ──────────────────────

/// Lock the round-84 invariant from the Rust side: anon vs nominal
/// with same fields still compares equal (we already exercise this
/// from `silt run` in `round84_anonrec_unify_eq_tests.rs`, but
/// locking it here too means a regression that breaks both rounds at
/// once is caught in this file).
#[test]
fn round84_anon_eq_nominal_still_holds() {
    use silt::Value;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    let mut fa = BTreeMap::new();
    fa.insert("x".to_string(), Value::Int(1));
    let p = Value::Record("P".to_string(), Arc::new(fa));

    let mut fb = BTreeMap::new();
    fb.insert("x".to_string(), Value::Int(1));
    let r = Value::Record("<anon>".to_string(), Arc::new(fb));

    assert_eq!(p, r, "round-84 `<anon>`-wildcard PartialEq must still hold");
}
