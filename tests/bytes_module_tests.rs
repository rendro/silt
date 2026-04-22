//! End-to-end tests for the `bytes` builtin module (v0.9 PR 1).
//!
//! Critical invariants locked here:
//! - Structural equality (two `from_string("x")` calls produce equal Bytes)
//! - Hash consistency with equality (Bytes works as Map/Set key)
//! - All 14 functions cover their happy path + key error cases
//! - Forward-compat: behavior here will not change when Bytes is later
//!   promoted to a language-level `Type::Bytes`.

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

// ── Constructors ───────────────────────────────────────────────────────

#[test]
fn test_empty_returns_zero_length() {
    let v = run(r#"
import bytes
fn main() { bytes.length(bytes.empty()) }
"#);
    assert_eq!(v, Value::Int(0));
}

#[test]
fn test_from_string_to_string_roundtrip_ascii() {
    let v = run(r#"
import bytes
fn main() {
  match bytes.to_string(bytes.from_string("hello")) {
    Ok(s) -> s
    Err(e) -> e.message()
  }
}
"#);
    assert_eq!(v, Value::String("hello".into()));
}

#[test]
fn test_from_string_to_string_roundtrip_multibyte_utf8() {
    let v = run(r#"
import bytes
fn main() {
  match bytes.to_string(bytes.from_string("café 🎉")) {
    Ok(s) -> s
    Err(e) -> e.message()
  }
}
"#);
    assert_eq!(v, Value::String("café 🎉".into()));
}

#[test]
fn test_to_string_invalid_utf8_returns_err() {
    // 0xff alone is not valid utf-8.
    let v = run(r#"
import bytes
fn main() {
  match bytes.from_list([255]) {
    Ok(b) -> match bytes.to_string(b) {
      Ok(_) -> "wrong: should have errored"
      Err(_) -> "ok"
    }
    Err(_) -> "wrong: from_list rejected"
  }
}
"#);
    assert_eq!(v, Value::String("ok".into()));
}

// ── Hex ────────────────────────────────────────────────────────────────

#[test]
fn test_from_hex_to_hex_roundtrip_lowercase() {
    let v = run(r#"
import bytes
fn main() {
  match bytes.from_hex("48656c6c6f") {
    Ok(b) -> bytes.to_hex(b)
    Err(e) -> e.message()
  }
}
"#);
    assert_eq!(v, Value::String("48656c6c6f".into()));
}

#[test]
fn test_from_hex_accepts_uppercase() {
    let v = run(r#"
import bytes
fn main() {
  match bytes.from_hex("DEADBEEF") {
    Ok(b) -> bytes.to_hex(b)
    Err(e) -> e.message()
  }
}
"#);
    assert_eq!(v, Value::String("deadbeef".into()));
}

#[test]
fn test_from_hex_odd_length_errors() {
    let v = run(r#"
import bytes
fn main() {
  match bytes.from_hex("abc") {
    Ok(_) -> "wrong: should error"
    Err(e) -> e.message()
  }
}
"#);
    let Value::String(s) = v else {
        panic!("expected String, got {v:?}")
    };
    assert!(s.contains("even length"), "got: {s}");
}

#[test]
fn test_from_hex_invalid_char_errors() {
    let v = run(r#"
import bytes
fn main() {
  match bytes.from_hex("xyzz") {
    Ok(_) -> "wrong: should error"
    Err(e) -> e.message()
  }
}
"#);
    let Value::String(s) = v else {
        panic!("expected String, got {v:?}")
    };
    assert!(s.contains("invalid hex"), "got: {s}");
}

#[test]
fn test_to_hex_empty() {
    let v = run(r#"
import bytes
fn main() { bytes.to_hex(bytes.empty()) }
"#);
    assert_eq!(v, Value::String("".into()));
}

// ── Base64 ─────────────────────────────────────────────────────────────

#[test]
fn test_base64_roundtrip() {
    let v = run(r#"
import bytes
fn main() {
  let original = bytes.from_string("hello world")
  let encoded = bytes.to_base64(original)
  match bytes.from_base64(encoded) {
    Ok(decoded) -> bytes.eq(original, decoded)
    Err(_) -> false
  }
}
"#);
    assert_eq!(v, Value::Bool(true));
}

#[test]
fn test_to_base64_known_value() {
    let v = run(r#"
import bytes
fn main() { bytes.to_base64(bytes.from_string("hello")) }
"#);
    assert_eq!(v, Value::String("aGVsbG8=".into()));
}

#[test]
fn test_from_base64_invalid_errors() {
    let v = run(r#"
import bytes
fn main() {
  match bytes.from_base64("!!!not base64!!!") {
    Ok(_) -> "wrong: should error"
    Err(e) -> e.message()
  }
}
"#);
    let Value::String(s) = v else {
        panic!("expected String, got {v:?}")
    };
    assert!(s.contains("invalid base64"), "got: {s}");
}

// ── List conversion ───────────────────────────────────────────────────

#[test]
fn test_from_list_to_list_roundtrip() {
    let v = run(r#"
import bytes
fn main() {
  match bytes.from_list([72, 105, 33]) {
    Ok(b) -> bytes.to_list(b)
    Err(_) -> []
  }
}
"#);
    assert_eq!(
        v,
        Value::List(Arc::new(vec![
            Value::Int(72),
            Value::Int(105),
            Value::Int(33),
        ]))
    );
}

#[test]
fn test_from_list_byte_too_large_errors() {
    let v = run(r#"
import bytes
fn main() {
  match bytes.from_list([10, 256, 30]) {
    Ok(_) -> "wrong: should error"
    Err(e) -> e.message()
  }
}
"#);
    let Value::String(s) = v else {
        panic!("expected String, got {v:?}")
    };
    assert!(s.contains("256") && s.contains("out of range"), "got: {s}");
}

#[test]
fn test_from_list_negative_byte_errors() {
    let v = run(r#"
import bytes
fn main() {
  match bytes.from_list([0, -1, 0]) {
    Ok(_) -> "wrong: should error"
    Err(e) -> e.message()
  }
}
"#);
    let Value::String(s) = v else {
        panic!("expected String, got {v:?}")
    };
    assert!(s.contains("out of range"), "got: {s}");
}

// ── Length / slice / get ───────────────────────────────────────────────

#[test]
fn test_length_empty_and_nonempty() {
    let v = run(r#"
import bytes
fn main() {
  let a = bytes.length(bytes.empty())
  let b = bytes.length(bytes.from_string("hello"))
  [a, b]
}
"#);
    assert_eq!(v, Value::List(Arc::new(vec![Value::Int(0), Value::Int(5)])));
}

#[test]
fn test_slice_basic() {
    let v = run(r#"
import bytes
fn main() {
  match bytes.slice(bytes.from_string("hello world"), 6, 11) {
    Ok(s) -> match bytes.to_string(s) { Ok(t) -> t, Err(e) -> e }
    Err(e) -> e.message()
  }
}
"#);
    assert_eq!(v, Value::String("world".into()));
}

#[test]
fn test_slice_empty_range() {
    let v = run(r#"
import bytes
fn main() {
  match bytes.slice(bytes.from_string("hello"), 2, 2) {
    Ok(s) -> bytes.length(s)
    Err(_) -> -1
  }
}
"#);
    assert_eq!(v, Value::Int(0));
}

#[test]
fn test_slice_start_after_end_errors() {
    let v = run(r#"
import bytes
fn main() {
  match bytes.slice(bytes.from_string("hello"), 4, 2) {
    Ok(_) -> "wrong"
    Err(e) -> e.message()
  }
}
"#);
    let Value::String(s) = v else {
        panic!("expected String, got {v:?}")
    };
    assert!(s.contains("out of bounds"), "got: {s}");
}

#[test]
fn test_slice_end_out_of_bounds_errors() {
    let v = run(r#"
import bytes
fn main() {
  match bytes.slice(bytes.from_string("hello"), 0, 99) {
    Ok(_) -> "wrong"
    Err(e) -> e.message()
  }
}
"#);
    let Value::String(s) = v else {
        panic!("expected String, got {v:?}")
    };
    assert!(s.contains("out of bounds"), "got: {s}");
}

#[test]
fn test_get_basic() {
    let v = run(r#"
import bytes
fn main() {
  match bytes.get(bytes.from_string("hello"), 1) {
    Ok(n) -> n
    Err(_) -> -1
  }
}
"#);
    assert_eq!(v, Value::Int(101)); // 'e'
}

#[test]
fn test_get_out_of_bounds_errors() {
    let v = run(r#"
import bytes
fn main() {
  match bytes.get(bytes.from_string("hi"), 5) {
    Ok(_) -> "wrong"
    Err(e) -> e.message()
  }
}
"#);
    let Value::String(s) = v else {
        panic!("expected String, got {v:?}")
    };
    assert!(s.contains("out of bounds"), "got: {s}");
}

// ── Concat ─────────────────────────────────────────────────────────────

#[test]
fn test_concat_basic() {
    let v = run(r#"
import bytes
fn main() {
  let a = bytes.from_string("hello ")
  let b = bytes.from_string("world")
  match bytes.to_string(bytes.concat(a, b)) {
    Ok(s) -> s
    Err(e) -> e.message()
  }
}
"#);
    assert_eq!(v, Value::String("hello world".into()));
}

#[test]
fn test_concat_with_empty() {
    let v = run(r#"
import bytes
fn main() {
  let a = bytes.from_string("hello")
  let e = bytes.empty()
  bytes.eq(bytes.concat(a, e), a)
}
"#);
    assert_eq!(v, Value::Bool(true));
}

#[test]
fn test_concat_all_empty_list() {
    let v = run(r#"
import bytes
fn main() { bytes.length(bytes.concat_all([])) }
"#);
    assert_eq!(v, Value::Int(0));
}

#[test]
fn test_concat_all_many() {
    let v = run(r#"
import bytes
fn main() {
  let parts = [
    bytes.from_string("a"),
    bytes.from_string("bb"),
    bytes.from_string("ccc"),
  ]
  match bytes.to_string(bytes.concat_all(parts)) {
    Ok(s) -> s
    Err(e) -> e.message()
  }
}
"#);
    assert_eq!(v, Value::String("abbccc".into()));
}

// ── Equality (the load-bearing forward-compat invariants) ─────────────

#[test]
fn test_structural_equality_via_eq_function() {
    // Two separately-constructed Bytes with identical content must be equal.
    // This is the contract that lets Bytes promote to a native value type
    // later without breaking existing programs.
    let v = run(r#"
import bytes
fn main() {
  let a = bytes.from_string("hello")
  let b = bytes.from_string("hello")
  bytes.eq(a, b)
}
"#);
    assert_eq!(v, Value::Bool(true));
}

#[test]
fn test_structural_equality_via_native_eq_operator() {
    // The silt `==` operator must use the same structural semantics as
    // `bytes.eq`. (PartialEq impl in src/value.rs.)
    let v = run(r#"
import bytes
fn main() {
  let a = bytes.from_string("hello")
  let b = bytes.from_string("hello")
  let c = bytes.from_string("world")
  [a == b, a == c]
}
"#);
    assert_eq!(
        v,
        Value::List(Arc::new(vec![Value::Bool(true), Value::Bool(false)]))
    );
}

#[test]
fn test_inequality_different_length() {
    let v = run(r#"
import bytes
fn main() {
  bytes.eq(bytes.from_string("hi"), bytes.from_string("hello"))
}
"#);
    assert_eq!(v, Value::Bool(false));
}

#[test]
fn test_empty_bytes_eq_empty_bytes() {
    let v = run(r#"
import bytes
fn main() { bytes.eq(bytes.empty(), bytes.empty()) }
"#);
    assert_eq!(v, Value::Bool(true));
}

#[test]
fn test_bytes_works_as_map_key() {
    // Hash + Eq consistency: two equal Bytes values used as Map keys must
    // collapse to one entry. This is the invariant that protects BTreeMap
    // / BTreeSet correctness.
    let v = run(r#"
import bytes
fn main() {
  let m = #{
    bytes.from_string("a"): 1,
    bytes.from_string("a"): 2,
    bytes.from_string("b"): 3,
  }
  -- Map literal evaluation order: later entries overwrite earlier;
  -- the "a" entry should be a single slot now holding 2.
  m
}
"#);
    let Value::Map(m) = v else {
        panic!("expected Map, got {v:?}")
    };
    assert_eq!(m.len(), 2, "duplicate Bytes keys must collapse");
}

// ── Type-level integration ────────────────────────────────────────────

#[test]
fn test_typechecker_accepts_bytes_signatures() {
    // No type errors when passing values through the bytes module.
    let errs = type_errors(
        r#"
import bytes
fn main() {
  let a = bytes.from_string("x")
  let b = bytes.concat(a, bytes.empty())
  let n = bytes.length(b)
  let _ = bytes.eq(a, b)
  let _ = n + 1
}
"#,
    );
    assert!(errs.is_empty(), "got: {errs:?}");
}

// ── Search / prefix / suffix / split ─────────────────────────────────

#[test]
fn test_index_of_found() {
    let v = run(r#"
import bytes
fn main() {
  let hay = bytes.from_string("hello world")
  let needle = bytes.from_string("world")
  match bytes.index_of(hay, needle) {
    Some(i) -> i
    None -> -1
  }
}
"#);
    assert_eq!(v, Value::Int(6));
}

#[test]
fn test_index_of_not_found() {
    let v = run(r#"
import bytes
fn main() {
  match bytes.index_of(bytes.from_string("hello"), bytes.from_string("xyz")) {
    Some(_) -> 1
    None -> 0
  }
}
"#);
    assert_eq!(v, Value::Int(0));
}

#[test]
fn test_index_of_empty_needle_is_zero() {
    let v = run(r#"
import bytes
fn main() {
  match bytes.index_of(bytes.from_string("hello"), bytes.empty()) {
    Some(i) -> i
    None -> -1
  }
}
"#);
    assert_eq!(v, Value::Int(0));
}

#[test]
fn test_index_of_first_occurrence() {
    let v = run(r#"
import bytes
fn main() {
  match bytes.index_of(bytes.from_string("abcabc"), bytes.from_string("b")) {
    Some(i) -> i
    None -> -1
  }
}
"#);
    assert_eq!(v, Value::Int(1));
}

#[test]
fn test_starts_with_true() {
    let v = run(r#"
import bytes
fn main() {
  bytes.starts_with(bytes.from_string("hello world"), bytes.from_string("hello"))
}
"#);
    assert_eq!(v, Value::Bool(true));
}

#[test]
fn test_starts_with_false() {
    let v = run(r#"
import bytes
fn main() {
  bytes.starts_with(bytes.from_string("hello"), bytes.from_string("world"))
}
"#);
    assert_eq!(v, Value::Bool(false));
}

#[test]
fn test_starts_with_empty_prefix_is_true() {
    let v = run(r#"
import bytes
fn main() {
  [
    bytes.starts_with(bytes.from_string("hello"), bytes.empty()),
    bytes.starts_with(bytes.empty(), bytes.empty()),
  ]
}
"#);
    assert_eq!(
        v,
        Value::List(Arc::new(vec![Value::Bool(true), Value::Bool(true)]))
    );
}

#[test]
fn test_starts_with_prefix_longer_than_bytes() {
    let v = run(r#"
import bytes
fn main() {
  bytes.starts_with(bytes.from_string("hi"), bytes.from_string("hi there"))
}
"#);
    assert_eq!(v, Value::Bool(false));
}

#[test]
fn test_ends_with_true() {
    let v = run(r#"
import bytes
fn main() {
  bytes.ends_with(bytes.from_string("hello world"), bytes.from_string("world"))
}
"#);
    assert_eq!(v, Value::Bool(true));
}

#[test]
fn test_ends_with_false() {
    let v = run(r#"
import bytes
fn main() {
  bytes.ends_with(bytes.from_string("hello"), bytes.from_string("world"))
}
"#);
    assert_eq!(v, Value::Bool(false));
}

#[test]
fn test_ends_with_empty_suffix_is_true() {
    let v = run(r#"
import bytes
fn main() {
  [
    bytes.ends_with(bytes.from_string("hello"), bytes.empty()),
    bytes.ends_with(bytes.empty(), bytes.empty()),
  ]
}
"#);
    assert_eq!(
        v,
        Value::List(Arc::new(vec![Value::Bool(true), Value::Bool(true)]))
    );
}

#[test]
fn test_split_basic() {
    let v = run(r#"
import bytes
import list
fn main() {
  let parts = bytes.split(bytes.from_string("a,b,c"), bytes.from_string(","))
  list.map(parts) { p ->
    match bytes.to_string(p) {
      Ok(s) -> s
      Err(e) -> e.message()
    }
  }
}
"#);
    assert_eq!(
        v,
        Value::List(Arc::new(vec![
            Value::String("a".into()),
            Value::String("b".into()),
            Value::String("c".into()),
        ]))
    );
}

#[test]
fn test_split_no_separator_occurrence() {
    let v = run(r#"
import bytes
import list
fn main() {
  let parts = bytes.split(bytes.from_string("hello"), bytes.from_string(","))
  list.length(parts)
}
"#);
    assert_eq!(v, Value::Int(1));
}

#[test]
fn test_split_empty_bytes_yields_single_empty() {
    // Consistent with Rust's str::split / silt's string.split on empty input.
    let v = run(r#"
import bytes
import list
fn main() {
  let parts = bytes.split(bytes.empty(), bytes.from_string(","))
  let first_len = match list.head(parts) {
    Some(b) -> bytes.length(b)
    None -> -1
  }
  (list.length(parts), first_len)
}
"#);
    // Expect length 1 with the single element having length 0.
    assert_eq!(v, Value::Tuple(vec![Value::Int(1), Value::Int(0)]));
}

#[test]
fn test_split_consecutive_separators_produce_empties() {
    let v = run(r#"
import bytes
import list
fn main() {
  let parts = bytes.split(bytes.from_string("a,,b"), bytes.from_string(","))
  list.length(parts)
}
"#);
    assert_eq!(v, Value::Int(3));
}

#[test]
fn test_split_multibyte_separator() {
    let v = run(r#"
import bytes
import list
fn main() {
  let parts = bytes.split(bytes.from_string("foo::bar::baz"), bytes.from_string("::"))
  list.length(parts)
}
"#);
    assert_eq!(v, Value::Int(3));
}

#[test]
fn test_split_empty_sep_errors() {
    // Empty separator is ambiguous; runtime must panic/error rather than
    // silently infinite-loop.
    let tokens = silt::lexer::Lexer::new(
        r#"
import bytes
fn main() {
  bytes.split(bytes.from_string("hello"), bytes.empty())
}
"#,
    )
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
    let err = vm.run(script).expect_err("expected runtime error");
    let msg = format!("{err}");
    assert!(
        msg.contains("non-empty") || msg.contains("empty"),
        "got: {msg}"
    );
}

#[test]
fn test_typechecker_rejects_wrong_arg_type() {
    // bytes.length takes Bytes, not String.
    let errs = type_errors(
        r#"
import bytes
fn main() {
  bytes.length("not bytes")
}
"#,
    );
    assert!(!errs.is_empty(), "expected a type error");
}
