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

fn type_errors(input: &str) -> Vec<String> {
    silt::session::testing::check_str(input)
        .into_iter()
        .filter(|d| d.is_error())
        .map(|d| d.message)
        .collect()
}

// ── Docs / registration cross-check ───────────────────────────────────

/// Every row of `crypto` has a doc an editor shows, and that doc (the
/// module's page, `docs/stdlib/crypto.md`) mentions the function.
#[test]
fn test_documented_crypto_functions_match_registration() {
    let docs = silt::builtins::registry::docs::builtin_docs();
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
            "crypto.{name} has no builtin doc \
             (src/builtins/registry/docs.rs::builtin_docs)"
        );
        // The crypto module overview should mention the function name
        // (it appears in the Summary table at minimum).
        let bare = format!("`{}`", name);
        assert!(
            body.contains(&bare) || body.contains(&qualified),
            "the crypto page (docs/stdlib/crypto.md) \
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
