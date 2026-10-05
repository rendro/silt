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

// ── Recursive variant match certifies in polynomial time ────────
//
// Regression for a doubly-exponential blowup bug. On a recursive
// enum like `Expr { Leaf(Int), Pair(Expr, Expr) }`, the usefulness
// algorithm used to re-enumerate every variant at every level of
// the recursion — `Pair`'s two `Expr` sub-columns each triggered a
// fresh round of constructor enumeration, and the work grew as
// `k^d` until `MAX_EXHAUSTIVENESS_DEPTH` tripped. The match was
// then reported as "could not verify exhaustiveness" (and before
// that, silently accepted).
//
// The fix is a standard Maranget shortcut: if any row in the matrix
// at the current column is a bare wildcard/ident, it already covers
// every value at that column, so no wildcard query can be useful.
// This collapses `Pair(_, _)`-style arms to O(1) work per column
// instead of `k^d`. This test locks in that the shortcut fires:
// the match is certified exhaustive with no depth-limit warning and
// no spurious diagnostics.
#[test]
fn test_recursive_variant_match_certifies_without_depth_bailout() {
    use super::MAX_EXHAUSTIVENESS_DEPTH;
    use crate::intern::intern;
    use crate::source::Span;

    let mut tc = TypeChecker::new();

    // Register a recursive enum `Expr { Leaf(Int), Pair(Expr, Expr) }`.
    // (Constructed directly because writing a depth-20+ nested pattern
    // in source would be unwieldy and fragile.)
    let expr_name = TypeRef {
        id: crate::defs::TypeId(crate::defs::DefId(u32::MAX - 1)),
        name: intern("ExhaustivenessDepthExpr"),
    };
    let leaf_name = intern("ExhaustivenessDepthLeaf");
    let pair_name = intern("ExhaustivenessDepthPair");
    let expr_ty = Type::Generic(expr_name, vec![]);

    tc.tables.enums.insert(
        expr_name,
        EnumInfo {
            params: vec![],
            param_var_ids: vec![],
            variants: vec![
                VariantInfo {
                    name: leaf_name,
                    field_types: vec![Type::Int],
                },
                VariantInfo {
                    name: pair_name,
                    field_types: vec![expr_ty.clone(), expr_ty.clone()],
                },
            ],
            defined_in: super::TypeChecker::builtin_pkg(),
        },
    );

    // Build a two-arm match that IS logically exhaustive — every
    // `Expr` is either a `Leaf` or a `Pair`. Pre-fix, the Maranget
    // algorithm re-enumerated all variants at every level as it
    // recursed into `Pair`'s two `Expr` columns, hit the depth
    // bound, and raised "could not verify". With the wildcard-row
    // shortcut the algorithm certifies this cleanly and fast.
    let span = Span::point(crate::source::FileId::default(), 0);
    let body = Expr::new(crate::ast::ExprKind::Int(0), span);
    let wild = || Pattern::new(PatternKind::Wildcard, span);
    let arms = vec![
        MatchArm {
            pattern: Pattern::new(
                PatternKind::Constructor {
                    qualifier: Vec::new(),
                    name: leaf_name,
                    name_span: span,
                    args: vec![wild()],
                },
                span,
            ),
            guard: None,
            body: body.clone(),
        },
        MatchArm {
            pattern: Pattern::new(
                PatternKind::Constructor {
                    qualifier: Vec::new(),
                    name: pair_name,
                    name_span: span,
                    args: vec![wild(), wild()],
                },
                span,
            ),
            guard: None,
            body: body.clone(),
        },
    ];
    // Silence unused-warning — `MAX_EXHAUSTIVENESS_DEPTH` is imported
    // as a documentation anchor for this test.
    let _ = MAX_EXHAUSTIVENESS_DEPTH;

    tc.check_exhaustiveness(&arms, &expr_ty, span);

    // Post-fix expectation: the match is certified exhaustive with
    // no "could not verify" warning, no "non-exhaustive" error, and
    // no depth-bailout flag set.
    assert!(
        tc.errors.is_empty(),
        "expected no diagnostics, got: {:?}",
        tc.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
    assert!(
        !tc.exhaustiveness_depth_exceeded.get(),
        "depth bound should not be hit on a simple recursive variant match",
    );
}
