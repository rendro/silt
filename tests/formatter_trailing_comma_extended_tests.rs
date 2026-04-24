//! Round-52 deferred-item 7: the formatter must preserve (mirror) the
//! source's trailing-comma state on every comma-separated construct,
//! not just `TypeBody::Enum` (covered in
//! `tests/formatter_trailing_comma_tests.rs`). Extending the round-51
//! fix keeps the fuzz invariant "significant token count unchanged"
//! (see `src/fuzz_invariants.rs::check_formatter_invariants` and
//! `significant_token_count`, which counts `Comma`) satisfied across
//! every delimited literal / parameter list / argument list / field
//! list / item list the grammar accepts.
//!
//! For each construct this file locks three invariants:
//!   1. Source WITH a trailing comma → output keeps it.
//!   2. Source WITHOUT a trailing comma → output does NOT add one.
//!   3. Idempotency: `format(format(src)) == format(src)` for a sample
//!      containing the construct.
//!
//! The per-construct coverage mirrors the list in the round-52 deferred
//! item 7 spec: list literals, tuple literals, record literals,
//! record-update literals, set literals, map literals, call arguments,
//! fn parameter lists, lambda parameter lists, type record field lists,
//! selective import item lists, and match arms (whose comma separator
//! is grammar-optional but must still be mirrored).

use silt::formatter;
use silt::fuzz_invariants::check_formatter_invariants;

/// Locks all three invariants for one source sample:
///   - `check_formatter_invariants(src, format(src))` holds (token
///     counts / delimiter balance / comment markers / parseability);
///   - if `expected_substring` is `Some(s)`, the output CONTAINS `s`;
///   - if `forbidden_substring` is `Some(s)`, the output does NOT
///     contain `s`;
///   - `format(format(src)) == format(src)` (idempotency).
fn assert_trailing_comma_behavior(
    src: &str,
    expected_substring: Option<&str>,
    forbidden_substring: Option<&str>,
) {
    let once = formatter::format(src).expect("format should succeed");
    check_formatter_invariants(src, &once)
        .expect("formatter invariants (token count / balance / parse) must hold");
    if let Some(s) = expected_substring {
        assert!(
            once.contains(s),
            "expected output to contain `{s}`; actual:\n{once}"
        );
    }
    if let Some(s) = forbidden_substring {
        assert!(
            !once.contains(s),
            "output unexpectedly contained `{s}`; actual:\n{once}"
        );
    }
    let twice = formatter::format(&once).expect("second pass must succeed");
    assert_eq!(
        once, twice,
        "format(format(src)) != format(src); src:\n{src}\nfirst pass:\n{once}\nsecond pass:\n{twice}"
    );
}

// ── List literals ───────────────────────────────────────────────────

#[test]
fn list_literal_preserves_trailing_comma_single_line() {
    let src = "fn main() = [1, 2, 3,]\n";
    assert_trailing_comma_behavior(src, Some("[1, 2, 3,]"), None);
}

#[test]
fn list_literal_does_not_insert_trailing_comma_single_line() {
    let src = "fn main() = [1, 2, 3]\n";
    assert_trailing_comma_behavior(src, Some("[1, 2, 3]"), Some("3,]"));
}

#[test]
fn list_literal_preserves_trailing_comma_multiline_collapsed() {
    // Source is multi-line with no interior comments — the formatter
    // will collapse to single-line. The trailing comma must survive.
    // Using the `= expr` simple-body form (not `= { block }` — the
    // formatter reshapes the latter to `{ block }` which legitimately
    // drops the `=` significant token, confusing the invariant check).
    let src = "fn xs() = [\n    1,\n    2,\n  ]\n";
    assert_trailing_comma_behavior(src, Some("[1, 2,]"), None);
}

#[test]
fn list_literal_idempotent_with_trailing_comma() {
    // Dedicated idempotency test for a list with a trailing comma.
    let src = "fn main() = [1, 2, 3,]\n";
    let once = formatter::format(src).expect("pass 1");
    let twice = formatter::format(&once).expect("pass 2");
    assert_eq!(once, twice);
}

