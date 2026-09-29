use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

/// Create a temporary .silt file with the given content.
/// Each call produces a unique filename to avoid collisions between tests.
fn temp_silt_file(prefix: &str, content: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join("silt_cli_tests");
    fs::create_dir_all(&dir).unwrap();
    let name = format!("{prefix}_{n}.silt");
    let path = dir.join(name);
    fs::write(&path, content).unwrap();
    path
}

// ── 1. No args shows usage ──────────────────────────────────────────

#[test]
fn test_no_args_shows_usage() {
    let output = silt_cmd().output().expect("failed to run silt");

    assert!(!output.status.success(), "expected non-zero exit code");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Usage:"),
        "expected usage text in stderr, got: {stderr}"
    );
    assert!(
        stderr.contains("silt run"),
        "expected 'silt run' in usage, got: {stderr}"
    );
}

// ── 4. Run nonexistent file ────────────────────────────────────────

#[test]
fn test_run_nonexistent_file() {
    let output = silt_cmd()
        .arg("run")
        .arg("/tmp/nonexistent_silt_file_99999.silt")
        .output()
        .expect("failed to run silt");

    assert!(!output.status.success(), "expected non-zero exit code");

    let stderr = String::from_utf8_lossy(&output.stderr);
    // silt's CLI wraps the OS error: "error reading <path>: <os-msg>".
    // The os-msg ("No such file or directory") is platform-dependent, but
    // the "error reading" prefix is silt's and stable.
    assert!(
        stderr.contains("error reading /tmp/nonexistent_silt_file_99999.silt"),
        "expected silt's 'error reading <path>' wrapper in stderr, got: {stderr}"
    );
}

// ── 11. Format file ────────────────────────────────────────────────

