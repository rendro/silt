//! Round-76 BLOAT/DC2 parser-test scaffolding lock.
//!
//! Before round 76 the `parser::tests` module open-coded the same
//! ~6-line decode sequence at ~28 distinct test sites:
//!
//! ```text
//! if let Decl::Fn(ref f) = prog.decls[0] {
//!     let stmts = match &f.body.kind {
//!         ExprKind::Block(stmts) => stmts,
//!         _ => panic!("expected block"),
//!     };
//!     let expr = match stmts.last().unwrap() {
//!         Stmt::Expr(e) => e,
//!         _ => panic!("expected expr stmt"),
//!     };
//!     ...
//! }
//! ```
//!
//! Round 76 collapses that scaffolding into three helpers:
//!
//! - `last_expr_of_main(&Program) -> &Expr`
//! - `last_match_of_main(&Program) -> (Option<&Expr>, &[MatchArm])`
//! - `first_match_arm_of_main(&Program) -> &MatchArm`
//!
//! This file pins that the inline pattern is gone (count drops from
//! ~28 to <5) and that the helpers are present. The remaining
//! occurrences are tests that inspect fn metadata directly (e.g.
//! `f.is_pub`, `f.name`, `f.where_clauses`) and don't decode the body.

const PARSER_SRC: &str = include_str!("../src/parser.rs");

/// Count occurrences of the inline `if let Decl::Fn(ref f) = prog.decls[0]`
/// fingerprint in the parser source.
fn count_inline_fn_decode() -> usize {
    PARSER_SRC
        .matches("if let Decl::Fn(ref f) = prog.decls[0]")
        .count()
}

#[test]
fn inline_fn_decode_pattern_is_under_threshold() {
    let n = count_inline_fn_decode();
    // Must be strictly less than 5. Was ~28 before round 76. The
    // remaining sites (3 today) inspect fn metadata directly, not the
    // body — those should NOT be rewritten.
    assert!(
        n < 5,
        "expected <5 occurrences of `if let Decl::Fn(ref f) = prog.decls[0]` \
         in src/parser.rs after round-76 helper extraction, but found {}. \
         Did a new test re-introduce the inline body-decode pattern? \
         Use `last_expr_of_main`, `last_match_of_main`, or \
         `first_match_arm_of_main` instead.",
        n
    );
}

#[test]
fn helpers_are_present_in_parser_test_module() {
    // Source-grep that the three helper signatures are wired in.
    assert!(
        PARSER_SRC.contains("fn last_expr_of_main(prog: &Program) -> &Expr"),
        "missing helper `last_expr_of_main` in src/parser.rs test module"
    );
    assert!(
        PARSER_SRC
            .contains("fn last_match_of_main(prog: &Program) -> (Option<&Expr>, &[MatchArm])"),
        "missing helper `last_match_of_main` in src/parser.rs test module"
    );
    assert!(
        PARSER_SRC.contains("fn first_match_arm_of_main(prog: &Program) -> &MatchArm"),
        "missing helper `first_match_arm_of_main` in src/parser.rs test module"
    );
}

#[test]
fn helper_contract_pin_test_is_present() {
    // The `helpers_match_hand_decoded_reference` unit test in
    // src/parser.rs locks the helpers' contracts against a hand-decoded
    // reference AST. It must remain — without it, future edits to the
    // helpers could silently drift away from the original semantics.
    assert!(
        PARSER_SRC.contains("fn helpers_match_hand_decoded_reference()"),
        "missing helper-contract pin test \
         `helpers_match_hand_decoded_reference` in src/parser.rs"
    );
}
