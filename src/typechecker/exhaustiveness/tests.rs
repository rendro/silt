use super::super::test_helpers::*;
use super::super::*;

// ── Or-pattern exhaustiveness ───────────────────────────────────

#[test]
fn test_or_pattern_exhaustive() {
    assert_no_errors(
        r#"
type Color { Red, Green, Blue }
fn describe(c) {
  match c {
    Red | Green -> "warm-ish"
    Blue -> "cool"
  }
}
fn main() { describe(Red) }
        "#,
    );
}

#[test]
fn test_or_pattern_non_exhaustive() {
    assert_has_error(
        r#"
type Color { Red, Green, Blue }
fn describe(c) {
  match c {
    Red | Green -> "warm-ish"
  }
}
fn main() { describe(Red) }
        "#,
        "non-exhaustive",
    );
}

// ── Nested constructor exhaustiveness ────────────────────────────

#[test]
fn test_nested_option_exhaustive() {
    assert_no_errors(
        r#"
fn process(x) {
  match x {
    Some(Some(v)) -> v
    Some(None) -> 0
    None -> 0
  }
}
fn main() { process(Some(Some(1))) }
        "#,
    );
}

#[test]
fn test_nested_option_missing_inner() {
    assert_has_error(
        r#"
fn process(x) {
  match x {
    Some(Some(v)) -> v
    None -> 0
  }
}
fn main() { process(Some(Some(1))) }
        "#,
        "non-exhaustive",
    );
}

// ── Tuple exhaustiveness ────────────────────────────────────────

#[test]
fn test_tuple_pair_exhaustive() {
    assert_no_errors(
        r#"
fn check(pair) {
  match pair {
    (true, true) -> 1
    (true, false) -> 2
    (false, true) -> 3
    (false, false) -> 4
  }
}
fn main() { check((true, false)) }
        "#,
    );
}

#[test]
fn test_tuple_pair_missing_case() {
    assert_has_error(
        r#"
fn check(pair) {
  match pair {
    (true, true) -> 1
    (true, false) -> 2
    (false, true) -> 3
  }
}
fn main() { check((true, false)) }
        "#,
        "non-exhaustive",
    );
}

// ── List pattern exhaustiveness ─────────────────────────────────

#[test]
fn test_list_with_wildcard_exhaustive() {
    assert_no_errors(
        r#"
fn head(xs) {
  match xs {
    [] -> 0
    [x, ..rest] -> x
  }
}
fn main() { head([1, 2, 3]) }
        "#,
    );
}

#[test]
fn test_list_missing_empty_case() {
    assert_has_error(
        r#"
fn head(xs) {
  match xs {
    [x, ..rest] -> x
  }
}
fn main() { head([1, 2, 3]) }
        "#,
        "non-exhaustive",
    );
}

// ── Empty match ─────────────────────────────────────────────────

#[test]
fn test_empty_match_non_exhaustive() {
    assert_has_error(
        r#"
fn process(x) {
  match x {
  }
}
fn main() { process(1) }
        "#,
        "non-exhaustive",
    );
}

// ── All guards non-exhaustive ───────────────────────────────────

#[test]
fn test_all_arms_guarded() {
    assert_has_error(
        r#"
fn check(x) {
  match x {
    n when n > 0 -> "positive"
    n when n < 0 -> "negative"
  }
}
fn main() { check(1) }
        "#,
        "non-exhaustive",
    );
}

// ── Wildcard covers everything ──────────────────────────────────

#[test]
fn test_single_wildcard_exhaustive() {
    assert_no_errors(
        r#"
fn id(x) {
  match x {
    _ -> x
  }
}
fn main() { id(42) }
        "#,
    );
}

// ── Bool or-pattern exhaustiveness ──────────────────────────────

#[test]
fn test_bool_or_pattern_covers() {
    assert_no_errors(
        r#"
fn check(b) {
  match b {
    true | false -> "done"
  }
}
fn main() { check(true) }
        "#,
    );
}

// ── Multi-field constructor exhaustiveness ──────────────────────

