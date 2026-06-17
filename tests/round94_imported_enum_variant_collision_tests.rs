//! Round 94 BROKEN regression: two imported libraries that share ANY
//! variant name must remain co-importable.
//!
//! Before the fix, the importer's `synthesize_auto_derive_impls` walked
//! ALL of `self.enums` (filtering only by `user_decl_type_names`) and
//! RE-synthesized Equal/Compare/Hash impls for IMPORTED enums too. The
//! synthesized constructor patterns use bare variant names, and the
//! `variant_to_enum` map is first-import-wins, so when two imported
//! enums shared a variant name (e.g. `Red` in both `ColorA` and
//! `ColorB`), the loser's synthesized body resolved its own `Red` to the
//! WINNER's enum — unifying `ColorB` against `ColorA` and emitting a
//! spurious `type mismatch: expected ColorA, got ColorB` per
//! auto-derived method, all with synthetic zero spans, before the user
//! wrote any code using either type.
//!
//! The fix: only synthesize auto-derive impls for types this package
//! OWNS (built-ins + the current package's own decls). Imported enums'
//! auto-derive impls were already synthesized + body-checked in their
//! producing package and re-imported via `trait_impl_entries`, so
//! `==` / `<` / `.hash()` on them still resolve — they're just not
//! re-type-checked here against the bare-name map.
//!
//! These tests exercise the REAL pipeline (the `silt` binary), not a
//! typecheck-only stub, because this was a compile-pipeline bug.

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
        "silt_round94_varcol_{label}_{}_{nanos}",
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

/// Lay out a three-package workspace:
///
///   <ws>/sa/silt.toml + src/lib.silt   (exports enum ColorA + mka())
///   <ws>/sb/silt.toml + src/lib.silt   (exports enum ColorB + mkb())
///   <ws>/app/silt.toml + src/main.silt (imports both sa and sb)
///
/// The two libs are given a shared variant name so the collision path is
/// exercised. Returns the path to the app's main.silt.
fn write_collision_workspace(ws: &Path, app_main: &str) -> PathBuf {
    let sa = ws.join("sa");
    let sb = ws.join("sb");
    let app = ws.join("app");
    std::fs::create_dir_all(sa.join("src")).expect("create sa src");
    std::fs::create_dir_all(sb.join("src")).expect("create sb src");
    std::fs::create_dir_all(app.join("src")).expect("create app src");

    std::fs::write(
        sa.join("silt.toml"),
        "[package]\nname = \"sa\"\nversion = \"0.1.0\"\n",
    )
    .expect("write sa silt.toml");
    // `Red` is shared with sb below — the collision trigger.
    std::fs::write(
        sa.join("src/lib.silt"),
        "pub type ColorA { Red, Blue }\npub fn mka() -> ColorA = Red\n",
    )
    .expect("write sa lib.silt");

    std::fs::write(
        sb.join("silt.toml"),
        "[package]\nname = \"sb\"\nversion = \"0.1.0\"\n",
    )
    .expect("write sb silt.toml");
    std::fs::write(
        sb.join("src/lib.silt"),
        "pub type ColorB { Red, Green }\npub fn mkb() -> ColorB = Green\n",
    )
    .expect("write sb lib.silt");

    std::fs::write(
        app.join("silt.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nsa = { path = \"../sa\" }\nsb = { path = \"../sb\" }\n",
    )
    .expect("write app silt.toml");
    std::fs::write(app.join("src/main.silt"), app_main).expect("write app main.silt");
    app.join("src/main.silt")
}

/// Core repro: importing two libs that share a variant name must
/// `silt check` clean — no spurious `type mismatch` from re-synthesized
/// auto-derive impls. Pre-fix this printed 6x
/// `type mismatch: expected ColorA, got ColorB` and exited nonzero.
#[test]
fn shared_variant_name_across_imports_checks_clean() {
    let ws = tempdir("check");
    let main = write_collision_workspace(
        &ws,
        "import sa\nimport sb\n\nfn main() {\n  println(\"hi\")\n}\n",
    );

    let (stdout, stderr, ok) = run_silt("check", &main);
    assert!(
        ok,
        "silt check must pass when two imported libs share a variant name; \
         stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        !stderr.contains("type mismatch"),
        "no spurious type-mismatch diagnostic expected; stderr={stderr}"
    );
    assert!(
        !stderr.contains("ColorA") && !stderr.contains("ColorB"),
        "the auto-derive collision must not leak either enum name into a \
         diagnostic; stderr={stderr}"
    );

    let _ = std::fs::remove_dir_all(&ws);
}

/// The same collision workspace must also `silt run` clean, proving the
/// fix doesn't merely silence a typecheck error while leaving the
/// compile/run pipeline broken.
#[test]
fn shared_variant_name_across_imports_runs_clean() {
    let ws = tempdir("run");
    let main = write_collision_workspace(
        &ws,
        "import sa\nimport sb\n\nfn main() {\n  println(\"ok\")\n}\n",
    );

    let (stdout, stderr, ok) = run_silt("run", &main);
    assert!(
        ok,
        "silt run must succeed for the collision workspace; \
         stdout={stdout:?} stderr={stderr:?}"
    );
    assert_eq!(
        stdout.lines().next().unwrap_or(""),
        "ok",
        "program must actually run; stdout={stdout:?} stderr={stderr:?}"
    );

    let _ = std::fs::remove_dir_all(&ws);
}

/// Load-bearing: the fix must NOT have disabled derive support for
/// imported enums. `==` on an imported enum value (whose variant name
/// collides with the other import's) must still return `true` at
/// runtime. If the fix had simply stopped registering the imported
/// enum's Equal impl, this would error or print `false`.
#[test]
fn equality_on_imported_enum_still_works_at_runtime() {
    let ws = tempdir("eq");
    let main = write_collision_workspace(
        &ws,
        "import sa\nimport sb\n\n\
         fn main() {\n  \
         let x = sa.mka()\n  \
         let y = sb.mkb()\n  \
         println(x == sa.mka())\n  \
         println(y == sb.mkb())\n  \
         println(x.hash() == sa.mka().hash())\n\
         }\n",
    );

    let (stdout, stderr, ok) = run_silt("run", &main);
    assert!(
        ok,
        "silt run must succeed when comparing imported enum values; \
         stdout={stdout:?} stderr={stderr:?}"
    );
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.first().copied(),
        Some("true"),
        "== on an imported enum value must return true; stdout={stdout:?}"
    );
    assert_eq!(
        lines.get(1).copied(),
        Some("true"),
        "== on the second imported enum value must return true; stdout={stdout:?}"
    );
    assert_eq!(
        lines.get(2).copied(),
        Some("true"),
        ".hash() on an imported enum must stay consistent for equal values; \
         stdout={stdout:?}"
    );

    let _ = std::fs::remove_dir_all(&ws);
}
