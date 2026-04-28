//! Source-grep locks for two error-message invariants in
//! `src/builtins/`.
//!
//! Round 58/61 canonicalised the user-facing internal-invariant
//! error prefix to `"internal VM error:"` (replacing the bare
//! `"internal:"` form) across `src/vm/`. The round 61
//! source-grep lock (`tests/vm_internal_error_prefix_lock_tests.rs`)
//! only covers `src/vm/mod.rs` and `src/vm/execute.rs` — it does
//! not extend to `src/builtins/`. Round 62 found one bare
//! `"internal:"` prefix at `src/builtins/postgres.rs:1976` that
//! escaped the round-58 sweep.
//!
//! Round 62 also found a Rust-identifier leak at
//! `src/builtins/data.rs:1287`: the `time.now` overflow message
//! contained the literal Rust local `epoch_ms`, the same shape
//! as the round-61 `read_byte` leak in `src/vm/mod.rs`.
//!
//! These tests `include_str!` the relevant builtin source files
//! and grep for the bug substrings (so they fail pre-fix and pass
//! post-fix) and for the canonical replacement substrings (so a
//! refactor that *removes* the leaky text but forgets to introduce
//! the replacement also fails the lock).
//!
//! Source-grep locks are preferred per the audit doc for
//! error-message fixes whose runtime path requires either a VM
//! invariant violation (postgres `transact` resume without a
//! `PgTx` handle) or a 290k-year clock skew (`time.now` overflow).
//! `include_str!` makes the scan unconditional regardless of
//! feature flags — `src/builtins/postgres.rs` is gated behind
//! `#![cfg(feature = "postgres")]`, but the file content is read
//! at compile time as a `&str`, not as Rust code.
//!
//! Complements `tests/vm_internal_error_prefix_lock_tests.rs`
//! (which locks `src/vm/mod.rs` + `src/vm/execute.rs`) — this
//! file extends the same invariants to `src/builtins/`.

// ── Source snapshots ─────────────────────────────────────────────────

const POSTGRES_RS: &str = include_str!("../src/builtins/postgres.rs");
const DATA_RS: &str = include_str!("../src/builtins/data.rs");

// ── postgres.rs: canonical "internal VM error:" prefix ───────────────

/// `src/builtins/postgres.rs` had one bare `"internal:"` prefix at
/// `transact`'s resume path that escaped the round-58 canonicalisation
/// sweep (which only covered `src/vm/`). Lock the file as a whole now,
/// matching the round-61 pattern from
/// `vm_mod_uses_canonical_internal_prefix`.
#[test]
fn postgres_module_uses_canonical_internal_prefix() {
    // Bug form: bare `"internal:"` literal prefix (opening double
    // quote + "internal:") — guards against literal occurrences in
    // `VmError::new("internal: ...")` while permitting documentation
    // text that mentions the word "internal" in prose.
    assert!(
        !POSTGRES_RS.contains("\"internal:"),
        "src/builtins/postgres.rs contains a bare '\"internal:' \
         string-literal prefix; use '\"internal VM error:' instead \
         to match the canonical round-58 prefix established for VM \
         internal-invariant failures"
    );
    // Post-fix form: the canonical replacement must actually be
    // present, so a future refactor that *removes* the leaky text
    // but forgets to introduce the replacement still fails the lock.
    assert!(
        POSTGRES_RS.contains("internal VM error:"),
        "src/builtins/postgres.rs missing the canonical \
         'internal VM error:' user-facing prefix; the round-62 fix \
         to `transact`'s resume path must use this phrasing"
    );
}

// ── data.rs: no `epoch_ms` Rust identifier in error text ─────────────

/// `src/builtins/data.rs:1285` binds the Rust local `epoch_ms` from
/// `vm.epoch_ms()?` and originally leaked that identifier verbatim
/// into the `time.now` overflow `VmError` message. This is the same
/// shape as the round-61 `read_byte` leak in `src/vm/mod.rs`.
///
/// Note: `epoch_ns` is *not* a leak — it's the user-facing field
/// name on the `Instant` record, so error messages mentioning
/// `epoch_ns` (e.g. `"Instant missing epoch_ns field"`) are
/// legitimate. We only lock against the leaked `epoch_ms` phrase.
#[test]
fn data_module_does_not_leak_epoch_ms_rust_identifier_in_error() {
    // Bug form: the original literal contained
    // `"time arithmetic overflow: time.now epoch_ms * 1_000_000"` —
    // grep the distinctive `time.now epoch_ms` substring so we
    // don't false-positive on a sentence like "we read epoch_ms".
    assert!(
        !DATA_RS.contains("time.now epoch_ms"),
        "src/builtins/data.rs leaks the Rust local 'epoch_ms' inside \
         a user-facing VmError message ('time.now epoch_ms' substring \
         found); rephrase using neutral wording like \
         'epoch milliseconds * 1_000_000 overflows i64'"
    );
    // Post-fix form: the canonical replacement must be present.
    assert!(
        DATA_RS
            .contains("time.now: epoch milliseconds * 1_000_000 overflows i64"),
        "src/builtins/data.rs missing the canonical user-facing \
         replacement for the time.now overflow message; expected \
         'time.now: epoch milliseconds * 1_000_000 overflows i64'"
    );
}
