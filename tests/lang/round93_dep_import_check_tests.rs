//! Round 93 BROKEN regression: importing a package dependency must NOT
//! silently disable static name/type checking in the importing file.
//!
//! Before the fix, `Compiler::pre_typecheck_imports` resolved a
//! `silt.toml` path dep's `src/lib.silt` and typechecked it, but cached
//! the exports under the dep's resolved module key (`"lib"`) instead of
//! the name the importer wrote (`"mathutil"`). The entrypoint typecheck
//! looked the import up by the written name, missed, and emitted the
//! "unknown module" warning — which tripped the file-wide
//! import-cascade suppression (`should_suppress_import_cascade`) and
//! swallowed EVERY undefined-variable / undefined-type / unknown-field
//! error in the file. And contrary to the old comment in
//! `src/cli/pipeline.rs`, the compiler does NOT backstop those names:
//! an undefined global only traps in the VM when the code path runs.
//! Net effect: `silt check` exited 0 on a file containing
//! `zzz_totally_undefined(1)`, `mathutil.nonexistent(1)`, and
//! `mathutil.double("not an int")` — and `silt run` exited 0 too when
//! the bad code sat in an uncalled function.
//!
//! The fix (`register_dep_import_exports` in `src/cli/pipeline.rs`)
//! typechecks each directly-imported declared dependency's `lib.silt`
//! and registers its exports under the import name before the
//! entrypoint typecheck, so the unknown-module warning never fires for
//! declared deps and the cascade filter stays dormant.
//!
//! Also locked here: the round-92 behaviors this fix must not regress —
//! dep-INTERNAL type errors surfacing through `silt check`, and
//! sibling-module import checking.

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
        "silt_round93_depimp_{label}_{}_{nanos}",
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

