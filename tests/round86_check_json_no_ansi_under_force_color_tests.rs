//! Round 86 — B1: `silt check --format json` must never emit ANSI
//! escapes regardless of upstream color signals.
//!
//! `format_module_source_error` (`src/compiler/mod.rs:218-239`) routes
//! its inner snippet glyphs through `crate::errors::active_colors()`,
//! which honors `FORCE_COLOR=1` (or a real TTY). Those ANSI codes get
//! baked into the resulting message string and previously leaked into
//! the `hints` array of `silt check --format json` output, corrupting
//! the machine-readable contract that editor plugins / LSP front-ends
//! / CI scripts depend on.
//!
//! Fix: strip ANSI at the JSON boundary in `print_json_errors`
//! (`src/cli/check.rs`). These locks run `silt check --format json`
//! against a module-import-with-lex-error fixture under both
//! `FORCE_COLOR=1` and `NO_COLOR=1` and assert the stdout contains
//! zero `\x1b` bytes in either case — symmetric coverage so a future
//! refactor can't restore the leak under only one signal.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// Build a fixture: a `main.silt` that imports a `badlex` module whose
/// source contains an illegal character (`@`) that triggers a lex
/// error inside `format_module_source_error`. Returns the path to
/// `main.silt`; the temp dir is intentionally leaked so the subprocess
/// can read the files (the OS reaps on test-process exit, same shape
/// as `tests/round85_followup_deferred_close_tests.rs`).
fn write_broken_import_fixture() -> PathBuf {
    let tmp = std::env::temp_dir().join(format!(
        "silt-round86-b1-fixture-{}-{}",
        std::process::id(),
        // Add a per-test nonce so parallel test cases in this file
        // don't share a fixture dir and stomp each other if the test
        // harness ever decides to fan them out.
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    ));
    fs::create_dir_all(&tmp).expect("create temp fixture dir");
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

/// Spawn `silt check --format json <main>` with the requested env
/// toggles and return `(exit_code, stdout, stderr)`.
fn run_silt_check_json(
    main: &PathBuf,
    force_color: Option<&str>,
    no_color: Option<&str>,
) -> (i32, String, String) {
    let bin = env!("CARGO_BIN_EXE_silt");
    let mut cmd = Command::new(bin);
    cmd.arg("check").arg("--format").arg("json").arg(main);
    cmd.env_remove("FORCE_COLOR");
    cmd.env_remove("NO_COLOR");
    if let Some(v) = force_color {
        cmd.env("FORCE_COLOR", v);
    }
    if let Some(v) = no_color {
        cmd.env("NO_COLOR", v);
    }
    let output = cmd.output().expect("spawn silt check");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn json_output_has_no_ansi_under_force_color() {
    let main = write_broken_import_fixture();
    let (code, stdout, stderr) = run_silt_check_json(&main, Some("1"), None);
    // Exit 1: errors detected.
    assert_eq!(
        code, 1,
        "silt check should exit 1 on lex error in imported module; \
         got code={code}\nstdout={stdout}\nstderr={stderr}"
    );
    // Sanity: the JSON shape is what we expect — a `hints` array is
    // present and references the broken module's filename.
    assert!(
        stdout.contains("\"hints\""),
        "expected JSON to contain a `hints` field; got stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("badlex.silt"),
        "expected JSON to reference `badlex.silt` (the broken import); \
         got stdout:\n{stdout}"
    );
    // The actual lock: zero ANSI escape sequences anywhere in stdout,
    // regardless of FORCE_COLOR=1. The B1 bug was that the cyan
    // `\x1b[36m-->\x1b[0m` and bold-red `\x1b[1m\x1b[31m^\x1b[0m`
    // sequences from `format_module_source_error` leaked into the
    // `hints` strings here.
    //
    // Two flavors to check:
    //   - Raw `\x1b` bytes (if serde ever changes to keep them raw).
    //   - `` JSON-escape form (the current serde encoding for
    //     control bytes inside string values — this is what actually
    //     reproduces the bug in the wild before the fix).
    assert!(
        !stdout.contains('\x1b'),
        "FORCE_COLOR=1: `silt check --format json` stdout must contain \
         no raw ANSI escape bytes. Got stdout:\n{stdout}"
    );
    assert!(
        !stdout.contains("\\u001b") && !stdout.contains("\\u001B"),
        "FORCE_COLOR=1: `silt check --format json` stdout must contain \
         no `\\u001b` JSON-encoded ANSI escape sequences. Got stdout:\n{stdout}"
    );
}

#[test]
fn json_output_has_no_ansi_under_no_color() {
    // Symmetric control: `NO_COLOR=1` already disables color in the
    // upstream renderer, so this case should pass even without the
    // boundary strip — but we lock it too so a regression that
    // accidentally re-enables color under NO_COLOR still fails here.
    let main = write_broken_import_fixture();
    let (code, stdout, stderr) = run_silt_check_json(&main, None, Some("1"));
    assert_eq!(
        code, 1,
        "silt check should exit 1 on lex error in imported module; \
         got code={code}\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stdout.contains("\"hints\""),
        "expected JSON to contain a `hints` field; got stdout:\n{stdout}"
    );
    assert!(
        !stdout.contains('\x1b'),
        "NO_COLOR=1: `silt check --format json` stdout must contain no \
         raw ANSI escape bytes. Got stdout:\n{stdout}"
    );
    assert!(
        !stdout.contains("\\u001b") && !stdout.contains("\\u001B"),
        "NO_COLOR=1: `silt check --format json` stdout must contain no \
         `\\u001b` JSON-encoded ANSI escape sequences. Got stdout:\n{stdout}"
    );
}
