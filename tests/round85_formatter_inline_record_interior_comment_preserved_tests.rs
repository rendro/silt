//! Round-85 lock: extend the round-84 bracket-interior comment
//! preservation fix to the two `{` opener cases the round-84 patch
//! missed — INLINE `RecordUpdate` (`expr.{ field: ... }`) and INLINE
//! `AnonRecord` (`{ field: ... }`).
//!
//! Pre-round-85 behaviour:
//!   - `RecordCreate` (`Name { ... }`), block bodies, calls, lists,
//!     tuples, sets, maps — all preserved their bracket-interior `--`
//!     comments after round 84.
//!   - `RecordUpdate` and `AnonRecord` SILENTLY DROPPED them. The
//!     round-83 scanner correctly recorded the comment, but the
//!     emitter for these two `ExprKind` variants (`format_expr_inner`)
//!     had ONLY a single-line form with no multi-line fallback —
//!     nothing consumed the recorded standalone comment, so it was
//!     orphaned in the comment pool.
//!
//! Fix: add `format_record_update_expr_if_multiline` and
//! `format_anon_record_expr_if_multiline`, modeled byte-for-byte on
//! the existing `format_record_create_expr_if_multiline`. Each is
//! invoked from `format_expr` before falling through to the inline
//! emitter in `format_expr_inner`.
//!
//! Lock cases:
//!   1. Inline anon-record with interior comment preserved + idempotent.
//!   2. Inline record-update with interior comment preserved + idempotent.
//!   3. Anon-record with spread head + interior comment preserved.

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
fn inline_anon_record_with_interior_comment_preserved() {
    let src = "\
fn main() {
  let r = { -- header
    a: 1
  }
  r
}
";
    let out = formatter::format(src).expect("format");
    assert!(
        out.contains("-- header"),
        "anon-record brace-opener interior comment was dropped:\n{out}"
    );
    assert_idempotent(src);
}

#[test]
fn inline_record_update_with_interior_comment_preserved() {
    let src = "\
type Q = { x: Int, y: Int }

fn main() {
  let q = Q { x: 1, y: 2 }
  let p = q.{ -- update
    x: 10
  }
  p
}
";
    let out = formatter::format(src).expect("format");
    assert!(
        out.contains("-- update"),
        "record-update brace-opener interior comment was dropped:\n{out}"
    );
    assert_idempotent(src);
}

#[test]
fn inline_anon_record_with_spread_and_interior_comment_preserved() {
    let src = "\
fn main() {
  let other = { a: 1, b: 2 }
  let r = { -- overlay
    ...other,
    a: 99
  }
  r
}
";
    let out = formatter::format(src).expect("format");
    assert!(
        out.contains("-- overlay"),
        "anon-record (with spread) brace-opener interior comment was dropped:\n{out}"
    );
    assert_idempotent(src);
}
