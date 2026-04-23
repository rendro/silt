//! Regression tests for the pattern duplicate-binding soundness fix.
//!
//! BROKEN (pre-fix): duplicate binding names within a single conjunctive
//! pattern scope silently shadowed each other instead of erroring.
//! Repros:
//!
//!   fn main() { let (a, a) = (1, 2); println(a) }            // printed 2
//!   fn pair(a: Int, a: Int) -> Int { a }                     // typechecked
//!   fn main() { match (1, 2) { (x, x) -> println(x) } }      // typechecked
//!
//! All three bound the *second* occurrence of the name on top of the
//! first at runtime, leaking stale values / silently choosing the wrong
//! parameter / making the scrutinee equality check unreachable.
//!
//! Fix: a pre-binding walk in `src/typechecker/inference.rs`
//! (`check_pattern_duplicate_bindings` + `check_fn_params_duplicate_bindings`
//! + `collect_pattern_binders_into`) rejects duplicates at the four
//! entry points that open a new conjunctive binding scope:
//!
//!   1. fn param list  (InferenceCtx::check_fn or equivalent, fn body)
//!   2. lambda param list (ExprKind::Lambda)
//!   3. `let PATTERN = ...` / `when let PATTERN = ...` (Stmt::Let / Stmt::When)
//!   4. `match e { PATTERN -> ... }` arm patterns
//!
//! Or-patterns (`p1 | p2`) are intentionally exempted: the same name
//! appearing across `|` alternatives is how or-patterns work. Only
//! duplicates within one conjunctive-sibling scope are rejected.
//!
//! Each test below was authored to FAIL against the pre-fix codebase and
//! PASS after the fix. The error-message anchor is quoted verbatim so a
//! future weakening of the diagnostic wording fails the lock.

use silt::typechecker;
use silt::types::Severity;

fn type_errors(input: &str) -> Vec<String> {
    let tokens = silt::lexer::Lexer::new(input)
        .tokenize()
        .expect("lexer error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    let errors = typechecker::check(&mut program);
    errors
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

/// The exact phrase emitted by `check_pattern_duplicate_bindings` and
/// `check_fn_params_duplicate_bindings` in `src/typechecker/inference.rs`.
/// A drift in the wording of this diagnostic fails every test in this
/// file, so the lock is load-bearing — tests pin the phrase, not just
/// "some error happened".
const ANCHOR: &str = "duplicate binding";

// ── Negative tests (all three BROKEN repros) ────────────────────────

#[test]
fn let_tuple_duplicate_binding_rejected() {
    // Primary B2 repro: `let (a, a) = (1, 2)` used to silently print `2`.
    let errs = type_errors(
        r#"
fn main() {
  let (a, a) = (1, 2)
  println(a)
}
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains(ANCHOR) && e.contains("'a'")),
        "expected \"duplicate binding 'a' in pattern\" for `let (a, a) = (1, 2)`, got: {errs:?}"
    );
}

#[test]
fn fn_params_duplicate_binding_rejected() {
    // B2 fn-param variant: `fn pair(a: Int, a: Int)` used to typecheck,
    // silently shadowing the first param with the second. The whole
    // param list is a single conjunctive scope, so the check must be
    // threaded across every parameter — not per-pattern.
    let errs = type_errors(
        r#"
fn pair(a: Int, a: Int) -> Int { a }
fn main() { println(pair(1, 2)) }
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains(ANCHOR) && e.contains("'a'")),
        "expected \"duplicate binding 'a' in pattern\" for `fn pair(a, a)`, got: {errs:?}"
    );
}

#[test]
fn match_tuple_duplicate_binding_rejected() {
    // B2 match variant: `match (1, 2) { (x, x) -> ... }` used to
    // typecheck silently. The match-arm path goes through
    // `check_pattern`, not `bind_pattern`, so the fix must instrument
    // both entry points.
    let errs = type_errors(
        r#"
fn main() {
  match (1, 2) {
    (x, x) -> println(x)
  }
}
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains(ANCHOR) && e.contains("'x'")),
        "expected \"duplicate binding 'x' in pattern\" for `match (1, 2) {{ (x, x) -> ... }}`, got: {errs:?}"
    );
}

// ── Additional conjunctive-scope variants ──────────────────────────

#[test]
fn let_nested_tuple_duplicate_binding_rejected() {
    // A duplicate across two nested tuple levels is still in one
    // conjunctive scope — both `a`s bind at the same runtime point.
    let errs = type_errors(
        r#"
fn main() {
  let ((a, b), a) = ((1, 2), 3)
  println(a)
}
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains(ANCHOR) && e.contains("'a'")),
        "expected duplicate-binding error for `let ((a, b), a)`, got: {errs:?}"
    );
}

