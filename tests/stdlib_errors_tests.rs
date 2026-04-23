//! Phase 0 of the stdlib error redesign: verify the six typed error
//! enums are registered correctly as ordinary silt types.
//!
//! Design reference: `docs/proposals/stdlib-errors.md`.
//! User-facing reference: `docs/stdlib/errors.md`.
//!
//! These tests cover the three invariants the proposal requires for
//! Phase 0:
//!
//! 1. Construction. Each variant is constructible both as a bare name
//!    (`IoNotFound("x")`) and as a module-qualified call
//!    (`IoError.IoNotFound("x")`), producing the same `Value::Variant`.
//! 2. Exhaustiveness. A `match` covering every variant of an error
//!    enum must type-check clean — the typechecker knows the full
//!    variant set.
//! 3. Variant isolation. Each variant name maps to exactly one enum.
//!    Cross-enum collisions (e.g. mistaking `IoNotFound` for an
//!    `HttpError` variant) must produce a type error.

use std::sync::Arc;

use silt::types::Severity;
use silt::value::Value;

fn run(input: &str) -> Value {
    let tokens = silt::lexer::Lexer::new(input)
        .tokenize()
        .expect("lex error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = silt::compiler::Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = silt::vm::Vm::new();
    vm.run(script).expect("runtime error")
}

fn type_errors(input: &str) -> Vec<String> {
    let tokens = silt::lexer::Lexer::new(input)
        .tokenize()
        .expect("lex error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    let errors = silt::typechecker::check(&mut program);
    errors
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

fn expect_variant(v: Value) -> (String, Vec<Value>) {
    match v {
        Value::Variant(name, fields) => (name, fields),
        other => panic!("expected Variant, got {other:?}"),
    }
}

// ── (a) Construction — bare form and qualified form ─────────────────

#[test]
fn test_io_error_bare_construction() {
    let v = run(r#"
        import io
        fn main() -> IoError { IoNotFound("config.toml") }
    "#);
    let (name, fields) = expect_variant(v);
    assert_eq!(name, "IoNotFound");
    assert_eq!(fields.len(), 1);
    assert!(matches!(&fields[0], Value::String(s) if s == "config.toml"));
}

#[test]
fn test_io_error_qualified_construction() {
    let v = run(r#"
        import io
        fn main() -> IoError { IoError.IoNotFound("config.toml") }
    "#);
    let (name, fields) = expect_variant(v);
    // Qualified access resolves to the bare global, so the stored
    // variant name is the variant's own name — not "IoError.IoNotFound".
    assert_eq!(name, "IoNotFound");
    assert_eq!(fields.len(), 1);
    assert!(matches!(&fields[0], Value::String(s) if s == "config.toml"));
}

#[test]
fn test_bare_and_qualified_equal() {
    // Constructing via bare and qualified forms must yield equal values.
    let v = run(r#"
        import io
        fn main() -> Bool {
          let a = IoNotFound("x")
          let b = IoError.IoNotFound("x")
          a == b
        }
    "#);
    assert!(matches!(v, Value::Bool(true)));
}

#[test]
fn test_nullary_variant_is_value() {
    // Nullary variants register as `Value::Variant` directly, not as a
    // constructor.
    let v = run(r#"
        import io
        fn main() -> IoError { IoInterrupted }
    "#);
    let (name, fields) = expect_variant(v);
    assert_eq!(name, "IoInterrupted");
    assert!(fields.is_empty());
}

#[test]
fn test_each_enum_has_one_constructor_check() {
    // Smoke-check one variant per error enum so a registration typo in
    // any of the six taxonomies fails loudly, not just in `io`.
    let v = run(r#"
        import io
        import json
        import toml
        import int
        import http
        import regex
        fn main() -> Bool {
          let _io = IoNotFound("p")
          let _json = JsonSyntax("m", 0)
          let _toml = TomlSyntax("m", 0)
          let _parse = ParseEmpty
          let _http = HttpConnect("host")
          let _regex = RegexTooBig
          true
        }
    "#);
    assert!(matches!(v, Value::Bool(true)));
}

// ── (b) Exhaustiveness — full-variant matches warn-free ─────────────

#[test]
fn test_io_error_exhaustive_match_is_clean() {
    let errs = type_errors(
        r#"
        import io
        fn describe(e: IoError) -> String {
          match e {
            IoNotFound(p) -> "not found"
            IoPermissionDenied(p) -> "denied"
            IoAlreadyExists(p) -> "exists"
            IoInvalidInput(p) -> "invalid"
            IoInterrupted -> "interrupted"
            IoUnexpectedEof -> "eof"
            IoWriteZero -> "write zero"
            IoUnknown(m) -> "unknown"
          }
        }
        fn main() { println(describe(IoNotFound("x"))) }
    "#,
    );
    assert!(
        errs.is_empty(),
        "exhaustive IoError match produced errors: {errs:?}"
    );
}

#[test]
fn test_io_error_missing_variant_is_non_exhaustive() {
    // Sanity check the other direction — dropping `IoUnknown` must
    // trigger a non-exhaustive diagnostic, proving the typechecker
    // genuinely sees IoError's full variant set.
    let errs = type_errors(
        r#"
        import io
        fn describe(e: IoError) -> String {
          match e {
            IoNotFound(p) -> "not found"
            IoPermissionDenied(p) -> "denied"
            IoAlreadyExists(p) -> "exists"
            IoInvalidInput(p) -> "invalid"
            IoInterrupted -> "interrupted"
            IoUnexpectedEof -> "eof"
            IoWriteZero -> "write zero"
          }
        }
        fn main() { println(describe(IoNotFound("x"))) }
    "#,
    );
    assert!(
        errs.iter().any(|m| m.contains("non-exhaustive")),
        "expected non-exhaustive diagnostic, got {errs:?}"
    );
    assert!(
        errs.iter().any(|m| m.contains("IoUnknown")),
        "expected the missing variant 'IoUnknown' named in the diagnostic, got {errs:?}"
    );
}

#[test]
fn test_parse_error_exhaustive_match_is_clean() {
    let errs = type_errors(
        r#"
        import int
        fn describe(e: ParseError) -> String {
          match e {
            ParseEmpty -> "empty"
            ParseInvalidDigit(off) -> "bad digit"
            ParseOverflow -> "overflow"
            ParseUnderflow -> "underflow"
          }
        }
        fn main() { println(describe(ParseEmpty)) }
    "#,
    );
    assert!(
        errs.is_empty(),
        "exhaustive ParseError match produced errors: {errs:?}"
    );
}

#[test]
fn test_http_error_exhaustive_match_is_clean() {
    let errs = type_errors(
        r#"
        import http
        fn describe(e: HttpError) -> String {
          match e {
            HttpConnect(m) -> "connect"
            HttpTls(m) -> "tls"
            HttpTimeout -> "timeout"
            HttpInvalidUrl(u) -> "url"
            HttpInvalidResponse(m) -> "bad resp"
            HttpClosedEarly -> "closed"
            HttpStatusCode(s, b) -> "status"
            HttpUnknown(m) -> "unknown"
          }
        }
        fn main() { println(describe(HttpTimeout)) }
    "#,
    );
    assert!(
        errs.is_empty(),
        "exhaustive HttpError match produced errors: {errs:?}"
    );
}

// ── (c) Cross-enum collision — IoNotFound is IoError, not HttpError ─

#[test]
fn test_io_variant_is_not_http_error() {
    // Constructing `IoNotFound(...)` must yield a value whose type is
    // `IoError`. Annotating the local as `HttpError` must be rejected.
    let errs = type_errors(
        r#"
        import io
        fn main() {
          let e: HttpError = IoNotFound("x")
          println("unreachable")
        }
    "#,
    );
    assert!(
        !errs.is_empty(),
        "expected a type error for IoNotFound : HttpError; got none"
    );
}

#[test]
fn test_io_variant_is_not_json_error() {
    let errs = type_errors(
        r#"
        import io
        fn main() {
          let e: JsonError = IoNotFound("x")
          println("unreachable")
        }
    "#,
    );
    assert!(
        !errs.is_empty(),
        "expected a type error for IoNotFound : JsonError; got none"
    );
}

#[test]
fn test_match_on_io_does_not_accept_http_variant() {
    // Scrutinee is `IoError`; pattern `HttpConnect(_)` belongs to a
    // different enum and must be rejected.
    let errs = type_errors(
        r#"
        import io
        import http
        fn describe(e: IoError) -> String {
          match e {
            HttpConnect(m) -> "nope"
            IoNotFound(p) -> "ok"
            _ -> "other"
          }
        }
        fn main() { println(describe(IoNotFound("x"))) }
    "#,
    );
    assert!(
        !errs.is_empty(),
        "expected a type error for cross-enum pattern; got none"
    );
}

// ── Gating — bare construction without import must fail ────────────

#[test]
fn test_gated_variant_without_import_fails_compile() {
    // The compiler must refuse `IoNotFound(...)` unless `import io`
    // appears in the file. We rely on the compile-step erroring, so we
    // cannot use `run` (which expects a clean compile). Check via the
    // full pipeline and look for a compile error.
    let tokens = silt::lexer::Lexer::new(
        r#"
        fn main() { let _ = IoNotFound("x") }
    "#,
    )
    .tokenize()
    .expect("lex error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = silt::compiler::Compiler::new();
    let result = compiler.compile_program(&program);
    assert!(
        result.is_err(),
        "expected compile error for bare IoNotFound without import io"
    );
    let err = format!("{:?}", result.unwrap_err());
    // Narrow lock: the gating diagnostic emitted at src/compiler/mod.rs
    // (ExprKind::Ident arm) is the format string
    //     "'{name}' requires `import {required}`"
    // For IoNotFound without `import io`, this renders as:
    //     'IoNotFound' requires `import io`
    // The previous OR (`import io` || `IoNotFound`) let `IoNotFound`
    // trivially match any "unknown identifier" regression. Pin the
    // exact gate-message fragment that proves the gate (not some
    // unrelated error path) is what fired.
    assert!(
        err.contains("requires `import io`"),
        "error should be the gating diagnostic ``requires `import io```; got {err}"
    );
}
