//! Round-64 regression: the formatter's `ExprKind::Map` and
//! `ExprKind::SetLit` arms always emitted single-line `#{ k: v, ... }` /
//! `#[ ... ]` form, flattening multi-line user input and silently
//! dropping (Map) or relocating outside the literal (Set) any `--` /
//! `{- -}` comments attached to entries or sitting between them.
//!
//! Fix: `format_map_expr_if_multiline` and `format_set_expr_if_multiline`
//! mirror the existing list/tuple/record-create multi-line emitters —
//! when the literal spans multiple source lines AND has interior
//! comments or per-entry trailing comments, render one entry per line
//! with trivia preserved at its original attachment point.

use silt::formatter::format;
use silt::lexer::Lexer;
use silt::parser::Parser;

fn assert_formatted_parses(source: &str) {
    let formatted =
        format(source).unwrap_or_else(|e| panic!("format failed: {e:?}\nsource:\n{source}"));
    let tokens = Lexer::new(&formatted).tokenize().unwrap_or_else(|e| {
        panic!("formatted output failed to lex: {e:?}\nformatted:\n{formatted}")
    });
    Parser::new(tokens).parse_program().unwrap_or_else(|e| {
        panic!(
            "formatted output failed to parse: {e:?}\nsource:\n{source}\nformatted:\n{formatted}"
        )
    });
}

fn assert_idempotent(source: &str) {
    let first = format(source).unwrap_or_else(|e| panic!("first format failed: {e:?}"));
    let second =
        format(&first).unwrap_or_else(|e| panic!("second format failed: {e:?}\nfirst:\n{first}"));
    assert_eq!(
        first, second,
        "formatter must be idempotent\n---first---\n{first}\n---second---\n{second}"
    );
}

// ── Map literal: multi-line shape + trailing `--` per-entry comment ──

#[test]
fn test_map_multiline_with_trailing_comment_preserved() {
    // Pre-fix: the `--alpha` comment was silently dropped because the
    // single-line emitter never consulted the trailing-comment map. Fix
    // routes through `format_map_expr_if_multiline` which preserves it.
    let source = "fn main() {\n    let m = #{ \"a\": 1, -- alpha\n               \"b\": 2, }\n}\n";
    let formatted = format(source).expect("format succeeds");
    assert!(
        formatted.contains("-- alpha"),
        "trailing comment on map entry must survive:\n{formatted}"
    );
    // Multi-line shape: the map must NOT have collapsed to a single
    // `#{ ... }` line.
    assert!(
        formatted.contains("#{\n"),
        "map literal must keep multi-line shape:\n{formatted}"
    );
    assert_idempotent(source);
    assert_formatted_parses(source);
}

#[test]
fn test_map_multiline_without_comments_preserves_shape() {
    // No comments — but the source already spans multiple lines. The
    // formatter SHOULD still render single-line, because
    // `should_layout_multiline` only forces multi-line when there are
    // comments to preserve. Guard that behaviour: idempotency must hold,
    // and the output must re-parse cleanly.
    let source = "fn main() {\n    let m = #{\n        \"a\": 1,\n        \"b\": 2,\n    }\n}\n";
    assert_idempotent(source);
    assert_formatted_parses(source);
}

#[test]
fn test_map_single_line_stays_single_line() {
    // Single-line source must NOT be exploded into multi-line just
    // because the multi-line emitter exists.
    let source = "fn main() {\n    let m = #{ \"a\": 1, \"b\": 2 }\n}\n";
    let formatted = format(source).expect("format succeeds");
    assert!(
        formatted.contains("#{ \"a\": 1, \"b\": 2 }"),
        "single-line map literal must stay single-line:\n{formatted}"
    );
    assert_idempotent(source);
}

#[test]
fn test_map_multiline_standalone_interior_comment_preserved() {
    // A standalone comment between two entries must survive AND remain
    // INSIDE the literal — the existing `should_layout_multiline` rule
    // detects interior comments and routes to the multi-line emitter.
    let source = "fn main() {\n    let m = #{\n        \"a\": 1,\n        -- between\n        \"b\": 2,\n    }\n}\n";
    let formatted = format(source).expect("format succeeds");
    assert!(
        formatted.contains("-- between"),
        "interior standalone comment must survive:\n{formatted}"
    );
    // Comment must come AFTER the opening `#{` and BEFORE the closing `}`.
    let open = formatted.find("#{").expect("has `#{`");
    let close = formatted[open..].find('}').expect("has closing `}`") + open;
    let comment = formatted.find("-- between").expect("comment present");
    assert!(
        comment > open && comment < close,
        "comment must stay INSIDE the map literal:\n{formatted}"
    );
    assert_idempotent(source);
}

