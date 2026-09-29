//! Lock: the formatter must preserve and correctly attribute `--` line
//! comments that trail the LAST element on a source line shared by
//! multiple elements of a horizontally-laid-out delimited construct
//! (list `[]`, tuple `()`, set `#[]`, map `#{}`, call args, fn params).
//!
//! This is the element-line / multiple-elements-per-line shape, distinct
//! from the round-84 opener-line shape (`[ -- x\n ... ]`) which is locked
//! by `round84_formatter_bracket_interior_comment_preserved_tests.rs`.
//!
//! Two bugs were fixed:
//!
//! Bug A — SILENT DROP. Multiple elements on one source line followed by
//! a trailing `--` comment, with the close delimiter on the next line,
//! collapsed the collection to a single line and the comment VANISHED.
//! Root cause: a `--` after an element inside an unclosed `(`/`[` on the
//! SAME line was claimed by NO extractor (the plain trailing extractor
//! refuses at bracket_depth > 0, the bracket-interior extractor only
//! claims when the prev char is an opener, the block-body extractor only
//! claims for `{` block bodies). Fixed by
//! `extract_collection_interior_trailing_comment_from_line`, which records
//! it as a trailing comment so the multi-line emitter both forces a
//! multi-line layout and re-emits the comment.
//!
//! Bug B — MISATTRIBUTION. When exploding a multi-element line, the
//! trailing comment bound to the FIRST element (the first element on the
//! line called `take_trailing_for_line` and consumed the line's comment).
//! Fixed by an `is_last_on_line` gate in every per-element emitter so only
//! the LAST element sharing a source line may claim that line's comment.

use silt::formatter;

fn assert_idempotent(src: &str) {
    let once = formatter::format(src).expect("format pass 1");
    let twice = formatter::format(&once).expect("format pass 2");
    assert_eq!(
        once, twice,
        "non-idempotent\n--- once ---\n{once}\n--- twice ---\n{twice}"
    );
}

// ── Bug A: comment must SURVIVE (no silent drop) ─────────────────────

#[test]
fn bug_a_list_trailing_comment_survives() {
    let src = "\
fn main() {
  let xs = [1, 2, 3 -- belongs to 3
  ]
  println(xs)
}
";
    let out = formatter::format(src).expect("format");
    assert!(
        out.contains("-- belongs to 3"),
        "list element-line trailing comment was silently dropped:\n{out}"
    );
    assert_idempotent(src);
}

#[test]
fn bug_a_tuple_trailing_comment_survives() {
    let src = "\
fn main() {
  let xs = (1, 2, 3 -- belongs to 3
  )
  println(xs)
}
";
    let out = formatter::format(src).expect("format");
    assert!(
        out.contains("-- belongs to 3"),
        "tuple element-line trailing comment was silently dropped:\n{out}"
    );
    assert_idempotent(src);
}

#[test]
fn bug_a_set_trailing_comment_survives() {
    let src = "\
fn main() {
  let xs = #[1, 2, 3 -- belongs to 3
  ]
  println(xs)
}
";
    let out = formatter::format(src).expect("format");
    assert!(
        out.contains("-- belongs to 3"),
        "set element-line trailing comment was silently dropped:\n{out}"
    );
    assert_idempotent(src);
}

#[test]
fn bug_a_call_args_trailing_comment_survives() {
    let src = "\
fn main() {
  foo(1, 2, 3 -- belongs to 3
  )
}
";
    let out = formatter::format(src).expect("format");
    assert!(
        out.contains("-- belongs to 3"),
        "call-args element-line trailing comment was silently dropped:\n{out}"
    );
    assert_idempotent(src);
}

#[test]
fn bug_a_fn_params_trailing_comment_survives() {
    let src = "\
fn foo(a, b, c -- belongs to c
) {
  a
}
";
    let out = formatter::format(src).expect("format");
    assert!(
        out.contains("-- belongs to c"),
        "fn-params element-line trailing comment was silently dropped:\n{out}"
    );
    assert_idempotent(src);
}

