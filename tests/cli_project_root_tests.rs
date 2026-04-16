//! End-to-end tests for v0.7 project-root unification.
//!
//! Verifies that `silt run` / `silt check` work without an explicit file
//! when invoked inside a package; that explicit-file invocations still
//! work (backwards compat); that `silt fmt` only recurses inside a
//! package; and that the `silt update` → `silt self-update` rename and
//! its back-compat shim behave as documented.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

fn fresh_dir(prefix: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("silt_project_root_tests_{prefix}_{n}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Write a Cargo-style silt package: silt.toml + src/main.silt.
fn write_package(dir: &Path, pkg_name: &str, main_body: &str) {
    fs::write(
        dir.join("silt.toml"),
        format!("[package]\nname = \"{pkg_name}\"\nversion = \"0.1.0\"\n"),
    )
    .unwrap();
    let src = dir.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("main.silt"), main_body).unwrap();
}

#[test]
fn test_run_inside_package_with_no_args() {
    let dir = fresh_dir("run_inside");
    write_package(
        &dir,
        "run_inside_pkg",
        "fn main() {\n  println(\"from package\")\n}\n",
    );

    let out = silt_cmd()
        .arg("run")
        .current_dir(&dir)
        .output()
        .expect("failed to invoke silt run");
    assert!(
        out.status.success(),
        "silt run failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("from package"),
        "expected greeting; got stdout: {stdout}"
    );
}

#[test]
fn test_run_with_explicit_path_still_works() {
    // Mimics the legacy non-package workflow: a single .silt file with
    // an explicit path argument, no enclosing manifest. Must still run.
    let dir = fresh_dir("run_explicit");
    let file = dir.join("hello.silt");
    fs::write(&file, "fn main() { println(\"explicit\") }\n").unwrap();

    let out = silt_cmd()
        .arg("run")
        .arg(&file)
        .output()
        .expect("failed to invoke silt run");
    assert!(
        out.status.success(),
        "silt run failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("explicit"), "got stdout: {stdout}");
}

#[test]
fn test_check_inside_package_no_args() {
    let dir = fresh_dir("check_inside");
    write_package(
        &dir,
        "check_inside_pkg",
        "fn main() {\n  println(\"check me\")\n}\n",
    );

    let out = silt_cmd()
        .arg("check")
        .current_dir(&dir)
        .output()
        .expect("failed to invoke silt check");
    assert!(
        out.status.success(),
        "silt check failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn test_fmt_recursive_only_inside_package() {
    // Outside a package: implicit recursion must be refused with a
    // pointer at the new project-boundary marker (silt.toml).
    let outside = fresh_dir("fmt_outside");
    fs::write(outside.join("a.silt"), "fn main() {}\n").unwrap();
    let out_outside = silt_cmd()
        .args(["fmt", "--check"])
        .current_dir(&outside)
        .output()
        .expect("failed to invoke silt fmt");
    assert!(
        !out_outside.status.success(),
        "fmt should refuse recursion outside a package; got success"
    );
    let stderr = String::from_utf8_lossy(&out_outside.stderr);
    assert!(
        stderr.contains("refusing to recursively format") && stderr.contains("silt.toml"),
        "expected silt.toml-aware refusal; got: {stderr}"
    );

    // Inside a package: recursion should proceed (no error from the
    // project-anchor check; per-file format pass is what the rest does).
    let inside = fresh_dir("fmt_inside");
    write_package(
        &inside,
        "fmt_inside_pkg",
        "fn main() {\n  println(\"hi\")\n}\n",
    );
    let out_inside = silt_cmd()
        .args(["fmt", "--check"])
        .current_dir(&inside)
        .output()
        .expect("failed to invoke silt fmt");
    let stderr_inside = String::from_utf8_lossy(&out_inside.stderr);
    assert!(
        !stderr_inside.contains("refusing to recursively format"),
        "fmt should not refuse inside a package; got stderr: {stderr_inside}"
    );
}

#[test]
fn test_self_update_rename_works() {
    // Don't actually hit the network — just verify the new command name
    // exists and renders its help banner. Old code under "update --help"
    // would have produced the same banner; we now require the renamed
    // command to be the canonical entry point.
    let out = silt_cmd()
        .args(["self-update", "--help"])
        .output()
        .expect("failed to invoke silt self-update --help");
    assert!(
        out.status.success(),
        "silt self-update --help failed: {:?}",
        out
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("silt self-update"),
        "help banner missing self-update name: {stdout}"
    );
    assert!(
        stdout.contains("--dry-run") && stdout.contains("--force"),
        "help banner missing legacy flags: {stdout}"
    );
}

#[test]
fn test_old_silt_update_redirects() {
    // From outside any package, with no flags, the redirect kicks in
    // because there's no silt.toml above us.
    let dir = fresh_dir("old_update_redirect");
    let out = silt_cmd()
        .arg("update")
        .current_dir(&dir)
        .output()
        .expect("failed to invoke silt update");
    assert_eq!(
        out.status.code(),
        Some(2),
        "expected exit 2; got {:?}",
        out.status.code()
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("silt update has been renamed to silt self-update"),
        "stderr lacked redirect message: {stderr}"
    );

    // From INSIDE a package, --dry-run still triggers the legacy-flag
    // redirect (otherwise scripts would silently change behavior across
    // versions).
    let pkg = fresh_dir("old_update_dry_run");
    write_package(&pkg, "dryrun_pkg", "fn main() {}\n");
    let out2 = silt_cmd()
        .args(["update", "--dry-run"])
        .current_dir(&pkg)
        .output()
        .expect("failed to invoke silt update --dry-run");
    assert_eq!(out2.status.code(), Some(2));
    let stderr2 = String::from_utf8_lossy(&out2.stderr);
    assert!(
        stderr2.contains("silt update has been renamed to silt self-update"),
        "stderr lacked redirect message for --dry-run: {stderr2}"
    );
}

#[test]
fn test_silt_update_in_package_says_coming_soon() {
    let dir = fresh_dir("update_coming_soon");
    write_package(&dir, "coming_soon_pkg", "fn main() {}\n");

    let out = silt_cmd()
        .arg("update")
        .current_dir(&dir)
        .output()
        .expect("failed to invoke silt update");
    assert_eq!(
        out.status.code(),
        Some(2),
        "expected exit 2 from in-package silt update; got {:?}",
        out.status.code()
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("coming in v0.7 (PR 4)"),
        "stderr lacked coming-soon message: {stderr}"
    );
    assert!(
        stderr.contains("silt self-update"),
        "stderr lacked self-update pointer: {stderr}"
    );
}
