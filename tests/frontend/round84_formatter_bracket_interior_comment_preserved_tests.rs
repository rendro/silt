//! Round-84 lock: the formatter must preserve `--` line comments that
//! sit interior to `(`, `[`, AND `{` openers on their own source line.
//!
//! Pre-round-84 behaviour:
//!   - `{ -- comment\n ... \n}` (block body opener) was preserved —
//!     `format_body`'s empty-body drain and `format_stmts_with_comments`
//!     both used `take_comments_between(open_line - 1, ...)` so a
//!     bracket-interior comment attributed to the open-brace's own line
//!     was picked up.
//!   - `( -- comment\n ... \n)` and `[ -- comment\n ... \n]` (tuple,
//!     list, set, call args, record-create, map literal) were SILENTLY
//!     DROPPED. Their multi-line emitters all started `prev_line =
//!     open_line` and used `should_layout_multiline(open_line, ...)`,
//!     both with STRICT lower bounds that excluded the open_line. The
//!     bracket-interior comment was attributed to that excluded line by
//!     `extract_bracket_interior_line_comment_from_line` (round 51), so
//!     it was orphaned in the standalone-comment pool with no consumer.
//!
//! Fix: lower `should_layout_multiline`'s `has_comments_between` lower
//! bound and each multi-line emitter's initial `prev_line` from
//! `open_line` to `open_line.saturating_sub(1)`. This matches the `{`
//! block-body behaviour the formatter has used for blocks since
//! audit-round-51.
//!
//! Lock cases:
//!   1. `(` opener — comment preserved, output is multi-line.
//!   2. `[` opener — same.
//!   3. `{` opener (block body) — regression guard that the existing
//!      behaviour did not change.
//!   4. `{` opener (record-create) — preserved.
//!   5. Idempotency: `format(format(s)) == format(s)` for each.
//!   6. The comment text survives byte-identically (not just "some `--`
//!      survived").

use silt::formatter;

fn assert_idempotent(src: &str) {
    let once = formatter::format(src).expect("format pass 1");
    let twice = formatter::format(&once).expect("format pass 2");
    assert_eq!(
        once, twice,
        "non-idempotent\n--- once ---\n{once}\n--- twice ---\n{twice}"
    );
}

#[test]
fn round84_paren_opener_interior_comment_preserved() {
    let src = "\
fn bri() {
  let xs = ( -- pending items
    1, 2, 3
  )
  println(xs)
}
";
    let out = formatter::format(src).expect("format");
    assert!(
        out.contains("-- pending items"),
        "paren-opener interior comment was dropped:\n{out}"
    );
    assert_idempotent(src);
}

#[test]
fn round84_bracket_opener_interior_comment_preserved() {
    let src = "\
fn bri() {
  let xs = [ -- pending items
    1, 2, 3
  ]
  println(xs)
}
";
    let out = formatter::format(src).expect("format");
    assert!(
        out.contains("-- pending items"),
        "bracket-opener interior comment was dropped:\n{out}"
    );
    assert_idempotent(src);
}

#[test]
fn round84_brace_opener_block_body_interior_comment_preserved() {
    // Regression guard: `{` openers worked before round 84; they must
    // continue to work after the fix.
    let src = "\
fn bri() { -- pending items
  let x = 1
  println(x)
}
";
    let out = formatter::format(src).expect("format");
    assert!(
        out.contains("-- pending items"),
        "brace-opener interior comment was dropped (regression):\n{out}"
    );
    assert_idempotent(src);
}

#[test]
fn round84_record_create_brace_opener_interior_comment_preserved() {
    let src = "\
type Point = { x: Int, y: Int }

fn make() -> Point {
  Point { -- start fields
    x: 1,
    y: 2,
  }
}
";
    let out = formatter::format(src).expect("format");
    assert!(
        out.contains("-- start fields"),
        "record-create brace-opener interior comment was dropped:\n{out}"
    );
    assert_idempotent(src);
}

#[test]
fn round84_call_paren_opener_interior_comment_preserved() {
    let src = "\
fn bri() {
  println( -- args
    1,
    2,
  )
}
";
    let out = formatter::format(src).expect("format");
    assert!(
        out.contains("-- args"),
        "call-paren-opener interior comment was dropped:\n{out}"
    );
    assert_idempotent(src);
}

#[test]
fn round84_set_opener_interior_comment_preserved() {
    // Set literal: `#[ -- ... ]`. The `[` opener is the one tracked.
    let src = "\
fn bri() {
  let s = #[ -- set members
    1,
    2,
    3,
  ]
  println(s)
}
";
    let out = formatter::format(src).expect("format");
    assert!(
        out.contains("-- set members"),
        "set-bracket-opener interior comment was dropped:\n{out}"
    );
    assert_idempotent(src);
}

#[test]
fn round84_map_opener_interior_comment_preserved() {
    // Map literal: `#{ -- ... }`. The `{` opener is the one tracked.
    let src = "\
fn bri() {
  let m = #{ -- map entries
    \"a\": 1,
    \"b\": 2,
  }
  println(m)
}
";
    let out = formatter::format(src).expect("format");
    assert!(
        out.contains("-- map entries"),
        "map-brace-opener interior comment was dropped:\n{out}"
    );
    assert_idempotent(src);
}
