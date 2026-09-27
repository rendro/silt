//! Lock: `Token`'s `Display` impl must not embed raw control bytes or
//! raw newlines from string-token payloads into parser diagnostics.
//!
//! `scan_string` pushes ANY char into a string body (no control-char
//! gate), so a source file containing `"a<U+0001>b"` used to produce a
//! parser error (`expected declaration, found "a<U+0001>b"`) with the
//! raw control byte embedded between the quotes — an invisible
//! offender in terminals, log files, and LSP JSON. A raw newline in
//! the payload split the quoted found-token across the diagnostic
//! header and its continuation line, mangling the rendered quote.
//!
//! The fix (src/lexer.rs::escape_control_chars, applied in the
//! `StringLit`/`StringStart`/`StringMiddle`/`StringEnd` Display arms)
//! escapes control characters via `escape_default` while leaving all
//! printable characters — including non-ASCII — untouched.
//!
//! These tests drive the live lexer -> parser pipeline (the same path
//! `silt check` takes to a rendered diagnostic) rather than formatting
//! a hand-built token, so they lock the behavior a user actually sees.

use silt::lexer::Lexer;
use silt::parser::Parser;

/// Lex + parse with the recovering entry point and return every
/// collected parse-error message.
fn parse_errors(input: &str) -> Vec<String> {
    let tokens = Lexer::new(input).tokenize().expect("lexer");
    let (_program, errors) = Parser::new(tokens).parse_program_recovering();
    errors.into_iter().map(|e| e.message).collect()
}

/// A raw U+0001 inside a top-level string literal must surface in the
/// `expected declaration, found …` diagnostic as the visible `\u{1}`
/// escape, never as the raw control byte.
#[test]
fn control_byte_in_string_found_token_is_escaped() {
    let errs = parse_errors("\"a\u{1}b\"\n");
    let joined = errs.join("\n---\n");
    assert!(
        errs.iter().any(|e| e.contains(r#""a\u{1}b""#)),
        "diagnostic must render the string payload's U+0001 as the \
         literal `\\u{{1}}` escape, got:\n{joined}"
    );
    assert!(
        !errs.iter().any(|e| e.contains('\u{1}')),
        "no diagnostic may embed a raw U+0001 control byte, got:\n{joined:?}"
    );
}

/// A raw newline inside a string literal (permitted by `scan_string`)
/// must render as `\n` so the quoted found-token stays on one line of
/// the diagnostic instead of splitting across the header and the
/// `= note:` continuation.
#[test]
fn raw_newline_in_string_found_token_stays_on_one_line() {
    let errs = parse_errors("\"a\nb\"\n");
    let joined = errs.join("\n---\n");
    assert!(
        errs.iter().any(|e| e.contains(r#""a\nb""#)),
        "diagnostic must render the payload's raw newline as `\\n`, \
         got:\n{joined}"
    );
    let found_msg = errs
        .iter()
        .find(|e| e.contains("expected declaration"))
        .unwrap_or_else(|| panic!("expected a top-level declaration error, got:\n{joined}"));
    assert!(
        !found_msg.contains('\n'),
        "the found-token quote must stay on one line, got: {found_msg:?}"
    );
}

/// Same gate for the `StringStart` arm: an interpolated string whose
/// leading segment carries a control byte must escape it too.
#[test]
fn control_byte_in_interpolated_string_start_is_escaped() {
    let errs = parse_errors("\"a\u{1}{x}\"\n");
    let joined = errs.join("\n---\n");
    assert!(
        errs.iter().any(|e| e.contains(r#""a\u{1}{"#)),
        "StringStart diagnostic must escape the U+0001 payload byte, \
         got:\n{joined}"
    );
    assert!(
        !errs.iter().any(|e| e.contains('\u{1}')),
        "no diagnostic may embed a raw U+0001 control byte, got:\n{joined:?}"
    );
}

/// Positive control: a plain printable string still renders verbatim —
/// no escaping is introduced for the common case.
#[test]
fn plain_string_found_token_renders_unescaped() {
    let errs = parse_errors("\"hello\"\n");
    let joined = errs.join("\n---\n");
    let found_msg = errs
        .iter()
        .find(|e| e.contains("expected declaration"))
        .unwrap_or_else(|| panic!("expected a top-level declaration error, got:\n{joined}"));
    assert!(
        found_msg.contains(r#"found "hello""#),
        "plain string must render verbatim, got: {found_msg:?}"
    );
    assert!(
        !found_msg.contains('\\'),
        "plain string must not gain any escapes, got: {found_msg:?}"
    );
}

/// Printable non-ASCII passes through unchanged — only control
/// characters are escaped.
#[test]
fn non_ascii_printable_string_found_token_renders_plain() {
    let errs = parse_errors("\"héllo—wörld\"\n");
    let joined = errs.join("\n---\n");
    assert!(
        errs.iter().any(|e| e.contains(r#""héllo—wörld""#)),
        "printable non-ASCII must render plain (no escaping), got:\n{joined}"
    );
    assert!(
        !errs.iter().any(|e| e.contains(r"\u{")),
        "printable non-ASCII must not be escaped, got:\n{joined:?}"
    );
}
