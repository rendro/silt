//! Round-73 audit lock: `Value::TypeDescriptor` / `Value::PrimitiveDescriptor`
//! arm deduplication.
//!
//! Background: the two descriptor variants render to byte-identical
//! `<type:NAME>` strings in `Debug`, `Display`, and `format_silt`, and
//! hash by name only in `impl Hash for Value`. Round-73 collapsed each
//! pair of adjacent arms into one merged or-pattern arm
//! (`Value::TypeDescriptor(name) | Value::PrimitiveDescriptor(name) => …`).
//!
//! Hashing safety note: the merged Hash arm is sound because the outer
//! `impl Hash` body prefixes every value's hash with
//! `std::mem::discriminant(self).hash(state)` (see `src/value.rs`), so
//! the two variants still produce different total hashes despite sharing
//! the body — preserving the prior behaviour exactly.
//!
//! This file pins three things:
//!
//!   1. Display parity — `Display`, `Debug`, and `format_silt` of a
//!      `TypeDescriptor("Foo")` and a `PrimitiveDescriptor("Foo")` must
//!      render the same `<type:Foo>` string (already true today;
//!      refactor is identity-preserving).
//!
//!   2. Identity preserved — the two variants compare distinct via
//!      `PartialEq` (different kinds), even though they share a display.
//!      A `TypeDescriptor("Foo")` is not equal to a
//!      `PrimitiveDescriptor("Foo")`.
//!
//!   3. Hash distinctness — because `Hash` on `Value` hashes the
//!      discriminant first, the two variants must hash differently for
//!      the same payload. A regression that drops the discriminant
//!      prefix from the Hash impl would silently collide them.
//!
//!   4. Source-grep lock — the merged-arm syntax
//!      `Value::TypeDescriptor(name) | Value::PrimitiveDescriptor(name)`
//!      appears at the four expected sites (Debug, Display, format_silt,
//!      Hash) in `src/value.rs`. If a future edit re-splits any of them
//!      back into two adjacent arms, the lock fails.

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};

use silt::value::Value;

fn hash_of(v: &Value) -> u64 {
    let mut h = DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

#[test]
fn display_parity_type_and_primitive_descriptor() {
    let t = Value::TypeDescriptor("Foo".to_string());
    let p = Value::PrimitiveDescriptor("Foo".to_string());
    assert_eq!(format!("{t}"), format!("{p}"));
    assert_eq!(format!("{t}"), "<type:Foo>");
}

#[test]
fn debug_parity_type_and_primitive_descriptor() {
    let t = Value::TypeDescriptor("Foo".to_string());
    let p = Value::PrimitiveDescriptor("Foo".to_string());
    assert_eq!(format!("{t:?}"), format!("{p:?}"));
    assert_eq!(format!("{t:?}"), "<type:Foo>");
}

#[test]
fn format_silt_parity_type_and_primitive_descriptor() {
    let t = Value::TypeDescriptor("Foo".to_string());
    let p = Value::PrimitiveDescriptor("Foo".to_string());
    assert_eq!(t.format_silt(), p.format_silt());
    assert_eq!(t.format_silt(), "<type:Foo>");
}

#[test]
fn type_and_primitive_descriptors_are_not_equal_by_value_eq() {
    // Identity preserved: even though they display the same, the two
    // kinds are distinct. A regression that merged them in `PartialEq`
    // would break the silt type system's distinction between
    // user-defined and primitive types.
    let t = Value::TypeDescriptor("Foo".to_string());
    let p = Value::PrimitiveDescriptor("Foo".to_string());
    assert_ne!(t, p);
}

#[test]
fn type_and_primitive_descriptors_hash_distinctly_via_discriminant_prefix() {
    // The merged `Hash` arm shares the same `name.hash(state)` body, but
    // the outer `impl Hash for Value` hashes
    // `std::mem::discriminant(self)` first. That prefix keeps the two
    // variants distinct in the hasher output. This test fails if a
    // future refactor drops the discriminant prefix while keeping the
    // merged arm.
    let t = Value::TypeDescriptor("Foo".to_string());
    let p = Value::PrimitiveDescriptor("Foo".to_string());
    assert_ne!(hash_of(&t), hash_of(&p));
}

#[test]
fn same_kind_descriptors_with_same_name_hash_and_compare_equal() {
    // Sanity: two `TypeDescriptor("Foo")` instances must be `==` and
    // hash-equal, and the same for two `PrimitiveDescriptor("Foo")`.
    let t1 = Value::TypeDescriptor("Foo".to_string());
    let t2 = Value::TypeDescriptor("Foo".to_string());
    assert_eq!(t1, t2);
    assert_eq!(hash_of(&t1), hash_of(&t2));

    let p1 = Value::PrimitiveDescriptor("Foo".to_string());
    let p2 = Value::PrimitiveDescriptor("Foo".to_string());
    assert_eq!(p1, p2);
    assert_eq!(hash_of(&p1), hash_of(&p2));
}

#[test]
fn source_grep_lock_merged_arms_present_in_value_rs() {
    // Pin the merged-arm syntax at all four expected sites. If a future
    // edit re-splits any of them back into two adjacent arms, the
    // occurrence count drops below 4 and the lock fails.
    let src = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/value.rs"))
        .expect("read src/value.rs");

    let needle = "Value::TypeDescriptor(name) | Value::PrimitiveDescriptor(name)";
    let count = src.matches(needle).count();
    assert!(
        count >= 4,
        "expected the merged-arm syntax `{needle}` to appear at >=4 sites \
         (Debug, Display, format_silt, Hash) in src/value.rs, found {count}",
    );

    // And the old split-arm form for the descriptor pair must not
    // re-appear adjacent.
    let split = "Value::TypeDescriptor(name) => write!(f, \"<type:{name}>\"),\n            Value::PrimitiveDescriptor(name) => write!(f, \"<type:{name}>\"),";
    assert!(
        !src.contains(split),
        "split-arm form of TypeDescriptor/PrimitiveDescriptor display arms reappeared in src/value.rs",
    );
}