#[test]
fn test_fmt_file() {
    let path = temp_silt_file("fmt", "fn  main( ) {\nprintln(\"hello\")\n}\n");

    let output = silt_cmd()
        .arg("fmt")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    assert!(
        output.status.success(),
        "expected exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let formatted = fs::read_to_string(&path).expect("failed to read formatted file");
    assert!(
        formatted.contains("fn main()"),
        "expected normalized function signature, got: {formatted}"
    );
    assert!(
        formatted.contains("  println"),
        "expected indented body, got: {formatted}"
    );
}

// ── 11b. Format multiple files continues past errors ───────────────

#[test]
fn test_fmt_continues_past_error_in_multi_file() {
    // Create a valid file and an invalid file (syntax error)
    let good = temp_silt_file("fmt_good", "fn  main( ) {\nprintln(\"hello\")\n}\n");
    let bad = temp_silt_file("fmt_bad", "fn { invalid syntax ???");

    let output = silt_cmd()
        .arg("fmt")
        .arg(&bad)
        .arg(&good)
        .output()
        .expect("failed to run silt");

    // Should exit non-zero because of the bad file
    assert!(
        !output.status.success(),
        "expected non-zero exit due to bad file"
    );

    // The good file should still have been formatted despite the bad file
    let formatted = fs::read_to_string(&good).expect("failed to read good file");
    assert!(
        formatted.contains("fn main()"),
        "good file should still be formatted even when a sibling file fails, got: {formatted}"
    );
}

// ── 12. Init creates file ──────────────────────────────────────────

#[test]
fn test_init_creates_file() {
    // v0.7 init creates a Cargo-style package layout: silt.toml + src/main.silt.
    // The full behavior matrix lives in tests/cli/cli_init_tests.rs; this test
    // pins the bare-minimum integration smoke (init runs, both files exist).
    let dir = std::env::temp_dir().join("silt_cli_tests_init");
    // Clean up from any prior run
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let output = silt_cmd()
        .current_dir(&dir)
        .arg("init")
        .output()
        .expect("failed to run silt");

    assert!(
        output.status.success(),
        "expected exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let manifest = dir.join("silt.toml");
    let main_silt = dir.join("src").join("main.silt");
    assert!(manifest.exists(), "expected silt.toml to be created");
    assert!(main_silt.exists(), "expected src/main.silt to be created");

    let content = fs::read_to_string(&main_silt).expect("failed to read main.silt");
    assert!(
        content.contains("fn main()"),
        "expected fn main() in generated file, got: {content}"
    );
    assert!(
        content.contains("println"),
        "expected println in generated file, got: {content}"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("silt.toml") && stdout.contains("main.silt"),
        "expected creation message naming both files in stdout, got: {stdout}"
    );

    // Clean up
    let _ = fs::remove_dir_all(&dir);
}

// ── 13. Init refuses overwrite ─────────────────────────────────────

#[test]
fn test_init_refuses_overwrite() {
    // v0.7 init refuses to clobber an existing silt.toml; the message
    // mentions the file by name. (Refusal on existing src/main.silt is
    // covered separately in tests/cli/cli_init_tests.rs.)
    let dir = std::env::temp_dir().join("silt_cli_tests_init_overwrite");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    // Create an existing silt.toml (the new project marker).
    fs::write(dir.join("silt.toml"), "existing content").unwrap();

    let output = silt_cmd()
        .current_dir(&dir)
        .arg("init")
        .output()
        .expect("failed to run silt");

    assert!(!output.status.success(), "expected non-zero exit code");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("already exists"),
        "expected 'already exists' in stderr, got: {stderr}"
    );

    // Verify original file was not overwritten
    let content = fs::read_to_string(dir.join("silt.toml")).unwrap();
    assert_eq!(
        content, "existing content",
        "original file should not be modified"
    );

    // Clean up
    let _ = fs::remove_dir_all(&dir);
}

// ── Subcommand --help ───────────────────────────────────────────────

#[test]
fn test_run_help_flag() {
    for flag in ["--help", "-h"] {
        let output = silt_cmd()
            .arg("run")
            .arg(flag)
            .output()
            .expect("failed to run silt");
        assert!(
            output.status.success(),
            "silt run {flag}: expected exit 0, stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("Usage: silt run"),
            "silt run {flag}: expected usage text, got: {stdout}"
        );
    }
}

#[test]
fn test_disasm_help_flag() {
    for flag in ["--help", "-h"] {
        let output = silt_cmd()
            .arg("disasm")
            .arg(flag)
            .output()
            .expect("failed to run silt");
        assert!(
            output.status.success(),
            "silt disasm {flag}: expected exit 0, stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("Usage: silt disasm"),
            "silt disasm {flag}: expected usage text, got: {stdout}"
        );
    }
}

// Regression: `silt lsp --help` / `-h` must print usage and exit 0
// without booting the language server (which would hang on stdio).
#[cfg(feature = "lsp")]
#[test]
fn test_lsp_help_flag() {
    for flag in ["--help", "-h"] {
        let output = silt_cmd()
            .arg("lsp")
            .arg(flag)
            .output()
            .expect("failed to run silt");
        assert!(
            output.status.success(),
            "silt lsp {flag}: expected exit 0, stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        // Production message from src/main.rs lsp subcommand help.
        assert!(
            stdout.contains("Usage: silt lsp"),
            "silt lsp {flag}: expected 'Usage: silt lsp' in help output, got: {stdout}"
        );
        assert!(
            stdout.contains("Start the silt language server"),
            "silt lsp {flag}: expected description line, got: {stdout}"
        );
    }
}

// ── 20. DX1: silt test reports file errors separately from test counts ──
//
// Previously a single lex/parse/compile failure was booked as one "failed
// test", which under-reports how much of the suite actually ran.  The fix
// tracks file-level errors separately and prints them in the summary.

#[test]
fn test_silt_test_reports_file_errors_separately_from_test_counts() {
    // Build a fresh directory containing one good test file (two passing
    // tests) and one file that fails to parse.
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("silt_test_dx1_{n}"));
    fs::create_dir_all(&dir).unwrap();
    let good = dir.join("good_test.silt");
    fs::write(
        &good,
        r#"import test

fn test_one() {
  test.assert_eq(1, 1)
}

fn test_two() {
  test.assert_eq(2, 2)
}
"#,
    )
    .unwrap();
    let bad = dir.join("broken_test.silt");
    fs::write(&bad, "fn test_broken( {\n").unwrap();

    let output = silt_cmd()
        .arg("test")
        .arg(&dir)
        .output()
        .expect("failed to run silt");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");

    // A file-level parse failure must NOT be booked against a real test
    // count. Two tests from good_test.silt must still show up as passed.
    assert!(
        combined.contains("2 passed"),
        "expected '2 passed' despite the broken file, got:\n{combined}"
    );
    // The summary must explicitly report the file failure separately.
    assert!(
        combined.contains("1 file failed to compile"),
        "expected '1 file failed to compile' in summary, got:\n{combined}"
    );
    // And the broken file should get a `failed to compile` diagnostic.
    assert!(
        combined.contains("failed to compile"),
        "expected 'failed to compile' diagnostic on the broken file, got:\n{combined}"
    );
    // Exit code must remain non-zero so CI still fails.
    assert!(
        !output.status.success(),
        "expected non-zero exit when a test file fails to compile, stdout: {stdout}\nstderr: {stderr}"
    );
    // And crucially, the failed test count must NOT be inflated: the
    // summary should NOT claim any test failed (that would conflate
    // file errors with real test failures).
    assert!(
        combined.contains("0 failed"),
        "expected '0 failed' (file errors are tracked separately), got:\n{combined}"
    );
}

// ── silt fmt --check: rejects unformatted files without mutating ───

#[test]
fn test_fmt_check_mode_rejects_unformatted() {
    // Deliberately unformatted: extra whitespace in signature, unindented body.
    let original = "fn  main( ) {\nprintln(\"hello\")\n}\n";
    let path = temp_silt_file("fmt_check_unformatted", original);

    let output = silt_cmd()
        .arg("fmt")
        .arg("--check")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    // (a) Exit code must be 1 — the key --check contract.
    assert!(
        !output.status.success(),
        "expected non-zero exit for unformatted file, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let code = output.status.code().unwrap_or(-1);
    assert_eq!(
        code, 1,
        "expected exit code 1 for unformatted file, got {code}"
    );

    // (b) File on disk MUST be unchanged — --check is read-only.
    let on_disk = fs::read_to_string(&path).expect("failed to read file after --check");
    assert_eq!(
        on_disk, original,
        "silt fmt --check must not mutate the file on disk"
    );

    // (c) Some diagnostic about the file being unformatted must appear.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("not formatted"),
        "expected 'not formatted' diagnostic, got stdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

// ── silt test --help: mentions filename auto-discovery pattern ─────

#[test]
fn test_silt_test_help_mentions_filename_pattern() {
    for flag in ["--help", "-h"] {
        let output = silt_cmd()
            .arg("test")
            .arg(flag)
            .output()
            .expect("failed to run silt");
        assert!(
            output.status.success(),
            "silt test {flag}: expected exit 0, stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("_test.silt"),
            "silt test {flag}: expected '_test.silt' in help output, got: {stdout}"
        );
        assert!(
            stdout.contains(".test.silt"),
            "silt test {flag}: expected '.test.silt' in help output, got: {stdout}"
        );
    }
}

#[test]
fn test_fmt_empty_file() {
    let path = temp_silt_file("empty_fmt", "");

    let output = silt_cmd()
        .arg("fmt")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    // Formatting empty source is a no-op, exit 0.
    assert!(
        output.status.success(),
        "expected exit 0 for empty file under `silt fmt`, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked at"),
        "stderr should not contain a Rust panic, got: {stderr}"
    );

    // File on disk should still be trivially empty (or whitespace-only).
    let formatted = fs::read_to_string(&path).expect("failed to read formatted file");
    assert!(
        formatted.trim().is_empty(),
        "expected empty/whitespace-only content after fmt, got: {formatted:?}"
    );
}

// ════════════════════════════════════════════════════════════════════
// AUDIT ROUND-17 F16 / F17 / F18 LOCK TESTS
//
// F16 — `silt run --watch` without a file used to hang silently in the
// watch loop forever. Fix: dry-validate the underlying subcommand args
// before entering the watch loop. These tests lock the non-hanging
// behavior AND bound their own runtime so a regression cannot hang CI.
//
// F17 — `silt check` no-args banner used to drop `[--watch]`, so the
// usage line drifted between the `--help` path and the "no args" path.
// Fix: single source of truth via `check_usage_banner()`.
//
// F18 — `silt -v` used to error as "unknown command". UNIX convention
// lets lowercase `-v` print version info. Fix: add `-v` as a synonym
// for `--version` / `-V` in the dispatch arm.
// ════════════════════════════════════════════════════════════════════

/// Run `silt <args>` and return (exit_code, stdout, stderr), aborting
/// the child after `wait` if it doesn't exit on its own. Used to guard
/// tests for commands that could regress into an infinite loop (watch
/// hangs); the surrounding test asserts the child DID exit on its own.
///
/// Returns `Err(())` if the child had to be killed by the timeout
/// guard — this surfaces a watch-loop hang as a test failure instead
/// of letting it block CI indefinitely.
fn run_silt_with_timeout(
    args: &[&str],
    wait: std::time::Duration,
) -> Result<(Option<i32>, String, String), String> {
    use std::io::Read;
    use std::process::Stdio;
    use std::time::Instant;

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
                // Child has exited — drain pipes and return.
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
                    // Child is still running past our budget — kill it
                    // and report a hang. A passing fix never trips this.
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "silt {:?} did not exit within {:?} — hang regression",
                        args, wait
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(e) => return Err(format!("try_wait failed: {e}")),
        }
    }
}

