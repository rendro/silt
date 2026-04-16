//! Lock tests for `trait X for List(a)`, `trait X for Map(k, v)`, and
//! `trait X for Set(a)` — i.e. user trait impls targeting builtin
//! container types.
//!
//! `register_trait_impl` resolves builtin container heads via
//! `resolve_type_expr`, which knows the arities of List/Map/Set/Channel/
//! Tuple/Fn. Arity mismatches must be rejected with a clear diagnostic;
//! correct-arity impls must register methods under the container's
//! method_table key (`"List"`, `"Map"`, `"Set"`) so that
//! receiver-method dispatch on a `Type::List(_)` / `Type::Map(_, _)` /
//! `Type::Set(_)` value can find them.

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

// ── Positive: List(a) target ────────────────────────────────────────

/// `trait Display for List(a) { ... }` must typecheck cleanly. The
/// impl-scoped tyvar `a` is the element type and remains polymorphic
/// across the impl body.
#[test]
fn test_user_trait_impl_for_list_param_typechecks() {
    let errs = type_errors(
        r#"
trait MyDisplay { fn my_display(self) -> String }
trait MyDisplay for List(a) {
  fn my_display(self) -> String { "list" }
}
fn main() { }
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no errors for `trait MyDisplay for List(a)`, got: {errs:?}"
    );
}

// ── Positive: Map(k, v) target ──────────────────────────────────────

/// `trait Display for Map(k, v) { ... }` must typecheck cleanly. Two
/// impl-scoped tyvars `k` and `v` are the key and value types.
#[test]
fn test_user_trait_impl_for_map_two_params_typechecks() {
    let errs = type_errors(
        r#"
trait MyDisplay { fn my_display(self) -> String }
trait MyDisplay for Map(k, v) {
  fn my_display(self) -> String { "map" }
}
fn main() { }
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no errors for `trait MyDisplay for Map(k, v)`, got: {errs:?}"
    );
}

// ── Positive: Set(a) target ─────────────────────────────────────────

/// `trait Display for Set(a) { ... }` — third builtin container shape.
#[test]
fn test_user_trait_impl_for_set_param_typechecks() {
    let errs = type_errors(
        r#"
trait MyDisplay { fn my_display(self) -> String }
trait MyDisplay for Set(a) {
  fn my_display(self) -> String { "set" }
}
fn main() { }
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no errors for `trait MyDisplay for Set(a)`, got: {errs:?}"
    );
}

// ── Negative: List arity mismatch ───────────────────────────────────

/// `List` has arity 1; `trait X for List(a, b)` is a hard error. The
/// diagnostic must mention the type name and the expected/got arities so
/// users immediately see what's wrong.
#[test]
fn test_user_trait_impl_for_list_wrong_arity_rejected() {
    let errs = type_errors(
        r#"
trait MyDisplay { fn my_display(self) -> String }
trait MyDisplay for List(a, b) {
  fn my_display(self) -> String { "bad" }
}
fn main() { }
"#,
    );
    assert!(
        errs.iter().any(|e| {
            e.contains("type argument count mismatch")
                && e.contains("'List'")
                && e.contains("expected 1")
                && e.contains("got 2")
        }),
        "expected List arity-mismatch diagnostic, got: {errs:?}"
    );
}

// ── Negative: Map arity mismatch ────────────────────────────────────

/// `Map` has arity 2; `trait X for Map(a)` must be rejected.
#[test]
fn test_user_trait_impl_for_map_wrong_arity_rejected() {
    let errs = type_errors(
        r#"
trait MyDisplay { fn my_display(self) -> String }
trait MyDisplay for Map(a) {
  fn my_display(self) -> String { "bad" }
}
fn main() { }
"#,
    );
    assert!(
        errs.iter().any(|e| {
            e.contains("type argument count mismatch")
                && e.contains("'Map'")
                && e.contains("expected 2")
                && e.contains("got 1")
        }),
        "expected Map arity-mismatch diagnostic, got: {errs:?}"
    );
}

// ── Negative: Set arity mismatch ────────────────────────────────────

/// `Set` has arity 1; passing two binders is a hard error.
#[test]
fn test_user_trait_impl_for_set_wrong_arity_rejected() {
    let errs = type_errors(
        r#"
trait MyDisplay { fn my_display(self) -> String }
trait MyDisplay for Set(a, b) {
  fn my_display(self) -> String { "bad" }
}
fn main() { }
"#,
    );
    assert!(
        errs.iter().any(|e| {
            e.contains("type argument count mismatch")
                && e.contains("'Set'")
                && e.contains("expected 1")
                && e.contains("got 2")
        }),
        "expected Set arity-mismatch diagnostic, got: {errs:?}"
    );
}