#[test]
fn let_list_duplicate_binding_rejected() {
    let errs = type_errors(
        r#"
fn main() {
  let [a, a] = [1, 2]
  println(a)
}
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains(ANCHOR) && e.contains("'a'")),
        "expected duplicate-binding error for `let [a, a] = [1, 2]`, got: {errs:?}"
    );
}

#[test]
fn let_list_rest_duplicate_binding_rejected() {
    // The rest pattern is in the same conjunctive scope as the
    // element patterns. Silt's rest-pattern syntax is `..name`.
    let errs = type_errors(
        r#"
fn main() {
  let [a, ..a] = [1, 2, 3]
  println(a)
}
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains(ANCHOR) && e.contains("'a'")),
        "expected duplicate-binding error for `let [a, ..a]`, got: {errs:?}"
    );
}

#[test]
fn match_constructor_duplicate_binding_rejected() {
    // Constructor args are a conjunctive scope.
    let errs = type_errors(
        r#"
type Pair(a, b) { P(a, b) }
fn main() {
  match P(1, 2) {
    P(x, x) -> println(x)
  }
}
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains(ANCHOR) && e.contains("'x'")),
        "expected duplicate-binding error for `P(x, x)`, got: {errs:?}"
    );
}

// ── Positive locks: or-patterns must still work ─────────────────────

#[test]
fn or_pattern_simple_literals_still_work() {
    // Or-pattern alternatives with no bindings: no duplicate issue
    // to begin with, but this is the sanity-check baseline — the
    // round-35 fix-set elsewhere requires this to keep working.
    let errs = type_errors(
        r#"
fn main() {
  let result = match 2 {
    1 | 2 -> "small"
    3 -> "three"
    _ -> "big"
  }
  println(result)
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no errors for `1 | 2 -> ...`, got: {errs:?}"
    );
}

#[test]
fn or_pattern_tuple_compound_still_works() {
    // Compound or-pattern: `(1, _) | (_, 2)` binds no vars but does
    // exercise the per-alternative walk. A regression here would
    // indicate the fix broke the round-35 compound-or-pattern path.
    let errs = type_errors(
        r#"
fn main() {
  let result = match (5, 2) {
    (1, _) | (_, 2) -> "found"
    _ -> "nope"
  }
  println(result)
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no errors for `(1, _) | (_, 2)`, got: {errs:?}"
    );
}

#[test]
fn or_pattern_with_shared_binding_across_arms_still_works() {
    // Each alternative binds the same name `x` — that's how `|`
    // works. The duplicate check must NOT fire here. If it did,
    // this common idiom would become a compile error.
    let errs = type_errors(
        r#"
type E { A(Int), B(Int) }
fn main() {
  let e = A(3)
  let result = match e {
    A(x) | B(x) -> x
  }
  println(result)
}
"#,
    );
    assert!(
        errs.is_empty(),
        "or-pattern alternatives sharing `x` must remain legal, got: {errs:?}"
    );
}

// ── Positive locks: legitimate shadowing across disjoint scopes ─────

#[test]
fn fn_name_same_as_param_still_works() {
    // Shadowing the top-level function name with a param of the
    // same spelling is NOT a duplicate binding in the same pattern
    // scope — the fn name lives in the outer env and the param
    // name lives in the fn's local env. This must continue to
    // typecheck.
    let errs = type_errors(
        r#"
fn a(a: Int) -> Int { a + 1 }
fn main() { println(a(41)) }
"#,
    );
    assert!(
        errs.is_empty(),
        "shadowing a fn name with a same-named param must remain legal, got: {errs:?}"
    );
}

#[test]
fn sibling_let_bindings_reusing_name_still_works() {
    // Two separate `let` statements reusing `a` are in different
    // conjunctive scopes (the second is an update / shadowing of
    // the first, not a duplicate within one pattern).
    let errs = type_errors(
        r#"
fn main() {
  let a = 1
  let a = 2
  println(a)
}
"#,
    );
    assert!(
        errs.is_empty(),
        "sequential `let a = ...; let a = ...` must remain legal, got: {errs:?}"
    );
}

#[test]
fn distinct_names_in_same_tuple_still_work() {
    // Baseline positive: `let (a, b) = (1, 2)` — no duplicates,
    // must typecheck. A regression in the sibling-walk accidentally
    // treating every name as a duplicate would trip this.
    let errs = type_errors(
        r#"
fn main() {
  let (a, b) = (1, 2)
  println(a + b)
}
"#,
    );
    assert!(
        errs.is_empty(),
        "distinct names `(a, b)` must remain legal, got: {errs:?}"
    );
}

#[test]
fn two_fn_params_with_distinct_names_still_work() {
    let errs = type_errors(
        r#"
fn add(a: Int, b: Int) -> Int { a + b }
fn main() { println(add(1, 2)) }
"#,
    );
    assert!(
        errs.is_empty(),
        "`fn add(a, b)` must remain legal, got: {errs:?}"
    );
}
