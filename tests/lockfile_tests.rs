//! End-to-end tests for the v0.7 lockfile + `silt update` command.
//!
//! Exercises the auto-regenerate path (silt run / check / test create
//! and refresh `silt.lock` automatically) and the explicit `silt update`
//! command (which always rewrites the lock from scratch). Each test
//! stages a real on-disk package layout (silt.toml + src/) and shells
//! out to the silt binary so we cover the full CLI dispatch.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use silt::git::GitRef;
use silt::lockfile::{LockedPackage, LockedSource, Lockfile};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

fn fresh_workspace(prefix: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("silt_lockfile_tests_{prefix}_{n}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Write a Cargo-style silt package: silt.toml + src/main.silt and
/// (optionally) src/lib.silt + arbitrary extra files. `deps` is the
/// `[dependencies]` body lines (already formatted as `name = { path = "..." }`).
fn write_package(dir: &Path, pkg_name: &str, deps: &[String], main_body: &str) {
    fs::create_dir_all(dir).unwrap();
    let mut manifest = format!("[package]\nname = \"{pkg_name}\"\nversion = \"0.1.0\"\n");
    if !deps.is_empty() {
        manifest.push_str("\n[dependencies]\n");
        for d in deps {
            manifest.push_str(d);
            manifest.push('\n');
        }
    }
    fs::write(dir.join("silt.toml"), manifest).unwrap();
    let src = dir.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("main.silt"), main_body).unwrap();
}

fn write_lib_package(dir: &Path, pkg_name: &str, deps: &[String], lib_body: &str) {
    fs::create_dir_all(dir).unwrap();
    let mut manifest = format!("[package]\nname = \"{pkg_name}\"\nversion = \"0.1.0\"\n");
    if !deps.is_empty() {
        manifest.push_str("\n[dependencies]\n");
        for d in deps {
            manifest.push_str(d);
            manifest.push('\n');
        }
    }
    fs::write(dir.join("silt.toml"), manifest).unwrap();
    let src = dir.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("lib.silt"), lib_body).unwrap();
}

fn read_lockfile(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e))
}

