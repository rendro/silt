//! Round-36 audit regressions for the `silt` CLI.
//!
//! Each test here pins one of the gaps called out in round 36:
//!
//! - GAP: `silt test` usage banner drift — top-level `silt --help` row
//!   and `silt test --help` had diverged on the positional token (`[path]`
//!   vs `[file]`). A new `test_usage_banner()` helper in `src/cli/help.rs`
//!   mirrors the pattern established by `run_usage_banner()`,
//!   `check_usage_banner()`, and `disasm_usage_banner()`, and is invoked
//!   from both sites.
//!
//! - LATENT: raw "VM error:" prefix leaking to user output on the
//!   span-less runtime-error fallback in `silt run` and `silt test`.
//!   `VmError::Display` starts with `"VM error: ..."`, which was reaching
//!   users when `Vm::enrich_error` no-opped (empty span table at IP).
//!   Fix funnels span-less errors through `SourceError::runtime_at` with
//!   a zero span so output renders with the canonical `error[runtime]:`
//!   header.

use std::process::Command;

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

// ── Banner parity ──────────────────────────────────────────────────

/// Extract the `silt test ...` row from the top-level `silt --help`
/// output. The row starts with two-space indent + "silt test " and the
/// banner signature extends up until the run of ≥2 spaces that
/// separates it from the description column.
fn extract_top_level_test_row(help: &str) -> Option<String> {
    for line in help.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("silt test ") {
            // Split on the first run of ≥2 spaces — everything before it
            // is the signature, everything after is the description.
            let bytes = trimmed.as_bytes();
            let mut i = 0;
            while i + 1 < bytes.len() {
                if bytes[i] == b' ' && bytes[i + 1] == b' ' {
                    return Some(trimmed[..i].to_string());
                }
                i += 1;
            }
            return Some(trimmed.to_string());
        }
    }
    None
}

/// Extract the "Usage: ..." line from `silt test --help` output and
/// return the text with the leading `Usage: ` prefix stripped.
fn extract_subcommand_usage(help: &str) -> Option<String> {
    help.lines()
        .find(|l| l.starts_with("Usage:"))
        .and_then(|l| l.strip_prefix("Usage: ").map(|s| s.to_string()))
}

/// Round-36 GAP: top-level `silt --help` row for `silt test` and the
/// subcommand's own `silt test --help` banner must match byte-for-byte
/// (after trimming and stripping the `Usage: ` prefix from the
/// subcommand banner).
///
/// Mutation reasoning: reverting the fix (restoring `[file]` in either
/// source) makes the two strings diverge and this assertion fails.
#[test]
fn test_silt_test_banner_consistency_top_level_and_subcommand() {
    let top_help = silt_cmd()
        .arg("--help")
        .output()
        .expect("failed to run silt --help");
    let top_stdout = String::from_utf8_lossy(&top_help.stdout);
    let top_row = extract_top_level_test_row(&top_stdout).unwrap_or_else(|| {
        panic!("no `silt test ...` row in top-level help output:\n{top_stdout}")
    });

    let sub_help = silt_cmd()
        .args(["test", "--help"])
        .output()
        .expect("failed to run silt test --help");
    let sub_stdout = String::from_utf8_lossy(&sub_help.stdout);
    let sub_banner = extract_subcommand_usage(&sub_stdout)
        .unwrap_or_else(|| panic!("no `Usage:` line in `silt test --help` output:\n{sub_stdout}"));

    assert_eq!(
        top_row.trim(),
        sub_banner.trim(),
        "top-level help row and subcommand usage banner diverged:\n\
         top-level: {top_row:?}\n\
         subcommand: {sub_banner:?}"
    );
}

