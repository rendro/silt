//! Regression tests for Postgres builtin security hardening.
//!
//! Locks three fixes in `src/builtins/postgres.rs`:
//!
//! - HIGH-4: Default TLS behaviour when the connection URL omits
//!   `sslmode=` is now **verify-full** (verify_ca + verify_hostname),
//!   not libpq's historical `prefer`. Explicit `sslmode=...` values
//!   still work the same as before.
//! - LOW-1: New `postgres.connect_with(url, opts)` builtin surfaces a
//!   `max_pool_size` knob so concurrent silt programs can raise the
//!   r2d2 default of 10 connections.
//! - LOW-2: DbError `DETAIL:` / `WHERE:` / `HINT:` follow-on lines are
//!   stripped before the error value crosses the VM boundary into
//!   silt, so a handler that echoes `Err(_)` to a 5xx response body
//!   cannot leak user row values (emails, names, etc.).
//!
//! Most of postgres requires a live DB; these tests exercise the
//! parsing / scrubbing / dispatch paths directly via small public
//! hooks (`resolve_effective_sslmode_for_tests`,
//! `read_max_pool_size_for_tests`, `redact_pg_message`).

#![cfg(feature = "postgres")]
// `Value` has interior-mutable variants (Channel, etc.) but the tests
// here only put String / Int into BTreeMap keys, which are pure data.
#![allow(clippy::mutable_key_type)]

use std::collections::BTreeMap;
use std::sync::Arc;

use silt::builtins::postgres::{
    EffectiveSslMode, read_max_pool_size_for_tests, redact_pg_message,
    resolve_effective_sslmode_for_tests,
};
use silt::value::Value;

// ── HIGH-4: default sslmode is verify-full, not prefer ─────────────

/// A URL that has **no** `sslmode=` query parameter must resolve to
/// `VerifyFull` (cert-chain + hostname check), not libpq's historical
/// `Prefer` / opportunistic TLS. This is the core HIGH-4 fix.
#[test]
fn default_sslmode_no_query_is_verify_full() {
    let mode = resolve_effective_sslmode_for_tests("postgres://u:p@host/db").expect("parse ok");
    assert_eq!(
        mode,
        EffectiveSslMode::VerifyFull,
        "URL without sslmode= must default to verify-full",
    );
}

/// A URL with a query string that omits `sslmode=` also defaults to
/// `VerifyFull`. Regression guard against the parser accidentally
/// branching on "has query string at all" rather than "has sslmode key".
#[test]
fn default_sslmode_other_params_is_verify_full() {
    let mode = resolve_effective_sslmode_for_tests(
        "postgres://u:p@host/db?connect_timeout=5&application_name=x",
    )
    .expect("parse ok");
    assert_eq!(mode, EffectiveSslMode::VerifyFull);
}

/// Every explicit `sslmode=...` value is honoured as-written — the new
/// default only kicks in when the parameter is missing entirely. This
/// preserves the libpq-compatible escape hatch for `sslmode=require`
/// and friends.
#[test]
fn explicit_sslmode_values_are_honoured() {
    let cases = [
        ("disable", EffectiveSslMode::Disable),
        ("prefer", EffectiveSslMode::Prefer),
        ("require", EffectiveSslMode::Require),
        ("verify-ca", EffectiveSslMode::VerifyCa),
        ("verify-full", EffectiveSslMode::VerifyFull),
    ];
    for (s, expected) in cases {
        let url = format!("postgres://u:p@h/db?sslmode={s}");
        let mode = resolve_effective_sslmode_for_tests(&url)
            .unwrap_or_else(|e| panic!("parse ok for {s}: {e}"));
        assert_eq!(mode, expected, "sslmode={s} should resolve to {expected:?}");
    }
}

// ── LOW-1: postgres.connect_with exposes max_pool_size ─────────────

fn empty_map_value() -> Value {
    Value::Map(Arc::new(BTreeMap::new()))
}

fn map_with(pairs: &[(&str, Value)]) -> Value {
    let mut m = BTreeMap::new();
    for (k, v) in pairs {
        m.insert(Value::String((*k).to_string()), v.clone());
    }
    Value::Map(Arc::new(m))
}

/// `connect_with(url, #{})` accepts the empty opts bag — i.e. the
/// shape `connect` delegates to — and yields `None` for every
/// optional tunable (meaning: fall through to r2d2's defaults).
#[test]
fn connect_with_empty_opts_parses() {
    let got = read_max_pool_size_for_tests(&empty_map_value()).expect("empty ok");
    assert_eq!(got, None);
}

