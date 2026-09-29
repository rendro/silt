//! Round-36 audit regression: `silt check --format json` must emit the
//! did-you-mean / `= help:` hint lines in a dedicated `hints` array, not
//! silently drop them.
//!
//! Before the fix, `print_json_errors` in `src/cli/check.rs` kept only
//! `e.message.lines().next()` and dropped every continuation line that
//! the human renderer shows as `= help: ...` / `= note: ...`. That meant
//! machine consumers (editors, LSP front-ends, CI scripts) that used
//! `--format json` never saw the typechecker's did-you-mean hints —
//! e.g. `prntln` → `println` — even though the human format surfaced
//! them just fine.
//!
//! The fix:
//! - `"message"` stays the first line (backward compat).
//! - A new `"hints"` array carries each remaining body line whose
//!   trimmed prefix is `help:` or `note:`.
//!
//! This test invokes the built binary with a `.silt` file that triggers
//! the suggestion (typo `prntln` → `println`), parses the stdout JSON,
//! and asserts a non-empty `hints` array containing "did you mean" AND
//! the exact suggestion target `println`.

use std::process::Command;

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

fn temp_silt_file(tag: &str, contents: &str) -> std::path::PathBuf {
    // Unique path per test run to avoid cross-test collisions.
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("silt_json_hint_r36_{tag}_{n}.silt"));
    std::fs::write(&path, contents).expect("failed to write temp silt file");
    path
}

/// A typo'd call — `prntln` is one edit away from the stdlib's `println`,
/// so the typechecker should append a `\nhelp: did you mean \`println\`?`
/// line to the undefined-variable error. `silt check --format json` must
/// surface that hint via the `hints` array.
#[test]
fn test_silt_check_json_emits_did_you_mean_hint_round36() {
    let path = temp_silt_file("did_you_mean", r#"fn main() { prntln("hi") }"#);

    let output = silt_cmd()
        .arg("check")
        .arg("--format")
        .arg("json")
        .arg(&path)
        .output()
        .expect("failed to run silt check --format json");

    // Typo is a hard error; expect non-zero exit.
    assert!(
        !output.status.success(),
        "expected non-zero exit for undefined variable, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);

    // (1) The JSON parses.
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout should be valid JSON ({e}):\n{stdout}"));
    let arr = parsed
        .as_array()
        .unwrap_or_else(|| panic!("expected JSON array at top level, got: {stdout}"));
    assert!(
        !arr.is_empty(),
        "expected at least one diagnostic in JSON, got: {stdout}"
    );

    // (2) At least one entry carries a `hints` array.
    let mut saw_hint_array = false;
    let mut all_hints: Vec<String> = Vec::new();
    for entry in arr {
        let hints = entry.get("hints").unwrap_or_else(|| {
            panic!("every diagnostic must expose a `hints` field, got: {entry}")
        });
        let hints_arr = hints
            .as_array()
            .unwrap_or_else(|| panic!("`hints` must be a JSON array, got: {hints}"));
        if !hints_arr.is_empty() {
            saw_hint_array = true;
        }
        for h in hints_arr {
            let s = h
                .as_str()
                .unwrap_or_else(|| panic!("hint entries must be strings, got: {h}"));
            all_hints.push(s.to_string());
        }
    }
    assert!(
        saw_hint_array,
        "expected at least one diagnostic with a non-empty `hints` array, got: {stdout}"
    );

    // (3) At least one hint mentions `did you mean` AND names `println`.
    let hit = all_hints
        .iter()
        .any(|h| h.contains("did you mean") && h.contains("println"));
    assert!(
        hit,
        "expected a hint containing both 'did you mean' and 'println', got hints: {all_hints:?}\nfull JSON:\n{stdout}"
    );

    // Cleanup (best-effort).
    let _ = std::fs::remove_file(&path);
}

/// Regression guard for the backward-compat half of the fix: `message`
/// must still be the FIRST line of the diagnostic body — so existing
/// consumers that key off `message` see the same top-level text they
/// saw before the `hints` field was added. Any hint content must land
/// in `hints`, NOT in `message`.
#[test]
fn test_silt_check_json_message_stays_first_line_round36() {
    let path = temp_silt_file("msg_first_line", r#"fn main() { prntln("hi") }"#);

    let output = silt_cmd()
        .arg("check")
        .arg("--format")
        .arg("json")
        .arg(&path)
        .output()
        .expect("failed to run silt check --format json");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout should be valid JSON ({e}):\n{stdout}"));
    let arr = parsed.as_array().expect("expected JSON array");
    let first = arr.first().expect("expected at least one diagnostic");

    let message = first
        .get("message")
        .and_then(|v| v.as_str())
        .expect("expected `message` field to be a string");
    // First-line invariant: no embedded newline, and no leaked "help:"
    // prefix that belongs in `hints`.
    assert!(
        !message.contains('\n'),
        "message must be a single line, got: {message:?}"
    );
    assert!(
        !message.trim_start().starts_with("help:"),
        "message must not start with a help hint prefix, got: {message:?}"
    );

    let _ = std::fs::remove_file(&path);
}
