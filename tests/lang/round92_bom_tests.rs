//! Regression tests for UTF-8 BOM handling (audit round 92).
//!
//! A file starting with the UTF-8 BOM (EF BB BF, U+FEFF) — the default
//! for Windows Notepad and PowerShell `>` redirects — used to hit the
//! lexer's generic catch-all and render
//! `error[lex]: unexpected character: ''` with a caret at 1:1, because
//! U+FEFF is zero-width and invisible.
//!
//! The fix skips a single *leading* BOM inside `Lexer::new` (the same
//! policy as rustc), which covers run/check/fmt/test/REPL/LSP uniformly
//! since every surface lexes through `Lexer::new(silt::source::FileId::default(), ...).tokenize()`. The
//! BOM is skipped, not stripped: byte offsets and line/col stay relative
//! to the original source string, with the BOM counting as the first
//! column of line 1. A BOM anywhere *else* in the file is still an
//! error, now reported by name (`byte-order mark (U+FEFF)`) instead of
//! as an invisible quoted character.
//!
//! Most cases are golden cases in tests/golden/lang/lexer/round92_bom__*.silt.
//! The two left here cannot be: the line-1 error must sit right after the
//! BOM, leaving no room for directive lines, and the fmt test checks the
//! rewritten file's contents.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const BOM: &str = "\u{FEFF}";

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

fn temp_silt_file(prefix: &str, content: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join("silt_round92_bom_tests");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{prefix}_{n}.silt"));
    fs::write(&path, content).unwrap();
    path
}

fn run_cmd(cmd: &mut Command) -> (i32, String, String) {
    let output = cmd.output().expect("failed to run silt");
    let code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    (code, stdout, stderr)
}

// ── Spans after a skipped BOM stay correct ────────────────────────────

#[test]
fn bom_then_error_on_line_one_points_past_the_bom() {
    // The BOM is *skipped*, not stripped, so it still counts as the
    // first character (column 1) of line 1; the offending `;` is the
    // second character. The locator must therefore read 1:2 — pointing
    // at the first real character — not 1:1 (the invisible BOM).
    let path = temp_silt_file("bom_line1_error", &format!("{BOM};\n"));

    let (code, _stdout, stderr) = run_cmd(silt_cmd().arg("run").arg(&path));

    assert_ne!(code, 0, "expected the lex error to fail the run");
    assert!(
        stderr.contains("semicolons are not used"),
        "expected the semicolon diagnostic, got stderr:\n{stderr}"
    );
    assert!(
        stderr.contains(":1:2"),
        "expected the error located at line 1, col 2 (after the skipped BOM), got stderr:\n{stderr}"
    );
}

// ── (d) fmt on a BOM file: clean round-trip, BOM dropped ──────────────

#[test]
fn fmt_on_bom_file_drops_bom_without_corruption() {
    let body = "fn main() {\n  println(\"fmt bom\")\n}\n";
    let path = temp_silt_file("bom_fmt", &format!("{BOM}{body}"));

    let (code, _stdout, stderr) = run_cmd(silt_cmd().arg("fmt").arg(&path));
    assert_eq!(
        code, 0,
        "expected fmt to succeed on a BOM-prefixed file, got exit {code}, stderr:\n{stderr}"
    );

    let after = fs::read_to_string(&path).unwrap();
    assert!(
        !after.starts_with(BOM),
        "fmt re-emits from the token stream, so the BOM must be dropped; got:\n{after:?}"
    );
    assert_eq!(
        after, body,
        "fmt must not corrupt the program while dropping the BOM"
    );

    // The reformatted file still runs.
    let (code, stdout, stderr) = run_cmd(silt_cmd().arg("run").arg(&path));
    assert_eq!(
        code, 0,
        "expected formatted file to run, got exit {code}, stderr:\n{stderr}"
    );
    assert_eq!(stdout, "fmt bom\n");
}
