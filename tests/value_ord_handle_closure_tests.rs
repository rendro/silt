//! Regression tests for round-23 audit fix: `Value::Ord` returning
//! `Equal` for distinct `Handle` / `VmClosure` / `BuiltinFn` /
//! `VariantConstructor` values caused `BTreeSet` / `BTreeMap` (used by
//! the `MakeSet` / `MakeMap` opcodes) to silently drop "duplicates" that
//! `PartialEq` already considers distinct.
//!
//! Rust's `Ord` contract requires that `a == b` ⇒ `cmp(a,b) == Equal`,
//! and contrapositively `a != b` ⇒ `cmp(a,b) != Equal`. Silt's
//! `PartialEq` returns `false` for these four kinds via the catch-all
//! `_ => false` arm, so `Ord` must never return `Equal` for distinct
//! instances — otherwise the `BTreeSet` backing `Set` and `BTreeMap`
//! backing `Map` will deduplicate values that the language layer
//! considers distinct, losing user data.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use silt::bytecode::{Function, VmClosure};
use silt::value::{TaskHandle, Value};

// ── TaskHandle ─────────────────────────────────────────────────────

/// Two distinct `TaskHandle` values (different `id`s) must compare
/// non-Equal via `Ord`, matching the `PartialEq` behavior of returning
/// `false` for them.
#[test]
fn ord_handle_distinct_ids_not_equal() {
    let h1 = Value::Handle(Arc::new(TaskHandle::new(1)));
    let h2 = Value::Handle(Arc::new(TaskHandle::new(2)));
    assert_ne!(h1.cmp(&h2), Ordering::Equal);
    assert_ne!(h2.cmp(&h1), Ordering::Equal);
    // Directional consistency: one side must be Less and the other Greater.
    assert_eq!(h1.cmp(&h2).reverse(), h2.cmp(&h1));
}

/// A `BTreeSet` built from distinct handles (the primary user-visible
/// symptom — the `MakeSet` opcode builds a `BTreeSet`) must retain all
/// elements.
#[test]
fn btreeset_of_distinct_handles_retains_all() {
    let h1 = Value::Handle(Arc::new(TaskHandle::new(10)));
    let h2 = Value::Handle(Arc::new(TaskHandle::new(20)));
    let h3 = Value::Handle(Arc::new(TaskHandle::new(30)));
    let mut s = BTreeSet::new();
    s.insert(h1);
    s.insert(h2);
    s.insert(h3);
    assert_eq!(
        s.len(),
        3,
        "BTreeSet must not drop distinct handles as duplicates"
    );
}

/// Same check for `BTreeMap` (backs `Map` literals via `MakeMap`): three
/// distinct handles used as keys must all survive insertion.
#[test]
fn btreemap_keyed_by_distinct_handles_retains_all() {
    let h1 = Value::Handle(Arc::new(TaskHandle::new(1)));
    let h2 = Value::Handle(Arc::new(TaskHandle::new(2)));
    let h3 = Value::Handle(Arc::new(TaskHandle::new(3)));
    let mut m = BTreeMap::new();
    m.insert(h1, Value::Int(1));
    m.insert(h2, Value::Int(2));
    m.insert(h3, Value::Int(3));
    assert_eq!(m.len(), 3);
}

// ── VmClosure ──────────────────────────────────────────────────────

fn mk_closure(name: &str) -> Value {
    Value::VmClosure(Arc::new(VmClosure {
        function: Arc::new(Function::new(name.to_string(), 0)),
        upvalues: Vec::new(),
    }))
}

/// Two distinct `VmClosure` values (different `Arc` allocations, even if
/// names happen to collide) must not be considered equal by `Ord`.
#[test]
fn ord_vmclosure_distinct_arcs_not_equal() {
    let c1 = mk_closure("f");
    let c2 = mk_closure("g");
    assert_ne!(c1.cmp(&c2), Ordering::Equal);
    // Even with identical names, distinct Arc allocations must remain distinct.
    let c3 = mk_closure("f");
    let c4 = mk_closure("f");
    assert_ne!(c3.cmp(&c4), Ordering::Equal);
}

