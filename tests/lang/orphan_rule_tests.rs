//! Trait-orphan rule tests (round 63 item 5).
//!
//! Silt rejects `impl Trait for Type` declarations in package P unless
//! at least one of `Trait` or `Type` is defined in P (or in the silt
//! built-in / stdlib). This file pins the rule's positive and negative
//! arms, the diagnostic wording, the auto-derive exemption, and the
//! REPL / scratch-package "rule disabled" behavior.
//!
//! The orphan check fires inside `register_trait_impl`. We exercise it
//! through `typechecker::check_with_package`, which is the same entry
//! point the compiler uses when typechecking a module under a known
//! owning package symbol. `typechecker::check` (no package) keeps the
//! pre-existing REPL / ad-hoc-script behavior — every decl looks local
//! to the scratch package, so the rule never trips.

use silt::intern::intern;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::types::Severity;

/// Type-check `input` as if it lived in package `pkg`. Returns the
/// list of hard-error messages.
fn errors_in_pkg(pkg: &str, input: &str) -> Vec<String> {
    let tokens = Lexer::new(input).tokenize().expect("lex error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    typechecker::check_with_package(&mut program, Some(intern(pkg)))
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

/// Type-check `input` with no package context (REPL / ad-hoc script).
/// The orphan rule is disabled in this mode.
fn errors_no_pkg(input: &str) -> Vec<String> {
    let tokens = Lexer::new(input).tokenize().expect("lex error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    typechecker::check_with_package(&mut program, None)
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

/// Returns the list of all (error + warning) diagnostics, including
/// non-orphan ones. Used by tests that want to assert the absence of
/// a specific orphan diagnostic without rejecting unrelated noise.
fn all_diagnostics_in_pkg(pkg: &str, input: &str) -> Vec<typechecker::TypeError> {
    let tokens = Lexer::new(input).tokenize().expect("lex error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    typechecker::check_with_package(&mut program, Some(intern(pkg)))
}

// ── Positive arms ──────────────────────────────────────────────────

/// Local trait + local type: both anchors are in the current package,
/// so the rule's "trait OR type must be local" requirement is satisfied
/// trivially.
#[test]
fn orphan_local_trait_local_type_accepted() {
    let errs = errors_in_pkg(
        "myapp",
        r#"
type Color { Red, Green, Blue }
trait Greet { fn greet(self) -> String }
trait Greet for Color {
  fn greet(self) -> String { "color" }
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no errors for local trait + local type, got: {errs:?}"
    );
}

/// Local trait + built-in type. The trait-local arm is enough: silt
/// owns `List`, but the user owns `MyTrait`, so registering
/// `impl MyTrait for List(a)` does not race with another package.
#[test]
fn orphan_local_trait_builtin_type_accepted() {
    let errs = errors_in_pkg(
        "myapp",
        r#"
trait Greet { fn greet(self) -> String }
trait Greet for List(a) {
  fn greet(self) -> String { "list" }
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no errors for local trait + built-in type, got: {errs:?}"
    );
}

/// Built-in trait + local type. The type-local arm is enough: silt
/// owns `Display`, but the user owns `Color`, so the auto-derived
/// `Display(Color)` slot can be safely overridden by a user impl.
#[test]
fn orphan_builtin_trait_local_type_accepted() {
    let errs = errors_in_pkg(
        "myapp",
        r#"
type Color { Red, Green, Blue }
trait Display for Color {
  fn display(self) -> String { "color" }
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no errors for built-in trait + local type, got: {errs:?}"
    );
}

// ── Negative arm: both foreign ─────────────────────────────────────

/// Built-in trait + built-in type: both anchors are stdlib-owned and
/// the user package owns neither. Reject.
#[test]
fn orphan_foreign_trait_foreign_type_rejected() {
    let errs = errors_in_pkg(
        "myapp",
        r#"
trait Display for List(a) {
  fn display(self) -> String { "stolen" }
}
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains("orphan impl")),
        "expected orphan diagnostic, got: {errs:?}"
    );
}

/// Diagnostic shape: the message must name both packages so the user
/// understands which constraints are foreign and where to move the
/// impl. The rule wording is locked here so future tweaks notice
/// downstream tooling that may key on it.
#[test]
fn orphan_diagnostic_message_shape() {
    let errs = errors_in_pkg(
        "myapp",
        r#"
trait Display for List(a) {
  fn display(self) -> String { "x" }
}
"#,
    );
    let msg = errs
        .iter()
        .find(|m| m.contains("orphan impl"))
        .unwrap_or_else(|| panic!("expected orphan diagnostic, got: {errs:?}"));
    assert!(
        msg.contains("orphan impl"),
        "message must lead with 'orphan impl': {msg}"
    );
    assert!(
        msg.contains("'Display'"),
        "message must name the trait: {msg}"
    );
    assert!(
        msg.contains("'List'"),
        "message must name the target type: {msg}"
    );
    assert!(
        msg.contains("__builtin__"),
        "message must name the trait's owning package (built-in): {msg}"
    );
    assert!(
        msg.contains("'myapp'"),
        "message must mention the current package: {msg}"
    );
}

/// The orphan diagnostic's caret should land on the impl block (the
/// `trait Foo for U { ... }` line), not somewhere unrelated. We assert
/// the rejection's span line falls inside the impl, mirroring the
/// span passed at the call site.
#[test]
fn orphan_message_locks_at_impl_span() {
    let src = r#"fn pad() { }
trait Display for List(a) {
  fn display(self) -> String { "x" }
}
"#;
    let tokens = Lexer::new(src).tokenize().expect("lex error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let diags = typechecker::check_with_package(&mut program, Some(intern("myapp")));
    let orphan: Vec<&typechecker::TypeError> = diags
        .iter()
        .filter(|e| e.message.contains("orphan impl"))
        .collect();
    assert_eq!(
        orphan.len(),
        1,
        "expected exactly one orphan diagnostic, got {}: {:?}",
        orphan.len(),
        diags.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
    // The impl block starts on line 2 (1-indexed). The caret must
    // point inside the impl (line >= 2), not at the unrelated `fn pad`
    // on line 1.
    let span = orphan[0].span;
    assert!(
        span.line >= 2,
        "orphan span.line should be >= 2 (impl block), got line={} col={}",
        span.line,
        span.col
    );
}

// ── Auto-derive exemption ──────────────────────────────────────────

/// Auto-derived synthetic impls bypass the orphan check. A user package
/// declares `type X` and the synth pass auto-derives `Display(List(X))`
/// (built-in trait + built-in head, where the type parameter is user-
/// owned). Without the exemption this would trip the orphan rule even
/// though it is conceptually the stdlib's specialised implementation.
#[test]
fn auto_derive_on_builtin_for_user_type_param_allowed() {
    let errs = errors_in_pkg(
        "myapp",
        r#"
type Tag { A, B }

fn main() {
  let xs: List(Tag) = [A, B]
  println(xs)
}
"#,
    );
    let orphan: Vec<&String> = errs.iter().filter(|m| m.contains("orphan impl")).collect();
    assert!(
        orphan.is_empty(),
        "auto-derived impls must bypass the orphan check; got: {orphan:?}"
    );
}

// ── REPL / scratch-package: rule disabled ─────────────────────────

/// With no current_package (REPL / ad-hoc script), every decl looks
/// local to the scratch package and the orphan rule is effectively
/// disabled. Locks the documented "playground" carve-out so the REPL
/// keeps working when the user types `trait Display for List(a)`
/// directly.
#[test]
fn orphan_rule_disabled_when_no_current_package() {
    let errs = errors_no_pkg(
        r#"
trait Display for List(a) {
  fn display(self) -> String { "scratch" }
}
"#,
    );
    let orphan: Vec<&String> = errs.iter().filter(|m| m.contains("orphan impl")).collect();
    assert!(
        orphan.is_empty(),
        "orphan rule must be disabled without current_package; got: {orphan:?}"
    );
}

// ── Direct typechecker probe (white-box) ───────────────────────────
//
// The pseudo-code in the implementation prompt suggests a white-box
// alternative when multi-package scaffolding is heavy: directly
// constructing the typechecker's package state. We exercise the same
// rule through the public `check_with_package` API instead — the test
// authoring is simpler and the rule's behaviour is identical.

/// Variant: when the trait has trait arguments (`trait Cast(Int) for
/// String`), the trait-local check still anchors on the trait's
/// `defined_in`; the local trait arm wins. Locks the third
/// surface-area judgment call (generic-arg-only trait impls).
#[test]
fn local_parameterized_trait_for_builtin_type_accepted() {
    let errs = errors_in_pkg(
        "myapp",
        r#"
trait Cast(b) { fn cast(self) -> b }
trait Cast(Int) for String {
  fn cast(self) -> Int { 0 }
}
"#,
    );
    let orphan: Vec<&String> = errs.iter().filter(|m| m.contains("orphan impl")).collect();
    assert!(
        orphan.is_empty(),
        "local parameterized trait + built-in type must satisfy the trait-local arm; got: {orphan:?}"
    );
}

/// Built-in enum head (`Option`) is treated the same as a built-in
/// container head: `__builtin__` stamp on `defined_in`, never local
/// on its own. A user package implementing a built-in trait for
/// `Option` is rejected for the same reason as `List` — both anchors
/// are foreign.
#[test]
fn orphan_builtin_trait_for_builtin_enum_rejected() {
    let errs = errors_in_pkg(
        "myapp",
        r#"
trait Display for Option(a) {
  fn display(self) -> String { "stolen-option" }
}
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains("orphan impl")),
        "expected orphan diagnostic for Display for Option, got: {errs:?}"
    );
}

/// Sanity probe: the orphan rule does NOT short-circuit other
/// diagnostics. A program with both a real type-error and an orphan
/// impl reports both — the orphan check runs before body inference but
/// only causes us to skip the offending impl's own registration, not
/// other decls. (Today the body of an orphan-rejected impl is still
/// emitted into compiler later; the typechecker simply doesn't add the
/// (trait, type) pair to its tables.)
#[test]
fn orphan_does_not_swallow_unrelated_diagnostics() {
    let diags = all_diagnostics_in_pkg(
        "myapp",
        r#"
trait Display for Map(k, v) {
  fn display(self) -> String { "x" }
}

fn typo() -> Int { "not an int" }
"#,
    );
    let messages: Vec<&str> = diags.iter().map(|e| e.message.as_str()).collect();
    let has_orphan = messages.iter().any(|m| m.contains("orphan impl"));
    // The body returns a String where Int is declared — any reasonable
    // mismatch wording locks the cohabitation property.
    let has_type_error = messages.iter().any(|m| {
        let lower = m.to_lowercase();
        (lower.contains("string") && lower.contains("int"))
            || lower.contains("mismatch")
            || lower.contains("expected")
    });
    assert!(
        has_orphan,
        "expected orphan diagnostic alongside the body type error; got: {messages:?}"
    );
    assert!(
        has_type_error,
        "expected a body-level type error to coexist with the orphan; got: {messages:?}"
    );
}
