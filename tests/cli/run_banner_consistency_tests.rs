// ════════════════════════════════════════════════════════════════════
// AUDIT ROUND-23 F: `silt run` usage-banner single-source-of-truth
//
// Before this fix, `silt run`'s usage banner was emitted from four
// separate string literals in src/main.rs:
//   - watch dry-validation path (approximately :402)
//   - `silt run --help` path      (approximately :458)
//   - no-args path                (approximately :471)
//   - missing-file-after-flags    (approximately :493)
//
// Three of those literals had actually drifted apart:
//   `silt run`                      -> "Usage: silt run <file.silt>"
//   `silt run --watch`              -> "Usage: silt run [--watch] <file.silt>"
//   `silt run --help`               -> "Usage: silt run [--watch] [--disassemble] <file.silt>"
//
// Fix: factor a `run_usage_banner()` helper (mirroring the round-16
// `check_usage_banner()` fix) and have every call site render from it.
// These tests lock the single-source-of-truth design so the banner
// can't drift again.
// ════════════════════════════════════════════════════════════════════

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

/// Run `silt <args>` with a hard timeout so a regression into a
/// hanging watch loop fails the test rather than blocking CI.
fn run_silt_with_timeout(
    args: &[&str],
    wait: Duration,
) -> Result<(Option<i32>, String, String), String> {
    let mut child = silt_cmd()
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn silt: {e}"))?;

    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut out = String::new();
                let mut err = String::new();
                if let Some(mut s) = child.stdout.take() {
                    let _ = s.read_to_string(&mut out);
                }
                if let Some(mut s) = child.stderr.take() {
                    let _ = s.read_to_string(&mut err);
                }
                return Ok((status.code(), out, err));
            }
            Ok(None) => {
                if start.elapsed() >= wait {
                    let _ = child.kill();
                    return Err(format!(
                        "silt {args:?} did not exit within {wait:?} — likely hang"
                    ));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return Err(format!("try_wait failed: {e}")),
        }
    }
}

/// Extract the first line starting with "Usage:" from `text`.
fn usage_line(text: &str) -> Option<String> {
    text.lines()
        .find(|l| l.starts_with("Usage:"))
        .map(|s| s.to_string())
}

/// Run silt and return (usage_line_from_merged_streams, full_stdout, full_stderr).
/// The usage banner may land on stdout (for --help) or stderr (for
/// error paths), so this helper checks both.
fn silt_usage_line(args: &[&str]) -> (String, String, String) {
    let wait = Duration::from_secs(3);
    let (_code, stdout, stderr) = run_silt_with_timeout(args, wait).expect("silt invocation hung");
    let line = usage_line(&stdout)
        .or_else(|| usage_line(&stderr))
        .unwrap_or_else(|| {
            panic!(
                "no 'Usage:' line in output for `silt {}`\nstdout: {stdout}\nstderr: {stderr}",
                args.join(" ")
            )
        });
    (line, stdout, stderr)
}

// ── Core test: all four `silt run` banners are byte-identical ───────

#[test]
fn test_silt_run_banner_consistency_all_paths() {
    // Path 1: `silt run` (no args)        — missing-args path (stderr)
    let (no_args_line, _, _) = silt_usage_line(&["run"]);

    // Path 2: `silt run --help`           — help path (stdout)
    let (help_line, _, _) = silt_usage_line(&["run", "--help"]);

    // Path 3: `silt run --watch`          — watch dry-validation path (stderr)
    //         (requires `watch` feature, which is on by default)
    let (watch_line, _, _) = silt_usage_line(&["run", "--watch"]);

    // Path 4: `silt run --watch --disassemble` (no file)
    //         — missing-file-after-flags path (stderr)
    //         This goes through the watch dry-validation path too
    //         because of the --watch flag; ensure it still matches.
    let (watch_disasm_line, _, _) = silt_usage_line(&["run", "--watch", "--disassemble"]);

    // All four must be byte-identical — the single-source-of-truth lock.
    assert_eq!(
        no_args_line, help_line,
        "`silt run` no-args banner must match `silt run --help` banner"
    );
    assert_eq!(
        no_args_line, watch_line,
        "`silt run` no-args banner must match `silt run --watch` banner"
    );
    assert_eq!(
        no_args_line, watch_disasm_line,
        "`silt run` no-args banner must match `silt run --watch --disassemble` banner"
    );

    // And all must mention the full flag set — [--watch] AND [--disassemble].
    assert!(
        no_args_line.contains("[--watch]"),
        "expected '[--watch]' in run banner, got: {no_args_line}"
    );
    assert!(
        no_args_line.contains("[--disassemble]"),
        "expected '[--disassemble]' in run banner, got: {no_args_line}"
    );
}

