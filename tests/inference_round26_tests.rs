//! Regression tests for round-26 audit findings on
//! `src/typechecker/inference.rs`.
//!
//! Covers:
//!  - B2: or-pattern variable-set mismatch must render symbol sets
//!    with source-level names, not the `Symbol(N: "x")` debug form.
//!  - L1: `let Ctor(x) = v` where `Ctor` is a declared record type
//!    must emit the "use record-pattern syntax" hint (mirroring the
//!    round-23 fix on the match arm), not a bare "undefined
//!    constructor" error.
//!  - L2: check_pattern arity error on constructor patterns must
//!    include the constructor name (matches bind_pattern wording).
//!  - L3: "undefined constructor" error caret must land on the
//!    pattern span, not the enclosing match/let scrutinee.
//!  - L4: pin-pattern "undefined variable" caret must land on the
//!    pin pattern, not the outer match.
//!  - L5: unknown-field diagnostics (record access, record literal,
//!    record pattern) must append a "did you mean `<field>`?" hint
//!    when the typo is close to a declared field, and must NOT add a
//!    hint when no candidate passes the suggest-similar threshold.

use silt::typechecker;
use silt::types::{Severity, TypeError};

fn type_errors(input: &str) -> Vec<TypeError> {
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
        .collect()
}

fn error_messages(input: &str) -> Vec<String> {
    type_errors(input).into_iter().map(|e| e.message).collect()
}

// ── B2: or-pattern variable-set error format ────────────────────────

#[test]
fn test_or_pattern_var_mismatch_renders_resolved_name() {
    // Round-26 B2 canonical repro. Before the fix the message embedded
    // `{Symbol(6: "x")}` which leaks the interner's Debug form into a
    // user-facing diagnostic.
    let errs = error_messages(
        r#"
type Shape { Circle(Int), Empty }
fn main() {
  let s = Circle(5)
  match s {
    Circle(x) | Empty -> println("ok")
  }
}
"#,
    );
    let hit = errs
        .iter()
        .find(|m| m.contains("or-pattern alternatives must bind"))
        .unwrap_or_else(|| {
            panic!(
                "expected an or-pattern variable-mismatch error, got: {errs:?}"
            )
        });
    // The resolved name must appear. We accept either bare `x` or the
    // quoted `'x'` form; the critical invariant is that the interner's
    // Debug form does NOT leak.
    assert!(
        !hit.contains("Symbol("),
        "or-pattern error must not leak `Symbol(...)` debug output, got: {hit:?}"
    );
    assert!(
        hit.contains('x'),
        "or-pattern error must mention the bound name `x`, got: {hit:?}"
    );
    // The bound-set rendering should use `{x}` (or similar) rather
    // than the BTreeSet `{Symbol(...)}` format.
    assert!(
        hit.contains("{x}") || hit.contains("{ x }"),
        "expected the first alternative's binding set rendered as `{{x}}`, got: {hit:?}"
    );
    assert!(
        hit.contains("{}"),
        "expected the second alternative's empty binding set rendered as `{{}}`, got: {hit:?}"
    );
}

// ── L1: bind_pattern record-type constructor hint ──────────────────

#[test]
fn test_bind_pattern_record_type_as_ctor_pattern_suggests_record_syntax() {
    // Round-26 L1 repro: `let Circle(r) = c` where `Circle` is a
    // record type. Before the fix this gave the generic "undefined
    // constructor" error; round-23 fixed the same case in the match
    // arm, and L1 mirrors that fix into bind_pattern.
    let errs = error_messages(
        r#"
type Circle { radius: Int }
fn main() {
  let c = Circle { radius: 5 }
  let Circle(r) = c
  println(r)
}
"#,
    );
    let hit = errs
        .iter()
        .any(|m| m.contains("record type") && m.contains("Circle") && m.contains("record-pattern"));
    assert!(
        hit,
        "expected a record-pattern suggestion for `let Circle(r) = c`, got: {errs:?}"
    );
    // The legacy "undefined constructor" phrasing must NOT appear — the
    // record IS defined; the shape is what's wrong.
    assert!(
        !errs
            .iter()
            .any(|m| m.contains("undefined constructor 'Circle'")),
        "the record-aware hint should replace the 'undefined constructor' message, got: {errs:?}"
    );
}

