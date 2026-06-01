//! Round 91 GAP regression: `silt test` must apply the SAME user-import
//! diagnostic filter as `silt run`/`silt check`/LSP.
//!
//! The `silt run`/`silt check` path (`reportable_type_errors`) filters out
//! BOTH the "unknown module" warning AND the follow-on undefined-name /
//! trait cascade that the compiler resolves at link time. The `silt test`
//! loop previously filtered ONLY the warning, leaking the cascade — so a
//! test file importing a sibling user module (with partial module-resolution
//! state) printed the cascade and failed with "failed to compile — type
//! errors" where `silt run` succeeded.
//!
//! A fully deterministic end-to-end repro needs partial module-resolution
//! state that is hard to construct, so this locks at the SHARED-PREDICATE
//! layer: `silt::diagnostic_filters::should_suppress_import_cascade_message`
//! is the single function both CLI paths route through (via the
//! `is_unknown_module_warning` / `is_user_import_resolvable_error`
//! SourceError-shaped helpers that delegate to the same message predicates).
//!
//! This test FAILS before the fix because before the fix the `silt test`
//! loop only suppressed `is_unknown_module_warning` and there was no shared
//! `should_suppress_import_cascade_message` predicate routing the cascade
//! suppression into the test path. After the fix, an undefined-name cascade
//! error IS suppressed when the unknown-module warning is present.

use silt::diagnostic_filters::should_suppress_import_cascade_message;

/// The "unknown module" warning itself is always suppressed (this is the
/// part `silt test` already handled before the fix).
#[test]
fn unknown_module_warning_is_always_suppressed() {
    // is_warning = true, regardless of has_user_import_warning.
    assert!(should_suppress_import_cascade_message(
        "unknown module `foo`",
        true,
        false
    ));
    assert!(should_suppress_import_cascade_message(
        "unknown module `foo`",
        true,
        true
    ));
}

/// The KEY regression: the follow-on undefined-name cascade MUST be
/// suppressed when the unknown-module warning is present. Before the fix the
/// `silt test` path leaked this, failing tests that `silt run` accepts.
#[test]
fn undefined_name_cascade_suppressed_when_user_import_warning_present() {
    // Each of the resolvable-error shapes is suppressed when the
    // unknown-module warning is present in the same compile.
    for msg in [
        "undefined variable `bar`",
        "undefined constructor `Baz`",
        "undefined type `Qux`",
        "unknown field `field`",
        "type `T` does not implement trait `Show`",
    ] {
        assert!(
            should_suppress_import_cascade_message(msg, /*is_warning=*/ false, true),
            "expected {msg:?} to be suppressed when has_user_import_warning=true"
        );
    }
}

/// The cascade is NOT suppressed when there was no unknown-module warning —
/// a genuine undefined-name error in a file with no user imports must still
/// abort the run/test. This guards against over-suppression.
#[test]
fn undefined_name_not_suppressed_without_user_import_warning() {
    for msg in [
        "undefined variable `bar`",
        "undefined constructor `Baz`",
        "type `T` does not implement trait `Show`",
    ] {
        assert!(
            !should_suppress_import_cascade_message(msg, /*is_warning=*/ false, false),
            "expected {msg:?} NOT to be suppressed when has_user_import_warning=false"
        );
    }
}

/// A real, unrelated type error (e.g. a type mismatch) is NEVER suppressed,
/// even when a user-import warning is present — only the recognised cascade
/// shapes are demoted (guards against GAP #7 over-suppression).
#[test]
fn real_type_mismatch_never_suppressed() {
    assert!(!should_suppress_import_cascade_message(
        "type mismatch: expected Int, got String",
        false,
        true
    ));
    assert!(!should_suppress_import_cascade_message(
        "type mismatch: expected Int, got String",
        false,
        false
    ));
}

/// Parity assertion: the predicate is symmetric in the two ways a cascade
/// can be present. With the unknown-module warning present, the combined
/// suppression set equals { the warning } ∪ { resolvable cascade errors };
/// without it, only the warning is suppressed. This mirrors what
/// `reportable_type_errors` (run/check) and the `silt test` loop both
/// compute via `should_suppress_import_cascade`.
#[test]
fn shared_predicate_matches_run_and_test_path_contract() {
    // With warning present: warning suppressed, cascade suppressed.
    assert!(should_suppress_import_cascade_message(
        "unknown module `foo`",
        true,
        true
    ));
    assert!(should_suppress_import_cascade_message(
        "undefined variable `x`",
        false,
        true
    ));

    // Without warning present: warning still suppressed (it is the warning),
    // but a lone undefined-name error is reported (not part of a cascade).
    assert!(should_suppress_import_cascade_message(
        "unknown module `foo`",
        true,
        false
    ));
    assert!(!should_suppress_import_cascade_message(
        "undefined variable `x`",
        false,
        false
    ));
}
