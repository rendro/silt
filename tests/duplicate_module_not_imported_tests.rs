//! Round 60 B9 regression lock.
//!
//! The typechecker emits `"module 'X' is not imported; add \`import X\`..."`
//! at `src/typechecker/inference.rs:1897`. The compiler independently
//! re-emits the identical sentence at `src/compiler/mod.rs:1923,
//! :2029, :2782`. Before this fix, the CLI pipeline fed both phases'
//! errors into the combined diagnostic vec with no deduplication, so
//! `silt check main.silt` rendered the same message twice — once as
//! `error[type]`, once as `error[compile]`.
//!
//! The fix adds `is_module_not_imported_typecheck_error` to the
//! per-entry filter in `reportable_type_errors`, which drops the
//! typechecker's copy. The compiler's version is authoritative because
//! it's what actually blocks bytecode emission; the typechecker-only
//! consumers (LSP, `missing_import_recommends_tests`) continue to see
//! the diagnostic via `typechecker::check` directly.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

fn temp_dir(prefix: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("silt_b9_{prefix}_{n}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_file(dir: &PathBuf, name: &str, content: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, content).unwrap();
    path
}

/// Exact repro from the audit finding: `println(math.sqrt(2.0))`
/// without `import math` should fail, but the "is not imported"
/// sentence must appear exactly once.
#[test]
fn test_module_not_imported_message_printed_once() {
    let dir = temp_dir("math_sqrt");
    let main = write_file(
        &dir,
        "main.silt",
        r#"fn main() {
  println(math.sqrt(2.0))
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
        "expected non-zero exit for missing import, got success"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{stdout}\n{stderr}");
    // The diagnostic renderer prints the message once in the header
    // (`error[kind]: <msg>`) and once inside the caret annotation, so a
    // single diagnostic contributes two textual matches. The duplicate
    // pre-fix bug produced FOUR matches (two diagnostics × two prints
    // each). Assert the deduplicated count is exactly 2.
    let count = combined.matches("module 'math' is not imported").count();
    assert_eq!(
        count, 2,
        "expected 'module 'math' is not imported' to appear exactly twice (one diagnostic × header+caret); got {count}:\n{combined}"
    );
    // Additionally assert the `error[type]` / `error[compile]`
    // distinction: the typechecker's `[type]` version should have been
    // dropped, leaving only the compiler's `[compile]` version.
    let type_err_count = combined.matches("error[type]: module 'math' is not imported").count();
    let compile_err_count = combined.matches("error[compile]: module 'math' is not imported").count();
    assert_eq!(
        type_err_count, 0,
        "typechecker's [type] version should be filtered out, got:\n{combined}"
    );
    assert_eq!(
        compile_err_count, 1,
        "compiler's [compile] version should remain, got:\n{combined}"
    );
}

/// Counterpart: when the user DOES import the module, no such message
/// appears at all. Locks that the dedup filter doesn't accidentally
/// suppress legitimate output.
#[test]
fn test_module_not_imported_message_absent_when_imported() {
    let dir = temp_dir("math_imported");
    let main = write_file(
        &dir,
        "main.silt",
        r#"import math

fn main() {
  println(math.sqrt(2.0))
}
"#,
    );

    let output = silt_cmd()
        .arg("check")
        .arg(&main)
        .output()
        .expect("failed to run silt");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{stdout}\n{stderr}");
    assert!(
        !combined.contains("is not imported"),
        "imported-module case should not emit 'is not imported', got:\n{combined}"
    );
    assert!(
        output.status.success(),
        "program with correct import should check successfully; stderr:\n{stderr}"
    );
}
