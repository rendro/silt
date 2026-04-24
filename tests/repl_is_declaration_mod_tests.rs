//! Regression lock for round-60 LATENT L8: the REPL's `is_declaration`
//! helper (`src/repl.rs`) used to check for the prefixes `fn `, `let `,
//! `type `, `trait `, `import `, and `pub ` — but omitted `mod `. A user
//! typing `mod foo { ... }` at the REPL prompt hit the expression path,
//! which wraps the input in `fn main() { ... }` and re-runs the parser;
//! the parser then rejected `mod` inside an expression body with a
//! confusing diagnostic that was nowhere near the actual user intent.
//!
//! The fix adds `mod ` to the prefix list so module declarations are
//! routed through `eval_declaration` like every other declaration form.
//!
//! This integration test calls the now-public `is_declaration` helper
//! directly (a unit-test-style check from outside the crate) and pins
//! both the positive cases (every prefix the parser accepts) and the
//! negative cases (expressions and near-miss words like `module `).

#![cfg(feature = "repl")]

use silt::repl::is_declaration;

#[test]
fn is_declaration_recognizes_mod_prefix() {
    // The round-60 LATENT find: `mod` declarations must be routed
    // through eval_declaration, not wrapped in fn main() as an
    // expression.
    assert!(
        is_declaration("mod foo { }"),
        "`mod foo {{ }}` must be recognised as a declaration so the REPL \
         routes it through eval_declaration (not eval_expression wrapping \
         it in `fn main()`)"
    );
    assert!(is_declaration("mod bar { pub fn x() = 1 }"));
    // Leading whitespace must not defeat the prefix check — REPL users
    // commonly paste indented code.
    assert!(is_declaration("   mod indented {}"));
    assert!(is_declaration("\tmod tabbed {}"));
}

#[test]
fn is_declaration_still_recognizes_every_other_decl_prefix() {
    // Parity with the pre-existing unit tests in src/repl.rs, pinned
    // from the integration-test side so a silent removal of any prefix
    // fails here too.
    assert!(is_declaration("fn foo() {}"));
    assert!(is_declaration("let x = 1"));
    assert!(is_declaration("type Color { Red, Green }"));
    assert!(is_declaration("trait Show { fn show(self) -> String }"));
    assert!(is_declaration("import list"));
    assert!(is_declaration("pub fn foo() {}"));
    assert!(is_declaration("pub mod bar {}"));
}

#[test]
fn is_declaration_rejects_expressions_and_near_misses() {
    // Expression forms: must NOT be classified as declarations — the
    // REPL wraps them in a throwaway fn main for eval.
    assert!(!is_declaration("1 + 2"));
    assert!(!is_declaration("foo(42)"));
    assert!(!is_declaration("\"hello\""));

    // Near-miss: identifiers that happen to START with a declaration
    // keyword but are not followed by a space (`module`, `modular`,
    // `fnord`, `letter`) must NOT match. The prefix check requires a
    // trailing space, which guards against these false positives.
    assert!(!is_declaration("module_path"));
    assert!(!is_declaration("modulo(7, 3)"));
    assert!(!is_declaration("letter = 'a'"));
    assert!(!is_declaration("fnord"));
    assert!(!is_declaration("typed_value"));
}
