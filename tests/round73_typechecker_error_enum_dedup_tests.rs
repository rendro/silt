//! Round 73 L4 (LATENT, dead-code dedup): the stdlib error-enum names
//! used by `register_builtin_trait_impls` in `src/typechecker/mod.rs`
//! were hand-rolled in a parallel array next to the authoritative
//! `module::builtin_error_enum_variants_with_arity()` registry. The
//! duplicate forced a parallel-array edit each time a typed-error enum
//! was added or renamed — exactly the drift class round 64 collapsed
//! for the dispatch-side mirror.
//!
//! These tests lock the dedup both behaviorally (the registry IS the
//! single source of truth — iterating it produces the same set) and
//! structurally (the hand-rolled list literal is gone from the
//! typechecker source).

use silt::module::builtin_error_enum_variants_with_arity;

const TYPECHECKER_MOD_RS: &str = include_str!("../src/typechecker/mod.rs");

/// Behavioral: the registry must contain every stdlib error enum we
/// expect. If a future change adds or renames a typed-error enum, this
/// list must be kept in sync — but it lives in ONE place
/// (`module::builtin_error_enum_variants_with_arity`), not three.
#[test]
fn registry_lists_all_known_error_enums() {
    let names: Vec<&str> = builtin_error_enum_variants_with_arity()
        .iter()
        .map(|(name, _)| *name)
        .collect();
    let expected = [
        "IoError",
        "JsonError",
        "TomlError",
        "ParseError",
        "HttpError",
        "RegexError",
        "PgError",
        "TcpError",
        "TimeError",
        "BytesError",
        "ChannelError",
    ];
    assert_eq!(
        names.len(),
        expected.len(),
        "registry has {} entries; expected {}: registry={names:?}",
        names.len(),
        expected.len()
    );
    for want in &expected {
        assert!(
            names.contains(want),
            "registry missing expected error enum '{want}'; got: {names:?}"
        );
    }
}

/// Behavioral: the registry returns `(name, variants)` tuples; every
/// entry must have at least one variant (an empty error enum would be
/// nonsensical).
#[test]
fn every_registry_entry_has_at_least_one_variant() {
    for (name, variants) in builtin_error_enum_variants_with_arity() {
        assert!(
            !variants.is_empty(),
            "error enum '{name}' has no variants — registry entry must list at least one"
        );
    }
}

/// Structural: the hand-rolled list of error-enum names that lived
/// inside the `register_auto_derived_impls_for(...)` call in
/// `register_builtin_trait_impls` is gone. We grep for the smoking-gun
/// adjacency `"IoError"` followed by `"JsonError"` on the next quoted
/// item — the unique signature of the deleted parallel array. The
/// typechecker may still mention `IoError` / `JsonError` in comments
/// (e.g. doc references), so we use a regex-like contiguous-string
/// check rather than a bare `contains("IoError")`.
#[test]
fn hand_rolled_error_enum_list_is_gone_from_typechecker() {
    // The deleted pattern was a stretch of `"X",\n            "Y",`
    // entries with `IoError` followed by `JsonError`. This contiguous
    // substring is what the parallel-array edit forced on every
    // additions and is the exact form the dedup eliminates.
    let smoking_gun = "\"IoError\",\n            \"JsonError\"";
    assert!(
        !TYPECHECKER_MOD_RS.contains(smoking_gun),
        "src/typechecker/mod.rs still contains the hand-rolled error-enum list \
         ({smoking_gun:?}). Round 73 L4 dedup expected this to be sourced from \
         module::builtin_error_enum_variants_with_arity() instead."
    );
    // Also assert the hand-rolled tail is gone — `BytesError` sat
    // adjacent to `ChannelError` in the deleted block. Using the same
    // contiguous-substring shape keeps false positives from reasonable
    // comment text (single-line mentions can't form this pattern).
    let smoking_gun_tail = "\"BytesError\",\n            \"ChannelError\"";
    assert!(
        !TYPECHECKER_MOD_RS.contains(smoking_gun_tail),
        "src/typechecker/mod.rs still contains the hand-rolled error-enum list tail \
         ({smoking_gun_tail:?})."
    );
}

/// Sanity: the typechecker source must REFERENCE the registry function
/// — that's how the dedup works (the call site sources its names
/// dynamically from the registry).
#[test]
fn typechecker_references_the_registry() {
    assert!(
        TYPECHECKER_MOD_RS.contains("builtin_error_enum_variants_with_arity"),
        "src/typechecker/mod.rs no longer references the canonical registry — \
         dedup must source error enum names from \
         module::builtin_error_enum_variants_with_arity"
    );
}