/// Lay out a two-package workspace:
///
///   <ws>/dep/silt.toml            name = "mathutil"
///   <ws>/dep/src/lib.silt         `dep_lib` source
///   <ws>/app/silt.toml            depends on mathutil via path
///   <ws>/app/src/main.silt        `app_main` source
///
/// Returns the path to the app's main.silt.
fn write_workspace(ws: &Path, dep_lib: &str, app_main: &str) -> PathBuf {
    let dep_root = ws.join("dep");
    let app_root = ws.join("app");
    std::fs::create_dir_all(dep_root.join("src")).expect("create dep src");
    std::fs::create_dir_all(app_root.join("src")).expect("create app src");
    std::fs::write(
        dep_root.join("silt.toml"),
        "[package]\nname = \"mathutil\"\nversion = \"0.1.0\"\n",
    )
    .expect("write dep silt.toml");
    std::fs::write(dep_root.join("src/lib.silt"), dep_lib).expect("write dep lib.silt");
    std::fs::write(
        app_root.join("silt.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nmathutil = { path = \"../dep\" }\n",
    )
    .expect("write app silt.toml");
    std::fs::write(app_root.join("src/main.silt"), app_main).expect("write app main.silt");
    app_root.join("src/main.silt")
}

const DEP_LIB_OK: &str = "pub fn double(x: Int) -> Int = x * 2\n";

/// Repro #1 from the round-93 finding: an importer with a path dep AND
/// a plainly undefined variable (in an uncalled fn, so the VM never
/// traps either). Pre-fix: `silt check` exited 0. Post-fix: nonzero,
/// naming the undefined variable.
#[test]
fn dep_import_does_not_suppress_unrelated_undefined_variable() {
    let ws = tempdir("undefvar");
    let main = write_workspace(
        &ws,
        DEP_LIB_OK,
        "import mathutil\n\n\
         fn unused() {\n  zzz_totally_undefined(1)\n}\n\n\
         fn main() {\n  println(mathutil.double(21))\n}\n",
    );

    let (stdout, stderr, ok) = run_silt("check", &main);
    assert!(
        !ok,
        "silt check must exit nonzero when the file contains an undefined \
         variable, even though it also imports a path dep; \
         stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stderr.contains("zzz_totally_undefined"),
        "the diagnostic must name the undefined variable; stderr={stderr}"
    );

    let _ = std::fs::remove_dir_all(&ws);
}

/// Repro #2: calling a member the dep does not export must fail check.
/// Pre-fix the dep's exports were invisible to the typechecker, so
/// `mathutil.nonexistent(1)` sailed through as part of the suppressed
/// cascade.
#[test]
fn dep_nonexistent_member_fails_check() {
    let ws = tempdir("nonexistent");
    let main = write_workspace(
        &ws,
        DEP_LIB_OK,
        "import mathutil\n\nfn main() {\n  mathutil.nonexistent(1)\n}\n",
    );

    let (stdout, stderr, ok) = run_silt("check", &main);
    assert!(
        !ok,
        "silt check must exit nonzero for a call to a member the dep does \
         not export; stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stderr.contains("nonexistent") && stderr.contains("mathutil"),
        "the diagnostic must name the missing member and the module; stderr={stderr}"
    );

    let _ = std::fs::remove_dir_all(&ws);
}

/// Repro #3 (sibling of #2): a wrong-argument-type call against a dep
/// export must be caught statically now that the export signatures are
/// visible to the typechecker.
#[test]
fn dep_export_wrong_arg_type_fails_check() {
    let ws = tempdir("wrongarg");
    let main = write_workspace(
        &ws,
        DEP_LIB_OK,
        "import mathutil\n\nfn main() {\n  println(mathutil.double(\"not an int\"))\n}\n",
    );

    let (stdout, stderr, ok) = run_silt("check", &main);
    assert!(
        !ok,
        "silt check must exit nonzero for a wrong-arg-type call against a \
         dep export; stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stderr.contains("type mismatch"),
        "expected a type mismatch diagnostic; stderr={stderr}"
    );

    let _ = std::fs::remove_dir_all(&ws);
}

/// Positive control: valid dep usage must keep checking green (with no
/// diagnostic noise — no leaked "unknown module" warning) and run
/// end-to-end.
#[test]
fn valid_dep_usage_checks_green_and_runs() {
    let ws = tempdir("valid");
    let main = write_workspace(
        &ws,
        DEP_LIB_OK,
        "import mathutil\n\nfn main() {\n  println(mathutil.double(21))\n}\n",
    );

    let (stdout, stderr, ok) = run_silt("check", &main);
    assert!(
        ok,
        "silt check must pass for valid dep usage; stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stderr.trim().is_empty(),
        "no diagnostics expected for valid dep usage; stderr={stderr}"
    );

    let (run_out, run_err, run_ok) = run_silt("run", &main);
    assert!(run_ok, "silt run must succeed; stderr={run_err}");
    assert_eq!(
        run_out.lines().next().unwrap_or(""),
        "42",
        "dep call must produce the right value; stdout={run_out:?}"
    );

    let _ = std::fs::remove_dir_all(&ws);
}

/// Round-92 retention: a type error INSIDE the dep's lib.silt must
/// still fail `silt check` of the importer, attributed to the dep's
/// file. The round-93 fix discards diagnostics from its extra
/// export-collection typecheck precisely so this path (the compiler
/// pre-pass harvest) stays the single source of dep-internal errors —
/// this test locks both that they surface and that the harvest still
/// owns them.
#[test]
fn dep_internal_type_error_still_fails_check() {
    let ws = tempdir("depinternal");
    let main = write_workspace(
        &ws,
        "pub fn double(x: Int) -> Int = x + \"s\"\n",
        "import mathutil\n\nfn main() {\n  println(mathutil.double(21))\n}\n",
    );

    let (stdout, stderr, ok) = run_silt("check", &main);
    assert!(
        !ok,
        "silt check must exit nonzero when the dep itself is ill-typed; \
         stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stderr.contains("type mismatch"),
        "expected the dep's type mismatch to surface; stderr={stderr}"
    );
    assert!(
        stderr.contains("lib.silt"),
        "the diagnostic must carry the dep's file path; stderr={stderr}"
    );

    let _ = std::fs::remove_dir_all(&ws);
}

/// Round-92 retention: same-package sibling-module imports must keep
/// their static checking — an undefined variable in a file importing a
/// sibling module still fails check.
#[test]
fn sibling_module_import_still_catches_undefined_variable() {
    let root = tempdir("sibling");
    std::fs::create_dir_all(root.join("src")).expect("create src dir");
    std::fs::write(
        root.join("silt.toml"),
        "[package]\nname = \"sibpkg\"\nversion = \"0.1.0\"\n",
    )
    .expect("write silt.toml");
    std::fs::write(
        root.join("src/helper.silt"),
        "pub fn double(x: Int) -> Int = x * 2\n",
    )
    .expect("write helper.silt");
    std::fs::write(
        root.join("src/main.silt"),
        "import helper\n\n\
         fn unused() {\n  zzz_sibling_undefined(1)\n}\n\n\
         fn main() {\n  println(helper.double(21))\n}\n",
    )
    .expect("write main.silt");
    let main = root.join("src/main.silt");

    let (stdout, stderr, ok) = run_silt("check", &main);
    assert!(
        !ok,
        "silt check must exit nonzero for an undefined variable in a file \
         importing a sibling module; stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stderr.contains("zzz_sibling_undefined"),
        "the diagnostic must name the undefined variable; stderr={stderr}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// Suppression retention: a genuinely unknown module (typo, not a
/// declared dep, no sibling file) must still produce ONE clear
/// module-level error — not a cascade of undefined-name errors for
/// every use of the module, and check must still exit nonzero.
#[test]
fn genuinely_unknown_module_still_one_clear_error() {
    let root = tempdir("typomod");
    std::fs::create_dir_all(root.join("src")).expect("create src dir");
    std::fs::write(
        root.join("silt.toml"),
        "[package]\nname = \"typopkg\"\nversion = \"0.1.0\"\n",
    )
    .expect("write silt.toml");
    std::fs::write(
        root.join("src/main.silt"),
        "import nosuchmod\n\nfn main() {\n  nosuchmod.foo(1)\n  nosuchmod.bar(2)\n}\n",
    )
    .expect("write main.silt");
    let main = root.join("src/main.silt");

    let (stdout, stderr, ok) = run_silt("check", &main);
    assert!(
        !ok,
        "silt check must exit nonzero for an unknown module; \
         stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stderr.contains("cannot load module 'nosuchmod'"),
        "expected the single module-level error; stderr={stderr}"
    );
    assert!(
        !stderr.contains("undefined variable"),
        "the undefined-name cascade must stay suppressed for genuinely \
         unknown modules; stderr={stderr}"
    );

    let _ = std::fs::remove_dir_all(&root);
}
