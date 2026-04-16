//! End-to-end tests for `silt add` (PR 5 of the v0.7 package manager).
//!
//! Each test stages a real on-disk package layout (silt.toml + src/)
//! and shells out to the `silt` binary so the full CLI dispatch path
//! is covered, including manifest discovery, name validation,
//! `toml_edit`-based mutation, and lockfile regeneration.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

fn fresh_workspace(prefix: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("silt_add_tests_{prefix}_{n}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Write a Cargo-style silt package: `silt.toml` + `src/main.silt`.
fn write_app_package(dir: &Path, pkg_name: &str, main_body: &str) {
    fs::create_dir_all(dir).unwrap();
    let manifest = format!("[package]\nname = \"{pkg_name}\"\nversion = \"0.1.0\"\n");
    fs::write(dir.join("silt.toml"), manifest).unwrap();
    let src = dir.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("main.silt"), main_body).unwrap();
}

/// Write a library package with `src/lib.silt`.
fn write_lib_package(dir: &Path, pkg_name: &str, lib_body: &str) {
    fs::create_dir_all(dir).unwrap();
    let manifest = format!("[package]\nname = \"{pkg_name}\"\nversion = \"0.1.0\"\n");
    fs::write(dir.join("silt.toml"), manifest).unwrap();
    let src = dir.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("lib.silt"), lib_body).unwrap();
}

#[test]
fn test_add_to_fresh_manifest() {
    let ws = fresh_workspace("fresh");
    let app = ws.join("app");
    let calc = ws.join("calc");
    write_app_package(&app, "the_app", "fn main() {}\n");
    write_lib_package(&calc, "calc", "pub fn one() = 1\n");

    let out = silt_cmd()
        .args(["add", "calc", "--path", "../calc"])
        .current_dir(&app)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "silt add failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let manifest = fs::read_to_string(app.join("silt.toml")).unwrap();
    assert!(
        manifest.contains("[dependencies]"),
        "manifest missing [dependencies]:\n{manifest}"
    );
    assert!(
        manifest.contains("calc") && manifest.contains("path") && manifest.contains("../calc"),
        "manifest missing calc entry:\n{manifest}"
    );
    assert!(
        app.join("silt.lock").is_file(),
        "silt.lock should have been generated"
    );
    let lock = fs::read_to_string(app.join("silt.lock")).unwrap();
    assert!(
        lock.contains("name = \"calc\""),
        "lock missing calc:\n{lock}"
    );
    assert!(
        lock.contains("name = \"the_app\""),
        "lock missing the_app:\n{lock}"
    );
}

#[test]
fn test_add_when_dependencies_section_missing() {
    let ws = fresh_workspace("missing_deps_section");
    let app = ws.join("app");
    let calc = ws.join("calc");
    write_app_package(&app, "no_deps_yet", "fn main() {}\n");
    write_lib_package(&calc, "calc", "pub fn x() = 1\n");

    // Sanity check: manifest currently has no [dependencies] header.
    let pre = fs::read_to_string(app.join("silt.toml")).unwrap();
    assert!(
        !pre.contains("[dependencies]"),
        "precondition failed:\n{pre}"
    );

    let out = silt_cmd()
        .args(["add", "calc", "--path", "../calc"])
        .current_dir(&app)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "silt add failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let post = fs::read_to_string(app.join("silt.toml")).unwrap();
    assert!(
        post.contains("[dependencies]"),
        "missing [dependencies] header after add:\n{post}"
    );
}

#[test]
fn test_add_when_dependencies_section_exists() {
    let ws = fresh_workspace("existing_deps_section");
    let app = ws.join("app");
    let calc = ws.join("calc");
    let extra = ws.join("extra");
    write_lib_package(&calc, "calc", "pub fn one() = 1\n");
    write_lib_package(&extra, "extra", "pub fn two() = 2\n");

    fs::create_dir_all(&app).unwrap();
    fs::write(
        app.join("silt.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\ncalc = { path = \"../calc\" }\n",
    )
    .unwrap();
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(app.join("src/main.silt"), "fn main() {}\n").unwrap();

    let out = silt_cmd()
        .args(["add", "extra", "--path", "../extra"])
        .current_dir(&app)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "silt add failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let manifest = fs::read_to_string(app.join("silt.toml")).unwrap();
    assert!(
        manifest.contains("calc"),
        "existing calc entry was lost:\n{manifest}"
    );
    assert!(
        manifest.contains("extra"),
        "new extra entry missing:\n{manifest}"
    );
}

#[test]
fn test_add_preserves_formatting() {
    let ws = fresh_workspace("preserve_fmt");
    let app = ws.join("app");
    let calc = ws.join("calc");
    write_lib_package(&calc, "calc", "pub fn one() = 1\n");

    // Manifest with comments and unusual whitespace in unrelated tables.
    fs::create_dir_all(&app).unwrap();
    let original = "# Top-level comment about the package\n\
                    [package]\n\
                    # Inline comment about name\n\
                    name = \"fmt_app\"\n\
                    version    =    \"0.1.0\"   # weird spacing\n\
                    \n\
                    # ── User-styled separator ──\n\
                    [dependencies]\n\
                    # existing dep stays put\n\
                    other = { path = \"../other\" }\n";
    fs::write(app.join("silt.toml"), original).unwrap();
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(app.join("src/main.silt"), "fn main() {}\n").unwrap();
    // We're never going to run the lock for this test, but
    // `silt add` regenerates one — make `other` resolvable so the
    // lock step doesn't error.
    let other = ws.join("other");
    write_lib_package(&other, "other", "pub fn other() = 1\n");

    let out = silt_cmd()
        .args(["add", "calc", "--path", "../calc"])
        .current_dir(&app)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "silt add failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let post = fs::read_to_string(app.join("silt.toml")).unwrap();
    // Comments survive verbatim.
    assert!(
        post.contains("# Top-level comment about the package"),
        "top-level comment lost:\n{post}"
    );
    assert!(
        post.contains("# Inline comment about name"),
        "inline comment lost:\n{post}"
    );
    assert!(
        post.contains("# ── User-styled separator ──"),
        "separator comment lost:\n{post}"
    );
    assert!(
        post.contains("# existing dep stays put"),
        "dep comment lost:\n{post}"
    );
    // Weird spacing on the version line is preserved.
    assert!(
        post.contains("version    =    \"0.1.0\""),
        "weird spacing lost:\n{post}"
    );
    // Existing dep retained, new dep added.
    assert!(post.contains("other"), "existing dep removed:\n{post}");
    assert!(post.contains("calc"), "new dep missing:\n{post}");
}

#[test]
fn test_add_fails_on_duplicate() {
    let ws = fresh_workspace("dup");
    let app = ws.join("app");
    let foo = ws.join("foo");
    write_app_package(&app, "dup_app", "fn main() {}\n");
    write_lib_package(&foo, "foo", "pub fn x() = 1\n");

    let first = silt_cmd()
        .args(["add", "foo", "--path", "../foo"])
        .current_dir(&app)
        .output()
        .unwrap();
    assert!(first.status.success(), "first add failed");

    let second = silt_cmd()
        .args(["add", "foo", "--path", "../foo"])
        .current_dir(&app)
        .output()
        .unwrap();
    assert!(!second.status.success(), "second add should have failed");
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains("dependency 'foo' is already declared"),
        "expected 'already declared' diagnostic; got: {stderr}"
    );
}

