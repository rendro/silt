//! Round-52 deferred item 8: lock `src/builtins/postgres.rs` against the
//! bare `.lock().unwrap()` pattern.
//!
//! The postgres builtin previously used `Mutex::lock().unwrap()` at ~20
//! sites. When a prior thread panicked while holding the lock (which the
//! VM's `catch_builtin_panic` already recovers from at the outer layer),
//! re-entering the guard would re-panic on the poison state and surface
//! as a *second* panic in logs — noisy and misleading.
//!
//! The fix: every `.lock().unwrap()` becomes
//! `.lock().unwrap_or_else(|e| e.into_inner())` so poisoned guards are
//! cleanly recovered instead of re-panicked. The registry / cursor /
//! conn data is not invariant-sensitive across the poison boundary
//! (inserts + removes of `Arc<...>` cells; no partial mutations live
//! across panic sites), so consuming the inner guard is safe.
//!
//! The grep-style regression test below reads the postgres source text
//! and asserts the forbidden pattern is absent, locking the refactor
//! against accidental reintroduction. It runs on every `cargo test`
//! invocation regardless of the `postgres` feature flag, since it only
//! consults file contents.
//!
//! A functional "force-poison a registry lock and verify the next call
//! doesn't double-panic" test was **skipped intentionally**: the
//! registry accessors (`registry()`, `tx_registry()`, `cursors()`) and
//! the `PoolEntry` / `PinnedConn` mutexes are all private module-level
//! state with no test-only hook to inject a panic inside a held guard
//! from outside the crate. Adding such a hook would widen the public
//! surface of `src/builtins/postgres.rs` — which is explicitly out of
//! scope for this task (the contract forbids touching other files and
//! discourages test-only public APIs for a grep-locked refactor that is
//! already syntactically checked). The grep test is sufficient to
//! prevent regression.

use std::fs;
use std::path::PathBuf;

/// Forbidden pattern: bare `.lock().unwrap()` on a `Mutex`. Every
/// occurrence must be rewritten as
/// `.lock().unwrap_or_else(|e| e.into_inner())` so that poisoned locks
/// are recovered rather than re-panicked.
#[test]
fn postgres_source_has_no_bare_lock_unwrap() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let path = PathBuf::from(manifest_dir)
        .join("src")
        .join("builtins")
        .join("postgres.rs");
    let src = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));

    // Scan line-by-line so the failure message points at the offender.
    let mut offenders: Vec<(usize, String)> = Vec::new();
    for (idx, line) in src.lines().enumerate() {
        // `.lock()` followed by `.unwrap()` with no intervening
        // `unwrap_or_else` / `expect` / map. We deliberately accept
        // `.lock().unwrap_or_else(...)` and `.lock().expect(...)` —
        // only the bare `.unwrap()` tail is forbidden.
        if line.contains(".lock().unwrap()") {
            offenders.push((idx + 1, line.to_string()));
        }
    }

    assert!(
        offenders.is_empty(),
        "src/builtins/postgres.rs must not contain `.lock().unwrap()` \
         (use `.lock().unwrap_or_else(|e| e.into_inner())` to recover \
         poisoned guards cleanly). Offending lines:\n{}",
        offenders
            .iter()
            .map(|(n, l)| format!("  line {n}: {l}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

/// Positive-shape assertion: the recovery pattern is actually used in
/// the file. Guards against a "delete the lock calls entirely" rewrite
/// that would pass the negative grep but leave the builtin broken.
#[test]
fn postgres_source_uses_poison_recovery_pattern() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let path = PathBuf::from(manifest_dir)
        .join("src")
        .join("builtins")
        .join("postgres.rs");
    let src = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));

    let hits = src.matches(".lock().unwrap_or_else(|e| e.into_inner())").count();
    assert!(
        hits >= 15,
        "expected the poison-recovery lock pattern to appear many \
         times in src/builtins/postgres.rs, but found only {hits} \
         occurrences — the registry/cursor/conn lock sites should all \
         use this form",
    );
}
