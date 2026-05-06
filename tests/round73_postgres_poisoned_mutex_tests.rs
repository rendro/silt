//! Round 73 L2: postgres `transact` finalizer must recover poisoned
//! mutexes cleanly rather than panicking.
//!
//! Background: round-66 converted every `Mutex::lock().unwrap()` site
//! in `src/builtins/postgres.rs` to the recovery form
//! `.lock().unwrap_or_else(|e| e.into_inner())` — see
//! `tests/postgres_poisoned_lock_tests.rs` for the lock on that
//! refactor. **One site was missed**: the COMMIT/ROLLBACK finalizer's
//! `Ok(mutex)` arm at line 2046 used `mutex.into_inner().unwrap()`
//! instead of `mutex.into_inner().unwrap_or_else(|e| e.into_inner())`.
//!
//! `Mutex::into_inner()` returns `LockResult<T>` exactly like `lock()`
//! does — a poisoned mutex makes the call return `Err(PoisonError<T>)`,
//! and the unwrap re-panics. The fix mirrors the sibling `Err(shared)`
//! arm directly below, which already used `.unwrap_or_else(|e|
//! e.into_inner())` correctly.
//!
//! ## What this file locks
//!
//! Source-grep regression test: scans `src/builtins/postgres.rs` for
//! the forbidden pattern `into_inner().unwrap()`. Runs without the
//! postgres feature so default `cargo test` catches the regression
//! even when libpq isn't installed.
//!
//! A direct functional test (force-poison the inner mutex inside a tx
//! cell, drive the finalizer through the `transact` path, assert no
//! panic + correct typed `Err(PgError)` returned) is **not feasible
//! from outside the crate**: the tx registry, `PinnedConn` mutex, and
//! `transact`'s `finalise` closure are all private module-level state
//! with no public test hook. Adding a test-only hook would widen the
//! public surface for what is otherwise a single-line refactor that's
//! perfectly captured by the grep lock — same trade-off the round-52
//! `postgres_poisoned_lock_tests.rs` made.

use std::fs;
use std::path::PathBuf;

/// Forbidden pattern: any `into_inner()` tail that panics on a
/// poisoned mutex. Every occurrence must be rewritten as
/// `into_inner().unwrap_or_else(|e| e.into_inner())` so that consuming
/// a poisoned mutex recovers the inner value instead of re-panicking.
///
/// Round-74 tightening: the original lock only rejected bare
/// `into_inner().unwrap()`. A future refactor that switched to
/// `into_inner().expect("poisoned")` or
/// `into_inner().unwrap_or_else(|_| panic!(...))` would preserve the
/// bug class (panic on poison) but slip past the negative grep. The
/// rejection list now covers all known panic-on-poison shapes.
#[test]
fn postgres_source_has_no_bare_into_inner_unwrap() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let path = PathBuf::from(manifest_dir)
        .join("src")
        .join("builtins")
        .join("postgres.rs");
    let src =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));

    let mut offenders: Vec<(usize, String)> = Vec::new();
    for (idx, line) in src.lines().enumerate() {
        // Skip doc comments — referencing the function name in prose
        // is fine.
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            continue;
        }
        // `.into_inner()` followed by any panic-on-error tail. We
        // accept ONLY `.into_inner().unwrap_or_else(|e| e.into_inner())`
        // (and shapes that recover the value rather than panicking).
        // The forbidden tails are:
        //   * `.into_inner().unwrap()`               — bare unwrap
        //   * `.into_inner().expect(`                — message + panic
        //   * `.into_inner().unwrap_or_else(|_| panic!`  — clever panic
        if line.contains(".into_inner().unwrap()")
            || line.contains(".into_inner().expect(")
            || line.contains(".into_inner().unwrap_or_else(|_| panic!")
        {
            offenders.push((idx + 1, line.to_string()));
        }
    }

    assert!(
        offenders.is_empty(),
        "src/builtins/postgres.rs must not contain any panic-on-poison \
         tail after `.into_inner()` (forbidden tails: `.unwrap()`, \
         `.expect(...)`, `.unwrap_or_else(|_| panic!...)`). Use \
         `.into_inner().unwrap_or_else(|e| e.into_inner())` to recover \
         from a poisoned mutex cleanly — see round-73 L2 fix and \
         round-74 lock tightening. Offending lines:\n{}",
        offenders
            .iter()
            .map(|(n, l)| format!("  line {n}: {l}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

/// Positive-shape assertion: the `transact` finalizer's `Ok(mutex)`
/// arm uses the recovery pattern. Guards against a "delete the COMMIT
/// path entirely" rewrite that would pass the negative grep but break
/// the builtin.
#[test]
fn postgres_finalizer_uses_poison_recovery_pattern() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let path = PathBuf::from(manifest_dir)
        .join("src")
        .join("builtins")
        .join("postgres.rs");
    let src =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));

    assert!(
        src.contains("mutex.into_inner().unwrap_or_else(|e| e.into_inner())"),
        "expected the COMMIT/ROLLBACK finalizer in src/builtins/postgres.rs \
         to consume the inner mutex via \
         `mutex.into_inner().unwrap_or_else(|e| e.into_inner())`, but the \
         pattern was not found",
    );
}