// ── F16: `silt run --watch` (no file) exits non-zero with usage ────

#[test]
fn test_run_watch_without_file_exits_non_zero_with_usage() {
    // A passing fix returns within ~100 ms; 3 s is a generous guard
    // that still fails loudly if the watcher re-enters the hang.
    let wait = std::time::Duration::from_secs(3);
    let result = run_silt_with_timeout(&["run", "--watch"], wait)
        .expect("silt run --watch hung — regression of F16");

    let (code, stdout, stderr) = result;
    assert_ne!(
        code,
        Some(0),
        "expected non-zero exit for `silt run --watch` with no file, got stdout: {stdout}, stderr: {stderr}"
    );
    assert!(
        stderr.contains("Usage: silt run"),
        "expected 'Usage: silt run' banner in stderr, got stdout: {stdout}, stderr: {stderr}"
    );
    // Sanity: the watcher's "[watch] Watching for changes..." banner
    // must NOT have been printed — we bailed out before entering the
    // loop at all.
    assert!(
        !stderr.contains("[watch] Watching for changes"),
        "watcher banner must not appear when watch loop was short-circuited, got stderr: {stderr}"
    );
}

// ── F16: `silt check --watch` (no file) exits non-zero with usage ──

#[test]
fn test_check_watch_without_file_exits_non_zero_with_usage() {
    let wait = std::time::Duration::from_secs(3);
    let result = run_silt_with_timeout(&["check", "--watch"], wait)
        .expect("silt check --watch hung — regression of F16");

    let (code, stdout, stderr) = result;
    assert_ne!(
        code,
        Some(0),
        "expected non-zero exit for `silt check --watch` with no file, got stdout: {stdout}, stderr: {stderr}"
    );
    assert!(
        stderr.contains("Usage: silt check"),
        "expected 'Usage: silt check' banner in stderr, got stdout: {stdout}, stderr: {stderr}"
    );
    assert!(
        !stderr.contains("[watch] Watching for changes"),
        "watcher banner must not appear when watch loop was short-circuited, got stderr: {stderr}"
    );
}

