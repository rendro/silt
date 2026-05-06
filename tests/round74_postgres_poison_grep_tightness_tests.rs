//! Round 74: meta-test that locks the round-73 source-grep tightening.
//!
//! The round-73 lock at `tests/round73_postgres_poisoned_mutex_tests.rs`
//! originally rejected only the exact pre-fix string
//! `into_inner().unwrap()`. A future refactor using
//! `into_inner().expect("poisoned")` would have preserved the
//! panic-on-poison bug class while slipping past the negative grep.
//!
//! Round-74 tightened the lock to ALSO reject:
//!   * `into_inner().expect(`
//!   * `into_inner().unwrap_or_else(|_| panic!`
//!
//! This file is the lock on the lock: if a future change loosens the
//! round-73 rejection list, these meta-tests fail.
//!
//! Implementation note: the source of round-73 is pulled in via
//! `include_str!` so this is a pure compile-time string check — no
//! filesystem races, no path discovery.
//!
//! See also: `feedback_one_way_to_do_things.md` — collapse equivalent
//! dual shapes to one unified form. The unified form here is
//! `.into_inner().unwrap_or_else(|e| e.into_inner())`; every other
//! tail must be rejected by the round-73 grep.

const ROUND73: &str = include_str!("round73_postgres_poisoned_mutex_tests.rs");

/// The original (pre-tightening) rejection must remain in place.
#[test]
fn round73_lock_still_rejects_bare_unwrap() {
    assert!(
        ROUND73.contains(".into_inner().unwrap()"),
        "round-73 lock must continue to reject `.into_inner().unwrap()`; \
         the literal pattern was not found in the test source",
    );
}

/// Round-74 tightening: the lock must reject `.into_inner().expect(`.
#[test]
fn round73_lock_rejects_into_inner_expect() {
    assert!(
        ROUND73.contains(".into_inner().expect("),
        "round-73 lock must reject `.into_inner().expect(` (round-74 \
         tightening). A future refactor that switched to \
         `into_inner().expect(\"poisoned\")` would preserve the \
         panic-on-poison bug class but slip past a narrower grep.",
    );
}

/// Round-74 tightening: the lock must reject the "clever panic" shape
/// `.into_inner().unwrap_or_else(|_| panic!`.
#[test]
fn round73_lock_rejects_into_inner_unwrap_or_else_panic() {
    assert!(
        ROUND73.contains(".into_inner().unwrap_or_else(|_| panic!"),
        "round-73 lock must reject `.into_inner().unwrap_or_else(|_| panic!` \
         (round-74 tightening). This shape is functionally identical to \
         `.expect(...)` and must not slip through.",
    );
}

/// Defensive: the rejection list must still leave the recovery form
/// alone. If this fires, someone over-tightened and broke the legal
/// pattern.
#[test]
fn round73_lock_still_documents_recovery_pattern() {
    assert!(
        ROUND73.contains(".into_inner().unwrap_or_else(|e| e.into_inner())"),
        "round-73 lock must continue to reference the recovery pattern \
         `.into_inner().unwrap_or_else(|e| e.into_inner())` as the \
         legal replacement",
    );
}
