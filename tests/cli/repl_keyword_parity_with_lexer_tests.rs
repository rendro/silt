//! Parity lock: REPL `builtin_names()` must source language keywords
//! from `lexer::KEYWORDS` and `lexer::KEYWORD_LITERALS` directly, not
//! a hand-rolled parallel array.
//!
//! Round 63 (commit f379b10) collapsed `src/lsp/completion.rs` and
//! `src/lsp/rename.rs` onto the lexer constants but left REPL with a
//! hand-rolled list. The DUP-2 finding from round 64 caught this: if
//! a future PR adds a keyword to `lexer::KEYWORDS`, REPL `<Tab>`
//! completion would silently miss it. These tests ensure that the
//! REPL stays in sync with the lexer's keyword set.

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