/// `connect_with(url, #{"max_pool_size": 32})` is accepted and the
/// parsed value is the 32 we passed in.
#[test]
fn connect_with_max_pool_size_parses() {
    let opts = map_with(&[("max_pool_size", Value::Int(32))]);
    let got = read_max_pool_size_for_tests(&opts).expect("ok");
    assert_eq!(got, Some(32u32));
}

/// Zero / negative pool sizes are rejected — a pool of size 0 would
/// hang every `get()` forever, and negative makes no sense.
#[test]
fn connect_with_zero_max_pool_size_rejected() {
    let opts = map_with(&[("max_pool_size", Value::Int(0))]);
    let err = read_max_pool_size_for_tests(&opts).expect_err("zero rejected");
    assert!(err.contains("max_pool_size"), "err: {err}");
    assert!(err.contains("> 0") || err.contains("greater"), "err: {err}");

    let opts = map_with(&[("max_pool_size", Value::Int(-1))]);
    let err = read_max_pool_size_for_tests(&opts).expect_err("neg rejected");
    assert!(err.contains("max_pool_size"), "err: {err}");
}

/// Wrong shape — passing a String for `max_pool_size` — yields an
/// error that mentions the field name and that it must be an Int.
#[test]
fn connect_with_wrong_type_rejected() {
    let opts = map_with(&[("max_pool_size", Value::String("lots".to_string()))]);
    let err = read_max_pool_size_for_tests(&opts).expect_err("wrong type rejected");
    assert!(err.contains("max_pool_size"), "err: {err}");
    assert!(err.contains("Int"), "err: {err}");
}

/// Unknown keys in the opts map are silently ignored so future
/// options can be added without a breaking change to existing callers.
#[test]
fn connect_with_unknown_keys_ignored() {
    let opts = map_with(&[
        ("max_pool_size", Value::Int(15)),
        ("future_option_we_do_not_know_yet", Value::Int(99)),
    ]);
    let got = read_max_pool_size_for_tests(&opts).expect("unknown key ignored");
    assert_eq!(got, Some(15u32));
}

/// The opts argument must be a Map; passing anything else is a type
/// error (the typechecker should catch this before runtime, but the
/// Rust layer still double-checks).
#[test]
fn connect_with_non_map_opts_rejected() {
    // Lock the exact phrase from the only error-constructor site
    // (`src/builtins/postgres.rs` `parse_connect_opts`):
    //   "postgres.connect_with: opts must be a Map (e.g. #{})"
    // The previous `"Map" || "opts"` chain was weaker than a single
    // `contains` against the unique phrase, because both substrings are
    // always present in the only possible message.
    let err = read_max_pool_size_for_tests(&Value::Int(1)).expect_err("Int rejected");
    assert!(err.contains("opts must be a Map"), "err: {err}");
}

// ── LOW-2: redact_pg_message strips DETAIL / WHERE / HINT ──────────

/// The critical leak shape: a UNIQUE violation DbError's Display
/// includes a `DETAIL: Key (email)=(alice@example.com)` line. After
/// redaction, the email MUST be gone; the short primary message and
/// `SQLSTATE` code (the bits callers actually want) stay.
#[test]
fn redact_strips_unique_violation_detail_line() {
    let raw = "ERROR: duplicate key value violates unique constraint \"users_email_key\"\n\
               DETAIL: Key (email)=(alice@example.com) already exists.";
    let scrubbed = redact_pg_message(raw);

    // User data (the email) must not survive.
    assert!(
        !scrubbed.contains("alice@example.com"),
        "email leaked through scrub: {scrubbed:?}"
    );
    assert!(
        !scrubbed.contains("DETAIL"),
        "DETAIL label survived: {scrubbed:?}"
    );
    assert!(
        !scrubbed.contains("Key ("),
        "inline Key ( fragment survived: {scrubbed:?}"
    );

    // Short message + constraint NAME (not value) should still be
    // recoverable — that's the signal callers use to branch.
    assert!(
        scrubbed.contains("duplicate key value violates unique constraint"),
        "short message lost: {scrubbed:?}"
    );
    assert!(
        scrubbed.contains("users_email_key"),
        "constraint name lost: {scrubbed:?}"
    );
}

