//! Phase D locks for the `--strict-effects` CLI flag plumbing.
//!
//! See `docs/proposals/effect-rows.md` (Part 7 Phase D) and
//! `docs/strict-effects-migration.md`. The flag is wired into
//! `silt check`, `silt run`, and `silt test`. A `[lints]
//! strict-effects = true` field in `silt.toml` opts the whole
//! package in without per-invocation flags. CLI flag wins over
//! manifest. All combinations are locked here.

use std::process::Command;

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

/// Make a tempdir specific to this test name. We avoid `tempfile` to
/// keep the dependency footprint zero — every other CLI test in this
/// suite already uses `std::env::temp_dir()` + a unique suffix.
fn make_tempdir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("silt_strict_effects_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ── 1. flag_parses_for_check ───────────────────────────────────────

#[test]
fn flag_parses_for_check() {
    // `silt check --strict-effects file.silt` must be a recognised
    // flag. Unknown flags emit "unknown flag '...'" on stderr; if the
    // flag were unrecognised we'd see that message. We DO expect a
    // strict-mode diagnostic on stderr because `read` calls
    // `io.read_file` without an annotation.
    let dir = make_tempdir("check_parse");
    let main = dir.join("main.silt");
    std::fs::write(
        &main,
        "import io\nfn read(p: String) -> Result(String, IoError) = io.read_file(p)\nfn main() = ()\n",
    )
    .unwrap();

    let output = silt_cmd()
        .args(["check", "--strict-effects", main.to_str().unwrap()])
        .output()
        .expect("failed to run silt check");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unknown flag"),
        "--strict-effects must be recognised by `silt check`, got stderr: {stderr}"
    );
    // Strict mode should fire — non-zero exit + diagnostic mentions
    // the strict-effects flag in the headline.
    assert!(
        !output.status.success(),
        "strict-effects must reject this fn under --strict-effects, got success"
    );
    assert!(
        stderr.contains("--strict-effects"),
        "expected strict-mode diagnostic mentioning --strict-effects, got: {stderr}"
    );
}

// ── 2. flag_parses_for_run ─────────────────────────────────────────

#[test]
fn flag_parses_for_run() {
    let dir = make_tempdir("run_parse");
    let main = dir.join("main.silt");
    std::fs::write(
        &main,
        "import io\nfn read(p: String) -> Result(String, IoError) = io.read_file(p)\nfn main() = ()\n",
    )
    .unwrap();

    let output = silt_cmd()
        .args(["run", "--strict-effects", main.to_str().unwrap()])
        .output()
        .expect("failed to run silt run");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unknown flag"),
        "--strict-effects must be recognised by `silt run`, got stderr: {stderr}"
    );
    assert!(
        !output.status.success(),
        "strict-effects must reject this fn under --strict-effects, got success"
    );
}

// ── 3. flag_parses_for_test ────────────────────────────────────────

#[test]
fn flag_parses_for_test() {
    // `silt test --strict-effects` runs over auto-discovered test
    // files. We construct a single _test.silt that imports io and
    // writes an unannotated fn so strict mode rejects it.
    let dir = make_tempdir("test_parse");
    let test_file = dir.join("strict_test.silt");
    std::fs::write(
        &test_file,
        "import io\nfn read(p: String) -> Result(String, IoError) = io.read_file(p)\nfn test_x() = ()\n",
    )
    .unwrap();

    let output = silt_cmd()
        .current_dir(&dir)
        .args(["test", "--strict-effects"])
        .output()
        .expect("failed to run silt test");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unknown flag"),
        "--strict-effects must be recognised by `silt test`, got stderr: {stderr}"
    );
    // The test command compiles per-file; under strict mode the fn
    // body's effects exceed the (flipped) declared bound, so the file
    // fails to compile. Lock the strict-mode diagnostic surface.
    assert!(
        stderr.contains("--strict-effects"),
        "expected strict-mode diagnostic in `silt test` output: {stderr}"
    );
}

// ── 4. manifest_strict_effects_field_recognised ────────────────────

#[test]
fn manifest_strict_effects_field_recognised() {
    // A package whose `silt.toml` has `[lints] strict-effects = true`
    // must opt every `silt check` invocation in WITHOUT the CLI flag.
    let dir = make_tempdir("manifest_field");
    std::fs::write(
        dir.join("silt.toml"),
        "[package]\nname = \"strict_pkg\"\nversion = \"0.1.0\"\n\n[lints]\nstrict-effects = true\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src/main.silt"),
        "import io\nfn read(p: String) -> Result(String, IoError) = io.read_file(p)\nfn main() = ()\n",
    )
    .unwrap();

    // No `--strict-effects` on the CLI — manifest provides it.
    let output = silt_cmd()
        .current_dir(&dir)
        .args(["check"])
        .output()
        .expect("failed to run silt check");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "manifest `[lints] strict-effects = true` must enable strict mode, but check passed: stderr={stderr}"
    );
    assert!(
        stderr.contains("--strict-effects"),
        "manifest-driven strict mode must produce the same diagnostic shape: {stderr}"
    );
}

// ── 5. cli_flag_overrides_manifest_off_to_on ───────────────────────

#[test]
fn cli_flag_overrides_manifest_off_to_on() {
    // Manifest does NOT enable strict mode (or has no [lints] table
    // at all). The CLI flag still flips strict mode on.
    let dir = make_tempdir("cli_override");
    std::fs::write(
        dir.join("silt.toml"),
        "[package]\nname = \"plain_pkg\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src/main.silt"),
        "import io\nfn read(p: String) -> Result(String, IoError) = io.read_file(p)\nfn main() = ()\n",
    )
    .unwrap();

    // Without flag: passes (manifest defaults strict-effects to off).
    let baseline = silt_cmd()
        .current_dir(&dir)
        .args(["check"])
        .output()
        .expect("failed to run silt check baseline");
    assert!(
        baseline.status.success(),
        "baseline `silt check` (no flag, no manifest opt-in) must pass, got stderr: {}",
        String::from_utf8_lossy(&baseline.stderr)
    );

    // With flag: errors.
    let with_flag = silt_cmd()
        .current_dir(&dir)
        .args(["check", "--strict-effects"])
        .output()
        .expect("failed to run silt check --strict-effects");
    let stderr = String::from_utf8_lossy(&with_flag.stderr);
    assert!(
        !with_flag.status.success(),
        "CLI flag must override absent manifest opt-in, but check passed: stderr={stderr}"
    );
    assert!(
        stderr.contains("--strict-effects"),
        "expected strict-mode diagnostic when CLI flag forces strict mode: {stderr}"
    );
}
