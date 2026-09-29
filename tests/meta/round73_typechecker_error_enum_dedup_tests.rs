//! Round 73 L4 (LATENT, dead-code dedup): the stdlib error-enum names
//! used by `register_builtin_trait_impls` in `src/typechecker/mod.rs`
//! were hand-rolled in a parallel array next to the authoritative
//! `module::builtin_error_enum_variants_with_arity()` registry. The
//! duplicate forced a parallel-array edit each time a typed-error enum
//! was added or renamed — exactly the drift class round 64 collapsed
//! for the dispatch-side mirror.
//!
//! These tests lock the registry's contents.

use silt::module::builtin_error_enum_variants_with_arity;

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
