//! Round-85 follow-up: `format_module_source_error` inner-snippet color
//! symmetry. Round 83 added `NO_COLOR` / `FORCE_COLOR` support to
//! `SourceError::Display` via `active_colors()`. The module-import inner
//! snippet rendered by `format_module_source_error` was still plain text,
//! producing a colored outer header and a plain inner snippet under
//! `FORCE_COLOR=1`. The inner `-->` / `|` / `^` glyphs now go through
//! `active_colors()` too. These tests set `FORCE_COLOR`, which the golden
//! harness cannot; the plain `NO_COLOR` form is the golden case
//! `tests/golden/lang/modules/round85_followup_deferred_close__inner_snippet_plain_under_no_color`.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// Build a fixture: a main file that imports a broken module, and a
/// broken module containing a lex error. Returns the path to the main
/// file (callers `silt run` it). The temp dir is leaked deliberately
/// so the subprocess can read the files; tests run in serial and the
/// OS reaps them on test-process exit.
fn write_broken_import_fixture() -> PathBuf {
    let tmp = std::env::temp_dir().join(format!(
        "silt-round85-deferred-fixture-{}",
        std::process::id()
    ));
    fs::create_dir_all(&tmp).expect("create temp fixture dir");
    // An illegal character `@` triggers a lex error inside the
    // imported module. format_module_source_error renders the inner
    // snippet pointing at it.
    fs::write(
        tmp.join("badlex.silt"),
        "pub fn hi() = 1\n@@@\npub fn bye() = 2\n",
    )
    .expect("write badlex.silt");
    let main_src = "import badlex\n\nfn main() {\n  badlex.hi()\n}\n";
    let main_path = tmp.join("main.silt");
    fs::write(&main_path, main_src).expect("write main.silt");
    main_path
}

fn run_silt_capture_stderr(
    main: &PathBuf,
    force_color: Option<&str>,
    no_color: Option<&str>,
) -> String {
    let bin = env!("CARGO_BIN_EXE_silt");
    let mut cmd = Command::new(bin);
    cmd.arg("run").arg(main);
    // Always set TERM so the child has a deterministic env. We
    // explicitly toggle FORCE_COLOR / NO_COLOR per case.
    cmd.env_remove("FORCE_COLOR");
    cmd.env_remove("NO_COLOR");
    if let Some(v) = force_color {
        cmd.env("FORCE_COLOR", v);
    }
    if let Some(v) = no_color {
        cmd.env("NO_COLOR", v);
    }
    let output = cmd.output().expect("spawn silt");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn item2_inner_snippet_colored_under_force_color() {
    let main = write_broken_import_fixture();
    let stderr = run_silt_capture_stderr(&main, Some("1"), None);
    // Sanity: the broken module's parse error reached the user
    // through `format_module_source_error`.
    assert!(
        stderr.contains("badlex.silt") && stderr.contains("@@@"),
        "expected error stderr to reference badlex.silt and contain \
         the broken source line, got: {stderr:?}"
    );
    // Inner-snippet glyphs must be wrapped in ANSI escapes. We
    // grep for the specific cyan-wrapped `-->` and bold-red `^` —
    // exact-match keeps the lock pointed at the format we ship.
    assert!(
        stderr.contains("\x1b[36m-->\x1b[0m"),
        "FORCE_COLOR=1: inner snippet `-->` must be cyan-wrapped. \
         Got stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("\x1b[1m\x1b[31m^\x1b[0m"),
        "FORCE_COLOR=1: inner snippet `^` must be bold-red-wrapped. \
         Got stderr:\n{stderr}"
    );
    // The `|` gutter must also be cyan-wrapped (at least one bar).
    assert!(
        stderr.contains("\x1b[36m|\x1b[0m"),
        "FORCE_COLOR=1: inner snippet `|` gutter must be cyan-wrapped. \
         Got stderr:\n{stderr}"
    );
}

#[test]
fn item2_no_color_wins_over_force_color() {
    let main = write_broken_import_fixture();
    let stderr = run_silt_capture_stderr(&main, Some("1"), Some("1"));
    assert!(
        !stderr.contains('\x1b'),
        "NO_COLOR=1 + FORCE_COLOR=1: NO_COLOR wins (no-color.org spec). \
         No ANSI escapes expected. Got stderr:\n{stderr}"
    );
}
