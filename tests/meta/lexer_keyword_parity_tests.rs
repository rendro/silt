//! Round-63 L1 parity lock: keyword-list authority lives in
//! `src/lexer.rs::KEYWORDS` (+ `KEYWORD_LITERALS` for `true`/`false`).
//!
//! Two LSP modules previously hand-rolled overlapping keyword arrays
//! (`src/lsp/completion.rs` and `src/lsp/rename.rs`). After round 63
//! both consume `crate::lexer::KEYWORDS` (and rename additionally
//! consults `KEYWORD_LITERALS`).
//!
//! These tests check the lists by behaviour: every listed keyword lexes
//! as a keyword token (not an identifier), and rename refuses every one.

use silt::lexer::{self, Lexer, Token};

fn first_token(src: &str) -> Token {
    let tokens = Lexer::new(silt::source::FileId::default(), src)
        .tokenize()
        .expect("lexer error");
    tokens.tokens.into_iter().next().expect("no token").kind
}

#[test]
fn lexer_keywords_const_matches_lexer_behaviour() {
    // Every entry in `KEYWORDS` must lex as a keyword token, and every
    // entry in `KEYWORD_LITERALS` as a `Token::Bool`. Catches a keyword
    // listed in the const but missing from the lexer's match arms.
    assert!(
        !lexer::KEYWORDS.is_empty(),
        "lexer::KEYWORDS must not be empty"
    );
    for kw in lexer::KEYWORDS {
        let tok = first_token(kw);
        assert!(
            !matches!(tok, Token::Ident(_)),
            "lexer::KEYWORDS contains `{kw}` but the lexer reads it as an \
             identifier. Either remove `{kw}` from `KEYWORDS` or add the \
             match arm in `scan_ident_or_keyword`."
        );
    }
    for kw in lexer::KEYWORD_LITERALS {
        let tok = first_token(kw);
        assert!(
            matches!(tok, Token::Bool(_)),
            "lexer::KEYWORD_LITERALS contains `{kw}` but the lexer reads it \
             as {tok:?}, not a Token::Bool."
        );
    }
}

#[test]
fn lsp_rename_rejects_every_keyword() {
    for kw in lexer::KEYWORDS.iter().chain(lexer::KEYWORD_LITERALS) {
        assert!(
            !silt::lsp::is_valid_silt_ident(kw),
            "a rename accepts the reserved word `{kw}` as a new name"
        );
    }
}
