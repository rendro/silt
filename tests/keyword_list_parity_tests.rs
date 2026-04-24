//! Round-60 G3 regression: lock the silt keyword list across the
//! three source surfaces that hardcode their own copies.
//!
//! The three lists, with their owners and known intentional deltas:
//!
//!   * `src/lsp/completion.rs::KEYWORDS` (14 items)
//!     — drives LSP completion. Excludes `true`/`false` because those
//!       are emitted as CONSTANT items (not KEYWORD items) elsewhere
//!       in the same file.
//!
//!   * `src/repl.rs` inlined keyword list (14 + `true` + `false`,
//!      plus REPL meta-commands `:quit` / `:help`)
//!     — drives REPL completion. Includes `true`/`false` as keyword-
//!       like completion entries.
//!
//!   * `src/lsp/rename.rs::SILT_KEYWORDS` (16 items)
//!     — drives the user-renameable check. Includes `true`/`false`
//!       so renaming a binding to `true`/`false` is rejected.
//!
//! The 14 *core* keywords must appear in all three lists. Drift on the
//! core set silently breaks parity (e.g. adding a new keyword to the
//! lexer but forgetting one of the three surfaces). This test asserts
//! the intersection is at least the expected core set.
//!
//! Authoritative shape:
//!   * If you add a new keyword token to the lexer, add it to ALL THREE
//!     source files above and update `EXPECTED_CORE_KEYWORDS` here.
//!   * `true`/`false` differences are intentional (see deltas above).

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
fn lsp_completion_keywords_contain_core_set() {
    let src = read_source("src/lsp/completion.rs");
    for kw in EXPECTED_CORE_KEYWORDS {
        assert!(
            source_mentions_quoted(&src, kw),
            "src/lsp/completion.rs::KEYWORDS missing core keyword `\"{kw}\"`. \
             If a new keyword was added, also update src/repl.rs and \
             src/lsp/rename.rs::SILT_KEYWORDS, then add it to \
             tests/keyword_list_parity_tests.rs::EXPECTED_CORE_KEYWORDS."
        );
    }
}

#[test]
fn lsp_rename_keywords_contain_core_set() {
    let src = read_source("src/lsp/rename.rs");
    for kw in EXPECTED_CORE_KEYWORDS {
        assert!(
            source_mentions_quoted(&src, kw),
            "src/lsp/rename.rs::SILT_KEYWORDS missing core keyword `\"{kw}\"`. \
             If a new keyword was added, also update src/lsp/completion.rs and \
             src/repl.rs, then add it to \
             tests/keyword_list_parity_tests.rs::EXPECTED_CORE_KEYWORDS."
        );
    }
    // Intentional delta: rename.rs MUST include `true`/`false` so
    // renaming a binding to `true`/`false` is rejected.
    assert!(
        source_mentions_quoted(&src, "true"),
        "src/lsp/rename.rs::SILT_KEYWORDS must include \"true\" so rename to a bool-literal name is rejected"
    );
    assert!(
        source_mentions_quoted(&src, "false"),
        "src/lsp/rename.rs::SILT_KEYWORDS must include \"false\" so rename to a bool-literal name is rejected"
    );
}

#[test]
fn repl_keywords_contain_core_set() {
    let src = read_source("src/repl.rs");
    for kw in EXPECTED_CORE_KEYWORDS {
        assert!(
            source_mentions_quoted(&src, kw),
            "src/repl.rs keyword list missing core keyword `\"{kw}\"`. \
             If a new keyword was added, also update src/lsp/completion.rs \
             and src/lsp/rename.rs::SILT_KEYWORDS, then add it to \
             tests/keyword_list_parity_tests.rs::EXPECTED_CORE_KEYWORDS."
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
