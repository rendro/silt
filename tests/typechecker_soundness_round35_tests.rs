//! Regression tests for typechecker soundness/diagnostic findings
//! fixed in round 35. One test per finding; each test was written to
//! FAIL against the pre-fix codebase and PASS after the corresponding
//! edits in `src/typechecker/*.rs`.

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

// ── F3: bind_pattern literal arms must type-check scrutinee ──────────
//
// Repro: `let 5 = "hello"` used to silently typecheck because the
// Int/Float/Bool/StringLit arms in `bind_pattern` were empty. After the
// fix the scrutinee is unified against the literal's type and the
// mismatch is surfaced as a type error.
#[test]
fn test_f3_bind_pattern_int_literal_rejects_string_scrutinee() {
    let errs = type_errors(
        r#"
fn main() {
  let 5 = "hello"
}
"#,
    );
    // Must not silently accept — there MUST be an error.
    assert!(
        !errs.is_empty(),
        "expected a type error for `let 5 = \"hello\"`, got none"
    );
    // Lock on the specific mismatch phrasing. The unify path produces a
    // diagnostic mentioning both Int and String (one as expected, the
    // other as actual).
    assert!(
        errs.iter()
            .any(|e| e.contains("Int") && e.contains("String")),
        "expected type error mentioning both Int and String, got: {errs:?}"
    );
}

// ── F4: duplicate record-literal fields must be rejected ─────────────
//
// Repro: `User { name: "Alice", name: "Bob", age: 30 }` used to be
// silently deduped downstream. After the fix the typechecker surfaces
// "duplicate field 'name' in record literal for 'User'".
#[test]
fn test_f4_duplicate_record_literal_field_rejected() {
    let errs = type_errors(
        r#"
type User { name: String, age: Int }
fn main() {
  let u = User { name: "Alice", name: "Bob", age: 30 }
  println(u.name)
}
"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("duplicate field 'name'")
                && e.contains("record literal")),
        "expected `duplicate field 'name'` in record literal diagnostic, got: {errs:?}"
    );
}

// ── F5: extraneous trait-impl methods must be rejected ───────────────
//
// Repro: an impl that defines a method not declared in the trait
// (`fn secret(...)` below) used to silently register the extra method
// into the method_table. After the fix the typechecker surfaces
// "method 'secret' is not declared in trait 'Greeter'".
#[test]
fn test_f5_extraneous_trait_impl_method_rejected() {
    let errs = type_errors(
        r#"
trait Greeter {
  fn greet(self) -> String
}
trait Greeter for Int {
  fn greet(self) -> String { "hi" }
  fn secret(self) -> Int { 42 }
}
fn main() {
  println((1).greet())
}
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains("method 'secret'")
            && e.contains("not declared in trait")
            && e.contains("Greeter")),
        "expected `method 'secret' is not declared in trait 'Greeter'`, got: {errs:?}"
    );
}

// ── F6: duplicate method names in a trait impl must be rejected ──────
//
// Repro: two `fn say(self) -> String` methods in a single trait impl
// used to silently overwrite each other in the method_table. After the
// fix the second (and subsequent) occurrence produces
// "duplicate method 'say' in trait impl 'Greet for Int'".
#[test]
fn test_f6_duplicate_method_in_trait_impl_rejected() {
    let errs = type_errors(
        r#"
trait Greet {
  fn say(self) -> String
}
trait Greet for Int {
  fn say(self) -> String { "one" }
  fn say(self) -> String { "two" }
}
fn main() {
  println((1).say())
}
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains("duplicate method 'say'")
            && e.contains("trait impl")),
        "expected `duplicate method 'say' in trait impl ...`, got: {errs:?}"
    );
}

// ── F7: did-you-mean on named-record field access ────────────────────
//
// Repro: a parameter typed as a named record surfaces to inference as
// `Type::Generic("User", [])` rather than `Type::Record`, so field
// access on it previously emitted
// "unknown field or method 'nam' on type User" with no suggestion.
// After the fix the diagnostic is augmented with
// `help: did you mean \`name\`?` — the same hint the anonymous-record
// path has offered since round 26.
#[test]
fn test_f7_field_access_did_you_mean_named_record() {
    let errs = type_errors(
        r#"
type User { name: String, age: Int }
fn print_name(u: User) {
  println(u.nam)
}
fn main() {
  print_name(User { name: "x", age: 1 })
}
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains("did you mean `name`")),
        "expected did-you-mean suggestion for `name`, got: {errs:?}"
    );
}
