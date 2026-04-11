//! Round-15 audit regressions for `silt test` / `silt fmt` / `silt run` DX
//! rendering. Each test here pins one of the gaps called out in the audit:
//!
//! - G1: silt test call-stack rendering on a failing test
//! - G2: silt test setup-error source snippet
//! - G4: silt fmt parse errors with caret
//! - G5: parse/lex EOF errors fall back to the last real source line
//! - G10: silt --help fmt row alignment
//! - L4-cosmetic: "1 test" / "N tests" grammar
//!
//! The tests use the same tempdir + Command pattern as tests/cli.rs so they
//! exercise the real binary end-to-end. We do NOT import from tests/cli.rs
//! (it belongs to another agent) — a small private `temp_silt_file` helper
//! is duplicated here so the suites stay decoupled.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

/// Create a temporary .silt file with the given content. Each call produces
/// a unique filename to avoid collisions between parallel tests.
fn temp_silt_file(prefix: &str, content: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join("silt_cli_test_rendering");
    fs::create_dir_all(&dir).unwrap();
    let name = format!("{prefix}_{n}.silt");
    let path = dir.join(name);
    fs::write(&path, content).unwrap();
    path
}

// ── G1: silt test renders call stack for deeply-failing test ────────

/// A test that fails inside a helper chain should surface every meaningful
/// frame between the test function and the failing assertion — not just
/// the innermost span.
///
/// Mutation reasoning: reverting the G1 fix (dropping the
/// `render_call_stack` call added in src/main.rs under the `Err(e)` arm
/// of the per-test runner) makes the `call stack` / `helper` / `inner`
/// assertions fail immediately — only the innermost span is printed.
#[test]
fn test_silt_test_failing_test_renders_call_stack_when_helper_chain() {
    let path = temp_silt_file(
        "helper_chain_fail_test",
        r#"import test
fn helper(x) {
  let r = inner(x)
  r
}
fn inner(x) {
  test.assert_eq(x, 99)
  x
}
fn test_fails_in_helper() {
  let r = helper(5)
  r
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
        "expected failing test to exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("FAIL"),
        "expected FAIL marker in stderr, got: {stderr}"
    );
    assert!(
        stderr.contains("test_fails_in_helper"),
        "expected test name in stderr, got: {stderr}"
    );
    assert!(
        stderr.contains("call stack"),
        "expected 'call stack' in stderr, got: {stderr}"
    );
    assert!(
        stderr.contains("helper"),
        "expected 'helper' frame in stderr, got: {stderr}"
    );
    assert!(
        stderr.contains("inner"),
        "expected 'inner' frame in stderr, got: {stderr}"
    );
}

// ── G2: silt test setup error renders structured source snippet ─────

/// A top-level runtime error (i.e. in the "setup" script that runs before
/// any test function) should render with a source snippet + caret, not
/// the bare `VmError::Display` string.
///
/// Mutation reasoning: reverting the G2 fix (replacing the
/// `SourceError::runtime_at` branch with the old `eprintln!("{path}: setup
/// error: {e}")` fallback) makes the `-->` locator assertion fail — the
/// VM's default Display never produces a `--> file:line:col` line.
#[test]
fn test_silt_test_setup_error_renders_source_snippet() {
    let path = temp_silt_file(
        "setup_err_test",
        r#"import test
let big = 1 / 0
fn test_never_runs() {
  test.assert_eq(big, 0)
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
        "expected setup error to exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("setup error"),
        "expected 'setup error' marker, got: {stderr}"
    );
    // Structured rendering markers
    assert!(
        stderr.contains("-->"),
        "expected '-->' locator in stderr, got: {stderr}"
    );
    assert!(
        stderr.contains("1 / 0"),
        "expected offending source line '1 / 0' in stderr, got: {stderr}"
    );
    assert!(
        stderr.contains("error[runtime]"),
        "expected 'error[runtime]' label in stderr, got: {stderr}"
    );
    // The bare VmError::Display string should NOT be the only content:
    // specifically, we should NOT see a line starting with the literal
    // "VM error:" prefix — that would indicate we fell back to the old
    // path that drops source info.
    assert!(
        !stderr.lines().any(|l| l.trim_start().starts_with("VM error:")),
        "expected structured rendering, not raw VmError::Display. stderr: {stderr}"
    );
}

// ── G4: silt fmt parse error renders with caret ─────────────────────

/// `silt fmt` on an unparseable file should render the parse error with a
/// source-line snippet and caret, the same as `silt run` / `silt check`.
///
/// Mutation reasoning: reverting the G4 fix (switching `pub fn format`
/// back to `Result<String, String>` or making `format_file` use the raw
/// `{e}` Display path) makes the `-->` locator assertion fail — the raw
/// `FmtError::Display` / `ParseError::Display` doesn't emit that marker.
#[test]
fn test_silt_fmt_parse_error_renders_with_caret() {
    // Intentional unterminated `let x` — classic parse-at-EOF.
    let path = temp_silt_file("fmt_parse_err", "fn main() { let x");

    let output = silt_cmd()
        .arg("fmt")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    assert!(
        !output.status.success(),
        "expected fmt parse error to exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("error[parse]"),
        "expected 'error[parse]' label in stderr, got: {stderr}"
    );
    assert!(
        stderr.contains("-->"),
        "expected '-->' locator in stderr, got: {stderr}"
    );
    assert!(
        stderr.contains("fn main() { let x"),
        "expected source line in stderr, got: {stderr}"
    );
    assert!(
        stderr.contains('^'),
        "expected caret marker in stderr, got: {stderr}"
    );
}

// ── G5: silt run EOF parse error renders last line + caret ──────────

/// When a parse error lands past the final newline (e.g. `let x` with
/// nothing after it), the SourceError renderer should clamp the caret to
/// the end of the last real line instead of showing just the `-->`
/// locator with no snippet.
///
/// Mutation reasoning: reverting the G5 fix (removing
/// `clamp_span_to_source` from `from_parse_error` / `from_lex_error` in
/// src/errors.rs) makes the source-line assertion fail — the renderer
/// would again return `None` for lines past EOF and skip the snippet.
#[test]
fn test_silt_run_eof_error_renders_last_line_with_caret() {
    let path = temp_silt_file("run_eof_err", "fn main() { let x");

    let output = silt_cmd()
        .arg("run")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    assert!(
        !output.status.success(),
        "expected run parse error to exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("error[parse]"),
        "expected 'error[parse]' label in stderr, got: {stderr}"
    );
    assert!(
        stderr.contains("-->"),
        "expected '-->' locator in stderr, got: {stderr}"
    );
    assert!(
        stderr.contains("fn main() { let x"),
        "expected last real line in stderr, got: {stderr}"
    );
    assert!(
        stderr.contains('^'),
        "expected caret marker in stderr, got: {stderr}"
    );
}

// ── G10: silt --help fmt row alignment ──────────────────────────────

/// All usage rows in `silt --help` should align their descriptions at
/// the same column. The fmt row was previously off by a couple spaces.
///
/// Mutation reasoning: reverting the G10 fix (restoring the extra
/// whitespace in the fmt row's description) makes the column-equality
/// assertion fail — the fmt row's description column would no longer
/// match the run/check/test/repl rows.
#[test]
fn test_silt_help_fmt_row_alignment() {
    let output = silt_cmd()
        .arg("--help")
        .output()
        .expect("failed to run silt --help");
    assert!(output.status.success(), "expected --help to exit 0");
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Collect the command rows. Each begins with "  silt " per usage_text().
    let rows: Vec<&str> = stdout
        .lines()
        .filter(|l| l.trim_start().starts_with("silt "))
        .collect();
    assert!(rows.len() >= 5, "expected several usage rows, got: {stdout}");

    // Compute the column of the description word for each row. The
    // description is whatever follows the run of ≥2 spaces after the
    // command signature. We use the position of the first occurrence
    // of `  ` (two spaces) past the first non-space char after `silt`
    // to find the start of the gap, then the first non-space after that.
    fn desc_column(row: &str) -> Option<usize> {
        // Skip leading indent.
        let (_lead_ws, body) = row.split_at(row.len() - row.trim_start().len());
        // Find the first sequence of ≥2 spaces in `body`.
        let mut i = 0;
        let bytes = body.as_bytes();
        while i + 1 < bytes.len() {
            if bytes[i] == b' ' && bytes[i + 1] == b' ' {
                // Scan forward to the first non-space.
                let mut j = i;
                while j < bytes.len() && bytes[j] == b' ' {
                    j += 1;
                }
                if j < bytes.len() {
                    // Column = leading whitespace + j.
                    return Some(row.len() - body.len() + j);
                }
            }
            i += 1;
        }
        None
    }

    // Find rows that the audit identifies. We look for anchor rows by
    // substring so we don't depend on exact wording beyond the command
    // signature.
    let find = |needle: &str| -> &str {
        *rows
            .iter()
            .find(|r| r.contains(needle))
            .unwrap_or_else(|| panic!("no row for {needle:?} in:\n{stdout}"))
    };
    let run_row = find("silt run ");
    let check_row = find("silt check ");
    let test_row = find("silt test ");
    let fmt_row = find("silt fmt ");
    let repl_row = find("silt repl");
    let init_row = find("silt init");

    let run_col = desc_column(run_row).expect("run row desc column");
    let check_col = desc_column(check_row).expect("check row desc column");
    let test_col = desc_column(test_row).expect("test row desc column");
    let fmt_col = desc_column(fmt_row).expect("fmt row desc column");
    let repl_col = desc_column(repl_row).expect("repl row desc column");
    let init_col = desc_column(init_row).expect("init row desc column");

    assert_eq!(
        run_col, check_col,
        "run vs check desc columns differ: run={run_col} check={check_col}"
    );
    assert_eq!(
        run_col, test_col,
        "run vs test desc columns differ: run={run_col} test={test_col}"
    );
    assert_eq!(
        run_col, fmt_col,
        "run vs fmt desc columns differ: run={run_col} fmt={fmt_col}\n\
         rows:\n{run_row}\n{fmt_row}"
    );
    assert_eq!(
        run_col, repl_col,
        "run vs repl desc columns differ: run={run_col} repl={repl_col}"
    );
    assert_eq!(
        run_col, init_col,
        "run vs init desc columns differ: run={run_col} init={init_col}"
    );
}

// ── L4-cosmetic: singular/plural grammar for tests count ────────────

/// A single-test run should print "1 test:", not "1 tests:".
///
/// Mutation reasoning: reverting the L4 fix (replacing the `test_word`
/// selection with a hard-coded "tests") makes the "1 test:" assertion
/// fail — the output would read "1 tests:" again.
#[test]
fn test_silt_test_singular_grammar_one_test() {
    let path = temp_silt_file(
        "singular_grammar",
        r#"import test
fn test_one() {
  test.assert_eq(1, 1)
}
"#,
    );

    let output = silt_cmd()
        .arg("test")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    assert!(
        output.status.success(),
        "expected exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("1 test:"),
        "expected '1 test:' (singular), got: {stderr}"
    );
    assert!(
        !stderr.contains("1 tests:"),
        "expected no plural for count=1, got: {stderr}"
    );
}

/// A multi-test run should still print "N tests:" (plural).
///
/// Mutation reasoning: reverting the L4 fix to hard-code "test" would
/// break this — the plural case needs the `total == 1` branch to be
/// distinct from the fallback.
#[test]
fn test_silt_test_plural_grammar_two_tests() {
    let path = temp_silt_file(
        "plural_grammar",
        r#"import test
fn test_a() {
  test.assert_eq(1, 1)
}
fn test_b() {
  test.assert_eq(2, 2)
}
"#,
    );

    let output = silt_cmd()
        .arg("test")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    assert!(
        output.status.success(),
        "expected exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("2 tests:"),
        "expected '2 tests:' (plural), got: {stderr}"
    );
}
