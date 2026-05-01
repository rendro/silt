//! Round-64 BROKEN doc-finding lock.
//!
//! `docs/strict-effects-migration.md` previously quoted the effect set
//! inside the `error[type]:` example like `effect '!{fs, io}'`. The
//! actual diagnostic at `src/typechecker/inference.rs:845` emits
//! `effect !{fs, io}` (no surrounding `'…'` quotes). The mismatch
//! defeats copy-paste search of the diagnostic text.
//!
//! These two locks double-gate the regression:
//!
//! 1. SOURCE-LEVEL grep: the migration doc string never contains
//!    `effect '!{` (the broken form). This is the audit-mandated
//!    weak-gate replacement — a "construct and format" test would not
//!    catch a doc revert.
//! 2. RUNTIME end-to-end: a real `silt check --strict-effects` run
//!    on a small fixture emits stderr that does NOT contain
//!    `effect '!{` and DOES contain `effect !{`. Catches the
//!    diagnostic itself drifting back to the quoted form.

use std::process::Command;

const DOC: &str = include_str!("../docs/strict-effects-migration.md");

#[test]
fn migration_doc_does_not_quote_effect_set_inline() {
    assert!(
        !DOC.contains("effect '!{"),
        "docs/strict-effects-migration.md must not quote the effect set in error examples; \
         the actual diagnostic emits `effect !{{...}}` without surrounding quotes \
         (see src/typechecker/inference.rs:845)"
    );
}

#[test]
fn migration_doc_uses_unquoted_effect_set_form() {
    // Positive shape: the doc still contains the unquoted form
    // somewhere (otherwise the grep above would silently pass after a
    // wholesale deletion of the example).
    assert!(
        DOC.contains("effect !{fs, io}"),
        "docs/strict-effects-migration.md should still contain at least one \
         `effect !{{fs, io}}` example to mirror the actual diagnostic format"
    );
}

#[test]
fn strict_effects_diagnostic_emits_unquoted_effect_set() {
    // End-to-end runtime lock: the diagnostic itself uses the
    // unquoted form. This is the second gate — catches the source
    // formatter drifting even if the doc stays in sync.
    let dir = std::env::temp_dir().join("silt_docs_strict_effects_quoted_form");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let main = dir.join("main.silt");
    std::fs::write(
        &main,
        "import io\n\
         fn load_settings(path: String) -> Result(String, IoError) =\n\
         \x20\x20io.read_file(path)\n\
         fn main() {\n\
         \x20\x20let _settings = load_settings(\"config.toml\")\n\
         \x20\x20()\n\
         }\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_silt"))
        .args(["check", "--strict-effects", main.to_str().unwrap()])
        .output()
        .expect("failed to run silt check");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !stderr.contains("effect '!{"),
        "strict-effects diagnostic must not surround the effect set with `'…'`; got stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("effect !{"),
        "strict-effects diagnostic must contain unquoted `effect !{{...}}` form; got stderr:\n{stderr}"
    );
}

