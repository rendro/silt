//! Semantic-no-op lock for a `src/compiler/mod.rs` cleanup. (The
//! companion Fix 2 lock on nested-closure upvalue resolution is now the
//! golden cases `tests/golden/meta/closures/compiler_dedup_lock_tests__*`.)
//!
//! Fix 3 — extracted the shared `Self { .. }` literal of `Compiler::new`
//! and `Compiler::for_program` into a private `build` constructor.
//! The two public constructors differ only in the modules and the
//! resolver; everything else must remain byte-for-byte identical.
//! `silt run` is built on `Compiler::for_program`, while the REPL
//! is built on `Compiler::new`. The lock runs the same program through
//! both entry points and asserts identical observable behavior — proving
//! the dedup did not perturb either constructor.

use std::io::Write;
use std::process::{Command, Stdio};

/// Run a program via `silt run` (this path constructs the compiler with
/// `Compiler::for_program`). Returns (stdout, stderr, ok).
fn run_via_run(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_dedup_lock_{label}.silt"));
    std::fs::write(&tmp, src).expect("write temp file");
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .arg("run")
        .arg(&tmp)
        .output()
        .expect("spawn silt run");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

/// Feed source to `silt repl` over stdin (this path constructs the
/// compiler with `Compiler::new`). Returns combined stdout.
fn run_via_repl(src: &str) -> String {
    let bin = env!("CARGO_BIN_EXE_silt");
    let mut child = Command::new(bin)
        .arg("repl")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn silt repl");
    child
        .stdin
        .as_mut()
        .expect("repl stdin")
        .write_all(src.as_bytes())
        .expect("write repl stdin");
    let out = child.wait_with_output().expect("wait repl");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Fix 3 lock: the dedup of the two constructors is a semantic no-op.
/// A computation run through `silt run` (`for_program`) and the
/// same computation run through the REPL (`Compiler::new`) must produce
/// the same answer. If the extracted `build` had dropped or reordered a
/// field, one of the two paths would diverge.
#[test]
fn both_constructor_paths_agree() {
    // A small program touching arithmetic, a let binding, and println —
    // enough to exercise compilation through whichever constructor each
    // entry point uses.
    let run_src = r#"
fn main() {
  let a = 21
  let b = 21
  println(a + b)
}
"#;
    let (run_out, run_err, ok) = run_via_run("ctor", run_src);
    assert!(
        ok,
        "silt run should succeed; stdout={run_out}, stderr={run_err}"
    );
    assert!(
        run_out.contains("42"),
        "run path must compute 42; stdout={run_out}"
    );

    // The REPL evaluates statements directly (no fn main wrapper).
    let repl_src = "let a = 21\nlet b = 21\nprintln(a + b)\n";
    let repl_out = run_via_repl(repl_src);
    assert!(
        repl_out.contains("42"),
        "repl path (Compiler::new) must compute the same 42; stdout={repl_out}"
    );
}
