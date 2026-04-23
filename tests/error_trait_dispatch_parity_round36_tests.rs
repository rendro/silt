//! Round-36 parity locks for the `trait Error for <FooError>` dispatch
//! helpers across 11 builtin error enums.
//!
//! Before the round-36 collapse, every `call_<X>_error_trait` (11 of
//! them, across 8 modules: concurrency, bytes, toml, postgres, tcp,
//! io, numeric, plus 4 in data.rs — json/http/regex/time) duplicated
//! the same three-piece scaffolding:
//!
//!   1. `args.len() != 1` → "takes 1 argument (self), got {n}"
//!   2. `args[0]` not a `Value::Variant` → "expected <Enum> variant, got <v>"
//!   3. unknown method name → "unknown <Enum> trait method: <name>"
//!
//! plus an inner variant → message rendering. Round-36 extracted the
//! three shared pieces into `builtins::dispatch_error_trait`, leaving
//! each site to supply only the inner render. These tests lock that
//! refactor: they exercise every one of the three shared paths for
//! each of the 11 enums, plus a `message()` happy-path for at least
//! one representative variant.
//!
//! Drift-resolution: inspection of the pre-refactor source showed
//! ZERO drift — all 11 sites already used identical phrasings with
//! `args.len()` (the receiver, which is arg 0, IS counted). The
//! helper therefore pins the phrasings verbatim; if a future caller
//! disagrees, these tests will fail.

use silt::builtins::bytes::call_bytes_error_trait;
use silt::builtins::concurrency::call_channel_error_trait;
use silt::builtins::data::{
    call_http_error_trait, call_json_error_trait, call_regex_error_trait,
    call_time_error_trait,
};
use silt::builtins::io::call_io_error_trait;
use silt::builtins::numeric::call_parse_error_trait;
use silt::builtins::toml::call_toml_error_trait;
use silt::value::Value;

#[cfg(feature = "postgres")]
use silt::builtins::postgres::call_pg_error_trait;
#[cfg(feature = "tcp")]
use silt::builtins::tcp::call_tcp_error_trait;

// ── tiny harness ─────────────────────────────────────────────────────

type Dispatch = fn(&str, &[Value]) -> Result<Value, silt::vm::VmError>;

fn s(lit: &str) -> Value {
    Value::String(lit.to_string())
}

fn v(tag: &str, fields: Vec<Value>) -> Value {
    Value::Variant(tag.to_string(), fields)
}

fn expect_ok(dispatch: Dispatch, name: &str, args: &[Value]) -> String {
    match dispatch(name, args) {
        Ok(Value::String(s)) => s,
        Ok(other) => panic!("expected Value::String, got {other:?}"),
        Err(e) => panic!("expected Ok, got Err({})", e.message),
    }
}

fn expect_err(dispatch: Dispatch, name: &str, args: &[Value]) -> String {
    match dispatch(name, args) {
        Ok(v) => panic!("expected Err, got Ok({v:?})"),
        Err(e) => e.message,
    }
}

// The helper shape — parametric over enum_name and a valid receiver
// variant. For each of the 11 enums we run this trio of assertions.
fn assert_scaffolding_parity(
    dispatch: Dispatch,
    enum_name: &str,
    receiver: Value,
    expected_message: &str,
) {
    // 1. Happy path — `message()` returns the rendered string.
    assert_eq!(
        expect_ok(dispatch, "message", &[receiver.clone()]),
        expected_message,
        "{enum_name}: message() happy-path rendering changed",
    );

    // 2. Wrong arity — extra arg past the receiver. `args.len()` IS
    //    what the error reports (receiver counts as arg 0, extras
    //    push the count above 1). Two extras → got 3, etc.
    assert_eq!(
        expect_err(
            dispatch,
            "message",
            &[receiver.clone(), Value::Int(99)]
        ),
        format!("{enum_name}.message takes 1 argument (self), got 2"),
        "{enum_name}: wrong-arity error wording changed",
    );
    // 0 args — missing receiver entirely.
    assert_eq!(
        expect_err(dispatch, "message", &[]),
        format!("{enum_name}.message takes 1 argument (self), got 0"),
        "{enum_name}: zero-arg error wording changed",
    );

    // 3. Unknown method name.
    assert_eq!(
        expect_err(dispatch, "not_a_real_method", &[receiver.clone()]),
        format!("unknown {enum_name} trait method: not_a_real_method"),
        "{enum_name}: unknown-method error wording changed",
    );

    // 4. Wrong receiver shape (non-Variant).
    assert_eq!(
        expect_err(dispatch, "message", &[Value::Int(7)]),
        format!("{enum_name}.message: expected {enum_name} variant, got 7"),
        "{enum_name}: wrong-receiver-shape error wording changed",
    );
}

