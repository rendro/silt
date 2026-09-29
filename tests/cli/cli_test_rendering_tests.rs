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
//! Every case except G10 is a golden case under
//! `tests/golden/cli/rendering/`; the help-row alignment check stays here.

use std::process::Command;

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
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
    assert!(
        rows.len() >= 5,
        "expected several usage rows, got: {stdout}"
    );

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
        rows.iter()
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
