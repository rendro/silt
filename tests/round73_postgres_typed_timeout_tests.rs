//! Round 73 B3: postgres builtins must surface a typed `PgError` variant
//! on deadline-driven timeouts, not the generic `IoError::IoUnknown(_)`
//! shape.
//!
//! Background: every fallible `postgres.*` call is typechecked as
//! `Result(_, PgError)` (see `src/typechecker/builtins/postgres.rs`).
//! Pre-fix, the runtime used the default `Vm::io_entry_guard(args)` and
//! `IoPool::submit(...)` paths, both of which fall through to the
//! generic `io_unknown_timeout_err` factory. When the watchdog fired or
//! `task.deadline` elapsed mid-call, the silt program received an
//! `IoError`-shaped variant against a `PgError` signature — a
//! type-system / runtime mismatch.
//!
//! The fix: postgres now mirrors the tcp / http modules and supplies a
//! dedicated `pg_timeout_err` factory (surfacing `PgTimeout` as a
//! nullary variant) plus a `pg_completion()` helper. All 9
//! `io_entry_guard` sites now use `io_entry_guard_with(args,
//! &pg_timeout_err)` and all 5 `io_pool.submit` sites now use
//! `io_pool.submit_with(pg_completion(), ...)`.
//!
//! ## What this file locks
//!
//! 1. **Unit shape test** (cfg-gated on `postgres`): calls the
//!    `pg_timeout_err_for_tests` helper directly and asserts the
//!    exact `Err(PgTimeout)` shape — same shape the watchdog produces.
//! 2. **Source-grep lock** (always runs): scans `src/builtins/postgres.rs`
//!    and asserts no remaining `vm.io_entry_guard(args)` or bare
//!    `io_pool.submit(...)` call sites — both forms are equivalent to
//!    using the default `IoError`-flavoured factory and would re-introduce
//!    the bug. Runs without the postgres feature so default `cargo test`
//!    catches a regression even when libpq isn't installed.

use std::fs;
use std::path::PathBuf;

/// Source-grep lock: every postgres builtin entry-guard call must go
/// through the typed-error variant `io_entry_guard_with(_, &pg_timeout_err)`.
/// A bare `vm.io_entry_guard(args)` call would fall back to
/// `io_unknown_timeout_err` and re-introduce B3.
#[test]
fn postgres_source_uses_typed_io_entry_guard_only() {
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
        // Forbidden: bare `io_entry_guard(...)` (no `_with` suffix).
        // We accept `io_entry_guard_with(...)` because that takes an
        // explicit factory argument.
        if line.contains("io_entry_guard(") {
            offenders.push((idx + 1, line.to_string()));
        }
    }

    assert!(
        offenders.is_empty(),
        "src/builtins/postgres.rs must use `io_entry_guard_with(args, \
         &pg_timeout_err)` rather than the bare `io_entry_guard(args)` \
         form (the bare form falls back to the IoError-flavoured \
         timeout factory). Offending lines:\n{}",
        offenders
            .iter()
            .map(|(n, l)| format!("  line {n}: {l}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

/// Source-grep lock: every postgres `io_pool.submit(...)` site must use
/// the typed `submit_with(pg_completion(), ...)` form so the watchdog's
/// timeout-error factory produces `Err(PgTimeout)` rather than
/// `Err(IoUnknown(_))`.
#[test]
fn postgres_source_uses_typed_io_pool_submit_only() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let path = PathBuf::from(manifest_dir)
        .join("src")
        .join("builtins")
        .join("postgres.rs");
    let src =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));

    let mut offenders: Vec<(usize, String)> = Vec::new();
    for (idx, line) in src.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            continue;
        }
        // Forbidden: `.submit(` directly on `io_pool`. Accept
        // `.submit_with(`.
        if (line.contains("io_pool.submit(") || line.contains("io_pool\n"))
            && !line.contains("submit_with")
        {
            // Heuristic above doesn't cover wrapped lines where
            // `.submit(` is on its own line after a `.io_pool` chain.
            // The simpler check below catches the inline form which is
            // how we wrote them originally.
            if line.contains("io_pool.submit(") {
                offenders.push((idx + 1, line.to_string()));
            }
        }
        // Multi-line chain form: `.io_pool\n    .submit(...)`
        if line.trim() == ".submit(move || do_connect(url));"
            || line.trim().starts_with(".submit(move ||")
        {
            offenders.push((idx + 1, line.to_string()));
        }
    }
    // De-dup just in case both checks fired on the same line.
    offenders.sort();
    offenders.dedup();

    assert!(
        offenders.is_empty(),
        "src/builtins/postgres.rs must use `io_pool.submit_with(\
         pg_completion(), ...)` rather than the bare `io_pool.submit(...)` \
         form (the bare form uses the default IoError-flavoured \
         IoCompletion). Offending lines:\n{}",
        offenders
            .iter()
            .map(|(n, l)| format!("  line {n}: {l}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

/// Positive shape: confirm the typed helpers actually appear in the
/// source. Guards against a "delete the calls entirely" rewrite that
/// would pass the negative greps but break the module.
#[test]
fn postgres_source_has_typed_io_helpers() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let path = PathBuf::from(manifest_dir)
        .join("src")
        .join("builtins")
        .join("postgres.rs");
    let src =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));

    let entry_guard_with = src.matches("io_entry_guard_with(args, &pg_timeout_err)").count();
    let submit_with = src.matches("submit_with(pg_completion()").count();

    // 9 entry-guard sites + 5 submit sites per the round-73 audit.
    assert!(
        entry_guard_with >= 9,
        "expected at least 9 `io_entry_guard_with(args, &pg_timeout_err)` \
         sites in src/builtins/postgres.rs, found {entry_guard_with}",
    );
    assert!(
        submit_with >= 5,
        "expected at least 5 `submit_with(pg_completion(), ...)` sites \
         in src/builtins/postgres.rs, found {submit_with}",
    );
    assert!(
        src.contains("fn pg_timeout_err"),
        "expected `fn pg_timeout_err` to be defined in src/builtins/postgres.rs",
    );
    assert!(
        src.contains("fn pg_completion"),
        "expected `fn pg_completion` helper to be defined in src/builtins/postgres.rs",
    );
}

