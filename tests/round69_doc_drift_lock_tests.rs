//! Round-69 lock tests for documentation drift.
//!
//! Each test pins one stale claim that an earlier round had to chase
//! after a feature shipped or a release rolled. They are source-grep
//! checks against the bundled markdown so they fail loudly the next
//! time someone re-introduces the drifted prose.
//!
//! - **F1** `operators.md` no longer claims `pair.0` postfix tuple
//!   indexing exists — the typechecker rejects it.
//! - **F2** `traits.md` no longer claims silt has "no associated
//!   types"; the feature shipped (commit ceb11c9) and is documented in
//!   `generics.md`.
//! - **F3** v0.13 prose in the strict-effects guide and effect-rows
//!   proposal is updated to v0.14 (the released version).
//! - **F4** `why-silt.md` no longer tells OCaml/ReScript readers silt
//!   has only nominal records — anonymous structural records and row
//!   polymorphism shipped in round 68.

const OPERATORS_MD: &str = include_str!("../docs/language/operators.md");
const TRAITS_MD: &str = include_str!("../docs/language/traits.md");
const WHY_SILT_MD: &str = include_str!("../docs/why-silt.md");
const STRICT_EFFECTS_MIGRATION_MD: &str = include_str!("../docs/strict-effects-migration.md");
const EFFECT_ROWS_MD: &str = include_str!("../docs/proposals/effect-rows.md");

// ── F1 ──────────────────────────────────────────────────────────────

#[test]
fn operators_md_does_not_advertise_tuple_index() {
    assert!(
        !OPERATORS_MD.contains("pair.0"),
        "operators.md must not advertise `pair.0` postfix tuple \
         indexing — the typechecker rejects it with `unknown method \
         '0' on Tuple`. Use a `match` destructure instead."
    );
    assert!(
        !OPERATORS_MD.contains("Tuple index:"),
        "operators.md must not have a `Tuple index:` bullet — postfix \
         tuple indexing is not a real silt operator."
    );
}

// ── F2 ──────────────────────────────────────────────────────────────

#[test]
fn traits_md_does_not_claim_no_associated_types() {
    assert!(
        !TRAITS_MD.contains("no associated"),
        "traits.md must not say `no associated types` — the feature \
         shipped (commit ceb11c9) and is documented in generics.md."
    );
    assert!(
        TRAITS_MD.contains("associated type"),
        "traits.md should reference `associated type`(s) — they exist \
         and are first-class. Link to generics.md."
    );
}

// ── F3 ──────────────────────────────────────────────────────────────

#[test]
fn version_prose_does_not_reference_v013() {
    // strict-effects-migration.md
    assert!(
        !STRICT_EFFECTS_MIGRATION_MD.contains("through v0.13 without the flag"),
        "strict-effects-migration.md must not say `through v0.13 \
         without the flag` — the codebase is at v0.14.0."
    );
    assert!(
        !STRICT_EFFECTS_MIGRATION_MD.contains("stays with you in v0.13"),
        "strict-effects-migration.md must not say `stays with you in \
         v0.13` — the codebase is at v0.14.0."
    );

    // effect-rows.md
    assert!(
        !EFFECT_ROWS_MD.contains("permissive as of v0.13"),
        "effect-rows.md must not say `permissive as of v0.13` — the \
         codebase is at v0.14.0."
    );
    assert!(
        !EFFECT_ROWS_MD.contains("permissive in v0.13"),
        "effect-rows.md must not say `permissive in v0.13` — the \
         codebase is at v0.14.0."
    );
}

// ── F4 ──────────────────────────────────────────────────────────────

#[test]
fn why_silt_acknowledges_anonymous_records() {
    assert!(
        !WHY_SILT_MD.contains("not structural"),
        "why-silt.md must not claim records are `not structural` — \
         silt has anonymous structural records and row polymorphism \
         (round 68, commit aafeaeb)."
    );
    assert!(
        WHY_SILT_MD.contains("anonymous") || WHY_SILT_MD.contains("structural"),
        "why-silt.md should mention `anonymous` or `structural` \
         records to acknowledge round-68 features."
    );
}