// ── Set literal: standalone leading comment must stay INSIDE ─────────

#[test]
fn test_set_multiline_with_leading_standalone_comment_preserved() {
    // Pre-fix: the standalone `-- standalone` comment was lifted out of
    // the literal (either dropped or attached to the surrounding
    // statement), changing attached-comment semantics. Fix forces multi-
    // line emission and emits the comment inside the brackets.
    let source =
        "fn main() {\n    let s = #[\n        -- standalone\n        1,\n        2,\n    ]\n}\n";
    let formatted = format(source).expect("format succeeds");
    assert!(
        formatted.contains("-- standalone"),
        "standalone comment must survive:\n{formatted}"
    );
    // The comment must remain INSIDE the literal: between `#[` and `]`.
    let open = formatted.find("#[").expect("has `#[`");
    let close_offset = formatted[open..].find(']').expect("has closing `]`");
    let close = open + close_offset;
    let comment = formatted.find("-- standalone").expect("comment present");
    assert!(
        comment > open && comment < close,
        "comment must stay INSIDE the set literal:\n{formatted}"
    );
    assert_idempotent(source);
    assert_formatted_parses(source);
}

#[test]
fn test_set_multiline_without_comments_idempotent() {
    let source = "fn main() {\n    let s = #[\n        1,\n        2,\n        3,\n    ]\n}\n";
    assert_idempotent(source);
    assert_formatted_parses(source);
}

#[test]
fn test_set_single_line_stays_single_line() {
    let source = "fn main() {\n    let s = #[1, 2, 3]\n}\n";
    let formatted = format(source).expect("format succeeds");
    assert!(
        formatted.contains("#[1, 2, 3]"),
        "single-line set literal must stay single-line:\n{formatted}"
    );
    assert_idempotent(source);
}

#[test]
fn test_set_multiline_with_trailing_per_entry_comment_preserved() {
    // Set element trailing comment also routes through the multi-line
    // emitter via `should_layout_multiline`'s trailing-comment branch.
    let source = "fn main() {\n    let s = #[\n        1, -- one\n        2,\n    ]\n}\n";
    let formatted = format(source).expect("format succeeds");
    assert!(
        formatted.contains("-- one"),
        "trailing per-entry comment on set element must survive:\n{formatted}"
    );
    assert_idempotent(source);
    assert_formatted_parses(source);
}

// ── Cross-cutting: idempotency on the exact bug repros ──────────────

#[test]
fn test_map_repro_idempotent_two_passes() {
    // Verbatim shape from the round-64 audit description.
    let source = "fn main() {\n    let m = #{ \"a\": 1, -- alpha\n               \"b\": 2, }\n}\n";
    let first = format(source).expect("first format");
    let second = format(&first).expect("second format");
    assert_eq!(
        first, second,
        "map repro must be idempotent\n---first---\n{first}\n---second---\n{second}"
    );
    // Every formatting pass must keep the comment.
    assert!(
        first.contains("-- alpha"),
        "pass 1 lost the comment:\n{first}"
    );
    assert!(
        second.contains("-- alpha"),
        "pass 2 lost the comment:\n{second}"
    );
}

#[test]
fn test_set_repro_idempotent_two_passes() {
    // Verbatim shape from the round-64 audit description.
    let source =
        "fn main() {\n    let s = #[\n        -- standalone\n        1,\n        2,\n    ]\n}\n";
    let first = format(source).expect("first format");
    let second = format(&first).expect("second format");
    assert_eq!(
        first, second,
        "set repro must be idempotent\n---first---\n{first}\n---second---\n{second}"
    );
    assert!(
        first.contains("-- standalone"),
        "pass 1 lost the comment:\n{first}"
    );
    assert!(
        second.contains("-- standalone"),
        "pass 2 lost the comment:\n{second}"
    );
}
