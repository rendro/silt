//! Regression lock for audit GAP #7 — `is_user_import_resolvable_error`
//! in `src/cli/pipeline.rs` used to include a `starts_with("type ")`
//! arm that was far too broad. Every real
//! `"type mismatch: expected X, got Y"` produced by the type checker
//! also starts with `"type "`, so any file that imported a user module
//! had its genuine type errors silently demoted to suppressed
//! warnings. The fix: drop the `"type "` prefix and rely on the
//! narrower `contains("does not implement")` substring, which still
//! catches the three legitimate `"type '<X>' ... does not implement
//! ..."` messages the typechecker emits.
//!
//! These tests exercise the real `silt check` binary end-to-end so
//! the regression would be caught at the same layer where it
//! actually manifested (the CLI pipeline's post-typecheck filter).
//! The helper functions mirror the pattern in `tests/cli.rs` and
//! `tests/cli_test_rendering_tests.rs`, duplicated locally so the
//! suites stay decoupled.
//!
//! Mutation reasoning: restoring the `starts_with("type ")` arm in
//! `is_user_import_resolvable_error` makes
//! `test_type_mismatch_surfaces_even_with_user_import` fail (the
//! mismatch error disappears from stderr and the process exits 0).
//! Removing the `starts_with("undefined variable")` arm (or the
//! whole filter) makes
//! `test_undefined_from_user_import_still_suppressed` fail (the
//! typechecker's "undefined variable" leaks through when the real
//! user module is resolvable by the compiler).

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

/// Create a unique temp directory per call; return its path. Tests can
/// drop multiple `.silt` files in the same directory so user-module
/// imports resolve against a sibling file on disk.
fn temp_dir(prefix: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("silt_pipeline_filter_{prefix}_{n}"));
    // Start clean so a rerun doesn't inherit stale files.
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_file(dir: &PathBuf, name: &str, content: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, content).unwrap();
    path
}

// ── Test 1: type mismatch must surface when a user module is imported ──

/// GAP #7 core repro. A program that imports a user module AND has a
/// genuine `let x: Int = true` must report the type mismatch, not
/// silently succeed. Pre-fix this test failed because the filter's
/// `starts_with("type ")` arm swallowed the mismatch.
#[test]
fn test_type_mismatch_surfaces_even_with_user_import() {
    let dir = temp_dir("type_mismatch_surfaces");
    // A real user module so the compiler resolves the import cleanly —
    // this isolates the test to the typechecker's post-filter behavior
    // rather than a module-resolution error.
    write_file(
        &dir,
        "helper.silt",
        r#"pub fn add_one(x: Int) -> Int {
  x + 1
}
"#,
    );
    let main = write_file(
        &dir,
        "main.silt",
        r#"import helper.{add_one}

fn main() {
  let x: Int = true
  let y = add_one(x)
  println(y)
}
"#,
    );

    let output = silt_cmd()
        .arg("check")
        .arg(&main)
        .output()
        .expect("failed to run silt");

    assert!(
        !output.status.success(),
        "expected non-zero exit when type mismatch is present, got success. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("type mismatch: expected Int, got Bool"),
        "expected the real type-mismatch diagnostic to surface, got stderr: {stderr}"
    );
}

// ── Test 2: intended filter target is still suppressed ──────────────

/// The flip side of GAP #7: the filter must still suppress the
/// typechecker's noise for names that come from a user module the
/// typechecker can't see into. When `import shapes.{Circle, area}`
/// references a real on-disk user module, the typechecker emits
/// `"unknown module 'shapes'"` + `"undefined variable 'Circle'"` /
/// `"undefined variable 'area'"` — all of which the compiler resolves
/// later. `silt check` must therefore exit cleanly with empty JSON
/// output. If the filter regressed to let `undefined variable`
/// through, we'd see stderr noise and a non-zero exit here.
#[test]
fn test_undefined_from_user_import_still_suppressed() {
    let dir = temp_dir("undefined_suppressed");
    write_file(
        &dir,
        "shapes.silt",
        r#"pub type Shape {
  Circle(Int),
  Square(Int),
}

pub fn area(s: Shape) -> Int {
  match s {
    Circle(r) -> r * r * 3
    Square(l) -> l * l
  }
}
"#,
    );
    let main = write_file(
        &dir,
        "main.silt",
        r#"import shapes.{Circle, area}

fn main() {
  let s = Circle(5)
  let a = area(s)
  println(a)
}
"#,
    );

    let output = silt_cmd()
        .arg("check")
        .arg("--format")
        .arg("json")
        .arg(&main)
        .output()
        .expect("failed to run silt");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "expected clean exit (filter should suppress typechecker 'undefined variable' for \
         names imported from a resolvable user module). stdout: {stdout}, stderr: {stderr}"
    );
    // JSON output: an empty array means zero reportable diagnostics.
    assert_eq!(
        stdout.trim(),
        "[]",
        "expected empty diagnostic array when user-import names resolve via the compiler, got: {stdout}"
    );
}