/// A `BTreeSet` of closures with identical names (but distinct `Arc`
/// allocations) must retain all of them.
#[test]
fn btreeset_of_distinct_closures_retains_all() {
    let mut s = BTreeSet::new();
    s.insert(mk_closure("inc"));
    s.insert(mk_closure("inc"));
    s.insert(mk_closure("inc"));
    assert_eq!(
        s.len(),
        3,
        "distinct VmClosure Arc allocations must not be deduplicated"
    );
}

/// A shared (cloned) `Arc<VmClosure>` compares Equal to itself (it is
/// the same Arc), which is the expected identity behavior for a single
/// closure value.
#[test]
fn ord_vmclosure_same_arc_equal() {
    let f = Arc::new(VmClosure {
        function: Arc::new(Function::new("id".into(), 0)),
        upvalues: Vec::new(),
    });
    let a = Value::VmClosure(f.clone());
    let b = Value::VmClosure(f);
    assert_eq!(a.cmp(&b), Ordering::Equal);
}

// ── BuiltinFn ──────────────────────────────────────────────────────

/// Two `BuiltinFn` values with different names must order non-Equal.
#[test]
fn ord_builtin_fn_distinct_names_not_equal() {
    let a = Value::BuiltinFn("println".into());
    let b = Value::BuiltinFn("print".into());
    assert_ne!(a.cmp(&b), Ordering::Equal);
    assert_eq!(a.cmp(&b).reverse(), b.cmp(&a));
}

/// `BTreeSet` of builtin fns with distinct names retains all entries.
#[test]
fn btreeset_of_distinct_builtin_fns_retains_all() {
    let mut s = BTreeSet::new();
    s.insert(Value::BuiltinFn("println".into()));
    s.insert(Value::BuiltinFn("print".into()));
    s.insert(Value::BuiltinFn("map".into()));
    assert_eq!(s.len(), 3);
}

// ── VariantConstructor ─────────────────────────────────────────────

/// Distinct constructors (different name, different arity, or both)
/// must order non-Equal.
#[test]
fn ord_variant_constructor_distinct_not_equal() {
    let a = Value::VariantConstructor("Some".into(), 1);
    let b = Value::VariantConstructor("None".into(), 0);
    let c = Value::VariantConstructor("Some".into(), 2); // same name, different arity
    assert_ne!(a.cmp(&b), Ordering::Equal);
    assert_ne!(a.cmp(&c), Ordering::Equal);
    assert_ne!(b.cmp(&c), Ordering::Equal);
}

/// `BTreeSet` of distinct constructors retains all.
#[test]
fn btreeset_of_distinct_variant_constructors_retains_all() {
    let mut s = BTreeSet::new();
    s.insert(Value::VariantConstructor("Some".into(), 1));
    s.insert(Value::VariantConstructor("None".into(), 0));
    s.insert(Value::VariantConstructor("Ok".into(), 1));
    s.insert(Value::VariantConstructor("Err".into(), 1));
    assert_eq!(s.len(), 4);
}

// ── Preserved invariants ───────────────────────────────────────────
//
// The fix must NOT alter ordering for types where `PartialEq` already
// distinguishes by value. Spot-check the load-bearing discriminant
// ordering and value-level ordering.

/// Discriminant-based ordering: `Int < Float < String` (per the `disc`
/// table in `Ord::cmp`).
#[test]
fn ord_discriminant_ordering_preserved() {
    assert_eq!(Value::Int(1).cmp(&Value::Float(0.0)), Ordering::Less);
    assert_eq!(
        Value::Float(1e9).cmp(&Value::String("".into())),
        Ordering::Less
    );
    assert_eq!(
        Value::String("".into()).cmp(&Value::List(Arc::new(vec![]))),
        Ordering::Less
    );
}

/// Value-level ordering within a type still uses content comparison
/// (not identity).
#[test]
fn ord_value_types_still_content_compared() {
    assert_eq!(Value::Int(1).cmp(&Value::Int(2)), Ordering::Less);
    assert_eq!(
        Value::String("a".into()).cmp(&Value::String("b".into())),
        Ordering::Less
    );
    assert_eq!(
        Value::Tuple(vec![Value::Int(1)]).cmp(&Value::Tuple(vec![Value::Int(2)])),
        Ordering::Less
    );
    // Reflexivity for value types.
    assert_eq!(Value::Int(42).cmp(&Value::Int(42)), Ordering::Equal);
    assert_eq!(
        Value::String("x".into()).cmp(&Value::String("x".into())),
        Ordering::Equal
    );
}
