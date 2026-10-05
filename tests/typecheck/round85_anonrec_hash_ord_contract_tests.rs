//! Round 85 regression tests.
//!
//! ── REGRESSION(2f4aa6a): Eq/Hash/Ord contract violation ──────────────
//!
//! Round 84 patched `Value::PartialEq` for `Value::Record` to treat
//! `type_name == "<anon>"` as a wildcard (src/value/key.rs): when
//! either side carries `<anon>`, compare fields-only. That fixed
//! `==` for anon-typed nominals (the `unify_anon_nominal` widening
//! at type-check time without runtime rebrand).
//!
//! But `Value::Ord` and `Value::Hash` were NOT updated. The Rust
//! contracts require:
//!   - `a == b ⇒ cmp(a, b) == Ordering::Equal` (src/value/key.rs,
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

// The user-visible silt-level repros (set.contains / set.insert dedup)
// are golden cases `round85_anonrec_hash_ord_contract__*` under
// tests/golden/typecheck/hashing/.

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
    let p = Value::Record(record_type("P"), Arc::new(fa));

    let mut fb = BTreeMap::new();
    fb.insert("x".to_string(), Value::Int(1));
    let r = Value::Record(
        silt::typeinfo::builtin_type(silt::typeinfo::ty::ANON_RECORD).clone(),
        Arc::new(fb),
    );

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
    let p = Value::Record(record_type("P"), Arc::new(fa));

    let mut fb = BTreeMap::new();
    fb.insert("x".to_string(), Value::Int(1));
    let r = Value::Record(
        silt::typeinfo::builtin_type(silt::typeinfo::ty::ANON_RECORD).clone(),
        Arc::new(fb),
    );

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
    let p = Value::Record(record_type("P"), Arc::new(fa));

    let mut fb = BTreeMap::new();
    fb.insert("x".to_string(), Value::Int(1));
    let r = Value::Record(
        silt::typeinfo::builtin_type(silt::typeinfo::ty::ANON_RECORD).clone(),
        Arc::new(fb),
    );

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
    let p = Value::Record(record_type("P"), Arc::new(fa));

    let mut fb = BTreeMap::new();
    fb.insert("x".to_string(), Value::Int(1));
    let r = Value::Record(
        silt::typeinfo::builtin_type(silt::typeinfo::ty::ANON_RECORD).clone(),
        Arc::new(fb),
    );

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
    let person = Value::Record(record_type("Person"), Arc::new(fa));

    let mut fb = BTreeMap::new();
    fb.insert("x".to_string(), Value::Int(1));
    let car = Value::Record(record_type("Car"), Arc::new(fb));

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
    let p = Value::Record(record_type("P"), Arc::new(fa));

    let mut fb = BTreeMap::new();
    fb.insert("x".to_string(), Value::Int(1));
    let r = Value::Record(
        silt::typeinfo::builtin_type(silt::typeinfo::ty::ANON_RECORD).clone(),
        Arc::new(fb),
    );

    assert_eq!(p, r, "round-84 `<anon>`-wildcard PartialEq must still hold");
}

/// A program's record type named `name`, with an id of its own.
fn record_type(name: &str) -> std::sync::Arc<silt::typeinfo::TypeInfo> {
    let id = name
        .bytes()
        .fold(9000u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32))
        % 100_000
        + 10_000;
    silt::typeinfo::TypeInfo::new_record(
        silt::defs::TypeId(silt::defs::DefId(id)),
        name,
        Vec::new(),
    )
}
