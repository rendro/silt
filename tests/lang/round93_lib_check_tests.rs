//! Round 93 (fix G2): lib-only packages — the REQUIRED shape for
//! dependencies (`src/lib.silt`, no `src/main.silt`) — must be
//! checkable with a bare `silt check`.
//!
//! PRE-fix, `resolve_package_entry_point` (src/cli/package.rs)
//! hard-required `src/main.silt`, so `silt check` inside a lib-only
//! package died with "package has no entry point — expected
//! `src/main.silt`" even though `silt check src/lib.silt` worked.
//!
//! POST-fix:
//!   - `silt check` (no file arg) falls back to `src/lib.silt` when
//!     `src/main.silt` is absent (`EntryPointKind::AllowLib`).
//!   - `silt run` still requires `src/main.silt`, but its diagnostic
//!     now names both ways out: the package is library-only, `silt
//!     check` works on it, and adding `src/main.silt` makes it runnable.
//!   - Packages WITH `src/main.silt` behave exactly as before for both
//!     subcommands.
//!
//! Each test runs the compiled `silt` binary in a fresh temp package.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Unique temp dir per call so parallel tests never share state.
fn temp_dir(prefix: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("silt_round93_lib_check_{prefix}_{pid}_{n}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Write a minimal package manifest + the given `src/` files.
fn write_package(dir: &Path, name: &str, files: &[(&str, &str)]) {
    fs::write(
        dir.join("silt.toml"),
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n"),
    )
    .unwrap();
    let src = dir.join("src");
    fs::create_dir_all(&src).unwrap();
    for (file, content) in files {
        fs::write(src.join(file), content).unwrap();
    }
}

/// Run a bare `silt <subcommand>` (no file argument) with `cwd` as the
/// working directory — the package-entry-point resolution path.
fn silt_bare(cwd: &Path, subcommand: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg(subcommand)
        .current_dir(cwd)
        .output()
        .expect("failed to spawn silt binary")
}

const LIB_SRC: &str = "pub fn greet() = \"hello from lib\"\n";
const MAIN_SRC: &str = "fn main() {\n  println(\"hello from main\")\n}\n";

// ── Lib-only package ────────────────────────────────────────────────

#[test]
fn lib_only_package_bare_check_succeeds() {
    let dir = temp_dir("libonly_check");
    write_package(&dir, "mylib", &[("lib.silt", LIB_SRC)]);

    let out = silt_bare(&dir, "check");
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "`silt check` must accept a lib-only package; stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("package has no entry point"),
        "no entry-point error expected for `silt check`, got:\n{stderr}"
    );
}

#[test]
fn lib_only_package_bare_run_fails_naming_both_options() {
    let dir = temp_dir("libonly_run");
    write_package(&dir, "mylib", &[("lib.silt", LIB_SRC)]);

    let out = silt_bare(&dir, "run");
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        !out.status.success(),
        "`silt run` must still require src/main.silt; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("package has no entry point") && stderr.contains("src/main.silt"),
        "expected the entry-point error naming src/main.silt, got:\n{stderr}"
    );
    // The improved message names BOTH options: the package is
    // library-only (checkable), and a main.silt makes it runnable.
    assert!(
        stderr.contains("library-only package"),
        "expected the lib-only note, got:\n{stderr}"
    );
    assert!(
        stderr.contains("silt check"),
        "expected the `silt check` pointer, got:\n{stderr}"
    );
}

// ── Package with a main entry point: unchanged ──────────────────────

#[test]
fn package_with_main_bare_check_unchanged() {
    let dir = temp_dir("main_check");
    write_package(&dir, "myapp", &[("main.silt", MAIN_SRC)]);

    let out = silt_bare(&dir, "check");
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "`silt check` on a package with main must keep passing; stderr:\n{stderr}"
    );
}

#[test]
fn package_with_main_bare_run_unchanged() {
    let dir = temp_dir("main_run");
    write_package(&dir, "myapp", &[("main.silt", MAIN_SRC)]);

    let out = silt_bare(&dir, "run");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        out.status.success(),
        "`silt run` on a package with main must keep running; stderr:\n{stderr}"
    );
    assert!(
        stdout.contains("hello from main"),
        "expected main's output, got:\n{stdout}"
    );
}

// ── Package with BOTH main and lib: main wins for check too ─────────

#[test]
fn package_with_both_prefers_main_for_check() {
    let dir = temp_dir("both_check");
    write_package(
        &dir,
        "myboth",
        &[("main.silt", MAIN_SRC), ("lib.silt", LIB_SRC)],
    );

    let out = silt_bare(&dir, "check");
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "`silt check` with both entry files must pass (main preferred); stderr:\n{stderr}"
    );
}

// ── Package with NEITHER: historical error retained ─────────────────

#[test]
fn package_with_neither_keeps_historical_error() {
    let dir = temp_dir("neither");
    write_package(&dir, "myempty", &[]);

    for sub in ["check", "run"] {
        let out = silt_bare(&dir, sub);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success(),
            "`silt {sub}` must fail with no entry point at all; stderr:\n{stderr}"
        );
        assert!(
            stderr.contains("package has no entry point") && stderr.contains("src/main.silt"),
            "`silt {sub}`: expected the historical no-entry-point error, got:\n{stderr}"
        );
        assert!(
            !stderr.contains("library-only"),
            "`silt {sub}`: no lib-only note without a lib.silt, got:\n{stderr}"
        );
    }
}
