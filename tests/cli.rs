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
    // silt's CLI wraps the OS error: "error reading <path>: <os-msg>".
    // The os-msg ("No such file or directory") is platform-dependent, but
    // the "error reading" prefix is silt's and stable.
    assert!(
        stderr.contains("error reading /tmp/nonexistent_silt_file_99999.silt"),
        "expected silt's 'error reading <path>' wrapper in stderr, got: {stderr}"
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
    // Pin the exact parser diagnostic for `fn { }`. The `error[parse]`
    // header comes from the CLI renderer; the specific phrase
    // "expected identifier, found {" is emitted by
    // src/parser.rs:115 when parsing a function's name slot.
    // A bare `contains("error")` would also match type / runtime /
    // CLI errors, masking drift to the wrong error kind.
    assert!(
        stderr.contains("error[parse]") && stderr.contains("expected identifier, found {"),
        "expected parse-phase identifier error in stderr, got: {stderr}"
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
    // v0.7 init creates a Cargo-style package layout: silt.toml + src/main.silt.
    // The full behavior matrix lives in tests/cli_init_tests.rs; this test
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
    // covered separately in tests/cli_init_tests.rs.)
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
    // Disassembly should contain bytecode-like content. The exact format is
    // stable and produced by src/debug.rs.
    assert!(
        stdout.contains("== <script> (arity=0, upvalues=0) =="),
        "expected script header in disasm output, got: {stdout}"
    );
    assert!(
        stdout.contains("== main (arity=0, upvalues=0) =="),
        "expected main header in disasm output, got: {stdout}"
    );
    assert!(
        stdout.contains("Return"),
        "expected Return opcode in disasm output, got: {stdout}"
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
    // Production summary format from src/cli/test.rs: "N tests: A passed,
    // B failed, C skipped". We expect exactly "1 failed" and the per-case
    // "FAIL" marker.
    assert!(
        combined.contains("1 failed"),
        "expected '1 failed' in test summary, got stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        combined.contains("FAIL "),
        "expected per-test 'FAIL ' marker, got stdout: {stdout}\nstderr: {stderr}"
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

// ── E1: runtime errors from imported modules render with module source ──
//
// Before the fix, a runtime error inside `foo.silt` (imported by
// `main.silt`) was rendered as `main.silt:<line>:<col>` using the main
// file's source text for the snippet, because `Function` carried no
// source_file identity and `vm_run_file` always passed the main source
// to `SourceError::runtime_at`. Users got a nonsensical pointer into
// the main file at a line that might not even exist.

#[test]
fn test_runtime_error_from_module_shows_module_source() {
    // Fresh project dir so imports resolve cleanly relative to main.silt.
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("silt_e1_module_err_{n}"));
    fs::create_dir_all(&dir).unwrap();

    // foo.silt: a public function with a division-by-zero panic.
    // Using `g() + 0` in f so neither frame gets TCO-collapsed.
    let foo = dir.join("foo.silt");
    fs::write(&foo, "pub fn bad() {\n  1 / 0\n}\n").unwrap();

    // main.silt: imports foo and calls foo.bad from main.
    let main = dir.join("main.silt");
    fs::write(&main, "import foo\n\nfn main() {\n  foo.bad()\n}\n").unwrap();

    let output = silt_cmd()
        .arg("run")
        .arg(&main)
        .output()
        .expect("failed to run silt");

    assert!(
        !output.status.success(),
        "expected non-zero exit from module runtime error"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);

    // The error must be rendered against foo.silt — both the file label
    // and the snippet line must come from foo.silt, not main.silt. We
    // assert on a substring rather than the exact path so the test
    // survives changes to how the error renderer formats paths.
    assert!(
        stderr.contains("foo.silt"),
        "expected `foo.silt` in stderr (the file the error actually came from), got:\n{stderr}"
    );
    // The snippet in the rendered error should contain the offending
    // code from foo.silt, NOT main.silt. `1 / 0` is the body line we
    // placed in foo.silt.
    assert!(
        stderr.contains("1 / 0"),
        "expected source snippet `1 / 0` (from foo.silt) in stderr, got:\n{stderr}"
    );
    // And the error message itself must mention division by zero.
    assert!(
        stderr.contains("division by zero"),
        "expected `division by zero` in stderr, got:\n{stderr}"
    );
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

// ── silt fmt --check: accepts already-formatted files ──────────────

#[test]
fn test_fmt_check_mode_accepts_formatted() {
    // Already formatted by silt's formatter.
    let original = "fn main() {\n  println(\"hello\")\n}\n";
    let path = temp_silt_file("fmt_check_formatted", original);

    let output = silt_cmd()
        .arg("fmt")
        .arg("--check")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    // Exit 0 for a well-formatted file.
    assert!(
        output.status.success(),
        "expected exit 0 for already-formatted file, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // File on disk must still be unchanged (--check is read-only).
    let on_disk = fs::read_to_string(&path).expect("failed to read file after --check");
    assert_eq!(
        on_disk, original,
        "silt fmt --check must not mutate the file on disk"
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

// ════════════════════════════════════════════════════════════════════
// AUDIT REGRESSION: `silt test <dir> --filter <needle>` with zero
// surviving files must exit 0 with a specific "no matching test files
// found" message, rather than treating the empty filter result as a
// failure. Locks the fix in src/main.rs:1131-1166.
// ════════════════════════════════════════════════════════════════════

#[test]
fn test_silt_test_filter_empty_result_exits_zero_with_message() {
    // Dedicated directory so this test cannot collide with sibling tests
    // that share the shared `silt_cli_tests` tempdir.
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("silt_cli_filter_empty_{n}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    // A real, valid `_test.silt` file containing one test function.
    // The filter below must NOT match this function's name.
    let test_file = dir.join("foo_test.silt");
    fs::write(&test_file, "fn test_a() { }\nfn main() { }\n").unwrap();

    let output = silt_cmd()
        .arg("test")
        .arg(dir.to_str().unwrap())
        .arg("--filter")
        .arg("xyz_nonexistent_filter")
        .output()
        .expect("failed to run silt");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "expected exit 0 for empty --filter result, stdout: {stdout}, stderr: {stderr}"
    );
    // Lock the exact "no matching test files found" string from main.rs
    // so a regression that treats an empty filter result as a fatal
    // error — or changes the message beyond recognition — is caught.
    assert!(
        stdout.contains("no matching test files found"),
        "expected 'no matching test files found' in stdout, got: {stdout}"
    );

    // Clean up.
    let _ = fs::remove_dir_all(&dir);
}

// ── Module runtime error with name collision renders correct file ──
//
// When a module and the main file both define `fn run()` (or any other
// colliding top-level name), the runtime-error renderer must NOT blame
// the module's source — the bare-name map in `collect_module_function_sources`
// cannot disambiguate, so the renderer must fall back to the main file's
// source instead. Without the fix, a `y / 0` in main's `run()` was rendered
// at `mod1.silt:5:3` with mod1's snippet line, producing a completely
// nonsensical error pointer.

#[test]
fn test_module_runtime_error_with_name_collision_renders_correct_file() {
    // Fresh project dir so imports resolve cleanly relative to main.silt.
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("silt_module_err_collision_{n}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    // mod1.silt defines its own `pub fn run()` — the NAME collides with
    // main's `fn run()` below.  Line 5 is `let z = 99`, which must NEVER
    // appear in the rendered error snippet.
    let mod1 = dir.join("mod1.silt");
    fs::write(
        &mod1,
        "pub fn run() {\n  \"this is completely unrelated\"\n  42\n  1 * 2\n  let z = 99\n}\n",
    )
    .unwrap();

    // main.silt imports mod1 and defines its OWN `fn run()` whose line 5
    // is `y / 0`. The runtime error fires here, in main.silt.
    let main = dir.join("main.silt");
    fs::write(
        &main,
        "import mod1\n\nfn run() {\n  let y = 5\n  y / 0\n}\n\nfn main() { run() }\n",
    )
    .unwrap();

    let output = silt_cmd()
        .arg("run")
        .arg(&main)
        .output()
        .expect("failed to run silt");

    assert!(
        !output.status.success(),
        "expected non-zero exit from runtime error"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);

    // Error message must mention division by zero.
    assert!(
        stderr.contains("division by zero"),
        "expected `division by zero` in stderr, got:\n{stderr}"
    );
    // Error must be rendered against main.silt — the actual origin file.
    assert!(
        stderr.contains("main.silt"),
        "expected `main.silt` in stderr (the real origin of the error), got:\n{stderr}"
    );
    // Error must NOT be blamed on mod1.silt.  That's the exact bug we're
    // locking down: bare-name collisions caused the renderer to resolve
    // to the first-seen module that happens to define a function with the
    // same name.
    assert!(
        !stderr.contains("mod1.silt"),
        "did not expect `mod1.silt` in stderr (collision should fall back to main), got:\n{stderr}"
    );
    // Snippet must show the OFFENDING line from main.silt, not mod1.silt.
    assert!(
        stderr.contains("y / 0"),
        "expected snippet `y / 0` (from main.silt) in stderr, got:\n{stderr}"
    );
    assert!(
        !stderr.contains("let z = 99"),
        "did not expect mod1.silt's `let z = 99` snippet in stderr, got:\n{stderr}"
    );

    // Clean up.
    let _ = fs::remove_dir_all(&dir);
}

// ── Empty file input (round-16 GAP G7) ─────────────────────────────
//
// Every `silt <subcmd>` path must handle a zero-byte `.silt` file
// without panicking. Historically, an indexing bug on `tokens[0]` or
// an unconditional `main.call(...)` could ship a user-visible panic
// for the common case of an empty file (user just ran `silt init` and
// deleted the scaffolding, or is piping in a placeholder).
//
// Each test pins the exact handled behavior observed at lock time:
//
//   silt run    : exit 1 with `error[compile]: program has no main() function`
//                 — the entry-point check catches the empty program cleanly.
//                 (Round-24 B: now rendered with the canonical compile-error
//                 header, not a bare `{path}: ...` line.)
//   silt check  : exit 1 with same `error[compile]: program has no main()`
//                 diagnostic — `silt check` mirrors `silt run`.
//                 (Round-24 B: was previously exit 0 silent, which meant
//                 `silt check` passed programs that `silt run` rejected.)
//   silt fmt    : exit 0 (formatting empty source is a no-op).
//   silt test   : exit 0 with "0 tests: 0 passed, 0 failed, 0 skipped".
//   silt disasm : exit 0 (disassembles the implicit script frame).
//
// If any of these regress to a panic or non-zero unhandled error,
// this walker catches it before release.

#[test]
fn test_run_empty_file() {
    let path = temp_silt_file("empty_run", "");

    let output = silt_cmd()
        .arg("run")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    // Must NOT crash / panic. The handled behavior is exit 1 with a
    // clean "no main() function" diagnostic from the CLI.
    assert!(
        !output.status.success(),
        "expected non-zero exit (empty program has no main), stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let code = output.status.code();
    assert_eq!(
        code,
        Some(1),
        "expected clean exit code 1, got {:?} (panic / unhandled error?)",
        code
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("program has no main() function"),
        "expected clean 'no main()' diagnostic, got: {stderr}"
    );
    // A panic would surface `panicked at` or `RUST_BACKTRACE` hints.
    assert!(
        !stderr.contains("panicked at"),
        "stderr should not contain a Rust panic, got: {stderr}"
    );
}

#[test]
fn test_check_empty_file() {
    let path = temp_silt_file("empty_check", "");

    let output = silt_cmd()
        .arg("check")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    // Round-24 B: `silt check` now surfaces the same missing-main
    // diagnostic as `silt run`, so the canonical outcome is exit 1
    // with `error[compile]: program has no main() function`. Without
    // this alignment, `silt check` would pass a program that
    // `silt run` rejects — off-spec.
    assert!(
        !output.status.success(),
        "expected non-zero exit for empty file under `silt check`, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked at"),
        "stderr should not contain a Rust panic, got: {stderr}"
    );
    assert!(
        stderr.contains("error[compile]:"),
        "expected canonical `error[compile]:` header, got: {stderr}"
    );
    assert!(
        stderr.contains("program has no main() function"),
        "expected missing-main payload, got: {stderr}"
    );
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

#[test]
fn test_test_empty_file() {
    let path = temp_silt_file("empty_test", "");

    let output = silt_cmd()
        .arg("test")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    // Empty file has no test functions, so `silt test` reports 0/0/0
    // and exits cleanly. Exit 0 is the handled behavior.
    assert!(
        output.status.success(),
        "expected exit 0 for empty file under `silt test`, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked at"),
        "stderr should not contain a Rust panic, got: {stderr}"
    );
    // The "0 tests: ..." summary line is emitted on stderr by the
    // test runner (stdout is reserved for test output).
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stderr.contains("0 tests:"),
        "expected '0 tests:' line in stderr, got stderr: {stderr}\nstdout: {stdout}"
    );
}

#[test]
fn test_disasm_empty_file() {
    let path = temp_silt_file("empty_disasm", "");

    let output = silt_cmd()
        .arg("disasm")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    // An empty program still produces a valid (trivial) implicit
    // script frame. Exit 0 is the handled behavior.
    assert!(
        output.status.success(),
        "expected exit 0 for empty file under `silt disasm`, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked at"),
        "stderr should not contain a Rust panic, got: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("== <script>"),
        "expected script header in disasm output, got: {stdout}"
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

// ── JSON message field must not contain embedded newlines ───────────
//
// Module import errors produce multi-line diagnostic strings (source
// snippets, carets, gutter lines).  The JSON output must truncate at the
// first newline so programmatic consumers get a clean single-line message.

#[test]
fn test_json_error_message_field_has_no_embedded_newlines() {
    // Fresh project dir so imports resolve relative to main.silt.
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("silt_json_nl_{n}"));
    fs::create_dir_all(&dir).unwrap();

    // broken.silt: a module with a parse error (unclosed parameter list).
    let broken = dir.join("broken.silt");
    fs::write(&broken, "pub fn broken(\n").unwrap();

    // main.silt: imports the broken module.
    let main = dir.join("main.silt");
    fs::write(&main, "import broken\n\nfn main() {}\n").unwrap();

    let output = silt_cmd()
        .arg("check")
        .arg("--format")
        .arg("json")
        .arg(&main)
        .output()
        .expect("failed to run silt");

    assert!(
        !output.status.success(),
        "expected non-zero exit code for broken import"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout should be valid JSON");
    let arr = parsed.as_array().expect("expected JSON array");
    assert!(
        !arr.is_empty(),
        "expected at least one error in JSON output"
    );

    // Find an error whose message mentions the module parse failure.
    let module_err = arr.iter().find(|e| {
        e["message"]
            .as_str()
            .is_some_and(|m| m.contains("module 'broken'"))
    });
    assert!(
        module_err.is_some(),
        "expected a module-related error in JSON output, got: {stdout}"
    );

    let msg = module_err.unwrap()["message"].as_str().unwrap();

    // The message field must NOT contain a newline character.
    assert!(
        !msg.contains('\n'),
        "JSON message field must not contain embedded newlines, got: {msg:?}"
    );

    // The message field should still contain the key error phrase
    // including the single-quoted module name so callers can
    // programmatically identify what went wrong.
    assert!(
        msg.contains("module 'broken'") && msg.contains("expected parameter name"),
        "expected key error phrase in message, got: {msg:?}"
    );
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

// ── Fix 4: silt fmt --check error has no redundant path prefix ────────

#[test]
fn test_fmt_check_error_no_redundant_path_prefix() {
    // Create a file with a parse error so fmt --check hits the error path.
    let path = temp_silt_file("fmt_redundant_prefix", "fn broken( {\n}\n");

    let output = silt_cmd()
        .arg("fmt")
        .arg("--check")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    assert!(!output.status.success(), "expected non-zero exit code");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let path_str = path.to_str().unwrap();

    // The stderr should NOT start with "path: error[..." — the path should
    // only appear in the "--> path:line:col" locator line inside the SourceError.
    // Check that no line begins with the filename followed by ": error[".
    for line in stderr.lines() {
        assert!(
            !line.starts_with(&format!("{path_str}: error[")),
            "found redundant path prefix in error output: {line}\nfull stderr:\n{stderr}"
        );
    }

    // The path should still appear in the --> locator line.
    assert!(
        stderr.contains("-->"),
        "expected '-->' locator line in stderr, got: {stderr}"
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
