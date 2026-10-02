//! End-to-end tests for the `uuid` builtin module.
//!
//! Mirrors the shape of `tests/lang/crypto_module_tests.rs`: each test drives
//! the VM via the same lex → parse → typecheck → compile → run
//! pipeline, then asserts on the returned `Value`. Tests cover the
//! full advertised API surface (`v4`, `v7`, `parse`, `nil`,
//! `is_valid`) plus a couple of typechecker / registration cross-checks
//! so a future drift between the runtime, typechecker, and the
//! `src/module.rs` function list will fail loudly instead of silently.
//!
//! The API tests are golden cases in tests/golden/lang/stdlib/uuid_module__*.silt;
//! the registration cross-check below needs the crate's module table.

use silt::diagnostic::Severity;

fn type_errors(input: &str) -> Vec<String> {
    let tokens = silt::lexer::Lexer::new(silt::source::FileId::default(), input)
        .tokenize()
        .expect("lex error");
    let mut program = silt::parser::Parser::new(tokens, input)
        .parse_program()
        .expect("parse error");
    let errors = silt::typechecker::check(&mut program);
    errors
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

// ── Typechecker + registration cross-checks ────────────────────────────

/// Every function registered in `src/module.rs::builtin_module_functions("uuid")`
/// must have a type signature so that `uuid.<fn>` resolves in the
/// typechecker. Catches drift where a new function is added to
/// module.rs but the typechecker/runtime never learn about it.
#[test]
fn test_every_uuid_function_has_a_type_signature() {
    let expected = silt::module::builtin_module_functions("uuid");
    assert!(
        !expected.is_empty(),
        "module::builtin_module_functions(\"uuid\") returned empty"
    );
    for name in &expected {
        let input = format!(
            r#"
import uuid
fn main() {{
  let _ = uuid.{name}
}}
"#
        );
        let errs = type_errors(&input);
        for e in &errs {
            let lower = e.to_ascii_lowercase();
            assert!(
                !(lower.contains("unknown") && lower.contains(name as &str)),
                "uuid.{name} appears to be unregistered in the typechecker: {e}"
            );
        }
    }
}
