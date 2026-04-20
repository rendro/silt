//! End-to-end tests for the `toml` builtin stdlib module.
//!
//! Mirrors the `json` module tests in `tests/integration.rs` to make the
//! behavioural symmetry between the two modules easy to compare at a glance.
//! The invariants locked here:
//! - Typed parse (`toml.parse`) for scalars, records, nested records, lists
//!   inside records.
//! - `toml.parse_list` over a single `[[items]]` section.
//! - `toml.parse_map` for a top-level table into `Map(String, V)`.
//! - Error messages name the offending field on type mismatch, and the field
//!   name on missing-required-field.
//! - Round-trip `parse → stringify → parse` preserves record values.
//! - Date fields parse from TOML's native datetime literal into `Date`.

use std::sync::Arc;

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

// ── 1. Basic record round-trip ──────────────────────────────────────

#[test]
fn test_toml_parse_basic_record() {
    // name: String, age: Int, active: Bool — the baseline scalar mix.
    let result = run(r#"
import toml
type User { name: String, age: Int, active: Bool }
fn main() {
  let input = "name = \"Alice\"\nage = 30\nactive = true\n"
  match toml.parse(User, input) {
    Ok(u) -> u.name
    Err(_) -> "fail"
  }
}
"#);
    assert_eq!(result, Value::String("Alice".into()));
}

#[test]
fn test_toml_parse_basic_record_all_fields() {
    // Verify the record's Int and Bool fields round-trip to the expected
    // scalar values (no string conversion required — check each field
    // directly by selecting a distinct return type per branch).
    let age = run(r#"
import toml
type User { name: String, age: Int, active: Bool }
fn main() {
  let input = "name = \"Alice\"\nage = 30\nactive = true\n"
  match toml.parse(User, input) {
    Ok(u) -> u.age
    Err(_) -> -1
  }
}
"#);
    assert_eq!(age, Value::Int(30));

    let active = run(r#"
import toml
type User { name: String, age: Int, active: Bool }
fn main() {
  let input = "name = \"Alice\"\nage = 30\nactive = true\n"
  match toml.parse(User, input) {
    Ok(u) -> u.active
    Err(_) -> false
  }
}
"#);
    assert_eq!(active, Value::Bool(true));
}

// ── 2. Missing-required-field error ─────────────────────────────────

#[test]
fn test_toml_parse_missing_required_field_error() {
    let result = run(r#"
import toml
type User { name: String, age: Int }
fn main() {
  let input = "name = \"Alice\"\n"
  match toml.parse(User, input) {
    Ok(_) -> "unexpected"
    Err(e) -> e
  }
}
"#);
    assert_eq!(
        result,
        Value::String("toml.parse(User): missing field 'age'".into())
    );
}

// ── 3. Type-mismatch error names the field ──────────────────────────

#[test]
fn test_toml_parse_type_mismatch_names_field() {
    let result = run(r#"
import toml
type User { name: String, age: Int }
fn main() {
  -- 'name' is provided as an integer instead of a string.
  let input = "name = 42\nage = 30\n"
  match toml.parse(User, input) {
    Ok(_) -> "unexpected"
    Err(e) -> e
  }
}
"#);
    let msg = match result {
        Value::String(s) => s,
        other => panic!("expected String error, got {other:?}"),
    };
    assert!(msg.contains("field 'name'"), "error should name field 'name': {msg}");
    assert!(
        msg.contains("expected String"),
        "error should say expected String: {msg}"
    );
}

// ── 4. parse_list over a single [[items]] section ───────────────────

#[test]
fn test_toml_parse_list_array_of_tables() {
    let result = run(r#"
import toml
import list
type Point { x: Int, y: Int }
fn main() {
  let input = "[[points]]\nx = 1\ny = 2\n\n[[points]]\nx = 3\ny = 4\n"
  match toml.parse_list(Point, input) {
    Ok(pts) -> list.length(pts)
    Err(_) -> 0
  }
}
"#);
    assert_eq!(result, Value::Int(2));
}

// ── 5. parse_map for String→Int ─────────────────────────────────────

#[test]
fn test_toml_parse_map_string_to_int() {
    let result = run(r#"
import toml
import map
fn main() {
  let input = "x = 10\ny = 20\n"
  match toml.parse_map(Int, input) {
    Ok(m) -> map.length(m)
    Err(_) -> -1
  }
}
"#);
    assert_eq!(result, Value::Int(2));
}

#[test]
fn test_toml_parse_map_string_to_int_get() {
    let result = run(r#"
import toml
import map
fn main() {
  let input = "x = 10\ny = 20\n"
  match toml.parse_map(Int, input) {
    Ok(m) -> match map.get(m, "x") {
      Some(v) -> v
      None -> -1
    }
    Err(_) -> -2
  }
}
"#);
    assert_eq!(result, Value::Int(10));
}

// ── 6. Round-trip parse → stringify → parse ─────────────────────────

#[test]
fn test_toml_stringify_round_trip() {
    // Parse a record, stringify it, re-parse the output, then compare a
    // scalar field. The stringify result is wrapped in Ok(...) since
    // top-level table restrictions are surfaced via Result.
    let result = run(r#"
import toml
type User { name: String, age: Int, active: Bool }
fn main() {
  let input = "name = \"Alice\"\nage = 30\nactive = true\n"
  match toml.parse(User, input) {
    Ok(u1) -> match toml.stringify(u1) {
      Ok(s) -> match toml.parse(User, s) {
        Ok(u2) -> u2.name
        Err(_) -> "parse2 failed"
      }
      Err(_) -> "stringify failed"
    }
    Err(_) -> "parse1 failed"
  }
}
"#);
    assert_eq!(result, Value::String("Alice".into()));
}

// ── 7. Nested records serialise as nested tables ────────────────────

#[test]
fn test_toml_nested_record_round_trip() {
    let result = run(r#"
import toml
type Address { city: String, zip: String }
type User { name: String, address: Address }
fn main() {
  let input = "name = \"Alice\"\n\n[address]\ncity = \"NYC\"\nzip = \"10001\"\n"
  match toml.parse(User, input) {
    Ok(u) -> u.address.city
    Err(_) -> "fail"
  }
}
"#);
    assert_eq!(result, Value::String("NYC".into()));
}

#[test]
fn test_toml_nested_record_stringify_produces_nested_table() {
    // stringify of a record with a nested record field should produce
    // TOML that contains a [address] section header (native nested-table
    // syntax) — not an inline object.
    let result = run(r#"
import toml
type Address { city: String, zip: String }
type User { name: String, address: Address }
fn main() {
  let input = "name = \"Alice\"\n\n[address]\ncity = \"NYC\"\nzip = \"10001\"\n"
  match toml.parse(User, input) {
    Ok(u) -> match toml.stringify(u) {
      Ok(s) -> s
      Err(_) -> "stringify failed"
    }
    Err(_) -> "parse failed"
  }
}
"#);
    let s = match result {
        Value::String(s) => s,
        other => panic!("expected String, got {other:?}"),
    };
    assert!(s.contains("[address]"), "expected nested-table header: {s}");
    assert!(s.contains("city = \"NYC\""), "expected city field: {s}");
    assert!(s.contains("zip = \"10001\""), "expected zip field: {s}");
}

// ── 8. Date-ish field: TOML native date literal → time.Date ─────────

#[test]
fn test_toml_parse_date_field_native_literal() {
    // TOML's native `1979-05-27` literal parses into a `Date` record field.
    let result = run(r#"
import toml
import time
type Event { name: String, date: Date }
fn main() {
  let input = "name = \"launch\"\ndate = 1979-05-27\n"
  match toml.parse(Event, input) {
    Ok(e) -> e.date.year
    Err(err) -> -1
  }
}
"#);
    assert_eq!(result, Value::Int(1979));
}

#[test]
fn test_toml_parse_date_field_month_day() {
    let result = run(r#"
import toml
import time
type Event { name: String, date: Date }
fn main() {
  let input = "name = \"launch\"\ndate = 1979-05-27\n"
  match toml.parse(Event, input) {
    Ok(e) -> e.date.month * 100 + e.date.day
    Err(_) -> -1
  }
}
"#);
    assert_eq!(result, Value::Int(5 * 100 + 27));
}

// ── Extra: Option fields default to None when missing ───────────────

#[test]
fn test_toml_parse_option_field_missing_defaults_none() {
    let result = run(r#"
import toml
type User { name: String, nickname: Option(String) }
fn main() {
  let input = "name = \"Alice\"\n"
  match toml.parse(User, input) {
    Ok(u) -> u.nickname
    Err(_) -> Some("fail")
  }
}
"#);
    assert_eq!(result, Value::Variant("None".into(), vec![]));
}

// ── Extra: parse_map over a record value type ───────────────────────

#[test]
fn test_toml_parse_map_record_values() {
    let result = run(r#"
import toml
import map
type Item { qty: Int, unit: String }
fn main() {
  let input = "[apples]\nqty = 5\nunit = \"kg\"\n\n[bananas]\nqty = 7\nunit = \"each\"\n"
  match toml.parse_map(Item, input) {
    Ok(m) -> map.length(m)
    Err(_) -> -1
  }
}
"#);
    assert_eq!(result, Value::Int(2));
}

// ── Extra: invalid TOML surfaces as Err, not a panic ────────────────

#[test]
fn test_toml_parse_invalid_toml_surfaces_err() {
    let result = run(r#"
import toml
type User { name: String }
fn main() {
  match toml.parse(User, "not = = toml") {
    Ok(_) -> false
    Err(_) -> true
  }
}
"#);
    assert_eq!(result, Value::Bool(true));
}
