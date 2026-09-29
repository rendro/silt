//! Round 71 LATENT lock tests.
//!
//! ERR-2: `test.assert*` failure messages must render values via the
//!   silt-canonical `Value::format_silt` (no `ExtFloat(...)` Rust
//!   variant leak). Driven through a real `silt test` subprocess so the
//!   runtime path actually fires (a typecheck-only / format-only lock
//!   would not touch `call_test`).
//!
//! (The companion `test.assert(false)` rendering lock is the golden case
//! tests/golden/meta/testing/round71_misc_latent_lock_tests__assert_single_arg_false_renders_canonically.silt.)

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

// ── Test-file helpers (subprocess pattern) ──────────────────────────

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

fn temp_dir(prefix: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("silt_round71_{prefix}_{n}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

// ── ERR-2: assert formatting via real `silt test` subprocess ────────

/// Repro from the audit: dividing two `Float` produces an `ExtFloat`,
/// so `test.assert_eq(1.0/3.0, 0.3333)` previously rendered as
/// `assertion failed: ExtFloat(0.333…) != 0.3333`. The fix routes the
/// values through `Value::format_silt`, which writes ExtFloat as a
/// bare numeric literal.
#[test]
fn err2_silt_test_assert_eq_failure_does_not_leak_extfloat_variant_name() {
    let dir = temp_dir("err2");
    let test_file = dir.join("extfloat_test.silt");
    fs::write(
        &test_file,
        // `Float / Float` widens to `ExtFloat` (per
        // `src/vm/arithmetic.rs:53-55`), so both operands here are
        // ExtFloat at runtime — and the typechecker accepts the call
        // because both sides have the same static type. The values
        // are obviously unequal (1/3 vs 1/4), so the assertion fails
        // and the formatter is exercised on the runtime path.
        "import test\n\nfn test_extfloat_render() {\n  test.assert_eq(1.0/3.0, 1.0/4.0)\n}\n",
    )
    .unwrap();

    let output = silt_cmd()
        .arg("test")
        .arg(&test_file)
        .output()
        .expect("failed to spawn `silt test`");

    // The test must FAIL (by design — the values aren't equal).
    // Combine stdout+stderr because the test runner writes summary
    // lines to one stream and assertion bodies to another depending
    // on the path.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");

    // Sanity: this should be the assertion failure path.
    assert!(
        combined.contains("assertion failed"),
        "expected an assertion-failed message in `silt test` output. \
         stdout={stdout:?} stderr={stderr:?}"
    );

    // The lock: no Rust variant name leak. If this assertion ever
    // re-fails, the assert builtins have regressed to using `{:?}`
    // (Debug) on `Value`.
    assert!(
        !combined.contains("ExtFloat("),
        "`silt test` failure rendered the Rust variant name `ExtFloat(...)` — \
         the assert builtins must format values with `Value::format_silt` so \
         the user-visible failure shows a bare numeric literal. \
         stdout={stdout:?} stderr={stderr:?}"
    );
}
