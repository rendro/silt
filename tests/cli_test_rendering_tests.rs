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

// ── F12: multi-line VmError body renders below caret ─────────────────

/// Round-17 audit F12: a runtime error whose message body spans
/// multiple lines (e.g. `regex.is_match("[unclosed", "hello")`'s parse
/// error, which embeds its own caret diagram) must render the body as
/// `  = note:` continuation lines AFTER the caret block, not inline
/// with the header where it orphans above the `-->` location.
///
/// Mutation reasoning: reverting the F12 fix in src/errors.rs (removing
/// the `note_body` emission block and writing the full `self.message`
/// into the header as before) makes the `  = note:` substring either
/// not appear at all, or appear BEFORE the caret — both of which fail
/// the ordering assertion below.
#[test]
fn test_multi_line_vm_error_renders_body_below_caret() {
    let path = temp_silt_file(
        "multi_line_vm_err",
        r#"import regex
fn main() {
  let r = regex.is_match("[unclosed", "hello")
  r
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
        "expected multi-line regex error to exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("error[runtime]"),
        "expected 'error[runtime]' label, got: {stderr}"
    );
    assert!(
        stderr.contains("-->"),
        "expected '-->' locator, got: {stderr}"
    );
    assert!(
        stderr.contains("= note:"),
        "expected '= note:' continuation marker, got: {stderr}"
    );

    // Ordering: the header must come first, then `-->`, then the caret
    // line, then the `= note:` body. Any other order is the F12 bug.
    let header_idx = stderr
        .find("error[runtime]")
        .expect("header not found");
    let loc_idx = stderr.find("-->").expect("--> not found");
    // Find the caret-bearing line (the one with `^` pointing at the
    // offending source span). This must come after `-->` and before
    // the `= note:` body.
    let caret_idx = stderr
        .lines()
        .scan(0usize, |acc, l| {
            let at = *acc;
            *acc += l.len() + 1;
            Some((at, l))
        })
        .find(|(_, l)| l.contains('^') && l.contains("invalid regex"))
        .map(|(at, _)| at)
        .expect("caret line with 'invalid regex' not found");
    let note_idx = stderr.find("= note:").expect("= note: not found");

    assert!(
        header_idx < loc_idx,
        "header must precede '-->' locator, got: {stderr}"
    );
    assert!(
        loc_idx < caret_idx,
        "'-->' locator must precede caret, got: {stderr}"
    );
    assert!(
        caret_idx < note_idx,
        "caret must precede '= note:' body, got: {stderr}"
    );
}

// ── F14: multiple errors have blank separator ───────────────────────

/// Round-17 audit F14: when `silt check` reports multiple type errors
/// in a single file, consecutive error blocks must be separated by a
/// blank line — otherwise the terminal output looks like a solid wall
/// of text that's hard to scan. rustc/gcc both follow this convention.
///
/// Mutation reasoning: reverting the F14 fix in src/main.rs (dropping
/// the `eprintln!()` after each `eprintln!("{err}")` in the check path)
/// removes the blank separator, so `"\n\nerror["` is no longer present
/// between consecutive errors.
#[test]
fn test_multiple_errors_render_with_blank_separator() {
    let path = temp_silt_file(
        "multi_err_separator",
        r#"fn main() {
  let a: Int = "foo"
  let b: Int = true
  let c: Int = [1]
  a
}
"#,
    );

    let output = silt_cmd()
        .arg("check")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    assert!(
        !output.status.success(),
        "expected multi-error file to exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);

    // At least two error blocks must be present.
    let error_count = stderr.matches("error[type]").count();
    assert!(
        error_count >= 2,
        "expected >= 2 type errors, got {error_count}: {stderr}"
    );
    // The key ordering marker: two consecutive newlines (a blank line)
    // immediately before the second `error[` header. rustc convention.
    assert!(
        stderr.contains("\n\nerror["),
        "expected '\\n\\nerror[' (blank line between diagnostics), got: {stderr}"
    );
}

// ── F13: cross-module call stack consistent path style ─────────────

/// Round-17 audit F13: when a runtime error's call stack crosses a
/// module boundary, every `->` frame line in the rendered stack must
/// use a consistent path style (either all absolute or all relative to
/// cwd). Previously the module_sources lookup returned absolute paths
/// while the fallback user path was verbatim (often relative), so a
/// cross-module stack rendered a confusing mix like
/// `-> wrapper  at /tmp/.../calc.silt:2:20` alongside
/// `-> main  at main.silt:9:11`.
///
/// Mutation reasoning: reverting the F13 fix in src/main.rs (dropping
/// the path-normalization in `print_frame`) makes the paths for
/// module-local frames absolute while user-code frames stay verbatim
/// (relative), tripping the "consistent prefix" assertion below.
#[test]
fn test_cross_module_call_stack_uses_consistent_path_style() {
    // Set up a temp dir with lib + main; use relative path on the
    // command line so the main file's path stays relative. The module
    // resolver canonicalizes imports to absolute paths — that's the
    // asymmetry the finding points at.
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("silt_cross_module_paths_{n}"));
    fs::create_dir_all(&dir).unwrap();
    let lib = dir.join("calc.silt");
    let main = dir.join("main.silt");
    fs::write(
        &lib,
        "pub fn kaboom() = 1 / 0\npub fn wrapper() = kaboom()\n",
    )
    .unwrap();
    fs::write(
        &main,
        r#"import calc

fn helper() {
  let r = calc.wrapper()
  r
}

fn main() {
  let r = helper()
  r
}
"#,
    )
    .unwrap();

    let output = silt_cmd()
        .arg("run")
        .arg("main.silt")
        .current_dir(&dir)
        .output()
        .expect("failed to run silt");

    assert!(
        !output.status.success(),
        "expected runtime error to exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("division by zero"),
        "expected division by zero, got: {stderr}"
    );
    assert!(
        stderr.contains("call stack:"),
        "expected call stack block, got: {stderr}"
    );

    // Collect every "  -> <name>  at <path>:line:col" frame line. The
    // path prefix before the `<name>.silt:` portion must be consistent
    // across all frames — either all absolute (starting with `/`) or
    // all relative (no leading `/`).
    let frame_lines: Vec<&str> = stderr
        .lines()
        .filter(|l| l.trim_start().starts_with("->") && l.contains(".silt:"))
        .collect();
    assert!(
        frame_lines.len() >= 2,
        "expected >= 2 frame lines, got: {stderr}"
    );

    // Extract the "at <path>" portion of each frame line. A consistent
    // style means every extracted path either all starts with `/` or
    // none do.
    let mut absolute_count = 0;
    let mut relative_count = 0;
    for line in &frame_lines {
        let Some(at_idx) = line.find(" at ") else {
            continue;
        };
        let after_at = &line[at_idx + 4..];
        // Trim to just the path portion (before the final `:line:col`).
        // We don't need perfect parsing — just check whether it starts
        // with `/`.
        if after_at.starts_with('/') {
            absolute_count += 1;
        } else {
            relative_count += 1;
        }
    }

    assert!(
        absolute_count == 0 || relative_count == 0,
        "expected consistent path style across frames (all absolute OR all relative), \
         got {absolute_count} absolute and {relative_count} relative frames.\n\
         frame lines:\n{}\n\nfull stderr:\n{stderr}",
        frame_lines.join("\n")
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

// ── Round-19 G1: silt test renders module source for cross-module errors ──

/// When a test imports a module and the runtime error occurs inside that
/// module's code, `silt test` should render the *module's* source line —
/// not a random line from the test file at the module's line number.
///
/// Mutation reasoning: reverting the G1 fix (removing the
/// `collect_module_function_sources` call and the module-aware source
/// lookup in the per-test failure path) makes the assertion for the
/// module source line (`a / b`) fail — the renderer would show whatever
/// line N of the *test* file happens to be, which is wrong.
#[test]
fn test_silt_test_cross_module_error_renders_module_source() {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("silt_r19_g1_{n}"));
    fs::create_dir_all(&dir).unwrap();

    // Module file: calc.silt with a division function
    let calc = dir.join("calc.silt");
    fs::write(&calc, "pub fn divide(a, b) = a / b\n").unwrap();

    // Test file that imports calc and triggers division by zero
    let test_file = dir.join("calc_test.silt");
    fs::write(
        &test_file,
        r#"import calc

fn test_divide_by_zero() {
  let r = calc.divide(10, 0)
  r
}
"#,
    )
    .unwrap();

    let output = silt_cmd()
        .arg("test")
        .arg("calc_test.silt")
        .current_dir(&dir)
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
    // The error snippet must contain the module's source line, not a
    // random test-file line.
    assert!(
        stderr.contains("a / b") || stderr.contains("divide"),
        "expected module source (a / b or divide) in stderr, got: {stderr}"
    );
    // The error path in the snippet should reference the module file,
    // not just the test file.
    assert!(
        stderr.contains("calc.silt"),
        "expected 'calc.silt' in error output, got: {stderr}"
    );
}

// ── Round-19 G2: silt test --filter finds pub fn test_ functions ──────

/// When a test file uses `pub fn test_*` instead of plain `fn test_*`,
/// `silt test --filter` should still find and run those tests. Previously
/// the text-scan pre-filter only tried `strip_prefix("fn ")` and missed
/// `pub fn` variants.
///
/// Mutation reasoning: reverting the G2 fix (removing the
/// `strip_prefix("pub fn ")` branch in the filter text-scan) makes the
/// test fail — the file is skipped entirely because no `fn test_alpha`
/// is found, so 0 tests run instead of 1.
#[test]
fn test_silt_test_filter_finds_pub_fn_tests() {
    let path = temp_silt_file(
        "pub_fn_filter_test",
        r#"import test
pub fn test_alpha() {
  test.assert_eq(1, 1)
}
"#,
    );

    let output = silt_cmd()
        .arg("test")
        .arg(&path)
        .arg("--filter")
        .arg("alpha")
        .output()
        .expect("failed to run silt");

    assert!(
        output.status.success(),
        "expected exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("1 test"),
        "expected '1 test' in output, got: {stderr}"
    );
    assert!(
        stderr.contains("PASS"),
        "expected PASS marker in output, got: {stderr}"
    );
}

// ── Round-19 G3: silt disasm rejects unknown flags ────────────────────

/// `silt disasm --bogus` should fail with an "unknown flag" message
/// instead of silently treating `--bogus` as a filename.
///
/// Mutation reasoning: reverting the G3 fix (removing the unknown-flag
/// validation loop before `disasm_file`) makes the assertion fail — the
/// CLI would try to open a file called `--bogus` instead of reporting
/// the flag error.
#[test]
fn test_disasm_unknown_flag() {
    let path = temp_silt_file(
        "disasm_unk_flag",
        r#"fn main() {
  println("hello")
}
"#,
    );

    let output = silt_cmd()
        .arg("disasm")
        .arg("--bogus")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    assert!(
        !output.status.success(),
        "expected non-zero exit code"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("unknown flag"),
        "expected 'unknown flag' in stderr, got: {stderr}"
    );
    assert!(
        stderr.contains("--bogus"),
        "expected '--bogus' in stderr, got: {stderr}"
    );
    assert!(
        stderr.contains("silt disasm --help"),
        "expected help hint in stderr, got: {stderr}"
    );
}

