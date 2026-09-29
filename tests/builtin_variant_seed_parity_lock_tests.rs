//! Round 67 DEAD-dedup + parity lock — `value::builtin_variant_seed`
//! duplicated `module::builtin_enum_variants` (two parallel ~115-line
//! lists). The `value.rs` comment claimed a "circular dependency"
//! preventing a direct call, but neither file imports the other —
//! `value.rs` does not `use crate::module` and `module.rs` does not
//! `use crate::value`. Round 67 deletes/forwards `builtin_variant_seed`
//! to `crate::module::builtin_enum_variants()`.
//!
//! This file is a test binary of its own: the ordinal registry is
//! process-global, and in-process tests in a shared suite register user
//! variants with the same names, which changes what this test observes.
//!
//! PARITY lock: the variant-name set actually used to seed the ordinal
//! registry must equal the flattened `module::builtin_enum_variants()`
//! set, so whatever feeds the registry agrees with the module-side
//! authoritative source.

use silt::module::builtin_enum_variants;
use silt::value::lookup_variant_ordinal;

// ── Parity lock ─────────────────────────────────────────────────────

/// Every variant declared in `module::builtin_enum_variants` must be
/// registered in the value-side ordinal registry (which is seeded on
/// first access — `lookup_variant_ordinal` triggers seeding).
#[test]
fn module_variants_match_value_side_ordinal_registry() {
    let mut missing = Vec::new();
    let mut wrong_ordinal = Vec::new();
    for (enum_name, variants) in builtin_enum_variants() {
        for (idx, variant) in variants.iter().enumerate() {
            let expected = idx as u32;
            match lookup_variant_ordinal(variant) {
                None => missing.push(format!("{enum_name}.{variant}")),
                Some(actual) if actual != expected => wrong_ordinal.push(format!(
                    "{enum_name}.{variant}: registry={actual}, module={expected}"
                )),
                Some(_) => {}
            }
        }
    }
    assert!(
        missing.is_empty(),
        "variants in `module::builtin_enum_variants` not registered \
         in the value-side ordinal registry (the seed list has \
         drifted): {missing:?}"
    );
    assert!(
        wrong_ordinal.is_empty(),
        "variants registered with wrong declaration-order ordinal \
         (registry seed disagrees with module list — the two lists \
         have drifted): {wrong_ordinal:?}"
    );
}
