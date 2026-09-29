//! Round 81 regression locks: the formatter's BinOp binding-power
//! handling (unified into one `bp::binop_l_bp` table in round 81) must
//! agree with the parser's precedence ladder.
//!
//! The locks are behavioural:
//!
//! 1. **Round-trip / idempotency.** Format mixed-precedence BinOp
//!    snippets and assert `fmt(fmt(src)) == fmt(src)` and that
//!    higher-precedence children are NOT spuriously parenthesised on
//!    re-emit, while semantically required parens are kept.
//!
//! 2. **Output identity.** Pin exact formatter output bytes for a
//!    handful of expressions covering all 6 bp levels (Or/And/Eq/Cmp/
//!    Add/Mul).

use silt::formatter::format;
use silt::lexer::Lexer;
use silt::parser::Parser;

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
        (
            "fn main() { let x = a == b && c < d }",
            &["a == b && c < d"],
        ),
        // And (30) inside Or (20): right child is higher precedence, no parens.
        ("fn main() { let x = a || b && c }", &["a || b && c"]),
        // Mul (80) and Div (80) inside Add/Sub (70): all unparenthesised.
        (
            "fn main() { let x = a + b * c - d / e }",
            &["a + b * c - d / e"],
        ),
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
