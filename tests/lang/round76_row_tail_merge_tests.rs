//! Round 76 LATENT T3 lock: row-tail merge must follow the same
//! "existing wins" policy in both `apply` (typechecker/mod.rs) and
//! `substitute_vars` (types/mod.rs).
//!
//! Pre-fix:
//!   - `apply` used `new_fields.entry(n).or_insert(t)` (existing
//!     wins).
//!   - `substitute_vars` used `merged.insert(*n, ...)`
//!     (substitution wins).
//!
//! No production caller exercised overlap (the unifier pairwise-
//! unifies overlapping fields before binding the tail), but the
//! divergence was fragile: any new caller that fed an overlapping
//! row through `substitute_vars` would silently drift from `apply`.
//!
//! Post-fix: both sites use `or_insert` (existing wins). This
//! matches the unifier's invariant that overlapping fields have
//! already been pairwise-unified, so the existing field is the
//! canonical one.

use std::collections::BTreeMap;

use silt::intern::intern;
use silt::types::{RowTail, Type, substitute_vars};

#[test]
fn row_tail_substitute_keeps_existing_field_when_substituted_record_overlaps() {
    // Setup: a row var `tail_v` that we will substitute with an
    // AnonRecord claiming `{ a: String, b: Bool }`. The base record
    // already declares `{ a: Int }`. Existing-wins policy: result
    // must keep `a: Int` (the existing) and add `b: Bool` (the new).
    let tail_v: silt::types::TyVar = 4242;
    let a_sym = intern("a");
    let b_sym = intern("b");

    // Existing: { a: Int | tail_v }
    let mut existing_fields: BTreeMap<silt::intern::Symbol, Type> = BTreeMap::new();
    existing_fields.insert(a_sym, Type::Int);
    let existing = Type::AnonRecord {
        fields: existing_fields,
        tail: RowTail::Var(tail_v),
    };

    // Substituted: tail_v -> { a: String, b: Bool | Closed }
    let mut sub_fields: BTreeMap<silt::intern::Symbol, Type> = BTreeMap::new();
    sub_fields.insert(a_sym, Type::String);
    sub_fields.insert(b_sym, Type::Bool);
    let sub_record = Type::AnonRecord {
        fields: sub_fields,
        tail: RowTail::Closed,
    };

    let mut mapping: std::collections::HashMap<silt::types::TyVar, Type> =
        std::collections::HashMap::new();
    mapping.insert(tail_v, sub_record);

    let result = substitute_vars(&existing, &mapping);
    let Type::AnonRecord { fields, tail } = result else {
        panic!("expected AnonRecord, got {result:?}");
    };
    // Existing-wins: `a` must still be Int.
    assert_eq!(
        fields.get(&a_sym),
        Some(&Type::Int),
        "row-tail merge: existing field 'a' must win (Int), got {:?}",
        fields.get(&a_sym)
    );
    // The new `b` field must be present.
    assert_eq!(
        fields.get(&b_sym),
        Some(&Type::Bool),
        "row-tail merge: new field 'b' must be added (Bool), got {:?}",
        fields.get(&b_sym)
    );
    // Substituted record's tail (Closed) must propagate.
    assert!(
        matches!(tail, RowTail::Closed),
        "row-tail merge: substituted Closed tail must propagate, got {:?}",
        tail
    );
}

#[test]
fn row_tail_substitute_no_overlap_merges_all_fields() {
    // Sanity check: no overlap means both sets of fields appear.
    let tail_v: silt::types::TyVar = 4243;
    let x_sym = intern("x");
    let y_sym = intern("y");

    let mut existing_fields: BTreeMap<silt::intern::Symbol, Type> = BTreeMap::new();
    existing_fields.insert(x_sym, Type::Int);
    let existing = Type::AnonRecord {
        fields: existing_fields,
        tail: RowTail::Var(tail_v),
    };

    let mut sub_fields: BTreeMap<silt::intern::Symbol, Type> = BTreeMap::new();
    sub_fields.insert(y_sym, Type::String);
    let sub_record = Type::AnonRecord {
        fields: sub_fields,
        tail: RowTail::Closed,
    };

    let mut mapping: std::collections::HashMap<silt::types::TyVar, Type> =
        std::collections::HashMap::new();
    mapping.insert(tail_v, sub_record);

    let result = substitute_vars(&existing, &mapping);
    let Type::AnonRecord { fields, .. } = result else {
        panic!("expected AnonRecord");
    };
    assert_eq!(fields.get(&x_sym), Some(&Type::Int));
    assert_eq!(fields.get(&y_sym), Some(&Type::String));
}
