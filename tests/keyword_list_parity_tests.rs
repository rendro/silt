//! Round-60 G3 regression, updated round-63 L1: lock the silt keyword
//! list across surfaces that consume it.
//!
//! Authoritative source: `src/lexer.rs::KEYWORDS` (+ `KEYWORD_LITERALS`
//! for `true`/`false`). Round-63 collapsed the previously-hand-rolled
//! arrays in `src/lsp/completion.rs` and `src/lsp/rename.rs` into
//! references to the lexer constants. The structural drift check for
//! those LSP files now lives in `tests/lexer_keyword_parity_tests.rs`
//! (it source-greps for re-introduction of the hand-rolled head).
//!
//! What remains here:
//!
//!   * `lexer::KEYWORDS` itself contains every core keyword. Belt-and-
//!     braces: if someone trims the const we still notice.
//!
//!   * `src/repl.rs` still inlines its own keyword list (REPL completion
//!     surface, not in scope of round-63 L1). It must include the 14
//!     core keywords plus `true`/`false`.
//!
//! Authoritative shape:
//!   * If you add a new keyword token to the lexer, add it to
//!     `lexer::KEYWORDS`, mirror it into `src/repl.rs`, and update
//!     `EXPECTED_CORE_KEYWORDS` here.

use std::fs;
use std::path::PathBuf;

const EXPECTED_CORE_KEYWORDS: &[&str] = &[
    "as", "else", "fn", "import", "let", "loop", "match", "mod", "pub", "return", "trait", "type",
    "when", "where",
];

fn read_source(rel: &str) -> String {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Whole-word presence check: `name` must appear as a string literal
/// `"name"` in `src`. We require the surrounding quote to avoid false
/// positives from comments or other identifiers.
fn source_mentions_quoted(src: &str, name: &str) -> bool {
    let needle = format!("\"{name}\"");
    src.contains(&needle)
}

#[test]
fn lexer_keywords_const_contains_core_set() {
    // After round-63 L1, the LSP modules consume `lexer::KEYWORDS`
    // directly — checking the source files for quoted strings is no
    // longer meaningful (they don't appear there anymore). Instead we
    // assert that the authoritative const itself contains the core set.
    for kw in EXPECTED_CORE_KEYWORDS {
        assert!(
            silt::lexer::KEYWORDS.contains(kw),
            "lexer::KEYWORDS missing core keyword `\"{kw}\"`. \
             If a keyword was removed from the lexer's match arms, also \
             remove the EXPECTED_CORE_KEYWORDS entry. If the const \
             drifted from the match arms, the parity-lock in \
             tests/lexer_keyword_parity_tests.rs catches that."
        );
    }
}

#[test]
fn lexer_keyword_literals_const_contains_true_false() {
    // The bool-literal split (round-63): `true`/`false` live in their
    // own const because the lexer emits them as `Token::Bool(_)`, not
    // as keyword tokens. Rename consults BOTH constants so renaming a
    // binding to `true`/`false` is rejected.
    assert!(
        silt::lexer::KEYWORD_LITERALS.contains(&"true"),
        "lexer::KEYWORD_LITERALS must include \"true\""
    );
    assert!(
        silt::lexer::KEYWORD_LITERALS.contains(&"false"),
        "lexer::KEYWORD_LITERALS must include \"false\""
    );
}

#[test]
fn repl_keywords_contain_core_set() {
    let src = read_source("src/repl.rs");
    for kw in EXPECTED_CORE_KEYWORDS {
        assert!(
            source_mentions_quoted(&src, kw),
            "src/repl.rs keyword list missing core keyword `\"{kw}\"`. \
             If a new keyword was added, also update lexer::KEYWORDS \
             and EXPECTED_CORE_KEYWORDS here."
        );
    }
    // Intentional delta: REPL keyword list also includes `true`/`false`
    // as completion entries (CONSTANT-shaped, not KEYWORD-shaped).
    assert!(
        source_mentions_quoted(&src, "true"),
        "src/repl.rs keyword list must include \"true\" as completion entry"
    );
    assert!(
        source_mentions_quoted(&src, "false"),
        "src/repl.rs keyword list must include \"false\" as completion entry"
    );
}
