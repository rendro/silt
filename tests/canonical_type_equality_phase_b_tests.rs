//! Lock tests for Phase B of the canonical type-equality refactor.
//!
//! Phase A (commit 2919f1b) introduced
//! [`crate::types::canonical::canonicalize`] / `types_equal` /
//! `canonical_name` in `src/types/canonical.rs` but left them unwired.
//! Phase B routes the typechecker through them at the dispatch and
//! user-annotation entry points, then deletes the dedicated
//! Range-special-case redirects that previously kept the two shapes
//! interoperable through duplicated arm logic.
//!
//! Each test below exercises a path that previously had a Range
//! special case in the typechecker. The post-phase-B implementation
//! must keep these passing because:
//!
//!   - `resolve_type_expr` now canonicalises every user-written type
//!     annotation (Range → List) at the boundary, so the unifier and
//!     every dispatch table see one unified representative.
//!   - `type_name_for_impl`, `type_args_of`, and `trait_arg_compatible`
//!     canonicalise their inputs at entry, so the dedicated Range arm
//!     formerly required to map Range → "List" / strip an element /
//!     compare a Range pair is unreachable and was removed.
//!   - `register_trait_impl` canonicalises the target-type symbol
//!     (`canonicalize_type_name` in `src/typechecker/mod.rs`), so
//!     `trait X for Range(a)` registers under the same key the
//!     dispatch path looks up for both List and Range receivers.
//!
//! Display fidelity invariant: the Display impl on `Type` is
//! deliberately untouched. Tests that assert "Range" appears in a
//! diagnostic continue to rely on `ExprKind::Range` returning
//! `Type::Range(Box::new(Type::Int))` — the value-side type — for
//! diagnostic rendering. `display_still_says_range` below locks that
//! contract.

use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::types::{Severity, Type};
use silt::types::canonical::{canonicalize, types_equal};

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

// ── 1. Range-annotated let with a list value ───────────────────────

/// `let r: Range(Int) = [1, 2, 3]` must typecheck post-phase-B. The
/// pre-phase-B path relied on the dedicated `(Type::Range, Type::List)`
/// cross-arm in `unify` to bridge the annotation and the literal. With
/// `resolve_type_expr` canonicalising the annotation to `List(Int)`,
/// the unifier sees a `List(Int) ~ List(Int)` pair and the cross-arm
/// is not consulted on this path. The user-facing typecheck must
/// remain green.
#[test]
fn range_let_annotation_unifies_with_list_value() {
    assert_ok(
        r#"
        fn main() {
            let r: Range(Int) = [1, 2, 3]
            r
        }
        "#,
    );
}

// ── 2. List-annotated let with a range value ───────────────────────

/// `let xs: List(Int) = 1..5` must typecheck. The annotation
/// resolves directly to `List(Int)`; the value-side `1..5` still
/// produces `Type::Range(Int)` (preserved for display fidelity at
/// `ExprKind::Range`), so the unifier hits the Range/List cross-arm
/// at the value side. Phase B deliberately did not delete that
/// cross-arm — the value-side Range form is still load-bearing for
/// diagnostics.
#[test]
fn list_let_annotation_unifies_with_range_value() {
    assert_ok(
        r#"
        fn main() {
            let xs: List(Int) = 1..5
            xs
        }
        "#,
    );
}

// ── 3. Function parameter annotated as Range, called with a list ──

/// `fn f(x: Range(Int)) -> Int = list.length(x); f([1, 2, 3])`
/// exercises the canonicalisation at the function-application site:
/// the parameter annotation `Range(Int)` arrives at unify as
/// `List(Int)`, the argument type `List(Int)` from the literal
/// matches without any dedicated Range arm.
#[test]
fn fn_param_range_accepts_list_argument() {
    assert_ok(
        r#"
        import list
        fn f(x: Range(Int)) -> Int { list.length(x) }
        fn main() -> Int {
            f([1, 2, 3])
        }
        "#,
    );
}

// ── 4. Trait impl on Range, dispatched on a List receiver ──────────

