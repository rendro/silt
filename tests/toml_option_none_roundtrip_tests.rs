//! Regression tests for the TOML `Option::None` round-trip data-corruption bug.
//!
//! `toml.stringify` used to emit `Option::None` record fields as `field = ""`
//! (an empty-string placeholder) instead of omitting them. On parse-back this
//! silently corrupted data:
//!   - `Option(String)` None -> `Some("")` (silent corruption, no error).
//!   - `Option(Int)`    None -> `age = ""` -> type-mismatch parse error.
//!
//! The fix omits `None` fields from the emitted table; the TOML parser already
//! defaults missing Option fields to None. These tests exercise the full
//! runtime path (compile + run a silt program) and assert the program output.

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

// ── Option(Int) None round-trips to None (was: parse error) ─────────────

#[test]
fn test_toml_option_int_none_roundtrips_to_none() {
    let result = run(r#"
import toml
type Rec { name: String, age: Option(Int) }
fn main() {
  let r = Rec { name: "x", age: None }
  match toml.stringify(r) {
    Ok(s) -> match toml.parse(s, Rec) {
      Ok(v) -> match v.age {
        Some(n) -> "CORRUPT"
        None -> "None ok"
      }
      Err(e) -> "PARSE ERR"
    }
    Err(e) -> "STRINGIFY ERR"
  }
}
"#);
    assert_eq!(result, Value::String("None ok".into()));
}

// ── Option(String) None round-trips to None (was: Some("")) ─────────────

#[test]
fn test_toml_option_string_none_roundtrips_to_none() {
    let result = run(r#"
import toml
type Rec { name: String, nickname: Option(String) }
fn main() {
  let r = Rec { name: "x", nickname: None }
  match toml.stringify(r) {
    Ok(s) -> match toml.parse(s, Rec) {
      Ok(v) -> match v.nickname {
        Some(n) -> "CORRUPT: {n}"
        None -> "None ok"
      }
      Err(e) -> "PARSE ERR"
    }
    Err(e) -> "STRINGIFY ERR"
  }
}
"#);
    assert_eq!(result, Value::String("None ok".into()));
}

// ── Some(v) field still round-trips its inner value ─────────────────────

#[test]
fn test_toml_option_int_some_roundtrips_value() {
    let result = run(r#"
import toml
type Rec { name: String, age: Option(Int) }
fn main() {
  let r = Rec { name: "x", age: Some(42) }
  match toml.stringify(r) {
    Ok(s) -> match toml.parse(s, Rec) {
      Ok(v) -> match v.age {
        Some(n) -> n
        None -> -1
      }
      Err(e) -> -2
    }
    Err(e) -> -3
  }
}
"#);
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_toml_option_string_some_roundtrips_value() {
    let result = run(r#"
import toml
type Rec { name: String, nickname: Option(String) }
fn main() {
  let r = Rec { name: "x", nickname: Some("bob") }
  match toml.stringify(r) {
    Ok(s) -> match toml.parse(s, Rec) {
      Ok(v) -> match v.nickname {
        Some(n) -> n
        None -> "none"
      }
      Err(e) -> "parse err"
    }
    Err(e) -> "stringify err"
  }
}
"#);
    assert_eq!(result, Value::String("bob".into()));
}

// ── A record mixing None, Some, and a required field ────────────────────

#[test]
fn test_toml_mixed_none_some_required_roundtrip() {
    // age = None (omitted), nickname = Some("bob"), name required.
    // Encode all three outcomes into one string so a single assert proves
    // the whole record round-trips correctly.
    let result = run(r#"
import toml
type Rec { name: String, age: Option(Int), nickname: Option(String) }
fn main() {
  let r = Rec { name: "alice", age: None, nickname: Some("bob") }
  match toml.stringify(r) {
    Ok(s) -> match toml.parse(s, Rec) {
      Ok(v) -> {
        let age_part = match v.age {
          Some(n) -> "age=CORRUPT"
          None -> "age=None"
        }
        let nick_part = match v.nickname {
          Some(nk) -> "nick={nk}"
          None -> "nick=MISSING"
        }
        "{v.name}|{age_part}|{nick_part}"
      }
      Err(e) -> "PARSE ERR"
    }
    Err(e) -> "STRINGIFY ERR"
  }
}
"#);
    assert_eq!(result, Value::String("alice|age=None|nick=bob".into()));
}
