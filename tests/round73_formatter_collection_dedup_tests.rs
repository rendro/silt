//! Round-73 dedup: the formatter's four collection-literal multi-line
//! emitters (list / tuple / map / set) shared ~45 of ~55 lines apiece —
//! same open/close-line scan, same `should_layout_multiline` short-
//! circuit, same source-trailing-comma probe, same per-element loop
//! shape, same tail-comment drain. Round 69 added the map/set siblings
//! by mirroring list/tuple instead of factoring; round 73 collapses
//! the four into thin wrappers over a single
//! `format_delimited_collection` helper.
//!
//! This is a pure refactor. There must be NO observable change in the
//! emitted bytes for any input — including the corner cases the four
//! emitters previously handled with their own divergent logic:
//!
//!   - List: handles `ListElem::Single` and `ListElem::Spread` (the
//!     latter rendered with a `..` prefix on the inner expression).
//!   - Tuple: forces a trailing comma on single-element tuples
//!     (`(x,)`) for grammar disambiguation, even when the source
//!     omitted it.
//!   - Map: anchors pre-element comment drain on the KEY's source line
//!     but trailing-comment lookup AND `prev_line` advancement on the
//!     VALUE's source line, so a `-- comment` after `"k": v,` (where
//!     value sits on the value's line, not the key's) binds correctly.
//!   - Set: emits the `#[ ... ]` shape; bracket scanner uses `[`/`]`
//!     because the leading `#` is a scalar character.
//!
//! Lock strategy:
//!   1. **Byte-pin lock** — the formatter output for a corpus of
//!      multi-line list / tuple / map / set inputs (with comments,
//!      trailing commas, single-elem tuple, nested) is asserted
//!      byte-identical to a string captured from the formatter BEFORE
//!      the refactor. Not a snapshot file — explicit `expected ==
//!      actual` `assert_eq!` so any future divergence shows in the diff.
//!   2. **Structural lock** — the helper exists in `src/formatter.rs`,
//!      AND each of the four wrappers is short (< 30 lines), proving
//!      the dedup actually happened (a future "I'll just inline it"
//!      regression would balloon the wrapper line count and trip this
//!      check).
//!   3. **Idempotency lock** — every byte-pin input is also
//!      `format(format(src)) == format(src)`. This is the same
//!      invariant the existing 78 `assert_idempotent` tests across the
//!      `tests/` tree enforce — adding one explicit check per input
//!      keeps the round-73 corpus self-contained.

use silt::formatter;

fn assert_idempotent(src: &str) {
    let once = formatter::format(src).expect("format pass 1");
    let twice = formatter::format(&once).expect("format pass 2");
    assert_eq!(
        once, twice,
        "formatter must be idempotent\nsrc:\n{src}\n--- pass 1 ---\n{once}\n--- pass 2 ---\n{twice}"
    );
}

fn assert_byte_exact(name: &str, src: &str, expected: &str) {
    let actual = formatter::format(src).expect("format succeeds");
    assert_eq!(
        actual, expected,
        "byte-exact lock for {name} failed\n--- src ---\n{src}\n--- expected ---\n{expected}\n--- actual ---\n{actual}"
    );
    assert_idempotent(src);
}

// ── Byte-pin lock: list literal, multi-line, trailing comments + commas ─

#[test]
fn round73_list_multiline_with_trailing_comments_byte_exact() {
    let src = "fn main() {\n    let xs = [\n        1, -- one\n        2,\n        3,\n    ]\n}\n";
    let expected = "fn main() {\n  let xs = [\n    1, -- one\n    2,\n    3,\n  ]\n}\n";
    assert_byte_exact("list_trailing", src, expected);
}

#[test]
fn round73_list_multiline_with_standalone_comment_byte_exact() {
    let src = "fn main() {\n    let xs = [\n        -- header\n        1,\n        2,\n    ]\n}\n";
    let expected = "fn main() {\n  let xs = [\n    -- header\n    1,\n    2,\n  ]\n}\n";
    assert_byte_exact("list_standalone", src, expected);
}

// ── Byte-pin lock: tuple literal, multi-line + single-elem disambiguator ─

#[test]
fn round73_tuple_multiline_with_trailing_comments_byte_exact() {
    let src = "fn main() {\n    let t = (\n        1, -- one\n        2,\n        3,\n    )\n}\n";
    let expected = "fn main() {\n  let t = (\n    1, -- one\n    2,\n    3,\n  )\n}\n";
    assert_byte_exact("tuple_trailing", src, expected);
}

#[test]
fn round73_tuple_single_elem_keeps_disambiguating_comma_byte_exact() {
    // The `(x,)` disambiguator must survive even when wrapped multi-line
    // by an interior standalone comment. `force_last_comma=true` is the
    // only divergence between tuple and list emitters; this lock catches
    // any regression that drops it.
    let src = "fn main() {\n    let t = (\n        -- only\n        42,\n    )\n}\n";
    let expected = "fn main() {\n  let t = (\n    -- only\n    42,\n  )\n}\n";
    assert_byte_exact("tuple_single", src, expected);
}

// ── Byte-pin lock: map literal, KEY-anchored pre, VALUE-anchored end ─

