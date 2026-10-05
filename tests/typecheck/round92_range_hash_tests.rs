//! Regression tests for round-92 audit fix: `Hash for Value::Range` walked
//! the whole range unbounded — `(0..4_000_000_000).hash()` spun ~4e9
//! iterations inside a single opcode (uninterruptible by the time-slice
//! scheduler) and `0..i64::MAX` hung the program forever. Every other range
//! materialization site is guarded by `checked_range_len`; Hash was the
//! single unguarded walk.
//!
//! Fix (src/value/key.rs, `Value::Range` arm of `impl Hash`): cap the
//! element-by-element walk at `MAX_RANGE_MATERIALIZE` (10_000_000) and hash
//! over-cap ranges in closed form (`tag, len, lo, hi`). Contract reasoning:
//!   - within the cap the byte stream is identical to the equal `List`'s,
//!     so the round-74 List ↔ Range hash contract is preserved;
//!   - over the cap, no `Value::List` can exist with > cap elements (every
//!     list-producing site enforces the cap), so the only values equal to
//!     an over-cap Range are Ranges with identical endpoints — hashing the
//!     endpoints is contract-safe;
//!   - doing the cap inside `impl Hash` (not the dispatch arm) bounds the
//!     nested auto-derive paths (record/variant/tuple/list containing a
//!     range field) for free, since the impl recurses into fields.
//!
//! The silt-level end-to-end cases live as golden cases under
//! `tests/golden/typecheck/ranges/round92_range_hash__*` (the harness's
//! per-case timeout turns a pre-fix hang into a failure).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

use silt::typeinfo::bv;
use silt::value::Value;

/// Mirror of `silt::value::MAX_RANGE_MATERIALIZE` (pub(crate), not
/// re-exported). If the constant changes, the boundary tests below should
/// be updated to match.
const CAP: i64 = 10_000_000;

fn hash_of(v: &Value) -> u64 {
    let mut h = DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

// ── (a) Rust-level: huge range hashes complete promptly ────────────

#[test]
fn huge_range_hash_completes_promptly() {
    let start = Instant::now();
    // ~4e9 elements — the audit repro. Pre-fix this alone takes minutes.
    let _ = hash_of(&Value::Range(0, 4_000_000_000));
    // ~2^63 elements — pre-fix this never terminates.
    let _ = hash_of(&Value::Range(0, i64::MAX));
    let _ = hash_of(&Value::Range(i64::MIN, i64::MAX));
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "over-cap range hashing must be closed-form (O(1)), took {:?}",
        start.elapsed()
    );
}

#[test]
fn range_hash_at_cap_boundary_completes() {
    let start = Instant::now();
    // Exactly at the cap: still element-by-element (matches list hashing).
    let _ = hash_of(&Value::Range(1, CAP));
    // One past the cap: closed-form.
    let _ = hash_of(&Value::Range(1, CAP + 1));
    assert!(
        start.elapsed() < Duration::from_secs(30),
        "cap-boundary range hashing took {:?}",
        start.elapsed()
    );
}

// ── (b) Rust-level: List ↔ Range contract preserved within the cap ──

#[test]
fn small_list_range_equal_pairs_still_hash_equal() {
    let list = Value::List(Arc::new((1..=5).map(Value::Int).collect()));
    let range = Value::Range(1, 5);
    assert_eq!(list, range, "PartialEq: List([1..5]) == Range(1,5)");
    assert_eq!(
        hash_of(&list),
        hash_of(&range),
        "round-74 contract: equal List/Range pairs must hash equal"
    );
}

#[test]
fn empty_ranges_hash_equal_to_each_other_and_empty_list() {
    // All empty ranges compare equal regardless of endpoints, and equal
    // the empty list; the over-cap closed form must not disturb this
    // (empty ranges take the len == 0 path).
    let a = Value::Range(5, 4);
    let b = Value::Range(100, 2);
    let empty = Value::List(Arc::new(vec![]));
    assert_eq!(a, b);
    assert_eq!(a, empty);
    assert_eq!(hash_of(&a), hash_of(&b));
    assert_eq!(hash_of(&a), hash_of(&empty));
}

// ── (d) Rust-level: equal over-cap ranges hash equal ───────────────

#[test]
fn equal_over_cap_ranges_hash_equal() {
    let a = Value::Range(0, 4_000_000_000);
    let b = Value::Range(0, 4_000_000_000);
    assert_eq!(a, b, "non-empty ranges are equal iff endpoints match");
    assert_eq!(
        hash_of(&a),
        hash_of(&b),
        "Hash/Eq contract: equal over-cap ranges must hash equal"
    );
    // Distinct over-cap ranges are unequal (no hash requirement, but the
    // closed form should keep them distinguishable in practice).
    let c = Value::Range(1, 4_000_000_001);
    assert_ne!(a, c);
}

// ── (c) Rust-level: nested huge ranges (auto-derive recursion path) ─

#[test]
fn nested_huge_range_hash_completes_promptly() {
    use std::collections::BTreeMap;
    let start = Instant::now();

    // Record with an over-cap range field.
    let mut fields = BTreeMap::new();
    fields.insert("xs".to_string(), Value::Range(0, i64::MAX));
    let _ = hash_of(&Value::Record(record_type("R"), Arc::new(fields)));

    // Tuple, list, and variant containing over-cap ranges.
    let _ = hash_of(&Value::Tuple(vec![
        Value::Int(1),
        Value::Range(0, 4_000_000_000),
    ]));
    let _ = hash_of(&Value::List(Arc::new(vec![Value::Range(0, i64::MAX)])));
    let _ = hash_of(&Value::variant(
        bv::SOME,
        vec![Value::Range(i64::MIN, i64::MAX)],
    ));

    assert!(
        start.elapsed() < Duration::from_secs(5),
        "nested over-cap range hashing must be O(1) per range, took {:?}",
        start.elapsed()
    );
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
