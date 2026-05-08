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
//!
//! Round 75 lock tightening: the structural source-grep tests
//! (`all_occurs_check_sites_route_through_helper`,
//! `terse_pre_fix_wording_is_eliminated`,
//! `helper_body_produces_canonical_suffix_form`) only constrain the
//! source text. They do NOT exercise the helper. If the helper's
//! signature changes, source-grep keeps passing; the test does not
//! break. We add direct behavioural calls below
//! (`infinite_type_message_helper_renders_canonical_form_for_all_arms`)
//! that invoke `TypeChecker::infinite_type_message` with synthetic
//! `Type` values shaped like every leftover/leftover-row produced by
//! the 5 row-unif arms (and the main `Var(v) ↔ t` arm). Any future
//! signature change must update these tests.

use silt::intern::intern;
use silt::typechecker::{Type, TypeChecker};
use silt::types::RowTail;
use std::collections::BTreeMap;

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
/// `Var(v) ↔ t` arm also routes through the helper. Round 75 DEAD-5
/// extracted `unify_open_closed` to dedupe the byte-symmetric Open×Closed
/// and Closed×Open arms; both call the shared helper which contains one
/// occurs-check / `Self::infinite_type_message(...)` site. Combined floor
/// is therefore 5 (the dedupe consolidated 2 call sites into 1).
#[test]
fn all_occurs_check_sites_route_through_helper() {
    let occurs_sites = TYPECHECKER_MOD_RS.matches("if !occurs_in(").count();
    assert!(
        occurs_sites >= 4,
        "expected at least 4 `if !occurs_in(...)` sites in row unification \
         (post round 75 DEAD-5 dedupe); found {occurs_sites}"
    );
    let helper_calls = TYPECHECKER_MOD_RS
        .matches("Self::infinite_type_message(")
        .count();
    assert!(
        helper_calls >= 5,
        "expected at least 5 `Self::infinite_type_message(...)` call sites \
         (4 row-unif arms incl. shared `unify_open_closed` + 1 main `Var(v) ↔ t` arm); \
         found {helper_calls}. A future change must keep every occurs-check site \
         routed through the helper to preserve canonical wording."
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

// ── Behavioural: invoke the helper directly ────────────────────────
//
// The structural locks above only constrain source text. They do NOT
// exercise the helper. If the helper signature/return-shape changes,
// the source-grep tests stay green; the test does not break. The
// tests below construct synthetic `Type` values shaped like every
// leftover/leftover-row produced by the 5 row-unif arms (and the
// main `Var(v) ↔ t` arm) and pin both the canonical form and the
// helper signature `(&Type) -> String`.

/// Round 75 lock: the helper must accept `&Type` and return `String`,
/// and its body must produce the canonical-suffix form for every
/// `Type` shape the row-unif arms feed it. Five leftover shapes:
///
/// 1. `unify_anon_anon`, `(Var, Closed)` arm — leftover is an
///    `AnonRecord { fields: only_in_2, tail: Closed }`.
/// 2. `unify_anon_anon`, `(Closed, Var)` arm — leftover is an
///    `AnonRecord { fields: only_in_1, tail: Closed }`.
/// 3. `unify_anon_anon`, `(Var, Var)` arm — `to_v1` is an
///    `AnonRecord { fields: only_in_2, tail: Var(new) }`.
/// 4. Same arm — `to_v2` is an `AnonRecord { fields: only_in_1, tail: Var(new) }`.
/// 5. `unify_anon_nominal`, `RowTail::Var(v)` arm — leftover is an
///    `AnonRecord { fields: only_in_nom, tail: Closed }`.
/// 6. Main `Var(v) ↔ t` arm — `t` is any non-record, e.g. `List(Var(v))`.
///
/// Each call must yield the canonical wording with the offending
/// type's `Display` rendering embedded. We assert the prefix (so the
/// suffix-bearing form is locked) AND that the rendered offending
/// type appears in the message — otherwise a regression that
/// returned a constant `"infinite type"` would pass the prefix check.
#[test]
fn infinite_type_message_helper_renders_canonical_form_for_all_arms() {
    let canonical_prefix = "infinite type: the type variable appears inside ";

    // Arm 1 / 2 / 5: leftover is a closed AnonRecord.
    let mut fields1 = BTreeMap::new();
    fields1.insert(intern("name"), Type::String);
    let leftover_closed = Type::AnonRecord {
        fields: fields1.clone(),
        tail: RowTail::Closed,
    };
    let msg1 = TypeChecker::infinite_type_message(&leftover_closed);
    assert!(
        msg1.starts_with(canonical_prefix),
        "arm 1/2/5 (closed AnonRecord leftover): expected canonical prefix {canonical_prefix:?}; \
         got: {msg1:?}"
    );
    assert!(
        msg1.contains("name"),
        "arm 1/2/5: rendered leftover must include the field name `name`; got: {msg1:?}"
    );

    // Arm 3 / 4: leftover is an open AnonRecord with a fresh row tail.
    let mut fields2 = BTreeMap::new();
    fields2.insert(intern("age"), Type::Int);
    let leftover_open = Type::AnonRecord {
        fields: fields2,
        tail: RowTail::Var(99),
    };
    let msg2 = TypeChecker::infinite_type_message(&leftover_open);
    assert!(
        msg2.starts_with(canonical_prefix),
        "arm 3/4 (open AnonRecord leftover): expected canonical prefix {canonical_prefix:?}; \
         got: {msg2:?}"
    );
    assert!(
        msg2.contains("age"),
        "arm 3/4: rendered leftover must include the field name `age`; got: {msg2:?}"
    );

    // Main `Var(v) ↔ t` arm: e.g. `List(Var(v))`.
    let main_arm_t = Type::List(Box::new(Type::Var(7)));
    let msg_main = TypeChecker::infinite_type_message(&main_arm_t);
    assert!(
        msg_main.starts_with(canonical_prefix),
        "main `Var(v) ↔ t` arm: expected canonical prefix {canonical_prefix:?}; \
         got: {msg_main:?}"
    );
    // List renders as `List(_)` — the Display impl elides type-var ids
    // (line ~110). Just assert the leftover is rendered (not empty).
    assert!(
        msg_main.len() > canonical_prefix.len(),
        "main arm: rendered leftover must be non-empty; got: {msg_main:?}"
    );

    // Function-type leftover: covers a different Display arm.
    let fn_t = Type::Fun(vec![Type::Var(0)], Box::new(Type::Var(0)));
    let msg_fn = TypeChecker::infinite_type_message(&fn_t);
    assert!(
        msg_fn.starts_with(canonical_prefix),
        "function-type leftover: expected canonical prefix; got: {msg_fn:?}"
    );
    assert!(
        msg_fn.contains("Fn("),
        "function-type leftover must render through the `Fn(...)` Display arm; got: {msg_fn:?}"
    );
}

/// Round 75 lock: the helper signature `(&Type) -> String` is part of
/// the contract. Any rename or argument-count change should break
/// these tests at compile time (which is the whole point — the
/// pre-fix structural-only test would have stayed green).
#[test]
fn infinite_type_message_helper_signature_is_pinned() {
    // Take a function pointer with the exact signature we expect. If
    // someone changes `infinite_type_message` to e.g. `(t: Type) -> String`
    // (move not borrow) or `(t: &Type, span: Span) -> TypeError`, this
    // assignment fails to compile.
    let f: fn(&Type) -> String = TypeChecker::infinite_type_message;
    let s = f(&Type::Int);
    assert_eq!(
        s, "infinite type: the type variable appears inside Int",
        "helper called via fn-pointer must produce the canonical form"
    );
}
