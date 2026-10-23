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
    let loaded =
        silt::manifest::Manifest::load(&manifest_path, &mut silt::source::SourceMap::new())
            .unwrap_or_else(|e| panic!("Manifest::load failed: {e:?}"));
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

// ── `silt init --lib` ─────────────────────────────────────────────────

#[test]
fn test_init_lib_creates_a_library_that_checks_and_is_imported() {
    let dir = temp_dir_named("shapes");
    let out = silt_cmd()
        .args(["init", "--lib"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "silt init --lib failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.starts_with("created silt package `shapes`:\n"),
        "{stdout}"
    );
    let lib_line = format!("src{}lib.silt\n", std::path::MAIN_SEPARATOR);
    assert!(stdout.contains(&lib_line), "{stdout}");
    assert!(
        stdout.ends_with("\n  check: silt check\n  test:  silt test\n"),
        "{stdout}"
    );
    assert_eq!(
        read(&dir.join("src").join("lib.silt")),
        "pub fn hello() -> String {\n  \"hello, silt!\"\n}\n"
    );
    assert!(
        !dir.join("src").join("main.silt").exists(),
        "a library has no main"
    );

    let check = silt_cmd().arg("check").current_dir(&dir).output().unwrap();
    assert!(
        check.status.success(),
        "silt check of the library failed: {}",
        String::from_utf8_lossy(&check.stderr)
    );

    // A program that depends on it calls its function.
    let app = dir.parent().unwrap().join("app");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(
        app.join("silt.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nshapes = { path = \"../shapes\" }\n",
    )
    .unwrap();
    fs::write(
        app.join("src").join("main.silt"),
        "import shapes\nfn main() { println(shapes.hello()) }\n",
    )
    .unwrap();
    let run = silt_cmd().arg("run").current_dir(&app).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "hello, silt!\n");
}

#[test]
fn test_init_lib_refuses_to_overwrite_lib() {
    let dir = temp_dir_named("has_lib");
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("src").join("lib.silt"), "pub fn mine() { 1 }\n").unwrap();
    let out = silt_cmd()
        .args(["init", "--lib"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr).starts_with("src/lib.silt already exists at "),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        read(&dir.join("src").join("lib.silt")),
        "pub fn mine() { 1 }\n"
    );
    assert!(!dir.join("silt.toml").exists(), "nothing is written");
}

#[test]
fn test_init_help_tells_of_lib() {
    let out = silt_cmd().args(["init", "--help"]).output().unwrap();
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(help.starts_with("Usage: silt init [--lib]\n"), "{help}");
    assert!(
        help.contains("with --lib\nsrc/lib.silt, a library"),
        "{help}"
    );
    assert!(
        help.contains("  --lib    A library: src/lib.silt instead of src/main.silt\n"),
        "{help}"
    );
}
