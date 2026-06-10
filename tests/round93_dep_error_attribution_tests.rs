//! Round 93 regression: runtime errors inside package-DEPENDENCY code
//! must be rendered against the dependency's own source file, not the
//! importer's.
//!
//! Before the fix, `cli::module_sources::collect_module_function_sources`
//! resolved imports only as `<dir-of-entry>/<name>.silt` — manifest deps
//! (path deps, git checkouts) were never mapped, so the error renderer
//! fell back to the entry file's source and printed e.g.
//! `error[runtime]: division by zero --> .../proj/src/main.silt:2:3`
//! with a caret on an unrelated (often blank) line, plus a call-stack
//! frame `divide at .../main.silt:2:3` — wrong file in both places.
//!
//! The fix mirrors the compiler's `resolve_import` rules through the
//! manifest/lockfile channel (`Lockfile::package_roots`): an import
//! segment matching a dep package resolves to `<dep_src>/lib.silt`, and
//! the dep's OWN sibling imports resolve against the dep's source root.
//!
//! Sibling-module attribution within the importer's package (the prior
//! E1 fix) must keep working — locked here by the control test.
//!
//! Full RUN-path tests: each builds real packages on disk (silt.toml +
//! src/ + path-dep manifest) and executes via the CLI binary.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Fresh per-test temp workspace so parallel test runs don't collide.
fn temp_workspace(label: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "silt_round93_depattr_{label}_{}_{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp workspace");
    dir
}

