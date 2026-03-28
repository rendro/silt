use std::fs;
use std::path::PathBuf;

use silt::interpreter::Interpreter;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::value::Value;

/// Helper: create a temp directory with module files, parse and run the main program.
fn run_module_test(files: &[(&str, &str)], main_source: &str) -> Value {
    let dir = tempdir();

    // Write all module files
    for (name, content) in files {
        let path = dir.join(name);
        fs::write(&path, content).expect("failed to write module file");
    }

    // Parse and run the main source
    let tokens = Lexer::new(main_source).tokenize().expect("lexer error");
    let program = Parser::new(tokens).parse_program().expect("parse error");
    let mut interp = Interpreter::with_project_root(dir.clone());
    interp.run(&program).expect("runtime error")
}

fn run_module_test_err(files: &[(&str, &str)], main_source: &str) -> String {
    let dir = tempdir();

    for (name, content) in files {
        let path = dir.join(name);
        fs::write(&path, content).expect("failed to write module file");
    }

    let tokens = Lexer::new(main_source).tokenize().expect("lexer error");
    let program = Parser::new(tokens).parse_program().expect("parse error");
    let mut interp = Interpreter::with_project_root(dir.clone());
    match interp.run(&program) {
        Err(e) => e.to_string(),
        Ok(_) => panic!("expected error but got success"),
    }
}

/// Create a temporary directory for test module files.
fn tempdir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("silt_test_{}", std::process::id()));
    // Use a sub-directory with a random-ish name to avoid collisions
    let sub = dir.join(format!("{}", rand_u64()));
    fs::create_dir_all(&sub).expect("failed to create temp dir");
    sub
}

fn rand_u64() -> u64 {
    use std::time::SystemTime;
    let d = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap();
    d.as_nanos() as u64
}

// ── Basic module import ─────────────────────────────────────────────

