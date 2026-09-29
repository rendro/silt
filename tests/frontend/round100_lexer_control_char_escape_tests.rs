//! Round-100 MINOR (consistency): the lexer's catch-all
//! "unexpected character" error embedded a RAW control byte between the
//! quotes (`unexpected character: '<0x01>'`), rendering an empty-looking
//! error in any non-`cat -v` sink (log files, LSP JSON diagnostics,
//! captured test output).
//!
//! The codebase already established the "name invisible characters
//! instead of quoting the raw byte" policy for the BOM (U+FEFF, round 92)
//! — but applied it to only that one invisible char. ASCII/Unicode
//! control chars are equally invisible. Fix: the catch-all arm now
//! escapes control characters via `char::escape_default` (`\u{1}`, `\t`,
//! …); printable chars like `@` keep their plain quoted form.
//!
//! Locked through the compiled binary so the full lex error-render path
//! is exercised.

use std::process::Command;

fn check_stderr(label: &str, src: &str) -> String {
    let tmp = std::env::temp_dir().join(format!("silt_r100_ctrl_{label}.silt"));
    std::fs::write(&tmp, src).expect("write temp file");
    let out = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("check")
        .arg(&tmp)
        .output()
        .expect("spawn silt check");
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn control_character_is_escaped_not_raw() {
    // A U+0001 in source must surface as the escaped form, and the raw
    // control byte must NOT appear in the message.
    let src = "fn main() {\n  let x = \u{1}\n}\n";
    let stderr = check_stderr("u1", src);
    assert!(
        stderr.contains("unexpected character: '\\u{1}'"),
        "control char must be escaped to `\\u{{1}}`; got:\n{stderr}"
    );
    // The MESSAGE line itself must carry the escaped form, not the raw
    // byte. (The renderer's source-line snippet below the message still
    // echoes the offending line verbatim, which is a separate concern.)
    let message_line = stderr
        .lines()
        .find(|l| l.contains("unexpected character:"))
        .expect("a message line");
    assert!(
        !message_line.contains('\u{1}'),
        "the raw U+0001 control byte must NOT appear in the error message line; \
         got: {message_line:?}"
    );
}

#[test]
fn printable_character_keeps_plain_quoted_form() {
    // `@` is not a control char — it must stay plainly quoted (no escape),
    // matching the pre-existing behaviour.
    let src = "fn main() {\n  let x = @\n}\n";
    let stderr = check_stderr("at", src);
    assert!(
        stderr.contains("unexpected character: '@'"),
        "a printable char must keep its plain quoted form; got:\n{stderr}"
    );
}
