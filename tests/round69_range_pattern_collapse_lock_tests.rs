//! Round 69 — Range-pattern parser collapse lock.
//!
//! Background: `src/parser.rs` previously inlined the `..[-]N` /
//! `..[-]F` tail logic four times — once each for positive int,
//! negative int, positive float, negative float range heads. The
//! four blocks shared identical structure but each spelled out its
//! own `Err(ParseError { ..., span: self.span() })` returns and
//! literal end-of-range messages. Drift here would produce
//! asymmetric error wording between `1..` and `-1..` sibling
//! mistakes — an audit-medium "BLOAT-DUP" finding.
//!
//! The fix extracts two helpers,
//! `Parser::parse_range_tail_int(start: i64)` and
//! `Parser::parse_range_tail_float(start: f64)`, that consume
//! `[-]N` / `[-]F` after the `..` token has already been advanced.
//! Both sign branches in `parse_primary_pattern` now pass their
//! already-signed start bound into the helper, so a single shared
//! body produces both the AST and the error text.
//!
//! These tests pin the refactor as a SEMANTIC NO-OP:
//!
//! 1. **Acceptance corpus** — a grid of well-formed range patterns
//!    that must parse + typecheck successfully.
//! 2. **Rejection corpus** — malformed tails that must surface
//!    EXACT historical error wording (so a refactor that changes
//!    the message text fails the test).
//! 3. **Symmetry locks** — same shape with positive vs. negated
//!    head must produce the same error; same shape with int vs.
//!    float must use the matching error vocabulary.
//! 4. **i64 boundary case** — silt's lexer rejects
//!    `9223372036854775808` at lex time, so `i64::MIN` cannot be
//!    written as a literal range bound. We pin both that lex-level
//!    rejection AND the largest writable negative bound
//!    (`-9223372036854775807`, i.e. `i64::MIN + 1`) parsing
//!    correctly via both head-sign paths.
//!
//! Owner-file boundary: this test exclusively touches the parser's
//! pattern range surface. It deliberately exercises both the
//! direct `Parser::parse_program` path AND the typechecker via
//! `silt::typechecker::check`, because the brief specifies that
//! the lock must run "via the typechecker / parser, NOT just the
//! bytecode VM (the VM may bypass invariants the parser
//! enforces)".

use silt::ast::{Decl, ExprKind, Pattern, PatternKind, Program, Stmt};
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::types::Severity;

// ── Plumbing ─────────────────────────────────────────────────────────

/// Parse a full program; return the program or the first parse error
/// message. Uses the strict `parse_program` (not the recovering
/// variant), so any parse error short-circuits.
fn parse_program(src: &str) -> Result<Program, String> {
    let tokens = Lexer::new(src)
        .tokenize()
        .map_err(|e| format!("lex error: {}", e.message))?;
    Parser::new(tokens).parse_program().map_err(|e| e.message)
}

/// Parse + typecheck. Returns Ok(()) when both succeed without errors,
/// or the first non-warning diagnostic message on failure.
fn parse_and_typecheck(src: &str) -> Result<(), String> {
    let mut prog = parse_program(src)?;
    let errors = typechecker::check(&mut prog);
    if let Some(e) = errors.into_iter().find(|e| e.severity == Severity::Error) {
        return Err(e.message);
    }
    Ok(())
}

/// Pull the first match arm's pattern out of a `fn f(x) { match x { ... } }`
/// shape. Panics with a descriptive message on any structural mismatch
/// so test failures point straight at the AST drift.
fn first_match_arm_pattern(prog: &Program) -> Pattern {
    let f = match &prog.decls[0] {
        Decl::Fn(f) => f,
        other => panic!("expected fn decl, got {other:?}"),
    };
    let stmts = match &f.body.kind {
        ExprKind::Block(stmts) => stmts,
        ExprKind::Match { arms, .. } => return arms[0].pattern.clone(),
        other => panic!("expected fn body to be Block or Match, got {other:?}"),
    };
    let last = stmts.last().expect("body has no stmts");
    let expr = match last {
        Stmt::Expr(e) => e,
        other => panic!("expected last stmt to be Expr, got {other:?}"),
    };
    match &expr.kind {
        ExprKind::Match { arms, .. } => arms[0].pattern.clone(),
        other => panic!("expected match expr, got {other:?}"),
    }
}

/// Wrap a single range pattern in the canonical match scaffold.
fn match_with_pattern(pat: &str) -> String {
    format!(
        r#"
fn f(x) {{
    match x {{
        {pat} -> "yes"
        _ -> "no"
    }}
}}
"#
    )
}

// ── 1. Acceptance corpus ─────────────────────────────────────────────

