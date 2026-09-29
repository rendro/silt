//! End-to-end tests for `silt init`.
//!
//! These exercise the v0.7 package-style init path: `silt init` should
//! produce a Cargo-style layout (`silt.toml` + `src/main.silt`) named after
//! the current directory, refuse to clobber existing files, and leave the
//! package in a state where `silt run` (no args) immediately works.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

/// Create a fresh empty temp directory whose final segment is `name`.
/// Tests rely on the dirname for package-name derivation, so the segment
/// has to be exact (no random suffix appended). We use a per-test scratch
/// root to keep collisions impossible without polluting the dirname.
fn temp_dir_named(name: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!("silt_init_tests_{n}"));
    let dir = root.join(name);
    // Clean any leftover from previous runs of this slot.
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e))
}

#[test]
fn test_init_creates_manifest_and_main() {
    let dir = temp_dir_named("hello_pkg");
    let out = silt_cmd()
        .arg("init")
        .current_dir(&dir)
        .output()
        .expect("failed to invoke silt init");
    assert!(
        out.status.success(),
        "silt init failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let manifest_path = dir.join("silt.toml");
    let main_path = dir.join("src").join("main.silt");
    assert!(manifest_path.is_file(), "silt.toml was not created");
    assert!(main_path.is_file(), "src/main.silt was not created");

    let manifest = read(&manifest_path);
    assert!(
        manifest.contains("[package]"),
        "manifest missing [package]: {manifest}"
    );
    assert!(
        manifest.contains("name = \"hello_pkg\""),
        "manifest missing name: {manifest}"
    );
    assert!(
        manifest.contains("version = \"0.1.0\""),
        "manifest missing version: {manifest}"
    );

    // The manifest must round-trip through the production loader so we
    // catch any drift between init's output and the parser's schema.
    let loaded = silt::manifest::Manifest::load(&manifest_path)
        .unwrap_or_else(|e| panic!("Manifest::load failed: {e}"));
    assert_eq!(silt::intern::resolve(loaded.package.name), "hello_pkg");
    assert_eq!(loaded.package.version, "0.1.0");

    let main = read(&main_path);
    assert!(
        main.contains("fn main()"),
        "main.silt missing main fn: {main}"
    );
    assert!(
        main.contains("hello, silt!"),
        "main.silt missing greeting: {main}"
    );
}

#[test]
fn test_init_uses_dirname_as_package_name() {
    let dir = temp_dir_named("my_app");
    let out = silt_cmd()
        .arg("init")
        .current_dir(&dir)
        .output()
        .expect("failed to invoke silt init");
    assert!(out.status.success(), "silt init failed: {:?}", out);

    let manifest = read(&dir.join("silt.toml"));
    assert!(
        manifest.contains("name = \"my_app\""),
        "expected name `my_app`, got manifest:\n{manifest}"
    );
}

#[test]
fn test_init_sanitizes_dirname() {
    let dir = temp_dir_named("My-App");
    let out = silt_cmd()
        .arg("init")
        .current_dir(&dir)
        .output()
        .expect("failed to invoke silt init");
    assert!(
        out.status.success(),
        "silt init failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let manifest = read(&dir.join("silt.toml"));
    assert!(
        manifest.contains("name = \"my_app\""),
        "expected sanitized name `my_app` (lowercase + dash→underscore), got:\n{manifest}"
    );
}

#[test]
fn test_init_refuses_to_overwrite_manifest() {
    let dir = temp_dir_named("already_inited");
    fs::write(
        dir.join("silt.toml"),
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let out = silt_cmd()
        .arg("init")
        .current_dir(&dir)
        .output()
        .expect("failed to invoke silt init");
    assert!(!out.status.success(), "silt init should have refused");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("silt.toml already exists"),
        "stderr lacked overwrite refusal: {stderr}"
    );
    // Source tree should NOT have been created when the manifest blocked init.
    assert!(
        !dir.join("src").exists(),
        "src/ should not have been created"
    );
}

#[test]
fn test_init_refuses_to_overwrite_main() {
    let dir = temp_dir_named("partial_state");
    let src = dir.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("main.silt"), "fn main() {}\n").unwrap();
    let out = silt_cmd()
        .arg("init")
        .current_dir(&dir)
        .output()
        .expect("failed to invoke silt init");
    assert!(!out.status.success(), "silt init should have refused");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("src/main.silt already exists"),
        "stderr lacked overwrite refusal: {stderr}"
    );
    assert!(
        !dir.join("silt.toml").exists(),
        "silt.toml should not have been created"
    );
}

#[test]
fn test_init_then_run_works() {
    let dir = temp_dir_named("runnable_pkg");
    let init = silt_cmd()
        .arg("init")
        .current_dir(&dir)
        .output()
        .expect("failed to invoke silt init");
    assert!(init.status.success(), "silt init failed: {:?}", init);

    let run = silt_cmd()
        .arg("run")
        .current_dir(&dir)
        .output()
        .expect("failed to invoke silt run");
    assert!(
        run.status.success(),
        "silt run failed: stdout={} stderr={}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("hello, silt!"),
        "expected greeting in stdout, got: {stdout}"
    );
}
