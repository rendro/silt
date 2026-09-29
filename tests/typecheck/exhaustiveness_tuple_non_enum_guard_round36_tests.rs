//! Round-36 LATENT regression tests for the
//! `first_col_non_enumerable` guard in `is_tuple_useful_recursive`.
//!
//! The guard decides whether a tuple's first-column type forces the
//! Maranget witness-split path because `constructors_for_query` cannot
//! faithfully enumerate its inhabitants. Before round 36 this was a
//! `match` with a `_ => true` fallback, so a future `Type` variant
//! (e.g. a hypothetical `BigInt`) could silently regress nested-tuple
//! exhaustiveness — if misclassified as enumerable (returning `false`),
//! specific-value rows in the first column would wrongly convince the
//! checker the column was fully covered and `(Foo{a:1,b:2}, SomeVariant)`
//! would be reported exhaustive.
//!
//! The round-36 fix rewrites `first_col_non_enumerable` as an exhaustive
//! `match` over every `Type` variant with NO wildcard arm, turning the
//! synchronization between `src/types.rs` and
//! `src/typechecker/exhaustiveness.rs` into a compile-time lock.
//!
//! These tests are the runtime regression half of the invariant: they
//! exercise three non-enumerable first-column column types —
//! `(Record, Enum)`, `(List, Enum)`, `(Map, Enum)` — and assert a
//! specific non-exhaustive witness text is produced. If any of these
//! types ever get misclassified as enumerable, the witness-split path
//! would not run and the assertions below would fail.

use silt::typechecker;
use silt::types::Severity;

/// Parse `input` and return the hard (Error-severity) type-check errors.
fn hard_errors(input: &str) -> Vec<String> {
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

// ── (Record, Enum) ─────────────────────────────────────────────────
//
// Mirrors the round-26 B1 repro: `(Pair{a:0,b:0}, _)` is the only arm
// but `(Pair{a:1,b:2}, 99)` exists. The non-exhaustive diagnostic must
// contain the witness-describing phrase (not merely the word
// "non-exhaustive"). The `Record` arm of `first_col_non_enumerable`
// must return `true` for this test to pass.

#[test]
fn test_round36_record_enum_first_col_emits_specific_witness() {
    let errs = hard_errors(
        r#"
type Pair { a: Int, b: Int }
type Color { Red, Green, Blue }
fn main() {
  let t = (Pair { a: 1, b: 2 }, Red)
  match t {
    (Pair { a: 0, b: 0 }, _) -> println("origin+any")
  }
}
"#,
    );
    assert!(
        errs.iter().any(|m| m.contains("non-exhaustive match")),
        "expected a non-exhaustive match diagnostic for (Record, Enum) \
         first-column tuple, got: {errs:?}"
    );
    // Round-36 lock: the witness-split path must run and the witness
    // phrasing must reach the user. `missing_description` returns
    // "not all patterns are covered" for the top-level Tuple witness —
    // that IS the specific witness text for this shape. If
    // `first_col_non_enumerable` ever returns `false` for `Record`, the
    // checker would silently accept the match and this assertion would
    // fail because no diagnostic would be emitted.
    assert!(
        errs.iter()
            .any(|m| m.contains("not all patterns are covered")),
        "expected the tuple-witness phrasing 'not all patterns are covered' \
         in the diagnostic for (Record, Enum), got: {errs:?}"
    );
}

// ── (List, Enum) ───────────────────────────────────────────────────
//
// The `List` arm of `first_col_non_enumerable` must return `true`:
// lists have unboundedly many shapes, so a specific-length row like
// `([0], _)` cannot cover every inhabitant. Pre-fix with a misclassified
// `List` this snippet would be wrongly accepted.

#[test]
fn test_round36_list_enum_first_col_emits_specific_witness() {
    let errs = hard_errors(
        r#"
type Color { Red, Green, Blue }
fn main() {
  let t = ([1, 2, 3], Red)
  match t {
    ([0], _) -> println("singleton zero")
  }
}
"#,
    );
    assert!(
        errs.iter().any(|m| m.contains("non-exhaustive match")),
        "expected a non-exhaustive match diagnostic for (List, Enum) \
         first-column tuple, got: {errs:?}"
    );
    assert!(
        errs.iter()
            .any(|m| m.contains("not all patterns are covered")),
        "expected the tuple-witness phrasing 'not all patterns are covered' \
         in the diagnostic for (List, Enum), got: {errs:?}"
    );
}

#[test]
fn test_round36_list_enum_first_col_with_wildcard_arm_is_exhaustive() {
    // Control: a list wildcard binding in the first column plus any
    // enum arm covers the whole scrutinee — no non-exhaustive error.
    let errs = hard_errors(
        r#"
type Color { Red, Green, Blue }
fn main() {
  let t = ([1, 2, 3], Red)
  match t {
    (_, Red) -> println("red")
    (_, Green) -> println("green")
    (_, Blue) -> println("blue")
  }
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no errors with enum-covering arms on (List, Enum), got: {errs:?}"
    );
}

// ── (Map, Enum) ────────────────────────────────────────────────────
//
// The `Map` arm of `first_col_non_enumerable` must return `true`:
// maps have unbounded shapes and any specific-key pattern leaves
// entire classes of maps uncovered.

#[test]
fn test_round36_map_enum_first_col_emits_specific_witness() {
    let errs = hard_errors(
        r#"
type Color { Red, Green, Blue }
fn main() {
  let t = (#{ "k": 1 }, Red)
  match t {
    (#{ "k": 0 }, _) -> println("zero-keyed")
  }
}
"#,
    );
    assert!(
        errs.iter().any(|m| m.contains("non-exhaustive match")),
        "expected a non-exhaustive match diagnostic for (Map, Enum) \
         first-column tuple, got: {errs:?}"
    );
    assert!(
        errs.iter()
            .any(|m| m.contains("not all patterns are covered")),
        "expected the tuple-witness phrasing 'not all patterns are covered' \
         in the diagnostic for (Map, Enum), got: {errs:?}"
    );
}

// ── Control: (Bool, Enum) still enumerates correctly ───────────────
//
// The `Bool` arm of `first_col_non_enumerable` returns `false` so the
// witness-split does NOT fire and the checker must enumerate both
// bool constructors. If a maintainer accidentally flips Bool to
// non-enumerable when adding a new Type variant, this test still
// passes (witness-split is still sound for Bool), but the exhaustive-
// match lock in source code prevents the silent regression in the
// other direction (non-enumerables being classified as enumerable).

#[test]
fn test_round36_bool_enum_first_col_exhaustive_enumeration_clean() {
    // Covering every (Bool, Color) cell must produce zero errors.
    let errs = hard_errors(
        r#"
type Color { Red, Green, Blue }
fn main() {
  let t = (true, Red)
  match t {
    (true, Red) -> println("a")
    (true, Green) -> println("b")
    (true, Blue) -> println("c")
    (false, Red) -> println("d")
    (false, Green) -> println("e")
    (false, Blue) -> println("f")
  }
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no errors for full (Bool, Color) enumeration, got: {errs:?}"
    );
}
