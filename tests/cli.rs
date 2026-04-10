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

// ── 2. Unknown command ──────────────────────────────────────────────

#[test]
fn test_unknown_command() {
    let output = silt_cmd()
        .arg("foobar")
        .output()
        .expect("failed to run silt");

    assert!(!output.status.success(), "expected non-zero exit code");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Unknown command"),
        "expected 'Unknown command' in stderr, got: {stderr}"
    );
}

// ── 3. Run hello world ─────────────────────────────────────────────

#[test]
fn test_run_hello_world() {
    let path = temp_silt_file(
        "hello",
        r#"fn main() {
  println("hello")
}
"#,
    );

    let output = silt_cmd()
        .arg("run")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    assert!(
        output.status.success(),
        "expected exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("hello"),
        "expected 'hello' in stdout, got: {stdout}"
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
    assert!(
        stderr.contains("error reading") || stderr.contains("No such file"),
        "expected file-not-found error in stderr, got: {stderr}"
    );
}

// ── 5. Run with parse error ────────────────────────────────────────

#[test]
fn test_run_parse_error() {
    let path = temp_silt_file("parse_err", "fn { }");

    let output = silt_cmd()
        .arg("run")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    assert!(!output.status.success(), "expected non-zero exit code");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("error"),
        "expected parse error in stderr, got: {stderr}"
    );
}

// ── 6. Run with type error ─────────────────────────────────────────

#[test]
fn test_run_type_error() {
    let path = temp_silt_file("type_err", "fn main() { let x: Int = \"hello\" }");

    let output = silt_cmd()
        .arg("run")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    assert!(!output.status.success(), "expected non-zero exit code");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("type mismatch"),
        "expected type error in stderr, got: {stderr}"
    );
}

// ── 7. Run with runtime error (division by zero) ───────────────────

#[test]
fn test_run_runtime_error() {
    let path = temp_silt_file("runtime_err", "fn main() { 1 / 0 }");

    let output = silt_cmd()
        .arg("run")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    assert!(!output.status.success(), "expected non-zero exit code");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("division by zero"),
        "expected runtime error in stderr, got: {stderr}"
    );
}

// ── 7b. Run rejects unknown flags ──────────────────────────────────

#[test]
fn test_run_unknown_flag() {
    let path = temp_silt_file(
        "run_unknown_flag",
        r#"fn main() {
  println("hello")
}
"#,
    );

    let output = silt_cmd()
        .arg("run")
        .arg("--bogus")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    assert!(!output.status.success(), "expected non-zero exit code");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("silt run: unknown flag '--bogus'"),
        "expected unknown-flag error in stderr, got: {stderr}"
    );
    assert!(
        stderr.contains("Run 'silt run --help' for usage."),
        "expected help hint in stderr, got: {stderr}"
    );
}

// ── 8. Check valid file ────────────────────────────────────────────

#[test]
fn test_check_valid_file() {
    let path = temp_silt_file(
        "check_valid",
        r#"fn main() {
  println("ok")
}
"#,
    );

    let output = silt_cmd()
        .arg("check")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    assert!(
        output.status.success(),
        "expected exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ── 9. Check invalid file ──────────────────────────────────────────

#[test]
fn test_check_invalid_file() {
    let path = temp_silt_file("check_invalid", "fn main() { let x: Int = \"hello\" }");

    let output = silt_cmd()
        .arg("check")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    assert!(!output.status.success(), "expected non-zero exit code");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("type mismatch"),
        "expected type error in stderr, got: {stderr}"
    );
}

// ── 10. Check with JSON format ─────────────────────────────────────

#[test]
fn test_check_json_format() {
    let path = temp_silt_file("check_json", "fn main() { let x: Int = \"hello\" }");

    let output = silt_cmd()
        .arg("check")
        .arg("--format")
        .arg("json")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    assert!(!output.status.success(), "expected non-zero exit code");

    let stdout = String::from_utf8_lossy(&output.stdout);
    // The JSON output should be valid JSON
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout should be valid JSON");

    // Should be an array of error objects
    let arr = parsed.as_array().expect("expected JSON array");
    assert!(!arr.is_empty(), "expected at least one error in JSON");

    let first = &arr[0];
    assert!(first.get("file").is_some(), "expected 'file' field");
    assert!(first.get("line").is_some(), "expected 'line' field");
    assert!(first.get("col").is_some(), "expected 'col' field");
    assert!(first.get("message").is_some(), "expected 'message' field");
    assert!(first.get("severity").is_some(), "expected 'severity' field");

    let message = first["message"].as_str().unwrap();
    assert!(
        message.contains("type mismatch"),
        "expected type mismatch in message, got: {message}"
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

    let main_silt = dir.join("main.silt");
    assert!(main_silt.exists(), "expected main.silt to be created");

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
        stdout.contains("created main.silt"),
        "expected creation message in stdout, got: {stdout}"
    );

    // Clean up
    let _ = fs::remove_dir_all(&dir);
}

// ── 13. Init refuses overwrite ─────────────────────────────────────