#[test]
fn test_import_module_qualified() {
    let result = run_module_test(
        &[("math.silt", r#"
pub fn add(a, b) = a + b
pub fn square(x) = x * x
fn internal_helper(x) = x * 2
        "#)],
        r#"
import math

fn main() {
  math.add(3, 4)
}
        "#,
    );
    assert_eq!(result, Value::Int(7));
}

#[test]
fn test_import_module_multiple_functions() {
    let result = run_module_test(
        &[("math.silt", r#"
pub fn add(a, b) = a + b
pub fn square(x) = x * x
        "#)],
        r#"
import math

fn main() {
  math.add(math.square(3), math.square(4))
}
        "#,
    );
    assert_eq!(result, Value::Int(25));
}

// ── Selective import ────────────────────────────────────────────────

#[test]
fn test_import_specific_items() {
    let result = run_module_test(
        &[("math.silt", r#"
pub fn add(a, b) = a + b
pub fn square(x) = x * x
        "#)],
        r#"
import math.{ add, square }

fn main() {
  add(square(3), square(4))
}
        "#,
    );
    assert_eq!(result, Value::Int(25));
}

#[test]
fn test_import_single_item() {
    let result = run_module_test(
        &[("math.silt", r#"
pub fn add(a, b) = a + b
pub fn square(x) = x * x
        "#)],
        r#"
import math.{ add }

fn main() {
  add(10, 20)
}
        "#,
    );
    assert_eq!(result, Value::Int(30));
}

// ── Alias import ────────────────────────────────────────────────────

#[test]
fn test_import_module_with_alias() {
    let result = run_module_test(
        &[("math.silt", r#"
pub fn add(a, b) = a + b
pub fn square(x) = x * x
        "#)],
        r#"
import math as m

fn main() {
  m.add(3, 4)
}
        "#,
    );
    assert_eq!(result, Value::Int(7));
}

#[test]
fn test_import_alias_multiple_calls() {
    let result = run_module_test(
        &[("math.silt", r#"
pub fn add(a, b) = a + b
pub fn mul(a, b) = a * b
        "#)],
        r#"
import math as m

fn main() {
  m.add(m.mul(2, 3), m.mul(4, 5))
}
        "#,
    );
    assert_eq!(result, Value::Int(26));
}

// ── Pub visibility enforcement ──────────────────────────────────────

#[test]
fn test_private_function_not_importable_qualified() {
    let err = run_module_test_err(
        &[("math.silt", r#"
pub fn add(a, b) = a + b
fn secret(x) = x * 2
        "#)],
        r#"
import math

fn main() {
  math.secret(5)
}
        "#,
    );
    assert!(
        err.contains("undefined"),
        "expected error about undefined name, got: {err}"
    );
}

#[test]
fn test_private_function_not_selectively_importable() {
    let err = run_module_test_err(
        &[("math.silt", r#"
pub fn add(a, b) = a + b
fn secret(x) = x * 2
        "#)],
        r#"
import math.{ secret }

fn main() {
  secret(5)
}
        "#,
    );
    assert!(
        err.contains("no public item"),
        "expected error about no public item, got: {err}"
    );
}

// ── Module caching ──────────────────────────────────────────────────

#[test]
fn test_module_loaded_only_once() {
    // Importing the same module twice should work (cached)
    let result = run_module_test(
        &[("math.silt", r#"
pub fn add(a, b) = a + b
        "#)],
        r#"
import math
import math.{ add }

fn main() {
  add(math.add(1, 2), 3)
}
        "#,
    );
    assert_eq!(result, Value::Int(6));
}

// ── Module not found ────────────────────────────────────────────────

#[test]
fn test_module_not_found() {
    let err = run_module_test_err(
        &[],
        r#"
import nonexistent

fn main() {
  nonexistent.foo()
}
        "#,
    );
    assert!(
        err.contains("cannot load module"),
        "expected file-not-found error, got: {err}"
    );
}

// ── Builtin module imports ──────────────────────────────────────────

#[test]
fn test_import_builtin_string_module() {
    // `import string` should be a no-op (builtins already registered)
    // and string.split should still work
    let tokens = Lexer::new(r#"
import string

fn main() {
  let parts = "a,b,c" |> string.split(",")
  parts
}
    "#).tokenize().expect("lexer error");
    let program = Parser::new(tokens).parse_program().expect("parse error");
    let mut interp = Interpreter::new();
    let result = interp.run(&program).expect("runtime error");
    assert_eq!(
        result,
        Value::List(std::rc::Rc::new(vec![
            Value::String("a".into()),
            Value::String("b".into()),
            Value::String("c".into()),
        ]))
    );
}

#[test]
fn test_import_builtin_items() {
    // `import string.{ split }` should bring split into scope directly
    let tokens = Lexer::new(r#"
import string.{ split }

fn main() {
  "a,b,c" |> split(",")
}
    "#).tokenize().expect("lexer error");
    let program = Parser::new(tokens).parse_program().expect("parse error");
    let mut interp = Interpreter::new();
    let result = interp.run(&program).expect("runtime error");
    assert_eq!(
        result,
        Value::List(std::rc::Rc::new(vec![
            Value::String("a".into()),
            Value::String("b".into()),
            Value::String("c".into()),
        ]))
    );
}

#[test]
fn test_import_builtin_with_alias() {
    // `import string as s` should make s.split available
    let tokens = Lexer::new(r#"
import string as s

fn main() {
  "hello world" |> s.split(" ")
}
    "#).tokenize().expect("lexer error");
    let program = Parser::new(tokens).parse_program().expect("parse error");
    let mut interp = Interpreter::new();
    let result = interp.run(&program).expect("runtime error");
    assert_eq!(
        result,
        Value::List(std::rc::Rc::new(vec![
            Value::String("hello".into()),
            Value::String("world".into()),
        ]))
    );
}

#[test]
fn test_import_builtin_io_module() {
    let tokens = Lexer::new(r#"
import io

fn main() {
  let args = io.args()
  -- just verify it returns a list
  len(args)
}
    "#).tokenize().expect("lexer error");
    let program = Parser::new(tokens).parse_program().expect("parse error");
    let mut interp = Interpreter::new();
    let result = interp.run(&program).expect("runtime error");
    // Should return some Int (the number of args)
    match result {
        Value::Int(_) => {} // ok
        other => panic!("expected Int, got {other}"),
    }
}

// ── Module with types ───────────────────────────────────────────────

#[test]
fn test_module_with_pub_type() {
    let result = run_module_test(
        &[("shapes.silt", r#"
pub type Shape {
  Circle(Float)
  Rect(Float, Float)
}

pub fn area(shape) {
  match shape {
    Circle(r) -> 3.14 * r * r
    Rect(w, h) -> w * h
  }
}
        "#)],
        r#"
import shapes.{ area, Shape }

fn main() {
  area(Rect(3.0, 4.0))
}
        "#,
    );
    assert_eq!(result, Value::Float(12.0));
}

// ── Circular import detection ───────────────────────────────────────

#[test]
fn test_circular_import_detected() {
    let err = run_module_test_err(
        &[
            ("a.silt", r#"
import b
pub fn fa() = 1
            "#),
            ("b.silt", r#"
import a
pub fn fb() = 2
            "#),
        ],
        r#"
import a

fn main() {
  a.fa()
}
        "#,
    );
    assert!(
        err.contains("circular import"),
        "expected circular import error, got: {err}"
    );
}

// ── Multi-module example ────────────────────────────────────────────

#[test]
fn test_multi_module_example() {
    let result = run_module_test(
        &[
            ("math.silt", r#"
pub fn add(a, b) = a + b
pub fn square(x) = x * x
fn internal_helper(x) = x * 2
            "#),
            ("utils.silt", r#"
pub fn double(x) = x * 2
pub fn triple(x) = x * 3
            "#),
        ],
        r#"
import math
import utils.{ double }

fn main() {
  let x = math.add(3, 4)
  let y = math.square(x)
  double(y)
}
        "#,
    );
    // x = 7, y = 49, double(49) = 98
    assert_eq!(result, Value::Int(98));
}