// ── per-enum parity tests (11 total) ─────────────────────────────────

#[test]
fn io_error_dispatch_parity() {
    assert_scaffolding_parity(
        call_io_error_trait,
        "IoError",
        v("IoNotFound", vec![s("/nope.txt")]),
        "file not found: /nope.txt",
    );
    // Extra variant check — rendering per-variant must not drift.
    assert_eq!(
        expect_ok(
            call_io_error_trait,
            "message",
            &[v("IoInterrupted", vec![])]
        ),
        "operation interrupted"
    );
    assert_eq!(
        expect_ok(
            call_io_error_trait,
            "message",
            &[v("IoPermissionDenied", vec![s("/etc/shadow")])]
        ),
        "permission denied: /etc/shadow"
    );
    // Unknown variant falls through to the helper's generic phrasing.
    assert_eq!(
        expect_ok(
            call_io_error_trait,
            "message",
            &[v("IoUnknownBogusTag", vec![])]
        ),
        "IoError: unrecognized variant shape `IoUnknownBogusTag`"
    );
}

#[test]
fn json_error_dispatch_parity() {
    assert_scaffolding_parity(
        call_json_error_trait,
        "JsonError",
        v(
            "JsonSyntax",
            vec![s("unexpected token"), Value::Int(42)],
        ),
        "json syntax error at byte 42: unexpected token",
    );
    assert_eq!(
        expect_ok(
            call_json_error_trait,
            "message",
            &[v(
                "JsonTypeMismatch",
                vec![s("string"), s("number")]
            )]
        ),
        "json type mismatch: expected string, got number"
    );
    assert_eq!(
        expect_ok(
            call_json_error_trait,
            "message",
            &[v("JsonMissingField", vec![s("name")])]
        ),
        "json missing field: name"
    );
}

#[test]
fn http_error_dispatch_parity() {
    assert_scaffolding_parity(
        call_http_error_trait,
        "HttpError",
        v("HttpConnect", vec![s("refused")]),
        "http connect failed: refused",
    );
    assert_eq!(
        expect_ok(
            call_http_error_trait,
            "message",
            &[v("HttpTimeout", vec![])]
        ),
        "http request timed out"
    );
    // HttpStatusCode: empty body → no trailing colon-body.
    assert_eq!(
        expect_ok(
            call_http_error_trait,
            "message",
            &[v("HttpStatusCode", vec![Value::Int(404), s("")])]
        ),
        "http status 404"
    );
    assert_eq!(
        expect_ok(
            call_http_error_trait,
            "message",
            &[v(
                "HttpStatusCode",
                vec![Value::Int(500), s("boom")]
            )]
        ),
        "http status 500: boom"
    );
}

#[test]
fn regex_error_dispatch_parity() {
    assert_scaffolding_parity(
        call_regex_error_trait,
        "RegexError",
        v(
            "RegexInvalidPattern",
            vec![s("bad escape"), Value::Int(3)],
        ),
        "invalid regex pattern at position 3: bad escape",
    );
    assert_eq!(
        expect_ok(
            call_regex_error_trait,
            "message",
            &[v("RegexTooBig", vec![])]
        ),
        "compiled regex exceeds size budget"
    );
}