// ── Runtime shape test (postgres feature only) ──────────────────────

#[cfg(feature = "postgres")]
mod with_feature {
    use silt::builtins::postgres::pg_timeout_err_for_tests;
    use silt::value::Value;

    /// The typed-timeout factory must produce exactly the shape the
    /// silt-side `Result(_, PgError)` signature expects: an outer `Err`
    /// variant wrapping a nullary `PgTimeout` constructor. The message
    /// argument is intentionally dropped because `PgTimeout` carries no
    /// payload (the trait `e.message()` impl in
    /// `src/builtins/postgres.rs` synthesises the user-visible string).
    #[test]
    fn pg_timeout_err_returns_typed_pg_error_variant() {
        let v = pg_timeout_err_for_tests("watchdog: deadline exceeded");
        let Value::Variant(outer_tag, outer_fields) = &v else {
            panic!("expected Value::Variant, got {v:?}");
        };
        assert_eq!(outer_tag.as_str(), "Err", "outer tag must be `Err`");
        assert_eq!(outer_fields.len(), 1, "Err must carry exactly one payload");

        let Value::Variant(inner_tag, inner_fields) = &outer_fields[0] else {
            panic!("expected inner Value::Variant, got {:?}", outer_fields[0]);
        };
        assert_eq!(
            inner_tag.as_str(),
            "PgTimeout",
            "inner tag must be `PgTimeout` (typed PgError variant), \
             not `IoUnknown` (the IoError default)",
        );
        assert!(
            inner_fields.is_empty(),
            "PgTimeout is nullary in the typechecker registry, but \
             pg_timeout_err produced fields: {inner_fields:?}",
        );
    }

    /// Cross-check: the variant must NOT be the `IoError::IoUnknown`
    /// shape that the pre-fix runtime emitted. This is what the audit
    /// caught — silt code typed the call as returning `PgError` but got
    /// `IoUnknown(msg)` at runtime instead.
    #[test]
    fn pg_timeout_err_is_not_io_unknown() {
        let v = pg_timeout_err_for_tests("any message");
        let Value::Variant(_, outer_fields) = &v else {
            panic!("expected Value::Variant, got {v:?}");
        };
        let Value::Variant(inner_tag, _) = &outer_fields[0] else {
            panic!("expected inner Value::Variant, got {:?}", outer_fields[0]);
        };
        assert_ne!(
            inner_tag.as_str(),
            "IoUnknown",
            "pg_timeout_err must not produce the IoError-flavoured \
             `IoUnknown` shape — that's the bug round-73 B3 fixed",
        );
    }
}
