//! Round-23 GAP #2 regression test.
//!
//! Cross-enum duplicate variant names used to silently shadow the
//! earlier-declared owner because `register_type_decl` called
//! `self.variant_to_enum.insert(variant.name, td.name)` unconditionally.
//! A program like:
//!
//! ```silt
//! type A { Red, Green }
//! type B { Red, Blue }
//! fn label(x: A) -> String { match x { Red -> "a-red", Green -> "a-green" } }
//! ```
//!
//! would type-check, but when the user tried to use `A::Red` by its
//! bare name the variant would resolve to `B::Red`, producing a
//! misleading "expected B, got A" diagnostic — the opposite of what
//! the code seemed to say. The fix emits a warning (not a hard error:
//! the language still resolves by most-recent-wins) that makes the
//! shadowing visible.
//!
//! Same-enum duplicates (`type C { Red, Red }`) were already caught
//! as a hard error in round-16 G3; this test file covers only the
//! cross-enum case. The same warning must also trigger when a user
//! `type Result { ... }` shadows the builtin Result/Ok/Err registered
//! before user code runs.

use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::types::Severity;

/// Typecheck and return warning messages only.
fn type_warnings(input: &str) -> Vec<String> {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    typechecker::check(&mut program)
        .into_iter()
        .filter(|e| e.severity == Severity::Warning)
        .map(|e| e.message)
        .collect()
}

/// Typecheck and return hard-error messages only.
fn type_errors(input: &str) -> Vec<String> {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    typechecker::check(&mut program)
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

/// Primary repro: two user enums each declare `Red`. The second
/// declaration must trigger a shadow warning that names both owners.
#[test]
fn test_cross_enum_variant_collision_warns() {
    let warnings = type_warnings(
        r#"
type A { Red, Green }
type B { Red, Blue }
fn main() { }
"#,
    );
    assert!(
        warnings.iter().any(|w| {
            w.contains("variant 'Red'")
                && w.contains("enum 'B'")
                && w.contains("enum 'A'")
                && w.contains("shadows")
        }),
        "expected cross-enum shadow warning naming both A and B, got: {warnings:?}"
    );
}

/// The shadowing must not be a hard error — silt still resolves the
/// bare name to the most-recently-declared owner, and existing programs
/// that rely on that behaviour should continue to compile.
#[test]
fn test_cross_enum_variant_collision_is_not_error() {
    let errs = type_errors(
        r#"
type A { Red, Green }
type B { Red, Blue }
fn main() { }
"#,
    );
    // The warning path must not spill into the hard-error list.
    assert!(
        !errs.iter().any(|e| e.contains("variant 'Red'")),
        "shadow should be a warning, not an error; got errors: {errs:?}"
    );
}

/// A user `type Result { ... }` shadows the builtin Ok/Err variants.
/// Because builtins populate `variant_to_enum` before user code runs,
/// this is structurally identical to the cross-enum case and must
/// produce the same warning.
#[test]
fn test_user_result_shadows_builtin() {
    let warnings = type_warnings(
        r#"
type Result { Ok, Err }
fn main() { }
"#,
    );
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("variant 'Ok'") && w.contains("shadows")),
        "expected shadow warning for user Result.Ok vs builtin Result.Ok, got: {warnings:?}"
    );
}

/// Disjoint variant sets must not warn — only actual name collisions.
#[test]
fn test_disjoint_enums_do_not_warn() {
    let warnings = type_warnings(
        r#"
type Color { Red, Green, Blue }
type Suit { Hearts, Diamonds, Clubs, Spades }
fn main() { }
"#,
    );
    assert!(
        !warnings.iter().any(|w| w.contains("shadows")),
        "disjoint enums must not produce shadow warnings, got: {warnings:?}"
    );
}

/// Same-enum duplicates must still be a hard error (round-16 G3),
/// never downgraded to a warning by this new check.
#[test]
fn test_same_enum_duplicate_still_hard_error() {
    let errs = type_errors(
        r#"
type Color { Red, Green, Red }
fn main() { }
"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("duplicate variant 'Red' in enum 'Color'")),
        "same-enum duplicate must remain a hard error, got: {errs:?}"
    );
}
