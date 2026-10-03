//! Round 86 — B1: `silt check --format json` must never emit ANSI
//! escapes regardless of upstream color signals. A diagnostic holds no
//! rendered text, and the JSON renderer writes none, so `FORCE_COLOR=1`
//! (which the golden harness cannot set) must not reach the output. The
//! fixture is a module import with a lex error. The `NO_COLOR=1` half is
//! the golden case `tests/golden/lang/diagnostics/round86_check_json_no_ansi__no_color`.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// Build a fixture: a `main.silt` that imports a `badlex` module whose
/// source contains an illegal character (`@`) that triggers a lex
/// error. Returns the path to `main.silt`; the temp dir is
/// intentionally leaked so the subprocess can read the files (the OS
/// reaps on test-process exit).
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
        "pub fn hi() { 1 }\n@@@\npub fn bye() { 2 }\n",
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
    // Sanity: the JSON shape is what we expect — `notes` and `help`
    // arrays are present and the diagnostic is in the broken module.
    assert!(
        stdout.contains("\"notes\"") && stdout.contains("\"help\""),
        "expected JSON to contain `notes` and `help` fields; got stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("badlex.silt"),
        "expected JSON to reference `badlex.silt` (the broken import); \
         got stdout:\n{stdout}"
    );
    // The actual lock: zero ANSI escape sequences anywhere in stdout,
    // regardless of FORCE_COLOR=1. The B1 bug was that colored snippet
    // text baked into a message leaked into the JSON strings.
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