#[test]
fn test_bind_pattern_enum_variant_ctor_still_works() {
    // Control: `let Some(x) = foo` still works normally for an enum
    // variant (no spurious error, no record hint). We just verify the
    // typechecker accepts the pattern without an undefined-constructor
    // error, since the pattern is well-formed.
    let errs = error_messages(
        r#"
type Maybe { Some(Int), None }
fn main() {
  let v = Some(5)
  let Some(x) = v
  println(x)
}
"#,
    );
    assert!(
        errs.iter().all(|m| !m.contains("undefined constructor 'Some'")
            && !m.contains("is a record type")),
        "enum-variant binding should not trigger undefined-constructor or record hint, got: {errs:?}"
    );
}

// ── L2: check_pattern constructor arity error includes name ──────────

#[test]
fn test_check_pattern_arity_error_names_the_constructor() {
    // Round-26 L2: the match-arm arity error previously said
    // "constructor expects 1 field, but pattern has 2" without telling
    // the user WHICH constructor is wrong. Mirror bind_pattern's
    // wording which already names the constructor.
    let errs = error_messages(
        r#"
fn main() {
  match Some(1) {
    Some(x, y) -> println(x)
    None -> println(0)
  }
}
"#,
    );
    let hit = errs
        .iter()
        .any(|m| m.contains("constructor 'Some'") && m.contains("expects"));
    assert!(
        hit,
        "expected arity error to name 'Some', got: {errs:?}"
    );
}

// ── L3: "undefined constructor" caret falls on pattern span ──────────

#[test]
fn test_undefined_constructor_span_is_pattern_not_outer_match() {
    // Round-26 L3: the undefined-constructor error's span used to
    // point at the enclosing match head, not at the `Unknown(x)`
    // sub-pattern. Verify the caret now lands on the pattern itself.
    //
    // In the program below, `match s {` is on line 4 (col 3) and
    // `Unknown(x) ->` is on line 5. We assert the reported line is
    // line 5 (the pattern), not line 4 (the match head).
    let src = "\
type Shape { Circle(Int) }
fn main() {
  let s = Circle(5)
  match s {
    Unknown(x) -> println(0)
    Circle(r) -> println(r)
  }
}
";
    let errs = type_errors(src);
    let hit = errs
        .iter()
        .find(|e| e.message.contains("undefined constructor 'Unknown'"))
        .unwrap_or_else(|| panic!("expected undefined-constructor error, got: {:?}",
            errs.iter().map(|e| e.message.clone()).collect::<Vec<_>>()));
    // The pattern `Unknown(x)` is on line 5 of the source (1-indexed).
    assert_eq!(
        hit.span.line, 5,
        "expected caret on the pattern (line 5), got line {}: {}",
        hit.span.line, hit.message
    );
}

#[test]
fn test_undefined_constructor_span_is_pattern_in_let_binding() {
    // Companion coverage for the bind_pattern path at inference.rs:838.
    let src = "\
type Circle { radius: Int }
fn main() {
  let c = Circle { radius: 5 }
  let Unknown(x) = c
  println(x)
}
";
    let errs = type_errors(src);
    let hit = errs.iter().find(|e| {
        e.message.contains("undefined constructor 'Unknown'")
            // The record-aware hint replaces this message when the name
            // matches a record; `Unknown` isn't a record, so the
            // plain "undefined constructor" path must fire here.
    });
    let hit = hit.unwrap_or_else(|| {
        panic!(
            "expected undefined-constructor error on `let Unknown(x) = c`, got: {:?}",
            errs.iter().map(|e| e.message.clone()).collect::<Vec<_>>()
        )
    });
    // `let Unknown(x) = c` is on line 4 in the source above. The outer
    // `let` statement's span also starts on line 4, but round-26 L3
    // still requires pattern.span, not the enclosing let span. We
    // can't easily distinguish them by line here, so instead we pin
    // the column: the pattern's `Unknown` starts at col 7 (after
    // `  let `), the let statement starts at col 3.
    assert!(
        hit.span.col >= 7,
        "expected caret on the pattern (col >= 7), got col {}: {}",
        hit.span.col, hit.message
    );
}

