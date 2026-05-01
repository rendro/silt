//! Round 67 DEAD-dedup + parity lock — `value::builtin_variant_seed`
//! duplicated `module::builtin_enum_variants` (two parallel ~115-line
//! lists). The `value.rs` comment claimed a "circular dependency"
//! preventing a direct call, but neither file imports the other —
//! `value.rs` does not `use crate::module` and `module.rs` does not
//! `use crate::value`. Round 67 deletes/forwards `builtin_variant_seed`
//! to `crate::module::builtin_enum_variants()`.
//!
//! Two locks:
//!   1. SOURCE-GREP — assert `value.rs` does not redefine the parallel
//!      list. Either the function is gone, or its body is a thin
//!      forward.
//!   2. PARITY — assert that the variant-name set actually used to
//!      seed the ordinal registry equals the flattened
//!      `module::builtin_enum_variants()` set. This is the lock that
//!      survives any future re-introduction of a hand-rolled list:
//!      whatever feeds the registry must agree with the module-side
//!      authoritative source.

use silt::module::builtin_enum_variants;
use silt::value::lookup_variant_ordinal;

const VALUE_RS: &str = include_str!("../src/value.rs");

// ── Source-grep lock ────────────────────────────────────────────────

/// `value.rs` must NOT re-declare any of the typed-error variants as
/// `&[(&str, &[&str])]` literal entries. The pre-round-67 form had a
/// `("Result", &["Ok", "Err"])` tuple literal sitting inside a
/// hand-rolled `builtin_variant_seed` body; lock against any such
/// pattern reappearing.
#[test]
fn value_rs_has_no_parallel_variant_seed_literal() {
    // The pre-round-67 form had `("Result", &["Ok", "Err"]),` as the
    // first entry of the hand-rolled list. If value.rs ever has both
    // a `"Result"` and a `"Ok"` string-literal in the same line range
    // (i.e. a hand-rolled enum-variant tuple), reject.
    assert!(
        !VALUE_RS.contains("(\"Result\", &[\"Ok\", \"Err\"])"),
        "src/value.rs contains a hand-rolled `(\"Result\", &[\"Ok\", \
         \"Err\"])` enum-variant tuple — round 67 removed the parallel \
         seed list and forwards to `module::builtin_enum_variants()`. \
         Update the seed call site instead of re-introducing a \
         hand-rolled list."
    );
    assert!(
        !VALUE_RS.contains("(\"Option\", &[\"Some\", \"None\"])"),
        "src/value.rs contains a hand-rolled `(\"Option\", &[\"Some\", \
         \"None\"])` enum-variant tuple — round 67 removed the parallel \
         seed list and forwards to `module::builtin_enum_variants()`."
    );
    assert!(
        !VALUE_RS.contains("\"IoNotFound\""),
        "src/value.rs mentions `\"IoNotFound\"` — that variant lives \
         in `module::builtin_enum_variants()` only. Round 67 removed \
         the parallel seed."
    );
}

// ── Parity lock ─────────────────────────────────────────────────────

/// Every variant declared in `module::builtin_enum_variants` must be
/// registered in the value-side ordinal registry (which is seeded on
/// first access — `lookup_variant_ordinal` triggers seeding).
///
/// This is the load-bearing lock: a future contributor who
/// reintroduces a parallel list in `value.rs` and forgets to add a
/// freshly-added module variant will fail this test, even if the
/// source-grep lock passes (e.g. they restructure the literal).
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
