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
