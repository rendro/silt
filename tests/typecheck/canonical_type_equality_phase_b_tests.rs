//! Lock tests for Phase B of the canonical type-equality refactor.
//!
//! The typecheck-level cases (Range annotations, Range impls on List
//! receivers, Range display fidelity) live as golden cases under
//! `tests/golden/typecheck/ranges/canonical_type_equality_phase_b__*`.
//! What remains here exercises the internal `types_equal` predicate
//! directly.

use silt::types::Type;
use silt::types::canonical::{Resolver, canonicalize, types_equal};

// ── Mixed nesting: types_equal collapses across the wrapper ─────

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
    let resolver = Resolver::new();
    assert!(
        types_equal(&resolver, &a, &b),
        "expected canonical equality across Fn(Range(Int)) -> List(Bool) ~ Fn(List(Int)) -> Range(Bool); \
         canonicalize(a) = {:?}, canonicalize(b) = {:?}",
        canonicalize(&resolver, &a),
        canonicalize(&resolver, &b)
    );
}