#[test]
fn accept_pos_int_range() {
    let src = match_with_pattern("1..10");
    let prog = parse_program(&src).expect("parse");
    match first_match_arm_pattern(&prog).kind {
        PatternKind::Range(1, 10) => {}
        other => panic!("expected Range(1, 10), got {other:?}"),
    }
}

#[test]
fn accept_neg_int_range_both_negative() {
    let src = match_with_pattern("-10..-1");
    let prog = parse_program(&src).expect("parse");
    match first_match_arm_pattern(&prog).kind {
        PatternKind::Range(-10, -1) => {}
        other => panic!("expected Range(-10, -1), got {other:?}"),
    }
}

#[test]
fn accept_neg_int_range_crossing_zero() {
    let src = match_with_pattern("-5..5");
    let prog = parse_program(&src).expect("parse");
    match first_match_arm_pattern(&prog).kind {
        PatternKind::Range(-5, 5) => {}
        other => panic!("expected Range(-5, 5), got {other:?}"),
    }
}

#[test]
fn accept_pos_int_range_with_negative_tail() {
    // Strange but legal at parse time: `1..-1`. Runtime won't match
    // anything (lo > hi) but the parser must accept and produce the
    // exact AST shape the four pre-refactor inline blocks would have.
    let src = match_with_pattern("1..-1");
    let prog = parse_program(&src).expect("parse");
    match first_match_arm_pattern(&prog).kind {
        PatternKind::Range(1, -1) => {}
        other => panic!("expected Range(1, -1), got {other:?}"),
    }
}

#[test]
fn accept_pos_float_range() {
    let src = match_with_pattern("1.0..10.0");
    let prog = parse_program(&src).expect("parse");
    match first_match_arm_pattern(&prog).kind {
        PatternKind::FloatRange(lo, hi) if lo == 1.0 && hi == 10.0 => {}
        other => panic!("expected FloatRange(1.0, 10.0), got {other:?}"),
    }
}

#[test]
fn accept_neg_float_range_crossing_zero() {
    let src = match_with_pattern("-1.5..1.5");
    let prog = parse_program(&src).expect("parse");
    match first_match_arm_pattern(&prog).kind {
        PatternKind::FloatRange(lo, hi) if lo == -1.5 && hi == 1.5 => {}
        other => panic!("expected FloatRange(-1.5, 1.5), got {other:?}"),
    }
}

#[test]
fn accept_neg_float_range_both_negative() {
    let src = match_with_pattern("-2.5..-1.5");
    let prog = parse_program(&src).expect("parse");
    match first_match_arm_pattern(&prog).kind {
        PatternKind::FloatRange(lo, hi) if lo == -2.5 && hi == -1.5 => {}
        other => panic!("expected FloatRange(-2.5, -1.5), got {other:?}"),
    }
}

#[test]
fn accept_pos_float_range_with_negative_tail() {
    let src = match_with_pattern("1.5..-1.5");
    let prog = parse_program(&src).expect("parse");
    match first_match_arm_pattern(&prog).kind {
        PatternKind::FloatRange(lo, hi) if lo == 1.5 && hi == -1.5 => {}
        other => panic!("expected FloatRange(1.5, -1.5), got {other:?}"),
    }
}

/// All accepted shapes must also typecheck cleanly when the
/// scrutinee unifies to the matching numeric type. (`x` is a free
/// parameter, so the unifier infers `Int` or `Float` from the
/// pattern.)
#[test]
fn accept_corpus_typechecks() {
    let int_patterns = ["1..10", "-10..-1", "-5..5", "1..-1"];
    let float_patterns = ["1.0..10.0", "-1.5..1.5", "-2.5..-1.5", "1.5..-1.5"];
    for p in int_patterns.iter().chain(float_patterns.iter()) {
        let src = match_with_pattern(p);
        parse_and_typecheck(&src)
            .unwrap_or_else(|e| panic!("pattern `{p}` should typecheck cleanly, got error: {e}"));
    }
}

// ── 2. Rejection corpus ──────────────────────────────────────────────

/// EXACT historical wording — pinned by the four pre-refactor inline
/// blocks. Helper changes that alter wording fail this test.
const ERR_INT_END: &str = "expected integer end for range pattern";
const ERR_INT_AFTER_MINUS: &str = "expected integer after - in range pattern";
const ERR_FLOAT_END: &str = "expected float end for range pattern";
const ERR_FLOAT_AFTER_MINUS: &str = "expected float after - in range pattern";

#[test]
fn reject_int_range_with_float_tail() {
    let src = match_with_pattern("1..1.0");
    let err = parse_program(&src).expect_err("parse must fail");
    assert_eq!(
        err, ERR_INT_END,
        "exact wording drift in int range tail error: got `{err}`"
    );
}