// ── Tuple literals ──────────────────────────────────────────────────

#[test]
fn tuple_literal_preserves_trailing_comma_single_line() {
    let src = "fn main() = (1, 2, 3,)\n";
    assert_trailing_comma_behavior(src, Some("(1, 2, 3,)"), None);
}

#[test]
fn tuple_literal_does_not_insert_trailing_comma_single_line() {
    let src = "fn main() = (1, 2, 3)\n";
    assert_trailing_comma_behavior(src, Some("(1, 2, 3)"), Some("3,)"));
}

/// Single-element tuple `(x,)` always retains its disambiguating
/// comma — the parser folds `(x)` down to a bare expression, so
/// emitting `(x,)` is load-bearing. This test locks that behavior
/// isn't accidentally loosened by the round-52 extension.
#[test]
fn tuple_single_element_always_keeps_disambiguating_comma() {
    let src = "fn main() = (1,)\n";
    assert_trailing_comma_behavior(src, Some("(1,)"), None);
}

// ── Record literals ─────────────────────────────────────────────────

#[test]
fn record_literal_preserves_trailing_comma_single_line() {
    let src = "type Point { x: Int, y: Int }\nfn main() = Point { x: 1, y: 2, }\n";
    assert_trailing_comma_behavior(src, Some("Point { x: 1, y: 2, }"), None);
}

#[test]
fn record_literal_does_not_insert_trailing_comma_single_line() {
    let src = "type Point { x: Int, y: Int }\nfn main() = Point { x: 1, y: 2 }\n";
    assert_trailing_comma_behavior(src, Some("Point { x: 1, y: 2 }"), Some("2, }"));
}

#[test]
fn record_update_preserves_trailing_comma() {
    let src = "type Point { x: Int, y: Int }\nfn update(p) = p.{ x: 5, }\n";
    assert_trailing_comma_behavior(src, Some("p.{ x: 5, }"), None);
}

#[test]
fn record_update_does_not_insert_trailing_comma() {
    let src = "type Point { x: Int, y: Int }\nfn update(p) = p.{ x: 5 }\n";
    assert_trailing_comma_behavior(src, Some("p.{ x: 5 }"), Some("5, }"));
}

// ── Set literals ────────────────────────────────────────────────────

#[test]
fn set_literal_preserves_trailing_comma() {
    let src = "fn main() = #[1, 2, 3,]\n";
    assert_trailing_comma_behavior(src, Some("#[1, 2, 3,]"), None);
}

#[test]
fn set_literal_does_not_insert_trailing_comma() {
    let src = "fn main() = #[1, 2, 3]\n";
    assert_trailing_comma_behavior(src, Some("#[1, 2, 3]"), Some("3,]"));
}

// ── Map literals ────────────────────────────────────────────────────

#[test]
fn map_literal_preserves_trailing_comma() {
    let src = "fn main() = #{\"a\": 1, \"b\": 2,}\n";
    assert_trailing_comma_behavior(src, Some("2, }"), None);
}

#[test]
fn map_literal_does_not_insert_trailing_comma() {
    let src = "fn main() = #{\"a\": 1, \"b\": 2}\n";
    assert_trailing_comma_behavior(src, Some("\"b\": 2 }"), Some("2, }"));
}

// ── Call arguments ──────────────────────────────────────────────────

#[test]
fn call_args_preserve_trailing_comma_single_line() {
    let src = "fn add(a, b) = a + b\nfn main() = add(1, 2,)\n";
    assert_trailing_comma_behavior(src, Some("add(1, 2,)"), None);
}

#[test]
fn call_args_does_not_insert_trailing_comma_single_line() {
    let src = "fn add(a, b) = a + b\nfn main() = add(1, 2)\n";
    assert_trailing_comma_behavior(src, Some("add(1, 2)"), Some("2,)"));
}

