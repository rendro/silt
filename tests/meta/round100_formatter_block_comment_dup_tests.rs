//! Round-100 BROKEN: the formatter DUPLICATED a trailing `{- ... -}`
//! block comment that sat on a statement whose body expanded across
//! multiple output lines (a `match` / lambda that the formatter
//! re-flowed).
//!
//! Pre-fix repro: `let a = match x { 1 -> p _ -> q } {- block -}` round-
//! tripped to output containing TWO `{- block -}` — one relocated onto an
//! arm line by the main pretty-printer, plus a second re-inserted by
//! `splice_inline_block_comments`. The splice pass only checked whether
//! the comment was present in its OWN source token-gap; because the main
//! formatter had relocated it to an earlier gap, the gap-local check
//! missed it and re-spliced → duplicate (and non-idempotent output).
//!
//! Rounds 95/97 fixed the analogous `--` line-comment paths; the block-
//! comment splice path was the remaining hole. Fix: `splice_inline_block_
//! comments` now accounts for relocated comments with a global per-text
//! budget (occurrences already in the output minus same-gap matches), so a
//! comment the formatter moved to another gap is not re-inserted.
//!
//! These locks assert the comment survives exactly once and that
//! formatting is idempotent.

use silt::formatter;

fn count_occurrences(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

#[test]
fn trailing_block_comment_on_expanding_match_is_not_duplicated() {
    let src = "fn f() {\n  let a = match x { 1 -> p _ -> q } {- block -}\n  print(a)\n}\n";
    let out = formatter::format(src).expect("format");
    assert_eq!(
        count_occurrences(&out, "{- block -}"),
        1,
        "trailing block comment on an expanding match must appear exactly \
         once, not be duplicated by the splice pass.\n--- output ---\n{out}"
    );
}

#[test]
fn trailing_block_comment_on_expanding_lambda_is_not_duplicated() {
    let src = "fn f() {\n  let g = fn(x) { match x { 1 -> a _ -> b } } {- tail -}\n  g\n}\n";
    let out = formatter::format(src).expect("format");
    assert_eq!(
        count_occurrences(&out, "{- tail -}"),
        1,
        "trailing block comment on an expanding lambda must appear exactly \
         once.\n--- output ---\n{out}"
    );
}

#[test]
fn top_level_trailing_block_comment_on_expanding_match_is_not_duplicated() {
    let src = "let a = match x { 1 -> p _ -> q } {- c -}\n";
    let out = formatter::format(src).expect("format");
    assert_eq!(
        count_occurrences(&out, "{- c -}"),
        1,
        "top-level trailing block comment must appear exactly once.\n--- output ---\n{out}"
    );
}

#[test]
fn expanding_match_block_comment_is_idempotent() {
    let src = "fn f() {\n  let a = match x { 1 -> p _ -> q } {- block -}\n  print(a)\n}\n";
    let once = formatter::format(src).expect("format pass 1");
    let twice = formatter::format(&once).expect("format pass 2");
    assert_eq!(
        once, twice,
        "formatting must be idempotent for a trailing block comment on an \
         expanding match.\n--- once ---\n{once}\n--- twice ---\n{twice}"
    );
}
