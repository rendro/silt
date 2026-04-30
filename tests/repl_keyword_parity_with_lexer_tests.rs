//! Parity lock: REPL `builtin_names()` must source language keywords
//! from `lexer::KEYWORDS` and `lexer::KEYWORD_LITERALS` directly, not
//! a hand-rolled parallel array.
//!
//! Round 63 (commit f379b10) collapsed `src/lsp/completion.rs` and
//! `src/lsp/rename.rs` onto the lexer constants but left REPL with a
//! hand-rolled list. The DUP-2 finding from round 64 caught this: if
//! a future PR adds a keyword to `lexer::KEYWORDS`, REPL `<Tab>`
//! completion would silently miss it. These tests ensure that the
//! REPL stays in sync with the lexer's keyword set, and that the
//! source no longer carries a hand-rolled keyword array literal.

#[test]
fn repl_builtin_names_is_superset_of_lexer_keywords() {
    let names: std::collections::HashSet<String> =
        silt::repl::builtin_names().into_iter().collect();
    for kw in silt::lexer::KEYWORDS {
        assert!(
            names.contains(*kw),
            "REPL builtin_names missing lexer keyword `{kw}` — \
             repl.rs must consume lexer::KEYWORDS directly"
        );
    }
    for kw in silt::lexer::KEYWORD_LITERALS {
        assert!(
            names.contains(*kw),
            "REPL builtin_names missing lexer keyword literal `{kw}`"
        );
    }
}

#[test]
fn repl_includes_short_commands() {
    let names: std::collections::HashSet<String> =
        silt::repl::builtin_names().into_iter().collect();
    for short in &[":quit", ":q", ":help", ":h"] {
        assert!(names.contains(*short));
    }
}

#[test]
fn repl_source_does_not_contain_handrolled_keyword_array() {
    // Source-grep guard: ensure the specific old hand-rolled keyword
    // array literal that DUP-2 flagged has not been re-introduced.
    // Phrase chosen to be distinctive enough not to false-positive
    // against incidental occurrences.
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/repl.rs",
    ))
    .expect("read src/repl.rs");
    assert!(
        !src.contains(r#""as", "else", "fn", "import","#),
        "src/repl.rs has re-introduced a hand-rolled keyword list — \
         REPL must consume lexer::KEYWORDS directly (round-64 DUP-2)"
    );
}
