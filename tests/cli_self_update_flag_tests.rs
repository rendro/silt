//! Round-75 G4 — `silt self-update` flag-suggestion arms.
//!
//! `src/cli/self_update.rs:24-29` recognizes three common typos for the
//! supported flags and surfaces a `(did you mean --<flag>?)` hint:
//!
//! | typed             | suggestion        |
//! |-------------------|-------------------|
//! | `--dryrun` / `-n` | `--dry-run`       |
//! | `-f` / `--reinstall` | `--force`      |
//! | `--h` / `-help`   | `--help`          |
//!
//! Pre-round-75 the suggestion arms had no dedicated tests; the only
//! coverage was `tests/cli_project_root_tests.rs::test_self_update_rename_works`,
//! which exercises only the happy `--help` path. The sibling
//! flag-suggestion paths for `silt run` / `silt --version` ARE tested
//! (see `cli_help_and_unknown_subcommand_tests.rs` and
//! `round73_cli_help_gaps_tests.rs::silt_verison_typo_suggests_version_flag`),
//! so the absence of self-update coverage was a tooling gap.
//!
//! These tests exercise the binary via subprocess so the unknown-flag
//! match arm in `dispatch` actually runs. Crucially, the suggestion
//! path fires BEFORE any real update logic (the arm calls
//! `process::exit(1)`), so invoking `silt self-update --<typo>` is
//! safe on the test runner — no network, no replacement of the binary.

use std::process::Command;

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

#[test]
fn silt_self_update_dryrun_typo_suggests_dry_run() {
    // `--dryrun` (no hyphen) should surface "(did you mean --dry-run?)".
    // Without this lock, removing the `--dryrun` arm in self_update.rs:25
    // would silently drop the hint.
    let output = silt_cmd()
        .args(["self-update", "--dryrun"])
        .output()
        .expect("failed to run silt self-update --dryrun");
    assert!(
        !output.status.success(),
        "silt self-update --dryrun must exit non-zero, got {:?}",
        output.status
    );
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("did you mean --dry-run?"),
        "silt self-update --dryrun stderr must surface \
         '(did you mean --dry-run?)', got: {stderr}"
    );
    // The error must also name the offending token so the user sees
    // exactly what was typed back to them.
    assert!(
        stderr.contains("--dryrun"),
        "silt self-update --dryrun stderr must echo the offending \
         token, got: {stderr}"
    );
}

#[test]
fn silt_self_update_reinstall_typo_suggests_force() {
    // `--reinstall` should surface "(did you mean --force?)". Reverting
    // the `-f | --reinstall` arm at self_update.rs:26 would drop this.
    let output = silt_cmd()
        .args(["self-update", "--reinstall"])
        .output()
        .expect("failed to run silt self-update --reinstall");
    assert!(
        !output.status.success(),
        "silt self-update --reinstall must exit non-zero, got {:?}",
        output.status
    );
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("did you mean --force?"),
        "silt self-update --reinstall stderr must surface \
         '(did you mean --force?)', got: {stderr}"
    );
    assert!(
        stderr.contains("--reinstall"),
        "silt self-update --reinstall stderr must echo the offending \
         token, got: {stderr}"
    );
}

#[test]
fn silt_self_update_h_typo_suggests_help() {
    // `--h` (single-letter long form) should surface "(did you mean
    // --help?)". Reverting the `--h | -help` arm at self_update.rs:27
    // would drop this hint.
    let output = silt_cmd()
        .args(["self-update", "--h"])
        .output()
        .expect("failed to run silt self-update --h");
    assert!(
        !output.status.success(),
        "silt self-update --h must exit non-zero, got {:?}",
        output.status
    );
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("did you mean --help?"),
        "silt self-update --h stderr must surface \
         '(did you mean --help?)', got: {stderr}"
    );
    assert!(
        stderr.contains("--h"),
        "silt self-update --h stderr must echo the offending token, \
         got: {stderr}"
    );
}
