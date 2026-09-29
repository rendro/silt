//! Lock tests for the formatter's inline `{- ... -}` block-comment
//! preservation logic and top-level line-comment trailing whitespace
//! normalization. These cover the fixes from audit round 17 findings F7
//! (mid-expression / parameter / argument / list / statement block
//! comments silently deleted) and F10 (top-level line comments kept
//! trailing whitespace that the body path stripped).
//!
//! Each F7 test asserts the block comment is still present after
//! `silt::formatter::format` runs, and that formatting is idempotent —
//! i.e. `format(format(source)) == format(source)`. The F10 test
//! asserts trailing ASCII whitespace on a top-level line comment is
//! stripped.

use silt::formatter;

fn fmt(src: &str) -> String {
    formatter::format(src).expect("format failed")
}

fn assert_idempotent(src: &str) {
    let once = fmt(src);
    let twice = fmt(&once);
    assert_eq!(
        once, twice,
        "format is not idempotent:\nfirst:\n{once}\nsecond:\n{twice}"
    );
}

#[test]
fn test_format_preserves_inline_block_in_parameter_list() {
    // The `{- the addend -}` block comment sits between the `a` param
    // and the `,` separator. Round 16's fix only handled leading inline
    // block comments on statement lines; mid-parameter-list block
    // comments were silently dropped by the main pretty-printer. The
    // post-processing splicer reintroduced in round 17 should restore
    // them.
    let src = "fn add(a {- the addend -}, b) { a + b }\n";
    let out = fmt(src);
    assert!(
        out.contains("{- the addend -}"),
        "expected inline block comment to survive formatting, got:\n{out}"
    );
    // Verify positional correctness: the comment stays between `a` and `,`.
    let a_pos = out.find('a').expect("missing `a`");
    let comment_pos = out.find("{- the addend -}").expect("missing comment");
    let comma_pos = out.find(',').expect("missing `,`");
    assert!(
        a_pos < comment_pos && comment_pos < comma_pos,
        "expected `a` < comment < `,`, got a={a_pos} cmt={comment_pos} comma={comma_pos} in:\n{out}"
    );
    assert_idempotent(src);
}

#[test]
fn test_format_preserves_inline_block_in_call_arguments() {
    // Two block comments inside a call argument list, one after each
    // positional arg. Both need to survive formatting in source order.
    let src = "fn add(a, b) { a + b }\n\
               fn main() {\n  \
               let r = add(1 {- one -}, 2 {- two -})\n  \
               println(r)\n\
               }\n";
    let out = fmt(src);
    assert!(
        out.contains("{- one -}"),
        "expected `{{- one -}}` to survive formatting, got:\n{out}"
    );
    assert!(
        out.contains("{- two -}"),
        "expected `{{- two -}}` to survive formatting, got:\n{out}"
    );
    // Ordering check: `{- one -}` before `{- two -}`.
    let one_pos = out.find("{- one -}").unwrap();
    let two_pos = out.find("{- two -}").unwrap();
    assert!(
        one_pos < two_pos,
        "expected `{{- one -}}` before `{{- two -}}` in:\n{out}"
    );
    assert_idempotent(src);
}

#[test]
fn test_format_preserves_inline_block_in_mid_expression() {
    let src = "fn main() { let x = 1 + {- middle -} 2 }\n";
    let out = fmt(src);
    assert!(
        out.contains("{- middle -}"),
        "expected `{{- middle -}}` to survive formatting, got:\n{out}"
    );
    // Positional check: sits between `+` and `2`.
    let plus_pos = out.find('+').expect("missing `+`");
    let cmt_pos = out.find("{- middle -}").unwrap();
    let two_pos = out.rfind('2').expect("missing `2`");
    assert!(
        plus_pos < cmt_pos && cmt_pos < two_pos,
        "expected `+` < `{{- middle -}}` < `2` in:\n{out}"
    );
    assert_idempotent(src);
}

#[test]
fn test_format_preserves_inline_block_in_list_literal() {
    let src = "fn main() { let xs = [1, {- mid -} 2, 3] }\n";
    let out = fmt(src);
    assert!(
        out.contains("{- mid -}"),
        "expected `{{- mid -}}` to survive formatting, got:\n{out}"
    );
    // Positional check: the comment sits between the first comma and `2`.
    let first_comma = out.find(',').expect("missing `,`");
    let cmt_pos = out.find("{- mid -}").unwrap();
    assert!(
        first_comma < cmt_pos,
        "expected first `,` before `{{- mid -}}` in:\n{out}"
    );
    // `2` should come after the comment.
    let two_idx = out[cmt_pos..].find('2').map(|k| k + cmt_pos);
    assert!(
        two_idx.is_some(),
        "expected `2` after `{{- mid -}}` in:\n{out}"
    );
    assert_idempotent(src);
}

#[test]
fn test_format_preserves_inline_block_trailing_on_top_level_stmt() {
    // This case is handled by the existing trailing-comment extractor
    // (the block comment's closer is followed only by whitespace), but
    // the lock test guarantees it stays preserved even after the
    // round-17 splicer rework.
    let src = "type X { a: Int } {- trailing -}\n";
    let out = fmt(src);
    assert!(
        out.contains("{- trailing -}"),
        "expected `{{- trailing -}}` to survive formatting, got:\n{out}"
    );
    // The comment should come after the closing `}` of the type body.
    let close_brace = out.rfind('}').expect("missing `}`");
    let cmt_pos = out.find("{- trailing -}").unwrap();
    // The comment is itself a block comment containing `}` at its end,
    // so `rfind('}')` picks the last one — inside the comment — but we
    // actually want the PREVIOUS `}` (the type body closer). Re-scan.
    let mut prev_close = None;
    for (idx, ch) in out.char_indices() {
        if ch == '}' && idx < cmt_pos {
            prev_close = Some(idx);
        }
    }
    let _ = close_brace;
    let type_close = prev_close.expect("missing type body `}` before comment");
    assert!(
        type_close < cmt_pos,
        "expected type body `}}` before `{{- trailing -}}` in:\n{out}"
    );
    assert_idempotent(src);
}

#[test]
fn test_format_strips_trailing_whitespace_in_top_level_line_comment() {
    // A top-level `-- comment` followed by ASCII spaces must be
    // normalized so the formatted output has no trailing whitespace,
    // matching the body-path `.trim()` behaviour.
    let src = "-- top comment   \nfn main() { 1 }\n";
    let out = fmt(src);
    let first_line = out.lines().next().expect("empty output");
    assert_eq!(
        first_line, "-- top comment",
        "expected trailing whitespace to be stripped from top-level line comment, got `{first_line}`"
    );
    // And explicitly no trailing space on the comment line anywhere.
    assert!(
        !out.contains("-- top comment  "),
        "output still contains trailing whitespace after the top-level comment:\n{out:?}"
    );
    assert_idempotent(src);
}
