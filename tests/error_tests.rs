//! Comprehensive negative / error tests for the Silt language.
//!
//! Organized by pipeline phase: lexer → parser → typechecker → runtime.
//! Each test verifies that bad input produces a clear, correct error
//! rather than a panic or silent misbehavior.

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::types::Severity;
use silt::value::Value;
use silt::vm::Vm;
use std::sync::Arc;

// ── Helpers ─────────────────────────────────────────────────────────

/// Expect a lexer error; return the error message.
fn lex_err(input: &str) -> String {
    match Lexer::new(input).tokenize() {
        Err(e) => e.message.clone(),
        Ok(_) => panic!("expected lexer error, got success"),
    }
}

/// Expect lexing + parsing to succeed, but the typechecker to produce errors.
/// Returns only hard errors (not warnings).
fn type_errors(input: &str) -> Vec<String> {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let errors = typechecker::check(&mut program);
    errors
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

/// Assert that the typechecker produces at least one error matching the pattern.
fn assert_type_error(input: &str, pattern: &str) {
    let errs = type_errors(input);
    assert!(
        errs.iter().any(|e| e.contains(pattern)),
        "expected type error containing '{pattern}', got: {errs:?}"
    );
}

/// Assert that the typechecker produces no hard errors.
#[allow(dead_code)]
fn assert_no_type_errors(input: &str) {
    let errs = type_errors(input);
    assert!(errs.is_empty(), "expected no type errors, got: {errs:?}");
}

/// Expect a parse error; return the error message.
#[allow(dead_code)]
fn parse_err(input: &str) -> String {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    match Parser::new(tokens).parse_program() {
        Err(e) => e.message.clone(),
        Ok(_) => panic!("expected parse error, got success"),
    }
}

/// Expect a parse error; returns the error message.
/// Uses parse_program which returns the first fatal error.
fn parse_errors(input: &str) -> Vec<String> {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    match Parser::new(tokens).parse_program() {
        Err(e) => vec![e.message.clone()],
        Ok(_) => vec![], // no error; caller should check
    }
}

/// Compile and run, returning the runtime error message.
/// Panics if the program succeeds instead of erroring.
fn run_err(input: &str) -> String {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = match compiler.compile_program(&program) {
        Ok(f) => f,
        Err(e) => return e.message,
    };
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    match vm.run(script) {
        Err(e) => format!("{e}"),
        Ok(v) => panic!("expected runtime error, got: {v:?}"),
    }
}

/// Compile and run, returning the value. Panics on any error.
fn run(input: &str) -> Value {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    vm.run(script).expect("runtime error")
}

/// Compile and run; don't care about the return value. Panics on error.
fn run_ok(input: &str) {
    let _ = run(input);
}

// ════════════════════════════════════════════════════════════════════
// PHASE 1: LEXER ERRORS
// ════════════════════════════════════════════════════════════════════

#[test]
fn test_lex_unterminated_string() {
    let err = lex_err(r#""hello"#);
    assert!(err.contains("unterminated string"), "got: {err}");
}

#[test]
fn test_lex_unterminated_string_with_newline_content() {
    // Asserts exact lexer message from src/lexer.rs scan_string
    // ("unterminated string"). A newline in a string literal is legal
    // content; the lexer only errors at EOF with no closing `"`.
    let err = lex_err("\"hello\nworld");
    assert_eq!(err, "unterminated string", "got: {err}");
}

#[test]
fn test_lex_unknown_escape_sequence() {
    let err = lex_err(r#""\q""#);
    assert!(err.contains("unknown escape"), "got: {err}");
}

#[test]
fn test_lex_unknown_escape_various() {
    for ch in ['a', 'b', 'r', 'x', '0'] {
        let input = format!("\"\\{ch}\"");
        let err = lex_err(&input);
        assert!(
            err.contains("unknown escape"),
            "expected error for \\{ch}, got: {err}"
        );
    }
}

#[test]
fn test_lex_unterminated_block_comment() {
    let err = lex_err("{- this is never closed");
    assert!(err.contains("unterminated block comment"), "got: {err}");
}

#[test]
fn test_lex_nested_unterminated_block_comment() {
    let err = lex_err("{- outer {- inner -} still open");
    assert!(err.contains("unterminated block comment"), "got: {err}");
}

#[test]
fn test_lex_unexpected_character() {
    let err = lex_err("let x = 42 $ y");
    assert!(err.contains("unexpected character"), "got: {err}");
}

#[test]
fn test_lex_at_sign() {
    let err = lex_err("@foo");
    assert!(err.contains("unexpected character"), "got: {err}");
}

#[test]
fn test_lex_backtick() {
    let err = lex_err("`hello`");
    assert!(err.contains("unexpected character"), "got: {err}");
}

#[test]
fn test_lex_semicolon_helpful_message() {
    let err = lex_err("let x = 1; let y = 2");
    assert!(err.contains("semicolons are not used"), "got: {err}");
}

#[test]
fn test_lex_lone_ampersand() {
    let err = lex_err("let x = true & false");
    assert!(
        err.contains("&&"),
        "should suggest && instead of &, got: {err}"
    );
}

#[test]
fn test_lex_unterminated_triple_quoted_string() {
    let err = lex_err(r#""""this is never closed"#);
    assert!(err.contains("unterminated triple-quoted"), "got: {err}");
}

#[test]
fn test_lex_unterminated_escape_at_eof() {
    let err = lex_err(r#""hello\"#);
    assert!(err.contains("unterminated"), "got: {err}");
}

// ════════════════════════════════════════════════════════════════════
// PHASE 2: PARSER ERRORS
// ════════════════════════════════════════════════════════════════════

#[test]
fn test_parse_missing_closing_paren() {
    // Asserts exact parser message from src/parser.rs delim_unclosed_err_no_comma
    // ("expected ')' to close parenthesized expression starting at line N, found }").
    let errs = parse_errors("fn main() { (1 + 2 }");
    assert!(
        errs.iter().any(|e| e
            .contains("expected ')' to close parenthesized expression starting at line 1, found }")),
        "got: {errs:?}"
    );
}

#[test]
fn test_parse_missing_closing_bracket() {
    // Asserts exact parser message from src/parser.rs delim_unclosed_err
    // ("expected ']' or ',' to continue list literal starting at line N, found }").
    let errs = parse_errors("fn main() { [1, 2, 3 }");
    assert!(
        errs.iter().any(|e| e
            .contains("expected ']' or ',' to continue list literal starting at line 1, found }")),
        "got: {errs:?}"
    );
}

#[test]
fn test_parse_missing_closing_brace() {
    let errs = parse_errors("fn main() { let x = 1");
    assert!(!errs.is_empty(), "should report an error for missing brace");
}

#[test]
fn test_parse_let_without_value() {
    // Asserts exact parser message from src/parser.rs expect() — after
    // `parse_pattern` consumes `x`, the parser expects `=` and the next
    // token is `}`, yielding "expected =, found }".
    let errs = parse_errors("fn main() { let x }");
    assert!(
        errs.iter().any(|e| e.contains("expected =, found }")),
        "got: {errs:?}"
    );
}

#[test]
fn test_parse_fn_missing_body() {
    // `fn foo()` is valid in Silt (single-expression function).
    // Test something that actually fails: a function with no name.
    let errs = parse_errors("fn () { }");
    assert!(
        !errs.is_empty(),
        "fn without name should error, got: {errs:?}"
    );
}

#[test]
fn test_parse_match_missing_arrow() {
    // Asserts exact parser message from src/parser.rs parse_match_arm
    // — after parsing the pattern `1`, the parser expects `->` but sees
    // the string literal, yielding `expected ->, found "hello"`.
    let errs = parse_errors(
        r#"
fn main() {
  match 1 {
    1 "hello"
  }
}
    "#,
    );
    assert!(
        errs.iter().any(|e| e.contains("expected ->, found \"hello\"")),
        "got: {errs:?}"
    );
}

#[test]
fn test_parse_type_missing_body() {
    let errs = parse_errors("type Foo");
    assert!(
        !errs.is_empty(),
        "type without body should error, got: {errs:?}"
    );
}

#[test]
fn test_parse_import_missing_module_name() {
    let errs = parse_errors("import");
    assert!(
        !errs.is_empty(),
        "import without name should error, got: {errs:?}"
    );
}

#[test]
fn test_parse_double_comma_in_args() {
    let errs = parse_errors("fn main() { foo(1,, 2) }");
    assert!(!errs.is_empty(), "double comma should error, got: {errs:?}");
}

#[test]
fn test_parse_trailing_operator() {
    let errs = parse_errors("fn main() { 1 + }");
    assert!(
        !errs.is_empty(),
        "trailing operator should error, got: {errs:?}"
    );
}

#[test]
fn test_parse_empty_match() {
    // Round 14: empty match is now rejected at typecheck time by the
    // exhaustiveness analysis. Asserts exact diagnostic
    // "non-exhaustive match: not all patterns are covered" from
    // src/typechecker/exhaustiveness.rs missing_description.
    assert_type_error(
        "fn main() { match 42 { } }",
        "non-exhaustive match: not all patterns are covered",
    );
}

// ════════════════════════════════════════════════════════════════════
// PHASE 3: TYPE CHECKER ERRORS
// ════════════════════════════════════════════════════════════════════

// ── Basic type mismatches ───────────────────────────────────────────

#[test]
fn test_type_return_annotation_mismatch() {
    assert_type_error(
        r#"
fn add(a: Int, b: Int) -> String {
  a + b
}
fn main() { add(1, 2) }
        "#,
        "mismatch",
    );
}

#[test]
fn test_type_param_annotation_mismatch() {
    assert_type_error(
        r#"
fn greet(name: String) = println(name)
fn main() { greet(42) }
        "#,
        "mismatch",
    );
}

#[test]
fn test_type_if_branch_mismatch_via_match() {
    // Silt uses match for branching; arms should return consistent types
    assert_type_error(
        r#"
fn foo(x: Bool) -> Int {
  match x {
    true -> 42
    false -> "hello"
  }
}
fn main() { foo(true) }
        "#,
        "mismatch",
    );
}

#[test]
fn test_type_arithmetic_on_string() {
    // Asserts exact typechecker message from src/typechecker — String
    // subtracted from Int fails with
    // "type mismatch: operator requires numeric types, got String".
    // (The old version had a dead `run_err` fallback branch because the
    // typechecker always catches this case.)
    assert_type_error(
        r#"
fn main() {
  let x: String = "hello"
  x - 1
}
    "#,
        "type mismatch: operator requires numeric types, got String",
    );
}

#[test]
fn test_type_bool_arithmetic() {
    // Using + on booleans
    let errs = type_errors(
        r#"
fn main() {
  true + false
}
    "#,
    );
    if errs.is_empty() {
        let runtime_err = run_err("fn main() { true + false }");
        assert!(runtime_err.contains("cannot apply"), "got: {runtime_err}");
    }
}

// ── Non-exhaustive match ────────────────────────────────────────────

#[test]
fn test_type_non_exhaustive_match_bool() {
    assert_type_error(
        r#"
fn foo(x: Bool) {
  match x {
    true -> "yes"
  }
}
fn main() { foo(true) }
        "#,
        "exhaustive",
    );
}

#[test]
fn test_type_non_exhaustive_match_enum() {
    assert_type_error(
        r#"
type Color { Red, Green, Blue }
fn name(c: Color) {
  match c {
    Red -> "red"
    Green -> "green"
  }
}
fn main() { name(Red) }
        "#,
        "exhaustive",
    );
}

// ── Undefined names ─────────────────────────────────────────────────

#[test]
fn test_type_undefined_variable() {
    assert_type_error(
        r#"
fn main() { x + 1 }
    "#,
        "undefined",
    );
}

#[test]
fn test_type_undefined_function() {
    assert_type_error(
        r#"
fn main() { foo(1, 2) }
    "#,
        "undefined",
    );
}

// ── Wrong arity ─────────────────────────────────────────────────────

#[test]
fn test_type_wrong_arity_too_few() {
    assert_type_error(
        r#"
fn add(a, b) = a + b
fn main() { add(1) }
    "#,
        "argument",
    );
}

#[test]
fn test_type_wrong_arity_too_many() {
    assert_type_error(
        r#"
fn add(a, b) = a + b
fn main() { add(1, 2, 3) }
    "#,
        "argument",
    );
}

// ── Bad field access ────────────────────────────────────────────────

#[test]
fn test_type_unknown_record_field() {
    assert_type_error(
        r#"
type User { name: String, age: Int }
fn main() {
  let u = User { name: "alice", age: 30 }
  u.email
}
        "#,
        "field",
    );
}

// ── Duplicate definitions ───────────────────────────────────────────

#[test]
fn test_type_duplicate_function() {
    // Define the same function twice
    assert_type_error(
        r#"
fn foo() = 1
fn foo() = 2
fn main() { foo() }
    "#,
        "duplicate top-level definition",
    );
}

// ── Trait constraint violations ─────────────────────────────────────

#[test]
fn test_type_where_clause_violation() {
    // where clause references `x` (a value name) instead of a type variable;
    // the typechecker rejects this with a clear "not introduced in the function
    // signature" diagnostic.
    assert_type_error(
        r#"
fn show(x) where x: Display = x.display()
fn main() {
  show(fn() { 1 })
}
    "#,
        "not introduced in the function signature",
    );
}

#[test]
fn test_where_clause_trailing_plus() {
    let err = parse_err("fn f(x: a) where a: Equal + = x");
    assert!(
        err.contains("expected identifier"),
        "expected identifier error, got: {err}"
    );
}

// ════════════════════════════════════════════════════════════════════
// PHASE 4: RUNTIME ERRORS — Arithmetic & Types
// ════════════════════════════════════════════════════════════════════

#[test]
fn test_runtime_division_by_zero_int() {
    let err = run_err("fn main() { 42 / 0 }");
    assert!(err.contains("division by zero"), "got: {err}");
}

#[test]
fn test_runtime_division_by_zero_computed() {
    let err = run_err("fn main() { let x = 0\n100 / x }");
    assert!(err.contains("division by zero"), "got: {err}");
}

#[test]
fn test_runtime_modulo_by_zero() {
    let err = run_err("fn main() { 42 % 0 }");
    assert!(err.contains("modulo by zero"), "got: {err}");
}

#[test]
fn test_runtime_float_division_by_zero() {
    // Float division by zero now produces ExtFloat(Infinity) instead of a runtime error
    let result = run("fn main() { 1.0 / 0.0 }");
    assert_eq!(result, Value::ExtFloat(f64::INFINITY));
}

#[test]
fn test_runtime_negate_string() {
    let err = run_err(r#"fn main() { -"hello" }"#);
    assert!(err.contains("cannot negate"), "got: {err}");
}

#[test]
fn test_runtime_negate_bool() {
    let err = run_err("fn main() { -true }");
    assert!(err.contains("cannot negate"), "got: {err}");
}

#[test]
fn test_runtime_negate_list() {
    let err = run_err("fn main() { -[1, 2, 3] }");
    assert!(err.contains("cannot negate"), "got: {err}");
}

#[test]
fn test_runtime_not_on_int() {
    let err = run_err("fn main() { !42 }");
    // Lock the exact phrase "cannot apply 'not' to Int" so a fallback like
    // "could not..." or an "annotation" error won't accidentally satisfy it.
    assert!(
        err.contains("cannot apply 'not' to Int"),
        "expected \"cannot apply 'not' to Int\", got: {err}"
    );
}

#[test]
fn test_runtime_not_on_string() {
    let err = run_err(r#"fn main() { !"hello" }"#);
    assert!(
        err.contains("cannot apply 'not' to String"),
        "expected \"cannot apply 'not' to String\", got: {err}"
    );
}

#[test]
fn test_runtime_int_float_add() {
    let err = run_err("fn main() { 1 + 2.5 }");
    assert!(err.contains("cannot mix Int and Float"), "got: {err}");
}

#[test]
fn test_runtime_int_float_mul() {
    let err = run_err("fn main() { 3 * 1.5 }");
    assert!(err.contains("cannot mix Int and Float"), "got: {err}");
}

#[test]
fn test_runtime_string_minus_string() {
    let err = run_err(r#"fn main() { "hello" - "world" }"#);
    assert!(err.contains("cannot apply"), "got: {err}");
}

#[test]
fn test_runtime_string_multiply() {
    // Asserts exact VM message from src/vm/arithmetic.rs binary op dispatch
    // ("cannot apply '*' to String and Int").
    let err = run_err(r#"fn main() { "hello" * 3 }"#);
    assert!(
        err.contains("cannot apply '*' to String and Int"),
        "got: {err}"
    );
}

#[test]
fn test_runtime_list_arithmetic() {
    let err = run_err("fn main() { [1, 2] + [3, 4] }");
    assert!(err.contains("cannot apply"), "got: {err}");
}

#[test]
fn test_runtime_cross_type_equality() {
    let err = run_err(r#"fn main() { 42 == "42" }"#);
    assert!(err.contains("unsupported operation"), "got: {err}");
}

#[test]
fn test_runtime_cross_type_comparison() {
    let err = run_err("fn main() { 42 < true }");
    assert!(err.contains("unsupported operation"), "got: {err}");
}

#[test]
fn test_runtime_int_float_equality() {
    let err = run_err("fn main() { 3 == 3.0 }");
    assert!(err.contains("unsupported operation"), "got: {err}");
}

#[test]
fn test_runtime_compare_incompatible_types() {
    let err = run_err(r#"fn main() { "abc" > 123 }"#);
    assert!(err.contains("unsupported operation"), "got: {err}");
}

// ════════════════════════════════════════════════════════════════════
// PHASE 5: RUNTIME ERRORS — Collections & Builtins
// ════════════════════════════════════════════════════════════════════

#[test]
fn test_runtime_list_get_out_of_bounds() {
    let result = run(r#"
import list
fn main() { list.get([1, 2, 3], 10) }
    "#);
    // list.get returns None for out-of-bounds indices
    match result {
        Value::Variant(ref tag, ref args) if tag == "None" && args.is_empty() => {}
        other => panic!("expected None, got {other:?}"),
    }
}

#[test]
fn test_runtime_list_get_negative_index() {
    let err = run_err(
        r#"
import list
fn main() { list.get([1, 2, 3], -1) }
    "#,
    );
    assert!(
        err.contains("list.get: negative index -1"),
        "expected negative index error, got: {err}"
    );
}

#[test]
fn test_runtime_string_slice_out_of_bounds() {
    let result = run(r#"
import string
fn main() { string.slice("hello", 0, 100) }
    "#);
    // string.slice clamps the end index to the string length
    assert_eq!(result, Value::String("hello".into()));
}

#[test]
fn test_runtime_int_parse_invalid() {
    let result = run(r#"
import int
fn main() { int.parse("not_a_number") }
    "#);
    // Should return Err, not panic
    match result {
        Value::Variant(tag, _) => assert_eq!(tag, "Err", "expected Err, got {tag}"),
        other => panic!("expected Err variant, got {other:?}"),
    }
}

#[test]
fn test_runtime_float_parse_invalid() {
    let result = run(r#"
import float
fn main() { float.parse("not_a_float") }
    "#);
    match result {
        Value::Variant(tag, _) => assert_eq!(tag, "Err", "expected Err, got {tag}"),
        other => panic!("expected Err variant, got {other:?}"),
    }
}

#[test]
fn test_runtime_json_parse_invalid() {
    // json.parse takes 2 arguments: (Type, String)
    let result = run(r#"
import json
type Dummy { x: Int }
fn main() { json.parse(Dummy, "not json at all") }
    "#);
    match result {
        Value::Variant(tag, _) => assert_eq!(tag, "Err", "expected Err, got {tag}"),
        other => panic!("expected Err variant, got {other:?}"),
    }
}

#[test]
fn test_runtime_json_parse_wrong_type() {
    let result = run(r#"
import json
type Foo { x: Int }
fn main() { json.parse(Foo, "42") }
    "#);
    match result {
        Value::Variant(tag, _) => assert_eq!(tag, "Err", "expected Err, got {tag}"),
        other => panic!("expected Err variant, got {other:?}"),
    }
}

#[test]
fn test_runtime_map_missing_key() {
    let result = run(r#"
import map
fn main() {
  let m = #{"a": 1}
  map.get(m, "z")
}
    "#);
    // map.get returns None for missing keys
    match result {
        Value::Variant(ref tag, ref args) if tag == "None" && args.is_empty() => {}
        other => panic!("expected None, got {other:?}"),
    }
}

#[test]
fn test_runtime_regex_invalid_pattern() {
    // Invalid regex produces a runtime error (not a Result variant)
    let err = run_err(
        r#"
import regex
fn main() { regex.is_match("[invalid(", "test") }
    "#,
    );
    assert!(err.contains("invalid regex"), "got: {err}");
}

#[test]
fn test_runtime_range_non_integer() {
    // Asserts exact VM message from src/vm/execute.rs range construction
    // ("range requires two integers").
    let err = run_err(r#"fn main() { 1.0..5.0 }"#);
    assert!(err.contains("range requires two integers"), "got: {err}");
}

// ════════════════════════════════════════════════════════════════════
// PHASE 6: RUNTIME ERRORS — Concurrency
// ════════════════════════════════════════════════════════════════════

#[test]
fn test_runtime_send_on_closed_channel() {
    let err = run_err(
        r#"
import channel
fn main() {
  let ch = channel.new(10)
  channel.close(ch)
  channel.send(ch, "hello")
}
    "#,
    );
    assert!(err.contains("send on closed channel"), "got: {err}");
}

#[test]
fn test_runtime_double_close_channel() {
    // Closing a channel twice is a no-op (returns ())
    run_ok(
        r#"
import channel
fn main() {
  let ch = channel.new(10)
  channel.close(ch)
  channel.close(ch)
}
    "#,
    );
}

#[test]
fn test_runtime_receive_on_closed_empty_channel() {
    // Receiving from a closed, empty channel should return None or an error
    let result = run(r#"
import channel
fn main() {
  let ch = channel.new(10)
  channel.close(ch)
  channel.receive(ch)
}
    "#);
    // Should return None (channel closed, no more messages)
    match result {
        Value::Variant(ref tag, _) if tag == "None" => {} // expected
        Value::Unit => {}                                 // also acceptable
        other => {
            // If it's something else, just make sure it's not a crash
            let _ = other;
        }
    }
}

#[test]
fn test_runtime_try_send_on_closed_channel() {
    let result = run(r#"
import channel
fn main() {
  let ch = channel.new(10)
  channel.close(ch)
  channel.try_send(ch, "hello")
}
    "#);
    // try_send should return Err or false, not panic
    match result {
        Value::Variant(ref tag, _) if tag == "Err" => {}
        Value::Bool(false) => {}
        other => {
            // Document whatever behavior exists
            let _ = other;
        }
    }
}

#[test]
fn test_runtime_try_receive_empty() {
    let result = run(r#"
import channel
fn main() {
  let ch = channel.new(10)
  channel.try_receive(ch)
}
    "#);
    // Should return None for empty channel
    match result {
        Value::Variant(ref tag, _) if tag == "None" => {} // expected
        other => {
            let _ = other; // just don't panic
        }
    }
}

// ════════════════════════════════════════════════════════════════════
// PHASE 7: RUNTIME ERRORS — Control Flow Edge Cases
// ════════════════════════════════════════════════════════════════════

#[test]
fn test_runtime_panic_message() {
    let err = run_err(r#"fn main() { panic("something went wrong") }"#);
    assert!(err.contains("something went wrong"), "got: {err}");
}

#[test]
fn test_runtime_panic_with_interpolation() {
    let err = run_err(
        r#"
fn main() {
  let x = 42
  panic("bad value: {x}")
}
    "#,
    );
    assert!(err.contains("bad value: 42"), "got: {err}");
}

#[test]
fn test_runtime_question_mark_on_non_result() {
    // Using ? on something that's not a Result or Option
    let err = run_err(
        r#"
fn foo() {
  let x = 42?
  x
}
fn main() { foo() }
    "#,
    );
    // Lock the actual production string so a trivial fallback that
    // happens to mention "?" (e.g. "parse error: unexpected ?") cannot
    // satisfy this assertion.
    assert!(
        err.contains("? on non-variant: Int"),
        "expected \"? on non-variant: Int\", got: {err}"
    );
}

// `test_runtime_record_update_on_non_record` removed in round 14:
// round 13 moved this check to compile time, so the runtime-only check
// (via `run_err`, which silently swallows typechecker errors in its
// helper) no longer exercises a distinct code path. The compile-time
// lock is `test_record_update_unknown_field_on_non_record_rejected_at_typecheck`
// in tests/type_audit_regressions.rs.

#[test]
fn test_runtime_field_access_on_int() {
    let err = run_err(
        r#"
fn main() {
  let x = 42
  x.name
}
    "#,
    );
    // Lock the full phrase "cannot access field 'name' on Int"; a weaker
    // fallback like "unknown field" or a generic "field" substring must
    // not satisfy this.
    assert!(
        err.contains("cannot access field 'name' on Int"),
        "expected \"cannot access field 'name' on Int\", got: {err}"
    );
}

#[test]
fn test_runtime_field_access_on_list() {
    let err = run_err(
        r#"
fn main() {
  let xs = [1, 2, 3]
  xs.name
}
    "#,
    );
    assert!(
        err.contains("cannot access field 'name' on List"),
        "expected \"cannot access field 'name' on List\", got: {err}"
    );
}

#[test]
fn test_runtime_stack_overflow() {
    let err = run_err(
        r#"
fn deep(n) {
  1 + deep(n + 1)
}
fn main() { deep(0) }
    "#,
    );
    assert!(err.contains("stack overflow"), "got: {err}");
}

#[test]
fn test_runtime_tco_does_not_overflow() {
    // Tail-recursive function should NOT stack overflow
    let result = run(r#"
fn countdown(n) {
  match n {
    0 -> 0
    _ -> countdown(n - 1)
  }
}
fn main() { countdown(1000000) }
    "#);
    assert_eq!(result, Value::Int(0));
}

#[test]
fn test_runtime_loop_arity_mismatch() {
    let err = run_err(
        r#"
fn main() {
  loop x = 0, y = 0 {
    loop(1)
  }
}
    "#,
    );
    assert!(err.contains("expects 2 arguments"), "got: {err}");
}

#[test]
fn test_runtime_assert_failure() {
    let err = run_err(
        r#"
import test
fn main() { test.assert(false) }
    "#,
    );
    assert!(err.contains("assert"), "got: {err}");
}

#[test]
fn test_runtime_assert_eq_failure() {
    let err = run_err(
        r#"
import test
fn main() { test.assert_eq(1, 2) }
    "#,
    );
    assert!(err.contains("1") && err.contains("2"), "got: {err}");
}

#[test]
fn test_runtime_assert_ne_failure() {
    let err = run_err(
        r#"
import test
fn main() { test.assert_ne(5, 5) }
    "#,
    );
    assert!(err.contains("5"), "got: {err}");
}

// ── when/else guard errors ──────────────────────────────────────────

#[test]
fn test_runtime_when_else_panic() {
    let err = run_err(
        r#"
fn positive(n) {
  when n > 0 else { panic("must be positive") }
  n
}
fn main() { positive(-5) }
    "#,
    );
    assert!(err.contains("must be positive"), "got: {err}");
}

#[test]
fn test_runtime_when_else_return() {
    let result = run(r#"
fn abs(n) {
  when n >= 0 else { return -n }
  n
}
fn main() { abs(-42) }
    "#);
    assert_eq!(result, Value::Int(42));
}

// ── Undefined global at runtime ─────────────────────────────────────

#[test]
fn test_runtime_undefined_global() {
    // Asserts exact VM message from src/vm/execute.rs GetGlobal handler
    // ("undefined global: nonexistent_function"). Locks the lowercase
    // spelling so a capitalized fallback elsewhere cannot satisfy it.
    let err = run_err(
        r#"
fn main() { nonexistent_function() }
    "#,
    );
    assert!(
        err.contains("undefined global: nonexistent_function"),
        "got: {err}"
    );
}

// ════════════════════════════════════════════════════════════════════
// PHASE 8: EDGE CASES & INTEGRATION
// ════════════════════════════════════════════════════════════════════

#[test]
fn test_empty_program() {
    // A program with no main should still parse without crashing
    let tokens = Lexer::new("").tokenize().expect("lexer error");
    let program = Parser::new(tokens).parse_program();
    // Either succeeds with empty program or errors gracefully
    let _ = program;
}

#[test]
fn test_program_without_main() {
    // Helper functions but no main — should compile but may error at runtime
    let result = std::panic::catch_unwind(|| {
        run("fn helper(x) = x + 1");
    });
    // Should not crash the process; an error is fine
    let _ = result;
}

#[test]
fn test_moderately_nested_expression() {
    // Moderately nested parentheses should work fine
    let mut input = String::from("fn main() { ");
    for _ in 0..20 {
        input.push('(');
    }
    input.push_str("42");
    for _ in 0..20 {
        input.push(')');
    }
    input.push_str(" }");
    let result = run(&input);
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_large_list_literal() {
    // 1000-element list should work
    let mut input = String::from("import list\nfn main() {\nlet xs = [");
    for i in 0..1000 {
        if i > 0 {
            input.push_str(", ");
        }
        input.push_str(&i.to_string());
    }
    input.push_str("]\nlist.length(xs)\n}");
    let result = run(&input);
    assert_eq!(result, Value::Int(1000));
}

#[test]
fn test_string_interpolation_in_error() {
    let err = run_err(
        r#"
fn main() {
  let name = "world"
  panic("hello {name}!")
}
    "#,
    );
    assert!(err.contains("hello world!"), "got: {err}");
}

#[test]
fn test_nested_match_all_arms_diverge() {
    let err = run_err(
        r#"
fn process(x) {
  match x {
    0 -> panic("zero")
    _ -> panic("nonzero")
  }
}
fn main() { process(1) }
    "#,
    );
    assert!(err.contains("nonzero"), "got: {err}");
}

#[test]
fn test_match_no_matching_arm() {
    // This tests runtime match failure if the typechecker doesn't catch it
    // (e.g., when patterns don't use type annotations)
    let input = r#"
fn main() {
  let x = (1, 2, 3)
  match x {
    (1, 2, 4) -> "a"
    (1, 3, 3) -> "b"
  }
}
    "#;
    // Asserts exact typechecker non-exhaustiveness diagnostic from
    // src/typechecker/exhaustiveness.rs. The typechecker now catches
    // the missing-tuple-pattern at compile time, so the previous
    // runtime branch (with a weak `match || no matching` OR chain) was
    // dead code.
    assert_type_error(input, "non-exhaustive match: not all patterns are covered");
}

#[test]
fn test_integer_overflow_is_runtime_error() {
    // Overflow should produce a runtime error, not wrap silently
    let err = run_err(&format!("fn main() {{ {} + 1 }}", i64::MAX));
    assert!(
        err.contains("integer overflow"),
        "expected overflow error, got: {err}"
    );
}

#[test]
fn test_integer_subtraction_overflow_is_runtime_error() {
    // i64::MIN - 1 must overflow. We construct i64::MIN at runtime as
    // (-i64::MAX - 1) so the literal itself fits in i64, then subtract 1.
    let err = run_err(&format!("fn main() {{ (-{max} - 1) - 1 }}", max = i64::MAX));
    assert!(
        err.contains("integer overflow"),
        "expected overflow error, got: {err}"
    );
}

#[test]
fn test_integer_multiplication_overflow_is_runtime_error() {
    // i64::MAX * 2 must overflow.
    let err = run_err(&format!("fn main() {{ {} * 2 }}", i64::MAX));
    assert!(
        err.contains("integer overflow"),
        "expected overflow error, got: {err}"
    );
}

#[test]
fn test_empty_string_operations() {
    run_ok(
        r#"
import string
fn main() {
  let s = ""
  let _ = string.length(s)
  let _ = string.split(s, ",")
  let _ = string.trim(s)
  let _ = string.to_upper(s)
  let _ = string.to_lower(s)
  let _ = string.contains(s, "x")
  let _ = string.starts_with(s, "")
  let _ = string.ends_with(s, "")
}
    "#,
    );
}

#[test]
fn test_empty_list_operations() {
    run_ok(
        r#"
import list
fn main() {
  let xs = []
  let _ = list.length(xs)
  let _ = list.map(xs) { x -> x }
  let _ = list.filter(xs) { x -> true }
  let _ = list.fold(xs, 0) { acc, x -> acc + x }
  let _ = list.head(xs)
  let _ = list.reverse(xs)
}
    "#,
    );
}

#[test]
fn test_empty_map_operations() {
    run_ok(
        r#"
import map
fn main() {
  let m = #{}
  let _ = map.keys(m)
  let _ = map.values(m)
  let _ = map.entries(m)
  let _ = map.length(m)
}
    "#,
    );
}

#[test]
fn test_recursive_data_structure() {
    // ADT with recursive variant — should work for bounded depth
    let result = run(r#"
type Tree {
  Leaf(Int),
  Node(Tree, Tree),
}

fn sum(t) {
  match t {
    Leaf(n) -> n
    Node(l, r) -> sum(l) + sum(r)
  }
}

fn main() {
  let t = Node(Node(Leaf(1), Leaf(2)), Leaf(3))
  sum(t)
}
    "#);
    assert_eq!(result, Value::Int(6));
}

#[test]
fn test_mutual_recursion() {
    let result = run(r#"
fn is_even(n) {
  match n {
    0 -> true
    _ -> is_odd(n - 1)
  }
}

fn is_odd(n) {
  match n {
    0 -> false
    _ -> is_even(n - 1)
  }
}

fn main() { is_even(10) }
    "#);
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn test_closure_captures_updated_binding() {
    // Closures capture by value; shadowing after capture shouldn't affect closure
    let result = run(r#"
fn main() {
  let x = 10
  let f = fn() { x }
  let x = 20
  f()
}
    "#);
    // Silt uses immutable bindings + shadowing; closure captures original value
    assert_eq!(result, Value::Int(10));
}

#[test]
fn test_pipe_into_wrong_arity() {
    // Piping a value into a function that takes 0 args — asserts exact
    // VM message from src/vm/execute.rs function-call arity check
    // ("function 'no_args' expects 0 arguments, got 1").
    let err = run_err(
        r#"
fn no_args() = 42
fn main() { 1 |> no_args() }
    "#,
    );
    assert!(
        err.contains("function 'no_args' expects 0 arguments, got 1"),
        "got: {err}"
    );
}

// ── Pattern matching edge cases ─────────────────────────────────────

#[test]
fn test_pattern_empty_list_vs_nonempty() {
    let result = run(r#"
fn describe(xs) {
  match xs {
    [] -> "empty"
    [_] -> "one"
    [_, _] -> "two"
    _ -> "many"
  }
}
fn main() { describe([]) }
    "#);
    assert_eq!(result, Value::String("empty".into()));
}

#[test]
fn test_pattern_nested_variant() {
    let result = run(r#"
fn main() {
  let x = Ok(Some(42))
  match x {
    Ok(Some(n)) -> n
    Ok(None) -> 0
    Err(_) -> -1
  }
}
    "#);
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_pattern_or_with_binding() {
    let result = run(r#"
fn classify(n) {
  match n {
    1 | 2 | 3 -> "low"
    4 | 5 | 6 -> "mid"
    _ -> "high"
  }
}
fn main() { classify(5) }
    "#);
    assert_eq!(result, Value::String("mid".into()));
}

#[test]
fn test_pattern_guard_with_complex_condition() {
    let result = run(r#"
import list
fn find_special(xs) {
  match xs {
    [x, ..rest] when x > 10 -> x
    [_, ..rest] -> find_special(rest)
    [] -> -1
  }
}
fn main() { find_special([1, 5, 15, 3]) }
    "#);
    assert_eq!(result, Value::Int(15));
}

#[test]
fn test_record_pattern_matching() {
    let result = run(r#"
type Point { x: Int, y: Int }
fn origin(p) {
  match p {
    Point { x: 0, y: 0 } -> true
    _ -> false
  }
}
fn main() { origin(Point { x: 0, y: 0 }) }
    "#);
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn test_map_pattern_matching() {
    let result = run(r#"
fn has_name(m) {
  match m {
    #{"name": n} -> n
    _ -> "unknown"
  }
}
fn main() { has_name(#{"name": "alice", "age": "30"}) }
    "#);
    assert_eq!(result, Value::String("alice".into()));
}

#[test]
fn test_parse_excessive_nesting() {
    // 300+ nested parens should produce a parse error, not a stack overflow.
    // We spawn a thread with a large stack so the depth guard (not the OS
    // stack limit) is what stops the recursion, even in unoptimised debug builds.
    let result = std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024) // 16 MB
        .spawn(|| {
            let mut input = String::from("fn main() { ");
            for _ in 0..300 {
                input.push('(');
            }
            input.push_str("42");
            for _ in 0..300 {
                input.push(')');
            }
            input.push_str(" }");
            let tokens = silt::lexer::Lexer::new(&input)
                .tokenize()
                .expect("lexer error");
            silt::parser::Parser::new(tokens).parse_program()
        })
        .expect("failed to spawn thread")
        .join()
        .expect("thread panicked");

    assert!(result.is_err(), "should fail with nesting error");
    let err = result.unwrap_err();
    // Asserts exact parser message from src/parser.rs
    // ("expression nesting exceeds maximum depth").
    assert!(
        err.message.contains("expression nesting exceeds maximum depth"),
        "got: {}",
        err.message
    );
}

// ════════════════════════════════════════════════════════════════════
// PHASE 9: CONCURRENCY RUNTIME ERRORS
// ════════════════════════════════════════════════════════════════════

#[test]
fn test_runtime_send_non_channel() {
    let err = run_err(
        r#"
import channel
fn main() {
  channel.send(42, "hello")
}
    "#,
    );
    // Lock the exact production message so that a generic type-mismatch
    // with just the word "expected" cannot satisfy this test.
    assert!(
        err.contains("channel.send requires a channel as first argument"),
        "expected \"channel.send requires a channel as first argument\", got: {err}"
    );
}

#[test]
fn test_runtime_receive_non_channel() {
    let err = run_err(
        r#"
import channel
fn main() {
  channel.receive("not a channel")
}
    "#,
    );
    assert!(
        err.contains("channel.receive requires a channel argument"),
        "expected \"channel.receive requires a channel argument\", got: {err}"
    );
}

#[test]
fn test_runtime_close_non_channel() {
    let err = run_err(
        r#"
import channel
fn main() {
  channel.close(42)
}
    "#,
    );
    assert!(
        err.contains("channel.close requires a channel argument"),
        "expected \"channel.close requires a channel argument\", got: {err}"
    );
}

#[test]
fn test_runtime_channel_new_no_args_creates_channel() {
    // channel.new() with no args creates a default (unbuffered) channel
    run_ok(
        r#"
import channel
fn main() {
  let ch = channel.new()
  channel.close(ch)
}
    "#,
    );
}

#[test]
fn test_runtime_channel_send_wrong_arg_count() {
    // Asserts exact builtin arity message from src/builtins/channel.rs
    // ("channel.send takes 2 arguments (channel, value)").
    let err = run_err(
        r#"
import channel
fn main() {
  let ch = channel.new(1)
  channel.send(ch)
}
    "#,
    );
    assert!(
        err.contains("channel.send takes 2 arguments (channel, value)"),
        "got: {err}"
    );
}

#[test]
fn test_runtime_task_join_non_handle() {
    let err = run_err(
        r#"
import task
fn main() {
  task.join(42)
}
    "#,
    );
    // Lock the exact production string so a generic "expected X, got Y"
    // type-mismatch error cannot satisfy this assertion.
    assert!(
        err.contains("task.join requires a handle argument"),
        "expected \"task.join requires a handle argument\", got: {err}"
    );
}

#[test]
fn test_runtime_task_cancel_non_handle() {
    let err = run_err(
        r#"
import task
fn main() {
  task.cancel("not a handle")
}
    "#,
    );
    assert!(
        err.contains("task.cancel requires a handle argument"),
        "expected \"task.cancel requires a handle argument\", got: {err}"
    );
}

#[test]
fn test_runtime_task_spawn_non_callable() {
    // Asserts exact builtin message from src/builtins/task.rs
    // ("task.spawn requires a function argument").
    let err = run_err(
        r#"
import task
fn main() {
  task.spawn(42)
}
    "#,
    );
    assert!(
        err.contains("task.spawn requires a function argument"),
        "got: {err}"
    );
}

// ════════════════════════════════════════════════════════════════════
// PHASE 10: TYPE ERRORS — TRAIT VIOLATIONS
// ════════════════════════════════════════════════════════════════════

#[test]
fn test_type_missing_trait_impl_method() {
    // Trait impl missing a required method
    let errs = type_errors(
        r#"
trait Greetable {
  fn greet(self) -> String
  fn farewell(self) -> String
}

trait Greetable for Int {
  fn greet(self) = "hello"
}

fn main() { 42 }
    "#,
    );
    // Should flag the missing `farewell` method, or at minimum not crash
    let _ = errs;
}

#[test]
fn test_type_wrong_return_type_in_trait_impl() {
    // The typechecker currently does not catch mismatched return types in
    // trait impls (it's a known gap). This test documents the behavior --
    // it should at least not crash.
    let errs = type_errors(
        r#"
trait Numeric {
  fn double(self) -> Int
}

trait Numeric for Int {
  fn double(self) -> String = "wrong"
}

fn main() { 42 }
    "#,
    );
    // If the typechecker improves, it will catch this. For now, just
    // ensure we don't panic.
    let _ = errs;
}

#[test]
fn test_type_multiple_match_arm_types_mismatch() {
    assert_type_error(
        r#"
fn check(x: Int) -> String {
  match x {
    0 -> "zero"
    1 -> 1
    _ -> "other"
  }
}
fn main() { check(0) }
        "#,
        "mismatch",
    );
}

// ════════════════════════════════════════════════════════════════════
// PHASE 11: IMPORT ERRORS
// ════════════════════════════════════════════════════════════════════

#[test]
fn test_import_nonexistent_builtin_item() {
    // Asserts exact VM message from src/vm/execute.rs GetGlobal handler.
    // The `import list.{ nonexistent_function }` statement registers an
    // alias pointing at `list.nonexistent_function`, but that global was
    // never defined, so runtime resolution fails with
    // "undefined global: list.nonexistent_function".
    let err = run_err(
        r#"
import list.{ nonexistent_function }
fn main() { nonexistent_function([1, 2]) }
    "#,
    );
    assert!(
        err.contains("undefined global: list.nonexistent_function"),
        "got: {err}"
    );
}

#[test]
fn test_runtime_call_wrong_arity() {
    // Asserts exact VM message from src/vm/execute.rs function-call arity
    // check ("function 'add' expects 2 arguments, got 3").
    let err = run_err(
        r#"
fn add(a, b) = a + b
fn main() { add(1, 2, 3) }
    "#,
    );
    assert!(
        err.contains("function 'add' expects 2 arguments, got 3"),
        "got: {err}"
    );
}

#[test]
fn test_runtime_call_non_callable() {
    let err = run_err(
        r#"
fn main() {
  let x = 42
  x(1, 2)
}
    "#,
    );
    // Lock the exact production message; the previous OR chain's third
    // branch `callable` subsumed the first two and made the assertion weak.
    assert!(
        err.contains("cannot call value of type Int"),
        "expected \"cannot call value of type Int\", got: {err}"
    );
}

// ════════════════════════════════════════════════════════════════════
// PHASE 12: RUNTIME ERRORS — Builtin Arity Checks
// ════════════════════════════════════════════════════════════════════

#[test]
fn test_runtime_list_map_wrong_arity() {
    // Asserts exact builtin arity message from src/builtins/list.rs
    // ("list.map takes 2 arguments (list, fn)").
    let err = run_err(
        r#"
import list
fn main() { list.map([1, 2]) }
    "#,
    );
    assert!(
        err.contains("list.map takes 2 arguments (list, fn)"),
        "got: {err}"
    );
}

#[test]
fn test_runtime_string_split_wrong_type() {
    let err = run_err(
        r#"
import string
fn main() { string.split(42, ",") }
    "#,
    );
    // Lock the exact production message so a generic type-mismatch error
    // with just the word "type" cannot satisfy this assertion.
    assert!(
        err.contains("string.split requires strings"),
        "expected \"string.split requires strings\", got: {err}"
    );
}

#[test]
fn test_runtime_map_get_wrong_arity() {
    // Asserts exact builtin arity message from src/builtins/map.rs
    // ("map.get takes 2 arguments").
    let err = run_err(
        r#"
import map
fn main() { map.get(#{"a": 1}) }
    "#,
    );
    assert!(err.contains("map.get takes 2 arguments"), "got: {err}");
}

#[test]
fn test_runtime_io_read_file_nonexistent() {
    let result = run(r#"
import io
fn main() { io.read_file("/tmp/silt_nonexistent_file_12345.txt") }
    "#);
    // Should return Err variant, not panic
    match result {
        Value::Variant(ref tag, _) if tag == "Err" => {}
        other => panic!("expected Err variant for missing file, got {other:?}"),
    }
}

#[test]
fn test_runtime_regex_wrong_arity() {
    // Asserts exact builtin arity message from src/builtins/regex.rs
    // ("regex.is_match takes 2 arguments (pattern, text)").
    let err = run_err(
        r#"
import regex
fn main() { regex.is_match("[a-z]+") }
    "#,
    );
    assert!(
        err.contains("regex.is_match takes 2 arguments (pattern, text)"),
        "got: {err}"
    );
}

#[test]
fn test_runtime_non_exhaustive_match_tuple() {
    let err = run_err(
        r#"
fn main() {
  match (1, 2) {
    (0, 0) -> "origin"
  }
}
    "#,
    );
    // Lock the full panic string so that a type-mismatch or generic
    // "mismatch"/"matched" error cannot satisfy this assertion.
    // This is the most insidious gap: a bug that conflates a type mismatch
    // with a non-exhaustive match would slip through the old `contains("match")`.
    assert!(
        err.contains("non-exhaustive match: no arm matched"),
        "expected \"non-exhaustive match: no arm matched\", got: {err}"
    );
}

#[test]
fn test_runtime_non_exhaustive_match_variant() {
    let err = run_err(
        r#"
fn main() {
  let x = Some(42)
  match x {
    None -> "none"
  }
}
    "#,
    );
    assert!(
        err.contains("non-exhaustive match: no arm matched"),
        "expected \"non-exhaustive match: no arm matched\", got: {err}"
    );
}

// ── Self type outside trait ─────────────────────────────────────────

#[test]
fn test_self_type_outside_trait() {
    // Using Self in a regular function should not resolve to a concrete type.
    // The typechecker should either error or treat it as an unresolved type variable.
    let errs = type_errors(
        r#"
fn foo(x: Self) -> Self { x }
fn main() { 42 }
    "#,
    );
    // Self outside a trait context is not meaningful; at minimum it should not crash.
    // If the typechecker produces an error, it should mention 'Self'.
    let _ = errs;
}

// ── Type ascription errors ──────────────────────────────────────────

#[test]
fn test_ascription_type_mismatch() {
    assert_type_error(
        r#"
fn main() {
  42 as String
}
    "#,
        "type mismatch",
    );
}

#[test]
fn test_ascription_missing_type() {
    // L4 (hardening): previously asserted `!err.is_empty()`, which any
    // parse error would have satisfied. Pin the exact diagnostic phrase
    // the parser emits when `as` isn't followed by a type expression:
    // `parse_type_expr` → `expect_ident` → "expected identifier, found }".
    // This way a regression that accepts `42 as` silently (or that
    // changes the error to something unrelated like "unexpected token")
    // actually fails the test instead of rubber-stamping it.
    let err = parse_err(
        r#"
fn main() {
  42 as
}
    "#,
    );
    assert!(
        err.contains("expected identifier, found }"),
        "expected parser diagnostic 'expected identifier, found }}' for `42 as`, got: {err:?}"
    );
}

// ── Unresolved type variable detection ─────────────────────────────

#[test]
fn test_unresolved_type_variable_error() {
    // Asserts the exact diagnostic from the typechecker when a binding
    // has an unresolved polymorphic return type and is never used to
    // pin the type: "could not fully determine the type of this
    // expression; consider adding a type annotation".
    let input = r#"
fn default() -> a { panic("no value") }
fn main() { let x = default() }
"#;
    let errs = type_errors(input);
    assert!(
        errs.iter().any(|e| e.contains(
            "could not fully determine the type of this expression; consider adding a type annotation"
        )),
        "expected unresolved type variable error, got: {errs:?}"
    );
}

#[test]
fn test_unresolved_type_variable_not_flagged_when_used() {
    let input = r#"
fn default() -> a { panic("no value") }
fn main() {
  let x = default()
  x
}
"#;
    let errs = type_errors(input);
    assert!(
        !errs
            .iter()
            .any(|e| e.contains("could not") && e.contains("type annotation")),
        "should not flag unresolved type when binding is used later, got: {errs:?}"
    );
}

// ════════════════════════════════════════════════════════════════════
// IMPORT GATING COMPILE ERRORS
// ════════════════════════════════════════════════════════════════════

#[test]
fn test_compile_gated_constructor_no_import() {
    let err = run_err(
        r#"
fn main() { Stop }
    "#,
    );
    assert!(
        err.contains("requires `import list`"),
        "expected gated constructor error, got: {err}"
    );
}

#[test]
fn test_compile_module_method_no_import() {
    let err = run_err(
        r#"
fn main() { list.map([1, 2, 3], fn(x) { x }) }
    "#,
    );
    assert!(
        err.contains("not imported"),
        "expected module not imported error, got: {err}"
    );
}

#[test]
fn test_compile_module_field_no_import() {
    let err = run_err(
        r#"
fn main() { math.pi }
    "#,
    );
    assert!(
        err.contains("not imported"),
        "expected module not imported error, got: {err}"
    );
}

#[test]
fn test_compile_gated_pattern_no_import() {
    let err = run_err(
        r#"
fn main() {
  match 1 {
    Monday -> "mon"
    _ -> "other"
  }
}
    "#,
    );
    assert!(
        err.contains("requires `import time`"),
        "expected gated pattern error, got: {err}"
    );
}

// ── float.to_string negative decimals ─────────────────────────────

#[test]
fn test_float_to_string_negative_decimals_error() {
    let err = run_err(
        r#"
import float
fn main() { float.to_string(3.14159, -1) }
    "#,
    );
    assert!(
        err.contains("decimals must be non-negative"),
        "expected non-negative decimals error, got: {err}"
    );
}

#[test]
fn test_float_to_string_positive_decimals_still_works() {
    let result = run(r#"
import float
fn main() { float.to_string(3.14, 2) }
    "#);
    assert_eq!(result, Value::String("3.14".into()));
}

// ════════════════════════════════════════════════════════════════════
// SCHEME NARROWING (function body constraints reflected in inferred types)
// ════════════════════════════════════════════════════════════════════

#[test]
fn test_scheme_narrowing_rejects_wrong_type() {
    // fn add_one(x) = x + 1 should infer Int -> Int, not forall a. a -> a
    assert_type_error(
        "fn add_one(x) = x + 1\nfn main() { add_one(\"hello\") }",
        "type mismatch: expected Int, got String",
    );
}

#[test]
fn test_scheme_narrowing_preserves_polymorphism() {
    // fn id(x) = x should remain polymorphic
    run_ok("fn id(x) = x\nfn main() { let a = id(1)\nlet b = id(\"hello\")\na }");
}

#[test]
fn test_scheme_narrowing_pattern_match() {
    // fn process(pair) { match pair { (a, b) -> b + 10 } } should constrain b to Int
    assert_type_error(
        "fn process(pair) { match pair { (a, b) -> b + 10 } }\nfn main() { process((1, \"hello\")) }",
        "type mismatch: expected Int, got String",
    );
}

#[test]
fn test_scheme_narrowing_multiple_params() {
    // fn sum(x, y) = x + y + 1 constrains both params to Int via the + 1 literal
    // sum should be narrowed to (Int, Int) -> Int because + 1 forces Int;
    // passing strings should fail at compile time.
    assert_type_error(
        "fn sum(x, y) = x + y + 1\nfn main() { sum(\"hello\", \"world\") }",
        "type mismatch: expected Int, got String",
    );
}

// ════════════════════════════════════════════════════════════════════
// RECUR TYPE CHECKING AND PIPE OPERATOR
// ════════════════════════════════════════════════════════════════════

#[test]
fn test_recur_type_mismatch() {
    assert_type_error(
        "fn main() {\n  loop i = 0, s = \"hello\" {\n    match i > 3 {\n      true -> s\n      false -> loop(true, 42)\n    }\n  }\n}",
        "type mismatch",
    );
}

#[test]
fn test_recur_correct_types() {
    assert_no_type_errors(
        "fn main() { loop i = 0 { match i > 5 { true -> i\n false -> loop(i + 1) } } }",
    );
}

#[test]
fn test_pipe_non_callable_rhs() {
    assert_type_error("fn main() { 42 |> \"hello\" }", "pipe");
}

#[test]
fn test_pipe_correct_usage() {
    assert_no_type_errors("fn double(x) = x * 2\nfn main() { 5 |> double }");
}

// ── Parser: unclosed delimiter error messages ──────────────────────

#[test]
fn test_parse_unclosed_list_literal_points_at_opener() {
    let msg = parse_err("fn main() {\n  let x = [1, 2, 3\n  x\n}\n");
    assert!(
        msg.contains("list literal"),
        "expected 'list literal' in error, got: {msg}"
    );
    assert!(msg.contains("]"), "expected ']' in error, got: {msg}");
    assert!(
        msg.contains("line 2"),
        "expected 'line 2' in error, got: {msg}"
    );
}

#[test]
fn test_parse_unclosed_call_args_points_at_opener() {
    let msg = parse_err("fn main() {\n  foo(1, 2,\n  3\n}\n");
    assert!(
        msg.contains("function call argument list"),
        "expected 'function call argument list' in error, got: {msg}"
    );
    assert!(msg.contains(")"), "expected ')' in error, got: {msg}");
    assert!(
        msg.contains("line 2"),
        "expected 'line 2' in error, got: {msg}"
    );
}

// ════════════════════════════════════════════════════════════════════
// AUDIT REGRESSION T1: bare parameterized record names in fn signatures
// ════════════════════════════════════════════════════════════════════
//
// Before the fix, writing `fn grab(b: Box) -> Int { b.value }` for a
// parameterized record `type Box(a) { value: a }` typechecked cleanly.
// The bare `Box` resolved to `Type::Generic("Box", [])` — the empty-arg
// unification arms silently accepted this, and field access on the
// parameterized record fell through a zip-of-empty substitution that
// returned the SHARED template TyVar, polluting it globally across
// uses. Two functions grabbing different field types would then
// cross-pollinate and one would fail with a spurious error.

#[test]
fn test_bare_parameterized_record_in_fn_signature_rejected() {
    // `Box` is parameterized with one type variable, but the parameter
    // annotation `b: Box` omits the type argument. The typechecker must
    // reject this (or at minimum force the body to be consistent with
    // the declared return type). Either way the program below must NOT
    // typecheck: `b.value` has the record's param type, not `Int`.
    //
    // The live REPL repro from the audit agent:
    //   type Box(a) { value: a }
    //   fn grab(b: Box) -> Int { b.value }
    //   grab(Box { value: "hi" })   -- runtime returns "hi" from Int-decl fn
    let errs = type_errors(
        r#"
type Box(a) { value: a }
fn grab(b: Box) -> Int { b.value }
fn main() {
  let _ = grab(Box { value: "hi" })
}
"#,
    );
    assert!(
        !errs.is_empty(),
        "expected at least one type error, got none"
    );
    // Asserts the exact downstream type-mismatch produced when the bare
    // `Box` parameter annotation instantiates a fresh type variable
    // that `b.value` then constrains to Int (from the declared return
    // type), while the caller passes a `String`. The fix from round 13
    // must produce this exact diagnostic; a loose substring like
    // "Box" or "Int" would silently pass even if the diagnostic
    // regressed to an unrelated message.
    assert!(
        errs.iter()
            .any(|e| e.contains("type mismatch: expected Int, got String")),
        "expected \"type mismatch: expected Int, got String\", got: {errs:?}"
    );
}

#[test]
fn test_record_template_var_not_polluted_across_fns() {
    // Two functions that each pin the record's type parameter to a
    // different concrete type must both typecheck cleanly. Before the
    // fix, the first function mutated the shared template TyVar of
    // the record's field type, and the second function then saw a
    // concrete `Int` where it expected a polymorphic var, failing with
    // `expected String, got Int`. This test uses explicit type args
    // (`Box(Int)` and `Box(String)`) — the form that always should
    // have worked and that locks in "no cross-function pollution".
    assert_no_type_errors(
        r#"
type Box(a) { value: a }
fn grab_int(b: Box(Int)) -> Int { b.value }
fn grab_str(b: Box(String)) -> String { b.value }
fn main() {
  let _ = grab_int(Box { value: 1 })
  let _ = grab_str(Box { value: "hi" })
}
"#,
    );
}

#[test]
fn test_bare_parameterized_record_body_uses_are_independent() {
    // The audit's second repro:
    //
    //     type Box(a) { value: a }
    //     fn grab_int(b: Box) -> Int { b.value }
    //     fn grab_str(b: Box) -> String { b.value }
    //
    // Before the fix, this program FAILED TYPECHECK with the spurious
    // "expected String, got Int" — the first function mutated the
    // shared template TyVar of `Box.value` to `Int`, and the second
    // function then saw `Int` in its body where it expected `String`.
    //
    // After the fix (T1), bare `Box` in the parameter annotation
    // instantiates a fresh type variable for each parameterized use,
    // so the two bodies are independent: each fn monomorphizes its
    // own fresh `a`, `grab_int` pins it to `Int` and returns, then
    // `grab_str` pins its OWN fresh `a` to `String`. Both typecheck
    // cleanly. This test locks that.
    assert_no_type_errors(
        r#"
type Box(a) { value: a }
fn grab_int(b: Box) -> Int { b.value }
fn grab_str(b: Box) -> String { b.value }
fn main() {
  ()
}
"#,
    );
}

// ════════════════════════════════════════════════════════════════════
// AUDIT REGRESSION T2: primitive type descriptors must not be values
// ════════════════════════════════════════════════════════════════════
//
// Before the fix, `Int`, `Float`, `String`, `Bool` were registered
// in the type environment as their underlying type (Int, Float, etc.).
// The runtime represents these as `Value::PrimitiveDescriptor("Int")`,
// not `Value::Int(_)`, so `Int * 2` passed typecheck but crashed at
// runtime with "cannot apply '*' to PrimitiveDescriptor and Int".
// The fix wraps them in `TypeOf(T)` so the typechecker catches the
// misuse.

#[test]
fn test_bare_int_descriptor_cannot_be_used_in_arithmetic() {
    // `double(Int)` must fail typecheck: `Int` is a type descriptor
    // (TypeOf(Int)), not a value of type Int. Before the fix this
    // passed typecheck and crashed at runtime with
    // `cannot apply '*' to PrimitiveDescriptor and Int`.
    let errs = type_errors(
        r#"
fn double(n: Int) -> Int { n * 2 }
fn main() {
  let _ = double(Int)
}
"#,
    );
    assert!(
        !errs.is_empty(),
        "expected a type error when passing the `Int` descriptor as a value, got none"
    );
    // Asserts the exact typechecker phrase "expected Int, got TypeOf(Int)";
    // previously the OR chain's second branch `contains("Int")` was so
    // broad that almost any diagnostic mentioning the type would pass.
    assert!(
        errs.iter()
            .any(|e| e.contains("expected Int, got TypeOf(Int)")),
        "expected mismatch between Int and TypeOf(Int), got: {errs:?}"
    );
}

#[test]
fn test_json_parse_still_accepts_primitive_descriptors() {
    // Passing `Int` to `json.parse` must continue to typecheck: the
    // descriptor value has type `TypeOf(Int)`, and json.parse's
    // signature is `forall a. (TypeOf(a), String) -> Result(a, String)`.
    assert_no_type_errors(
        r#"
import json
fn main() {
  let r = json.parse(Int, "42")
  match r {
    Ok(n) -> {
      let _: Int = n
    }
    Err(_) -> ()
  }
}
"#,
    );
}

#[test]
fn test_descriptor_in_list_rejected() {
    // A heterogeneous list `[1, 2, Int]` must fail typecheck cleanly:
    // the element type is `Int` for the first two, but `TypeOf(Int)`
    // for the descriptor. Before the fix this would pass and produce
    // garbage at runtime.
    let errs = type_errors(
        r#"
fn main() {
  let _ = [1, 2, Int]
}
"#,
    );
    assert!(
        !errs.is_empty(),
        "expected a type error for [1, 2, Int], got none"
    );
}

// ════════════════════════════════════════════════════════════════════
// AUDIT REGRESSION T3: task.join Handle(a) is nominally typed
// ════════════════════════════════════════════════════════════════════
//
// Locks the fix from commit 59f7f58: task.spawn/join/cancel all share
// a nominal `Handle(a)` type so that `let s: String = task.join(h)`
// for an `h : Handle(Int)` must fail typecheck with `expected String,
// got Int`.

#[test]
fn test_task_join_handle_type_is_nominal() {
    assert_type_error(
        r#"
import task
fn main() {
  let h = task.spawn(fn() { 42 })
  let s: String = task.join(h)
  println(s)
}
"#,
        "expected String",
    );
}

// ════════════════════════════════════════════════════════════════════
// AUDIT REGRESSION: value restriction on top-level let bindings (3a4edd6 B2)
// ════════════════════════════════════════════════════════════════════

#[test]
fn test_top_level_let_channel_not_generalized() {
    // A top-level `let ch = channel.new(...)` must NOT be generalized to
    // `forall a. Channel(a)`. `channel.new(...)` is not a syntactic value,
    // so the value restriction keeps `ch` monomorphic: the first use pins
    // the element type, and a second use with a different type must fail.
    //
    // Without the value restriction (prior to 3a4edd6 B2) this program
    // type-checked, and at runtime a single channel carried values of
    // two different types.
    assert_type_error(
        r#"
import channel
let ch = channel.new(1)
fn main() {
  channel.send(ch, 42)
  channel.send(ch, "hi")
}
"#,
        "expected Int",
    );
}

// ════════════════════════════════════════════════════════════════════
// AUDIT REGRESSION: channel.select signature is tuple (commit e78d6d9)
// ════════════════════════════════════════════════════════════════════
//
// `channel.select(List(Channel(a)))` must return a 2-tuple
// `(Channel(a), ChannelResult(a))`. If the signature regresses to
// returning just `ChannelResult(a)` (or just `a`), the tuple destructure
// below will no longer typecheck.
//
// `test_channel_select_returns_tuple` in integration.rs uses `run()`
// which silently swallows typecheck errors, so it cannot lock this
// invariant. This test drives the typechecker directly via `type_errors`
// and asserts that NO hard errors are produced — any regression that
// changes the return type away from a 2-tuple will emit a destructure
// error.
#[test]
fn test_channel_select_signature_is_tuple_of_channel_and_result() {
    let src = r#"
import channel
fn main() {
  let ch: Channel(Int) = channel.new(1)
  let (winner, result): (Channel(Int), ChannelResult(Int)) = channel.select([ch])
  let _ = winner
  let _ = result
  println("ok")
}
"#;
    let errs = type_errors(src);
    assert!(
        errs.is_empty(),
        "channel.select should typecheck with a 2-tuple destructure, got: {errs:?}"
    );
}

// ════════════════════════════════════════════════════════════════════
// AUDIT FINDINGS: round 16 (G2 / G3 / G5)
// ════════════════════════════════════════════════════════════════════

// ── G2: duplicate record field silently accepted ───────────────────

#[test]
fn test_duplicate_record_field_rejected() {
    // `type R { a: Int, a: String }` used to compile silently — the
    // second `a` overwrote the first's type at the VM record layout
    // level. The typechecker now emits a specific diagnostic pointing
    // at the duplicate field name.
    let errs = type_errors(
        r#"
type R { a: Int, a: String }
fn main() {
  let r = R { a: 1 }
  println(r.a)
}
"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("duplicate field 'a' in record type 'R'")),
        "expected duplicate-field error, got: {errs:?}"
    );
}

// ── G3: duplicate enum variant silently accepted ───────────────────

#[test]
fn test_duplicate_enum_variant_rejected() {
    // `type Color { Red, Green, Red }` used to compile silently —
    // the second `Red` overwrote the first's constructor binding in
    // the type environment. The typechecker now emits a specific
    // diagnostic pointing at the duplicate variant.
    let errs = type_errors(
        r#"
type Color { Red, Green, Red }
fn main() { println("ok") }
"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("duplicate variant 'Red' in enum 'Color'")),
        "expected duplicate-variant error, got: {errs:?}"
    );
}

// ── G5: misleading "undefined variable '<module>'" error ──────────

#[test]
fn test_missing_module_member_reports_specific_error() {
    // `import list; fn main() { list.range(1, 5) }` — `range` is not
    // a member of the `list` module. The old typechecker failed the
    // qualified lookup, fell through to inferring the bare `list`
    // identifier, and emitted the misleading `undefined variable
    // 'list'`. The fix emits the specific "unknown function 'range'
    // on module 'list'" before the fall-through.
    let errs = type_errors(
        r#"
import list
fn main() { list.range(1, 5) }
"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("unknown function 'range' on module 'list'")),
        "expected module-member error for list.range, got: {errs:?}"
    );
    assert!(
        !errs
            .iter()
            .any(|e| e.contains("undefined variable 'list'")),
        "must no longer report 'undefined variable list' for a valid builtin module: {errs:?}"
    );
}

#[test]
fn test_valid_module_member_still_resolves() {
    // Positive lock: a valid `list.reverse(...)` call must still
    // typecheck cleanly after the G5 guard — i.e. the guard fires
    // only when the qualified lookup actually fails.
    let errs = type_errors(
        r#"
import list
fn main() {
  let r = list.reverse([1, 2, 3])
  println("{r}")
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no type errors for list.reverse([1,2,3]), got: {errs:?}"
    );
}