/// Round-?? LATENT D3 lock.
///
/// Earlier the migration doc rendered the `error[type]: ...` block
/// with a `^^^^^^^^^^^...` row that spanned the entire `fn` line.
/// `silt check --strict-effects` actually emits a single `^` caret
/// at the offending column. Cosmetic but the document presents it
/// as literal CLI output, so a reader who copies the example into a
/// bug report misrepresents the tool.
///
/// This test re-renders the doc's first error block from a real
/// fixture (`load_settings` with `io.read_file` on the same line as
/// the `fn` header — exactly what the doc shows on lines 38-45) and
/// asserts the doc's caret row contains exactly one `^` character —
/// matching the actual diagnostic. Stronger lock: the body of the
/// caret row is asserted byte-equal to the diagnostic's caret row
/// after stripping the leading source-path-dependent column number.
#[test]
fn migration_doc_error_block_matches_actual_diagnostic_shape() {
    // 1. Render the actual diagnostic from a fixture that mirrors
    //    the doc's first quoted error block.
    let dir = std::env::temp_dir().join("silt_docs_strict_effects_caret_shape");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let main = dir.join("main.silt");
    std::fs::write(
        &main,
        "import io\n\
         \n\
         fn load_settings(path: String) -> Result(String, IoError) = io.read_file(path)\n\
         \n\
         fn main() {\n\
         \x20\x20let _settings = load_settings(\"config.toml\")\n\
         \x20\x20()\n\
         }\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_silt"))
        .args(["check", "--strict-effects", main.to_str().unwrap()])
        .output()
        .expect("failed to run silt check");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    // 2. Extract the strict-effects diagnostic block from stderr.
    //    It begins with the `error[type]: function 'load_settings' …`
    //    line and runs until the next blank line or end of stderr.
    let stderr_block = extract_strict_effects_block(&stderr).unwrap_or_else(|| {
        panic!("could not find strict-effects error block in stderr:\n{stderr}")
    });

    // 3. Pull the caret line out of the actual diagnostic. Format:
    //       "   |   ^ function 'load_settings' …"
    let stderr_caret_line = stderr_block
        .lines()
        .find(|l| {
            let trimmed = l.trim_start();
            trimmed.starts_with('|')
                && trimmed[1..].trim_start().starts_with('^')
        })
        .unwrap_or_else(|| {
            panic!("no caret line in stderr block:\n{stderr_block}")
        });
    let stderr_caret_count = stderr_caret_line.chars().filter(|&c| c == '^').count();

    // 4. Pull the doc's first `error[type]:` block. Same heuristic:
    //    starts at the `error[type]:` line, runs to the next ``` fence.
    let doc_block = extract_doc_first_error_block(DOC).unwrap_or_else(|| {
        panic!("could not find error[type] block in migration doc")
    });
    let doc_caret_line = doc_block
        .lines()
        .find(|l| {
            let trimmed = l.trim_start();
            trimmed.starts_with('|')
                && trimmed[1..].trim_start().starts_with('^')
        })
        .unwrap_or_else(|| {
            panic!("no caret line in migration doc error block:\n{doc_block}")
        });
    let doc_caret_count = doc_caret_line.chars().filter(|&c| c == '^').count();

    // 5. Structural lock: the doc must show exactly as many `^` as
    //    the actual diagnostic. Catches the old long-row form which
    //    had ~70+ carets while the real output has 1.
    assert_eq!(
        doc_caret_count, stderr_caret_count,
        "caret-character count mismatch:\n  doc caret line:    {doc_caret_line:?}\n  stderr caret line: {stderr_caret_line:?}\n  doc has {doc_caret_count} `^`, stderr has {stderr_caret_count}\n  full stderr block:\n{stderr_block}"
    );

    // 6. Stronger lock: the doc's diagnostic body (drop the path-
    //    dependent `--> main.silt:N:M` line, since the actual run
    //    uses an absolute tempdir path) must appear verbatim in the
    //    actual stderr.
    let doc_signal_lines: Vec<&str> = doc_block
        .lines()
        .filter(|l| !l.trim_start().starts_with("--> "))
        .collect();
    for line in &doc_signal_lines {
        // Skip empty surrounds.
        if line.trim().is_empty() {
            continue;
        }
        assert!(
            stderr.contains(line),
            "doc error-block line not present in actual stderr:\n  doc line: {line:?}\n  stderr:\n{stderr}"
        );
    }
}

/// Pulls the strict-effects `error[type]:` block out of stderr —
/// from the `error[type]: function '…' uses effect …` line through
/// the trailing `= help:` line.
fn extract_strict_effects_block(stderr: &str) -> Option<String> {
    let lines: Vec<&str> = stderr.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.starts_with("error[type]:") && l.contains("uses effect"))?;
    let mut end = start;
    while end < lines.len() {
        let l = lines[end];
        if l.is_empty() && end > start {
            break;
        }
        end += 1;
    }
    Some(lines[start..end].join("\n"))
}

/// Pulls the first ```-fenced block in the doc whose first content
/// line begins with `error[type]:`.
fn extract_doc_first_error_block(doc: &str) -> Option<String> {
    let lines: Vec<&str> = doc.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() == "```" {
            // Find next fence.
            let body_start = i + 1;
            let mut j = body_start;
            while j < lines.len() && lines[j].trim() != "```" {
                j += 1;
            }
            if body_start < lines.len()
                && lines[body_start].starts_with("error[type]:")
            {
                return Some(lines[body_start..j].join("\n"));
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }
    None
}
