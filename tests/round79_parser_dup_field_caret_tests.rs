//! Regression test for ERR-G1+G2 (round 79): parser duplicate-field caret
//! span used to land *after* the duplicate value, on the trailing `,` or
//! `}`, because `parse_anon_record_literal` discarded the ident span and
//! used `self.span()` after `parse_expr()` had advanced past the value.
//!
//! After the fix, the error span points at the second `a` token — the
//! actual duplicate field name — exactly like the sibling site in
//! `parse_trait_decl` (src/parser.rs:1696-1701) does for duplicate type
//! variables.
//!
//! Bug shape: span/caret position. Wording is unchanged.
//!
//! The lock is load-bearing: it must FAIL before the fix and PASS after.
//! It computes the byte offset of the second `a` deterministically from
//! the literal source string and asserts the ParseError span starts at
//! that offset, NOT at the trailing `,` or `}`.

use silt::lexer::Lexer;
use silt::parser::{ParseError, Parser};

fn parse_err(src: &str) -> ParseError {
    let tokens = Lexer::new(src)
        .tokenize()
        .expect("lexer must accept source");
    Parser::new(tokens)
        .parse_program()
        .expect_err("parser must reject duplicate anon-record field")
}

/// Source whose duplicate-field error we want to inspect. The two `a`
/// fields are separated by `, ` so each character offset is easy to
/// reason about. The expression `{ a: 1, a: 2 }` is parsed as an
/// anonymous record literal (sigil-less `{Ident COLON ...}`).
///
/// Note: silt rejects `fn main() = { ... }` outright (one shape per
/// construct — the `=` form is for non-block expressions only), so we
/// embed the offending literal inside a block body using `let`.
const SRC: &str = "fn main() {\n  let r = { a: 1, a: 2 }\n}\n";

/// Compute the byte offset of the *second* `a` token in SRC.
///
/// SRC layout (0-indexed):
///   "fn main() = { a: 1, a: 2 }\n"
///                  ^         ^
///                  first a   second a
fn second_a_offset() -> usize {
    let first = SRC.find(" a: ").expect("first ` a: ` must exist") + 1;
    let after_first = first + 1;
    let rel = SRC[after_first..]
        .find(" a: ")
        .expect("second ` a: ` must exist");
    after_first + rel + 1
}

#[test]
fn duplicate_anon_record_field_error_message_mentions_field_name() {
    // Sanity: confirm the parser produces the expected wording. This
    // protects against accidental message drift while we focus the lock
    // below on span position.
    let err = parse_err(SRC);
    assert!(
        err.message.contains("duplicate field 'a'"),
        "expected error to mention `duplicate field 'a'`, got: {}",
        err.message
    );
    assert!(
        err.message.contains("anon record literal"),
        "expected error to mention `anon record literal`, got: {}",
        err.message
    );
}

#[test]
fn duplicate_anon_record_field_caret_lands_on_second_field_name() {
    // LOAD-BEARING: this test fails before the fix (span points at `,` or
    // `}` after the duplicate `2` value) and passes after (span points
    // at the second `a`).
    let err = parse_err(SRC);
    let want = second_a_offset();

    // Self-check the offset arithmetic against the source so a future
    // edit to SRC fails loudly here rather than silently passing.
    assert_eq!(
        SRC.as_bytes()[want],
        b'a',
        "second-a offset arithmetic is wrong: byte at {} is {:?}, source: {:?}",
        want,
        SRC.as_bytes()[want] as char,
        SRC
    );

    assert_eq!(
        err.span.offset, want,
        "duplicate-field error span must point at the second `a` (offset {}), \
         not at the trailing `,` or `}}`. \
         Got offset {} (byte {:?}). Full source: {:?}. Error: {}",
        want,
        err.span.offset,
        SRC.as_bytes().get(err.span.offset).map(|b| *b as char),
        SRC,
        err.message,
    );
}

#[test]
fn duplicate_anon_record_field_caret_is_not_after_the_value() {
    // Belt-and-braces lock: independently of the precise expected offset,
    // the caret must NOT land on a `,` or `}` byte. This catches the
    // pre-fix behaviour even if SRC ever changes shape.
    let err = parse_err(SRC);
    let byte = SRC.as_bytes().get(err.span.offset).copied();
    assert!(
        byte != Some(b',') && byte != Some(b'}'),
        "duplicate-field caret landed on terminator byte {:?} at offset {}; \
         it must point at the duplicate field name. Source: {:?}. Error: {}",
        byte.map(|b| b as char),
        err.span.offset,
        SRC,
        err.message,
    );
}
