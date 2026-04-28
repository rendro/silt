//! Architectural lock for the Phase A effect-rows plumbing.
//!
//! Locks the data-layout decision: `Scheme` must declare an
//! `effects: EffectSet` field. A future refactor that drops the field
//! (or renames it without updating this test) flips the test red so the
//! review catches the regression.
//!
//! See `docs/proposals/effect-rows.md` for the proposal and
//! `src/types/effects.rs` for the EffectSet definition.

#[test]
fn scheme_carries_effect_set_field() {
    let src = include_str!("../src/types/mod.rs");
    assert!(
        src.contains("effects: EffectSet"),
        "Scheme must declare an `effects: EffectSet` field — Phase A plumbing."
    );
}

#[test]
fn effects_module_exists_under_types() {
    // The dedicated `src/types/effects.rs` module is the single source
    // of truth for `Effect` and `EffectSet`. Phase B-D will add to it
    // (parser surface, LSP renderer, --strict-effects checker), so
    // collapsing it back into mod.rs would be a regression.
    let src = include_str!("../src/types/effects.rs");
    assert!(
        src.contains("pub enum Effect"),
        "src/types/effects.rs must define `pub enum Effect`."
    );
    assert!(
        src.contains("pub struct EffectSet"),
        "src/types/effects.rs must define `pub struct EffectSet`."
    );
    assert!(
        src.contains("pub const TOP"),
        "EffectSet must expose a `TOP` constant for the gradual-rollout default."
    );
    assert!(
        src.contains("pub const EMPTY"),
        "EffectSet must expose an `EMPTY` constant for fully pure functions."
    );
}

#[test]
fn stdlib_sweep_classifies_every_builtin() {
    // Phase C of the effect-rows proposal: after the stdlib sweep, no
    // function-typed builtin remains at `EffectSet::TOP`. The bulk
    // negative-check lives in
    // `tests/effect_stdlib_sweep_lock_tests.rs`; this test is the
    // architectural-lock counterpart that fails if a future refactor
    // reintroduces a registration path bypassing the explicit-effects
    // requirement.
    //
    // We require that the constructors `EffectSet::pure / io / io_fs /
    // io_net / io_time / io_random` are all present in the source —
    // their disappearance would force a return to ad-hoc bitset
    // construction at every site, which the sweep was designed to
    // prevent.
    let src = include_str!("../src/types/effects.rs");
    for ctor in &[
        "pub const fn pure()",
        "pub const fn io()",
        "pub const fn io_fs()",
        "pub const fn io_net()",
        "pub const fn io_time()",
        "pub const fn io_random()",
        "pub const fn io_time_random()",
    ] {
        assert!(
            src.contains(ctor),
            "EffectSet must expose `{ctor}` (Phase C convenience constructor)."
        );
    }

    // Likewise the `Scheme` mono-helpers used by the per-builtin
    // registration sites: removing one would force the sweep back into
    // verbose `Scheme::with_effects(_, EffectSet::*)` calls.
    let mod_src = include_str!("../src/types/mod.rs");
    for helper in &[
        "fn with_effects(",
        "fn pure_mono(",
        "fn io_mono(",
        "fn io_fs_mono(",
        "fn io_net_mono(",
        "fn io_time_mono(",
        "fn io_random_mono(",
    ] {
        assert!(
            mod_src.contains(helper),
            "Scheme must expose `{helper}` constructor (Phase C helpers)."
        );
    }
}