#[test]
fn call_args_preserve_trailing_comma_multiline_collapsed() {
    let src = "fn add(a, b) = a + b\nfn main() = add(\n  1,\n  2,\n)\n";
    let out = formatter::format(src).expect("format");
    // Check the call collapsed to `add(1, 2,)` and ran through the
    // invariant check (identical source / formatted token counts).
    assert!(
        out.contains("add(1, 2,)"),
        "multi-line call with trailing comma should collapse to `add(1, 2,)`; got:\n{out}"
    );
    check_formatter_invariants(src, &out).expect("invariants");
    let twice = formatter::format(&out).expect("pass 2");
    assert_eq!(out, twice);
}

// ── Fn parameter lists (def site) ───────────────────────────────────

#[test]
fn fn_params_preserve_trailing_comma_single_line() {
    let src = "fn add(a, b,) = a + b\n";
    assert_trailing_comma_behavior(src, Some("fn add(a, b,)"), None);
}

#[test]
fn fn_params_does_not_insert_trailing_comma_single_line() {
    let src = "fn add(a, b) = a + b\n";
    assert_trailing_comma_behavior(src, Some("fn add(a, b)"), Some("b,)"));
}

// ── Lambda parameter lists ──────────────────────────────────────────

#[test]
fn lambda_params_preserve_trailing_comma() {
    let src = "fn mk() = fn(a, b,) { a + b }\n";
    assert_trailing_comma_behavior(src, Some("fn(a, b,)"), None);
}

#[test]
fn lambda_params_does_not_insert_trailing_comma() {
    let src = "fn mk() = fn(a, b) { a + b }\n";
    assert_trailing_comma_behavior(src, Some("fn(a, b)"), Some("b,)"));
}

// ── Type record field lists ─────────────────────────────────────────

#[test]
fn type_record_preserves_trailing_comma() {
    let src = "type Point {\n  x: Int,\n  y: Int,\n}\n";
    assert_trailing_comma_behavior(src, Some("y: Int,"), None);
}

#[test]
fn type_record_does_not_insert_trailing_comma() {
    // The round-52 fix locks this direction: previously the formatter
    // ALWAYS put a trailing comma on type record fields; now it mirrors
    // the source. Sources with no trailing comma keep it that way.
    let src = "type Point {\n  x: Int,\n  y: Int\n}\n";
    assert_trailing_comma_behavior(src, Some("y: Int\n"), Some("y: Int,\n}"));
}

// ── Selective import item lists ─────────────────────────────────────

#[test]
fn import_items_preserve_trailing_comma() {
    let src = "import math.{ sqrt, abs, }\n";
    assert_trailing_comma_behavior(src, Some("sqrt, abs, }"), None);
}

#[test]
fn import_items_does_not_insert_trailing_comma() {
    let src = "import math.{ sqrt, abs }\n";
    assert_trailing_comma_behavior(src, Some("sqrt, abs }"), Some("abs, }"));
}

// ── Match arms (comma separator is grammar-optional) ────────────────

#[test]
fn match_arms_preserve_separator_commas() {
    let src = "fn f(x) = match x { 1 -> \"one\", 2 -> \"two\", _ -> \"other\" }\n";
    let out = formatter::format(src).expect("format");
    check_formatter_invariants(src, &out).expect("invariants");
    // Source had 2 commas between arms (no trailing); output must
    // keep 2 top-level `,` tokens to preserve the significant-token
    // count (see `significant_token_count` in
    // `src/fuzz_invariants.rs`).
    use silt::lexer::{Lexer, Token};
    let src_commas = Lexer::new(src)
        .tokenize()
        .unwrap()
        .iter()
        .filter(|(t, _)| matches!(t, Token::Comma))
        .count();
    let out_commas = Lexer::new(&out)
        .tokenize()
        .unwrap()
        .iter()
        .filter(|(t, _)| matches!(t, Token::Comma))
        .count();
    assert_eq!(
        src_commas, out_commas,
        "match arm comma count changed: {src_commas} -> {out_commas}; output:\n{out}"
    );
    let twice = formatter::format(&out).expect("pass 2");
    assert_eq!(out, twice);
}

