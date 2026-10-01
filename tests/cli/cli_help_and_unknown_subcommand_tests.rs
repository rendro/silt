//! Round-62 audit regressions for `silt` CLI ergonomics.
//!
//! **L8**: invoking `silt rn examples/hello.silt` produces only
//! "Unknown command: rn" with no hint that the user probably meant
//! `run`. Wire up a Levenshtein-distance suggestion so close typos
//! surface a "Did you mean" line, but leave wildly unrelated typos
//! (`silt zzzzzz`) alone — a wrong suggestion is worse than no
//! suggestion.

use std::process::Command;

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
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
