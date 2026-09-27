//! Round-101 GAP: the `unknown escape sequence: \<c>` error in
//! `scan_string` embedded the RAW control byte after the backslash —
//! the missed sibling of the round-100 fix to the `unexpected
//! character` catch-all (tests/round100_lexer_control_char_escape_tests.rs).
//!
//! A backslash followed by CR (CRLF file with the backslash at end of
//! line) returned the terminal cursor to column 0 mid-message, garbling
//! the diagnostic; backslash + U+0001 rendered an invisible offender.
//! Fix: the unknown-escape arm now renders control characters via
//! `char::escape_default` (`\r`, `\u{1}`, `\t`, …). Printable unknown
//! escapes like `\q` keep their plain form, matching the round-100
//! precedent.
//!
//! Locked through the compiled binary so the full lex error-render path
//! is exercised.

use std::process::Command;

fn check_stderr(label: &str, src: &str) -> String {
    let tmp = std::env::temp_dir().join(format!("silt_r101_esc_ctrl_{label}.silt"));
    std::fs::write(&tmp, src).expect("write temp file");
    let out = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("check")
        .arg(&tmp)
        .output()
        .expect("spawn silt check");
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The `unknown escape sequence:` message line, without the renderer's
/// source-line snippet below it (the snippet echoes the offending line
/// verbatim, which is a separate concern — same carve-out as round 100).
fn message_line(stderr: &str) -> String {
    stderr
        .lines()
        .find(|l| l.contains("unknown escape sequence:"))
        .unwrap_or_else(|| panic!("no `unknown escape sequence:` line in:\n{stderr}"))
        .to_owned()
}

#[test]
fn escaped_u0001_is_named_not_raw() {
    // String containing backslash + U+0001.
    let src = "fn main() {\n  let s = \"a\\\u{1}b\"\n}\n";
    let stderr = check_stderr("u1", src);
    assert!(
        stderr.contains("unknown escape sequence: \\\\u{1}"),
        "control char after backslash must surface as `\\u{{1}}`; got:\n{stderr}"
    );
    let line = message_line(&stderr);
    assert!(
        !line.contains('\u{1}'),
        "the raw U+0001 byte must NOT appear in the message line; got: {line:?}"
    );
}

#[test]
fn escaped_carriage_return_is_named_not_raw() {
    // Backslash at end of a CRLF line: backslash + CR + LF.
    let src = "fn main() {\n  let s = \"abc\\\r\n}\n";
    let stderr = check_stderr("cr", src);
    assert!(
        stderr.contains("unknown escape sequence: \\\\r"),
        "CR after backslash must surface as `\\r`; got:\n{stderr}"
    );
    let line = message_line(&stderr);
    assert!(
        !line.contains('\r'),
        "the raw CR byte must NOT appear in the message line (it returns \
         the terminal cursor to column 0); got: {line:?}"
    );
}

#[test]
fn escaped_tab_is_named_not_raw() {
    // Backslash + literal TAB character.
    let src = "fn main() {\n  let s = \"a\\\tb\"\n}\n";
    let stderr = check_stderr("tab", src);
    assert!(
        stderr.contains("unknown escape sequence: \\\\t"),
        "TAB after backslash must surface as `\\t`; got:\n{stderr}"
    );
    let line = message_line(&stderr);
    assert!(
        !line.contains('\t'),
        "the raw TAB byte must NOT appear in the message line; got: {line:?}"
    );
}

#[test]
fn printable_unknown_escape_keeps_plain_form() {
    // `q` is not a control char — `\q` must keep its plain rendering,
    // with a single backslash (no escape_default doubling).
    let src = "fn main() {\n  let s = \"a\\qb\"\n}\n";
    let stderr = check_stderr("q", src);
    assert!(
        stderr.contains("unknown escape sequence: \\q"),
        "a printable unknown escape must keep its plain form; got:\n{stderr}"
    );
    assert!(
        !stderr.contains("unknown escape sequence: \\\\q"),
        "printable unknown escapes must NOT be escape_default-doubled; got:\n{stderr}"
    );
}
