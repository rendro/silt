use super::super::test_helpers::*;
use super::super::*;

// ── Builtin registration completeness ───────────────────────────

#[test]
fn test_register_builtins_populates_env() {
    let mut checker = TypeChecker::new();
    let mut env = TypeEnv::new();
    checker.register_builtins(&mut env);
    // Core functions should be registered
    assert!(
        env.lookup(intern("print")).is_some(),
        "print not registered"
    );
    assert!(
        env.lookup(intern("println")).is_some(),
        "println not registered"
    );
    assert!(
        env.lookup(intern("panic")).is_some(),
        "panic not registered"
    );
    assert!(env.lookup(intern("Some")).is_some(), "Some not registered");
    assert!(env.lookup(intern("None")).is_some(), "None not registered");
}

#[test]
fn test_builtin_type_signatures_returns_qualified_names() {
    let sigs = builtin_type_signatures();
    assert!(
        sigs.contains_key("list.map"),
        "list.map missing from signatures"
    );
    assert!(
        sigs.contains_key("string.split"),
        "string.split missing from signatures"
    );
    assert!(
        sigs.contains_key("math.sqrt"),
        "math.sqrt missing from signatures"
    );
    // Should not contain unqualified names
    assert!(
        !sigs.contains_key("print"),
        "unqualified 'print' should not be in qualified signatures"
    );
}

// ── Math module ─────────────────────────────────────────────────

#[test]
fn test_math_sqrt() {
    assert_no_errors(
        r#"
import math
fn main() {
  let x = math.sqrt(4.0)
  x
}
        "#,
    );
}

#[test]
fn test_math_trig_functions() {
    assert_no_errors(
        r#"
import math
fn main() {
  let a = math.sin(1.0)
  let b = math.cos(1.0)
  let c = math.tan(1.0)
  a + b + c
}
        "#,
    );
}

#[test]
fn test_math_pow() {
    assert_no_errors(
        r#"
import math
fn main() {
  math.pow(2.0, 10.0)
}
        "#,
    );
}

// ── Time module ─────────────────────────────────────────────────

#[test]
fn test_time_now() {
    assert_no_errors(
        r#"
import time
fn main() {
  time.now()
}
        "#,
    );
}

#[test]
fn test_time_sleep() {
    assert_no_errors(
        r#"
import time
fn main() {
  time.sleep(time.ms(100))
}
        "#,
    );
}

// ── HTTP module ─────────────────────────────────────────────────

#[test]
fn test_http_get_type() {
    assert_no_errors(
        r#"
import http
fn main() {
  http.get("http://example.com")
}
        "#,
    );
}

// ── FS module ───────────────────────────────────────────────────

#[test]
fn test_fs_exists_type_check() {
    assert_no_errors(
        r#"
import fs
fn main() {
  fs.exists("file.txt")
}
        "#,
    );
}

// ── Test module ─────────────────────────────────────────────────

#[test]
fn test_test_module_assert_eq() {
    assert_no_errors(
        r#"
import test
fn main() {
  test.assert_eq(1, 1)
}
        "#,
    );
}

// ── Builtin type mismatches ─────────────────────────────────────

#[test]
fn test_math_sqrt_wrong_type() {
    assert_has_error(
        r#"
import math
fn main() {
  math.sqrt("hello")
}
        "#,
        "type mismatch",
    );
}
