//! Tests for the two final generics polish items:
//!   Gap 2 — trait-level where bounds on trait params
//!   Gap 3 — trait params in supertrait references

use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::types::Severity;

fn type_errors(input: &str) -> Vec<String> {
    let tokens = Lexer::new(input).tokenize().expect("lexer");
    let mut program = Parser::new(tokens).parse_program().expect("parse");
    typechecker::check(&mut program)
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

fn assert_ok(src: &str) {
    let errs = type_errors(src);
    assert!(errs.is_empty(), "expected no errors, got:\n{}", errs.join("\n"));
}

fn assert_rejected(src: &str, needle: &str) {
    let errs = type_errors(src);
    assert!(
        errs.iter().any(|m| m.contains(needle)),
        "expected error containing {needle:?}; got:\n{}",
        errs.join("\n")
    );
}

// ── Gap 2: trait-level where bounds ─────────────────────────────────

#[test]
fn trait_param_bound_parses() {
    assert_ok(
        r#"
        trait HashTable(k) where k: Hash {
            fn noop(self)
        }
        "#,
    );
}

#[test]
fn trait_param_bound_multi_trait_bound() {
    assert_ok(
        r#"
        trait HashTable(k) where k: Hash + Equal {
            fn noop(self)
        }
        "#,
    );
}

#[test]
fn trait_param_bound_satisfied_at_impl_time() {
    // String auto-derives Hash + Equal so this impl typechecks.
    assert_ok(
        r#"
        type Store { data: Int }

        trait HashTable(k) where k: Hash + Equal {
            fn noop(self)
        }

        trait HashTable(String) for Store {
            fn noop(self) { () }
        }
        "#,
    );
}

#[test]
fn trait_param_bound_violated_at_impl_time() {
    // A hand-rolled trait never implemented by Int — passing Int as the
    // bound-carrying param should reject at impl time.
    assert_rejected(
        r#"
        trait CustomBound {
            fn custom(self)
        }

        type Target { x: Int }

        trait Wrap(a) where a: CustomBound {
            fn wrap(self)
        }

        trait Wrap(Int) for Target {
            fn wrap(self) { () }
        }
        "#,
        "does not implement trait 'CustomBound'",
    );
}

// ── Gap 3: trait params in supertrait references ────────────────────

#[test]
fn supertrait_with_args_parses() {
    assert_ok(
        r#"
        trait Parent(a) {
            fn parent_fn(self) -> a
        }

        trait Child(a): Parent(a) {
            fn child_fn(self) -> a
        }
        "#,
    );
}

#[test]
fn supertrait_arg_flows_to_super_method_through_where_clause() {
    // The payoff: a generic fn constrained on `b: Child(a)` can call
    // Parent's method on b, and the `a` from Child's args flows into
    // Parent's method return type correctly.
    assert_ok(
        r#"
        trait Parent(a) {
            fn parent_fn(self) -> a
        }

        trait Child(a): Parent(a) {
            fn child_fn(self) -> a
        }

        type Holder { v: Int }

        trait Parent(Int) for Holder {
            fn parent_fn(self) -> Int { self.v }
        }

        trait Child(Int) for Holder {
            fn child_fn(self) -> Int { self.v + 1 }
        }

        fn call_parent(x: b, type a) -> a where b: Child(a) {
            x.parent_fn()
        }

        fn use_it() {
            let h = Holder { v: 5 }
            let r: Int = call_parent(h, Int)
        }
        "#,
    );
}

#[test]
fn supertrait_concrete_arg_in_reference() {
    // `trait Sub: Super(Int)` — the supertrait reference takes a
    // concrete arg directly. Allowed syntactically.
    assert_ok(
        r#"
        trait Parent(a) {
            fn p(self) -> a
        }

        trait Child: Parent(Int) {
            fn c(self) -> Int
        }
        "#,
    );
}
