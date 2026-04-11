//! Regression tests for audit fixes to the type-domain enforcement in
//! arithmetic/comparison operators, field access, unary negation, pipe
//! arity, duplicate top-level definitions, and unknown record types.
//!
//! See the audit bug IDs (B2, B3, B4, B5, B6, G1, G2) referenced below.

use silt::parser::Parser;
use silt::typechecker;
use silt::types::Severity;

/// Run the type checker and return hard-error messages.
fn type_errors(input: &str) -> Vec<String> {
    let tokens = silt::lexer::Lexer::new(input)
        .tokenize()
        .expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let errors = typechecker::check(&mut program);
    errors
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

/// Assert that at least one error contains the given substring.
fn assert_has_error(input: &str, needle: &str) {
    let errs = type_errors(input);
    assert!(
        errs.iter().any(|e| e.contains(needle)),
        "expected error containing '{needle}', got: {errs:?}"
    );
}

// ── B2: Arithmetic operand domain ──────────────────────────────────────

#[test]
fn b2_bool_plus_bool_is_rejected() {
    let src = r#"
fn add(a: Bool, b: Bool) -> Bool { a + b }
fn main() { let _ = add(true, false) }
"#;
    let errs = type_errors(src);
    // Lock the exact production error that the inference pass emits for
    // an invalid `+` operand:
    //   "operator '+' requires Int, Float, ExtFloat, or String, got 'Bool'"
    // The previous OR chain was catastrophically broad: `.contains("Int")`
    // matched most type errors, so a bug that rejected Bool + Bool for
    // the WRONG reason would silently still pass.
    assert!(
        errs.iter().any(|e| e
            .contains("operator '+' requires Int, Float, ExtFloat, or String, got 'Bool'")),
        "expected operator '+' Bool-domain rejection, got: {errs:?}"
    );
}

#[test]
fn b2_list_plus_list_is_rejected() {
    assert_has_error(
        r#"
fn main() {
  let xs = [1, 2] + [3, 4]
  xs
}
"#,
        "'+'",
    );
}

#[test]
fn b2_bool_mul_bool_is_rejected() {
    assert_has_error(
        r#"
fn main() {
  let x = true * false
  x
}
"#,
        "'*'",
    );
}

// ── B3: Comparison operand domain ──────────────────────────────────────

#[test]
fn b3_tuple_lt_tuple_is_rejected() {
    assert_has_error(
        r#"
fn main() {
  let a = (1, 2)
  let b = (3, 4)
  a < b
}
"#,
        "'<'",
    );
}

#[test]
fn b3_tuple_eq_tuple_is_allowed() {
    // Equality (Eq/Neq) SHOULD work on tuples.
    let errs = type_errors(
        r#"
fn main() {
  let a = (1, 2)
  let b = (3, 4)
  a == b
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no errors for tuple equality, got: {errs:?}"
    );
}

#[test]
fn b3_map_ordering_is_rejected() {
    assert_has_error(
        r#"
fn main() {
  let a = #{"x": 1}
  let b = #{"y": 2}
  a < b
}
"#,
        "'<'",
    );
}

// ── B4: Field access on unconstrained type variable ────────────────────

#[test]
fn b4_field_access_on_concrete_int_is_rejected() {
    // Concrete Int has no .foo.
    // (The polymorphic case — `fn mystery(x) { x.foo }` — remains
    // accepted because Silt's current let-polymorphism architecture
    // can't re-check the body per call site; see the deferred check
    // comment in finalize_deferred_checks.)
    assert_has_error(
        r#"
fn main() {
  let x = 42
  let _ = x.foo
  0
}
"#,
        "foo",
    );
}

#[test]
fn b4_field_access_on_concrete_record_wrong_field_is_rejected() {
    let errs = type_errors(
        r#"
type Point { x: Int, y: Int }
fn main() {
  let p = Point { x: 1, y: 2 }
  let _ = p.z
  0
}
"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("z") || e.contains("unknown")),
        "expected an unknown-field error, got: {errs:?}"
    );
}

// ── B5: Unary `-` on unresolved var ────────────────────────────────────

#[test]
fn b5_unary_neg_on_string_is_rejected() {
    // Direct unary negation of a String binding — concrete at inference time.
    // NOTE: the audit's original repro (`let id = {x -> -x}; id("hello")`)
    // relies on polymorphic let-generalization that the current Silt
    // architecture doesn't re-check per call site, so the concrete case
    // is what we exercise here. See report for B5 status.
    assert_has_error(
        r#"
fn main() {
  let s = "hello"
  let _ = -s
  0
}
"#,
        "unary '-'",
    );
}

#[test]
fn b5_unary_neg_on_list_is_rejected() {
    assert_has_error(
        r#"
fn main() {
  let xs = [1, 2, 3]
  let _ = -xs
  0
}
"#,
        "unary '-'",
    );
}

// ── B6: Pipe into non-Call with wrong arity ────────────────────────────

#[test]
fn b6_pipe_into_two_arg_fn_is_rejected() {
    assert_has_error(
        r#"
fn add(a: Int, b: Int) -> Int { a + b }
fn main() {
  let r = 5 |> add
  r + 1
}
"#,
        "pipe",
    );
}

// ── G1: Duplicate top-level definitions ────────────────────────────────

#[test]
fn g1_duplicate_fn_is_rejected() {
    assert_has_error(
        r#"
fn greet() { "hi" }
fn greet() { "bye" }
fn main() { greet() }
"#,
        "duplicate top-level",
    );
}

#[test]
fn g1_duplicate_top_let_is_rejected() {
    assert_has_error(
        r#"
let x = 1
let x = 2
fn main() { x }
"#,
        "duplicate top-level",
    );
}

#[test]
fn g1_duplicate_type_is_rejected() {
    assert_has_error(
        r#"
type Foo { A }
type Foo { B }
fn main() { 0 }
"#,
        "duplicate top-level",
    );
}

#[test]
fn g1_inner_shadowing_is_allowed() {
    let errs = type_errors(
        r#"
fn main() {
  let x = 1
  let x = 2
  x
}
"#,
    );
    assert!(
        errs.is_empty(),
        "inner-scope shadowing should be allowed, got errors: {errs:?}"
    );
}

// ── G2: Unknown record type ────────────────────────────────────────────

#[test]
fn g2_unknown_record_type_is_rejected() {
    assert_has_error(
        r#"
fn main() {
  let p = NotDeclared { x: 1, y: 2 }
  p
}
"#,
        "undefined type",
    );
}