#[test]
fn test_add_fails_on_missing_path() {
    let ws = fresh_workspace("missing_path");
    let app = ws.join("app");
    write_app_package(&app, "miss_app", "fn main() {}\n");

    let out = silt_cmd()
        .args(["add", "ghost", "--path", "/does/not/exist/anywhere"])
        .current_dir(&app)
        .output()
        .unwrap();
    assert!(!out.status.success(), "add should have failed");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("does not exist"),
        "expected 'does not exist' diagnostic; got: {stderr}"
    );
}

#[test]
fn test_add_fails_on_non_package_path() {
    let ws = fresh_workspace("non_pkg_path");
    let app = ws.join("app");
    let empty = ws.join("empty_dir");
    write_app_package(&app, "non_pkg_app", "fn main() {}\n");
    fs::create_dir_all(&empty).unwrap();
    fs::write(empty.join("README.md"), "# nope\n").unwrap();

    let out = silt_cmd()
        .args(["add", "empty", "--path", "../empty_dir"])
        .current_dir(&app)
        .output()
        .unwrap();
    assert!(!out.status.success(), "add should have failed");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("is not a silt package") && stderr.contains("no silt.toml"),
        "expected 'is not a silt package (no silt.toml found)' diagnostic; got: {stderr}"
    );
}

#[test]
fn test_add_fails_on_invalid_name() {
    let ws = fresh_workspace("bad_name");
    let app = ws.join("app");
    let foo = ws.join("foo");
    write_app_package(&app, "bad_name_app", "fn main() {}\n");
    write_lib_package(&foo, "foo", "pub fn x() = 1\n");

    let out = silt_cmd()
        .args(["add", "Foo", "--path", "../foo"])
        .current_dir(&app)
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "add with uppercase name should have failed"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("invalid dependency name"),
        "expected 'invalid dependency name' diagnostic; got: {stderr}"
    );
}

#[test]
fn test_add_fails_on_builtin_collision() {
    let ws = fresh_workspace("builtin_clash");
    let app = ws.join("app");
    let pkg = ws.join("listpkg");
    write_app_package(&app, "builtin_clash_app", "fn main() {}\n");
    write_lib_package(&pkg, "listpkg", "pub fn x() = 1\n");

    let out = silt_cmd()
        .args(["add", "list", "--path", "../listpkg"])
        .current_dir(&app)
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "add with builtin name should have failed"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("builtin module") && stderr.contains("list"),
        "expected builtin-collision diagnostic; got: {stderr}"
    );
}

#[test]
fn test_add_outside_package_errors() {
    let ws = fresh_workspace("outside");
    // No silt.toml in `ws` — straight to `silt add` from the bare tmp dir.
    let bar = ws.join("bar");
    fs::create_dir_all(&bar).unwrap();

    let out = silt_cmd()
        .args(["add", "foo", "--path", "bar"])
        .current_dir(&ws)
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "add outside a package should have failed"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("must be run inside a silt package"),
        "expected 'must be run inside a silt package'; got: {stderr}"
    );
}

#[test]
fn test_add_then_run_works() {
    let ws = fresh_workspace("e2e");
    let app = ws.join("app");
    let calc = ws.join("calc");
    write_app_package(&app, "e2e_app", "fn main() {}\n");
    write_lib_package(&calc, "calc", "pub fn add(a, b) = a + b\n");

    let add = silt_cmd()
        .args(["add", "calc", "--path", "../calc"])
        .current_dir(&app)
        .output()
        .unwrap();
    assert!(
        add.status.success(),
        "silt add failed: {}",
        String::from_utf8_lossy(&add.stderr)
    );

    fs::write(
        app.join("src/main.silt"),
        "import calc\nfn main() { println(calc.add(40, 2)) }\n",
    )
    .unwrap();

    let run = silt_cmd().arg("run").current_dir(&app).output().unwrap();
    assert!(
        run.status.success(),
        "silt run failed: stdout={} stderr={}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.trim() == "42",
        "expected output '42'; got: {stdout:?}"
    );
}