// ── F16 bonus: `silt run --watch --help` prints help and exits 0 ───

#[test]
fn test_run_watch_help_exits_zero_with_help() {
    // --help combined with --watch used to be treated the same as a
    // plain `run --watch` (which hung). Fix: detect --help in the watch
    // dispatcher and run the subcommand once so its help handler fires.
    let wait = std::time::Duration::from_secs(3);
    let result = run_silt_with_timeout(&["run", "--watch", "--help"], wait)
        .expect("silt run --watch --help hung — regression of F16");

    let (code, stdout, stderr) = result;
    assert_eq!(
        code,
        Some(0),
        "expected exit 0 for `silt run --watch --help`, got stdout: {stdout}, stderr: {stderr}"
    );
    assert!(
        stdout.contains("Usage: silt run"),
        "expected 'Usage: silt run' in stdout, got stdout: {stdout}, stderr: {stderr}"
    );
    assert!(
        !stderr.contains("[watch] Watching for changes"),
        "watcher banner must not appear for --help, got stderr: {stderr}"
    );
}

// ── F17: `silt check` no-args banner matches `silt check --help` ───

#[test]
fn test_silt_check_no_args_banner_matches_help() {
    // Run `silt check` with no args — it should print the canonical
    // usage banner on stderr and exit 1.
    let no_args = silt_cmd()
        .arg("check")
        .output()
        .expect("failed to run silt check");
    assert!(
        !no_args.status.success(),
        "expected non-zero exit for `silt check` with no args"
    );
    let no_args_stderr = String::from_utf8_lossy(&no_args.stderr);
    let no_args_usage_line = no_args_stderr
        .lines()
        .find(|l| l.starts_with("Usage:"))
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            panic!("expected a 'Usage:' line in `silt check` stderr, got: {no_args_stderr}")
        });

    // Now run `silt check --help` — it should print the same canonical
    // usage banner on stdout and exit 0.
    let help = silt_cmd()
        .arg("check")
        .arg("--help")
        .output()
        .expect("failed to run silt check --help");
    assert!(
        help.status.success(),
        "expected exit 0 for `silt check --help`, stderr: {}",
        String::from_utf8_lossy(&help.stderr)
    );
    let help_stdout = String::from_utf8_lossy(&help.stdout);
    let help_usage_line = help_stdout
        .lines()
        .find(|l| l.starts_with("Usage:"))
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            panic!("expected a 'Usage:' line in `silt check --help` stdout, got: {help_stdout}")
        });

    // The two banners must be byte-identical — regression-lock the
    // single-source-of-truth design.
    assert_eq!(
        no_args_usage_line, help_usage_line,
        "silt check no-args banner ({no_args_usage_line:?}) must match --help banner ({help_usage_line:?})"
    );
    // And both must still mention [--watch].
    assert!(
        no_args_usage_line.contains("[--watch]"),
        "expected '[--watch]' in check no-args banner, got: {no_args_usage_line}"
    );
}