#[test]
fn round73_map_multiline_trailing_comment_on_value_line_byte_exact() {
    // Map's defining divergence: pre-comment drain anchors on the KEY
    // line, but trailing-comment lookup and `prev_line` advancement use
    // the VALUE line. With key+value on the same source line the two
    // collapse, but the helper must still wire them through correctly.
    let src = "fn main() {\n    let m = #{ \"a\": 1, -- alpha\n               \"b\": 2, }\n}\n";
    let expected = "fn main() {\n  let m = #{\n    \"a\": 1, -- alpha\n    \"b\": 2,\n  }\n}\n";
    assert_byte_exact("map_trailing", src, expected);
}

#[test]
fn round73_map_multiline_standalone_interior_comment_byte_exact() {
    let src = "fn main() {\n    let m = #{\n        \"a\": 1,\n        -- between\n        \"b\": 2,\n    }\n}\n";
    let expected = "fn main() {\n  let m = #{\n    \"a\": 1,\n    -- between\n    \"b\": 2,\n  }\n}\n";
    assert_byte_exact("map_standalone", src, expected);
}

// ── Byte-pin lock: set literal `#[ ... ]` ─────────────────────────────

#[test]
fn round73_set_multiline_with_trailing_comment_byte_exact() {
    let src = "fn main() {\n    let s = #[\n        1, -- one\n        2,\n    ]\n}\n";
    let expected = "fn main() {\n  let s = #[\n    1, -- one\n    2,\n  ]\n}\n";
    assert_byte_exact("set_trailing", src, expected);
}

#[test]
fn round73_set_multiline_with_standalone_comment_byte_exact() {
    let src =
        "fn main() {\n    let s = #[\n        -- standalone\n        1,\n        2,\n    ]\n}\n";
    let expected = "fn main() {\n  let s = #[\n    -- standalone\n    1,\n    2,\n  ]\n}\n";
    assert_byte_exact("set_standalone", src, expected);
}

// ── Byte-pin lock: nested collection (list inside map) ────────────────

#[test]
fn round73_nested_list_in_map_byte_exact() {
    // Cross-cutting: nested call ordering is the most fragile aspect of
    // the refactor. The outer map's per-entry render closure invokes
    // `format_expr` on the value (which itself contains an inner list
    // emitter); the helper MUST run the closure inside its per-element
    // loop (after the pre-comment drain, before the trailing-comment
    // probe), not eagerly upfront — otherwise the inner list's nested
    // comment consumption races the outer map's pre-comment drain. This
    // input would diverge if the helper ever switched back to eager
    // rendering.
    let src = "fn main() {\n    let m = #{\n        \"xs\": [\n            1, -- one\n            2,\n        ], -- xs\n        \"y\": 99,\n    }\n}\n";
    let expected = "fn main() {\n  let m = #{ \"xs\": [\n    1, -- one\n    2,\n  ], \"y\": 99, }\n}\n";
    assert_byte_exact("nested_list_in_map", src, expected);
}

// ── Structural lock: helper exists, wrappers are thin ────────────────

#[test]
fn round73_format_delimited_collection_helper_exists() {
    // The helper must exist by name in src/formatter.rs. A future
    // refactor that renames or removes it has to also update this lock
    // (and presumably re-prove the dedup invariant).
    let src = include_str!("../src/formatter.rs");
    assert!(
        src.contains("fn format_delimited_collection"),
        "expected `fn format_delimited_collection` helper in src/formatter.rs"
    );
}

#[test]
fn round73_collection_emitters_are_thin_wrappers() {
    // Each of the four emitter functions must be a thin wrapper over the
    // shared helper. We bound each at < 30 source lines (counted from
    // the `fn ...` line through its closing `}`). The pre-refactor
    // bodies were 50–60 lines; thirty leaves comfortable headroom for
    // doc-comment trim or argument-name tweaks while still tripping any
    // attempted re-inlining.
    let src = include_str!("../src/formatter.rs");
    for name in [
        "format_list_expr_if_multiline",
        "format_tuple_expr_if_multiline",
        "format_map_expr_if_multiline",
        "format_set_expr_if_multiline",
    ] {
        let body_lines = count_fn_body_lines(src, name);
        assert!(
            body_lines < 30,
            "expected `{name}` to be a thin wrapper (< 30 lines), got {body_lines}"
        );
    }
}

/// Count the number of source lines from the `fn <name>` definition
/// through its matching closing `}` (inclusive). Uses brace-depth
/// counting on the substring after the `fn` line so attribute lines or
/// preceding doc-comments are ignored.
fn count_fn_body_lines(src: &str, name: &str) -> usize {
    let needle = format!("fn {name}(");
    let start = src
        .find(&needle)
        .unwrap_or_else(|| panic!("function `{name}` not found in source"));
    let after = &src[start..];
    let mut depth: i32 = 0;
    let mut seen_open = false;
    let mut byte_end = 0usize;
    for (i, ch) in after.char_indices() {
        match ch {
            '{' => {
                depth += 1;
                seen_open = true;
            }
            '}' => {
                depth -= 1;
                if seen_open && depth == 0 {
                    byte_end = i + 1;
                    break;
                }
            }
            _ => {}
        }
    }
    assert!(
        byte_end > 0,
        "could not find matching `}}` for fn `{name}`"
    );
    after[..byte_end].lines().count()
}
