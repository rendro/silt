//! Round 92 GAP regression: `silt check` must surface REAL type errors
//! living in imported user modules.
//!
//! Before the fix, `src/compiler/mod.rs::pre_typecheck_user_module` and
//! `compile_file_module_inner` bound each imported module's typechecker
//! diagnostics to `_errors` / `_type_errors` and dropped them wholesale.
//! Only the entrypoint's own diagnostics ever reached the renderer, so
//! `silt check main.silt` exited 0 on a program whose imported module
//! fails its own direct `silt check` — and `silt run` then died at
//! runtime instead.
//!
//! The fix harvests imported-module diagnostics through the SAME shared
//! suppression predicate the CLI already uses
//! (`silt::diagnostic_filters`): real errors (e.g. type mismatches)
//! surface, rendered against the imported module's own file path and
//! spans, while the import-resolvable cascade (undefined-name / trait
//! shapes behind an "unknown module" warning, e.g. from transitive
//! cross-package imports the typechecker can't see into) stays
//! suppressed — preserving round-91's run/check/test filter parity.
//!
//! E2E coverage via the CLI binary on real on-disk packages:
//!
//!   1. Ill-typed imported module → `silt check` exits nonzero and
//!      names the error with the imported file's path. `silt run`
//!      fails the same way (consistency lock).
//!   2. Clean multi-file package → still checks green.
//!   3. Transitive cross-package import inside a local module →
//!      resolvable shapes stay suppressed; check green, run works.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Fresh per-test temp directory so parallel test runs don't collide.
fn tempdir(label: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "silt_round92_chkimp_{label}_{}_{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Run `silt <subcommand> <file>` and return (stdout, stderr, success).
fn run_silt(subcommand: &str, file: &Path) -> (String, String, bool) {
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .arg(subcommand)
        .arg(file)
        .output()
        .expect("spawn silt");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (stdout, stderr, out.status.success())
}

/// Write a single-package layout: silt.toml + src/<files>.
fn write_package(root: &Path, name: &str, files: &[(&str, &str)]) {
    std::fs::create_dir_all(root.join("src")).expect("create src dir");
    std::fs::write(
        root.join("silt.toml"),
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n"),
    )
    .expect("write silt.toml");
    for (file, contents) in files {
        std::fs::write(root.join("src").join(file), contents).expect("write source file");
    }
}

/// The canonical repro: helper.silt has a real type error (`x + "s"`
/// where Int is required). `silt check main.silt` must exit nonzero
/// and report the mismatch with the imported file's path — before the
/// fix it exited 0 with no output and the error only appeared at
/// runtime.
#[test]
fn check_reports_type_error_in_imported_module() {
    let root = tempdir("illtyped");
    write_package(
        &root,
        "chkimp",
        &[
            ("helper.silt", "pub fn good(x: Int) -> Int = x + \"s\"\n"),
            (
                "main.silt",
                "import helper\n\nfn main() {\n  println(helper.good(1))\n}\n",
            ),
        ],
    );
    let main = root.join("src/main.silt");

    let (stdout, stderr, ok) = run_silt("check", &main);
    assert!(
        !ok,
        "silt check must exit nonzero for an ill-typed imported module; stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stderr.contains("type mismatch"),
        "expected the imported module's type mismatch in check output; stderr={stderr}"
    );
    assert!(
        stderr.contains("helper.silt"),
        "diagnostic must carry the imported module's file path; stderr={stderr}"
    );

    // Consistency lock: `silt run` reports the same diagnostic up
    // front (instead of only dying at runtime mid-execution).
    let (_run_out, run_err, run_ok) = run_silt("run", &main);
    assert!(
        !run_ok,
        "silt run must also fail for the ill-typed imported module"
    );
    assert!(
        run_err.contains("type mismatch") && run_err.contains("helper.silt"),
        "run must surface the same imported-module diagnostic; stderr={run_err}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// Negative control: a clean multi-file package must keep checking
/// green — the new harvest must not leak warnings or resolvable noise.
#[test]
fn clean_multi_file_package_still_checks_green() {
    let root = tempdir("clean");
    write_package(
        &root,
        "chkclean",
        &[
            ("helper.silt", "pub fn double(x: Int) -> Int = x * 2\n"),
            (
                "main.silt",
                "import helper\n\nfn main() {\n  println(helper.double(21))\n}\n",
            ),
        ],
    );
    let main = root.join("src/main.silt");

    let (stdout, stderr, ok) = run_silt("check", &main);
    assert!(
        ok,
        "silt check must pass on a clean multi-file package; stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stderr.trim().is_empty(),
        "no diagnostics expected for a clean package; stderr={stderr}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// Transitive-import suppression lock (round-91 parity): a local
/// module that imports a path-dep package produces, during its own
/// typecheck, the "unknown module" warning plus the import-resolvable
/// undefined-name cascade (the dep's exports are cached under its
/// `lib` module key, not the package name). Those shapes are resolved
/// by the compiler at link time and MUST stay suppressed — check is
/// green and the program runs.
#[test]
fn transitive_cross_package_import_shapes_stay_suppressed() {
    let ws = tempdir("transitive");
    let dep_root = ws.join("mathx");
    let app_root = ws.join("app");
    write_package(
        &dep_root,
        "mathx",
        &[("lib.silt", "pub fn triple(x: Int) -> Int = x * 3\n")],
    );
    std::fs::create_dir_all(app_root.join("src")).expect("create app src");
    std::fs::write(
        app_root.join("silt.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nmathx = { path = \"../mathx\" }\n",
    )
    .expect("write app silt.toml");
    std::fs::write(
        app_root.join("src/shapes.silt"),
        "import mathx\n\npub fn nine(x: Int) -> Int = mathx.triple(x) * 3\n",
    )
    .expect("write shapes.silt");
    std::fs::write(
        app_root.join("src/main.silt"),
        "import shapes\n\nfn main() {\n  println(shapes.nine(1))\n}\n",
    )
    .expect("write main.silt");
    let main = app_root.join("src/main.silt");

    let (stdout, stderr, ok) = run_silt("check", &main);
    assert!(
        ok,
        "silt check must stay green when a local module imports a path dep; stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        !stderr.contains("unknown module") && !stderr.contains("undefined"),
        "import-resolvable shapes must stay suppressed; stderr={stderr}"
    );

    let (run_out, run_err, run_ok) = run_silt("run", &main);
    assert!(run_ok, "silt run must succeed; stderr={run_err}");
    assert_eq!(
        run_out.lines().next().unwrap_or(""),
        "9",
        "transitive dep call must produce the right value"
    );

    let _ = std::fs::remove_dir_all(&ws);
}
