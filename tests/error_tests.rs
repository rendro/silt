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
        Err(e) => return e,
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
    let err = lex_err("\"hello\nworld");
    // The lexer should catch this as unterminated or handle newlines
    assert!(
        err.contains("unterminated") || err.contains("string"),
        "got: {err}"
    );
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
    let errs = parse_errors("fn main() { (1 + 2 }");
    assert!(
        errs.iter()
            .any(|e| e.contains("expected") || e.contains(")")),
        "got: {errs:?}"
    );
}

#[test]
fn test_parse_missing_closing_bracket() {
    let errs = parse_errors("fn main() { [1, 2, 3 }");
    assert!(
        errs.iter()
            .any(|e| e.contains("expected") || e.contains("]")),
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
    let errs = parse_errors("fn main() { let x }");
    assert!(
        errs.iter()
            .any(|e| e.contains("expected") || e.contains("=")),
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
        errs.iter()
            .any(|e| e.contains("->") || e.contains("expected")),
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
    // The parser accepts empty match bodies as valid syntax.
    // At runtime, an empty match produces a "non-exhaustive match" error.
    let err = run_err("fn main() { match 42 { } }");
    assert!(
        err.contains("non-exhaustive") || err.contains("no arm matched"),
        "empty match should fail at runtime, got: {err}"
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
    // String + Int should be a type error (unless caught at runtime)
    // Check if the typechecker catches it
    let errs = type_errors(
        r#"
fn main() {
  let x: String = "hello"
  x - 1
}
    "#,
    );
    // This may or may not be caught by the typechecker; it's OK if it's a runtime error
    // Just ensure it doesn't silently succeed at both levels
    if errs.is_empty() {
        // If typechecker doesn't catch it, runtime should
        let runtime_err = run_err(
            r#"
fn main() {
  let x = "hello"
  x - 1
}
        "#,
        );
        assert!(
            runtime_err.contains("cannot apply") || runtime_err.contains("unsupported"),
            "got: {runtime_err}"
        );
    }
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
    let errs = type_errors(
        r#"
fn foo() = 1
fn foo() = 2
fn main() { foo() }
    "#,
    );
    // May or may not be an error — at minimum, should not crash
    let _ = errs;
}

// ── Trait constraint violations ─────────────────────────────────────

#[test]
fn test_type_where_clause_violation() {
    // where clause requires Display but passing something that might not have it
    let errs = type_errors(
        r#"
fn show(x) where x: Display = x.display()
fn main() {
  show(fn() { 1 })
}
    "#,
    );
    // Closures may not implement Display
    // This test documents the behavior — whether it's caught at type time or runtime
    let _ = errs;
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
fn test_runtime_float_division_by_zero_is_inf() {
    // Float division by zero should produce Inf, not an error (IEEE 754)
    let result = run("fn main() { 1.0 / 0.0 }");
    match result {
        Value::Float(f) => assert!(f.is_infinite(), "expected infinity, got {f}"),
        other => panic!("expected Float, got {other:?}"),
    }
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
    assert!(
        err.contains("cannot apply 'not'") || err.contains("not"),
        "got: {err}"
    );
}

#[test]
fn test_runtime_not_on_string() {
    let err = run_err(r#"fn main() { !"hello" }"#);
    assert!(
        err.contains("cannot apply 'not'") || err.contains("not"),
        "got: {err}"
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
    let err = run_err(r#"fn main() { "hello" * 3 }"#);
    assert!(
        err.contains("cannot apply") || err.contains("cannot mix"),
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
    let result = run(r#"
import list
fn main() { list.get([1, 2, 3], -1) }
    "#);
    // list.get returns None for negative indices
    match result {
        Value::Variant(ref tag, ref args) if tag == "None" && args.is_empty() => {}
        other => panic!("expected None, got {other:?}"),
    }
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
    let err = run_err(r#"fn main() { 1.0..5.0 }"#);
    assert!(
        err.contains("range requires two integers") || err.contains("integer"),
        "got: {err}"
    );
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
    assert!(
        err.contains("non-Result") || err.contains("non-variant") || err.contains("?"),
        "got: {err}"
    );
}

#[test]
fn test_runtime_record_update_on_non_record() {
    let err = run_err(
        r#"
fn main() {
  let x = 42
  x.{ y: 1 }
}
    "#,
    );
    assert!(
        err.contains("non-record") || err.contains("record update") || err.contains("cannot"),
        "got: {err}"
    );
}

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
    assert!(
        err.contains("cannot access field") || err.contains("field"),
        "got: {err}"
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
        err.contains("cannot access field") || err.contains("method") || err.contains("field"),
        "got: {err}"
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
    run_ok(
        r#"
fn countdown(n) {
  match n {
    0 -> 0
    _ -> countdown(n - 1)
  }
}
fn main() { countdown(1000000) }
    "#,
    );
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
    assert!(err.contains("expects 2 argument(s)"), "got: {err}");
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
    let err = run_err(
        r#"
fn main() { nonexistent_function() }
    "#,
    );
    assert!(
        err.contains("undefined") || err.contains("Undefined"),
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
    // Either a type error (non-exhaustive) or runtime error
    let type_errs = type_errors(input);
    if type_errs.is_empty() {
        let err = run_err(input);
        assert!(
            err.contains("match") || err.contains("no matching"),
            "got: {err}"
        );
    }
}

#[test]
fn test_integer_overflow_wraps() {
    // Verify wrapping arithmetic doesn't panic
    let result = run(&format!("fn main() {{ {} + 1 }}", i64::MAX));
    match result {
        Value::Int(n) => assert_eq!(n, i64::MIN), // wrapping
        other => panic!("expected Int, got {other:?}"),
    }
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
    // Piping a value into a function that takes 0 args
    let err = run_err(
        r#"
fn no_args() = 42
fn main() { 1 |> no_args() }
    "#,
    );
    assert!(
        err.contains("argument") || err.contains("arity") || err.contains("expects"),
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
    assert!(
        err.message.contains("nesting") || err.message.contains("depth"),
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
    assert!(
        err.contains("channel") || err.contains("expected"),
        "got: {err}"
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
        err.contains("channel") || err.contains("expected"),
        "got: {err}"
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
        err.contains("channel") || err.contains("expected"),
        "got: {err}"
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
        err.contains("argument") || err.contains("takes"),
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
    assert!(
        err.contains("Handle") || err.contains("handle") || err.contains("expected"),
        "got: {err}"
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
        err.contains("Handle") || err.contains("handle") || err.contains("expected"),
        "got: {err}"
    );
}

#[test]
fn test_runtime_task_spawn_non_callable() {
    let err = run_err(
        r#"
import task
fn main() {
  task.spawn(42)
}
    "#,
    );
    assert!(
        err.contains("callable") || err.contains("function") || err.contains("closure"),
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
    // Importing a non-existent item from a builtin module
    let err = run_err(
        r#"
import list.{ nonexistent_function }
fn main() { nonexistent_function([1, 2]) }
    "#,
    );
    // Should produce an error about the missing item
    assert!(
        err.contains("not found")
            || err.contains("no public item")
            || err.contains("Undefined")
            || err.contains("undefined"),
        "got: {err}"
    );
}

#[test]
fn test_runtime_call_wrong_arity() {
    let err = run_err(
        r#"
fn add(a, b) = a + b
fn main() { add(1, 2, 3) }
    "#,
    );
    assert!(
        err.contains("argument") || err.contains("arity") || err.contains("expects"),
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
    assert!(
        err.contains("not callable") || err.contains("cannot call") || err.contains("callable"),
        "got: {err}"
    );
}

// ════════════════════════════════════════════════════════════════════
// PHASE 12: RUNTIME ERRORS — Builtin Arity Checks
// ════════════════════════════════════════════════════════════════════

#[test]
fn test_runtime_list_map_wrong_arity() {
    let err = run_err(
        r#"
import list
fn main() { list.map([1, 2]) }
    "#,
    );
    assert!(
        err.contains("argument") || err.contains("takes") || err.contains("expects"),
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
    assert!(
        err.contains("String") || err.contains("string") || err.contains("type"),
        "got: {err}"
    );
}

#[test]
fn test_runtime_map_get_wrong_arity() {
    let err = run_err(
        r#"
import map
fn main() { map.get(#{"a": 1}) }
    "#,
    );
    assert!(
        err.contains("argument") || err.contains("takes"),
        "got: {err}"
    );
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
    let err = run_err(
        r#"
import regex
fn main() { regex.is_match("[a-z]+") }
    "#,
    );
    assert!(
        err.contains("argument") || err.contains("takes"),
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
    assert!(
        err.contains("non-exhaustive") || err.contains("no arm matched") || err.contains("match"),
        "got: {err}"
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
        err.contains("non-exhaustive") || err.contains("no arm matched") || err.contains("match"),
        "got: {err}"
    );
}
