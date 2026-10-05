use super::super::test_helpers::*;
use super::super::*;

// ── is_bare_type_var ────────────────────────────────────────────

#[test]
fn test_resolved_var_is_not_bare() {
    let mut checker = TypeChecker::new();
    let tv = checker.fresh_var();
    // Unify with Int, so it's resolved
    let span = crate::source::Span::BUILTIN;
    checker.unify(&tv, &Type::Int, span);
    assert!(!checker.is_bare_type_var(&tv));
}

#[test]
fn test_unresolved_var_is_bare() {
    let mut checker = TypeChecker::new();
    let tv = checker.fresh_var();
    assert!(checker.is_bare_type_var(&tv));
}

// ── Unresolved type variable detection ──────────────────────────

#[test]
fn test_used_variable_no_false_positive() {
    // A let binding whose value type is initially unknown but resolved by usage
    assert_no_errors(
        r#"
import list
fn main() {
  let x = []
  let y = list.append(x, 1)
  y
}
        "#,
    );
}

#[test]
fn test_annotated_let_not_flagged() {
    assert_no_errors(
        r#"
fn main() {
  let x: Int = 42
  x
}
        "#,
    );
}

// ── stmt_references_name / expr_references_name ─────────────────

#[test]
fn test_stmt_references_name_in_let() {
    // If a later statement uses the variable, the unresolved check skips it
    assert_no_errors(
        r#"
fn identity(x) { x }
fn main() {
  let x = identity(42)
  x + 1
}
        "#,
    );
}

#[test]
fn test_expr_references_in_nested_block() {
    assert_no_errors(
        r#"
fn main() {
  let f = { x -> x + 1 }
  f(42)
}
        "#,
    );
}

// ── resolve_all_types applies substitution ──────────────────────

#[test]
fn test_type_annotations_resolved_after_check() {
    let input = r#"
fn double(x) { x * 2 }
fn main() { double(5) }
        "#;
    let (program, errors) = crate::session::testing::analyze_str(input);
    assert!(
        errors
            .iter()
            .filter(|e| e.severity == Severity::Error)
            .count()
            == 0
    );
    // After checking, the function body should have resolved types (no bare Vars)
    for decl in &program.decls {
        if let Decl::Fn(f) = decl
            && f.name == crate::intern::intern("double")
            && let Some(ty) = &f.body.ty
        {
            assert!(
                !matches!(ty, Type::Var(_)),
                "body type should be resolved, got {ty}"
            );
        }
    }
}

// ── Edge case: function call return type resolved by context ────

#[test]
fn test_generic_call_resolved_by_context() {
    assert_no_errors(
        r#"
fn id(x) { x }
fn main() {
  let n = id(42)
  n + 1
}
        "#,
    );
}
