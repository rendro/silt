//! Round 81 regression locks: BinOp binding-power table parity between
//! `src/formatter.rs` and `src/parser.rs`.
//!
//! Round 81 unified the BinOp precedence ladder in `src/formatter.rs`,
//! which had encoded the parser's `(l_bp, r_bp)` table for each Binary
//! variant THREE times (see `precedence`, `expr_top_l_bp`, and
//! `format_expr_with_parens`). The canonical `(l_bp, r_bp)` pairs live
//! in `src/parser.rs::parse_expr_bp` (one arm per token group). Drift
//! between the two sides silently mis-groups re-emitted expressions —
//! Round 67 already had a regression in this area for the cross-family
//! constructs (Range / FloatElse / Ascription / QuestionMark).
//!
//! These tests pin three independent properties:
//!
//! 1. **Parity lock against parser source.** For every `BinOp` variant,
//!    grep the parser source for the structural `(l_bp, r_bp)` literal
//!    pair and assert it matches the value the formatter helper bakes
//!    in. If the parser's bp ladder shifts without a corresponding
//!    formatter change, this fails loudly.
//!
//! 2. **Behavioural round-trip / idempotency.** Format mixed-precedence
//!    BinOp snippets and assert `fmt(fmt(src)) == fmt(src)` and that
//!    higher-precedence children are NOT spuriously parenthesised on
//!    re-emit.
//!
//! 3. **Output identity.** Pin exact formatter output bytes for a
//!    handful of expressions covering all 6 bp levels (Or/And/Eq/Cmp/
//!    Add/Mul). Locks the no-op refactor: any accidental shift in
//!    precedence handling fails the byte comparison.
//!
//! These tests intentionally read `src/parser.rs` via `include_str!` so
//! they fail at compile-or-test time the moment either side drifts.

use silt::formatter::format;
use silt::lexer::Lexer;
use silt::parser::Parser;

const PARSER_SRC: &str = include_str!("../src/parser.rs");

/// Canonical (variant_token, expected_l_bp) pairs. Kept in lockstep
/// with `bp::binop_l_bp` in `src/formatter.rs` and the `(l_bp, r_bp)`
/// arms of `parse_expr_bp` in `src/parser.rs`. If the parser changes,
/// the parity test below will fail; if the formatter helper changes
/// without updating both sides, the byte-identity tests below will
/// also fail (because the formatter's child-paren decisions shift).
///
/// The variants are grouped exactly as in the parser arms. Each entry
/// is `(token_label, BinOp variants in that arm, expected_l_bp)`.
fn parser_binop_arms() -> Vec<(&'static str, Vec<&'static str>, u8)> {
    vec![
        ("Token::OrOr", vec!["BinOp::Or"], 20),
        ("Token::AndAnd", vec!["BinOp::And"], 30),
        ("Token::EqEq | Token::NotEq", vec!["BinOp::Eq", "BinOp::Neq"], 40),
        (
            "Token::Lt | Token::Gt | Token::LtEq | Token::GtEq",
            vec!["BinOp::Lt", "BinOp::Gt", "BinOp::Leq", "BinOp::Geq"],
            50,
        ),
        ("Token::Plus | Token::Minus", vec!["BinOp::Add", "BinOp::Sub"], 70),
        (
            "Token::Star | Token::Slash | Token::Percent",
            vec!["BinOp::Mul", "BinOp::Div", "BinOp::Mod"],
            80,
        ),
    ]
}

/// Test 1a: each parser arm contains the literal `(l_bp, r_bp) = (N, N+1)`
/// pair for the expected bp value. Locks the parser side.
#[test]
fn parser_arms_carry_expected_l_bp_r_bp_literal_pairs() {
    for (token_label, _variants, expected_l_bp) in parser_binop_arms() {
        let arm_idx = PARSER_SRC.find(token_label).unwrap_or_else(|| {
            panic!(
                "parser source no longer contains arm label `{token_label}` — \
                 the parser BinOp arms have been restructured. Update \
                 `parser_binop_arms` in this test and `bp::binop_l_bp` in \
                 src/formatter.rs to match."
            )
        });
        // Search a 600-byte window after the arm label for the (l_bp, r_bp)
        // pair literal. The `parse_expr_bp` arm bodies are short — the
        // assignment `let (l_bp, r_bp) = (N, N+1);` lives within ~10 lines
        // of the arm head.
        let window_end = (arm_idx + 600).min(PARSER_SRC.len());
        let window = &PARSER_SRC[arm_idx..window_end];
        let expected_pair = format!("({}, {})", expected_l_bp, expected_l_bp + 1);
        assert!(
            window.contains(&expected_pair),
            "parser arm `{token_label}` no longer contains literal `(l_bp, r_bp) = {expected_pair}`. \
             The parser bp ladder has shifted — update `bp::binop_l_bp` in src/formatter.rs \
             and `parser_binop_arms` in this test in lockstep."
        );
        // Also assert the `if l_bp < min_bp` guard is present so we know
        // we're looking at a real prec-climbing arm, not e.g. a comment.
        assert!(
            window.contains("if l_bp < min_bp"),
            "parser arm `{token_label}` does not contain `if l_bp < min_bp` guard \
             within 600 bytes — the arm structure has been refactored. Re-anchor \
             this test."
        );
    }
}

