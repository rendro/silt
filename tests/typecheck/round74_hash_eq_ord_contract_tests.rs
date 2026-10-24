//! The list a range makes (`1..3`, which holds no element) and the list
//! of the same elements made any other way are equal, so they hash alike
//! and are ordered as equal.
//!
//! Rust's contract: `a == b` ⇒ `hash(a) == hash(b)` AND `a.cmp(&b) ==
//! Equal`. These tests assert all three for each such pair.

use silt::typeinfo::bv;
use std::cmp::Ordering;
use std::collections::HashSet;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use silt::bytecode::{Function, VmClosure};
use silt::runtime::handle::TaskHandle;
use silt::value::Value;

/// The builtin function `name` (`println`, `list.map`) as a value.
fn builtin(name: &str) -> Value {
    silt::builtins::registry::registry()
        .named(name)
        .unwrap_or_else(|| panic!("the builtin {name}"))
        .value()
}

/// The list `lo..hi` makes: it holds no element.
fn range(lo: i64, hi: i64) -> Value {
    Value::List(silt::value::List::ints(lo, hi).expect("a list"))
}

fn hash_of(v: &Value) -> u64 {
    let mut h = DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

// ── A list that holds its elements, and the list a range makes ────

#[test]
fn list_range_equal_pairs_hash_equal() {
    let list = Value::list(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
    let range = range(1, 3);
    assert_eq!(list, range);
    assert_eq!(range, list);
    assert_eq!(hash_of(&list), hash_of(&range));
    assert_eq!(list.cmp(&range), Ordering::Equal);
    assert_eq!(range.cmp(&list), Ordering::Equal);
}

#[test]
fn list_range_empty_hash_equal() {
    // Empty range (lo > hi) equals empty list per PartialEq.
    let list = Value::list(vec![]);
    let range = range(5, 4);
    assert_eq!(list, range);
    assert_eq!(hash_of(&list), hash_of(&range));
    assert_eq!(list.cmp(&range), Ordering::Equal);
}

#[test]
fn list_range_single_element_hash_equal() {
    let list = Value::list(vec![Value::Int(42)]);
    let range = range(42, 42);
    assert_eq!(list, range);
    assert_eq!(hash_of(&list), hash_of(&range));
    assert_eq!(list.cmp(&range), Ordering::Equal);
}

#[test]
fn list_range_unequal_pairs_ord_consistent() {
    // List doesn't materialize to range: should NOT be Equal.
    let list = Value::list(vec![
        Value::Int(1),
        Value::Int(3),
        Value::Int(2), // out of order — not a range
    ]);
    let range = range(1, 3);
    assert_ne!(list, range);
    assert_ne!(list.cmp(&range), Ordering::Equal);
}

// ── HashMap / HashSet round-trip ───────────────────────────────────

/// A `HashSet` takes the two for one value.
#[test]
fn hashset_dedup_across_list_range() {
    use std::collections::HashSet;
    let mut s: HashSet<Value> = HashSet::new();
    s.insert(Value::list(vec![
        Value::Int(1),
        Value::Int(2),
        Value::Int(3),
    ]));
    s.insert(range(1, 3));
    assert_eq!(
        s.len(),
        1,
        "HashSet must dedup [1, 2, 3] and 1..3 — they're equal"
    );
}

// ── Round 75 lock tightenings: reflexivity arms for opaque values ──
//
// The original round-74 contract tests covered a list against the
// list a range makes: one kind of value stored in two ways, the case
// that motivated the round-74 audit fix. They did NOT cover Handle, VmClosure,
// BuiltinFn, or VariantConstructor reflexivity. The PartialEq impl
// in value/key.rs declares:
//   - Handle(a) == Handle(b) iff a.id == b.id
//   - VmClosure(a) == VmClosure(b) iff they run one Function and
//     their captures are equal
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

/// A closure of `function` that captured `captures`.
fn closure_of(function: &Arc<Function>, captures: Vec<Value>) -> Value {
    Value::VmClosure(Arc::new(VmClosure::new(function.clone(), captures)))
}

/// Asserts the contract for two values that are equal: they hash
/// alike, are ordered as equal, and a `HashSet` and a `BTreeSet` take
/// them for one value.
fn assert_one_value(a: Value, b: Value) {
    assert_eq!(a, b);
    assert_eq!(b, a);
    assert_eq!(hash_of(&a), hash_of(&b), "Hash/Eq contract");
    assert_eq!(a.cmp(&b), Ordering::Equal, "Ord/Eq contract");
    assert_eq!(b.cmp(&a), Ordering::Equal, "Ord/Eq contract");
    let hashed: HashSet<Value> = [a.clone(), b.clone()].into_iter().collect();
    assert_eq!(hashed.len(), 1);
    let ordered: std::collections::BTreeSet<Value> = [a, b].into_iter().collect();
    assert_eq!(ordered.len(), 1);
}

#[test]
fn vm_closure_reflexive_eq_hash_dedup() {
    // One closure, two references to it.
    let func = Arc::new(Function::returning_unit("test_fn".into(), 0));
    let closure = closure_of(&func, Vec::new());
    assert_one_value(closure.clone(), closure);
}

/// Two closures of one function with equal captures are one value,
/// though each is an allocation of its own: a task's own copy of a
/// top-level function is that function.
#[test]
fn vm_closures_of_one_function_with_equal_captures_are_equal() {
    let func = Arc::new(Function::returning_unit("f".into(), 0));
    assert_one_value(closure_of(&func, Vec::new()), closure_of(&func, Vec::new()));
    let captures = || vec![Value::Int(1), Value::String("a".into())];
    assert_one_value(closure_of(&func, captures()), closure_of(&func, captures()));
    // The captures are compared as values: a closure among them too.
    let inner = Arc::new(Function::returning_unit("g".into(), 0));
    assert_one_value(
        closure_of(&func, vec![closure_of(&inner, vec![Value::Int(7)])]),
        closure_of(&func, vec![closure_of(&inner, vec![Value::Int(7)])]),
    );
}

/// Closures of one function with different captures are two values,
/// in the order of their captures, the way records are in the order
/// of their fields.
#[test]
fn vm_closures_of_one_function_with_different_captures_differ() {
    let func = Arc::new(Function::returning_unit("f".into(), 0));
    let one = closure_of(&func, vec![Value::Int(1), Value::Int(9)]);
    let two = closure_of(&func, vec![Value::Int(2), Value::Int(0)]);
    assert_ne!(one, two);
    assert_eq!(one.cmp(&two), Ordering::Less);
    assert_eq!(two.cmp(&one), Ordering::Greater);
    let inner = Arc::new(Function::returning_unit("g".into(), 0));
    let deep_one = closure_of(&func, vec![closure_of(&inner, vec![Value::Int(1)])]);
    let deep_two = closure_of(&func, vec![closure_of(&inner, vec![Value::Int(2)])]);
    assert_ne!(deep_one, deep_two);
    assert_eq!(deep_one.cmp(&deep_two), Ordering::Less);
    let hashed: HashSet<Value> = [one, two, deep_one, deep_two].into_iter().collect();
    assert_eq!(hashed.len(), 4);
}

/// Closures of two functions are two values, whatever they capture,
/// even if the functions have one name.
#[test]
fn vm_closures_of_two_functions_differ() {
    let f = Arc::new(Function::returning_unit("f".into(), 0));
    let g = Arc::new(Function::returning_unit("f".into(), 0));
    let a = closure_of(&f, vec![Value::Int(1)]);
    let b = closure_of(&g, vec![Value::Int(1)]);
    assert_ne!(a, b);
    assert_ne!(a.cmp(&b), Ordering::Equal);
    assert_eq!(a.cmp(&b), b.cmp(&a).reverse());
    // The function decides before the captures do.
    let a_low = closure_of(&f, vec![Value::Int(0)]);
    let b_high = closure_of(&g, vec![Value::Int(9)]);
    assert_eq!(a_low.cmp(&b_high), a.cmp(&b));
}

/// The order of closures is total: any list of them sorts into one
/// order, whatever order it starts in, and closures of one function
/// are in the order of their captures on every run.
#[test]
fn vm_closure_order_is_total_and_stable() {
    let f = Arc::new(Function::returning_unit("f".into(), 0));
    let g = Arc::new(Function::returning_unit("g".into(), 0));
    let values = vec![
        closure_of(&f, vec![Value::Int(3)]),
        closure_of(&g, vec![Value::Int(1)]),
        closure_of(&f, vec![Value::Int(1)]),
        closure_of(&g, vec![]),
        closure_of(&f, vec![Value::Int(2)]),
        closure_of(&f, vec![Value::Int(1)]),
    ];
    // Antisymmetric and transitive over every pair and triple.
    for a in &values {
        for b in &values {
            assert_eq!(a.cmp(b), b.cmp(a).reverse());
            assert_eq!(a.cmp(b) == Ordering::Equal, a == b);
            for c in &values {
                if a.cmp(b) != Ordering::Greater && b.cmp(c) != Ordering::Greater {
                    assert_ne!(a.cmp(c), Ordering::Greater);
                }
            }
        }
    }
    let mut forward = values.clone();
    forward.sort();
    let mut backward: Vec<Value> = values.iter().rev().cloned().collect();
    backward.sort();
    assert_eq!(forward, backward);
    // The closures of `f` sit side by side, by their captures.
    let of_f: Vec<Value> = forward
        .iter()
        .filter(|value| matches!(value, Value::VmClosure(c) if Arc::ptr_eq(&c.function, &f)))
        .cloned()
        .collect();
    assert_eq!(
        of_f,
        vec![
            closure_of(&f, vec![Value::Int(1)]),
            closure_of(&f, vec![Value::Int(1)]),
            closure_of(&f, vec![Value::Int(2)]),
            closure_of(&f, vec![Value::Int(3)]),
        ]
    );
    let set: std::collections::BTreeSet<Value> = values.into_iter().collect();
    assert_eq!(set.len(), 5);
}

#[test]
fn builtin_fn_reflexive_eq_hash_dedup() {
    // BuiltinFn equality is by name string.
    let a = builtin("println");
    let b = builtin("println");
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
    let func = Arc::new(Function::returning_unit("f".into(), 0));
    let closure = Arc::new(VmClosure::new(func, Vec::new()));
    let handle = Value::Handle(h);
    let vm_closure = Value::VmClosure(closure);
    let builtin = builtin("panic");
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