// ── Round-19 L5: per-test multi-line error indentation ────────────────

/// When `silt test` renders a multi-line SourceError for a failing test,
/// every line of the error (including the `-->` locator, source snippet,
/// and caret) should be indented with 4 spaces — not just the first
/// line.
///
/// Mutation reasoning: reverting the L5 fix (replacing the per-line
/// indent loop with the old `eprintln!("    {source_err}")`) makes the
/// assertion fail — only the first line is indented while subsequent
/// lines start at column 0.
#[test]
fn test_silt_test_multiline_error_indentation() {
    let path = temp_silt_file(
        "multiline_indent_test",
        r#"import test

fn test_division_error() {
  let r = 1 / 0
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

    // Every non-empty line after the "FAIL" line that belongs to the
    // error rendering (up to the summary line) should be indented.
    // Specifically, the `-->`, source line, and `^` caret line must
    // all start with at least 4 spaces of indentation.
    let fail_idx = stderr.find("FAIL").expect("FAIL not found");
    let after_fail = &stderr[fail_idx..];
    let error_lines: Vec<&str> = after_fail
        .lines()
        .skip(1) // skip the FAIL line itself
        .take_while(|l| !l.contains("test:") && !l.contains("tests:"))
        .filter(|l| !l.is_empty())
        .collect();

    assert!(
        !error_lines.is_empty(),
        "expected error lines after FAIL, got nothing"
    );

    for line in &error_lines {
        // Each error-rendering line should be indented by at least 4
        // spaces. We check that the line starts with "    " (4 spaces).
        assert!(
            line.starts_with("    "),
            "expected 4-space indent on error line, got: {:?}\nfull stderr:\n{stderr}",
            line
        );
    }
}