#[test]
fn reject_neg_int_range_with_float_tail() {
    // Symmetric to the positive case; this is the half of the
    // duplicated block that used to be the silent drift risk.
    let src = match_with_pattern("-1..1.0");
    let err = parse_program(&src).expect_err("parse must fail");
    assert_eq!(
        err, ERR_INT_END,
        "negated int range tail must use the same wording as positive"
    );
}

#[test]
fn reject_int_range_with_minus_then_float() {
    let src = match_with_pattern("1..-1.0");
    let err = parse_program(&src).expect_err("parse must fail");
    assert_eq!(
        err, ERR_INT_AFTER_MINUS,
        "exact wording drift in int range minus-then-non-int error: got `{err}`"
    );
}

#[test]
fn reject_neg_int_range_with_minus_then_float() {
    let src = match_with_pattern("-1..-1.0");
    let err = parse_program(&src).expect_err("parse must fail");
    assert_eq!(
        err, ERR_INT_AFTER_MINUS,
        "negated int range minus-then-non-int must use same wording"
    );
}

#[test]
fn reject_float_range_with_int_tail() {
    let src = match_with_pattern("1.0..1");
    let err = parse_program(&src).expect_err("parse must fail");
    assert_eq!(
        err, ERR_FLOAT_END,
        "exact wording drift in float range tail error: got `{err}`"
    );
}

#[test]
fn reject_neg_float_range_with_int_tail() {
    let src = match_with_pattern("-1.0..1");
    let err = parse_program(&src).expect_err("parse must fail");
    assert_eq!(
        err, ERR_FLOAT_END,
        "negated float range tail must use the same wording as positive"
    );
}

#[test]
fn reject_float_range_with_minus_then_int() {
    let src = match_with_pattern("1.0..-1");
    let err = parse_program(&src).expect_err("parse must fail");
    assert_eq!(
        err, ERR_FLOAT_AFTER_MINUS,
        "exact wording drift in float range minus-then-non-float error: got `{err}`"
    );
}

#[test]
fn reject_neg_float_range_with_minus_then_int() {
    let src = match_with_pattern("-1.0..-1");
    let err = parse_program(&src).expect_err("parse must fail");
    assert_eq!(
        err, ERR_FLOAT_AFTER_MINUS,
        "negated float range minus-then-non-float must use same wording"
    );
}

// ── 3. Symmetry locks ────────────────────────────────────────────────

/// The *single most important* lock: positive and negated head paths
/// must surface IDENTICAL error wording for the same tail mistake.
/// Pre-refactor, the four exits were hand-typed; a typo in one would
/// drift silently. Post-refactor, both paths funnel through the same
/// helper so the two messages are literally the same string.
#[test]
fn symmetry_int_end_message() {
    let pos = parse_program(&match_with_pattern("1..1.0")).expect_err("pos must fail");
    let neg = parse_program(&match_with_pattern("-1..1.0")).expect_err("neg must fail");
    assert_eq!(
        pos, neg,
        "positive vs. negated int range tail error must be identical"
    );
}

#[test]
fn symmetry_int_after_minus_message() {
    let pos = parse_program(&match_with_pattern("1..-1.0")).expect_err("pos must fail");
    let neg = parse_program(&match_with_pattern("-1..-1.0")).expect_err("neg must fail");
    assert_eq!(
        pos, neg,
        "positive vs. negated int range minus-tail error must be identical"
    );
}

#[test]
fn symmetry_float_end_message() {
    let pos = parse_program(&match_with_pattern("1.0..1")).expect_err("pos must fail");
    let neg = parse_program(&match_with_pattern("-1.0..1")).expect_err("neg must fail");
    assert_eq!(
        pos, neg,
        "positive vs. negated float range tail error must be identical"
    );
}

#[test]
fn symmetry_float_after_minus_message() {
    let pos = parse_program(&match_with_pattern("1.0..-1")).expect_err("pos must fail");
    let neg = parse_program(&match_with_pattern("-1.0..-1")).expect_err("neg must fail");
    assert_eq!(
        pos, neg,
        "positive vs. negated float range minus-tail error must be identical"
    );
}

/// AST-level symmetry: both sign branches must produce structurally
/// identical `PatternKind` values once the start bound is sign-applied.
/// Pinning this catches a refactor that, e.g., accidentally drops the
/// sign on either side of the helper boundary.
#[test]
fn symmetry_negated_head_applies_sign() {
    let prog = parse_program(&match_with_pattern("-3..7")).expect("parse");
    match first_match_arm_pattern(&prog).kind {
        PatternKind::Range(-3, 7) => {}
        other => panic!("expected Range(-3, 7), got {other:?}"),
    }

    let prog = parse_program(&match_with_pattern("-3.5..7.5")).expect("parse");
    match first_match_arm_pattern(&prog).kind {
        PatternKind::FloatRange(lo, hi) if lo == -3.5 && hi == 7.5 => {}
        other => panic!("expected FloatRange(-3.5, 7.5), got {other:?}"),
    }
}

