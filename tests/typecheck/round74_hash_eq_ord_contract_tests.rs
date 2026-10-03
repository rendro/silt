//! Regression tests for round-74 audit fix: `Value::Hash` violated the
//! Hash/Eq contract for the cross-discriminant equal pair List↔Range —
//! incompatible with `PartialEq`, which considers
//! `List([1,2,3]) == Range(1,3)`.
//!
//! Rust's contract: `a == b` ⇒ `hash(a) == hash(b)` AND `a.cmp(&b) ==
//! Equal`. This test pins down each cross-discriminant equal pair and
//! asserts all three predicates.

use silt::typeinfo::bv;
use std::cmp::Ordering;
use std::collections::HashSet;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use silt::bytecode::{Function, VmClosure};
use silt::value::{TaskHandle, Value};

fn hash_of(v: &Value) -> u64 {
    let mut h = DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

// ── List ↔ Range ───────────────────────────────────────────────────

#[test]
fn list_range_equal_pairs_hash_equal() {
    // Range(1,3) materializes to [1,2,3]; PartialEq returns true (line
    // ~1729-1730), so Hash and Ord must agree.
    let list = Value::List(Arc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]));
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

// ── Round 75 lock tightenings: reflexivity arms for opaque values ──
//
// The original round-74 contract tests covered
// List↔Range — the cross-discriminant equality cases that motivated
// the round-74 audit fix. They did NOT cover Handle, VmClosure,
// BuiltinFn, or VariantConstructor reflexivity. The PartialEq impl
// at value.rs:1759-1762 declares:
//   - Handle(a) == Handle(b) iff a.id == b.id
//   - VmClosure(a) == VmClosure(b) iff Arc::ptr_eq(a, b)
//   - BuiltinFn(a) == BuiltinFn(b) iff a == b
//   - VariantConstructor(na, aa) == VariantConstructor(nb, ab) iff
//     na == nb && aa == ab
// Without reflexivity locks, a future regression that special-cases
// these arms (e.g. always returning false) wouldn't trip the test
// suite. Use a HashSet insertion-dedup probe (round-74 style) so
// Hash/Eq are exercised together — `s.insert(h); s.insert(h);
// assert size == 1` is the canonical contract probe.

#[test]
fn handle_reflexive_eq_hash_dedup() {
    // TaskHandle id 42 — same id ⇒ eq ⇒ hash equal ⇒ HashSet dedups.
    let h = Arc::new(TaskHandle::new(42));
    let a = Value::Handle(h.clone());
    let b = Value::Handle(h);
    assert_eq!(a, b, "Handle(a) == Handle(a) (same id) must hold");
    assert_eq!(
        hash_of(&a),
        hash_of(&b),
        "Hash/Eq contract: equal Handles must hash equal"
    );
    assert_eq!(
        a.cmp(&b),
        Ordering::Equal,
        "Ord/Eq contract: a == b ⇒ a.cmp(&b) == Equal"
    );
    let mut s: HashSet<Value> = HashSet::new();
    s.insert(a);
    s.insert(b);
    assert_eq!(
        s.len(),
        1,
        "HashSet must dedup two Value::Handle wrappers around the same TaskHandle Arc"
    );
}

#[test]
fn vm_closure_reflexive_eq_hash_dedup() {
    // VmClosure equality is by Arc::ptr_eq — same Arc, same identity.
    let func = Arc::new(Function::new("test_fn".into(), 0));
    let closure = Arc::new(VmClosure {
        function: func,
        upvalues: Vec::new(),
    });
    let a = Value::VmClosure(closure.clone());
    let b = Value::VmClosure(closure);
    assert_eq!(
        a, b,
        "VmClosure(arc) == VmClosure(arc) (same Arc) must hold"
    );
    assert_eq!(
        hash_of(&a),
        hash_of(&b),
        "Hash/Eq contract: equal VmClosures must hash equal"
    );
    assert_eq!(
        a.cmp(&b),
        Ordering::Equal,
        "Ord/Eq contract: a == b ⇒ a.cmp(&b) == Equal"
    );
    let mut s: HashSet<Value> = HashSet::new();
    s.insert(a);
    s.insert(b);
    assert_eq!(
        s.len(),
        1,
        "HashSet must dedup two Value::VmClosure wrappers around the same Arc"
    );
}

#[test]
fn builtin_fn_reflexive_eq_hash_dedup() {
    // BuiltinFn equality is by name string.
    let a = Value::BuiltinFn("println".into());
    let b = Value::BuiltinFn("println".into());
    assert_eq!(
        a, b,
        "BuiltinFn(name) == BuiltinFn(name) (same name) must hold"
    );
    assert_eq!(
        hash_of(&a),
        hash_of(&b),
        "Hash/Eq contract: equal BuiltinFns must hash equal"
    );
    assert_eq!(
        a.cmp(&b),
        Ordering::Equal,
        "Ord/Eq contract: a == b ⇒ a.cmp(&b) == Equal"
    );
    let mut s: HashSet<Value> = HashSet::new();
    s.insert(a);
    s.insert(b);
    assert_eq!(
        s.len(),
        1,
        "HashSet must dedup two Value::BuiltinFn(\"println\") values"
    );
}

#[test]
fn variant_constructor_reflexive_eq_hash_dedup() {
    // VariantConstructor equality is by (name, arity).
    let a = Value::VariantConstructor(bv::SOME.tag());
    let b = Value::VariantConstructor(bv::SOME.tag());
    assert_eq!(
        a, b,
        "VariantConstructor(name, arity) == VariantConstructor(name, arity) must hold"
    );
    assert_eq!(
        hash_of(&a),
        hash_of(&b),
        "Hash/Eq contract: equal VariantConstructors must hash equal"
    );
    assert_eq!(
        a.cmp(&b),
        Ordering::Equal,
        "Ord/Eq contract: a == b ⇒ a.cmp(&b) == Equal"
    );
    let mut s: HashSet<Value> = HashSet::new();
    s.insert(a);
    s.insert(b);
    assert_eq!(
        s.len(),
        1,
        "HashSet must dedup two Value::VariantConstructor(\"Some\", 1) values"
    );
}

// Cross-discriminant negative locks: opaque values must NOT collapse
// across discriminants. Without these, a regression that hashed every
// opaque variant to the same bucket would still pass the reflexivity
// tests above. (We assert hash inequality probabilistically via the
// HashSet len check — collisions are possible but the inserted Values
// are inequal so dedup must NOT fire regardless.)

#[test]
fn opaque_values_distinct_across_discriminants() {
    let h = Arc::new(TaskHandle::new(1));
    let func = Arc::new(Function::new("f".into(), 0));
    let closure = Arc::new(VmClosure {
        function: func,
        upvalues: Vec::new(),
    });
    let handle = Value::Handle(h);
    let vm_closure = Value::VmClosure(closure);
    let builtin = Value::BuiltinFn("f".into());
    let ctor = Value::VariantConstructor(bv::OK.tag());
    // Pairwise: must all be unequal.
    assert_ne!(handle, vm_closure);
    assert_ne!(handle, builtin);
    assert_ne!(handle, ctor);
    assert_ne!(vm_closure, builtin);
    assert_ne!(vm_closure, ctor);
    assert_ne!(builtin, ctor);
    // HashSet length must be 4 — no false collapses.
    let mut s: HashSet<Value> = HashSet::new();
    s.insert(handle);
    s.insert(vm_closure);
    s.insert(builtin);
    s.insert(ctor);
    assert_eq!(
        s.len(),
        4,
        "four distinct opaque values must NOT dedup across discriminants"
    );
}
