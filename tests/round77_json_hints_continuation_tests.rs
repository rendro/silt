//! Round-77 audit regression (GAP ERR-1): `silt check --format json` must
//! emit ALL continuation lines from a diagnostic body, not just those
//! prefixed with `help:` / `note:`.
//!
//! The human renderer (`SourceError::Display` at `src/errors.rs:366-389`)
//! prepends `= note:` to any unprefixed first body line. So a diagnostic
//! whose message body is the multi-line string
//!
//!     program has no main() function
//!     add one as the entry point
//!
//! renders to stderr as:
//!
//!     error[compile]: program has no main() function
//!       = note: add one as the entry point
//!
//! Before the fix, `print_json_errors` in `src/cli/check.rs` filtered
//! continuation lines down to those whose trimmed text started with
//! `help:` or `note:`. Structural notes like `add one as the entry point`
//! — which appear unprefixed because the renderer auto-prefixes them —
//! were silently dropped from JSON output, leaving JSON consumers (LSP
//! front-ends, CI scripts) with `"hints": []` while stderr had the
//! continuation. The fix mirrors the renderer: an unprefixed first body
//! line is emitted as a `note:` hint in JSON.
//!
//! This test locks BOTH surfaces in agreement:
//!   1. `silt check --format json` on an empty file emits a `hints`
//!      array containing the continuation note.
//!   2. `silt check` (human) on the same file emits the same hint text
//!      to stderr.

use std::process::Command;

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

fn temp_silt_file(tag: &str, contents: &str) -> std::path::PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("silt_round77_json_hints_{tag}_{n}.silt"));
    std::fs::write(&path, contents).expect("failed to write temp silt file");
    path
}

/// Empty source triggers the `program has no main() function` diagnostic
/// with a structural note `add one as the entry point`. The note must
/// appear in JSON `hints`.
#[test]
fn test_silt_check_json_emits_unprefixed_continuation_note_round77() {
    let path = temp_silt_file("missing_main", "");

    let output = silt_cmd()
        .arg("check")
        .arg("--format")
        .arg("json")
        .arg(&path)
        .output()
        .expect("failed to run silt check --format json");

    assert!(
        !output.status.success(),
        "expected non-zero exit for missing main, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout should be valid JSON ({e}):\n{stdout}"));
    let arr = parsed
        .as_array()
        .unwrap_or_else(|| panic!("expected JSON array at top level, got: {stdout}"));
    assert!(
        !arr.is_empty(),
        "expected at least one diagnostic in JSON, got: {stdout}"
    );

    // Find the `program has no main() function` diagnostic and confirm
    // its `hints` array contains the continuation note.
    let mut found = false;
    for entry in arr {
        let message = entry
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("expected `message` to be a string, got: {entry}"));
        if !message.contains("program has no main()") {
            continue;
        }
        found = true;
        let hints = entry
            .get("hints")
            .and_then(|v| v.as_array())
            .unwrap_or_else(|| panic!("expected `hints` array, got: {entry}"));
        let hint_strs: Vec<String> = hints
            .iter()
            .map(|h| {
                h.as_str()
                    .unwrap_or_else(|| panic!("hint entries must be strings, got: {h}"))
                    .to_string()
            })
            .collect();
        assert!(
            hint_strs.iter().any(|h| h.contains("add one as the entry point")),
            "expected hints to contain the continuation note 'add one as the entry point', got: {hint_strs:?}\nfull JSON:\n{stdout}"
        );
        // Backward-compat: the message line itself must NOT carry the
        // continuation — that belongs in `hints`.
        assert!(
            !message.contains("add one as the entry point"),
            "message must be the first line only, got: {message:?}"
        );
    }
    assert!(
        found,
        "expected a diagnostic mentioning `program has no main()`, got: {stdout}"
    );

    let _ = std::fs::remove_file(&path);
}

/// Parallel lock: the human renderer surfaces the same continuation
/// note. Without this, the JSON fix could drift from the human form.
#[test]
fn test_silt_check_human_form_shows_continuation_note_round77() {
    let path = temp_silt_file("missing_main_human", "");

    let output = silt_cmd()
        .arg("check")
        .arg(&path)
        .output()
        .expect("failed to run silt check");

    assert!(
        !output.status.success(),
        "expected non-zero exit for missing main"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("program has no main()"),
        "expected stderr to mention 'program has no main()', got:\n{stderr}"
    );
    assert!(
        stderr.contains("add one as the entry point"),
        "expected stderr to mention the continuation note 'add one as the entry point', got:\n{stderr}"
    );

    let _ = std::fs::remove_file(&path);
}
