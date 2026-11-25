#![allow(clippy::mutable_key_type)] // intentional: locks Value-as-key ordering

//! Regression tests for round-23 audit fix: `Value::Ord` returning
//! `Equal` for distinct `VmClosure` / `BuiltinFn` /
//! `VariantConstructor` values caused `BTreeSet` / `BTreeMap` (used by
//! the `MakeSet` / `MakeMap` opcodes) to silently drop "duplicates" that
//! `PartialEq` already considers distinct.
//!
//! Rust's `Ord` contract requires that `a == b` ⇒ `cmp(a,b) == Equal`,
//! and contrapositively `a != b` ⇒ `cmp(a,b) != Equal`. Silt's
//! `PartialEq` returns `false` for these three kinds via the catch-all
//! `_ => false` arm, so `Ord` must never return `Equal` for distinct
//! instances — otherwise the `BTreeSet` backing `Set` and `BTreeMap`
//! backing `Map` will deduplicate values that the language layer
//! considers distinct, losing user data.

use silt::testing::ValueInside;
use silt::testing::typeinfo::bv;
use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::sync::Arc;

use silt::testing::bytecode::{Function, VmClosure};
use silt::testing::value::Value;

/// The builtin function `name` (`println`, `list.map`) as a value.
fn builtin(name: &str) -> Value {
    silt::testing::builtins::registry::registry()
        .named(name)
        .unwrap_or_else(|| panic!("the builtin {name}"))
        .value()
}

// ── VmClosure ──────────────────────────────────────────────────────

fn mk_closure(name: &str) -> Value {
    Value::VmClosure(Arc::new(VmClosure::new(
        Arc::new(Function::returning_unit(name.to_string(), 0)),
        Vec::new(),
    )))
}

/// Closures of two functions (even if the names collide) must not be
/// considered equal by `Ord`.
#[test]
fn ord_vmclosure_distinct_arcs_not_equal() {
    let c1 = mk_closure("f");
    let c2 = mk_closure("g");
    assert_ne!(c1.cmp(&c2), Ordering::Equal);
    // Even with identical names, two functions must remain distinct.
    let c3 = mk_closure("f");
    let c4 = mk_closure("f");
    assert_ne!(c3.cmp(&c4), Ordering::Equal);
}

/// A `BTreeSet` of closures of functions with identical names (but
/// three functions) must retain all of them.
#[test]
fn btreeset_of_distinct_closures_retains_all() {
    let mut s = BTreeSet::new();
    s.insert(mk_closure("inc"));
    s.insert(mk_closure("inc"));
    s.insert(mk_closure("inc"));
    assert_eq!(
        s.len(),
        3,
        "closures of distinct functions must not be deduplicated"
    );
}

/// A shared (cloned) `Arc<VmClosure>` compares Equal to itself (it
/// runs the same function with the same captures).
#[test]
fn ord_vmclosure_same_arc_equal() {
    let f = Arc::new(VmClosure::new(
        Arc::new(Function::returning_unit("id".into(), 0)),
        Vec::new(),
    ));
    let a = Value::VmClosure(f.clone());
    let b = Value::VmClosure(f);
    assert_eq!(a.cmp(&b), Ordering::Equal);
}

// ── BuiltinFn ──────────────────────────────────────────────────────

/// Two `BuiltinFn` values with different names must order non-Equal.
#[test]
fn ord_builtin_fn_distinct_names_not_equal() {
    let a = builtin("println");
    let b = builtin("print");
    assert_ne!(a.cmp(&b), Ordering::Equal);
    assert_eq!(a.cmp(&b).reverse(), b.cmp(&a));
}

/// `BTreeSet` of builtin fns with distinct names retains all entries.
#[test]
fn btreeset_of_distinct_builtin_fns_retains_all() {
    let mut s = BTreeSet::new();
    s.insert(builtin("println"));
    s.insert(builtin("print"));
    s.insert(builtin("list.map"));
    assert_eq!(s.len(), 3);
}

// ── VariantConstructor ─────────────────────────────────────────────

/// Distinct constructors (another variant, or a variant of one name in
/// another enum) must order non-Equal.
#[test]
fn ord_variant_constructor_distinct_not_equal() {
    let a = Value::VariantConstructor(bv::SOME.tag());
    let b = Value::VariantConstructor(bv::OK.tag());
    let other = silt::testing::typeinfo::TypeInfo::new_enum(
        silt::testing::defs::TypeId(silt::testing::defs::DefId(9000)),
        "Maybe",
        &[("Some", 1)],
    );
    let c = Value::VariantConstructor(silt::testing::typeinfo::Tag::new(other, 0)); // same name, another enum
    assert_ne!(a.cmp(&b), Ordering::Equal);
    assert_ne!(a.cmp(&c), Ordering::Equal);
    assert_ne!(b.cmp(&c), Ordering::Equal);
}

/// `BTreeSet` of distinct constructors retains all.
#[test]
fn btreeset_of_distinct_variant_constructors_retains_all() {
    let mut s = BTreeSet::new();
    s.insert(Value::VariantConstructor(bv::SOME.tag()));
    s.insert(Value::VariantConstructor(bv::NONE.tag()));
    s.insert(Value::VariantConstructor(bv::OK.tag()));
    s.insert(Value::VariantConstructor(bv::ERR.tag()));
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
    let float =
        |f: f64| Value::Float(silt::testing::value::Float::new(f).expect("a finite number"));
    assert_eq!(Value::Int(1).cmp(&float(0.0)), Ordering::Less);
    assert_eq!(float(1e9).cmp(&Value::string("")), Ordering::Less);
    assert_eq!(Value::string("").cmp(&Value::list(vec![])), Ordering::Less);
}

/// Value-level ordering within a type still uses content comparison
/// (not identity).
#[test]
fn ord_value_types_still_content_compared() {
    assert_eq!(Value::Int(1).cmp(&Value::Int(2)), Ordering::Less);
    assert_eq!(Value::string("a").cmp(&Value::string("b")), Ordering::Less);
    assert_eq!(
        Value::tuple(vec![Value::Int(1)]).cmp(&Value::tuple(vec![Value::Int(2)])),
        Ordering::Less
    );
    // Reflexivity for value types.
    assert_eq!(Value::Int(42).cmp(&Value::Int(42)), Ordering::Equal);
    assert_eq!(Value::string("x").cmp(&Value::string("x")), Ordering::Equal);
}
