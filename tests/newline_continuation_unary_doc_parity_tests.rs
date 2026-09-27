//! Lock for the newline-continuation rules around `+`, `-`, and postfix `?`,
//! and for the prose that documents them.
//!
//! Background (audit finding, round 101):
//!   * `docs/language/operators.md` and `docs/language/design-decisions.md`
//!     used to claim silt has a unary `+x` and that a line starting with `+`
//!     (or a lone `?`) "parses as two expressions" / "starts a new expr".
//!   * Actual behavior: silt has NO unary plus (`parse_unary` handles only
//!     `Minus` and `Not`), so `+` at the start of a line is a parse error
//!     ("expected expression, found +"), and a lone `?` line is a parse
//!     error ("expected expression, found ?"). Only `-` at the start of a
//!     line genuinely parses as a new unary-negation statement.
//!
//! This file locks BOTH sides:
//!   1. Behavior: the three variants parse exactly as the (fixed) docs say.
//!      If unary `+` is ever added, `plus_at_line_start_is_a_parse_error`
//!      fails and forces a doc revisit.
//!   2. Prose: the false phrases are gone from both docs, and the corrected
//!      "no unary plus" wording is present.

use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::types::Severity;

const OPERATORS_DOC: &str = include_str!("../docs/language/operators.md");
const DESIGN_DOC: &str = include_str!("../docs/language/design-decisions.md");

/// Expect a parse error; return its message.
fn parse_err(input: &str) -> String {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    match Parser::new(tokens).parse_program() {
        Err(e) => e.message.clone(),
        Ok(_) => panic!("expected parse error, got success for:\n{input}"),
    }
}

/// Expect lex + parse + typecheck to succeed with no hard errors.
fn assert_checks_clean(input: &str) {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens)
        .parse_program()
        .unwrap_or_else(|e| panic!("expected clean parse, got: {} for:\n{input}", e.message));
    let errors: Vec<String> = typechecker::check(&mut program)
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect();
    assert!(
        errors.is_empty(),
        "expected no type errors, got {errors:?} for:\n{input}"
    );
}

// ── Behavior locks ──────────────────────────────────────────────────

#[test]
fn plus_at_line_start_is_a_parse_error() {
    // The exact example from docs/language/operators.md. Silt has no unary
    // plus, so the `+ 20` line cannot start a new expression — it is a
    // parse error, NOT "two expressions" as the docs used to claim.
    //
    // If this test starts failing because the program now parses, unary `+`
    // was added to `parse_unary` — update docs/language/operators.md,
    // docs/language/design-decisions.md, and the `Token::Plus | Token::Minus`
    // comment in src/parser.rs::parse_expr_bp, then re-aim this lock.
    let msg = parse_err("fn main() {\n  let y = 10\n    + 20\n  println(\"{y}\")\n}\n");
    assert!(
        msg.contains("expected expression, found +"),
        "expected 'expected expression, found +', got: {msg}"
    );
}

#[test]
fn lone_question_mark_line_is_a_parse_error() {
    // Postfix `?` does not cross newlines, and a lone `?` cannot start an
    // expression — parse error, NOT "two expressions".
    let msg = parse_err("fn main() {\n  let n = maybe()\n    ?\n  println(\"{n}\")\n}\n");
    assert!(
        msg.contains("expected expression, found ?"),
        "expected 'expected expression, found ?', got: {msg}"
    );
}

#[test]
fn minus_at_line_start_parses_as_unary_negation_statement() {
    // Unlike `+`, a line starting with `-` IS a valid new statement: `-20`
    // parses as a unary-negation expression. This is the genuine ambiguity
    // that motivates the newline rule, and it checks clean end to end.
    assert_checks_clean("fn main() {\n  let y = 10\n    - 20\n  println(\"{y}\")\n}\n");
}

// ── Doc-parity locks ────────────────────────────────────────────────

#[test]
fn docs_no_longer_claim_unary_plus_or_two_expression_parses() {
    for (name, doc) in [
        ("docs/language/operators.md", OPERATORS_DOC),
        ("docs/language/design-decisions.md", DESIGN_DOC),
    ] {
        for false_phrase in [
            "unary `-x` / `+x`",
            "(also unary)",
            "parses as two expressions",
            "starts a new expr",
        ] {
            assert!(
                !doc.contains(false_phrase),
                "{name} still contains the false claim {false_phrase:?}: \
                 silt has no unary plus, and a line starting with `+` or a \
                 lone `?` is a parse error, not a second expression. \
                 See the behavior locks in this file."
            );
        }
    }
}

#[test]
fn docs_state_the_corrected_no_unary_plus_rule() {
    // Positive side of the parity lock: both docs must carry the corrected
    // wording so a future rewrite can't silently drop the clarification.
    assert!(
        OPERATORS_DOC.contains("silt has no unary plus"),
        "docs/language/operators.md lost the 'silt has no unary plus' \
         clarification for the `+`/`-` newline rule"
    );
    assert!(
        DESIGN_DOC.contains("silt has no unary plus"),
        "docs/language/design-decisions.md lost the 'silt has no unary plus' \
         clarification for the `+`/`-` newline rule"
    );
}
