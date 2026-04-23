//! Regression tests: the formatter must be idempotent on line comments
//! that contain embedded null bytes (`\0`). Discovered by audit round 50
//! fuzz repro.

use silt::formatter::format;

#[test]
fn fuzz_repro_null_byte_in_line_comment_is_idempotent() {
    // Decoded from the round-50 audit base64 repro.
    let source = b"fn anic() {\n--:\x00\x00 wodc lOktanic() {\n--:\x00\x00 wodc list\n-henath%\n\n-- Trait siorpt lifn s() {\n  {-\n tsim() {\n-\n-pcmm() {\n--:\x00n -} pcccc\n\n}\n";
    let source = std::str::from_utf8(source).expect("valid utf-8");
    let pass1 = match format(source) {
        Ok(s) => s,
        // If the formatter rejects the input with a parse/lex error that's
        // fine — the bug we're guarding against is a SILENT mutation on
        // successful re-formats, not a parse error.
        Err(_) => return,
    };
    let pass2 = format(&pass1).expect("pass1 should re-format cleanly");
    assert_eq!(
        pass1, pass2,
        "formatter must be idempotent on line comments with embedded NULs\n\
         --- PASS1 ---\n{:?}\n--- PASS2 ---\n{:?}\n",
        pass1, pass2
    );
}

#[test]
fn minimized_null_byte_in_line_comment_is_idempotent() {
    // Minimized variant: a single top-level declaration with a line
    // comment containing an embedded NUL byte inside what would be a
    // would-be block-comment terminator sequence. The NUL causes the
    // comment-splicer to mis-handle re-matching across passes.
    //
    // Keep this tight — if this starts failing we know the regression is
    // specifically about NUL-bearing line comments, not wider fuzz input.
    let source = "fn f() {\n--:\x00 -} x\n}\n";
    let pass1 = match format(source) {
        Ok(s) => s,
        Err(_) => return,
    };
    let pass2 = format(&pass1).expect("pass1 should re-format cleanly");
    assert_eq!(
        pass1, pass2,
        "minimized repro: NUL-bearing line comment must round-trip idempotently\n\
         --- PASS1 ---\n{:?}\n--- PASS2 ---\n{:?}\n",
        pass1, pass2
    );
}

/// Count source lines that contain a `--` line-comment marker, matching
/// `fuzz_invariants::comment_marker_count`'s naïve byte scan (one count
/// per line max, scanning to end-of-line after each `--`).
fn line_comment_line_count(source: &str) -> usize {
    source
        .as_bytes()
        .split(|&b| b == b'\n')
        .filter(|line| line.windows(2).any(|w| w == b"--"))
        .count()
}

#[test]
fn fuzz_repro_crash3_full_input_preserves_comments() {
    // Round 51 fuzz repro (CI run 24837931337): formatter dropped 3 of 43
    // line comments on round-trip. Two distinct loss patterns contributed:
    //
    //   1. A `-- cmt` line comment placed AFTER a bracket-open (`{`, `(`,
    //      `[`) on the SAME source line, with the bracket closing later.
    //      `extract_trailing_comment_from_line` refuses to claim it
    //      (`line_comment_bracket_depth > 0`), and nothing else ever
    //      sees the `--` because it's lexed as whitespace.
    //   2. A trailing `-- cmt` on the LAST line of a multi-line
    //      expression (e.g. `-\n b -- c`, where unary `-` lives on one
    //      line and its operand plus trailing comment on the next). The
    //      per-stmt trailing pickup only looked at `stmt_start_line`, so
    //      the comment on the end line stayed orphaned in `trailing_map`.
    //
    // Exercise the full original repro verbatim and assert that every
    // `--`-bearing source line survives round-trip.
    let source = include_bytes!("fixtures/formatter_null_comment_crash3.silt");
    let source = std::str::from_utf8(source).expect("valid utf-8");
    let pass1 = match format(source) {
        Ok(s) => s,
        Err(_) => return,
    };
    let pass2 = format(&pass1).expect("pass1 should re-format cleanly");
    assert_eq!(
        pass1, pass2,
        "formatter must be idempotent on the crash3 repro"
    );
    assert_eq!(
        line_comment_line_count(source),
        line_comment_line_count(&pass1),
        "line-comment-marker count must be preserved on pass 1"
    );
    // `{-` block-start count must also match.
    assert_eq!(
        source.matches("{-").count(),
        pass1.matches("{-").count(),
        "`{{-`-marker count must be preserved on pass 1"
    );
}