#[test]
fn match_arms_preserve_trailing_comma_on_last_arm() {
    // Source has 3 commas — between each pair plus one after the last
    // arm (trailing). Output must preserve all 3 for the invariant.
    let src = "fn f(x) = match x { 1 -> \"one\", 2 -> \"two\", _ -> \"other\", }\n";
    let out = formatter::format(src).expect("format");
    check_formatter_invariants(src, &out).expect("invariants");
    let twice = formatter::format(&out).expect("pass 2");
    assert_eq!(out, twice);
}

#[test]
fn match_arms_does_not_insert_commas_when_source_has_none() {
    // Source has NO commas between arms (newline-separated is the
    // canonical multi-line form). Output must also have none.
    let src = "fn f(x) = match x {\n  1 -> \"one\"\n  2 -> \"two\"\n  _ -> \"other\"\n}\n";
    let out = formatter::format(src).expect("format");
    check_formatter_invariants(src, &out).expect("invariants");
    assert!(
        !out.contains("\"one\","),
        "formatter inserted a comma between match arms that the source did not have; output:\n{out}"
    );
    let twice = formatter::format(&out).expect("pass 2");
    assert_eq!(out, twice);
}

#[test]
fn match_arms_nested_match_in_lambda_does_not_spuriously_comma() {
    // Regression guard: the inner `match { ... }` is embedded inside
    // a lambda `{ acc, x -> match { ... } }`. An earlier version of
    // the comma-counting scanner mistakenly latched onto the lambda's
    // `{` as the inner match's body opener and counted the
    // lambda-parameter comma toward the inner match's arm commas,
    // producing a spurious `x < acc -> x,` on re-format. The fix in
    // `count_toplevel_commas_in_match_body` uses the match keyword's
    // column to anchor the scan.
    let src = "fn min_val(nums) {\n  match list.head(nums) {\n    Some(first) -> nums\n      |> list.fold(first) { acc, x -> match {\n        x < acc -> x\n        _ -> acc\n      } }\n    None -> 0\n  }\n}\n";
    let out = formatter::format(src).expect("format");
    check_formatter_invariants(src, &out).expect("invariants");
    assert!(
        !out.contains("x < acc -> x,"),
        "spurious comma appeared after inner match arm; output:\n{out}"
    );
    let twice = formatter::format(&out).expect("pass 2");
    assert_eq!(out, twice);
}

// ── Constructor argument lists (in patterns / expressions) ─────────

/// Regression: `fn wrap(x) = Some(x,)`. The fn-params check uses
/// `f.span` (at the `fn` keyword) and must anchor its scan to the
/// fn's `(...)` specifically — NOT latch onto the body's
/// `Some(x,)` close, which would falsely report a trailing comma
/// on the fn's empty-last-param position and produce
/// `fn wrap(x,) = Some(x,)`.
#[test]
fn fn_params_ignore_trailing_comma_in_body() {
    let src = "fn wrap(x) = Some(x,)\n";
    assert_trailing_comma_behavior(src, Some("fn wrap(x)"), Some("wrap(x,)"));
}

#[test]
fn constructor_pattern_args_behave_like_call_args() {
    // `Some(x,)` in a pattern uses the same parser as call args
    // (see `PatternKind::Constructor` in `src/ast.rs` + the parser).
    // This locks that the formatter doesn't mangle the pattern's
    // trailing comma (it's rendered by `format_pattern`, which is a
    // separate code path from `format_expr`). A source without a
    // trailing comma must stay without one.
    let src = "fn f(o) = match o {\n  Some(x) -> x\n  None -> 0\n}\n";
    assert_trailing_comma_behavior(src, Some("Some(x)"), Some("Some(x,)"));
}

// ── Trait method lists are newline-separated, not comma; no test ───

// Trait method lists (`trait T { fn m1(...) ...\nfn m2(...) ...\n}`)
// are NOT comma-separated — the parser reads each method as a
// separate top-level decl-within-trait and commas between methods
// would be a parse error. So this construct is not part of the
// round-52 extension. See `format_trait_methods` in `src/formatter.rs`.