/// Round-36 GAP: canonical form of the `silt test` banner uses `[path]`
/// (not `[file]`) because `silt test` accepts a directory too — the
/// positional argument is semantically a path.
///
/// Mutation reasoning: a regression that renames `[path]` back to
/// `[file]` fails this assertion against both outputs.
#[test]
fn test_silt_test_banner_uses_path_token_not_file() {
    // Top-level help row.
    let top_help = silt_cmd()
        .arg("--help")
        .output()
        .expect("failed to run silt --help");
    let top_stdout = String::from_utf8_lossy(&top_help.stdout);
    let top_row = extract_top_level_test_row(&top_stdout).expect("no silt test row");
    assert!(
        top_row.contains("[path]"),
        "top-level help row missing canonical `[path]` token: {top_row:?}"
    );
    assert!(
        !top_row.contains("[file]"),
        "top-level help row still uses stale `[file]` token: {top_row:?}"
    );

    // Subcommand help banner.
    let sub_help = silt_cmd()
        .args(["test", "--help"])
        .output()
        .expect("failed to run silt test --help");
    let sub_stdout = String::from_utf8_lossy(&sub_help.stdout);
    let sub_banner = extract_subcommand_usage(&sub_stdout).expect("no Usage: line");
    assert!(
        sub_banner.contains("[path]"),
        "subcommand banner missing canonical `[path]` token: {sub_banner:?}"
    );
    assert!(
        !sub_banner.contains("[file]"),
        "subcommand banner still uses stale `[file]` token: {sub_banner:?}"
    );
}

// ── "VM error:" leak guard ─────────────────────────────────────────

/// Round-36 LATENT: `silt run`'s unknown-flag error path must not
/// contain the raw "VM error:" prefix. This is a sentinel negative
/// test: the unknown-flag path doesn't actually trigger the span-less
/// runtime fallback, but it exercises `silt run`'s error rendering and
/// any regression that re-introduces "VM error:" anywhere in run's
/// user-facing output would trip this.
#[test]
fn test_silt_run_unknown_flag_does_not_leak_vm_error_prefix() {
    let output = silt_cmd()
        .args(["run", "--not-a-real-flag"])
        .output()
        .expect("failed to run silt run --not-a-real-flag");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stderr.contains("VM error:"),
        "stderr leaked `VM error:` prefix to user:\n{stderr}"
    );
    assert!(
        !stdout.contains("VM error:"),
        "stdout leaked `VM error:` prefix to user:\n{stdout}"
    );
}

/// Same sentinel guard for `silt test`'s unknown-flag path.
#[test]
fn test_silt_test_unknown_flag_does_not_leak_vm_error_prefix() {
    let output = silt_cmd()
        .args(["test", "--not-a-real-flag"])
        .output()
        .expect("failed to run silt test --not-a-real-flag");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stderr.contains("VM error:"),
        "stderr leaked `VM error:` prefix to user:\n{stderr}"
    );
    assert!(
        !stdout.contains("VM error:"),
        "stdout leaked `VM error:` prefix to user:\n{stdout}"
    );
}

/// Round-36 LATENT (direct repro): construct a `.silt` program that
/// hits a span-less runtime error path through `silt run`, and assert
/// the rendered diagnostic never contains the raw "VM error:" prefix
/// from `VmError::Display`. If `Vm::enrich_error` successfully attaches
/// a span here, the fix still holds because the non-span-less path has
/// always rendered via `SourceError::runtime_at`. If `enrich_error`
/// no-ops (empty span table at IP), the fix's fallback must kick in
/// and strip the prefix.
#[test]
fn test_silt_run_runtime_error_never_contains_vm_error_prefix() {
    // A runtime panic-style failure — divide by zero — is the
    // simplest way to trigger the runtime error path. Whether or not
    // the resulting VmError has a span, the user-facing output must
    // not contain "VM error:".
    let tmp = std::env::temp_dir().join("silt_cli_round36_runtime_error");
    std::fs::create_dir_all(&tmp).unwrap();
    let src = tmp.join("boom.silt");
    std::fs::write(
        &src,
        "fn main() -> Int {\n    let x: Int = 1 / 0\n    x\n}\n",
    )
    .unwrap();

    let output = silt_cmd()
        .current_dir(&tmp)
        .args(["run", "boom.silt"])
        .output()
        .expect("failed to run silt run boom.silt");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !output.status.success(),
        "expected failing exit for divide-by-zero:\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !stderr.contains("VM error:"),
        "stderr leaked `VM error:` prefix:\n{stderr}"
    );
    assert!(
        !stdout.contains("VM error:"),
        "stdout leaked `VM error:` prefix:\n{stdout}"
    );
}