/// Typechecker-level lock: `trait Foo for Range(a) { fn bar(self) }`
/// registers under the canonical `"List"` key (via phase B's
/// `canonicalize_type_name` at `register_trait_impl`), so a `List`
/// receiver finds the method at typecheck-time. Round-61's
/// `vm_range_receiver_trait_method_tests` covers the *symmetric*
/// case (impl on List, receiver Range) end-to-end through the VM;
/// the case here exercises the new direction the canonicalisation
/// at registration enables. Sibling end-to-end coverage for the
/// VM-level emission of `"Range.bar"` -> `"List.bar"` is
/// phase C scope (compiler ownership), not phase B.
///
/// Pre-phase-B: this typechecked because the receiver dispatch went
/// through `type_name_for_impl(List) = "List"` and the impl was
/// registered under "Range" — so the lookup
/// `(Foo, "List").bar` would miss in the method_table, but a
/// fallback path through the legacy `"Range.bar"` TypeEnv key
/// kicked in. With phase B, the canonical key is "List" on both
/// sides; the legacy fallback is no longer needed for typecheck.
#[test]
fn trait_impl_for_range_dispatches_on_list_receiver() {
    // Typechecker-only assertion: no runtime check (compiler-side
    // canonicalisation of the global-name emission is phase C).
    assert_ok(
        r#"
        trait Foo { fn bar(self) -> Int }
        trait Foo for Range(a) { fn bar(self) -> Int = 42 }
        fn main() -> Int {
            let xs = [1, 2, 3]
            xs.bar()
        }
        "#,
    );
}

// ── 5. Range field in a record, populated with a list ──────────────

/// A record with a `Range(Int)` field must accept a `List(Int)`
/// initializer. The field's annotation flows through
/// `resolve_type_expr` for the record-decl pass, canonicalising to
/// `List(Int)`; subsequent record-construction unifies field types
/// pairwise without needing the unify cross-arm.
#[test]
fn range_in_record_field_unifies_with_list_field() {
    assert_ok(
        r#"
        type R { xs: Range(Int) }
        fn main() -> R {
            R { xs: [1, 2, 3] }
        }
        "#,
    );
}

// ── 6. Mixed nesting: types_equal collapses across the wrapper ─────

/// Direct unit-level coverage for the canonical-equality predicate
/// at deeply nested positions. Locks that
/// `Fn(Range(Int)) -> List(Bool)` is canonical-equal to
/// `Fn(List(Int)) -> Range(Bool)`. This mirrors the behaviour the
/// typechecker now relies on at every dispatch site: structural
/// equality after Range-elimination.
#[test]
fn mixed_nesting_canonicalizes() {
    let a = Type::Fun(
        vec![Type::Range(Box::new(Type::Int))],
        Box::new(Type::List(Box::new(Type::Bool))),
    );
    let b = Type::Fun(
        vec![Type::List(Box::new(Type::Int))],
        Box::new(Type::Range(Box::new(Type::Bool))),
    );
    assert!(
        types_equal(&a, &b),
        "expected canonical equality across Fn(Range(Int)) -> List(Bool) ~ Fn(List(Int)) -> Range(Bool); \
         canonicalize(a) = {:?}, canonicalize(b) = {:?}",
        canonicalize(&a),
        canonicalize(&b)
    );
}

// ── 7. Display fidelity contract ───────────────────────────────────

/// Locks the user-facing-display contract: even after phase B routes
/// every internal lookup through canonical forms, a type-mismatch
/// diagnostic involving a Range-typed value still mentions the word
/// "Range". The value-side type is produced by `ExprKind::Range`
/// (`src/typechecker/inference.rs`) which deliberately keeps
/// returning `Type::Range(Box::new(Type::Int))` for display fidelity.
/// Sibling test in `tests/range_type_tests.rs::display_range_type_appears_in_diagnostic`
/// covers the same invariant; this one is duplicated in the phase-B
/// suite so a future refactor that accidentally canonicalises the
/// value-side too gets a phase-B-localised regression signal.
#[test]
fn display_still_says_range() {
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
        "expected diagnostic to spell 'Range(Int)' (display-fidelity contract); got:\n{joined}"
    );
}
