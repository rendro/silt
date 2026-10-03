//! Regression tests for round-26 finding L6: `Value::PartialEq`
//! returned `false` for reflexive comparisons on `Handle`, `VmClosure`,
//! `BuiltinFn`, and `VariantConstructor` via the catch-all `_ => false`
//! arm. That violated the `Eq` reflexivity contract (`a == a` ≡ `true`)
//! AND desynchronized `PartialEq` from `Ord` (round-23 added explicit
//! arms to `Ord` returning identity-based `Equal`).
//!
//! The fix mirrors the `Ord` arms into `PartialEq`:
//!   - `Handle(a) == Handle(b)` iff `a.id == b.id`
//!   - `VmClosure(a) == VmClosure(b)` iff `Arc::ptr_eq(a, b)`
//!   - `BuiltinFn(a) == BuiltinFn(b)` iff `a == b` (by name)
//!   - `VariantConstructor(na, aa) == VariantConstructor(nb, ab)`
//!     iff `na == nb && aa == ab`
//!
//! Cross-kind pairs still fall through to `_ => false` as before.
//!
//! The end-to-end `h == h` check is the golden case
//! `tests/golden/lang/tasks/value_partial_eq_round26__handle_reflexivity_via_eq_operator`.

use std::sync::Arc;

use silt::bytecode::{Function, VmClosure};
use silt::typeinfo::bv;
use silt::value::{TaskHandle, Value};

// ── Rust-level unit tests ──────────────────────────────────────────

// ── Handle ────────────────────────────────────────────────────────

/// Same handle id → equal. The primary reflexivity lock.
#[test]
fn partial_eq_handle_same_id() {
    let h = Value::Handle(Arc::new(TaskHandle::new(42)));
    // Reflexive on the Value itself (identical Value clone shares the Arc).
    assert_eq!(h, h.clone());
    // Two distinct Arcs with the same id also compare equal — the fix
    // is id-based, not Arc-ptr-based, matching `impl Ord`.
    let h1 = Value::Handle(Arc::new(TaskHandle::new(100)));
    let h2 = Value::Handle(Arc::new(TaskHandle::new(100)));
    assert_eq!(h1, h2);
}

/// Different handle ids → not equal.
#[test]
fn partial_eq_handle_different_ids() {
    let h1 = Value::Handle(Arc::new(TaskHandle::new(1)));
    let h2 = Value::Handle(Arc::new(TaskHandle::new(2)));
    assert_ne!(h1, h2);
}

// ── VmClosure ─────────────────────────────────────────────────────

fn mk_closure(name: &str) -> Arc<VmClosure> {
    Arc::new(VmClosure {
        function: Arc::new(Function::new(name.to_string(), 0)),
        upvalues: Vec::new(),
    })
}

/// Same Arc → equal (ptr_eq).
#[test]
fn partial_eq_vmclosure_same_arc() {
    let arc = mk_closure("f");
    let a = Value::VmClosure(arc.clone());
    let b = Value::VmClosure(arc);
    assert_eq!(a, b);
    // Reflexivity.
    assert_eq!(a, a.clone());
}

/// Different Arc allocations → not equal (even same name), matching
/// `impl Ord`'s identity-based semantics.
#[test]
fn partial_eq_vmclosure_distinct_arcs_not_equal() {
    let a = Value::VmClosure(mk_closure("f"));
    let b = Value::VmClosure(mk_closure("f"));
    assert_ne!(a, b);
    let c = Value::VmClosure(mk_closure("g"));
    assert_ne!(a, c);
}

// ── BuiltinFn ─────────────────────────────────────────────────────

/// Same name → equal.
#[test]
fn partial_eq_builtin_fn_same_name() {
    let a = Value::BuiltinFn("println".into());
    let b = Value::BuiltinFn("println".into());
    assert_eq!(a, b);
    assert_eq!(a, a.clone());
}

/// Different names → not equal.
#[test]
fn partial_eq_builtin_fn_different_names() {
    let a = Value::BuiltinFn("println".into());
    let b = Value::BuiltinFn("print".into());
    assert_ne!(a, b);
}

// ── VariantConstructor ────────────────────────────────────────────

/// Same name + arity → equal.
#[test]
fn partial_eq_variant_constructor_same_name_and_arity() {
    let a = Value::VariantConstructor(bv::SOME.tag());
    let b = Value::VariantConstructor(bv::SOME.tag());
    assert_eq!(a, b);
    assert_eq!(a, a.clone());
}

/// Another variant, or a variant of one name in another enum → not
/// equal.
#[test]
fn partial_eq_variant_constructor_differences() {
    let some_1 = Value::VariantConstructor(bv::SOME.tag());
    let none_0 = Value::VariantConstructor(bv::NONE.tag());
    let other = silt::typeinfo::TypeInfo::new_enum(
        silt::defs::TypeId(silt::defs::DefId(9000)),
        "Maybe",
        &[("Some", 2)],
    );
    let some_2 = Value::VariantConstructor(silt::typeinfo::Tag::new(other, 0));
    assert_ne!(some_1, none_0);
    assert_ne!(some_1, some_2);
    assert_ne!(none_0, some_2);
}

// ── Reflexivity: every variant `v == v` ───────────────────────────
//
// The core Eq invariant. Before the fix, `Handle`, `VmClosure`,
// `BuiltinFn`, and `VariantConstructor` would violate this.

#[test]
fn partial_eq_reflexivity_every_variant() {
    let handle_arc = Arc::new(TaskHandle::new(7));
    let closure_arc = mk_closure("id");
    let values: Vec<Value> = vec![
        Value::Unit,
        Value::Bool(true),
        Value::Int(42),
        Value::Float(1.5),
        Value::String("hi".into()),
        Value::List(Arc::new(vec![Value::Int(1), Value::Int(2)])),
        Value::Range(1, 5),
        Value::Tuple(vec![Value::Int(1), Value::String("x".into())]),
        Value::variant(bv::OK, vec![Value::Int(1)]),
        Value::VariantConstructor(bv::SOME.tag()),
        Value::BuiltinFn("println".into()),
        Value::VmClosure(closure_arc),
        Value::Handle(handle_arc),
        Value::TypeDescriptor(silt::typeinfo::TypeInfo::new_record(
            silt::defs::TypeId(silt::defs::DefId(9001)),
            "Point",
            Vec::new(),
        )),
        Value::PrimitiveDescriptor("Int".into()),
    ];
    for v in &values {
        assert_eq!(
            v,
            &v.clone(),
            "reflexivity violation: `{v:?}` is not equal to itself"
        );
    }
}

// ── Cross-kind comparisons still false ────────────────────────────

/// The new explicit arms must not accidentally cross-match. Example:
/// `Handle` vs `VmClosure` must still be `false` via the catch-all.
#[test]
fn partial_eq_cross_kind_still_false() {
    let h = Value::Handle(Arc::new(TaskHandle::new(1)));
    let c = Value::VmClosure(mk_closure("f"));
    let b = Value::BuiltinFn("println".into());
    let vc = Value::VariantConstructor(bv::SOME.tag());

    assert_ne!(h, c);
    assert_ne!(h, b);
    assert_ne!(h, vc);
    assert_ne!(c, b);
    assert_ne!(c, vc);
    assert_ne!(b, vc);
}