#[test]
fn test_lockfile_generated_on_first_run() {
    let ws = fresh_workspace("first_run");
    let app = ws.join("app");
    let dep = ws.join("calc");
    write_lib_package(&dep, "calc", &[], "pub fn add(a, b) = a + b\n");
    write_package(
        &app,
        "the_app",
        &[r#"calc = { path = "../calc" }"#.to_string()],
        "import calc\nfn main() { println(calc.add(2, 3)) }\n",
    );

    let out = silt_cmd()
        .arg("run")
        .current_dir(&app)
        .output()
        .expect("failed to invoke silt run");
    assert!(
        out.status.success(),
        "silt run failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let lock_path = app.join("silt.lock");
    assert!(
        lock_path.is_file(),
        "silt.lock should have been created at {}",
        lock_path.display()
    );
    let lock = read_lockfile(&lock_path);
    assert!(
        lock.contains("name = \"the_app\""),
        "lock missing app: {lock}"
    );
    assert!(
        lock.contains("name = \"calc\""),
        "lock missing calc: {lock}"
    );
    assert!(
        lock.contains("checksum = \"sha256:"),
        "lock missing checksum: {lock}"
    );
}

#[test]
fn test_lockfile_unchanged_on_second_run() {
    let ws = fresh_workspace("second_run");
    let app = ws.join("app");
    let dep = ws.join("dep");
    write_lib_package(&dep, "dep", &[], "pub fn id(x) = x\n");
    write_package(
        &app,
        "stable_app",
        &[r#"dep = { path = "../dep" }"#.to_string()],
        "import dep\nfn main() { println(dep.id(7)) }\n",
    );

    let first = silt_cmd().arg("run").current_dir(&app).output().unwrap();
    assert!(first.status.success(), "first run failed");
    let lock_path = app.join("silt.lock");
    let first_text = read_lockfile(&lock_path);
    let first_mtime = fs::metadata(&lock_path).unwrap().modified().unwrap();

    // Sleep just enough that a stray rewrite would change mtime
    // measurably even on coarse-grained filesystems.
    std::thread::sleep(std::time::Duration::from_millis(50));

    let second = silt_cmd().arg("run").current_dir(&app).output().unwrap();
    assert!(second.status.success(), "second run failed");
    let second_text = read_lockfile(&lock_path);
    let second_mtime = fs::metadata(&lock_path).unwrap().modified().unwrap();

    assert_eq!(
        first_text, second_text,
        "lockfile content changed between runs"
    );
    assert_eq!(
        first_mtime, second_mtime,
        "lockfile mtime changed between runs (was rewritten unnecessarily)"
    );
}

#[test]
fn test_lockfile_regenerates_when_manifest_changes() {
    let ws = fresh_workspace("manifest_change");
    let app = ws.join("app");
    let calc = ws.join("calc");
    let extra = ws.join("extra");
    write_lib_package(&calc, "calc", &[], "pub fn one() = 1\n");
    write_lib_package(&extra, "extra", &[], "pub fn two() = 2\n");
    write_package(
        &app,
        "growing_app",
        &[r#"calc = { path = "../calc" }"#.to_string()],
        "import calc\nfn main() { println(calc.one()) }\n",
    );

    let first = silt_cmd().arg("run").current_dir(&app).output().unwrap();
    assert!(first.status.success(), "first run failed: {first:?}");
    let lock_path = app.join("silt.lock");
    let first_text = read_lockfile(&lock_path);
    assert!(first_text.contains("name = \"calc\""));
    assert!(
        !first_text.contains("name = \"extra\""),
        "extra not yet a dep"
    );

    // Append a new dep to the manifest.
    let manifest = app.join("silt.toml");
    let manifest_text = fs::read_to_string(&manifest).unwrap();
    let manifest_text = manifest_text.replace(
        "[dependencies]\ncalc = { path = \"../calc\" }\n",
        "[dependencies]\ncalc = { path = \"../calc\" }\nextra = { path = \"../extra\" }\n",
    );
    fs::write(&manifest, manifest_text).unwrap();
    // Update main.silt so type-check / compile still passes.
    fs::write(
        app.join("src/main.silt"),
        "import calc\nimport extra\nfn main() { println(calc.one() + extra.two()) }\n",
    )
    .unwrap();

    let second = silt_cmd().arg("run").current_dir(&app).output().unwrap();
    assert!(
        second.status.success(),
        "second run failed: stdout={} stderr={}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains("Updating silt.lock"),
        "expected auto-regenerate notice on stderr; got: {stderr}"
    );
    let second_text = read_lockfile(&lock_path);
    assert!(
        second_text.contains("name = \"extra\""),
        "lock should now include extra; got:\n{second_text}"
    );
}

#[test]
fn test_lockfile_regenerates_when_dep_content_changes() {
    let ws = fresh_workspace("dep_change");
    let app = ws.join("app");
    let dep = ws.join("dep");
    write_lib_package(&dep, "dep", &[], "pub fn answer() = 41\n");
    write_package(
        &app,
        "cs_app",
        &[r#"dep = { path = "../dep" }"#.to_string()],
        "import dep\nfn main() { println(dep.answer()) }\n",
    );

    // Initial lock from `silt update`.
    let init = silt_cmd().arg("update").current_dir(&app).output().unwrap();
    assert!(init.status.success(), "silt update failed: {init:?}");
    let lock_path = app.join("silt.lock");
    let initial_text = read_lockfile(&lock_path);

    // Mutate the dep's source — the auto-regenerate logic doesn't pick
    // this up (it only watches the *manifest*) but `silt update` does.
    fs::write(dep.join("src/lib.silt"), "pub fn answer() = 42\n").unwrap();

    let updated = silt_cmd().arg("update").current_dir(&app).output().unwrap();
    assert!(updated.status.success(), "silt update failed: {updated:?}");
    let new_text = read_lockfile(&lock_path);
    assert_ne!(
        initial_text, new_text,
        "lockfile checksum should change when dep source changes; got identical content"
    );
}

#[test]
fn test_silt_update_works() {
    let ws = fresh_workspace("plain_update");
    let app = ws.join("app");
    let dep = ws.join("dep");
    write_lib_package(&dep, "dep", &[], "pub fn k() = 5\n");
    write_package(
        &app,
        "plain_update_app",
        &[r#"dep = { path = "../dep" }"#.to_string()],
        "import dep\nfn main() { println(dep.k()) }\n",
    );

    let out = silt_cmd().arg("update").current_dir(&app).output().unwrap();
    assert!(
        out.status.success(),
        "silt update failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Locked"),
        "expected 'Locked N dependencies.' line; got: {stderr}"
    );
    assert!(app.join("silt.lock").is_file(), "silt.lock not written");
}

#[test]
fn test_silt_update_named_dep_works() {
    let ws = fresh_workspace("named_update");
    let app = ws.join("app");
    let dep = ws.join("dep");
    write_lib_package(&dep, "dep", &[], "pub fn k() = 5\n");
    write_package(
        &app,
        "named_update_app",
        &[r#"dep = { path = "../dep" }"#.to_string()],
        "import dep\nfn main() { println(dep.k()) }\n",
    );

    let out = silt_cmd()
        .args(["update", "dep"])
        .current_dir(&app)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "silt update <name> failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(app.join("silt.lock").is_file(), "silt.lock not written");
}

#[test]
fn test_silt_update_named_dep_unknown_errors() {
    let ws = fresh_workspace("named_unknown");
    let app = ws.join("app");
    write_package(&app, "noop_app", &[], "fn main() {}\n");

    let out = silt_cmd()
        .args(["update", "no_such_dep"])
        .current_dir(&app)
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "expected failure for unknown dep name"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not declared"),
        "expected 'not declared' error; got: {stderr}"
    );
}

#[test]
fn test_silt_update_outside_package_errors() {
    let dir = fresh_workspace("outside");
    let out = silt_cmd().arg("update").current_dir(&dir).output().unwrap();
    assert!(
        !out.status.success(),
        "silt update outside a package should fail; succeeded with {:?}",
        out
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("must be run inside a silt package"),
        "expected 'must be run inside a silt package' error; got: {stderr}"
    );
}

#[test]
fn test_lockfile_dep_missing_path_is_error() {
    let ws = fresh_workspace("missing_dep");
    let app = ws.join("app");
    write_package(
        &app,
        "missing_dep_app",
        &[r#"ghost = { path = "../does-not-exist" }"#.to_string()],
        "fn main() {}\n",
    );
    let out = silt_cmd().arg("update").current_dir(&app).output().unwrap();
    assert!(
        !out.status.success(),
        "silt update should error when a dep path is missing"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("does not exist"),
        "expected 'does not exist' diagnostic; got: {stderr}"
    );
}

#[test]
fn test_lockfile_dep_not_a_package_is_error() {
    let ws = fresh_workspace("not_a_pkg");
    let app = ws.join("app");
    let dep = ws.join("not_a_pkg");
    fs::create_dir_all(&dep).unwrap();
    fs::write(dep.join("README.md"), "# nope\n").unwrap();
    write_package(
        &app,
        "not_pkg_app",
        &[r#"not_a_pkg = { path = "../not_a_pkg" }"#.to_string()],
        "fn main() {}\n",
    );
    let out = silt_cmd().arg("update").current_dir(&app).output().unwrap();
    assert!(
        !out.status.success(),
        "silt update should error when dep dir has no silt.toml"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("is not a silt package"),
        "expected 'is not a silt package' diagnostic; got: {stderr}"
    );
}

#[test]
fn test_lockfile_transitive_deps_locked() {
    // A depends on B depends on C. The lockfile must contain entries
    // for the local app, B, and C — even though only B appears in the
    // app's manifest.
    let ws = fresh_workspace("transitive");
    let c = ws.join("c");
    let b = ws.join("b");
    let app = ws.join("app");
    write_lib_package(&c, "c", &[], "pub fn z() = 100\n");
    write_lib_package(
        &b,
        "b",
        &[r#"c = { path = "../c" }"#.to_string()],
        "import c\npub fn y() = c.z() + 1\n",
    );
    write_package(
        &app,
        "transitive_app",
        &[r#"b = { path = "../b" }"#.to_string()],
        "import b\nfn main() { println(b.y()) }\n",
    );

    let out = silt_cmd().arg("update").current_dir(&app).output().unwrap();
    assert!(
        out.status.success(),
        "silt update failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lock = read_lockfile(&app.join("silt.lock"));
    assert!(
        lock.contains("name = \"transitive_app\""),
        "lock missing app"
    );
    assert!(lock.contains("name = \"b\""), "lock missing b");
    assert!(
        lock.contains("name = \"c\""),
        "lock missing transitive dep c; lock:\n{lock}"
    );
}

#[test]
fn test_lockfile_format_stable() {
    // Build the same manifest twice in two separate workspaces and
    // assert the lockfiles render byte-identically modulo the dep
    // path. Sorted-by-name + fixed key order makes the file
    // git-friendly: a no-op `silt update` produces no diff.
    fn make_workspace() -> PathBuf {
        let ws = fresh_workspace("format_stable");
        let app = ws.join("app");
        let alpha = ws.join("alpha");
        let zeta = ws.join("zeta");
        write_lib_package(&alpha, "alpha", &[], "pub fn a() = 1\n");
        write_lib_package(&zeta, "zeta", &[], "pub fn z() = 2\n");
        write_package(
            &app,
            "stable_app",
            &[
                r#"zeta = { path = "../zeta" }"#.to_string(),
                r#"alpha = { path = "../alpha" }"#.to_string(),
            ],
            "fn main() {}\n",
        );
        let out = silt_cmd().arg("update").current_dir(&app).output().unwrap();
        assert!(out.status.success(), "silt update failed");
        ws
    }

    let ws1 = make_workspace();
    let ws2 = make_workspace();
    let lock1 = read_lockfile(&ws1.join("app/silt.lock"));
    let lock2 = read_lockfile(&ws2.join("app/silt.lock"));

    // Strip the absolute path lines (workspaces are different temp
    // dirs); everything else must match exactly.
    fn strip_paths(s: &str) -> String {
        s.lines()
            .filter(|l| !l.contains("source = "))
            .collect::<Vec<_>>()
            .join("\n")
    }
    assert_eq!(
        strip_paths(&lock1),
        strip_paths(&lock2),
        "lockfile format should be deterministic; got divergence"
    );

    // alpha must appear before zeta — sorted by name regardless of
    // declaration order in silt.toml.
    let alpha_pos = lock1.find("name = \"alpha\"").expect("alpha present");
    let zeta_pos = lock1.find("name = \"zeta\"").expect("zeta present");
    assert!(
        alpha_pos < zeta_pos,
        "expected alpha before zeta in:\n{lock1}"
    );
}

#[test]
fn test_silt_fmt_does_not_create_lockfile() {
    // Read-only commands must not silently mutate the workspace.
    // `silt fmt --check` on a package without a lockfile should leave
    // the package without a lockfile.
    let ws = fresh_workspace("fmt_no_lock");
    let app = ws.join("app");
    let dep = ws.join("dep");
    write_lib_package(&dep, "dep", &[], "pub fn x() = 1\n");
    write_package(
        &app,
        "fmt_app",
        &[r#"dep = { path = "../dep" }"#.to_string()],
        "fn main() {}\n",
    );
    let out = silt_cmd()
        .args(["fmt", "--check"])
        .current_dir(&app)
        .output()
        .unwrap();
    // Whether --check passes or fails is irrelevant for this test —
    // we just want to confirm no lockfile materialized.
    let _ = out;
    assert!(
        !app.join("silt.lock").exists(),
        "silt fmt should not create silt.lock; found one at {}",
        app.join("silt.lock").display()
    );
}

// ── Hermetic git lockfile schema tests ────────────────────────────────
//
// These tests don't touch the network — they construct `Lockfile`
// values in memory, write them to disk, and parse them back. They
// pin the on-disk TOML schema for the three git ref forms (rev,
// branch, tag) and guard against accidental drift in the renderer
// or parser.

#[test]
fn test_lockfile_renders_git_source_with_rev() {
    let ws = fresh_workspace("git_rev_roundtrip");
    let lock_path = ws.join("silt.lock");
    let lock = Lockfile {
        version: 1,
        packages: vec![
            LockedPackage {
                name: "rev_app".into(),
                version: "0.1.0".into(),
                source: LockedSource::Local,
                checksum: String::new(),
            },
            LockedPackage {
                name: "rev_dep".into(),
                version: "0.1.0".into(),
                source: LockedSource::Git {
                    url: "https://example.com/rev_dep.git".into(),
                    ref_spec: GitRef::Rev("abc1234".into()),
                    resolved_sha: "abc1234".into(),
                },
                checksum: "sha256:deadbeef".into(),
            },
        ],
    };
    lock.write(&lock_path).expect("write lockfile");
    let text = fs::read_to_string(&lock_path).unwrap();
    // Rev form renders one `rev =` field, no `branch =` / `tag =`.
    assert!(
        text.contains("git = \"https://example.com/rev_dep.git\""),
        "lockfile missing git URL:\n{text}"
    );
    assert!(
        text.contains("rev = \"abc1234\""),
        "lockfile missing rev field:\n{text}"
    );
    assert!(
        !text.contains("branch ="),
        "rev form should not emit branch:\n{text}"
    );
    assert!(
        !text.contains("tag ="),
        "rev form should not emit tag:\n{text}"
    );

    let parsed = Lockfile::load(&lock_path).expect("parse lockfile");
    assert_eq!(parsed, lock, "git rev lockfile did not roundtrip");
}

#[test]
fn test_lockfile_renders_git_source_with_branch() {
    let ws = fresh_workspace("git_branch_roundtrip");
    let lock_path = ws.join("silt.lock");
    let resolved_sha = "0123456789abcdef0123456789abcdef01234567";
    let lock = Lockfile {
        version: 1,
        packages: vec![
            LockedPackage {
                name: "branch_app".into(),
                version: "0.1.0".into(),
                source: LockedSource::Local,
                checksum: String::new(),
            },
            LockedPackage {
                name: "branch_dep".into(),
                version: "0.1.0".into(),
                source: LockedSource::Git {
                    url: "https://example.com/branch_dep.git".into(),
                    ref_spec: GitRef::Branch("main".into()),
                    resolved_sha: resolved_sha.into(),
                },
                checksum: "sha256:deadbeef".into(),
            },
        ],
    };
    lock.write(&lock_path).expect("write lockfile");
    let text = fs::read_to_string(&lock_path).unwrap();
    assert!(
        text.contains("branch = \"main\""),
        "branch form must preserve user intent:\n{text}"
    );
    assert!(
        text.contains(&format!("rev = \"{resolved_sha}\"")),
        "branch form must pin resolved SHA:\n{text}"
    );

    let parsed = Lockfile::load(&lock_path).expect("parse lockfile");
    assert_eq!(parsed, lock, "git branch lockfile did not roundtrip");
}

#[test]
fn test_lockfile_renders_git_source_with_tag() {
    let ws = fresh_workspace("git_tag_roundtrip");
    let lock_path = ws.join("silt.lock");
    let resolved_sha = "fedcba9876543210fedcba9876543210fedcba98";
    let lock = Lockfile {
        version: 1,
        packages: vec![
            LockedPackage {
                name: "tag_app".into(),
                version: "0.1.0".into(),
                source: LockedSource::Local,
                checksum: String::new(),
            },
            LockedPackage {
                name: "tag_dep".into(),
                version: "0.1.0".into(),
                source: LockedSource::Git {
                    url: "https://example.com/tag_dep.git".into(),
                    ref_spec: GitRef::Tag("v1.0.0".into()),
                    resolved_sha: resolved_sha.into(),
                },
                checksum: "sha256:cafebabe".into(),
            },
        ],
    };
    lock.write(&lock_path).expect("write lockfile");
    let text = fs::read_to_string(&lock_path).unwrap();
    assert!(
        text.contains("tag = \"v1.0.0\""),
        "tag form must preserve user intent:\n{text}"
    );
    assert!(
        text.contains(&format!("rev = \"{resolved_sha}\"")),
        "tag form must pin resolved SHA:\n{text}"
    );

    let parsed = Lockfile::load(&lock_path).expect("parse lockfile");
    assert_eq!(parsed, lock, "git tag lockfile did not roundtrip");
}

#[test]
fn test_lockfile_resolve_git_dep_validates_manifest() {
    // A git dep with a malformed Rev (not a hex SHA) must error
    // at resolve time without ever contacting the network. Rev shape
    // validation is offline; the failure is `LockfileError::GitOperation`
    // wrapping `GitError::RefNotFound`.
    let ws = fresh_workspace("git_bad_rev");
    let app = ws.join("app");
    fs::create_dir_all(&app).unwrap();
    fs::write(
        app.join("silt.toml"),
        "[package]\nname = \"bad_rev_app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\n\
         broken = { git = \"https://example.com/broken.git\", rev = \"not-a-sha\" }\n",
    )
    .unwrap();
    let src = app.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("main.silt"), "fn main() {}\n").unwrap();

    let out = silt_cmd().arg("update").current_dir(&app).output().unwrap();
    assert!(
        !out.status.success(),
        "silt update should error on malformed rev; succeeded with stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("git dependency") && stderr.contains("https://example.com/broken.git"),
        "expected git dependency error mentioning the URL; got: {stderr}"
    );
}

// ── Git E2E tests (gated; require git on PATH) ────────────────────────
//
// Skipped by default; opt in with `SILT_GIT_INTEGRATION_TESTS=1`. These
// tests don't reach the public internet — they spawn a local `git init
// --bare` repo and point silt at a `file://` URL — but they do shell
// out to git, which we keep opt-in to match the gating used by the
// other PR 1/2 git tests and to keep `cargo test` fast/hermetic.

fn skip_unless_network() -> bool {
    if std::env::var("SILT_GIT_INTEGRATION_TESTS").is_err() {
        eprintln!("SKIP: network test skipped; set SILT_GIT_INTEGRATION_TESTS=1 to enable");
        true
    } else {
        false
    }
}

/// Create a bare git repo with a tiny silt library committed on `main`,
/// returning `(file_url, head_sha)`. The library exports a single
/// `pub fn answer() = 42` so dependents can both compile and run.
fn make_local_git_silt_package(workspace: &Path) -> (String, String) {
    let staging = workspace.join("staging");
    let bare = workspace.join("bare.git");
    fs::create_dir_all(&staging).unwrap();
    fs::create_dir_all(&bare).unwrap();

    // Author the package.
    fs::write(
        staging.join("silt.toml"),
        "[package]\nname = \"locallib\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::create_dir_all(staging.join("src")).unwrap();
    fs::write(staging.join("src/lib.silt"), "pub fn answer() = 42\n").unwrap();

    // Init the bare repo we'll publish to. Use `master` as the initial
    // branch — older `git init` defaults to it; `--initial-branch=main`
    // exists on newer git but we set the branch explicitly downstream
    // anyway, so be agnostic.
    let run = |cwd: &Path, args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?} spawn failed: {e}"));
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    };

    run(&bare, &["init", "--bare", "--initial-branch=main"]);

    // Init the staging clone, set local identity (so commit doesn't
    // require global config), commit, push to bare.
    run(&staging, &["init", "--initial-branch=main"]);
    run(&staging, &["config", "user.email", "tests@silt.local"]);
    run(&staging, &["config", "user.name", "silt tests"]);
    run(&staging, &["add", "."]);
    run(&staging, &["commit", "-m", "initial"]);
    let bare_url = format!("file://{}", bare.display());
    run(&staging, &["remote", "add", "origin", bare_url.as_str()]);
    run(&staging, &["push", "origin", "main"]);

    let head = run(&staging, &["rev-parse", "HEAD"]);
    let head_sha = String::from_utf8_lossy(&head.stdout).trim().to_string();
    (bare_url, head_sha)
}

#[test]
fn test_silt_update_git_branch_dep_locks_resolved_sha() {
    if skip_unless_network() {
        return;
    }
    let ws = fresh_workspace("git_branch_e2e");
    let (repo_url, head_sha) = make_local_git_silt_package(&ws);
    let app = ws.join("app");
    fs::create_dir_all(&app).unwrap();
    fs::write(
        app.join("silt.toml"),
        format!(
            "[package]\nname = \"branch_e2e_app\"\nversion = \"0.1.0\"\n\n\
             [dependencies]\nlocallib = {{ git = \"{repo_url}\", branch = \"main\" }}\n",
        ),
    )
    .unwrap();
    let src = app.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("main.silt"), "fn main() {}\n").unwrap();

    let out = silt_cmd().arg("update").current_dir(&app).output().unwrap();
    assert!(
        out.status.success(),
        "silt update failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lock = fs::read_to_string(app.join("silt.lock")).unwrap();
    assert!(
        lock.contains("branch = \"main\""),
        "lockfile missing branch intent:\n{lock}"
    );
    assert!(
        lock.contains(&format!("rev = \"{head_sha}\"")),
        "lockfile should pin the bare repo's HEAD `{head_sha}`:\n{lock}"
    );
}

#[test]
fn test_silt_update_git_rev_dep_no_op_on_second_call() {
    if skip_unless_network() {
        return;
    }
    // For a Rev dep, two consecutive `silt update` calls must produce
    // byte-identical lockfiles — the SHA is locked verbatim and no
    // network resolution can change it.
    let ws = fresh_workspace("git_rev_idem");
    let (repo_url, head_sha) = make_local_git_silt_package(&ws);
    let app = ws.join("app");
    fs::create_dir_all(&app).unwrap();
    fs::write(
        app.join("silt.toml"),
        format!(
            "[package]\nname = \"rev_idem_app\"\nversion = \"0.1.0\"\n\n\
             [dependencies]\nlocallib = {{ git = \"{repo_url}\", rev = \"{head_sha}\" }}\n",
        ),
    )
    .unwrap();
    let src = app.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("main.silt"), "fn main() {}\n").unwrap();

    let first = silt_cmd().arg("update").current_dir(&app).output().unwrap();
    assert!(
        first.status.success(),
        "first silt update failed: stderr={}",
        String::from_utf8_lossy(&first.stderr)
    );
    let lock1 = fs::read_to_string(app.join("silt.lock")).unwrap();
    let second = silt_cmd().arg("update").current_dir(&app).output().unwrap();
    assert!(
        second.status.success(),
        "second silt update failed: stderr={}",
        String::from_utf8_lossy(&second.stderr)
    );
    let lock2 = fs::read_to_string(app.join("silt.lock")).unwrap();
    assert_eq!(
        lock1, lock2,
        "rev-dep lockfile should be byte-identical across `silt update` calls"
    );
}

#[test]
fn test_silt_run_with_git_dep_works() {
    if skip_unless_network() {
        return;
    }
    // Full E2E: declare a git dep on a local bare repo holding a real
    // silt library, then `silt run` the consumer. The dep's
    // `pub fn answer() = 42` should be importable and runnable.
    let ws = fresh_workspace("git_run_e2e");
    let (repo_url, _head) = make_local_git_silt_package(&ws);
    let app = ws.join("app");
    fs::create_dir_all(&app).unwrap();
    fs::write(
        app.join("silt.toml"),
        format!(
            "[package]\nname = \"run_e2e_app\"\nversion = \"0.1.0\"\n\n\
             [dependencies]\nlocallib = {{ git = \"{repo_url}\", branch = \"main\" }}\n",
        ),
    )
    .unwrap();
    let src = app.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("main.silt"),
        "import locallib\nfn main() { println(locallib.answer()) }\n",
    )
    .unwrap();

    let out = silt_cmd().arg("run").current_dir(&app).output().unwrap();
    assert!(
        out.status.success(),
        "silt run failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("42"),
        "expected `42` in silt run output; got: {stdout}"
    );

    // Cache must contain the dep's silt.toml at the resolved SHA path.
    let lock = fs::read_to_string(app.join("silt.lock")).unwrap();
    let rev_idx = lock.find("rev = \"").expect("lockfile contains rev");
    let after = &lock[rev_idx + "rev = \"".len()..];
    let end = after.find('"').expect("rev string is closed");
    let sha = &after[..end];
    let cache_path = silt::git::cache_for(&repo_url, sha).expect("cache path");
    assert!(
        cache_path.join("silt.toml").is_file(),
        "expected cache to contain silt.toml at {}",
        cache_path.display()
    );
}
