//! Tests for the new `string` stdlib operations added in this change:
//! `last_index_of`, `split_at`, `lines`, and `starts_with_at`.
//!
//! Indexing convention: these ops use character indices (chars().count()),
//! matching `string.index_of` and `string.slice`.

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::value::Value;
use silt::vm::Vm;
use std::sync::Arc;

fn run(input: &str) -> Value {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    vm.run(script).expect("runtime error")
}

fn run_err(input: &str) -> String {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = match compiler.compile_program(&program) {
        Ok(f) => f,
        Err(e) => return e.message,
    };
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    let err = vm.run(script).expect_err("expected runtime error");
    format!("{err}")
}

fn some_int(n: i64) -> Value {
    Value::Variant("Some".into(), vec![Value::Int(n)])
}

fn none() -> Value {
    Value::Variant("None".into(), Vec::new())
}

// ── string.last_index_of ───────────────────────────────────────────

#[test]
fn test_last_index_of_simple() {
    let result = run(r#"
import string
fn main() { string.last_index_of("banana", "a") }
"#);
    // Characters: b(0) a(1) n(2) a(3) n(4) a(5)
    assert_eq!(result, some_int(5));
}

#[test]
fn test_last_index_of_multichar_needle() {
    let result = run(r#"
import string
fn main() { string.last_index_of("abcabc", "bc") }
"#);
    assert_eq!(result, some_int(4));
}

#[test]
fn test_last_index_of_missing() {
    let result = run(r#"
import string
fn main() { string.last_index_of("hello", "z") }
"#);
    assert_eq!(result, none());
}

#[test]
fn test_last_index_of_unicode_char_index() {
    // "café☕café" — last "café" starts at char index 5.
    // Chars: c(0) a(1) f(2) é(3) ☕(4) c(5) a(6) f(7) é(8)
    let result = run(r#"
import string
fn main() { string.last_index_of("café☕café", "café") }
"#);
    assert_eq!(result, some_int(5));
}

#[test]
fn test_last_index_of_empty_needle() {
    // Matches Rust rfind semantics for empty needle: last position is |s|
    // (in chars, the char count).
    let result = run(r#"
import string
fn main() { string.last_index_of("abc", "") }
"#);
    assert_eq!(result, some_int(3));
}

// ── string.split_at ────────────────────────────────────────────────

#[test]
fn test_split_at_middle() {
    let result = run(r#"
import string
fn main() { string.split_at("hello", 2) }
"#);
    assert_eq!(
        result,
        Value::Tuple(vec![
            Value::String("he".into()),
            Value::String("llo".into()),
        ])
    );
}

#[test]
fn test_split_at_zero() {
    let result = run(r#"
import string
fn main() { string.split_at("hello", 0) }
"#);
    assert_eq!(
        result,
        Value::Tuple(vec![
            Value::String("".into()),
            Value::String("hello".into())
        ])
    );
}

#[test]
fn test_split_at_len() {
    let result = run(r#"
import string
fn main() { string.split_at("hello", 5) }
"#);
    assert_eq!(
        result,
        Value::Tuple(vec![
            Value::String("hello".into()),
            Value::String("".into())
        ])
    );
}

#[test]
fn test_split_at_unicode() {
    // "café" is 4 chars (c,a,f,é) but 5 bytes. split at char 3 => ("caf", "é").
    let result = run(r#"
import string
fn main() { string.split_at("café", 3) }
"#);
    assert_eq!(
        result,
        Value::Tuple(vec![Value::String("caf".into()), Value::String("é".into())])
    );
}

#[test]
fn test_split_at_negative_panics() {
    let err = run_err(
        r#"
import string
fn main() { string.split_at("hello", -1) }
"#,
    );
    assert!(
        err.contains("split_at") && err.contains("negative"),
        "expected split_at/negative error, got: {err}"
    );
}

#[test]
fn test_split_at_out_of_bounds_panics() {
    let err = run_err(
        r#"
import string
fn main() { string.split_at("hello", 99) }
"#,
    );
    assert!(
        err.contains("split_at") && err.contains("out of bounds"),
        "expected split_at/out of bounds error, got: {err}"
    );
}

// ── string.lines ───────────────────────────────────────────────────

fn strings(items: &[&str]) -> Value {
    Value::List(Arc::new(
        items.iter().map(|s| Value::String((*s).into())).collect(),
    ))
}

#[test]
fn test_lines_basic() {
    let result = run(r#"
import string
fn main() { string.lines("a\nb\nc") }
"#);
    assert_eq!(result, strings(&["a", "b", "c"]));
}

#[test]
fn test_lines_trailing_newline_no_empty_element() {
    let result = run(r#"
import string
fn main() { string.lines("a\nb\n") }
"#);
    assert_eq!(result, strings(&["a", "b"]));
}

#[test]
fn test_lines_crlf_stripped() {
    // Silt string literals don't support \r; build the CR character via
    // string.from_char_code(13) and interpolate it into the input.
    let result = run(r#"
import string
fn main() {
    let cr = string.from_char_code(13)
    string.lines("a{cr}\nb{cr}\nc")
}
"#);
    assert_eq!(result, strings(&["a", "b", "c"]));
}

#[test]
fn test_lines_crlf_trailing() {
    let result = run(r#"
import string
fn main() {
    let cr = string.from_char_code(13)
    string.lines("a{cr}\nb{cr}\n")
}
"#);
    assert_eq!(result, strings(&["a", "b"]));
}

#[test]
fn test_lines_empty_string_returns_empty_list() {
    let result = run(r#"
import string
fn main() { string.lines("") }
"#);
    assert_eq!(result, strings(&[]));
}

#[test]
fn test_lines_internal_blank_lines_kept() {
    // Empty lines in the middle are real lines and must be kept.
    let result = run(r#"
import string
fn main() { string.lines("a\n\nb") }
"#);
    assert_eq!(result, strings(&["a", "", "b"]));
}

#[test]
fn test_lines_no_newline_single_line() {
    let result = run(r#"
import string
fn main() { string.lines("hello") }
"#);
    assert_eq!(result, strings(&["hello"]));
}

// ── string.starts_with_at ──────────────────────────────────────────

#[test]
fn test_starts_with_at_zero_is_starts_with() {
    let result = run(r#"
import string
fn main() { string.starts_with_at("hello", 0, "hel") }
"#);
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn test_starts_with_at_middle_match() {
    let result = run(r#"
import string
fn main() { string.starts_with_at("hello", 2, "ll") }
"#);
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn test_starts_with_at_middle_no_match() {
    let result = run(r#"
import string
fn main() { string.starts_with_at("hello", 2, "lx") }
"#);
    assert_eq!(result, Value::Bool(false));
}

#[test]
fn test_starts_with_at_negative_offset_returns_false() {
    let result = run(r#"
import string
fn main() { string.starts_with_at("hello", -1, "h") }
"#);
    assert_eq!(result, Value::Bool(false));
}

#[test]
fn test_starts_with_at_past_end_returns_false() {
    let result = run(r#"
import string
fn main() { string.starts_with_at("hello", 99, "h") }
"#);
    assert_eq!(result, Value::Bool(false));
}

#[test]
fn test_starts_with_at_empty_prefix_true() {
    let result = run(r#"
import string
fn main() { string.starts_with_at("hello", 3, "") }
"#);
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn test_starts_with_at_unicode() {
    // "café☕!" — char index 4 is ☕.
    let result = run(r#"
import string
fn main() { string.starts_with_at("café☕!", 4, "☕!") }
"#);
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn test_starts_with_at_at_length_empty_prefix_true() {
    // Offset exactly == length(s) with empty prefix should be true
    // (treated as a valid position at end-of-string).
    let result = run(r#"
import string
fn main() { string.starts_with_at("hi", 2, "") }
"#);
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn test_starts_with_at_at_length_nonempty_prefix_false() {
    let result = run(r#"
import string
fn main() { string.starts_with_at("hi", 2, "x") }
"#);
    assert_eq!(result, Value::Bool(false));
}
