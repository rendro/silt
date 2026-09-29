//! Regression lock tests for round-19 BROKEN fix: `generalize()` must
//! preserve where-clause constraints when a constrained function is
//! aliased via `let f = constrained_fn` or wrapped in a lambda.
//!
//! These four tests were named in commit 520e38e but never actually
//! added to the tree. A silent revert of the fix (switching
//! `constraints,` back to `constraints: vec![],` in the `Scheme`
//! construction inside `generalize()`) would cause the negative tests
//! to pass without any type error, making the revert undetectable by
//! the existing suite.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

fn temp_silt_file(prefix: &str, content: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join("silt_where_constraint_alias_tests");
    fs::create_dir_all(&dir).unwrap();
    let name = format!("{prefix}_{n}.silt");
    let path = dir.join(name);
    fs::write(&path, content).unwrap();
    path
}

// ── Negative tests: Int does NOT implement Greetable ──────────────

#[test]
fn test_where_constraint_survives_let_alias() {
    let path = temp_silt_file(
        "constraint_let_alias",
        r#"trait Greetable { fn greet(self) -> String }
trait Greetable for String { fn greet(self) -> String { self } }
fn use_g(x: a) -> String where a: Greetable { x.greet() }
let f = use_g
fn main() { f(42) }
"#,
    );

    let output = silt_cmd()
        .arg("check")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    assert!(
        !output.status.success(),
        "expected non-zero exit: aliased constrained fn must reject Int"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("does not implement trait 'Greetable'"),
        "expected trait-constraint error in stderr, got: {stderr}"
    );
}

#[test]
fn test_where_constraint_survives_lambda_wrapper() {
    let path = temp_silt_file(
        "constraint_lambda_wrapper",
        r#"trait Greetable { fn greet(self) -> String }
trait Greetable for String { fn greet(self) -> String { self } }
fn use_g(x: a) -> String where a: Greetable { x.greet() }
let f = fn(x) { use_g(x) }
fn main() { f(42) }
"#,
    );

    let output = silt_cmd()
        .arg("check")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    assert!(
        !output.status.success(),
        "expected non-zero exit: lambda-wrapped constrained fn must reject Int"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("does not implement trait 'Greetable'"),
        "expected trait-constraint error in stderr, got: {stderr}"
    );
}

// ── Positive tests: String DOES implement Greetable ───────────────

#[test]
fn test_where_constraint_alias_valid_call_still_works() {
    let path = temp_silt_file(
        "constraint_alias_valid",
        r#"trait Greetable { fn greet(self) -> String }
trait Greetable for String { fn greet(self) -> String { self } }
fn use_g(x: a) -> String where a: Greetable { x.greet() }
let f = use_g
fn main() { println(f("hello")) }
"#,
    );

    let output = silt_cmd()
        .arg("run")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    assert!(
        output.status.success(),
        "expected exit 0 for valid call through alias, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("hello"),
        "expected 'hello' in stdout, got: {stdout}"
    );
}

#[test]
fn test_where_constraint_lambda_valid_call_still_works() {
    let path = temp_silt_file(
        "constraint_lambda_valid",
        r#"trait Greetable { fn greet(self) -> String }
trait Greetable for String { fn greet(self) -> String { self } }
fn use_g(x: a) -> String where a: Greetable { x.greet() }
let f = fn(x) { use_g(x) }
fn main() { println(f("hello")) }
"#,
    );

    let output = silt_cmd()
        .arg("run")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    assert!(
        output.status.success(),
        "expected exit 0 for valid call through lambda, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("hello"),
        "expected 'hello' in stdout, got: {stdout}"
    );
}