#[test]
fn test_init_refuses_overwrite() {
    let dir = std::env::temp_dir().join("silt_cli_tests_init_overwrite");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    // Create an existing main.silt
    fs::write(dir.join("main.silt"), "existing content").unwrap();

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
    let content = fs::read_to_string(dir.join("main.silt")).unwrap();
    assert_eq!(
        content, "existing content",
        "original file should not be modified"
    );

    // Clean up
    let _ = fs::remove_dir_all(&dir);
}

// ── 14. Disassemble valid file ─────────────────────────────────────

#[test]
fn test_disasm_valid_file() {
    let path = temp_silt_file(
        "disasm",
        r#"fn main() {
  println("hello")
}
"#,
    );

    let output = silt_cmd()
        .arg("disasm")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    assert!(
        output.status.success(),
        "expected exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.trim().is_empty(),
        "expected non-empty disassembly output"
    );
    // Disassembly should contain bytecode-like content
    assert!(
        stdout.contains("==") || stdout.contains("Constant") || stdout.contains("Return"),
        "expected bytecode disassembly markers, got: {stdout}"
    );
}

// ── 15. Run file directly (without "run" subcommand) ───────────────

#[test]
fn test_run_file_directly() {
    let path = temp_silt_file(
        "direct",
        r#"fn main() {
  println("direct-run")
}
"#,
    );

    let output = silt_cmd().arg(&path).output().expect("failed to run silt");

    assert!(
        output.status.success(),
        "expected exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("direct-run"),
        "expected 'direct-run' in stdout, got: {stdout}"
    );
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

// ── 16. silt test passes a simple test file (G3) ──────────────────

#[test]
fn test_test_subcommand_runs_passing_tests() {
    let path = temp_silt_file(
        "test_pass",
        r#"import test

fn test_add() {
  test.assert_eq(1 + 1, 2)
}

fn test_mul() {
  test.assert_eq(2 * 3, 6)
}

fn skip_test_broken() {
  test.assert(false, "not ready yet")
}
"#,
    );

    let output = silt_cmd()
        .arg("test")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");

    assert!(
        output.status.success(),
        "expected exit 0, stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        combined.contains("passed"),
        "expected 'passed' in output, got stdout: {stdout}\nstderr: {stderr}"
    );
    // Summary should report exactly 2 passed, 0 failed, 1 skipped
    assert!(
        combined.contains("2 passed"),
        "expected '2 passed' in summary, got: {combined}"
    );
    assert!(
        combined.contains("0 failed"),
        "expected '0 failed' in summary, got: {combined}"
    );
    assert!(
        combined.contains("1 skipped"),
        "expected '1 skipped' in summary, got: {combined}"
    );
}

// ── 17. silt test --filter only runs matching tests (G3) ──────────

#[test]
fn test_test_subcommand_filter_flag() {
    let path = temp_silt_file(
        "test_filter",
        r#"import test

fn test_add_small() {
  test.assert_eq(1 + 1, 2)
}

fn test_subtract() {
  test.assert_eq(5 - 3, 2)
}

fn test_add_big() {
  test.assert_eq(100 + 200, 300)
}
"#,
    );

    let output = silt_cmd()
        .arg("test")
        .arg(&path)
        .arg("--filter")
        .arg("add")
        .output()
        .expect("failed to run silt");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");

    assert!(
        output.status.success(),
        "expected exit 0, stdout: {stdout}\nstderr: {stderr}"
    );
    // Only the two tests containing 'add' should have run.
    assert!(
        combined.contains("2 passed"),
        "expected '2 passed' from --filter add, got: {combined}"
    );
    assert!(
        combined.contains("test_add_small"),
        "expected test_add_small in output, got: {combined}"
    );
    assert!(
        combined.contains("test_add_big"),
        "expected test_add_big in output, got: {combined}"
    );
    assert!(
        !combined.contains("test_subtract"),
        "did not expect test_subtract in filtered output, got: {combined}"
    );
}

// ── 18. silt test reports failure with non-zero exit (G3) ─────────

#[test]
fn test_test_subcommand_failing_test_exits_nonzero() {
    let path = temp_silt_file(
        "test_fail",
        r#"import test

fn test_ok() {
  test.assert_eq(1, 1)
}

fn test_fails() {
  test.assert_eq(1, 2, "one equals two")
}
"#,
    );

    let output = silt_cmd()
        .arg("test")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    assert!(
        !output.status.success(),
        "expected non-zero exit code from failing test"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("1 failed") || combined.contains("FAIL"),
        "expected failure report in output, got stdout: {stdout}\nstderr: {stderr}"
    );
}

// ── 19. silt run on a file without main() shows test-file hint (L6) ───

#[test]
fn test_run_missing_main_test_file_hint() {
    // File contains fn test_* but no main — run should detect and nudge.
    let path = temp_silt_file(
        "no_main_test",
        r#"import test

fn test_thing() {
  test.assert_eq(1, 1)
}
"#,
    );

    let output = silt_cmd()
        .arg("run")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    assert!(
        !output.status.success(),
        "expected non-zero exit code when main is missing"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("looks like a test file"),
        "expected 'looks like a test file' hint in stderr, got: {stderr}"
    );
}