#[test]
fn bracket_interior_line_comment_on_open_line_is_preserved() {
    // Minimized synthetic: a `--` line comment that sits AFTER a
    // body-opening `{` on the SAME source line, with the matching `}`
    // on a later line. Before audit round 51 the comment was silently
    // dropped because `extract_trailing_comment_from_line` refuses to
    // claim comments at `bracket_depth > 0` (they'd mis-attach to the
    // enclosing statement) and no multi-line emitter ever saw the `--`.
    let source = "fn nc(){ --\n}";
    let pass1 = format(source).expect("should format");
    assert!(
        pass1.contains("--"),
        "`--` must survive round-trip; got {pass1:?}"
    );
    let pass2 = format(&pass1).expect("pass1 should re-format cleanly");
    assert_eq!(
        pass1, pass2,
        "bracket-interior line comment round-trip must be idempotent"
    );
    assert_eq!(
        line_comment_line_count(source),
        line_comment_line_count(&pass1)
    );
}

#[test]
fn multi_line_expression_trailing_line_comment_is_preserved() {
    // A unary `-` on line 2 with its operand on line 3 produces an
    // expression that spans lines 2-3. A trailing `-- c` on line 3 must
    // be attached to the collapsed expression so the `--` marker
    // survives. Before audit round 51 only `stmt_start_line` was
    // queried for trailing comments, so this case dropped the comment.
    let source = "fn a() {\n-\n b-- c\n}\n";
    let pass1 = format(source).expect("should format");
    assert!(
        pass1.contains("--"),
        "`--` must survive round-trip; got {pass1:?}"
    );
    let pass2 = format(&pass1).expect("pass1 should re-format cleanly");
    assert_eq!(
        pass1, pass2,
        "multi-line-expression trailing line-comment round-trip must be idempotent"
    );
    assert_eq!(
        line_comment_line_count(source),
        line_comment_line_count(&pass1)
    );
}

#[test]
fn trailing_line_comment_after_last_stmt_on_open_brace_line_is_preserved() {
    // Third sibling of the two classes fixed in commit da1c1ab: a trailing
    // `-- cmt` on the open-brace line AFTER a complete inner statement
    // (`fn f() { x -- cmt\n}`). The closing `}` is on a later line, so
    // `extract_trailing_comment_from_line` refuses to claim it
    // (`bracket_depth > 0` at the `--` because of the outer `{`), and the
    // bracket-interior helper only fires when the prev char is a bracket
    // opener — neither matches when the prev is real code like `x`.
    // Before the fix the comment vanished silently on round-trip.
    let source = "fn f() { x -- comment here\n }\n";
    let pass1 = format(source).expect("should format");
    assert!(
        pass1.contains("--"),
        "`--` must survive round-trip; got {pass1:?}"
    );
    let pass2 = format(&pass1).expect("pass1 should re-format cleanly");
    assert_eq!(
        pass1, pass2,
        "trailing-after-last-stmt-on-open-brace-line round-trip must be idempotent"
    );
    assert_eq!(
        line_comment_line_count(source),
        line_comment_line_count(&pass1)
    );
}

#[test]
fn trailing_line_comment_after_single_stmt_body_is_preserved() {
    // Variant of the third class where the body closer `}` appears on the
    // SAME source line as the trailing comment, e.g. `fn f() { x -- c }`.
    // Because `--` is a line comment to EOL, the lexer consumes the `}`
    // as part of the comment and the input no longer parses as a balanced
    // block. This is an intrinsic source-level issue, not a formatter
    // bug: the comment cannot coexist on the same line as the closer.
    //
    // The formatter must at minimum NOT silently succeed with a dropped
    // comment (which was the pre-fix misbehaviour for related inputs) —
    // it is acceptable for it to return a parse error. If a future
    // language change makes this parse, the assertion below should be
    // upgraded to require `--` survives in pass1 output.
    let source = "fn f() { x -- c }\n";
    match format(source) {
        Ok(pass1) => {
            // If it ever parses, the `--` must survive.
            assert!(
                pass1.contains("--"),
                "`--` must survive round-trip when input parses; got {pass1:?}"
            );
            let pass2 = format(&pass1).expect("pass1 should re-format cleanly");
            assert_eq!(pass1, pass2);
            assert_eq!(
                line_comment_line_count(source),
                line_comment_line_count(&pass1)
            );
        }
        Err(_) => {
            // Expected with today's lexer: `--` eats the `}` so the block
            // is unclosed. Not a silent mutation, so the formatter
            // invariant is intact.
        }
    }
}
