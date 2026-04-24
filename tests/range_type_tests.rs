//! Lock tests for Round 52 deferred item 6 — `Range(T)` nominal type.
//!
//! Before this change, `1..10` inferred as `Type::List(Int)` at
//! `src/typechecker/inference.rs:ExprKind::Range`, so the annotation
//! `let r: Range(Int) = 1..10` failed with
//! `expected Range(Int), got List(Int)` — the "Range" type was
//! undocumented and the docs at `docs/language/operators.md` claimed
//! it was a type.
//!
//! The chosen fix is a nominal `Range(T)` wrapper that unifies
//! bidirectionally with `List(T)` (see the `Type::Range` / `Type::List`
//! arms in `src/typechecker/mod.rs::unify`). At runtime, a range is
//! still a materialized `Vec<Value>`, so laziness is a future design —
//! the docs were corrected to say so.
//!
//! These tests lock in:
//!   1. `let r: Range(Int) = 1..10` typechecks.
//!   2. `let r: List(Int) = 1..10` still typechecks (Range→List).
//!   3. `let r: Range(Int) = [1, 2, 3]` typechecks (List→Range,
//!      bidirectional unify).
//!   4. `list.sum(1..10)` typechecks end-to-end at a call site.
//!   5. Pattern match `[a, b, c] -> ...` works against a range.
//!   6. Un-annotated `1..10` infers as `Range(Int)`.
//!   7. Negative: `let r: Range(String) = 1..10` rejects with an
//!      element-type error.
//!   8. Display renders as `Range(Int)`, not `List(Int)`.

use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::types::{Severity, Type};

fn type_errors(input: &str) -> Vec<String> {
    let tokens = Lexer::new(input).tokenize().expect("lexer");
    let mut program = Parser::new(tokens).parse_program().expect("parse");
    typechecker::check(&mut program)
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

fn assert_ok(src: &str) {
    let errs = type_errors(src);
    assert!(
        errs.is_empty(),
        "expected no errors, got:\n{}",
        errs.join("\n")
    );
}

// ── 1. Annotated as Range(Int) ─────────────────────────────────────

#[test]
fn range_annotation_accepts_range_expr() {
    // Before: "expected Range(Int), got List(Int)".
    // After: clean typecheck.
    assert_ok(
        r#"
        fn main() {
            let r: Range(Int) = 1..10
            r
        }
        "#,
    );
}

// ── 2. Range→List coercion at annotation site ──────────────────────

#[test]
fn list_annotation_accepts_range_expr() {
    // Range(T) unifies bidirectionally with List(T). Pre-existing
    // code that annotates range results as List(Int) must continue to
    // typecheck.
    assert_ok(
        r#"
        fn main() {
            let r: List(Int) = 1..10
            r
        }
        "#,
    );
}

// ── 3. List→Range coercion at annotation site ──────────────────────

#[test]
fn range_annotation_accepts_list_literal() {
    // b-lite coercion is bidirectional: a List(Int) literal can be
    // bound to a Range(Int) annotation. This keeps the nominal alias
    // maximally permissive without introducing separate subtyping
    // machinery.
    assert_ok(
        r#"
        fn main() {
            let r: Range(Int) = [1, 2, 3]
            r
        }
        "#,
    );
}

// ── 4. End-to-end: range flows into a list-typed call site ─────────

#[test]
fn range_flows_into_list_sum_call() {
    // The `list.sum` builtin has signature `(List(Int)) -> Int`
    // (see src/typechecker/builtins.rs). Passing a `Range(Int)` must
    // typecheck because Range unifies with List at the element level.
    assert_ok(
        r#"
        import list
        fn main() {
            let n: Int = list.sum(1..10)
            n
        }
        "#,
    );
}

// ── 5. Pattern match against a range ───────────────────────────────

#[test]
fn pattern_match_list_pattern_against_range() {
    // `[a, b, c]` is a list pattern. `match 1..3 { ... }` has
    // scrutinee type Range(Int); pattern-bind calls unify against
    // List(fresh) which succeeds via the Range↔List arm in unify.
    assert_ok(
        r#"
        fn main() -> Int {
            match 1..3 {
                [a, b, c] -> a + b + c,
                _ -> 0
            }
        }
        "#,
    );
}

// ── 6. Un-annotated inference picks Range(Int), not List(Int) ──────

#[test]
fn unannotated_range_infers_as_range_type() {
    // If inference ever silently widened `1..10` to List(Int), the
    // nominal distinction would be moot. We check the surface type by
    // feeding the range into a fn parameter typed Range(Int). If the
    // expression inferred as List(Int), unification would still
    // succeed (bidirectional), so the real lock is the internal
    // `Type::Range(_)` render in diagnostics — see the Display test
    // below.
    //
    // This test is a smoke check that the permissive direction still
    // works from un-annotated call sites.
    assert_ok(
        r#"
        fn takes_range(r: Range(Int)) -> Range(Int) { r }

        fn main() -> Range(Int) {
            let r = 1..10
            takes_range(r)
        }
        "#,
    );
}

// ── 7. Negative: element-type mismatch ─────────────────────────────

#[test]
fn range_annotation_rejects_element_type_mismatch() {
    let errs = type_errors(
        r#"
        fn main() {
            let r: Range(String) = 1..10
            r
        }
        "#,
    );
    assert!(
        !errs.is_empty(),
        "expected an element-type error, got none"
    );
    // The error comes from unifying Int against String at the
    // Range(_)~List(_) level — the element types are what mismatch.
    let joined = errs.join("\n");
    assert!(
        joined.contains("String") && joined.contains("Int"),
        "expected error to name both String and Int, got:\n{joined}"
    );
}

// ── 8. Display renders Range, not List ─────────────────────────────

#[test]
fn display_range_type_prints_range_prefix() {
    // Directly exercise the Display impl added in src/types.rs.
    let ty = Type::Range(Box::new(Type::Int));
    assert_eq!(format!("{ty}"), "Range(Int)");
}

#[test]
fn display_range_type_appears_in_diagnostic() {
    // Flow a range into a String-typed binder to force a mismatch
    // diagnostic against a Range. Without the Range Display arm, the
    // diagnostic would say "got List(Int)" — misleading, since the
    // user wrote `1..10`.
    let errs = type_errors(
        r#"
        fn main() {
            let s: String = 1..10
            s
        }
        "#,
    );
    assert!(!errs.is_empty(), "expected a type mismatch");
    let joined = errs.join("\n");
    assert!(
        joined.contains("Range(Int)"),
        "expected diagnostic to name Range(Int), got:\n{joined}"
    );
}

// ── Docs lock: the operators page was corrected ─────────────────────

#[test]
fn operators_doc_no_longer_claims_ranges_are_lazy_unconditionally() {
    // Before this change, docs/language/operators.md claimed "Ranges
    // are lazy and work anywhere a list does". That was false —
    // ranges are materialized at runtime today. We lock the corrected
    // wording here so the doc can't silently regress to the old
    // misleading claim.
    let doc = include_str!("../docs/language/operators.md");
    assert!(
        !doc.contains("Ranges are lazy and work anywhere a list does"),
        "docs still claim ranges are lazy; see docs/language/operators.md"
    );
    // The current doc should mention that range is a Range(Int) type
    // and that materialization is eager today.
    assert!(
        doc.contains("Range(Int)"),
        "docs should name the Range type explicitly"
    );
    assert!(
        doc.contains("materialized") || doc.contains("materialize"),
        "docs should explain eager materialization"
    );
}