// ── 4. i64 boundary cases ────────────────────────────────────────────

/// silt's lexer rejects `9223372036854775808` (one past i64::MAX) at
/// lex time — see src/lexer.rs:626-627 ("number literal too large").
/// This is the invariant that lets the parser's helper safely write
/// `-n` for the head and `-m` for the tail without overflow checks:
/// `Token::Int(n)` always satisfies `0 <= n <= i64::MAX`, and
/// negation produces a value in `[i64::MIN+1, 0]` — never `i64::MIN`.
///
/// If this lex invariant ever changes, the helpers' unchecked `-n`
/// becomes unsound and this test fires as a tripwire.
#[test]
fn lexer_rejects_i64_min_magnitude_literal() {
    let src = match_with_pattern("-9223372036854775808..0");
    let err = parse_program(&src).expect_err("must fail at lex time");
    assert!(
        err.contains("number literal too large"),
        "expected lex-time `number literal too large`, got: `{err}`"
    );
}

/// The largest writable negative i64 bound is `-9223372036854775807`
/// (== i64::MIN + 1), reachable via `Token::Minus + Token::Int(i64::MAX)`.
/// This must round-trip through BOTH the negative-head path
/// (`-MAX..0`) and the negative-tail path (`0..-MAX`) producing the
/// expected `Range` AST. This is the highest-risk drift point: a
/// helper that mis-applies the sign here would corrupt the bound by
/// flipping it.
#[test]
fn i64_min_plus_one_round_trips_in_negative_head() {
    let src = match_with_pattern("-9223372036854775807..0");
    let prog = parse_program(&src).expect("parse");
    match first_match_arm_pattern(&prog).kind {
        PatternKind::Range(lo, 0) => {
            assert_eq!(lo, i64::MIN + 1, "negative head must produce i64::MIN + 1");
        }
        other => panic!("expected Range(i64::MIN+1, 0), got {other:?}"),
    }
}

#[test]
fn i64_min_plus_one_round_trips_in_negative_tail() {
    let src = match_with_pattern("0..-9223372036854775807");
    let prog = parse_program(&src).expect("parse");
    match first_match_arm_pattern(&prog).kind {
        PatternKind::Range(0, hi) => {
            assert_eq!(hi, i64::MIN + 1, "negative tail must produce i64::MIN + 1");
        }
        other => panic!("expected Range(0, i64::MIN+1), got {other:?}"),
    }
}

/// And the same combined: `-MAX..-MAX` exercises both the negated
/// head AND the negated tail in one pattern. AST must show both
/// bounds as `i64::MIN + 1`.
#[test]
fn i64_min_plus_one_round_trips_in_both_head_and_tail() {
    let src = match_with_pattern("-9223372036854775807..-9223372036854775807");
    let prog = parse_program(&src).expect("parse");
    match first_match_arm_pattern(&prog).kind {
        PatternKind::Range(lo, hi) => {
            assert_eq!(lo, i64::MIN + 1);
            assert_eq!(hi, i64::MIN + 1);
        }
        other => panic!("expected Range(i64::MIN+1, i64::MIN+1), got {other:?}"),
    }
}

/// And the boundary case must also typecheck (range patterns
/// unify with `Int` regardless of the bound magnitudes — see
/// src/typechecker/inference.rs:1981-1983).
#[test]
fn i64_min_plus_one_typechecks_in_match_arm() {
    let src = match_with_pattern("-9223372036854775807..0");
    parse_and_typecheck(&src).expect("must typecheck");
}

// ── 5. Helper-presence source-grep lock ──────────────────────────────

/// Anchors the refactor at the source level: if someone later inlines
/// the helpers back into `parse_primary_pattern`, the four `Err` exits
/// would re-multiply and the BLOAT-DUP would resurface. Pinning the
/// helper signatures keeps the collapse intact.
#[test]
fn parser_source_contains_range_tail_helpers() {
    let src = include_str!("../src/parser.rs");
    assert!(
        src.contains("fn parse_range_tail_int(&mut self, start: i64)"),
        "Parser::parse_range_tail_int helper signature missing — was the \
         refactor reverted? The duplicated `..[-]N` blocks must stay \
         collapsed into the helper."
    );
    assert!(
        src.contains("fn parse_range_tail_float(&mut self, start: f64)"),
        "Parser::parse_range_tail_float helper signature missing — was \
         the refactor reverted? The duplicated `..[-]F` blocks must stay \
         collapsed into the helper."
    );
}
