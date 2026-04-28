//! Round-62 audit regressions for `silt` CLI ergonomics.
//!
//! Two unrelated gaps land in the same file because they both poke the
//! top-level dispatcher's user-facing strings:
//!
//! - **G5**: `silt run --help` was missing the `--strict-effects` flag
//!   even though `silt check --help` and `silt test --help` both list
//!   it and the flag is parsed by `silt run` itself
//!   (`src/cli/run.rs`). A user reading `run --help` would conclude the
//!   flag isn't supported there, when in fact it is. Lock the flag's
//!   presence in the run-help text and verify the existing `check` /
//!   `test` help paths still advertise it (regression guard).
//!
//! - **L8**: invoking `silt rn examples/hello.silt` produces only
//!   "Unknown command: rn" with no hint that the user probably meant
//!   `run`. Wire up a Levenshtein-distance suggestion so close typos
//!   surface a "Did you mean" line, but leave wildly unrelated typos
//!   (`silt zzzzzz`) alone — a wrong suggestion is worse than no
//!   suggestion.

use std::process::Command;

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

// ── G5: run --help lists --strict-effects ──────────────────────────

#[test]
fn silt_run_help_lists_strict_effects_flag() {
    // The flag is parsed at src/cli/run.rs:30 — its absence from the
    // help text was a documentation drift, not a missing feature.
    let output = silt_cmd()
        .args(["run", "--help"])
        .output()
        .expect("failed to run silt run --help");
    assert!(
        output.status.success(),
        "silt run --help must exit 0, got {:?}",
        output.status
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--strict-effects"),
        "silt run --help must list --strict-effects flag, got: {stdout}"
    );
    // Match the wording the other subcommands use for consistency.
    assert!(
        stdout.contains("Treat unannotated fns as pure (Phase D)"),
        "silt run --help must use the same phrasing as check/test, got: {stdout}"
    );
}

#[test]
fn silt_check_help_still_lists_strict_effects_flag() {
    // Regression guard: the `check` subcommand has long advertised
    // `--strict-effects` in its --help output. Lock that.
    let output = silt_cmd()
        .args(["check", "--help"])
        .output()
        .expect("failed to run silt check --help");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--strict-effects"),
        "silt check --help must list --strict-effects flag, got: {stdout}"
    );
}

#[test]
fn silt_test_help_still_lists_strict_effects_flag() {
    // Regression guard: ditto for `silt test --help`.
    let output = silt_cmd()
        .args(["test", "--help"])
        .output()
        .expect("failed to run silt test --help");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--strict-effects"),
        "silt test --help must list --strict-effects flag, got: {stdout}"
    );
}

// ── Source-grep lock (backup if the binary ever fails to build) ────

#[test]
fn run_help_text_source_mentions_strict_effects() {
    // Cheap, build-independent lock: the source of `run_help_text`
    // mentions the flag. Catches the regression even in workspaces
    // that don't have the silt binary built yet.
    let src = std::fs::read_to_string("src/cli/help.rs")
        .expect("src/cli/help.rs must be readable from the test cwd");
    // Find the run_help_text fn body.
    let idx = src
        .find("fn run_help_text()")
        .expect("run_help_text fn must exist in src/cli/help.rs");
    let body = &src[idx..];
    // Stop at the next top-level fn or end-of-file so we don't
    // accidentally match the (already-existing) mention in the
    // `check_usage_banner` doc comment.
    let end = body[1..].find("\npub(crate) fn ").map(|i| i + 1).unwrap_or(body.len());
    let body = &body[..end];
    assert!(
        body.contains("strict-effects"),
        "run_help_text body must mention strict-effects, got: {body}"
    );
}

// ── L8: unknown-subcommand "did you mean" hint ─────────────────────

#[test]
fn silt_unknown_subcommand_suggests_close_match() {
    // `rn` is one transposition / deletion away from `run`. Edit
    // distance 1 — well within the threshold of 2.
    let output = silt_cmd()
        .args(["rn", "examples/hello.silt"])
        .output()
        .expect("failed to run silt rn ...");
    assert!(
        !output.status.success(),
        "silt rn must exit non-zero (unknown subcommand)"
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "silt rn must exit 1, got {:?}",
        output.status.code()
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Did you mean 'run'"),
        "silt rn stderr must suggest 'run', got: {stderr}"
    );
    // The original "Unknown command:" line should still be there.
    assert!(
        stderr.contains("Unknown command:"),
        "silt rn must still print 'Unknown command:' line, got: {stderr}"
    );
}

#[test]
fn silt_unknown_subcommand_suggests_check_for_chek() {
    // Edit distance 1 (`chek` -> `check`, one insertion). Locks the
    // suggestion machinery against multiple subcommands, not just
    // `run`.
    let output = silt_cmd()
        .args(["chek"])
        .output()
        .expect("failed to run silt chek");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Did you mean 'check'"),
        "silt chek must suggest 'check', got: {stderr}"
    );
}

#[test]
fn silt_unknown_subcommand_no_suggestion_for_far_typo() {
    // `zzzzzz` is more than edit-distance 2 from every valid
    // subcommand — no suggestion line should fire.
    let output = silt_cmd()
        .args(["zzzzzz"])
        .output()
        .expect("failed to run silt zzzzzz");
    assert!(
        !output.status.success(),
        "silt zzzzzz must exit non-zero (unknown subcommand)"
    );
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("Did you mean"),
        "silt zzzzzz must NOT suggest a candidate (none within edit-distance 2), got: {stderr}"
    );
    // Still tells the user how to discover the actual subcommand list.
    assert!(
        stderr.contains("Run 'silt' with no arguments"),
        "silt zzzzzz must still point user at the discovery hint, got: {stderr}"
    );
}

#[test]
fn silt_unknown_subcommand_quotes_the_typo() {
    // The output shape specified by round-62 wraps the unknown name in
    // single quotes (`Unknown command: 'rn'`) so it's unambiguous what
    // the user typed even if it contains spaces or punctuation.
    let output = silt_cmd()
        .args(["zzzzzz"])
        .output()
        .expect("failed to run silt zzzzzz");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Unknown command: 'zzzzzz'"),
        "stderr must quote the unknown command, got: {stderr}"
    );
}
