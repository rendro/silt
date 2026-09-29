//! Round-88 dedup lock: the formatter's BinOp precedence ladder was
//! triple-encoded for a long time. Round 81 collapsed three of those
//! into `bp::binop_l_bp`, but a fourth — a `fn precedence(op: BinOp) ->
//! u8` returning a 1..=6 ladder — survived because it was used only by
//! a "same-family Binary-within-Binary" branch in
//! `format_expr_with_parens`, and that branch is fully subsumed by the
//! adjacent cross-family path via `paren_wrap_if_needed` /
//! `expr_top_l_bp` / `bp::binop_l_bp`.
//!
//! Equivalence argument (so any reader can audit the lock):
//!   - For a `Binary(_, child_op, _)` child,
//!     `expr_top_l_bp(child) == bp::binop_l_bp(child_op)`.
//!   - The cross-family branch wraps iff
//!     `expr_top_l_bp(child) < required`, where
//!     `required = parent_l_bp + (0 if is_left else 1)`.
//!   - For `is_left`:  wrap iff `binop_l_bp(child) <  binop_l_bp(parent)`.
//!     For `!is_left`: wrap iff `binop_l_bp(child) <= binop_l_bp(parent)`.
//!   - The deleted same-family branch wrapped iff
//!     `child_prec <  parent_prec`  (is_left) or
//!     `child_prec <= parent_prec`  (!is_left).
//!   - `precedence` (1..=6) and `binop_l_bp` (20,30,40,50,70,80) are
//!     strict-monotone re-codings of the same total order on `BinOp`,
//!     so the two predicates agree on every (parent, child, is_left)
//!     triple — the same-family branch is dead code.
//!
//! Behavioral byte-pin: format a corpus of nested-binary expressions
//! with mixed associativity and assert the emitted bytes match a
//! snapshot captured against the behavior preserved by the dedup.

use silt::formatter;

fn assert_byte_exact(name: &str, src: &str, expected: &str) {
    let actual = formatter::format(src).expect("format succeeds");
    assert_eq!(
        actual, expected,
        "byte-exact lock for {name} failed\n--- src ---\n{src}\n--- expected ---\n{expected}\n--- actual ---\n{actual}"
    );
    // Idempotency: formatting the formatted text yields the same bytes.
    let twice = formatter::format(&actual).expect("format pass 2");
    assert_eq!(
        actual, twice,
        "formatter not idempotent for {name}\n--- once ---\n{actual}\n--- twice ---\n{twice}"
    );
}

// ── Behavioral byte-pin: snapshot of formatter output for a corpus
//    of nested-binary expressions with mixed associativity. Snapshot
//    was captured by running `silt fmt` against the source BEFORE the
//    same-family branch / `fn precedence` were deleted. If any of
//    these assertions fail after the deletion, the dedup analysis was
//    wrong and the edit must be reverted. ─────────────────────────────

#[test]
fn round88_nested_binary_mixed_associativity_byte_exact() {
    // 4-space indent in; 2-space indent out (canonical silt indent).
    // Cases cover: left-assoc same-op chains (r1/r5/r6), mixed
    // precedence with the higher-prec op on either side (r2/r4),
    // explicit-paren preservation (r3/r8/r9/r13), and cross-family
    // logical/comparison interactions (r10/r11/r12/r14/r15).
    let src = "\
fn main() {
    let r1 = a + b + c
    let r2 = a + b * c
    let r3 = (a + b) * c
    let r4 = a * b + c
    let r5 = a - b - c
    let r6 = a / b / c
    let r7 = a + b - c
    let r8 = a - (b - c)
    let r9 = a * (b * c)
    let r10 = a && b || c
    let r11 = a || b && c
    let r12 = a == b && c == d
    let r13 = (a || b) && c
    let r14 = a < b || c > d
    let r15 = a + b == c + d
}
";
    let expected = "\
fn main() {
  let r1 = a + b + c
  let r2 = a + b * c
  let r3 = (a + b) * c
  let r4 = a * b + c
  let r5 = a - b - c
  let r6 = a / b / c
  let r7 = a + b - c
  let r8 = a - (b - c)
  let r9 = a * (b * c)
  let r10 = a && b || c
  let r11 = a || b && c
  let r12 = a == b && c == d
  let r13 = (a || b) && c
  let r14 = a < b || c > d
  let r15 = a + b == c + d
}
";
    assert_byte_exact("nested_binary_mixed_assoc", src, expected);
}