#[test]
fn test_multi_field_variant_exhaustive() {
    assert_no_errors(
        r#"
type Shape {
  Circle(Float),
  Rect(Float, Float),
}
fn area(s) {
  match s {
    Circle(r) -> 3.14 * r * r
    Rect(w, h) -> w * h
  }
}
fn main() { area(Circle(1.0)) }
        "#,
    );
}

#[test]
fn test_multi_field_variant_missing() {
    assert_has_error(
        r#"
type Shape {
  Circle(Float),
  Rect(Float, Float),
}
fn area(s) {
  match s {
    Circle(r) -> 3.14 * r * r
  }
}
fn main() { area(Circle(1.0)) }
        "#,
        "non-exhaustive",
    );
}

// ── The search itself, on lowered patterns ──────────────────────

use super::{CtorId, Pat, Unverified};

/// A checker that knows the enum `Expr { Leaf(Int), Pair(Expr, Expr) }`,
/// and the enum's type.
fn checker_with_expr() -> (TypeChecker, TypeRef) {
    use crate::intern::intern;
    let mut tc = TypeChecker::new();
    let expr = TypeRef {
        id: crate::defs::TypeId(crate::defs::DefId(u32::MAX - 1)),
        name: intern("ExhaustivenessExpr"),
    };
    let expr_ty = Type::Generic(expr, vec![]);
    tc.tables.enums.insert(
        expr,
        EnumInfo {
            params: vec![],
            param_var_ids: vec![],
            variants: vec![
                VariantInfo {
                    name: intern("ExhaustivenessLeaf"),
                    field_types: vec![Type::Int],
                },
                VariantInfo {
                    name: intern("ExhaustivenessPair"),
                    field_types: vec![expr_ty.clone(), expr_ty],
                },
            ],
            defined_in: super::TypeChecker::builtin_pkg(),
        },
    );
    (tc, expr)
}

fn bool_pat(value: bool) -> Pat {
    Pat::Ctor(CtorId::Bool(value), Vec::new())
}

fn tuple(elems: Vec<Pat>) -> Pat {
    Pat::Ctor(CtorId::Tuple, elems)
}

/// A recursive type is not unfolded: `Leaf(_) | Pair(_, _)` covers
/// `Expr` whatever `Pair` holds.
#[test]
fn test_recursive_variant_is_covered_by_its_two_constructors() {
    let (tc, expr) = checker_with_expr();
    let leaf = Pat::Ctor(CtorId::Variant(expr, 0), vec![Pat::Wild]);
    let pair = Pat::Ctor(CtorId::Variant(expr, 1), vec![Pat::Wild, Pat::Wild]);
    assert_eq!(
        tc.irrefutable(&Pat::Or(vec![leaf.clone(), pair.clone()])),
        Ok(true)
    );
    assert_eq!(tc.irrefutable(&leaf), Ok(false));
    // `Pair(Leaf(_), _) | Pair(Pair(_, _), _) | Leaf(_)`: one level down.
    let nested = Pat::Or(vec![
        Pat::Ctor(CtorId::Variant(expr, 1), vec![leaf.clone(), Pat::Wild]),
        Pat::Ctor(CtorId::Variant(expr, 1), vec![pair, Pat::Wild]),
        leaf,
    ]);
    assert_eq!(tc.irrefutable(&nested), Ok(true));
}

/// An or-pattern that covers its column is not split: sixty columns of
/// `true | false` are one row, not 2^60.
#[test]
fn test_or_patterns_in_many_columns_are_not_multiplied() {
    let tc = TypeChecker::new();
    let either = || Pat::Or(vec![bool_pat(true), bool_pat(false)]);
    assert_eq!(
        tc.irrefutable(&tuple((0..60).map(|_| either()).collect())),
        Ok(true)
    );
    let mut refutable: Vec<Pat> = (0..60).map(|_| either()).collect();
    refutable.push(bool_pat(true));
    assert_eq!(tc.irrefutable(&tuple(refutable)), Ok(false));
}

