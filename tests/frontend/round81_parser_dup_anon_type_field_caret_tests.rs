//! Regression test for ERR-GAP-2 (round 81): parser duplicate-field caret
//! span used to land *after* the duplicate type, on the trailing `,` or
//! `}`, because `parse_type_expr` discarded the ident span at the
//! anon-record-TYPE site (parser.rs:2063,2070) and used `self.span()`
//! after `parse_type_expr()` had advanced past the field's type
//! expression.
//!
//! Round 79 fixed the sibling LITERAL site at parser.rs:3424,3431 (see
//! `round79_parser_dup_field_caret_tests.rs`). Round 81 fixes the TYPE
//! site by capturing the field-name span from `expect_ident` and using
//! it on the duplicate-detection path.
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
        .expect_err("parser must reject duplicate anon-record type field")
}

/// Source whose duplicate-field error we want to inspect. The two `a`
/// fields appear inside an anonymous record TYPE expression in a fn
/// parameter type position. The expression `{ a: Int, a: String }` is
/// parsed by `parse_type_expr` (the anon-record-TYPE arm at
/// parser.rs:2047), distinct from the LITERAL arm fixed in round 79.
const SRC: &str = "fn pluck(r: { a: Int, a: String }) = r.a\n";

/// Compute the byte offset of the *second* `a` field name in SRC.
///
/// SRC layout (0-indexed):
///   "fn pluck(r: { a: Int, a: String }) = r.a\n"
///                  ^         ^
///                  first a   second a (the duplicate)
///
/// We find `" a: "` (space + a + colon + space) twice — first occurrence
/// is the first field, second is the duplicate.
fn second_a_offset() -> usize {
    let first = SRC.find(" a: ").expect("first ` a: ` must exist") + 1;
    let after_first = first + 1;
    let rel = SRC[after_first..]
        .find(" a: ")
        .expect("second ` a: ` must exist");
    after_first + rel + 1
}

#[test]
fn duplicate_anon_record_type_field_error_message_mentions_field_name() {
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
        err.message.contains("anon record type"),
        "expected error to mention `anon record type`, got: {}",
        err.message
    );
}

#[test]
fn duplicate_anon_record_type_field_caret_lands_on_second_field_name() {
    // LOAD-BEARING: this test fails before the fix (span points at `,` or
    // `}` after the duplicate type) and passes after (span points at the
    // second `a`).
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
        err.span.offset,
        want,
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
fn duplicate_anon_record_type_field_caret_slice_equals_field_name() {
    // The diagnostic span, when used to slice the source for one byte,
    // must yield the text "a" — the duplicate field name itself. (Span
    // carries only `offset` (plus line/col); the field name is a single
    // ASCII byte here, so a one-byte slice is exact.) This is the
    // precise shape the round-81 finding asks for.
    let err = parse_err(SRC);
    let start = err.span.offset;
    let end = (start + 1).min(SRC.len());
    let slice = &SRC[start..end];
    assert_eq!(
        slice, "a",
        "duplicate-field span must slice to the field name `a`, got {:?} \
         (offset {}). Source: {:?}",
        slice, err.span.offset, SRC,
    );
}

#[test]
fn duplicate_anon_record_type_field_caret_is_not_after_the_value() {
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
