//! Round 73 B3: postgres builtins must surface a typed `PgError` variant
//! on deadline-driven timeouts, not the generic `IoError::IoUnknown(_)`
//! shape.
//!
//! Background: every fallible `postgres.*` call is typechecked as
//! `Result(_, PgError)` (see `src/typechecker/builtins/postgres.rs`).
//! Pre-fix, the runtime used the default `Vm::io_entry_guard(args)` and
//! `IoPool::submit(...)` paths, both of which fall through to the
//! generic `io_unknown_timeout_err` factory. The fix supplies a
//! dedicated `pg_timeout_err` factory (surfacing `PgTimeout` as a
//! nullary variant) plus a `pg_completion()` helper.
//!
//! This file locks the factory's exact `Err(PgTimeout)` shape (the same
//! shape the watchdog produces), cfg-gated on `postgres`.

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
        assert_eq!(outer_tag.name(), "Err", "outer tag must be `Err`");
        assert_eq!(outer_fields.len(), 1, "Err must carry exactly one payload");

        let Value::Variant(inner_tag, inner_fields) = &outer_fields[0] else {
            panic!("expected inner Value::Variant, got {:?}", outer_fields[0]);
        };
        assert_eq!(
            inner_tag.name(),
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
            inner_tag.name(),
            "IoUnknown",
            "pg_timeout_err must not produce the IoError-flavoured \
             `IoUnknown` shape — that's the bug round-73 B3 fixed",
        );
    }
}
