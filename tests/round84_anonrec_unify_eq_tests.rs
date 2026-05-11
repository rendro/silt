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
//! Tests drive the full lex → parse → typecheck → compile → VM
//! pipeline via the `silt run` CLI, mirroring `round83`.

use std::process::Command;

/// Drive the full lex → parse → typecheck → compile → VM pipeline via
/// the `silt run` CLI and return (stdout, stderr, success).
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_round84_{label}.silt"));
    std::fs::write(&tmp, src).expect("write temp file");
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .arg("run")
        .arg(&tmp)
        .output()
        .expect("spawn silt run");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (stdout, stderr, out.status.success())
}

fn run_silt_ok(label: &str, src: &str) -> String {
    let (stdout, stderr, ok) = run_silt_raw(label, src);
    assert!(
        ok,
        "silt run should succeed for {label}; stdout={stdout}, stderr={stderr}"
    );
    stdout
}

fn first_line(out: &str) -> &str {
    out.lines().next().unwrap_or("").trim_end()
}

// ── Audit repro (a): direct ascription ───────────────────────────────

/// Case (a): a nominal value bound to an anon-typed local must compare
/// equal to an anon literal of the same shape. The typechecker widens
/// `P` to `{x: Int}` at the binding edge via `unify_anon_nominal`.
#[test]
fn nominal_eq_anon_literal_via_direct_ascription_prints_true() {
    let src = r#"
type P { x: Int }
fn main() {
  let p: P = P { x: 1 }
  let r: { x: Int } = { x: 1 }
  println(p == r)
}
"#;
    let out = run_silt_ok("direct_ascription", src);
    assert_eq!(
        first_line(&out),
        "true",
        "nominal `P{{x:1}}` must compare equal to anon literal `{{x:1}}` (the typechecker unified P with {{x: Int}} at the binding edge); got: {out:?}"
    );
}

// ── Audit repro (b): function-arg widening ───────────────────────────

/// Case (b): passing a nominal value `P` to a function whose parameter
/// is anon-typed must widen at the call boundary. Inside the function,
/// `r` is now anon-typed; `==` against an anon literal must succeed.
#[test]
fn nominal_eq_anon_literal_via_fn_arg_widening_prints_true() {
    let src = r#"
type P { x: Int }
fn f(r: { x: Int }) -> Bool { r == { x: 1 } }
fn main() {
  let p: P = P { x: 1 }
  println(f(p))
}
"#;
    let out = run_silt_ok("fn_arg_widening", src);
    assert_eq!(
        first_line(&out),
        "true",
        "passing nominal `P` to a `{{x: Int}}` parameter must unify; the inner `==` against an anon literal must return true; got: {out:?}"
    );
}

// ── Audit repro (c): dot-update on anon-typed nominal ────────────────

/// Case (c): a nominal value bound to an anon-typed local, then
/// dot-updated. The static type of the update is anon (parent was
/// anon), so `==` against an anon literal must succeed.
#[test]
fn dot_update_on_anon_typed_nominal_eq_anon_literal_prints_true() {
    let src = r#"
type P { x: Int }
fn main() {
  let p: { x: Int } = P { x: 1 }
  let q = p.{ x: 2 }
  println(q == { x: 2 })
}
"#;
    let out = run_silt_ok("dot_update_anon_typed_nominal", src);
    assert_eq!(
        first_line(&out),
        "true",
        "dot-update of an anon-typed nominal must yield an anon-comparable record; got: {out:?}"
    );
}

// ── Regression: don't make all records collapse to "equal" ───────────

/// Sanity: the fix collapses NAME-equality when one side is anon, but
/// FIELDS must still differ to be unequal. Different field values ⇒
/// `==` returns `false`.
#[test]
fn nominal_neq_anon_literal_with_different_fields_prints_false() {
    let src = r#"
type P { x: Int }
fn main() {
  let p: P = P { x: 1 }
  let r: { x: Int } = { x: 2 }
  println(p == r)
}
"#;
    let out = run_silt_ok("diff_fields_anon", src);
    assert_eq!(
        first_line(&out),
        "false",
        "distinct field values must remain unequal even after the anon-name relaxation; got: {out:?}"
    );
}

// ── Regression: don't break nominal-vs-nominal equality ──────────────

/// Two `Person` values with the same fields must still compare equal —
/// the value-level fix must NOT regress nominal record equality.
#[test]
fn nominal_eq_nominal_same_type_still_works() {
    let src = r#"
type Person { x: Int }
fn main() {
  let p: Person = Person { x: 1 }
  let s: Person = Person { x: 1 }
  println(p == s)
}
"#;
    let out = run_silt_ok("nominal_eq_nominal", src);
    assert_eq!(
        first_line(&out),
        "true",
        "nominal-vs-nominal record equality must still work; got: {out:?}"
    );
}

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