// ── F18: `silt -v` prints version like `silt -V` / `silt --version` ─

#[test]
fn test_silt_lowercase_v_prints_version() {
    let output = silt_cmd()
        .arg("-v")
        .output()
        .expect("failed to run silt -v");
    assert!(
        output.status.success(),
        "expected exit 0 for `silt -v`, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Pin against the cargo-injected package version so the assertion
    // can't drift when we bump the version in Cargo.toml.
    let expected = format!("silt {}", env!("CARGO_PKG_VERSION"));
    assert!(
        stdout.contains(&expected),
        "expected '{expected}' in stdout for `silt -v`, got: {stdout}"
    );

    // And for symmetry, `silt -V` and `silt --version` must still
    // print the same thing (guard against a copy/paste regression).
    for flag in ["-V", "--version"] {
        let other = silt_cmd()
            .arg(flag)
            .output()
            .unwrap_or_else(|e| panic!("failed to run silt {flag}: {e}"));
        assert!(
            other.status.success(),
            "expected exit 0 for `silt {flag}`, stderr: {}",
            String::from_utf8_lossy(&other.stderr)
        );
        let other_stdout = String::from_utf8_lossy(&other.stdout);
        assert!(
            other_stdout.contains(&expected),
            "expected '{expected}' in stdout for `silt {flag}`, got: {other_stdout}"
        );
    }
}

// ── Fix 1: init/repl/lsp reject unknown flags ────────────────────────

#[test]
fn test_init_unknown_flag() {
    let output = silt_cmd()
        .arg("init")
        .arg("--nonexistent")
        .output()
        .expect("failed to run silt");

    assert!(!output.status.success(), "expected non-zero exit code");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown flag") && stderr.contains("--nonexistent"),
        "expected error mentioning the unknown flag, got: {stderr}"
    );
}

#[test]
fn test_repl_unknown_flag() {
    let output = silt_cmd()
        .arg("repl")
        .arg("--nonexistent")
        .output()
        .expect("failed to run silt");

    assert!(!output.status.success(), "expected non-zero exit code");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown flag") && stderr.contains("--nonexistent"),
        "expected error mentioning the unknown flag, got: {stderr}"
    );
}

#[test]
fn test_lsp_unknown_flag() {
    let output = silt_cmd()
        .arg("lsp")
        .arg("--nonexistent")
        .output()
        .expect("failed to run silt");

    assert!(!output.status.success(), "expected non-zero exit code");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown flag") && stderr.contains("--nonexistent"),
        "expected error mentioning the unknown flag, got: {stderr}"
    );
}

// ── Shorthand `silt file.silt` forwards flags to run handler ────────

#[test]
fn test_shorthand_rejects_unknown_flag() {
    let path = temp_silt_file("shorthand_flag", "fn main() { 1 }");
    let output = silt_cmd()
        .arg(path.to_str().unwrap())
        .arg("--bogus")
        .output()
        .expect("failed to run silt");

    assert!(!output.status.success(), "expected non-zero exit code");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown flag") && stderr.contains("--bogus"),
        "expected unknown flag error, got: {stderr}"
    );
}

#[test]
fn test_shorthand_supports_disassemble() {
    let path = temp_silt_file("shorthand_disasm", "fn main() { 1 }");
    let output = silt_cmd()
        .arg(path.to_str().unwrap())
        .arg("--disassemble")
        .output()
        .expect("failed to run silt");

    assert!(output.status.success(), "expected exit 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("==") && stdout.contains("main"),
        "expected disassembly output, got: {stdout}"
    );
}
