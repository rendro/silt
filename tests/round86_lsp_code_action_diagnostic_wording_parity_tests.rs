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
//! `tests/round81_lsp_completion_parity_tests.rs::lsp_auto_derive_method_names_match_typechecker_trait_names`.
//!
//! These tests are **real parity locks**: they reach into the live
//! parser/typechecker (not stub error messages) and assert the produced
//! message string contains the substrings the matcher requires. An
//! optional static-grep assertion on `src/lsp/code_action.rs` catches
//! drift on the LSP side as well.

use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::types::Severity;

const CODE_ACTION_RS: &str = include_str!("../src/lsp/code_action.rs");

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
#[test]
fn fix_arrow_fn_type_matcher_matches_live_parser_error() {
    // `fn -> Foo() { ... }`: the `->` lands where `expect_ident` runs
    // immediately after `fn`. `expect_ident` formats the produced
    // message via `format!("expected identifier, found {}", self.peek())`
    // (src/parser.rs:589) and `Token::Arrow`'s Display impl writes "->"
    // (src/lexer.rs:138), so the message must contain the matcher's
    // required substring verbatim.
    let src = "fn -> Foo() { 1 }\n";
    let tokens = Lexer::new(src).tokenize().expect("lexer error");
    let err = Parser::new(tokens)
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

    // Static-grep companion: the matcher source still spells the
    // exact wording string we just observed from the parser. Together
    // with the live-parser check above this catches drift on either
    // side of the parity (parser wording change OR matcher edit).
    let matcher_needle = "\"expected identifier, found ->\"";
    assert!(
        CODE_ACTION_RS.contains(matcher_needle),
        "src/lsp/code_action.rs no longer contains the matcher literal \
         {matcher_needle}. If the matcher's wording changed, the live \
         parser error wording at src/parser.rs:589 / Token::Display \
         for Token::Arrow at src/lexer.rs:138 MUST change in \
         lock-step. Either revert the LSP edit or update both sides \
         AND this test."
    );
}

// ── Test 2: WrapInOk wording parity ─────────────────────────────────

/// Drive the live typechecker on a source where a non-Result value is
/// returned where `Result(a, e)` is expected, capture the produced
/// type-error message, and assert it satisfies the three substring
/// conditions `WrapInOk::diagnostic_matcher` requires:
///
///   1. Contains `"type mismatch"`.
///   2. Contains `"expected Result"`.
///   3. Does NOT contain `"got Result"` (the matcher excludes the
///      case where the *actual* type is also a Result — wrapping in
///      `Ok(...)` doesn't help when the user already produced a
///      Result of the wrong shape).
///
/// The fixture mirrors `tests/pipe_question_precedence_tests.rs:117`'s
/// `fn produce() -> Result(Int, Int) { Ok(21) }` style, but returns a
/// bare `Int` instead of `Ok(Int)` so the body's inferred type is `Int`
/// and the declared return type is `Result(Int, Int)`. The unify call
/// at `src/typechecker/mod.rs:1654` (Int-vs-Generic mismatch arm)
/// produces the message via `format!("type mismatch: expected {t2}, got {t1}")`.
#[test]
fn wrap_in_ok_matcher_matches_live_typechecker_error() {
    // Body returns bare `21` (an `Int`) but the signature declares
    // `Result(Int, Int)`. The typechecker unifies the body type
    // against the declared return type; the mismatch arm at
    // src/typechecker/mod.rs:1654 fires `"type mismatch: expected
    // Result(Int, Int), got Int"`. That message contains
    // `"type mismatch"` and `"expected Result"` and does NOT contain
    // `"got Result"` — exactly what `WrapInOk::diagnostic_matcher`
    // (src/lsp/code_action.rs:258) requires.
    let src = "fn produce() -> Result(Int, Int) { 21 }\n";
    let tokens = Lexer::new(src).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let errors = typechecker::check(&mut program);
    let messages: Vec<String> = errors
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect();

    let candidates: Vec<&String> = messages
        .iter()
        .filter(|m| {
            m.contains("type mismatch")
                && m.contains("expected Result")
                && !m.contains("got Result")
        })
        .collect();

    assert!(
        !candidates.is_empty(),
        "no live typechecker error matched all three \
         `WrapInOk::diagnostic_matcher` conditions \
         (contains `type mismatch`, contains `expected Result`, does \
         NOT contain `got Result`). Observed error messages: {:#?}. \
         If the typechecker unify wording at \
         src/typechecker/mod.rs:1605/1654 or `Type::Display` for \
         `Result` (src/types/mod.rs) changed, update the matcher AND \
         this test in lock-step.",
        messages,
    );

    // Static-grep companion: the matcher source still spells the
    // exact three substrings we just observed are produced by the
    // typechecker. Catches drift on the LSP side.
    let needle_mismatch = "\"type mismatch\"";
    let needle_expected = "\"expected Result\"";
    let needle_got = "\"got Result\"";
    assert!(
        CODE_ACTION_RS.contains(needle_mismatch),
        "src/lsp/code_action.rs no longer contains the matcher \
         literal {needle_mismatch}. If the matcher's wording changed, \
         the live typechecker error at \
         src/typechecker/mod.rs:1605/1654 MUST change in lock-step. \
         Either revert the LSP edit or update both sides AND this \
         test."
    );
    assert!(
        CODE_ACTION_RS.contains(needle_expected),
        "src/lsp/code_action.rs no longer contains the matcher \
         literal {needle_expected}. If the matcher's wording changed, \
         `Type::Display` for `Result` (src/types/mod.rs) MUST change \
         in lock-step. Either revert the LSP edit or update both \
         sides AND this test."
    );
    assert!(
        CODE_ACTION_RS.contains(needle_got),
        "src/lsp/code_action.rs no longer contains the matcher \
         literal {needle_got} (the exclusion condition). If the \
         exclusion was dropped, the WrapInOk quick-fix would start \
         firing on Result-vs-Result mismatches where wrapping in \
         `Ok(...)` is the wrong fix. Update both sides AND this test."
    );
}
