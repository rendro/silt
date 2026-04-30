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

const EXPECTED_CORE_KEYWORDS: &[&str] = &[
    "as", "else", "fn", "import", "let", "loop", "match", "mod", "pub", "return", "trait", "type",
    "when", "where",
];

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
    // Round-64 G4: src/repl.rs no longer inlines keyword string
    // literals — it iterates `lexer::KEYWORDS` and `KEYWORD_LITERALS`
    // directly. The invariant that REPL completion offers every core
    // keyword now lives at the runtime/API level: call
    // `repl::builtin_names()` and check membership.
    let names: std::collections::HashSet<String> =
        silt::repl::builtin_names().into_iter().collect();
    for kw in EXPECTED_CORE_KEYWORDS {
        assert!(
            names.contains(*kw),
            "repl::builtin_names() missing core keyword `\"{kw}\"`. \
             If a new keyword was added, also update lexer::KEYWORDS \
             and EXPECTED_CORE_KEYWORDS here. The structural lock that \
             repl.rs consumes lexer::KEYWORDS lives in \
             tests/repl_keyword_parity_with_lexer_tests.rs."
        );
    }
    // Intentional delta: REPL keyword list also includes `true`/`false`
    // as completion entries (CONSTANT-shaped, not KEYWORD-shaped).
    assert!(
        names.contains("true"),
        "repl::builtin_names() must include \"true\" as completion entry"
    );
    assert!(
        names.contains("false"),
        "repl::builtin_names() must include \"false\" as completion entry"
    );
}