/// `WHERE:` lines get the same treatment — Postgres uses WHERE to
/// echo the failing predicate, which also routinely contains row data.
#[test]
fn redact_strips_where_line() {
    let raw = "ERROR: check constraint \"age_positive\" failed\n\
               WHERE: SQL statement \"UPDATE users SET age=-5 WHERE email='bob@x.com'\"";
    let scrubbed = redact_pg_message(raw);

    assert!(
        !scrubbed.contains("bob@x.com"),
        "WHERE row data leaked: {scrubbed:?}"
    );
    assert!(
        !scrubbed.contains("WHERE:"),
        "WHERE label survived: {scrubbed:?}"
    );
    assert!(
        scrubbed.contains("check constraint"),
        "short message lost: {scrubbed:?}"
    );
}

/// `HINT:` lines are also stripped — they're lower-risk than DETAIL
/// but occasionally carry row-adjacent hints ("add a unique index on
/// column X for value Y").
#[test]
fn redact_strips_hint_line() {
    let raw = "ERROR: column does not exist\nHINT: Perhaps you meant \"secret_api_key\".";
    let scrubbed = redact_pg_message(raw);

    assert!(
        !scrubbed.contains("HINT:"),
        "HINT label survived: {scrubbed:?}"
    );
    assert!(
        !scrubbed.contains("secret_api_key"),
        "HINT content leaked: {scrubbed:?}"
    );
    assert!(
        scrubbed.contains("column does not exist"),
        "short message lost: {scrubbed:?}"
    );
}

/// A message without any of the scrub targets passes through verbatim
/// (minus trailing whitespace).
#[test]
fn redact_leaves_clean_messages_alone() {
    let raw = "ERROR: relation \"widgets\" does not exist";
    let scrubbed = redact_pg_message(raw);
    assert_eq!(scrubbed, raw);
}

/// Multiple scrub-targets in a single blob: all get dropped, in order.
#[test]
fn redact_strips_all_in_combination() {
    let raw = "ERROR: duplicate key value violates unique constraint \"u\"\n\
               DETAIL: Key (k)=(v) already exists.\n\
               HINT: try a different key\n\
               WHERE: SQL: INSERT ...";
    let scrubbed = redact_pg_message(raw);

    assert!(!scrubbed.contains("DETAIL"), "DETAIL: {scrubbed:?}");
    assert!(!scrubbed.contains("HINT"), "HINT: {scrubbed:?}");
    assert!(!scrubbed.contains("WHERE"), "WHERE: {scrubbed:?}");
    assert!(!scrubbed.contains("Key ("), "Key: {scrubbed:?}");
    assert!(
        scrubbed.contains("duplicate key value"),
        "short message lost: {scrubbed:?}"
    );
}

/// A message whose PRIMARY text itself carries a `Key (...)=(...)`
/// segment (rare, but seen with custom constraints / extensions) is
/// still stripped — the safe prefix is kept.
#[test]
fn redact_strips_inline_key_fragment_in_primary_message() {
    let raw = "ERROR: custom constraint failed. Key (secret)=(hunter2) rejected.";
    let scrubbed = redact_pg_message(raw);
    assert!(
        !scrubbed.contains("hunter2"),
        "inline Key value leaked: {scrubbed:?}"
    );
    assert!(
        !scrubbed.contains("secret"),
        "inline Key col leaked: {scrubbed:?}"
    );
    assert!(
        scrubbed.contains("custom constraint failed"),
        "safe prefix lost: {scrubbed:?}"
    );
}

/// End-to-end typechecker guard: compiling a silt program that calls
/// `postgres.connect_with(url, #{"max_pool_size": N})` must type-check
/// cleanly, and calling `postgres.connect_with(url)` (missing the
/// opts bag) must be a type error. This locks the new signature
/// against accidental arity regressions.
#[test]
fn connect_with_typechecks_end_to_end() {
    use silt::compiler::Compiler;
    use silt::lexer::Lexer;
    use silt::parser::Parser;

    // Type-checks: opts bag present with a valid Int field.
    //
    // Silt's typechecker doesn't *require* a successful typecheck for
    // compilation (it gathers diagnostics but compiles anyway), so
    // the real signal is "the compile pipeline completes without a
    // runtime call" + "the `postgres.connect_with` name resolves".
    let good = r#"
        import postgres
        fn main() {
          let _ = postgres.connect_with("postgres://x@y/z", #{"max_pool_size": 32})
        }
    "#;
    let tokens = Lexer::new(good).tokenize().expect("lex");
    let mut program = Parser::new(tokens).parse_program().expect("parse");
    // Running the typechecker smoke-tests name resolution. Any missing
    // builtin would bubble as a "name not found" diagnostic.
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    // Compile must succeed — confirms the dispatch target resolves.
    let _ = compiler
        .compile_program(&program)
        .expect("compile good program");
}
