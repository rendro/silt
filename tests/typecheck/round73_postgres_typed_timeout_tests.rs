//! Round 73 B3: postgres builtins must surface a typed `PostgresError` variant
//! on deadline-driven timeouts, not the generic `IoError::Other(_)`
//! shape.
//!
//! Background: every fallible `postgres.*` call is typechecked as
//! `Result(_, PostgresError)` (see the module's rows in the builtin registry).
//! Pre-fix, the runtime used the default `Vm::io_entry_guard(args)` and
//! `IoPool::submit(...)` paths, both of which fall through to the
//! generic `io_unknown_timeout_err` factory. The fix supplies a
//! dedicated `pg_timeout_err` factory (surfacing `Timeout` as a
//! nullary variant) plus a `pg_completion()` helper.
//!
//! This file locks the factory's exact `Err(Timeout)` shape (the same
//! shape the watchdog produces), cfg-gated on `postgres`.

// ── Runtime shape test (postgres feature only) ──────────────────────

#[cfg(feature = "postgres")]
mod with_feature {
    use silt::builtins::postgres::pg_timeout_err_for_tests;
    use silt::value::Value;

    /// The typed-timeout factory must produce exactly the shape the
    /// silt-side `Result(_, PostgresError)` signature expects: an outer `Err`
    /// variant wrapping a nullary `Timeout` constructor. The message
    /// argument is intentionally dropped because `Timeout` carries no
    /// payload (the trait `e.message()` impl in
    /// `src/builtins/postgres.rs` synthesises the user-visible string).
    #[test]
    fn pg_timeout_err_returns_typed_pg_error_variant() {
        let v = pg_timeout_err_for_tests("watchdog: deadline exceeded");
        let Value::Variant(outer_tag) = &v else {
            panic!("expected Value::Variant, got {v:?}");
        };
        assert_eq!(outer_tag.name(), "Err", "outer tag must be `Err`");
        assert_eq!(
            outer_tag.fields().len(),
            1,
            "Err must carry exactly one payload"
        );

        let Value::Variant(inner_tag) = &outer_tag.fields()[0] else {
            panic!(
                "expected inner Value::Variant, got {:?}",
                outer_tag.fields()[0]
            );
        };
        assert_eq!(
            inner_tag.name(),
            "Timeout",
            "inner tag must be `Timeout` (typed PostgresError variant), \
             not `Other` (the IoError default)",
        );
        assert!(
            inner_tag.fields().is_empty(),
            "Timeout is nullary in the typechecker registry, but \
             pg_timeout_err produced fields: {:?}",
            inner_tag.fields(),
        );
    }

    /// Cross-check: the variant must NOT be the `IoError::Other`
    /// shape that the pre-fix runtime emitted. This is what the audit
    /// caught — silt code typed the call as returning `PostgresError` but got
    /// `Other(msg)` at runtime instead.
    #[test]
    fn pg_timeout_err_is_not_io_unknown() {
        let v = pg_timeout_err_for_tests("any message");
        let Value::Variant(variant) = &v else {
            panic!("expected Value::Variant, got {v:?}");
        };
        let Value::Variant(inner_tag) = &variant.fields()[0] else {
            panic!(
                "expected inner Value::Variant, got {:?}",
                variant.fields()[0]
            );
        };
        assert_ne!(
            inner_tag.name(),
            "Other",
            "pg_timeout_err must not produce the IoError-flavoured \
             `Other` shape — that's the bug round-73 B3 fixed",
        );
    }
}
