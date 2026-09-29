//! Round-97 lock: the formatter must preserve a trailing `--` line
//! comment that sits on the CLOSING-delimiter line of a multi-line
//! call / collection / record that the formatter then collapses onto a
//! single line.
//!
//! Pre-round-97 behaviour (BROKEN): a `--` on the close-delimiter line —
//! a line NO element occupies — was SILENTLY DROPPED on round-trip:
//!
//! ```text
//! fn main() {
//!   bar(
//!     1
//!   ) -- note      <- dropped
//! }
//! ```
//!
//! Root cause: `expr_max_line` for `Call` / `List` / `Tuple` / `Map` /
//! `SetLit` / `RecordCreate` / `RecordUpdate` / `AnonRecord` visited
//! only the callee/args/elements and never added the construct's own
//! closing-bracket line. The statement-level drain therefore computed
//! `end_line = stmt_end_line(stmt)` = the last element line and never
//! reached `take_trailing_for_line(close_line)`; `take_comments_between`
//! is exclusive of the close line, so `trailing_map[close_line]` was
//! never consumed and the comment was lost.
//!
//! Fix: mirror the existing Lambda/Block handling in `expr_max_line` —
//! add the closing-bracket line (via `compute_bracket_end_line`) to the
//! running max for the bracketed-collection arms so the statement drain
//! reaches the orphaned `trailing_map` entry.
//!
//! This is DISTINCT from the round-95 fix (`--` on an ELEMENT line such
//! as `[1, 2, 3 -- c]`); here the `--` is on the close-delimiter line.
//! These tests also guard idempotency.

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
fn round97_call_close_line_trailing_comment_preserved() {
    let src = "\
fn main() {
  bar(
    1
  ) -- note
}
";
    let out = formatter::format(src).expect("format");
    assert!(
        out.contains("-- note"),
        "call close-line trailing comment was dropped:\n{out}"
    );
    assert_idempotent(src);
}

#[test]
fn round97_list_close_line_trailing_comment_preserved() {
    let src = "\
fn main() {
  let xs = [
    1
  ] -- note
}
";
    let out = formatter::format(src).expect("format");
    assert!(
        out.contains("-- note"),
        "list close-line trailing comment was dropped:\n{out}"
    );
    assert_idempotent(src);
}

#[test]
fn round97_record_create_close_line_trailing_comment_preserved() {
    let src = "\
type Point = { x: Int, y: Int }

fn make() -> Point {
  let p = Point {
    x: 1,
    y: 2
  } -- built
  p
}
";
    let out = formatter::format(src).expect("format");
    assert!(
        out.contains("-- built"),
        "record-create close-line trailing comment was dropped:\n{out}"
    );
    assert_idempotent(src);
}

#[test]
fn round97_tuple_close_line_trailing_comment_preserved() {
    let src = "\
fn main() {
  let t = (
    1,
    2
  ) -- pair
}
";
    let out = formatter::format(src).expect("format");
    assert!(
        out.contains("-- pair"),
        "tuple close-line trailing comment was dropped:\n{out}"
    );
    assert_idempotent(src);
}

#[test]
fn round97_map_close_line_trailing_comment_preserved() {
    let src = "\
fn main() {
  let m = #{
    \"a\": 1
  } -- table
}
";
    let out = formatter::format(src).expect("format");
    assert!(
        out.contains("-- table"),
        "map close-line trailing comment was dropped:\n{out}"
    );
    assert_idempotent(src);
}

#[test]
fn round97_set_close_line_trailing_comment_preserved() {
    let src = "\
fn main() {
  let s = #[
    1
  ] -- members
}
";
    let out = formatter::format(src).expect("format");
    assert!(
        out.contains("-- members"),
        "set close-line trailing comment was dropped:\n{out}"
    );
    assert_idempotent(src);
}

#[test]
fn round97_round95_element_line_comment_still_preserved() {
    // Regression guard: the round-95 case (`--` on an ELEMENT line, not
    // the close line) must still be preserved by the round-97 change.
    let src = "\
fn main() {
  let xs = [
    1, 2, 3 -- elems
  ]
}
";
    let out = formatter::format(src).expect("format");
    assert!(
        out.contains("-- elems"),
        "round-95 element-line comment regressed:\n{out}"
    );
    assert_idempotent(src);
}
