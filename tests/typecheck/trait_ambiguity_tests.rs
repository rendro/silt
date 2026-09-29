//! Lock tests for method-name ambiguity across traits on the same type.
//!
//! Two distinct user-defined traits providing the same method name
//! (e.g., both `trait A` and `trait B` declare `fn show(self) -> ...`)
//! and BOTH implemented on the same target (e.g., `Int`) creates a
//! receiver-method dispatch ambiguity. The typechecker must reject the
//! second registration with an error naming both traits — otherwise the
//! later impl silently overwrites the earlier one in `method_table` and
//! routes every `.show()` call to the last-registered trait.
//!
//! See `src/typechecker/mod.rs::register_trait_impl` (the
//! "ambiguous method '{}' on type '{}': provided by traits {}, {}"
//! diagnostic) for the implementation lock.

use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::types::Severity;

// ── Helpers ─────────────────────────────────────────────────────────

fn type_errors(input: &str) -> Vec<String> {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    typechecker::check(&mut program)
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

// ── Negative: same method name across two traits on same type ───────

/// Two traits define `show(self) -> String` and both are implemented
/// for Int. Registration of the second impl must error with an
/// ambiguity diagnostic naming both traits.
#[test]
fn test_two_traits_same_method_same_type_is_ambiguous() {
    let errs = type_errors(
        r#"
trait A { fn show(self) -> String }
trait B { fn show(self) -> String }
trait A for Int { fn show(self) -> String { "from-A" } }
trait B for Int { fn show(self) -> String { "from-B" } }
fn main() { }
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains("ambiguous method 'show'")
            && e.contains("'Int'")
            && e.contains("A")
            && e.contains("B")),
        "expected ambiguity diagnostic naming both traits, got: {errs:?}"
    );
}

/// The ambiguity diagnostic must mention the type, the method name, and
/// both conflicting trait names. This is a stricter substring check than
/// the previous test — it pins the exact format so a refactor of the
/// diagnostic surface that drops a name will fail loudly.
#[test]
fn test_ambiguity_error_names_both_conflicting_traits() {
    let errs = type_errors(
        r#"
trait Display1 { fn render(self) -> String }
trait Display2 { fn render(self) -> String }
trait Display1 for String { fn render(self) -> String { "1" } }
trait Display2 for String { fn render(self) -> String { "2" } }
fn main() { }
"#,
    );
    let combined = errs.join("\n");
    assert!(
        combined.contains("ambiguous method 'render'")
            && combined.contains("'String'")
            && combined.contains("Display1")
            && combined.contains("Display2"),
        "expected ambiguity error mentioning method, type, and both traits, got: {errs:?}"
    );
}

// ── Positive: same method name on different types is fine ───────────

/// Two traits with the same method name, but each implemented on a
/// DIFFERENT type, must NOT trigger ambiguity — receiver-method dispatch
/// on `(some_int).show()` resolves uniquely against trait A, and
/// `(some_string).show()` against trait B. There is no method_table
/// collision because the keys are `(Int, show)` and `(String, show)`.
#[test]
fn test_same_method_name_on_different_types_is_fine() {
    let errs = type_errors(
        r#"
trait A { fn show(self) -> String }
trait B { fn show(self) -> String }
trait A for Int { fn show(self) -> String { "int" } }
trait B for String { fn show(self) -> String { self } }
fn main() {
  println((5).show())
  println(("hi").show())
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no ambiguity when traits target different types, got: {errs:?}"
    );
}

/// A single trait implemented on multiple types is the canonical
/// non-ambiguous shape — no two distinct traits ever collide on the
/// same `(type, method)` key. Sanity-check that this still works.
#[test]
fn test_same_trait_multiple_types_is_fine() {
    let errs = type_errors(
        r#"
trait Show { fn show(self) -> String }
trait Show for Int { fn show(self) -> String { "int" } }
trait Show for String { fn show(self) -> String { self } }
trait Show for Bool {
  fn show(self) -> String { match self { true -> "T", false -> "F" } }
}
fn main() {
  println((1).show())
  println(("x").show())
  println(true.show())
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no errors for one trait on multiple types, got: {errs:?}"
    );
}

/// Registering both impls on the same type but with DIFFERENT method
/// names — even though the traits have a different shared method
/// elsewhere — is fine, because the conflict is per-(type, method).
#[test]
fn test_distinct_methods_from_distinct_traits_on_same_type_is_fine() {
    let errs = type_errors(
        r#"
trait A { fn alpha(self) -> Int }
trait B { fn beta(self) -> Int }
trait A for Int { fn alpha(self) -> Int { 1 } }
trait B for Int { fn beta(self) -> Int { 2 } }
fn main() {
  println((5).alpha())
  println((5).beta())
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no ambiguity for distinct methods, got: {errs:?}"
    );
}