// ── L4: pin-pattern span falls on pin pattern ──────────────────────

#[test]
fn test_pin_pattern_undefined_variable_span_is_pin_pattern() {
    // Round-26 L4: `^missing` in a pin pattern used to emit the error
    // at the outer match-arm head. Verify it now lands on the pin.
    let src = "\
fn main() {
  let v = 1
  match v {
    ^missing -> println(\"a\")
    _ -> println(\"b\")
  }
}
";
    let errs = type_errors(src);
    let hit = errs
        .iter()
        .find(|e| e.message.contains("undefined variable 'missing'"))
        .unwrap_or_else(|| {
            panic!(
                "expected pin-pattern undefined-variable error, got: {:?}",
                errs.iter().map(|e| e.message.clone()).collect::<Vec<_>>()
            )
        });
    // The pin pattern `^missing` is on line 4. The match head (`match v {`)
    // is on line 3. We want line 4.
    assert_eq!(
        hit.span.line, 4,
        "expected caret on the pin pattern (line 4), got line {}: {}",
        hit.span.line, hit.message
    );
}

// ── L5: "did you mean" hint on record field typos ──────────────────

#[test]
fn test_record_field_access_typo_suggests_similar() {
    // Round-26 L5: `u.nam` on a record with `name` field should
    // append `did you mean `name`?`.
    let errs = error_messages(
        r#"
type User { name: String, age: Int }
fn main() {
  let u = User { name: "Ann", age: 30 }
  println(u.nam)
}
"#,
    );
    let hit = errs
        .iter()
        .find(|m| m.contains("no field or method 'nam'"))
        .unwrap_or_else(|| {
            panic!(
                "expected 'no field or method nam' error, got: {errs:?}"
            )
        });
    assert!(
        hit.contains("did you mean `name`?"),
        "expected did-you-mean hint for `u.nam`, got: {hit:?}"
    );
}

#[test]
fn test_record_literal_unknown_field_suggests_similar() {
    // Round-26 L5: `User { nam: ..., age: ... }` should also suggest
    // `name`.
    let errs = error_messages(
        r#"
type User { name: String, age: Int }
fn main() {
  let u = User { nam: "B", age: 30 }
  println(u.age)
}
"#,
    );
    let hit = errs
        .iter()
        .find(|m| m.contains("unknown field 'nam'"))
        .unwrap_or_else(|| {
            panic!("expected 'unknown field nam' error, got: {errs:?}")
        });
    assert!(
        hit.contains("did you mean `name`?"),
        "expected did-you-mean hint for record-literal typo, got: {hit:?}"
    );
}

#[test]
fn test_record_pattern_unknown_field_suggests_similar() {
    // The `let User { nam } = u` shorthand goes through the
    // bind_pattern Record arm. The task explicitly calls for a
    // suggestion on this path.
    let errs = error_messages(
        r#"
type User { name: String, age: Int }
fn main() {
  let u = User { name: "Ann", age: 30 }
  let User { nam, age } = u
  println(age)
}
"#,
    );
    let hit = errs
        .iter()
        .find(|m| m.contains("has no field 'nam'"))
        .unwrap_or_else(|| {
            panic!(
                "expected 'has no field nam' error on record pattern, got: {errs:?}"
            )
        });
    assert!(
        hit.contains("did you mean `name`?"),
        "expected did-you-mean hint for record-pattern typo, got: {hit:?}"
    );
}

#[test]
fn test_record_field_access_no_near_match_omits_suggestion() {
    // Round-26 L5 control: a truly unrelated typo must NOT produce a
    // misleading suggestion. `completelydifferent` is miles away from
    // `name` and `age`.
    let errs = error_messages(
        r#"
type User { name: String, age: Int }
fn main() {
  let u = User { name: "Ann", age: 30 }
  println(u.completelydifferent)
}
"#,
    );
    let hit = errs
        .iter()
        .find(|m| m.contains("no field or method 'completelydifferent'"))
        .unwrap_or_else(|| {
            panic!("expected field-access error, got: {errs:?}")
        });
    assert!(
        !hit.contains("did you mean"),
        "unrelated typo must not produce a suggestion, got: {hit:?}"
    );
}
