//! Round-23 GAP #1 regression test.
//!
//! `trait Greet for Widget { ... }` where `Widget` is never declared
//! used to fall through `register_trait_impl`'s target-resolution path
//! — specifically `type_from_name`, which synthesises
//! `Type::Generic("Widget", vec![])` for any unknown uppercase symbol —
//! so `silt check` reported success even though the impl attached
//! methods to a phantom type that no value could ever inhabit.
//!
//! The fix validates `ti.target_type` against the set of known names
//! (primitives, user records, user enums, and the builtin container /
//! opaque families) and emits a hard error when the symbol matches none
//! of them. This is distinct from the round-17 `type_name_for_impl`
//! (`Fn → Some("Fun")`) fix — that one normalises a *known* target, this
//! one rejects a target that doesn't exist at all.

use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::types::Severity;

/// Typecheck-only: collect hard-error messages.
fn type_errors(input: &str) -> Vec<String> {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    typechecker::check(&mut program)
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

/// Primary repro from the audit finding: `Widget` is never declared.
/// Before the fix this program type-checked silently; after the fix it
/// must produce a specific "not a declared type" diagnostic.
#[test]
fn test_trait_impl_target_must_be_declared() {
    let errs = type_errors(
        r#"
trait Greet { fn hi(self) -> String }
trait Greet for Widget { fn hi(self) -> String = "hi" }
fn main() { }
"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("trait impl target 'Widget' is not a declared type")),
        "expected undeclared-target diagnostic, got: {errs:?}"
    );
}

/// Declared enum targets must continue to type-check without error.
#[test]
fn test_trait_impl_target_enum_accepted() {
    let errs = type_errors(
        r#"
type Shape { Circle, Square }
trait Area { fn area(self) -> Int }
trait Area for Shape { fn area(self) -> Int = 1 }
fn main() { }
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no type errors for declared enum target, got: {errs:?}"
    );
}

/// Declared record targets must continue to type-check without error.
#[test]
fn test_trait_impl_target_record_accepted() {
    let errs = type_errors(
        r#"
type Point { x: Int, y: Int }
trait Show { fn show(self) -> String }
trait Show for Point { fn show(self) -> String = "p" }
fn main() { }
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no type errors for declared record target, got: {errs:?}"
    );
}

/// Primitive targets (Int, Float, Bool, String) must continue to work —
/// they pre-date the declaration check and are a common trait-impl
/// pattern (see the `Zero for Int` / `Zero for Float` regressions in
/// tests/integration.rs).
#[test]
fn test_trait_impl_target_primitive_accepted() {
    let errs = type_errors(
        r#"
trait Zero { fn zero() -> Self }
trait Zero for Int { fn zero() -> Int = 0 }
fn main() { }
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no type errors for primitive Int target, got: {errs:?}"
    );
}

/// Builtin container targets (List(a), Map(k, v), ...) must continue
/// to work — they are resolved via the parametric `target_type_args`
/// path, not the bare `target_type` fall-through.
#[test]
fn test_trait_impl_target_builtin_list_accepted() {
    let errs = type_errors(
        r#"
trait Size { fn size(self) -> Int }
trait Size for List(a) { fn size(self) -> Int = 0 }
fn main() { }
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no type errors for List(a) target, got: {errs:?}"
    );
}

/// Builtin enum targets (Option, Result, ...) must continue to work —
/// they are registered in `self.enums` before user code runs, so they
/// satisfy the is_user_enum check.
#[test]
fn test_trait_impl_target_builtin_enum_accepted() {
    let errs = type_errors(
        r#"
trait Describe { fn describe(self) -> String }
trait Describe for Option(a) { fn describe(self) -> String = "opt" }
fn main() { }
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no type errors for Option(a) target, got: {errs:?}"
    );
}

/// Lowercase target names (`trait Display for a { ... }`) are the
/// generic-trait-impl form: `a` is a type variable, not a type name.
/// The declaration check must continue to accept these — this is the
/// pattern used by `test_trait_constraint_method_resolved` in
/// inference.rs and by real user code.
#[test]
fn test_trait_impl_target_lowercase_tyvar_accepted() {
    let errs = type_errors(
        r#"
trait Display for a {
  fn display(self) -> String = "?"
}
fn main() { }
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no errors for lowercase tyvar target, got: {errs:?}"
    );
}

/// Typos on otherwise-plausible names must be rejected — this is the
/// real-world motivation for the check.
#[test]
fn test_trait_impl_target_typo_rejected() {
    let errs = type_errors(
        r#"
type Point { x: Int, y: Int }
trait Show { fn show(self) -> String }
trait Show for Poitn { fn show(self) -> String = "p" }
fn main() { }
"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("trait impl target 'Poitn' is not a declared type")),
        "expected typo to trigger undeclared-target diagnostic, got: {errs:?}"
    );
}