/// Test 1b: each `BinOp::<Variant>` mention in `parse_expr_bp` lives near
/// the expected bp value. The "near" check is by construction tied to the
/// arm windows enumerated above — every variant in an arm shares its bp.
#[test]
fn each_binop_variant_appears_near_its_expected_bp_in_parser() {
    for (token_label, variants, expected_l_bp) in parser_binop_arms() {
        let arm_idx = PARSER_SRC
            .find(token_label)
            .unwrap_or_else(|| panic!("missing arm label `{token_label}` in parser source"));
        let window_end = (arm_idx + 600).min(PARSER_SRC.len());
        let window = &PARSER_SRC[arm_idx..window_end];
        let bp_str = expected_l_bp.to_string();
        for variant in variants {
            assert!(
                window.contains(variant),
                "parser arm `{token_label}` window does not contain `{variant}` — \
                 the arm body no longer constructs that BinOp variant. \
                 Update `parser_binop_arms` to reflect the new mapping."
            );
            // The bp literal must appear in the same window as the variant.
            // (Both the `(N, N+1)` pair literal and the variant live in the
            // ~10-line arm body.)
            assert!(
                window.contains(&bp_str),
                "parser arm `{token_label}` window does not contain literal `{bp_str}` \
                 alongside variant `{variant}` — the bp value has shifted but the \
                 formatter's `bp::binop_l_bp` table has not been updated."
            );
        }
    }
}

/// Test 1c: the formatter source itself contains a `binop_l_bp` helper
/// inside `mod bp` whose match arms emit the same bp values. We do NOT
/// duplicate the parser's match here in test code — instead we read the
/// formatter source and assert the canonical (variant, bp) pairs exist
/// in its body. This pins both ends of the parity.
#[test]
fn formatter_binop_l_bp_matches_canonical_pairs() {
    const FORMATTER_SRC: &str = include_str!("../src/formatter.rs");
    let helper_idx = FORMATTER_SRC
        .find("pub fn binop_l_bp(op: BinOp) -> u8")
        .or_else(|| FORMATTER_SRC.find("fn binop_l_bp(op: BinOp) -> u8"))
        .expect(
            "formatter no longer contains `binop_l_bp` helper inside `mod bp`. \
             The Round 81 unification has been undone — re-introduce the helper \
             so all three BinOp bp tables share one source of truth.",
        );
    let body_end = (helper_idx + 800).min(FORMATTER_SRC.len());
    let body = &FORMATTER_SRC[helper_idx..body_end];
    for (_token_label, variants, expected_l_bp) in parser_binop_arms() {
        for variant in variants {
            assert!(
                body.contains(variant),
                "formatter `binop_l_bp` body no longer mentions `{variant}` — \
                 a BinOp variant was removed from the helper but should still map \
                 to bp {expected_l_bp}."
            );
        }
        assert!(
            body.contains(&format!(" {expected_l_bp},"))
                || body.contains(&format!("=> {expected_l_bp}")),
            "formatter `binop_l_bp` body no longer emits literal `{expected_l_bp}`. \
             The helper has drifted from the parser's bp ladder. Update it in \
             lockstep with `parse_expr_bp` arms in src/parser.rs."
        );
    }
}

// ---------- Test 2: behavioural round-trip / idempotency ----------

fn fmt(src: &str) -> String {
    format(src).unwrap_or_else(|e| panic!("format failed for `{src}`: {e:?}"))
}

fn assert_lex_parse_ok(src: &str) {
    let toks = Lexer::new(src)
        .tokenize()
        .unwrap_or_else(|e| panic!("lex failed for `{src}`: {e:?}"));
    Parser::new(toks)
        .parse_program()
        .unwrap_or_else(|e| panic!("parse failed for `{src}`: {e:?}"));
}

