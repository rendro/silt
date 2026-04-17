#![allow(clippy::doc_overindented_list_items)]

//! Round-26 audit regressions for the `silt` CLI, fix agent CLI.
//!
//! Each test here pins one of the gaps called out in round 26:
//!
//! - G1: `silt disasm` usage banner single-source-of-truth
//!       (three drifting banners canonicalized to `Usage: silt disasm [<file.silt>]`
//!       via a `disasm_usage_banner()` helper, mirroring the round-23 pattern for
//!       `silt run`).
//! - G2: top-level `silt --help` rows advertise key subcommand flags
//!       (`check [--format json]`, `test [--filter <pattern>]`, `disasm [--watch]`).
//! - G3: `silt disasm --help` advertises `--watch` in an Options section
//!       (the watch interceptor already supported it; help text just didn't
//!       document it).
//! - G4: `silt foo.silt --help` matches `silt run --help` byte-for-byte
//!       (the legacy convenience path was dropping the Examples section).
//! - G6: `silt add` unknown-flag error includes the `Run 'silt add --help'
//!       for usage.` nudge on a second stderr line, matching every other
//!       subcommand.
//! - L9.1: `enabled_features()` lists `tcp`/`tcp-tls`/`postgres`/`postgres-tls`
//!         when those features are compiled in (at minimum `tcp` in default builds).
//! - L9.2: all 12 top-level help rows have their description columns aligned
//!         at the same byte offset — structural invariant for the FULL help
//!         surface, not just the 6 rows asserted by the round-15 test.

use std::process::Command;

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

/// Compute the byte column where the description word begins in a help row.
/// Each row is `  <signature (padded)>  <desc>`; the description is whatever
/// follows the first run of ≥2 spaces past the signature.
fn desc_column(row: &str) -> Option<usize> {
    let (_lead_ws, body) = row.split_at(row.len() - row.trim_start().len());
    let bytes = body.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b' ' && bytes[i + 1] == b' ' {
            let mut j = i;
            while j < bytes.len() && bytes[j] == b' ' {
                j += 1;
            }
            if j < bytes.len() {
                return Some(row.len() - body.len() + j);
            }
        }
        i += 1;
    }
    None
}

// ── G1: `silt disasm` usage banner single-source-of-truth ───────────

/// `silt disasm` outside a package with no file argument should print
/// the canonical `Usage: silt disasm [<file.silt>]` banner on stderr.
///
/// Mutation reasoning: reverting G1 (restoring the literal
/// `"Usage: silt disasm <file.silt>"` without optional brackets) makes
/// this assertion fail — the bracketed form is the canonical one because
/// the file argument is optional when invoked inside a package.
#[test]
fn test_silt_disasm_no_file_outside_package_prints_canonical_banner() {
    // Run from a tempdir that's definitely NOT inside a silt package,
    // so the no-arg path falls through to the banner fallback.
    let tmp = std::env::temp_dir().join("silt_cli_round26_disasm_no_file");
    std::fs::create_dir_all(&tmp).unwrap();
    let output = silt_cmd()
        .current_dir(&tmp)
        .arg("disasm")
        .output()
        .expect("failed to run silt disasm");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let first_line = stderr.lines().next().unwrap_or("");
    assert_eq!(
        first_line, "Usage: silt disasm [<file.silt>]",
        "expected canonical disasm banner, got stderr: {stderr}"
    );
    assert!(
        !output.status.success(),
        "expected non-zero exit for `silt disasm` with no file outside package"
    );
}

/// `silt disasm --watch` outside a package with no file should print
/// the same canonical banner via the watch dry-validation path.
#[test]
fn test_silt_disasm_watch_no_file_outside_package_prints_canonical_banner() {
    let tmp = std::env::temp_dir().join("silt_cli_round26_disasm_watch_no_file");
    std::fs::create_dir_all(&tmp).unwrap();
    let output = silt_cmd()
        .current_dir(&tmp)
        .args(["disasm", "--watch"])
        .output()
        .expect("failed to run silt disasm --watch");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let first_line = stderr.lines().next().unwrap_or("");
    assert_eq!(
        first_line, "Usage: silt disasm [<file.silt>]",
        "expected canonical disasm banner via watch path, got stderr: {stderr}"
    );
    assert!(
        !output.status.success(),
        "expected non-zero exit for `silt disasm --watch` with no file outside package"
    );
}

