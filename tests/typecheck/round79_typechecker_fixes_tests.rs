//! Round 79 typechecker-side audit fix TS-L3 (LATENT):
//! `align_tyvars_into` AnonRecord arm dropped mappings.
//!
//! The previous arm used `of.iter().zip(nf.iter())` and only aligned
//! when zipped pair's keys agreed. Mismatched key sets (e.g.
//! pass-3-narrowed scheme has additional fields) silently dropped
//! mappings for keys present on both sides at mismatched positions.
//! Round 79 walks the new field map by key; for each key present in
//! both, recurse into the type pair. Tail-var alignment unchanged.
//!
//! The round's behavioural locks (TS-B1 anon-record trait args, TS-L1
//! alias cycles) are golden cases named `round79_typechecker_fixes__*`
//! under `tests/golden/typecheck/`.

use std::collections::HashMap;

use silt::types::{TyVar, Type};

// ── TS-L3: align_tyvars maps common keys regardless of position ─────

/// Round 79 TS-L3 fix (structural unit test): `align_tyvars` must
/// align by key, not by zipped position. Pre-fix two anon records
/// `{x: α, y: β}` (old) and `{y: γ, z: δ}` (new) zip-aligned on the
/// first slot only when keys happened to match — here `x` vs `y`
/// would skip silently (pre-fix `if on == nn` guard) and `y` vs `z`
/// would also skip, so the shared key `y` (β ↦ γ) was lost.
///
/// `align_tyvars` is `pub fn` (no friend module needed): we exercise
/// it directly with hand-rolled types. Behavioural form would
/// require constructing a where-clause-driven body whose pass-3
/// narrowing produces differing key sets — which the unifier's
/// existing invariants prevent — so the structural form is the
/// load-bearing lock.
#[test]
fn align_tyvars_anon_record_walks_by_key_not_position() {
    use std::collections::BTreeMap;

    use silt::intern::intern;
    use silt::typechecker::align_tyvars;
    use silt::types::RowTail;

    let key_x = intern("x");
    let key_y = intern("y");
    let key_z = intern("z");

    let alpha: TyVar = 1;
    let beta: TyVar = 2;
    let gamma: TyVar = 3;
    let delta: TyVar = 4;

    let mut old_fields: BTreeMap<silt::intern::Symbol, Type> = BTreeMap::new();
    old_fields.insert(key_x, Type::Var(alpha));
    old_fields.insert(key_y, Type::Var(beta));

    let mut new_fields: BTreeMap<silt::intern::Symbol, Type> = BTreeMap::new();
    new_fields.insert(key_y, Type::Var(gamma));
    new_fields.insert(key_z, Type::Var(delta));

    let old = Type::AnonRecord {
        fields: old_fields,
        tail: RowTail::Closed,
    };
    let new = Type::AnonRecord {
        fields: new_fields,
        tail: RowTail::Closed,
    };

    let map: HashMap<TyVar, TyVar> = align_tyvars(&old, &new);
    // β ↦ γ MUST be present — `y` is the shared key. Pre-fix this
    // would be missing because zip put `x`-vs-`y` in slot 0 and
    // `y`-vs-`z` in slot 1, both of which the `on == nn` guard
    // rejected.
    assert_eq!(
        map.get(&beta).copied(),
        Some(gamma),
        "round 79 TS-L3 regression: shared key 'y' should produce \
         β ↦ γ in align_tyvars output regardless of position; map: \
         {map:?}"
    );
    // α has no match (key `x` only on the old side) — must NOT
    // appear in the map.
    assert!(
        !map.contains_key(&alpha),
        "round 79 TS-L3: α is unique to the old side; align_tyvars \
         should leave it unmapped; map: {map:?}"
    );
}

/// Companion test: when key sets agree but positions differ, the
/// post-fix walk still aligns every shared key. Pre-fix `BTreeMap`
/// happens to iterate in key order, so positions agree and zip
/// works — but a future change to ordering could regress without
/// the by-key walk.
#[test]
fn align_tyvars_anon_record_full_overlap_aligns_all() {
    use std::collections::BTreeMap;

    use silt::intern::intern;
    use silt::typechecker::align_tyvars;
    use silt::types::RowTail;

    let key_a = intern("a");
    let key_b = intern("b");

    let alpha: TyVar = 11;
    let beta: TyVar = 12;
    let gamma: TyVar = 13;
    let delta: TyVar = 14;

    let mut old_fields: BTreeMap<silt::intern::Symbol, Type> = BTreeMap::new();
    old_fields.insert(key_a, Type::Var(alpha));
    old_fields.insert(key_b, Type::Var(beta));

    let mut new_fields: BTreeMap<silt::intern::Symbol, Type> = BTreeMap::new();
    new_fields.insert(key_a, Type::Var(gamma));
    new_fields.insert(key_b, Type::Var(delta));

    let old = Type::AnonRecord {
        fields: old_fields,
        tail: RowTail::Closed,
    };
    let new = Type::AnonRecord {
        fields: new_fields,
        tail: RowTail::Closed,
    };

    let map: HashMap<TyVar, TyVar> = align_tyvars(&old, &new);
    assert_eq!(map.get(&alpha).copied(), Some(gamma));
    assert_eq!(map.get(&beta).copied(), Some(delta));
}
