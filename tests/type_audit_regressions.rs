//! Regression tests pinning the type-system audit fixes shipped alongside
//! this file. Each test was written to FAIL against the pre-fix codebase
//! and to PASS after the corresponding fix in `src/typechecker/*.rs`.
//!
//! These live in their own file (rather than `tests/error_tests.rs`) to
//! avoid edit collisions with the broader test-coverage strengthening
//! happening in parallel.

use silt::lexer::Span;
use silt::typechecker;
use silt::types::Severity;

// ── Helpers ─────────────────────────────────────────────────────────

fn type_errors(input: &str) -> Vec<String> {
    let tokens = silt::lexer::Lexer::new(input)
        .tokenize()
        .expect("lexer error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    let errors = typechecker::check(&mut program);
    errors
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

fn type_errors_full(input: &str) -> Vec<(String, Span)> {
    let tokens = silt::lexer::Lexer::new(input)
        .tokenize()
        .expect("lexer error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    let errors = typechecker::check(&mut program);
    errors
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| (e.message, e.span))
        .collect()
}

fn assert_type_error(input: &str, pattern: &str) {
    let errs = type_errors(input);
    assert!(
        errs.iter().any(|e| e.contains(pattern)),
        "expected type error containing '{pattern}', got: {errs:?}"
    );
}

// ── BROKEN-1: RecordUpdate on Type::Generic base ───────────────────

#[test]
fn test_record_update_unknown_field_on_param_rejected() {
    // When a RecordUpdate's base was a function parameter, its type
    // surfaced to inference as `Type::Generic("Config", [])` and the
    // unknown-field branch silently accepted every field, dropping the
    // `nonexistent: ...` write at runtime.
    assert_type_error(
        r#"
type Config { host: String, port: Int }
fn update(c: Config) -> Config { c.{ nonexistent: "bogus", port: 9090 } }
fn main() {
  let c = Config { host: "h", port: 80 }
  let c2 = update(c)
  println("{c2.port} {c2.host}")
}
"#,
        "unknown field 'nonexistent'",
    );
}

// ── BROKEN-2: RecordUpdate on non-record base ──────────────────────

#[test]
fn test_record_update_unknown_field_on_non_record_rejected_at_typecheck() {
    // `(42).{ bogus: 1 }` previously type-checked and exploded only at
    // runtime. It must now be a compile-time error at the base expr.
    assert_type_error(
        r#"
fn main() { let y = (42).{ bogus: 1 } println("{y}") }
"#,
        "record update",
    );
}

// ── BROKEN-3: match Pattern::Record unknown field ───────────────────

#[test]
fn test_match_pattern_unknown_record_field_rejected() {
    assert_type_error(
        r#"
type Point { x: Int, y: Int }
fn check(p: Point) -> String {
  match p {
    Point { z: 5 } -> "matched z"
    _ -> "fallback"
  }
}
fn main() { println(check(Point { x: 1, y: 2 })) }
"#,
        "no field 'z'",
    );
}

// ── BROKEN-4: let-destructure Pattern::Record ───────────────────────

#[test]
fn test_let_destructure_unknown_record_field_rejected() {
    assert_type_error(
        r#"
type User { name: String, age: Int }
fn main() {
  let u = User { name: "a", age: 10 }
  let User { nonexistent } = u
  println(nonexistent)
}
"#,
        "no field 'nonexistent'",
    );
}

#[test]
fn test_let_destructure_record_pattern_on_non_record_rejected() {
    // `let NotDeclared { x } = 42` uses a record pattern against a
    // non-record value — rejected at compile time with at least one
    // diagnostic mentioning either the undefined record type or that
    // a record pattern needs a record value.
    let errs = type_errors(
        r#"
fn main() {
  let NotDeclared { x } = 42
  println(x)
}
"#,
    );
    // Both diagnostics fire: one for the undefined record type, one for the
    // record-pattern-on-non-record mismatch. Assert both exact phrases.
    assert!(
        errs.iter()
            .any(|e| e.contains("undefined record type 'NotDeclared' in pattern")),
        "expected undefined-record-type-'NotDeclared' diagnostic, got: {errs:?}"
    );
    assert!(
        errs.iter().any(|e| e
            .contains("record pattern requires a record value, but 'Int' is not a record type")),
        "expected record-pattern-on-non-record diagnostic, got: {errs:?}"
    );
}

// ── GAP-1: where-clause on function value ───────────────────────────

#[test]
fn test_where_display_rejects_function_value() {
    // `type_name_for_impl` used to return None for Type::Fun, which
    // silently skipped the trait_impl_set check and let `show(f)`
    // through. It now resolves to "Fun", which has no trait impls, so
    // the error fires.
    assert_type_error(
        r#"
fn show(x: a) -> String where a: Display { "{x}" }
fn main() {
  let f = fn() { 42 }
  println(show(f))
}
"#,
        "Display",
    );
}

// ── GAP-2: trait impl missing-method diagnostic span ────────────────

#[test]
fn test_trait_impl_missing_method_has_real_span() {
    // The "missing method" diagnostic previously used Span::new(0, 0)
    // when the impl block had no methods to borrow a span from. It
    // must now be reported at the impl block's real location (non-zero
    // line number).
    let errs = type_errors_full(
        r#"
trait Foo {
  fn a(self) -> Int
  fn b(self) -> Int
}
trait Foo for Int { }
fn main() { 42 }
"#,
    );
    let missing: Vec<_> = errs
        .iter()
        .filter(|(m, _)| m.contains("missing method"))
        .collect();
    assert!(
        !missing.is_empty(),
        "expected at least one 'missing method' error, got: {errs:?}"
    );
    for (_, span) in &missing {
        assert!(
            span.line > 0,
            "missing-method diagnostic has sentinel span (line 0): {missing:?}"
        );
    }
}
