//! Registration cross-checks for the `encoding` builtin module: every
//! function registered in `src/module.rs` has a builtin doc and a type
//! signature. The behavioural tests of the module are golden cases
//! (`tests/golden/lang/stdlib/encoding_module__*`).

fn type_errors(input: &str) -> Vec<String> {
    silt::session::testing::check_str(input)
        .into_iter()
        .filter(|d| d.is_error())
        .map(|d| d.message)
        .collect()
}

/// Mirror of `test_documented_crypto_functions_match_registration`:
/// every row of the `encoding` module has a doc an editor shows, cut
/// from `docs/stdlib/encoding.md`.
#[test]
fn test_documented_encoding_functions_match_registration() {
    let docs = silt::builtins::registry::docs::builtin_docs();
    let expected = silt::module::builtin_module_functions("encoding");
    assert!(
        !expected.is_empty(),
        "module::builtin_module_functions(\"encoding\") returned empty — registration is missing"
    );

    for name in &expected {
        let qualified = format!("encoding.{}", name);
        let body = docs.get(&qualified).cloned().unwrap_or_default();
        assert!(
            !body.trim().is_empty(),
            "encoding.{name} has no builtin doc \
             (src/builtins/registry/docs.rs::builtin_docs)"
        );
    }
}

/// Every function registered for the encoding module must also have a
/// type signature in the type environment.
#[test]
fn test_every_encoding_function_has_a_type_signature() {
    let expected = silt::module::builtin_module_functions("encoding");
    for name in &expected {
        let input = format!(
            r#"
import encoding
fn main() {{
  let _ = encoding.{name}
}}
"#
        );
        let errs = type_errors(&input);
        for e in &errs {
            let lower = e.to_ascii_lowercase();
            assert!(
                !(lower.contains("unknown") && lower.contains(name as &str)),
                "encoding.{name} appears to be unregistered in the typechecker: {e}"
            );
        }
    }
}