// ── Bug B: comment must attach to the LAST element on the line ────────

/// Helper: find the output line that carries `comment`, and assert it
/// also carries `owner` and NOT `wrong`. This pins the comment to the
/// correct (last-on-line) element rather than merely "survives somewhere".
fn assert_comment_on_owner_line(out: &str, comment: &str, owner: &str, wrong: &str) {
    let line = out
        .lines()
        .find(|l| l.contains(comment))
        .unwrap_or_else(|| panic!("comment {comment:?} not found in:\n{out}"));
    assert!(
        line.contains(owner),
        "comment {comment:?} should bind to {owner:?} but was on line {line:?}\n{out}"
    );
    assert!(
        !line.contains(wrong),
        "comment {comment:?} mis-attached to {wrong:?} on line {line:?}\n{out}"
    );
}

#[test]
fn bug_b_list_comment_binds_to_last_element() {
    let src = "\
fn main() {
  let xs = [
    \"alpha\", \"beta\" -- two modes
  ]
  println(xs)
}
";
    let out = formatter::format(src).expect("format");
    assert_comment_on_owner_line(&out, "-- two modes", "\"beta\"", "\"alpha\"");
    assert_idempotent(src);
}

#[test]
fn bug_b_tuple_comment_binds_to_last_element() {
    let src = "\
fn main() {
  let xs = (
    \"alpha\", \"beta\" -- two modes
  )
  println(xs)
}
";
    let out = formatter::format(src).expect("format");
    assert_comment_on_owner_line(&out, "-- two modes", "\"beta\"", "\"alpha\"");
    assert_idempotent(src);
}

#[test]
fn bug_b_set_comment_binds_to_last_element() {
    let src = "\
fn main() {
  let xs = #[
    \"alpha\", \"beta\" -- two modes
  ]
  println(xs)
}
";
    let out = formatter::format(src).expect("format");
    assert_comment_on_owner_line(&out, "-- two modes", "\"beta\"", "\"alpha\"");
    assert_idempotent(src);
}

#[test]
fn bug_b_call_args_comment_binds_to_last_arg() {
    let src = "\
fn main() {
  foo(
    \"alpha\", \"beta\" -- two modes
  )
}
";
    let out = formatter::format(src).expect("format");
    assert_comment_on_owner_line(&out, "-- two modes", "\"beta\"", "\"alpha\"");
    assert_idempotent(src);
}

#[test]
fn bug_b_fn_params_comment_binds_to_last_param() {
    let src = "\
fn foo(
  alpha, beta -- two params
) {
  alpha
}
";
    let out = formatter::format(src).expect("format");
    assert_comment_on_owner_line(&out, "-- two params", "beta", "alpha");
    assert_idempotent(src);
}

#[test]
fn bug_b_map_comment_binds_to_last_entry() {
    let src = "\
fn main() {
  let m = #{
    \"a\": 1, \"b\": 2 -- two keys
  }
  println(m)
}
";
    let out = formatter::format(src).expect("format");
    assert_comment_on_owner_line(&out, "-- two keys", "\"b\"", "\"a\"");
    assert_idempotent(src);
}

// ── Idempotency lock for the exact prompt repros ─────────────────────

#[test]
fn idempotency_bug_a_repro() {
    let src = "\
fn main() {
  let xs = [1, 2, 3 -- belongs to 3
  ]
  println(xs)
}
";
    assert_idempotent(src);
    // And formatting the already-formatted output is a no-op too.
    let once = formatter::format(src).expect("format");
    let twice = formatter::format(&once).expect("format");
    assert_eq!(once, twice);
}

#[test]
fn idempotency_bug_b_repro() {
    let src = "\
fn main() {
  let xs = [
    \"alpha\", \"beta\" -- two modes
  ]
  println(xs)
}
";
    assert_idempotent(src);
}
