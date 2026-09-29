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
//! lives in `tests/lang/round73f_deferred_fixes_tests.rs` (updated to pin
//! the suffix).
//!
//! The user-visible wording of the main occurs-check arm is locked by
//! the golden case
//! `tests/golden/typecheck/inference/round74_infinite_type_canonical_form__self_application.silt`.
//! This file invokes `TypeChecker::infinite_type_message` directly with
//! synthetic `Type` values shaped like every leftover the row-unif arms
//! and the main `Var(v) ↔ t` arm feed it.

use silt::intern::intern;
use silt::typechecker::{Type, TypeChecker};
use silt::types::RowTail;
use std::collections::BTreeMap;

// ── Behavioural: invoke the helper directly ────────────────────────
//
// The tests below construct synthetic `Type` values shaped like every
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
