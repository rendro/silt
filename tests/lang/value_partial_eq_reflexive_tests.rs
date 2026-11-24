//! Regression tests for round-26 finding L6: `Value::PartialEq`
//! returned `false` for reflexive comparisons on `VmClosure`,
//! `BuiltinFn`, and `VariantConstructor` via the catch-all `_ => false`
//! arm. That violated the `Eq` reflexivity contract (`a == a` ≡ `true`)
//! AND desynchronized `PartialEq` from `Ord` (round-23 added explicit
//! arms to `Ord` returning identity-based `Equal`).
//!
//! The fix mirrors the `Ord` arms into `PartialEq`:
//!   - `VmClosure(a) == VmClosure(b)` iff they run one `Function`
//!     and their captures are equal
//!   - `BuiltinFn(a) == BuiltinFn(b)` iff `a == b` (by name)
//!   - `VariantConstructor(na, aa) == VariantConstructor(nb, ab)`
//!     iff `na == nb && aa == ab`
//!
//! Cross-kind pairs still fall through to `_ => false` as before.

use std::sync::Arc;

use silt::testing::ValueInside;
use silt::testing::bytecode::{Function, VmClosure};
use silt::testing::typeinfo::bv;
use silt::testing::value::Value;

/// The builtin function `name` (`println`, `list.map`) as a value.
fn builtin(name: &str) -> Value {
    silt::testing::builtins::registry::registry()
        .named(name)
        .unwrap_or_else(|| panic!("the builtin {name}"))
        .value()
}

// ── Rust-level unit tests ──────────────────────────────────────────

// ── VmClosure ─────────────────────────────────────────────────────

fn mk_closure(name: &str) -> Arc<VmClosure> {
    Arc::new(VmClosure::new(
        Arc::new(Function::returning_unit(name.to_string(), 0)),
        Vec::new(),
    ))
}

/// Same Arc → equal.
#[test]
fn partial_eq_vmclosure_same_arc() {
    let arc = mk_closure("f");
    let a = Value::VmClosure(arc.clone());
    let b = Value::VmClosure(arc);
    assert_eq!(a, b);
    // Reflexivity.
    assert_eq!(a, a.clone());
}

/// Closures of two functions → not equal (even same name), matching
/// `impl Ord`.
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
    let a = builtin("println");
    let b = builtin("println");
    assert_eq!(a, b);
    assert_eq!(a, a.clone());
}

/// Different names → not equal.
#[test]
fn partial_eq_builtin_fn_different_names() {
    let a = builtin("println");
    let b = builtin("print");
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
    let other = silt::testing::typeinfo::TypeInfo::new_enum(
        silt::testing::defs::TypeId(silt::testing::defs::DefId(9000)),
        "Maybe",
        &[("Some", 2)],
    );
    let some_2 = Value::VariantConstructor(silt::testing::typeinfo::Tag::new(other, 0));
    assert_ne!(some_1, none_0);
    assert_ne!(some_1, some_2);
    assert_ne!(none_0, some_2);
}

// ── Reflexivity: every variant `v == v` ───────────────────────────
//
// The core Eq invariant. Before the fix, `VmClosure`,
// `BuiltinFn`, and `VariantConstructor` would violate this.

#[test]
fn partial_eq_reflexivity_every_variant() {
    let closure_arc = mk_closure("id");
    let values: Vec<Value> = vec![
        Value::Unit,
        Value::Bool(true),
        Value::Int(42),
        Value::Float(silt::testing::value::Float::new(1.5).expect("a finite number")),
        Value::string("hi"),
        Value::list(vec![Value::Int(1), Value::Int(2)]),
        Value::List(silt::testing::value::List::ints(1, 5).expect("a list")),
        Value::tuple(vec![Value::Int(1), Value::string("x")]),
        Value::variant(bv::OK, [Value::Int(1)]),
        Value::VariantConstructor(bv::SOME.tag()),
        builtin("println"),
        Value::VmClosure(closure_arc),
        Value::TypeDescriptor(silt::testing::typeinfo::TypeInfo::new_record(
            silt::testing::defs::TypeId(silt::testing::defs::DefId(9001)),
            "Point",
            Vec::new(),
        )),
        Value::PrimitiveDescriptor("Int"),
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
/// `VmClosure` vs `BuiltinFn` must still be `false` via the catch-all.
#[test]
fn partial_eq_cross_kind_still_false() {
    let c = Value::VmClosure(mk_closure("f"));
    let b = builtin("println");
    let vc = Value::VariantConstructor(bv::SOME.tag());

    assert_ne!(c, b);
    assert_ne!(c, vc);
    assert_ne!(b, vc);
}