/// Integer ranges cover the integers when they leave no gap.
#[test]
fn test_int_ranges_cover_without_a_gap() {
    let tc = TypeChecker::new();
    let ranges = |cut: i64| {
        Pat::Or(vec![
            Pat::IntRange(i64::MIN, 0),
            Pat::IntRange(cut, i64::MAX),
        ])
    };
    assert_eq!(tc.irrefutable(&ranges(1)), Ok(true));
    assert_eq!(tc.irrefutable(&ranges(-5)), Ok(true));
    assert_eq!(tc.irrefutable(&ranges(2)), Ok(false));
    assert_eq!(tc.irrefutable(&Pat::IntRange(3, 3)), Ok(false));
}

/// Record patterns of one column may name different fields.
#[test]
fn test_record_patterns_align_by_field_name() {
    use crate::intern::intern;
    let tc = TypeChecker::new();
    let (a, b) = (intern("a"), intern("b"));
    let rec = |names: Vec<Symbol>, pats: Vec<Pat>| Pat::Ctor(CtorId::Record(names), pats);
    let covering = Pat::Or(vec![
        rec(vec![a], vec![bool_pat(true)]),
        rec(vec![b, a], vec![Pat::Wild, bool_pat(false)]),
    ]);
    assert_eq!(tc.irrefutable(&covering), Ok(true));
    let leaking = Pat::Or(vec![
        rec(vec![a], vec![bool_pat(true)]),
        rec(vec![b, a], vec![bool_pat(true), bool_pat(false)]),
    ]);
    assert_eq!(tc.irrefutable(&leaking), Ok(false));
}

/// A search that would look at more patterns than its bound gives up, and
/// says so: it does not answer. Sixteen pairs of columns, each pair
/// covered by four rows that leave every other column alone, so that no
/// row is decided before the second half of the columns.
#[test]
fn test_a_search_past_its_bound_is_unverified() {
    let tc = TypeChecker::new();
    let pairs = 16;
    let mut rows = Vec::new();
    for i in 0..pairs {
        for (first, second) in [(true, true), (true, false), (false, true), (false, false)] {
            let mut row = vec![Pat::Wild; 2 * pairs];
            row[i] = bool_pat(first);
            row[i + pairs] = bool_pat(second);
            rows.push(tuple(row));
        }
    }
    assert_eq!(tc.irrefutable(&Pat::Or(rows)), Err(Unverified));
}

/// So does one that would recurse deeper than its bound: a `Bool`
/// inside 1,100 nested pairs, where the `k13` repro has 25.
#[test]
fn test_a_search_too_deep_is_unverified() {
    let tc = TypeChecker::new();
    let nested = |depth: usize, value: bool| {
        (0..depth).fold(bool_pat(value), |inner, _| tuple(vec![inner, Pat::Wild]))
    };
    let both = |depth: usize| Pat::Or(vec![nested(depth, true), nested(depth, false)]);
    assert_eq!(tc.irrefutable(&both(25)), Ok(true));
    assert_eq!(tc.irrefutable(&nested(25, true)), Ok(false));
    assert_eq!(tc.irrefutable(&both(1_100)), Err(Unverified));
}

/// A list pattern is `cons` cells; one too long to follow is one test.
#[test]
fn test_list_patterns() {
    let tc = TypeChecker::new();
    let any = |n: usize, tail: Pat| Pat::list(vec![Pat::Wild; n], tail);
    // `[] | [_] | [_, _, ..rest]`
    let covering = Pat::Or(vec![
        any(0, Pat::nil()),
        any(1, Pat::nil()),
        any(2, Pat::Wild),
    ]);
    assert_eq!(tc.irrefutable(&covering), Ok(true));
    // `[] | [_, _, ..rest]`
    let leaking = Pat::Or(vec![any(0, Pat::nil()), any(2, Pat::Wild)]);
    assert_eq!(tc.irrefutable(&leaking), Ok(false));
    assert_eq!(any(5_000, Pat::Wild), Pat::Lit);
}
