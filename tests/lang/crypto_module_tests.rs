//! End-to-end tests for the `crypto` builtin module.
//!
//! Known-answer vectors pin the exact digests / HMAC tags so a future
//! refactor of the RustCrypto backend (or a switch to a pure-Rust
//! reimplementation) cannot silently change output. The CSPRNG tests
//! are probabilistic on the "distinctness" arm but deterministic on
//! the bounds-checking arms.
//!
//! The known-answer, CSPRNG and typechecker tests live in the golden cases
//! tests/golden/lang/stdlib/crypto_module__*.silt. The two tests left here
//! cross-check the crate's registration tables and need its internal API.

use silt::types::Severity;

fn type_errors(input: &str) -> Vec<String> {
    let tokens = silt::lexer::Lexer::new(input)
        .tokenize()
        .expect("lex error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    let errors = silt::typechecker::check(&mut program);
    errors
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

// ── Docs / registration cross-check ───────────────────────────────────

/// Walks the crypto doc page and asserts every function mentioned in
/// the summary table has a matching registration in the typechecker.
/// This mirrors the spirit of `docs_round26_tests::every_register_builtins_has_a_per_module_doc`
/// but runs in the other direction: docs → registration.
#[test]
fn test_documented_crypto_functions_match_registration() {
    // Round 62 phase-2 inlined the crypto module markdown into
    // `super::docs::CRYPTO_MD`, attached as a module-level overview
    // to every crypto.* binding via `attach_module_overview`. Every
    // function registered must have a non-empty doc body.
    let docs = silt::typechecker::builtin_docs();
    let expected = silt::module::builtin_module_functions("crypto");
    assert!(
        !expected.is_empty(),
        "module::builtin_module_functions(\"crypto\") returned empty — registration is missing"
    );

    for name in &expected {
        let qualified = format!("crypto.{}", name);
        let body = docs.get(&qualified).cloned().unwrap_or_default();
        assert!(
            !body.trim().is_empty(),
            "crypto.{name} has no registered builtin doc — verify \
             `attach_module_overview(env, super::docs::CRYPTO_MD, \
             \"crypto\")` fires from \
             src/typechecker/builtins/crypto.rs"
        );
        // The crypto module overview should mention the function name
        // (it appears in the Summary table at minimum).
        let bare = format!("`{}`", name);
        assert!(
            body.contains(&bare) || body.contains(&qualified),
            "the crypto module doc (now inlined as `super::docs::CRYPTO_MD`) \
             does not mention the function `{name}`. Add a row for it to \
             the Summary table."
        );
    }
}

/// Every function registered for the crypto module must also have a
/// type signature in the type environment. This catches a drift where
/// module.rs exposes a function name but the typechecker does not
/// know the signature.
#[test]
fn test_every_crypto_function_has_a_type_signature() {
    let expected = silt::module::builtin_module_functions("crypto");
    for name in &expected {
        let input = format!(
            r#"
import crypto
fn main() {{
  let _ = crypto.{name}
}}
"#
        );
        let errs = type_errors(&input);
        // We accept errors of the form "crypto.X is not callable" /
        // arity / etc., but we must NOT see the hard "unknown
        // identifier: crypto.X" form that would indicate missing
        // registration. Easiest signal: look for "unknown" in the
        // error list; the typechecker uses `Unknown identifier` or
        // `unknown function` wording for missing names.
        for e in &errs {
            let lower = e.to_ascii_lowercase();
            assert!(
                !(lower.contains("unknown") && lower.contains(name as &str)),
                "crypto.{name} appears to be unregistered in the typechecker: {e}"
            );
        }
    }
}