#[test]
fn binop_mixed_precedence_idempotent_and_no_spurious_parens() {
    // Each snippet exercises a different bp boundary. After the Round 81
    // refactor, the formatter MUST NOT add parens around the
    // higher-precedence child (no-op refactor on observable behaviour).
    let cases: &[(&str, &[&str])] = &[
        // Mul (80) inside Add (70): right child is higher precedence, no parens.
        (
            "fn main() { let x = 1 + 2 * 3 }",
            &["1 + 2 * 3"], // expected substring in output
        ),
        // Cmp (50) inside And (30): both children are higher precedence than parent.
        ("fn main() { let x = a == b && c < d }", &["a == b && c < d"]),
        // And (30) inside Or (20): right child is higher precedence, no parens.
        ("fn main() { let x = a || b && c }", &["a || b && c"]),
        // Mul (80) and Div (80) inside Add/Sub (70): all unparenthesised.
        ("fn main() { let x = a + b * c - d / e }", &["a + b * c - d / e"]),
        // Eq inside And, And inside Or — full ladder of decreasing prec.
        (
            "fn main() { let x = a || b && c == d }",
            &["a || b && c == d"],
        ),
        // Mod (80) at top level — bare `%` should not wrap.
        ("fn main() { let x = 10 % 3 }", &["10 % 3"]),
    ];
    for (src, must_contain) in cases {
        let first = fmt(src);
        let second = fmt(&first);
        assert_eq!(
            first, second,
            "formatter must be idempotent for `{src}`\n--- first ---\n{first}\n--- second ---\n{second}"
        );
        assert_lex_parse_ok(&first);
        for needle in *must_contain {
            assert!(
                first.contains(needle),
                "formatter output for `{src}` did not contain expected substring `{needle}`. \
                 This typically means a spurious paren was added around a higher-precedence \
                 child. Full output:\n{first}"
            );
        }
        // Locks "no spurious parens": none of the natural-form snippets
        // above should produce `((` anywhere in the let RHS.
        assert!(
            !first.contains("((") && !first.contains("))"),
            "formatter introduced double-parens for `{src}`. Output:\n{first}"
        );
    }
}

#[test]
fn binop_explicit_parens_are_preserved_when_needed() {
    // (1 + 2) * 3 — the parens are SEMANTICALLY required because the
    // child Add (70) is lower-precedence than the parent Mul (80) on the
    // left. The formatter must keep them.
    let src = "fn main() { let x = (1 + 2) * 3 }";
    let out = fmt(src);
    assert!(
        out.contains("(1 + 2) * 3"),
        "formatter dropped semantically-required parens around `1 + 2` in \
         `(1 + 2) * 3`. Output:\n{out}"
    );
    assert_eq!(out, fmt(&out), "idempotency");
    assert_lex_parse_ok(&out);
}

// ---------- Test 3: byte-identity output check ----------
//
// One pin per bp level (6 levels × 1-2 expressions). These were captured
// from the formatter immediately after the Round 81 refactor with the
// goal of locking the no-op property: any accidental shift in precedence
// handling produces different bytes and fails this test.

const FN_WRAP: &str = "fn main() {\n  let x = ";

fn pin(input_expr: &str, expected_expr: &str) {
    let src = format!("fn main() {{ let x = {input_expr} }}");
    let expected = format!("{FN_WRAP}{expected_expr}\n}}\n");
    let out = fmt(&src);
    assert_eq!(
        out, expected,
        "formatter byte-identity broken for `{input_expr}`.\n\
         expected:\n{expected}\nactual:\n{out}"
    );
}

#[test]
fn byte_identity_or_level() {
    pin("a || b", "a || b");
}

#[test]
fn byte_identity_and_level() {
    pin("a && b", "a && b");
}

#[test]
fn byte_identity_eq_level() {
    pin("a == b", "a == b");
    pin("a != b", "a != b");
}

#[test]
fn byte_identity_cmp_level() {
    pin("a < b", "a < b");
    pin("a >= b", "a >= b");
}

#[test]
fn byte_identity_add_level() {
    pin("1 + 2", "1 + 2");
    pin("a - b", "a - b");
}

#[test]
fn byte_identity_mul_level() {
    pin("2 * 3", "2 * 3");
    pin("10 % 3", "10 % 3");
}

#[test]
fn byte_identity_mixed_ladder() {
    // Full ladder from Or down to Mul. Verifies all 6 bp levels in one
    // expression with NO parens added (each child is higher-precedence
    // than its parent).
    pin(
        "a || b && c == d < e + f * g",
        "a || b && c == d < e + f * g",
    );
}

#[test]
fn byte_identity_required_parens_kept() {
    // Lower-precedence child on the left of a higher-precedence parent —
    // parens are semantically required and the formatter MUST keep them.
    pin("(1 + 2) * 3", "(1 + 2) * 3");
    pin("(a || b) && c", "(a || b) && c");
}
