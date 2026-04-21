//! End-to-end tests for the `env` builtin module: `get`, `set`,
//! `remove`, and `vars`.
//!
//! These tests mutate the process environment and therefore need
//! unique per-test variable names (prefixed `SILT_TEST_ENV_*`) so they
//! don't race with each other if two env tests happen to run in
//! parallel in the same test binary.

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

fn expect_string(v: Value) -> String {
    match v {
        Value::String(s) => s,
        other => panic!("expected String, got {other:?}"),
    }
}

/// `env.remove` on a variable that was never set must succeed silently
/// (idempotent contract).
#[test]
fn test_env_remove_nonexistent_is_idempotent() {
    // Pre-clear in case a prior test in the same process left something
    // behind. SAFETY: main thread at test entry.
    unsafe { std::env::remove_var("SILT_TEST_ENV_NEVER_EXISTED_XYZ") };
    let v = run(
        r#"
import env
fn main() {
  env.remove("SILT_TEST_ENV_NEVER_EXISTED_XYZ")
  match env.get("SILT_TEST_ENV_NEVER_EXISTED_XYZ") {
    Some(_) -> "unexpected: still set"
    None -> "ok"
  }
}
"#,
    );
    assert_eq!(expect_string(v), "ok");
}

/// `env.set` then `env.remove` then `env.get` must round-trip to None.
#[test]
fn test_env_remove_after_set_clears_value() {
    let v = run(
        r#"
import env
fn main() {
  env.set("SILT_TEST_ENV_ROUND_TRIP", "hello")
  env.remove("SILT_TEST_ENV_ROUND_TRIP")
  match env.get("SILT_TEST_ENV_ROUND_TRIP") {
    Some(_) -> "fail"
    None -> "ok"
  }
}
"#,
    );
    assert_eq!(expect_string(v), "ok");
}

/// `env.vars` must contain a variable we just set. We don't pin the
/// list length because every CI image has a different number of base
/// env vars; instead we fold and match on the key.
#[test]
fn test_env_vars_contains_newly_set() {
    unsafe { std::env::remove_var("SILT_TEST_ENV_VARS_PROBE") };
    let v = run(
        r#"
import env
import list
fn main() {
  env.set("SILT_TEST_ENV_VARS_PROBE", "42")
  let all = env.vars()
  list.fold(all, "missing") { acc, pair -> match pair {
    (k, v) -> match k == "SILT_TEST_ENV_VARS_PROBE" {
      true -> v
      false -> acc
    }
  } }
}
"#,
    );
    assert_eq!(expect_string(v), "42");
    // Clean up so a later test doesn't see it.
    unsafe { std::env::remove_var("SILT_TEST_ENV_VARS_PROBE") };
}

/// After `env.remove`, the probe must no longer appear in `env.vars`.
#[test]
fn test_env_vars_reflects_remove() {
    // Set via the libc side so we don't depend on env.set semantics in
    // this specific test.
    unsafe { std::env::set_var("SILT_TEST_ENV_VARS_REMOVED", "present") };
    let v = run(
        r#"
import env
import list
fn main() {
  env.remove("SILT_TEST_ENV_VARS_REMOVED")
  let all = env.vars()
  list.fold(all, "missing") { acc, pair -> match pair {
    (k, _) -> match k == "SILT_TEST_ENV_VARS_REMOVED" {
      true -> "still-there"
      false -> acc
    }
  } }
}
"#,
    );
    assert_eq!(expect_string(v), "missing");
}

/// `env.vars` returns a list of 2-tuples: exercise the shape once so
/// a regression on the tuple wrapping is caught at the language level.
#[test]
fn test_env_vars_shape_is_pairs() {
    unsafe { std::env::set_var("SILT_TEST_ENV_SHAPE_PROBE", "xy") };
    let v = run(
        r#"
import env
import list
import string
fn main() {
  let all = env.vars()
  list.fold(all, 0) { acc, pair -> match pair {
    (k, v) -> match k == "SILT_TEST_ENV_SHAPE_PROBE" {
      true -> string.length(v)
      false -> acc
    }
  } }
}
"#,
    );
    // "xy" is two characters.
    assert_eq!(v, Value::Int(2));
    unsafe { std::env::remove_var("SILT_TEST_ENV_SHAPE_PROBE") };
}

/// Docs ↔ registration cross-check, mirroring the encoding module's
/// test. Every function registered for `env` must be present in the
/// shared io-fs docs page.
#[test]
fn test_documented_env_functions_match_registration() {
    let doc_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("docs")
        .join("stdlib")
        .join("io-fs.md");
    let body = std::fs::read_to_string(&doc_path).expect("failed to read docs/stdlib/io-fs.md");
    let expected = silt::module::builtin_module_functions("env");
    assert!(
        !expected.is_empty(),
        "module::builtin_module_functions(\"env\") returned empty — registration is missing"
    );
    for name in &expected {
        let qualified = format!("env.{}", name);
        assert!(
            body.contains(&qualified),
            "docs/stdlib/io-fs.md does not mention `env.{name}`"
        );
    }
}
