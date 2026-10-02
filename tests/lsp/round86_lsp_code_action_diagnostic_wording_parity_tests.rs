//! Round 86 G2 parity locks for
//! `src/lsp/code_action.rs`'s `diagnostic_matcher` predicates.
//!
//! Two quick-fixes in the LSP code-action catalog identify their target
//! diagnostics by **substring match on the message string**:
//!
//!   * `FixArrowFnType::diagnostic_matcher` requires the substring
//!     `"expected identifier, found ->"` to appear in the diagnostic
//!     message (`src/lsp/code_action.rs:191`). The producing site is
//!     `Parser::expect_ident` at `src/parser.rs:589`, which builds the
//!     message via `format!("expected identifier, found {}", self.peek())`
//!     where `{}` flows through `impl Display for Token` and
//!     `Token::Arrow` renders as `"->"` (`src/lexer.rs:138`).
//!
//!   * `WrapInOk::diagnostic_matcher` requires the message to contain
//!     `"type mismatch"` AND `"expected Result"` AND to NOT contain
//!     `"got Result"` (`src/lsp/code_action.rs:258`). The producing
//!     sites are the `unify` mismatch formats at
//!     `src/typechecker/mod.rs:1605` and `:1654`, which build the
//!     message via `format!("type mismatch: expected {t2}, got {t1}")`
//!     where `t2`'s `Display` impl is `Result(<a>, <e>)`
//!     (`src/types/mod.rs::Type::Display`).
//!
//! If the parser wording, `Token::Display` for `Arrow`, the unify error
//! format, or `Type::Display` for `Result` changes — without an in-step
//! update to the matcher — the quick-fix would silently stop firing.
//! Existing precedent for this kind of cross-file wording parity lock:
//! `tests/lsp/round81_lsp_completion_parity_tests.rs::lsp_auto_derived_completions_match_typechecker`.
//!
//! Both locks reach the live producer rather than stub messages. The
//! `WrapInOk` lock runs end to end through `silt lsp`: the real
//! typechecker diagnostic must make the server offer the quick-fix, which
//! catches drift on either side. The `FixArrowFnType` lock drives the live
//! parser only (see its doc comment for why).

use serde_json::json;

use silt::lexer::Lexer;
use silt::parser::Parser;

use crate::support::LspClient;

// ── Test 1: FixArrowFnType wording parity ───────────────────────────

/// Drive the live parser on a source where `->` appears at an
/// identifier position (immediately after `fn`), capture the resulting
/// parse-error message, and assert it contains the exact substring
/// `FixArrowFnType::diagnostic_matcher` greps for. The lock fails on
/// either side of the parity:
///
///   * If `Parser::expect_ident` changes its wording (e.g. drops the
///     comma, renames the literal, switches to "got X" phrasing) the
///     produced message stops containing the needle.
///   * If `Token::Display for Token::Arrow` ever renders something
///     other than `"->"` (e.g. `"→"`, `"arrow"`, `"-> (Token::Arrow)"`)
///     the produced message stops containing the literal `"->"` and the
///     matcher still keys on the old surface form.
///
/// In either case the quick-fix would silently stop matching real
/// arrow-fn-type diagnostics in the editor; this test fires loudly.
///
/// This lock stays parser-only: an old-style `(Int -> Int)` type now
/// fails earlier with "expected ')' or ',' to continue tuple type, found
/// ->", so no current source makes the server offer this quick-fix end
/// to end.
#[test]
fn fix_arrow_fn_type_matcher_matches_live_parser_error() {
    // `fn -> Foo() { ... }`: the `->` lands where `expect_ident` runs
    // immediately after `fn`. `expect_ident` formats the produced
    // message via `format!("expected identifier, found {}", self.peek())`
    // (src/parser.rs:589) and `Token::Arrow`'s Display impl writes "->"
    // (src/lexer.rs:138), so the message must contain the matcher's
    // required substring verbatim.
    let src = "fn -> Foo() { 1 }\n";
    let tokens = Lexer::new(silt::source::FileId::default(), src)
        .tokenize()
        .expect("lexer error");
    let err = Parser::new(tokens, src)
        .parse_program()
        .expect_err("expected a parse error on `fn -> Foo() { ... }`");

    let needle = "expected identifier, found ->";
    assert!(
        err.message.contains(needle),
        "live parser error message must contain `{needle}` (the \
         substring `FixArrowFnType::diagnostic_matcher` at \
         src/lsp/code_action.rs:191 greps for); got message: {:?}. \
         If the parser wording or `Token::Display for Token::Arrow` \
         changed, update both the matcher AND this test in lock-step.",
        err.message,
    );
}

// ── Test 2: WrapInOk wording parity ─────────────────────────────────

/// Open a document whose body returns a bare `Int` where the signature
/// declares `Result(Int, Int)`, and ask the server for code actions on
/// the resulting diagnostic. `WrapInOk::diagnostic_matcher` needs the
/// message to contain `"type mismatch"` and `"expected Result"` and not
/// `"got Result"`; the typechecker's unify mismatch format
/// (`type mismatch: expected {t2}, got {t1}`) with `Type::Display` for
/// `Result` must keep producing that. If either the typechecker wording
/// or the matcher changes alone, the quick-fix stops being offered and
/// this test fails.
#[test]
fn wrap_in_ok_quickfix_offered_for_live_typechecker_error() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_r86_wrap_in_ok.silt";
    let source = "fn produce() -> Result(Int, Int) { 21 }\n";
    let diags = client.did_open_and_collect_diagnostics(uri, source);
    let mismatch = diags
        .iter()
        .find(|d| {
            d["message"]
                .as_str()
                .is_some_and(|m| m.contains("expected Result"))
        })
        .unwrap_or_else(|| panic!("expected a Result type-mismatch diagnostic; got {diags:?}"))
        .clone();

    let resp = client.request(
        "textDocument/codeAction",
        json!({
            "textDocument": { "uri": uri },
            "range": mismatch["range"].clone(),
            "context": { "diagnostics": [mismatch] }
        }),
    );
    let titles: Vec<&str> = resp["result"]
        .as_array()
        .map(|actions| actions.iter().filter_map(|a| a["title"].as_str()).collect())
        .unwrap_or_default();
    assert!(
        titles.contains(&"Wrap expression in `Ok(...)`"),
        "the typechecker's Result mismatch diagnostic no longer triggers the \
         WrapInOk quick-fix (src/lsp/code_action.rs); the diagnostic wording \
         and the matcher must change together. Diagnostics: {diags:?}; \
         actions: {resp}"
    );
    client.shutdown();
}