/// `silt disasm --help` stdout should contain the same canonical Usage line.
#[test]
fn test_silt_disasm_help_contains_canonical_banner() {
    let output = silt_cmd()
        .args(["disasm", "--help"])
        .output()
        .expect("failed to run silt disasm --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "expected exit 0 for --help");
    assert!(
        stdout
            .lines()
            .any(|l| l == "Usage: silt disasm [<file.silt>]"),
        "expected canonical Usage line in `silt disasm --help`, got:\n{stdout}"
    );
}

/// Top-level `silt --help` row for `disasm` must agree with the canonical
/// banner (both optional-file AND watch flag visible).
#[test]
fn test_silt_top_level_help_disasm_row_matches_banner() {
    let output = silt_cmd()
        .arg("--help")
        .output()
        .expect("failed to run silt --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let disasm_row = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("silt disasm"))
        .unwrap_or_else(|| panic!("no `silt disasm` row in top-level help:\n{stdout}"));
    // The row signature widens for watch advertisement — only require the
    // bracketed optional-file form, matching the canonical banner ending.
    assert!(
        disasm_row.contains("[<file.silt>]"),
        "expected disasm top-level row to show bracketed optional file, got: {disasm_row}"
    );
}

// ── G2: top-level help rows advertise key subcommand flags ──────────

#[test]
fn test_silt_top_level_help_check_row_advertises_format_json() {
    let output = silt_cmd()
        .arg("--help")
        .output()
        .expect("failed to run silt --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let check_row = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("silt check "))
        .unwrap_or_else(|| panic!("no `silt check` row:\n{stdout}"));
    assert!(
        check_row.contains("[--format json]"),
        "expected check row to advertise --format json, got: {check_row}"
    );
    assert!(
        check_row.contains("[--watch]"),
        "expected check row to advertise --watch, got: {check_row}"
    );
}

#[test]
fn test_silt_top_level_help_test_row_advertises_filter() {
    let output = silt_cmd()
        .arg("--help")
        .output()
        .expect("failed to run silt --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let test_row = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("silt test "))
        .unwrap_or_else(|| panic!("no `silt test` row:\n{stdout}"));
    assert!(
        test_row.contains("[--filter <pattern>]"),
        "expected test row to advertise --filter, got: {test_row}"
    );
    assert!(
        test_row.contains("[--watch]"),
        "expected test row to advertise --watch, got: {test_row}"
    );
}

#[test]
fn test_silt_top_level_help_disasm_row_advertises_watch() {
    let output = silt_cmd()
        .arg("--help")
        .output()
        .expect("failed to run silt --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let disasm_row = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("silt disasm"))
        .unwrap_or_else(|| panic!("no `silt disasm` row:\n{stdout}"));
    assert!(
        disasm_row.contains("[--watch]"),
        "expected disasm row to advertise --watch, got: {disasm_row}"
    );
}

// ── G3: `silt disasm --help` documents --watch in Options ───────────

#[test]
fn test_silt_disasm_help_documents_watch_flag() {
    let output = silt_cmd()
        .args(["disasm", "--help"])
        .output()
        .expect("failed to run silt disasm --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    assert!(
        stdout.contains("Options:"),
        "expected `Options:` section in `silt disasm --help`, got:\n{stdout}"
    );
    assert!(
        stdout.contains("--watch"),
        "expected --watch flag documented in `silt disasm --help`, got:\n{stdout}"
    );
}

// ── G4: `silt foo.silt --help` == `silt run --help` ─────────────────

#[test]
fn test_silt_file_help_matches_run_help() {
    let run_help = silt_cmd()
        .args(["run", "--help"])
        .output()
        .expect("failed to run silt run --help");
    let file_help = silt_cmd()
        .args(["foo.silt", "--help"])
        .output()
        .expect("failed to run silt foo.silt --help");
    assert!(run_help.status.success());
    assert!(file_help.status.success());
    let run_stdout = String::from_utf8_lossy(&run_help.stdout).into_owned();
    let file_stdout = String::from_utf8_lossy(&file_help.stdout).into_owned();
    assert_eq!(
        run_stdout, file_stdout,
        "`silt foo.silt --help` must match `silt run --help` byte-for-byte\n\
         run output:\n{run_stdout}\n---\nfile output:\n{file_stdout}"
    );
    // Sanity: the shared text must include the Examples section.
    assert!(
        file_stdout.contains("Examples:"),
        "expected Examples section in shared help text, got:\n{file_stdout}"
    );
}

// ── G6: `silt add` unknown-flag error includes --help nudge ─────────

#[test]
fn test_silt_add_unknown_flag_emits_help_nudge() {
    let output = silt_cmd()
        .args(["add", "foo", "--bogus"])
        .output()
        .expect("failed to run silt add foo --bogus");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let lines: Vec<&str> = stderr.lines().collect();
    assert!(
        lines.len() >= 2,
        "expected at least 2 stderr lines from `silt add foo --bogus`, got:\n{stderr}"
    );
    assert_eq!(
        lines[1], "Run 'silt add --help' for usage.",
        "expected help nudge on second stderr line, got:\n{stderr}"
    );
    assert!(
        !output.status.success(),
        "expected non-zero exit for unknown flag"
    );
}

// ── L9.1: enabled_features() lists tcp/tcp-tls/postgres/postgres-tls ─

/// Default builds compile `tcp` in (see Cargo.toml `default = [...]`).
/// The `Enabled features:` line in `silt --help` must include it.
#[test]
fn test_silt_help_enabled_features_lists_tcp_in_default_build() {
    let output = silt_cmd()
        .arg("--help")
        .output()
        .expect("failed to run silt --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let feat_line = stdout
        .lines()
        .find(|l| l.starts_with("Enabled features:"))
        .unwrap_or_else(|| panic!("no `Enabled features:` line in:\n{stdout}"));
    // Strip the prefix and parse the comma-separated list.
    let list = feat_line
        .strip_prefix("Enabled features:")
        .unwrap_or("")
        .trim();
    let feats: Vec<&str> = list.split(',').map(|s| s.trim()).collect();
    assert!(
        feats.contains(&"tcp"),
        "expected `tcp` in default-build Enabled features line, got: {feat_line}"
    );
}

/// When the crate is compiled with a given feature flag enabled, the
/// `Enabled features:` line must list it. The default build has `tcp`
/// but not the other three — we guard those with `cfg!` so the test
/// statically matches what the binary exposes.
#[test]
fn test_silt_help_enabled_features_lists_compiled_features() {
    let output = silt_cmd()
        .arg("--help")
        .output()
        .expect("failed to run silt --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let feat_line = stdout
        .lines()
        .find(|l| l.starts_with("Enabled features:"))
        .unwrap_or_else(|| panic!("no `Enabled features:` line in:\n{stdout}"));

    if cfg!(feature = "tcp-tls") {
        assert!(
            feat_line.contains("tcp-tls"),
            "expected `tcp-tls` in features line, got: {feat_line}"
        );
    }
    if cfg!(feature = "postgres") {
        assert!(
            feat_line.contains("postgres"),
            "expected `postgres` in features line, got: {feat_line}"
        );
    }
    if cfg!(feature = "postgres-tls") {
        assert!(
            feat_line.contains("postgres-tls"),
            "expected `postgres-tls` in features line, got: {feat_line}"
        );
    }
}

// ── L9.2: all 12 top-level help rows align at the same desc column ──

/// Parse every row from `silt --help` whose trimmed form starts with
/// `silt ` and assert they all have their description column at the
/// same byte offset. Round-25 locked this for 6 of 12 rows via
/// tests/cli_test_rendering_tests.rs; this lock extends the invariant
/// to the FULL help surface (all subcommand rows).
#[test]
fn test_silt_help_all_rows_desc_column_aligned() {
    let output = silt_cmd()
        .arg("--help")
        .output()
        .expect("failed to run silt --help");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);

    // The first `silt — ...` title line also passes `starts_with("silt ")`
    // after trim, so we require two leading spaces AND a description column
    // (≥2 consecutive spaces after the signature) to isolate real rows.
    let rows: Vec<&str> = stdout
        .lines()
        .filter(|l| l.starts_with("  silt ") && desc_column(l).is_some())
        .collect();
    // 12 rows: run, check, test, fmt, repl, init, lsp, disasm, self-update,
    // update, add --path, add --git.
    assert_eq!(
        rows.len(),
        12,
        "expected 12 subcommand rows in top-level help, got {}:\n{stdout}",
        rows.len()
    );

    let cols: Vec<(usize, &str)> = rows
        .iter()
        .map(|r| {
            let c = desc_column(r).unwrap_or_else(|| panic!("no description column for row: {r}"));
            (c, *r)
        })
        .collect();
    let (first_col, first_row) = cols[0];
    for (col, row) in &cols[1..] {
        assert_eq!(
            *col, first_col,
            "desc column mismatch — row 0 column {first_col} vs this row's column {col}\n\
             row 0: {first_row}\n\
             mismatch row: {row}"
        );
    }
}
