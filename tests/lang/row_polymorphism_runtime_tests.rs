//! Runtime tests for row polymorphism: anon record literals, the
//! extend operator (`{...p, age: 30}`), pattern destructure with rest,
//! and nominal-to-anon widening through a polymorphic fn.
//!
//! Drives the `silt` CLI to validate the full pipeline (lexer →
//! parser → typechecker → compiler → VM).

use std::process::Command;

fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_row_poly_rt_{label}.silt"));
    std::fs::write(&tmp, src).expect("write temp file");
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .arg("run")
        .arg(&tmp)
        .output()
        .expect("spawn silt run");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (stdout, stderr, out.status.success())
}

fn run_silt_ok(label: &str, src: &str) -> String {
    let (stdout, stderr, ok) = run_silt_raw(label, src);
    assert!(
        ok,
        "silt run should succeed for {label}; stdout={stdout}, stderr={stderr}"
    );
    stdout
}

#[test]
fn anon_record_literal_field_access() {
    let out = run_silt_ok(
        "lit_access",
        r#"
fn main() {
  let p = {name: "Alice", age: 30}
  println(p.name)
  println(p.age)
}
"#,
    );
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines.len() >= 2, "expected 2+ lines; got {out:?}");
    assert!(lines[0].contains("Alice"), "got line[0]={:?}", lines[0]);
    assert!(lines[1].contains("30"), "got line[1]={:?}", lines[1]);
}

#[test]
fn anon_record_extend_op() {
    let out = run_silt_ok(
        "extend",
        r#"
fn main() {
  let p = {name: "A"}
  let q = {...p, age: 30}
  println(q.name)
  println(q.age)
}
"#,
    );
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines.len() >= 2, "expected 2+ lines; got {out:?}");
    assert!(lines[0].contains("A"), "got line[0]={:?}", lines[0]);
    assert!(lines[1].contains("30"), "got line[1]={:?}", lines[1]);
}

#[test]
fn nominal_widens_through_open_row_fn() {
    let out = run_silt_ok(
        "nominal_widen",
        r#"
type Person { name: String, age: Int }
fn name(p: {name: String, ...r}) -> String = p.name
fn main() {
  println(name(Person { name: "Bob", age: 42 }))
}
"#,
    );
    assert!(out.contains("Bob"), "expected 'Bob' in stdout; got {out:?}");
}