// ── Exit-code lock: non-help paths exit non-zero; --help exits zero ─

#[test]
fn test_silt_run_banner_exit_codes() {
    let wait = Duration::from_secs(3);

    // No args: non-zero exit, banner on stderr.
    let (code, _, stderr) = run_silt_with_timeout(&["run"], wait).expect("silt run hung");
    assert_ne!(
        code,
        Some(0),
        "expected non-zero exit for `silt run`, stderr: {stderr}"
    );
    assert!(
        stderr.contains("Usage: silt run"),
        "expected 'Usage: silt run' in stderr for `silt run`, got: {stderr}"
    );

    // --help: exit 0, banner on stdout.
    let (code, stdout, stderr) =
        run_silt_with_timeout(&["run", "--help"], wait).expect("silt run --help hung");
    assert_eq!(
        code,
        Some(0),
        "expected exit 0 for `silt run --help`, stderr: {stderr}"
    );
    assert!(
        stdout.contains("Usage: silt run"),
        "expected 'Usage: silt run' in stdout for `silt run --help`, got: {stdout}"
    );

    // --watch (no file): non-zero exit, banner on stderr, and watch
    // loop must NOT have been entered (otherwise F16 has regressed).
    let (code, _, stderr) =
        run_silt_with_timeout(&["run", "--watch"], wait).expect("silt run --watch hung");
    assert_ne!(
        code,
        Some(0),
        "expected non-zero exit for `silt run --watch` with no file, stderr: {stderr}"
    );
    assert!(
        !stderr.contains("[watch] Watching for changes"),
        "watcher banner must not appear when watch loop was short-circuited, stderr: {stderr}"
    );
}

// ── Round-25 GAP: top-level `silt --help` advertises --disassemble ─
//
// The four subcommand-level banners (silt run / silt run --help /
// silt run --watch / silt run --watch --disassemble) are already
// locked to include `[--watch] [--disassemble]` by the tests above.
//
// Before round 25, the TOP-LEVEL `silt --help` summary line for
// `silt run` showed only `[--watch]`, so users reading the main
// help screen never discovered the `--disassemble` flag. Fix: add
// `[--disassemble]` to the summary line in src/main.rs usage_text().
// This test locks the summary line so it can't drift back.

#[test]
fn test_silt_top_level_help_run_line_advertises_disassemble() {
    let wait = Duration::from_secs(3);
    let (code, stdout, stderr) =
        run_silt_with_timeout(&["--help"], wait).expect("silt --help hung");
    assert_eq!(
        code,
        Some(0),
        "expected exit 0 for `silt --help`, stderr: {stderr}"
    );
    // Find the summary line for `silt run` in the top-level help.
    let run_line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("silt run "))
        .unwrap_or_else(|| {
            panic!(
                "no `silt run` summary line in top-level help output\nstdout: {stdout}\nstderr: {stderr}"
            )
        });
    assert!(
        run_line.contains("[--watch]"),
        "top-level `silt --help` run line must advertise [--watch], got: {run_line}"
    );
    assert!(
        run_line.contains("[--disassemble]"),
        "top-level `silt --help` run line must advertise [--disassemble] so users \
         can discover the flag from the main help screen (round-25 GAP lock), got: {run_line}"
    );
}
