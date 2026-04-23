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
