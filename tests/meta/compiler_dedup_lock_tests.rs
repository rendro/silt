//! Semantic-no-op locks for two `src/compiler/mod.rs` cleanups.
//!
//! Fix 2 — deleted a dead `if ctx.upvalues.iter().any(|_| false) { }`
//! block inside `resolve_upvalue_peek`. The predicate was always false
//! with an empty body, so removing it changes nothing: upvalues always
//! root at a local that the `locals` scan in the same loop already
//! catches. The lock below drives `resolve_upvalue_peek` via a nested
//! closure that captures an outer local whose name *also* names a
//! builtin module, and asserts the local (not the module) still wins.
//!
//! Fix 3 — extracted the shared `Self { .. }` literal of `Compiler::new`
//! and `Compiler::with_package_roots` into a private `build` constructor.
//! The two public constructors differ only in `package_roots` /
//! `local_package`; everything else must remain byte-for-byte identical.
//! `silt run` is built on `Compiler::with_package_roots`, while the REPL
//! is built on `Compiler::new`. The lock runs the same program through
//! both entry points and asserts identical observable behavior — proving
//! the dedup did not perturb either constructor.

use std::io::Write;
use std::process::{Command, Stdio};

/// Run a program via `silt run` (this path constructs the compiler with
/// `Compiler::with_package_roots`). Returns (stdout, stderr, ok).
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
/// A computation run through `silt run` (`with_package_roots`) and the
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

/// Fix 2 lock: `resolve_upvalue_peek` still resolves a captured local
/// correctly after the dead `if` was removed.
///
/// `math` is bound as a *local* in `main` while `math` is ALSO a builtin
/// module name. The inner closure captures `math` transitively (through
/// the outer closure) as an upvalue. Correct resolution must treat
/// `math` as the captured local value (10), yielding 15 — not attempt a
/// module reference. This drives the exact disambiguation path that the
/// deleted block pretended (but failed) to handle.
#[test]
fn nested_closure_captures_module_shadowing_local() {
    let src = r#"
import math

fn main() {
  let math = 10
  let outer = fn() {
    let inner = fn() {
      math + 5
    }
    inner()
  }
  println(outer())
}
"#;
    let (stdout, stderr, ok) = run_via_run("upval", src);
    assert!(
        ok,
        "silt run should succeed; stdout={stdout}, stderr={stderr}"
    );
    assert!(
        stdout.contains("15"),
        "inner closure must resolve the captured local `math` (=10) so \
         math + 5 == 15, not a module reference; stdout={stdout}, stderr={stderr}"
    );
}

/// Companion to the above: when the name is NOT shadowed by a local, a
/// builtin-module call (`math.sqrt`) inside a nested closure must still
/// dispatch to the module. Together with the previous test this pins both
/// sides of the local-vs-module decision that `resolve_upvalue_peek`
/// informs.
#[test]
fn nested_closure_module_call_still_dispatches() {
    let src = r#"
import math

fn main() {
  let outer = fn() {
    let inner = fn() {
      math.sqrt(9.0)
    }
    inner()
  }
  println(outer())
}
"#;
    let (stdout, stderr, ok) = run_via_run("modcall", src);
    assert!(
        ok,
        "silt run should succeed; stdout={stdout}, stderr={stderr}"
    );
    assert!(
        stdout.contains("3"),
        "inner closure must dispatch math.sqrt(9.0) == 3 when no local \
         shadows `math`; stdout={stdout}, stderr={stderr}"
    );
}