#[test]
fn time_error_dispatch_parity() {
    assert_scaffolding_parity(
        call_time_error_trait,
        "TimeError",
        v("TimeParseFormat", vec![s("invalid format")]),
        "time parse error: invalid format",
    );
    assert_eq!(
        expect_ok(
            call_time_error_trait,
            "message",
            &[v("TimeOutOfRange", vec![s("year 9999")])]
        ),
        "time out of range: year 9999"
    );
}

#[test]
fn toml_error_dispatch_parity() {
    assert_scaffolding_parity(
        call_toml_error_trait,
        "TomlError",
        v("TomlMissingField", vec![s("package")]),
        "toml missing field: package",
    );
    assert_eq!(
        expect_ok(
            call_toml_error_trait,
            "message",
            &[v("TomlSyntax", vec![s("bad token"), Value::Int(10)])]
        ),
        "toml syntax error at byte 10: bad token"
    );
    assert_eq!(
        expect_ok(
            call_toml_error_trait,
            "message",
            &[v("TomlTypeMismatch", vec![s("integer"), s("string")])]
        ),
        "toml type mismatch: expected integer, got string"
    );
    // TomlUnknown returns its contained string verbatim.
    assert_eq!(
        expect_ok(
            call_toml_error_trait,
            "message",
            &[v("TomlUnknown", vec![s("whatever happened")])]
        ),
        "whatever happened"
    );
}

#[test]
fn parse_error_dispatch_parity() {
    assert_scaffolding_parity(
        call_parse_error_trait,
        "ParseError",
        v("ParseEmpty", vec![]),
        "cannot parse empty string",
    );
    assert_eq!(
        expect_ok(
            call_parse_error_trait,
            "message",
            &[v("ParseInvalidDigit", vec![Value::Int(5)])]
        ),
        "invalid digit at byte 5"
    );
    assert_eq!(
        expect_ok(
            call_parse_error_trait,
            "message",
            &[v("ParseOverflow", vec![])]
        ),
        "number too large"
    );
    assert_eq!(
        expect_ok(
            call_parse_error_trait,
            "message",
            &[v("ParseUnderflow", vec![])]
        ),
        "number too small"
    );
}

#[test]
fn bytes_error_dispatch_parity() {
    assert_scaffolding_parity(
        call_bytes_error_trait,
        "BytesError",
        v("BytesInvalidUtf8", vec![Value::Int(7)]),
        "invalid UTF-8 at byte 7",
    );
    assert_eq!(
        expect_ok(
            call_bytes_error_trait,
            "message",
            &[v("BytesInvalidHex", vec![s("not hex")])]
        ),
        "invalid hex: not hex"
    );
    assert_eq!(
        expect_ok(
            call_bytes_error_trait,
            "message",
            &[v("BytesInvalidBase64", vec![s("pad issue")])]
        ),
        "invalid base64: pad issue"
    );
    assert_eq!(
        expect_ok(
            call_bytes_error_trait,
            "message",
            &[v("BytesByteOutOfRange", vec![Value::Int(300)])]
        ),
        "byte value out of range (expected 0..=255): 300"
    );
    assert_eq!(
        expect_ok(
            call_bytes_error_trait,
            "message",
            &[v("BytesOutOfBounds", vec![Value::Int(42)])]
        ),
        "index out of bounds: 42"
    );
}

#[test]
fn channel_error_dispatch_parity() {
    assert_scaffolding_parity(
        call_channel_error_trait,
        "ChannelError",
        v("ChannelTimeout", vec![]),
        "channel receive timed out",
    );
    assert_eq!(
        expect_ok(
            call_channel_error_trait,
            "message",
            &[v("ChannelClosed", vec![])]
        ),
        "channel closed with no more values"
    );
}

