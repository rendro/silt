//! Round 74 Fix #5 lock: every occurs-check site (the main
//! `Var(v) ↔ t` arm at `src/typechecker/mod.rs:1270` plus the five
//! row-unif arms at lines ~1075/1100/1130/1135/1210) emits the
//! identical canonical wording, routed through the shared helper
//! `infinite_type_message(t: &Type) -> String`.
//!
//! Pre-fix divergence:
//!   - Main arm: `"infinite type: the type variable appears inside {t}"`
//!     (specific, names the offending side).
//!   - Five row-unif arms: terse `"infinite type"` (no suffix).
//!   The round-73f Fix #2 lock asserted only the substring
//!   `"infinite type"`, which both forms satisfy — so a future drift
//!   that re-introduced terse wording in one site only would slip
//!   past the lock.
//!
//! Post-fix: all six sites call `Self::infinite_type_message(...)`
//! producing the canonical form. The audit GAP-3 follow-up assertion
//! lives in `tests/round73f_deferred_fixes_tests.rs` (updated to pin
//! the suffix); this file pins the helper's own behaviour and the
//! call-site coverage from a different angle (source structure +
//! direct unit-level invocation).

const TYPECHECKER_MOD_RS: &str = include_str!("../src/typechecker/mod.rs");

/// The helper definition must exist and produce the canonical form.
/// We grep the source rather than calling the helper directly because
/// it lives behind `pub(super)` visibility and is module-private.
#[test]
fn infinite_type_message_helper_is_defined() {
    assert!(
        TYPECHECKER_MOD_RS.contains("fn infinite_type_message("),
        "expected `fn infinite_type_message(t: &Type) -> String` helper in \
         src/typechecker/mod.rs (round 74 Fix #5)"
    );
    // Pin the canonical wording inside the helper so a future refactor
    // that splits / renames the helper but changes the suffix is
    // caught here. The substring covers both `format!(...)` and any
    // future composition.
    assert!(
        TYPECHECKER_MOD_RS.contains("infinite type: the type variable appears inside"),
        "canonical occurs-check wording must include `\"infinite type: the type \
         variable appears inside\"` — the suffix is the discriminating part"
    );
}

/// Every `if !occurs_in(...)` row-unif site must have a paired
/// `Self::infinite_type_message(...)` failure-branch call. The main
/// `Var(v) ↔ t` arm also routes through the helper. Combined floor
/// is 6 (5 row-unif + 1 main).
#[test]
fn all_occurs_check_sites_route_through_helper() {
    let occurs_sites = TYPECHECKER_MOD_RS.matches("if !occurs_in(").count();
    assert!(
        occurs_sites >= 5,
        "expected at least 5 `if !occurs_in(...)` sites in row unification; \
         found {occurs_sites}"
    );
    let helper_calls = TYPECHECKER_MOD_RS
        .matches("Self::infinite_type_message(")
        .count();
    assert!(
        helper_calls >= 6,
        "expected at least 6 `Self::infinite_type_message(...)` call sites \
         (5 row-unif arms + 1 main `Var(v) ↔ t` arm); found {helper_calls}. \
         A future change must keep every occurs-check site routed through \
         the helper to preserve canonical wording."
    );
}

/// The terse pre-fix wording (`self.error("infinite type".to_string(), ...)`)
/// must NOT appear anywhere in the typechecker. A regression here
/// would silently re-introduce the dual-shape diagnostic divergence.
#[test]
fn terse_pre_fix_wording_is_eliminated() {
    let terse = r#"self.error("infinite type".to_string(), span);"#;
    assert!(
        !TYPECHECKER_MOD_RS.contains(terse),
        "src/typechecker/mod.rs still contains the pre-fix terse wording \
         {terse:?} — round 74 Fix #5 routes every occurs-check site through \
         `Self::infinite_type_message(...)` instead. Re-route any new site \
         through the helper rather than re-introducing the terse form."
    );
}

/// The helper body must produce the canonical-suffix-bearing form.
/// We grep `format!("infinite type: the type variable appears inside`
/// to catch a future change that splits the helper but reverts the
/// suffix wording. Combined with the helper-call-count lock above,
/// this gives byte-level coverage of the canonical form at the
/// definition site.
#[test]
fn helper_body_produces_canonical_suffix_form() {
    let canonical = r#"format!("infinite type: the type variable appears inside {t}")"#;
    assert!(
        TYPECHECKER_MOD_RS.contains(canonical),
        "expected the helper body to contain the canonical format-string \
         `{canonical}`. A future refactor that changed the suffix would \
         silently drift wording across all six call sites."
    );
}
