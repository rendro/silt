//! Round 84 regression tests.
//!
//! ── BROKEN (extends round-83 incomplete fix): unify_anon_nominal soundness gap ──
//!
//! Round 83 patched ONE path of AnonRecord `==` divergence — the
//! `{...spread}` emission, via the new `Op::RecordUpdateAnon` opcode.
//! But three OTHER paths still produced wrong results:
//!
//!   (a) Direct ascription: `let r: { x: Int } = p` where `p: P`.
//!   (b) Function-arg widening: `fn f(r: { x: Int })` called with `P`.
//!   (c) Dot-update on anon-typed nominal: `p: { x: Int } = P{..}; p.{x:2}`.
//!
//! In each path the typechecker's `unify_anon_nominal`
//! (src/typechecker/mod.rs ~1556) widens
//! `Type::Record(P, fields)` ⇄ `Type::AnonRecord{closed}` at a
//! unification edge — i.e. the type system DECIDES these are the same
//! type — but the VM never rebrands the underlying `Value::Record`'s
//! `type_name`. So the original `Value::Record("P", ...)` flows
//! through unchanged, and `==` then compares it to a
//! `Value::Record("<anon>", ...)` literal of the same shape. The
//! naïve `na == nb && fa == fb` check (`src/value.rs` ~1714)
//! returned `false` — a runtime contradiction of the type-level
//! decision.
//!
//! Fix (option A, value-level): treat `"<anon>"` as a wildcard on the
//! name dimension. When at least one side carries
//! `type_name == "<anon>"`, compare fields-only. Two genuinely
//! distinct nominals (e.g. `Person{x:1}` vs `Car{x:1}`, neither anon)
//! still compare unequal — the typechecker would never unify those.
//!
//! The CLI-level cases (ascription, fn-arg widening, dot-update, and the
//! nominal/field-value controls) live as golden cases under
//! `tests/golden/typecheck/records/round84_anonrec_unify_eq__*`. What
//! remains here locks the invariant at the `Value::PartialEq` layer,
//! for shapes the typechecker rejects at source level.

// ── Regression: anon vs anon with different field sets ───────────────

/// Sanity: two anon records with different field SETS (not just
/// values) must remain unequal. Guards against an overzealous fix that
/// reduces equality to "any two anon records are equal".
///
/// The typechecker REJECTS the direct source-level comparison
/// `{x:Int,y:Int} == {x:Int}` (anon shape mismatch is a type error),
/// so we lock the invariant at the `Value::PartialEq` layer directly.
/// This is the layer the round-84 fix touches; the typechecker
/// already enforces the structural boundary above it.
#[test]
fn anon_neq_different_anon_shape_prints_false() {
    use silt::Value;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    let mut fa = BTreeMap::new();
    fa.insert("x".to_string(), Value::Int(1));
    fa.insert("y".to_string(), Value::Int(2));
    let a = Value::Record("<anon>".to_string(), Arc::new(fa));

    let mut fb = BTreeMap::new();
    fb.insert("x".to_string(), Value::Int(1));
    let b = Value::Record("<anon>".to_string(), Arc::new(fb));

    assert_ne!(
        a, b,
        "two anon records with different field SETS must remain unequal"
    );
}

// ── Critical regression: distinct nominals MUST NOT collapse ─────────

/// CRITICAL: two DISTINCT nominal types with the same fields, NEITHER
/// of which is anon, must still compare unequal. This is exactly the
/// failure mode the audit warns about — Option A must NOT let
/// `Person{x:1} == Car{x:1}` collapse to equal.
///
/// In well-typed source code the typechecker would reject the
/// comparison outright (`Person` and `Car` are distinct nominal
/// types), so we cannot drive this through the CLI. We lock the
/// invariant at the `Value::PartialEq` layer directly — that's the
/// layer Option A modifies. The fix must check `"<anon>"` on at least
/// one side before collapsing the name dimension; if both names are
/// concrete-and-different, the original `na == nb && fa == fb` arm
/// must apply and produce `false`.
#[test]
fn two_distinct_nominals_with_same_fields_still_neq() {
    use silt::Value;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    let mut fa = BTreeMap::new();
    fa.insert("x".to_string(), Value::Int(1));
    let person = Value::Record("Person".to_string(), Arc::new(fa));

    let mut fb = BTreeMap::new();
    fb.insert("x".to_string(), Value::Int(1));
    let car = Value::Record("Car".to_string(), Arc::new(fb));

    assert_ne!(
        person, car,
        "Person{{x:1}} and Car{{x:1}} (both concrete nominals, neither anon) must remain unequal — Option A must not collapse the name dimension unconditionally"
    );

    // And sanity: same nominal, same fields ⇒ equal.
    let mut fc = BTreeMap::new();
    fc.insert("x".to_string(), Value::Int(1));
    let person2 = Value::Record("Person".to_string(), Arc::new(fc));
    assert_eq!(person, person2, "Person == Person with same fields");
}