#[cfg(feature = "tcp")]
#[test]
fn tcp_error_dispatch_parity() {
    assert_scaffolding_parity(
        call_tcp_error_trait,
        "TcpError",
        v("TcpConnect", vec![s("refused")]),
        "tcp connect failed: refused",
    );
    assert_eq!(
        expect_ok(
            call_tcp_error_trait,
            "message",
            &[v("TcpTls", vec![s("handshake failed")])]
        ),
        "tcp TLS error: handshake failed"
    );
    assert_eq!(
        expect_ok(
            call_tcp_error_trait,
            "message",
            &[v("TcpClosed", vec![])]
        ),
        "tcp connection closed"
    );
    assert_eq!(
        expect_ok(
            call_tcp_error_trait,
            "message",
            &[v("TcpTimeout", vec![])]
        ),
        "tcp operation timed out"
    );
    assert_eq!(
        expect_ok(
            call_tcp_error_trait,
            "message",
            &[v("TcpUnknown", vec![s("surprise")])]
        ),
        "surprise"
    );
}

#[cfg(feature = "postgres")]
#[test]
fn pg_error_dispatch_parity() {
    assert_scaffolding_parity(
        call_pg_error_trait,
        "PgError",
        v("PgConnect", vec![s("refused")]),
        "postgres connect failed: refused",
    );
    // PgQuery with empty sqlstate → no brackets.
    assert_eq!(
        expect_ok(
            call_pg_error_trait,
            "message",
            &[v("PgQuery", vec![s("syntax error"), s("")])]
        ),
        "postgres query error: syntax error"
    );
    assert_eq!(
        expect_ok(
            call_pg_error_trait,
            "message",
            &[v(
                "PgQuery",
                vec![s("unique violation"), s("23505")]
            )]
        ),
        "postgres query error [23505]: unique violation"
    );
    assert_eq!(
        expect_ok(
            call_pg_error_trait,
            "message",
            &[v(
                "PgTypeMismatch",
                vec![s("id"), s("int"), s("string")]
            )]
        ),
        "postgres type mismatch on column `id`: expected int, got string"
    );
    assert_eq!(
        expect_ok(
            call_pg_error_trait,
            "message",
            &[v("PgNoSuchColumn", vec![s("foo")])]
        ),
        "postgres: no such column `foo`"
    );
    assert_eq!(
        expect_ok(
            call_pg_error_trait,
            "message",
            &[v("PgClosed", vec![])]
        ),
        "postgres connection closed"
    );
    assert_eq!(
        expect_ok(
            call_pg_error_trait,
            "message",
            &[v("PgTimeout", vec![])]
        ),
        "postgres operation timed out"
    );
    assert_eq!(
        expect_ok(
            call_pg_error_trait,
            "message",
            &[v("PgTxnAborted", vec![])]
        ),
        "postgres transaction aborted; rollback required"
    );
}

// ── cross-enum lock: helper wiring is symmetric ──────────────────────
//
// If a future edit accidentally swapped two dispatchers' enum_name
// strings (e.g. io passed "JsonError" to the helper), the happy-path
// tests above would still render correctly (they rely on variant
// tags, not enum name) — but the error-path phrasing would leak the
// wrong enum name. This test nails every dispatcher to its own name
// via the "unknown method" error.

#[test]
fn every_dispatcher_reports_its_own_enum_name() {
    let cases: &[(Dispatch, &str)] = &[
        (call_io_error_trait, "IoError"),
        (call_json_error_trait, "JsonError"),
        (call_http_error_trait, "HttpError"),
        (call_regex_error_trait, "RegexError"),
        (call_time_error_trait, "TimeError"),
        (call_toml_error_trait, "TomlError"),
        (call_parse_error_trait, "ParseError"),
        (call_bytes_error_trait, "BytesError"),
        (call_channel_error_trait, "ChannelError"),
        #[cfg(feature = "tcp")]
        (call_tcp_error_trait, "TcpError"),
        #[cfg(feature = "postgres")]
        (call_pg_error_trait, "PgError"),
    ];
    for (dispatch, enum_name) in cases {
        let msg = expect_err(*dispatch, "nope", &[v("Dummy", vec![])]);
        assert_eq!(
            msg,
            format!("unknown {enum_name} trait method: nope"),
            "dispatcher for {enum_name} reported the wrong enum name"
        );
    }
}
