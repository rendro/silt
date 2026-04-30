//! Round-63 L1 parity lock: keyword-list authority lives in
//! `src/lexer.rs::KEYWORDS` (+ `KEYWORD_LITERALS` for `true`/`false`).
//!
//! Two LSP modules previously hand-rolled overlapping keyword arrays:
//!
//!   * `src/lsp/completion.rs` — 14-entry `KEYWORDS` const used in the
//!     bare-completion loop.
//!   * `src/lsp/rename.rs` — 16-entry `SILT_KEYWORDS` const (same 14 +
//!     `true`/`false`) used by `is_user_renameable` /
//!     `is_valid_silt_ident`.
//!
//! Both lists were trivially derivable from the lexer's
//! `scan_ident_or_keyword` match arms, but adding a new keyword required
//! editing three places — a drift hazard. After round-63 both LSP
//! modules consume `crate::lexer::KEYWORDS` (and rename additionally
//! consults `KEYWORD_LITERALS`).
//!
//! These tests are *source-grep* parity locks: they `include_str!()` the
//! relevant files and assert structural invariants on the bytes. That
//! makes them robust against trivial reverts — re-pasting the old
//! hand-rolled array trips the second test, and adding a match arm
//! without updating `KEYWORDS` trips the first.

use silt::lexer;

/// `include_str!` resolves relative to this test file. The crate root is
/// one directory up.
const LEXER_SRC: &str = include_str!("../src/lexer.rs");
const COMPLETION_SRC: &str = include_str!("../src/lsp/completion.rs");
const RENAME_SRC: &str = include_str!("../src/lsp/rename.rs");

#[test]
fn lexer_keywords_const_matches_match_arms() {
    // For every entry in `lexer::KEYWORDS`, the lexer source must
    // contain a `"<kw>" => Token::` match arm. Catches:
    //   1. A keyword added to `KEYWORDS` but not the lexer match block
    //      (or vice versa).
    //   2. A keyword removed from one place but not the other.
    //
    // We deliberately do NOT walk the AST — a textual lock is stronger
    // because reverting the round-63 fix re-introduces the inconsistency.
    assert!(
        !lexer::KEYWORDS.is_empty(),
        "lexer::KEYWORDS must not be empty"
    );

    for kw in lexer::KEYWORDS {
        let needle = format!("\"{kw}\" => Token::");
        assert!(
            LEXER_SRC.contains(&needle),
            "lexer::KEYWORDS contains `{kw}` but `src/lexer.rs` has no \
             matching `{needle}` match arm in `scan_ident_or_keyword`. \
             Either remove `{kw}` from `KEYWORDS` or add the match arm."
        );
    }

    // Belt-and-braces: KEYWORD_LITERALS must also match a Token::Bool arm.
    for kw in lexer::KEYWORD_LITERALS {
        let needle = format!("\"{kw}\" => Token::Bool(");
        assert!(
            LEXER_SRC.contains(&needle),
            "lexer::KEYWORD_LITERALS contains `{kw}` but `src/lexer.rs` \
             has no matching `{needle}` arm. Either remove `{kw}` from \
             `KEYWORD_LITERALS` or add the match arm."
        );
    }
}

#[test]
fn lsp_modules_use_lexer_keywords() {
    // The hand-rolled lists in completion.rs and rename.rs both started
    // with the literal substring `"as", "else", "fn", "import"`. If a
    // future change re-introduces a hand-rolled keyword array, that
    // exact head will reappear and this test fails.
    //
    // We pick the first four entries (in source order) because they form
    // a unique-enough fingerprint that no unrelated code in these files
    // is likely to match — and small enough that a reverter is unlikely
    // to dodge it by reformatting.
    let bad_head = "\"as\", \"else\", \"fn\", \"import\"";

    assert!(
        !COMPLETION_SRC.contains(bad_head),
        "src/lsp/completion.rs re-introduced a hand-rolled keyword list \
         (substring `{bad_head}`). Source the keyword list from \
         `crate::lexer::KEYWORDS` instead — see round-63 L1."
    );

    assert!(
        !RENAME_SRC.contains(bad_head),
        "src/lsp/rename.rs re-introduced a hand-rolled keyword list \
         (substring `{bad_head}`). Source the keyword list from \
         `crate::lexer::KEYWORDS` (+ `KEYWORD_LITERALS` for true/false) \
         instead — see round-63 L1."
    );

    // Belt-and-braces: each LSP module must reference `lexer::KEYWORDS`
    // (or import it) so the dependency is explicit.
    assert!(
        COMPLETION_SRC.contains("lexer::KEYWORDS") || COMPLETION_SRC.contains("use crate::lexer::KEYWORDS"),
        "src/lsp/completion.rs must reference `lexer::KEYWORDS` so the \
         dependency on the authoritative list is explicit."
    );
    assert!(
        RENAME_SRC.contains("lexer::KEYWORDS"),
        "src/lsp/rename.rs must reference `lexer::KEYWORDS` so the \
         dependency on the authoritative list is explicit."
    );
    assert!(
        RENAME_SRC.contains("lexer::KEYWORD_LITERALS"),
        "src/lsp/rename.rs must also consult `lexer::KEYWORD_LITERALS` \
         to reject `true`/`false` as user identifiers (the lexer emits \
         these as `Token::Bool(_)`, not as keyword tokens, but they are \
         still reserved words)."
    );
}
