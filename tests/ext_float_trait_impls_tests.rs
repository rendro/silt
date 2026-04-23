//! Regression tests for the `ExtFloat` auto-derived trait-impl fix
//! (round-52 audit finding).
//!
//! BROKEN (pre-fix): `ExtFloat` — the widened-float type produced by
//! `Float / Float` (see `src/typechecker/inference.rs:2326-2333`) — was
//! absent from the primitive list passed to
//! `register_auto_derived_impls_for` in `register_builtin_trait_impls`
//! (`src/typechecker/mod.rs`). `type_name_for_impl` already mapped
//! `Type::ExtFloat -> intern("ExtFloat")`, so any attempt to route a
//! dividend through a trait boundary (`Display`/`Equal`/`Compare`/`Hash`)
//! was silently rejected with
//! `"type 'ExtFloat' does not implement trait '<T>'"`. The VM side
//! (`src/value.rs`, `src/vm/mod.rs:613`) already supports display,
//! equality, ordering, and hashing for `ExtFloat`, so the rejection was
//! purely a typechecker gap.
//!
//! Fix: add `"ExtFloat"` to the primitives list in
//! `register_builtin_trait_impls` so it auto-derives all four built-in
//! traits (`Equal`/`Compare`/`Hash`/`Display`), matching the set derived
//! for `Float`.
//!
//! Each test below was authored to FAIL against the pre-fix tree (where
//! the typechecker rejects the trait bound) and PASS after the fix.
//! Programs intentionally use `Float / Float` to produce an `ExtFloat`
//! and then route that value through a trait-bounded generic function
//! so the trait lookup happens at the generic call site.

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

/// The pre-fix failure mode: the typechecker rejects the trait bound
/// with a diagnostic that mentions both `ExtFloat` and the trait name.
/// We assert there is NO such error.
fn assert_no_ext_float_trait_rejection(errs: &[String], trait_name: &str) {
    let offenders: Vec<&String> = errs
        .iter()
        .filter(|e| e.contains("ExtFloat") && e.contains(trait_name))
        .collect();
    assert!(
        offenders.is_empty(),
        "expected no 'ExtFloat does not implement {trait_name}' error, got: {offenders:?} (all errors: {errs:?})"
    );
}

// ── Equal ───────────────────────────────────────────────────────────

#[test]
fn ext_float_satisfies_equal_trait_bound() {
    // `Float / Float` widens to `ExtFloat`; passing the result into a
    // generic fn constrained `where a: Equal` used to fail typechecking
    // with "type 'ExtFloat' does not implement trait 'Equal'".
    let errs = type_errors(
        r#"
fn eq(a: a, b: a) -> Bool where a: Equal { a.equal(b) }
fn main() {
  let x: Float = 3.0
  let y: Float = 2.0
  let r = x / y
  println(eq(r, r))
}
"#,
    );
    assert_no_ext_float_trait_rejection(&errs, "Equal");
    assert!(
        errs.is_empty(),
        "expected clean typecheck for ExtFloat+Equal, got: {errs:?}"
    );
}

// ── Display ─────────────────────────────────────────────────────────

#[test]
fn ext_float_satisfies_display_trait_bound() {
    // `println` ultimately requires `Display`. Routing the ExtFloat
    // value through a `where a: Display` bound used to fail typecheck.
    let errs = type_errors(
        r#"
fn show(a: a) -> String where a: Display { a.display() }
fn main() {
  let x: Float = 3.0
  let y: Float = 2.0
  let r = x / y
  println(show(r))
}
"#,
    );
    assert_no_ext_float_trait_rejection(&errs, "Display");
    assert!(
        errs.is_empty(),
        "expected clean typecheck for ExtFloat+Display, got: {errs:?}"
    );
}

// ── Hash ────────────────────────────────────────────────────────────

#[test]
fn ext_float_satisfies_hash_trait_bound() {
    // Hashing the widened dividend — f64 bit-pattern hashing is the
    // standard implementation — must typecheck.
    let errs = type_errors(
        r#"
fn h(a: a) -> Int where a: Hash { a.hash() }
fn main() {
  let x: Float = 3.0
  let y: Float = 2.0
  let r = x / y
  println(h(r))
}
"#,
    );
    assert_no_ext_float_trait_rejection(&errs, "Hash");
    assert!(
        errs.is_empty(),
        "expected clean typecheck for ExtFloat+Hash, got: {errs:?}"
    );
}

// ── Compare ─────────────────────────────────────────────────────────

#[test]
fn ext_float_satisfies_compare_trait_bound() {
    // Ordering comparisons on an ExtFloat must typecheck. `Compare`
    // is part of the primitives-auto-derived set (alongside Int/Float)
    // so `ExtFloat` needs to be in that same row.
    let errs = type_errors(
        r#"
fn cmp(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn main() {
  let x: Float = 3.0
  let y: Float = 2.0
  let r = x / y
  println(cmp(r, r))
}
"#,
    );
    assert_no_ext_float_trait_rejection(&errs, "Compare");
    assert!(
        errs.is_empty(),
        "expected clean typecheck for ExtFloat+Compare, got: {errs:?}"
    );
}