/// Build the standard two-package workspace:
///
/// ```text
/// <ws>/r93_mathdep/silt.toml            (package "r93_mathdep")
/// <ws>/r93_mathdep/src/lib.silt         pub fn divide — divides at line 2 col 3
/// <ws>/r93_mathdep/src/inner.silt       pub fn deep_div — divides at line 2 col 3
/// <ws>/app/silt.toml                    [dependencies] r93_mathdep = { path = "../r93_mathdep" }
/// <ws>/app/src/main.silt                (per-test body)
/// ```
///
/// Returns the path to `app/src/main.silt`.
fn build_workspace(ws: &Path, main_src: &str) -> PathBuf {
    let dep = ws.join("r93_mathdep");
    std::fs::create_dir_all(dep.join("src")).expect("create dep dirs");
    std::fs::write(
        dep.join("silt.toml"),
        "[package]\nname = \"r93_mathdep\"\nversion = \"0.1.0\"\n",
    )
    .expect("write dep silt.toml");
    // `a / b` sits at line 2, col 3 of BOTH dep files — the error span
    // each test asserts on.
    std::fs::write(
        dep.join("src/inner.silt"),
        "pub fn deep_div(a: Int, b: Int) -> Int {\n  a / b\n}\n",
    )
    .expect("write dep inner.silt");
    std::fs::write(
        dep.join("src/lib.silt"),
        "import inner\n\npub fn divide(a: Int, b: Int) -> Int {\n  a / b\n}\n\npub fn divide_deep(a: Int, b: Int) -> Int {\n  inner.deep_div(a, b)\n}\n",
    )
    .expect("write dep lib.silt");

    let app = ws.join("app");
    std::fs::create_dir_all(app.join("src")).expect("create app dirs");
    std::fs::write(
        app.join("silt.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nr93_mathdep = { path = \"../r93_mathdep\" }\n",
    )
    .expect("write app silt.toml");
    let main_path = app.join("src/main.silt");
    std::fs::write(&main_path, main_src).expect("write app main.silt");
    main_path
}

/// `silt run <main_path>`, returning (stderr, success).
fn silt_run(main_path: &Path) -> (String, bool) {
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .arg("run")
        .arg(main_path)
        .output()
        .expect("spawn silt run");
    (
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

/// Extract the `-->` locator line (the rendered error location).
fn locator_line(stderr: &str) -> &str {
    stderr
        .lines()
        .find(|l| l.contains("-->"))
        .unwrap_or_else(|| panic!("no `-->` locator line in stderr:\n{stderr}"))
}

// Note on the dep-side spans: `a / b` in the dep's lib.silt is at
// 4:3 (lines 1-2 are `import inner` + blank); in inner.silt it's at
// 2:3. The PRE-fix bug rendered exactly those line:col pairs against
// main.silt — so every dep-side assertion checks BOTH the file name
// and that main.silt is absent from the same line.

/// Finding case 1: a runtime error inside a path-dep function must be
/// attributed to the DEP's lib.silt (file + line:col), both in the
/// `-->` locator and in the call-stack frame, and main.silt must NOT
/// appear as the error location.
///
/// Pre-fix: locator was `--> .../app/src/main.silt:4:3` (a caret on
/// main's closing-brace line) and the stack frame read
/// `divide at .../main.silt:4:3`.
#[test]
fn runtime_error_in_path_dep_attributes_to_dep_lib() {
    let ws = temp_workspace("path_dep");
    let main_path = build_workspace(
        &ws,
        "import r93_mathdep\n\nfn main() {\n  r93_mathdep.divide(1, 0)\n}\n",
    );
    let (stderr, ok) = silt_run(&main_path);
    assert!(!ok, "division by zero must exit non-zero; stderr={stderr}");
    assert!(
        stderr.contains("division by zero"),
        "expected division-by-zero error; stderr={stderr}"
    );

    let locator = locator_line(&stderr);
    assert!(
        locator.contains("r93_mathdep") && locator.contains("lib.silt:4:3"),
        "error location must be the dep's lib.silt:4:3; locator={locator}\nstderr={stderr}"
    );
    assert!(
        !locator.contains("main.silt"),
        "importer's main.silt must NOT appear as the error location; locator={locator}\nstderr={stderr}"
    );

    // Call-stack frame for `divide` must point into the dep too.
    let divide_frame = stderr
        .lines()
        .find(|l| l.trim_start().starts_with("->") && l.contains("divide"))
        .unwrap_or_else(|| panic!("no `divide` call-stack frame in stderr:\n{stderr}"));
    assert!(
        divide_frame.contains("r93_mathdep") && divide_frame.contains("lib.silt:4:3"),
        "`divide` frame must point at the dep's lib.silt:4:3; frame={divide_frame}\nstderr={stderr}"
    );
    assert!(
        !divide_frame.contains("main.silt"),
        "`divide` frame must not point at main.silt; frame={divide_frame}\nstderr={stderr}"
    );

    // The `main` frame still legitimately points at main.silt (the call
    // site) — sanity-check the renderer didn't swing the other way.
    let main_frame = stderr
        .lines()
        .find(|l| l.trim_start().starts_with("->") && l.contains("main  at"))
        .unwrap_or_else(|| panic!("no `main` call-stack frame in stderr:\n{stderr}"));
    assert!(
        main_frame.contains("main.silt:4:3"),
        "`main` frame must keep pointing at the call site in main.silt; frame={main_frame}\nstderr={stderr}"
    );

    let _ = std::fs::remove_dir_all(&ws);
}

/// Nested case: the dep's lib.silt imports its OWN sibling module
/// (`inner`); an error inside that sibling must attribute to the dep's
/// inner.silt, resolved against the DEP's source root — not the
/// consumer's, and not the dep's lib.silt.
#[test]
fn runtime_error_in_dep_sibling_module_attributes_to_dep_file() {
    let ws = temp_workspace("dep_sibling");
    let main_path = build_workspace(
        &ws,
        "import r93_mathdep\n\nfn main() {\n  r93_mathdep.divide_deep(1, 0)\n}\n",
    );
    let (stderr, ok) = silt_run(&main_path);
    assert!(!ok, "division by zero must exit non-zero; stderr={stderr}");

    let locator = locator_line(&stderr);
    assert!(
        locator.contains("r93_mathdep") && locator.contains("inner.silt:2:3"),
        "error location must be the dep's inner.silt:2:3; locator={locator}\nstderr={stderr}"
    );
    assert!(
        !locator.contains("main.silt") && !locator.contains("lib.silt"),
        "neither main.silt nor lib.silt may be the error location; locator={locator}\nstderr={stderr}"
    );

    // Frames: deep_div → inner.silt, divide_deep → lib.silt.
    let deep_frame = stderr
        .lines()
        .find(|l| l.trim_start().starts_with("->") && l.contains("deep_div"))
        .unwrap_or_else(|| panic!("no `deep_div` frame in stderr:\n{stderr}"));
    assert!(
        deep_frame.contains("inner.silt:2:3"),
        "`deep_div` frame must point at the dep's inner.silt:2:3; frame={deep_frame}\nstderr={stderr}"
    );
    let wrapper_frame = stderr
        .lines()
        .find(|l| l.trim_start().starts_with("->") && l.contains("divide_deep"))
        .unwrap_or_else(|| panic!("no `divide_deep` frame in stderr:\n{stderr}"));
    assert!(
        wrapper_frame.contains("lib.silt:8:3"),
        "`divide_deep` frame must point at the dep's lib.silt:8:3; frame={wrapper_frame}\nstderr={stderr}"
    );

    let _ = std::fs::remove_dir_all(&ws);
}

/// Control (E1, prior fix — must pass before AND after): an error in a
/// sibling module of the importer's OWN package still attributes to
/// that sibling file.
#[test]
fn sibling_module_attribution_still_correct() {
    let ws = temp_workspace("sibling_control");
    let main_path = build_workspace(&ws, "import util\n\nfn main() {\n  util.boom(1, 0)\n}\n");
    // Add the sibling module next to main.silt.
    std::fs::write(
        ws.join("app/src/util.silt"),
        "pub fn boom(a: Int, b: Int) -> Int {\n  a / b\n}\n",
    )
    .expect("write util.silt");
    let (stderr, ok) = silt_run(&main_path);
    assert!(!ok, "division by zero must exit non-zero; stderr={stderr}");

    let locator = locator_line(&stderr);
    assert!(
        locator.contains("util.silt:2:3"),
        "error location must be the sibling util.silt:2:3; locator={locator}\nstderr={stderr}"
    );
    assert!(
        !locator.contains("main.silt"),
        "main.silt must not be the error location; locator={locator}\nstderr={stderr}"
    );
    let boom_frame = stderr
        .lines()
        .find(|l| l.trim_start().starts_with("->") && l.contains("boom"))
        .unwrap_or_else(|| panic!("no `boom` frame in stderr:\n{stderr}"));
    assert!(
        boom_frame.contains("util.silt:2:3"),
        "`boom` frame must point at util.silt:2:3; frame={boom_frame}\nstderr={stderr}"
    );

    let _ = std::fs::remove_dir_all(&ws);
}

/// Control: an error in the importer's OWN code (with a dep imported
/// and successfully used) still attributes to main.silt at the correct
/// line:col — the dep mapping must not steal attribution of main-file
/// frames.
#[test]
fn error_in_importer_own_code_attributes_to_main() {
    let ws = temp_workspace("own_code");
    let main_path = build_workspace(
        &ws,
        "import r93_mathdep\n\nfn main() {\n  let ok = r93_mathdep.divide(4, 2)\n  println(ok)\n  1 / 0\n}\n",
    );
    let (stderr, ok) = silt_run(&main_path);
    assert!(!ok, "division by zero must exit non-zero; stderr={stderr}");

    let locator = locator_line(&stderr);
    assert!(
        locator.contains("main.silt:6:3"),
        "error location must be main.silt:6:3 (the `1 / 0` in main); locator={locator}\nstderr={stderr}"
    );
    assert!(
        !locator.contains("lib.silt") && !locator.contains("inner.silt"),
        "no dep file may be the error location; locator={locator}\nstderr={stderr}"
    );
    // The rendered snippet must be main's own line, proving the right
    // SOURCE text was used (not just the right path label).
    assert!(
        stderr.contains("1 / 0"),
        "snippet must show main's `1 / 0` line; stderr={stderr}"
    );

    let _ = std::fs::remove_dir_all(&ws);
}

// ── Git-dep variant (gated; requires git on PATH) ──────────────────────
//
// Mirrors the gating convention of tests/lockfile_tests.rs: skipped by
// default, opt in with SILT_GIT_INTEGRATION_TESTS=1. The test never
// reaches the network — the "git dep" is a local `git init --bare`
// repo addressed by a file:// URL — but it shells out to git, so it
// stays opt-in to keep default `cargo test` runs hermetic.

fn skip_unless_git_integration() -> bool {
    if std::env::var("SILT_GIT_INTEGRATION_TESTS").is_err() {
        eprintln!("SKIP: git test skipped; set SILT_GIT_INTEGRATION_TESTS=1 to enable");
        true
    } else {
        false
    }
}

/// Run `git <args>` in `cwd`, panicking on failure.
fn git(cwd: &Path, args: &[&str]) {
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
}

/// Same attribution check as the path-dep test, but with the dep
/// fetched from a (local, file://) git repo, exercising the lockfile's
/// git-checkout-cache root resolution. XDG_CACHE_HOME is pointed at the
/// temp workspace so the checkout cache is hermetic per test.
#[test]
fn runtime_error_in_git_dep_attributes_to_dep_lib() {
    if skip_unless_git_integration() {
        return;
    }
    let ws = temp_workspace("git_dep");

    // Author the dep package and publish it to a local bare repo.
    let staging = ws.join("staging");
    let bare = ws.join("bare.git");
    std::fs::create_dir_all(staging.join("src")).expect("create staging dirs");
    std::fs::create_dir_all(&bare).expect("create bare dir");
    std::fs::write(
        staging.join("silt.toml"),
        "[package]\nname = \"r93_gitdep\"\nversion = \"0.1.0\"\n",
    )
    .expect("write staging silt.toml");
    std::fs::write(
        staging.join("src/lib.silt"),
        "pub fn divide(a: Int, b: Int) -> Int {\n  a / b\n}\n",
    )
    .expect("write staging lib.silt");
    git(&bare, &["init", "--bare", "--initial-branch=main"]);
    git(&staging, &["init", "--initial-branch=main"]);
    git(&staging, &["config", "user.email", "tests@silt.local"]);
    git(&staging, &["config", "user.name", "silt tests"]);
    git(&staging, &["add", "."]);
    git(&staging, &["commit", "-m", "initial"]);
    let bare_url = format!("file://{}", bare.display());
    git(&staging, &["remote", "add", "origin", bare_url.as_str()]);
    git(&staging, &["push", "origin", "main"]);

    // Consumer app depending on the git package.
    let app = ws.join("app");
    std::fs::create_dir_all(app.join("src")).expect("create app dirs");
    std::fs::write(
        app.join("silt.toml"),
        format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nr93_gitdep = {{ git = \"{bare_url}\", branch = \"main\" }}\n"
        ),
    )
    .expect("write app silt.toml");
    let main_path = app.join("src/main.silt");
    std::fs::write(
        &main_path,
        "import r93_gitdep\n\nfn main() {\n  r93_gitdep.divide(1, 0)\n}\n",
    )
    .expect("write app main.silt");

    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .arg("run")
        .arg(&main_path)
        // Hermetic git-checkout cache, used both by the lock resolver
        // (fetch) and by the error renderer (cache_for lookup).
        .env("XDG_CACHE_HOME", ws.join("cache"))
        .output()
        .expect("spawn silt run");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "division by zero must exit non-zero; stderr={stderr}"
    );

    let locator = locator_line(&stderr);
    assert!(
        locator.contains("lib.silt:2:3"),
        "error location must be the git checkout's lib.silt:2:3; locator={locator}\nstderr={stderr}"
    );
    assert!(
        !locator.contains("main.silt"),
        "importer's main.silt must NOT be the error location; locator={locator}\nstderr={stderr}"
    );
    let divide_frame = stderr
        .lines()
        .find(|l| l.trim_start().starts_with("->") && l.contains("divide"))
        .unwrap_or_else(|| panic!("no `divide` frame in stderr:\n{stderr}"));
    assert!(
        divide_frame.contains("lib.silt:2:3"),
        "`divide` frame must point at the git checkout's lib.silt:2:3; frame={divide_frame}\nstderr={stderr}"
    );

    let _ = std::fs::remove_dir_all(&ws);
}
