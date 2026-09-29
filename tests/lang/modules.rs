//! Module-system runtime defences. `silt check` / `silt run` reject both
//! programs below in the typechecker (golden cases
//! `tests/golden/lang/modules/modules__*_static*`); these tests skip the
//! typechecker's verdict and pin the compiler/VM's own "undefined global"
//! error for a private or missing module member. Every other module test
//! that used to live here is a golden case `modules__*` beside them.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use silt::compiler::Compiler;
use silt::intern;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::vm::Vm;

/// Build a Compiler whose only package is the synthetic `__local__`,
/// rooted at `root`. Equivalent to the pre-PR-4 `with_project_root`
/// shim (which the compiler module rightly dropped) — preserved here
/// so the legacy module tests don't have to be rewritten to stage a
/// real `silt.toml`/`src/` layout.
fn compiler_for_root(root: PathBuf) -> Compiler {
    let local = intern::intern("__local__");
    let mut roots = HashMap::new();
    roots.insert(local, root);
    Compiler::with_package_roots(local, roots)
}

fn run_module_test_err(files: &[(&str, &str)], main_source: &str) -> String {
    let dir = tempdir();

    for (name, content) in files {
        let path = dir.join(name);
        fs::write(&path, content).expect("failed to write module file");
    }

    let tokens = Lexer::new(main_source).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = compiler_for_root(dir.clone());
    match compiler.compile_program(&program) {
        Ok(functions) => {
            let script = Arc::new(functions.into_iter().next().unwrap());
            let mut vm = Vm::new();
            match vm.run(script) {
                Err(e) => e.to_string(),
                Ok(_) => panic!("expected error but got success"),
            }
        }
        Err(e) => e.message,
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

// ── Pub visibility enforcement ──────────────────────────────────────

#[test]
fn test_private_function_not_selectively_importable() {
    let err = run_module_test_err(
        &[(
            "calc.silt",
            r#"
pub fn add(a, b) = a + b
fn secret(x) = x * 2
        "#,
        )],
        r#"
import calc.{ secret }

fn main() {
  secret(5)
}
        "#,
    );
    assert!(
        err.contains("undefined global: calc.secret"),
        "expected 'undefined global: calc.secret', got: {err}"
    );
}

// ── Private module function visibility error ───────────────────────

/// Control: calling a name that is NOT in the imported module at all
/// (neither public nor private) should still fall through to the
/// existing "undefined" error path, not the new visibility-specific
/// message.
#[test]
fn test_truly_unknown_module_function_still_emits_undefined_error() {
    let err = run_module_test_err(
        &[(
            "mymod.silt",
            r#"
pub fn x() = 1
            "#,
        )],
        r#"
import mymod

fn main() {
  mymod.genuinely_missing()
}
        "#,
    );
    // `mymod.genuinely_missing` is not a known private fn, so the visibility
    // check in src/compiler/mod.rs passes through; the emitted GetGlobal for
    // `mymod.genuinely_missing` fails at runtime with the exact phrase from
    // src/vm/execute.rs:1126.
    assert!(
        err.contains("undefined global: mymod.genuinely_missing"),
        "unknown module-qualified names must surface the VM's undefined-global error, got: {err}"
    );
    assert!(
        !err.contains("but is not `pub`"),
        "visibility error must not fire for a name that doesn't exist at all, got: {err}"
    );
}

// ── G3 (round 15): module parse errors must include a source snippet ──
//
// Before the fix, `format_module_source_error` in src/compiler/mod.rs
// flattened the inner (module-file) parse error into a single-line
// message "module 'bad': parse error at bad.silt:3:1 — ..." and the
// outer renderer's caret landed at the `import bad` line in main.silt.
// Users had no way to see where the actual parse error was inside the
// imported module. The fix reproduces the offending source line from
// the module file plus a caret marker inline in the error message.
//
// Mutation reasoning: reverting the `format_module_source_error` body
// back to the flat one-line format would make this test fail because
// (a) the `-->` marker pointing at the module file wouldn't appear in
// the message, and (b) the actual line of module source would not be
// rendered.

// ── B6: module parse-error snippet must not render twice ───────────
//
// When a parse error inside an imported module is rendered via the
// full `SourceError::Display` path (as main.rs/repl.rs do), the
// inner module snippet must appear EXACTLY ONCE, not twice. The
// audit finding (B6) was that round 15 embedded a multi-line
// `--> ... | ^` snippet into `CompileError.message`; `SourceError::
// Display` then echoed that entire blob both in the header
// (`error[compile]: {msg}`) AND on the caret line (`^ {msg}`),
// duplicating the inner snippet. The fix (errors.rs) truncates
// `msg` at the first newline when rendering the caret line. This
// test counts occurrences of the unique inner source line and
// pins to exactly 1.

// ── G1: module parse error with EOF-past-end span must keep snippet ──
//
// A truncated inner module file (e.g. `pub fn broken(\n` with an
// unexpected EOF) parses with a span pointing at line 2, column 1 —
// one line past the end of the file. Before the fix,
// `format_module_source_error` silently dropped the snippet because
// `source.lines().nth(span.line - 1)` returned `None`. The fix
// clamps the span back onto the last real line. This test locks
// the rendered error message to include both the `pub fn broken(`
// line and a caret marker.

// ── G4: circular-import error must render the full chain ───────────
//
// A 3-cycle `c_a -> c_b -> c_c -> c_a` must produce an error message
// that includes the exact arrow chain as a substring, not just a
// bare "module 'c_a' imports itself" line. This lets the user see
// the path through which the cycle was reached.
