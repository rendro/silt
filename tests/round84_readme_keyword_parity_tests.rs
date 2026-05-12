//! Round 84 parity lock: the keyword and keyword-literal lists in
//! `README.md` (the "Reference" table) must match the canonical
//! constants in `src/lexer.rs` — `KEYWORDS` and `KEYWORD_LITERALS`.
//!
//! Without this lock, adding a new keyword to the lexer would silently
//! leave README out of date; equally, removing or renaming one would
//! orphan a phantom mention in README. Per silt's "one way to do
//! things" design rule, the canonical source is the lexer constants,
//! and the doc is the mirror.
//!
//! The README row looks like (line ~200):
//!   | **keywords** | `as else fn import let loop match mod pub return trait type when where` |
//!
//! Strategy: include the README at compile time, scan for the
//! `**keywords**` table row, extract the backtick-quoted body, split
//! on whitespace, and compare as a set against `silt::lexer::KEYWORDS`.
//! Also lock the boolean literals (`true`, `false`) referenced
//! elsewhere in README/syntax docs — they live in
//! `silt::lexer::KEYWORD_LITERALS`. We don't currently expose them in
//! README's Reference table, but we still assert the constant is
//! exactly `{"true", "false"}` so the LSP gating logic in
//! `src/lsp/rename.rs` stays in sync.

use std::collections::BTreeSet;

use silt::lexer::{KEYWORD_LITERALS, KEYWORDS};

const README: &str = include_str!("../README.md");

/// Extract the backtick-quoted content of the `**keywords**` row in
/// the Reference table. Returns the inner string, e.g.
/// `"as else fn import let loop match mod pub return trait type when where"`.
fn extract_readme_keyword_row() -> String {
    for line in README.lines() {
        let trimmed = line.trim_start();
        // Match the row whose label cell is exactly `| **keywords** |`.
        if !trimmed.starts_with("| **keywords**") {
            continue;
        }
        // The row body is the second cell, wrapped in single backticks.
        // Format: `| **keywords** | `<words>` |`
        let start = trimmed
            .find('`')
            .expect("keywords row should have a backtick");
        let after = &trimmed[start + 1..];
        let end = after
            .find('`')
            .expect("keywords row should have a closing backtick");
        return after[..end].to_string();
    }
    panic!("README.md did not contain a `**keywords**` row in the Reference table");
}

#[test]
fn round84_readme_keyword_row_matches_lexer_keywords() {
    let row = extract_readme_keyword_row();
    let doc_set: BTreeSet<&str> = row.split_ascii_whitespace().collect();
    let src_set: BTreeSet<&str> = KEYWORDS.iter().copied().collect();
    assert_eq!(
        doc_set, src_set,
        "README.md `**keywords**` row drifted from `silt::lexer::KEYWORDS`. \
         README has: {doc_set:?}, lexer has: {src_set:?}. \
         The lexer is canonical; update README to match."
    );
}

/// The doc row must not contain any boolean-literal-shaped tokens
/// (`true` / `false`). Those live in `KEYWORD_LITERALS` because they
/// lex to `Token::Bool(_)` rather than a keyword token. Mixing them
/// into the README keyword row would suggest they're keyword-shaped,
/// which is wrong.
#[test]
fn round84_readme_keyword_row_excludes_keyword_literals() {
    let row = extract_readme_keyword_row();
    let doc_set: BTreeSet<&str> = row.split_ascii_whitespace().collect();
    for lit in KEYWORD_LITERALS {
        assert!(
            !doc_set.contains(lit),
            "README `**keywords**` row contains {lit:?}, which is a \
             `KEYWORD_LITERALS` member (Token::Bool), not a keyword. \
             Remove it from the README keyword row; document it under \
             literals if a row is desired."
        );
    }
}

/// Lock `KEYWORD_LITERALS` itself: it must be exactly `{"true",
/// "false"}`. The LSP rename gate (`src/lsp/rename.rs`) and the
/// completion gate consult both `KEYWORDS` and `KEYWORD_LITERALS`,
/// so adding a third literal here without auditing those sites would
/// be a silent bug. If a future patch genuinely needs to add one,
/// fix the gates first and then update this expected set.
#[test]
fn round84_keyword_literals_exactly_true_and_false() {
    let actual: BTreeSet<&str> = KEYWORD_LITERALS.iter().copied().collect();
    let expected: BTreeSet<&str> = ["true", "false"].into_iter().collect();
    assert_eq!(
        actual, expected,
        "`KEYWORD_LITERALS` drifted from the expected {{\"true\", \"false\"}} set; \
         audit LSP rename/completion gates that consult this constant \
         before changing the expectation."
    );
}
