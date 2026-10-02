//! Registration cross-checks for the `encoding` builtin module: every
//! function registered in `src/module.rs` has a builtin doc and a type
//! signature. The behavioural tests of the module are golden cases
//! (`tests/golden/lang/stdlib/encoding_module__*`).

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

/// Mirror of `test_documented_crypto_functions_match_registration`:
/// every function registered for the `encoding` module in
/// `src/module.rs` must have a non-empty inlined builtin doc (round
/// 62 phase-2 moved encoding's prose into
/// `super::docs::ENCODING_MD`).
#[test]
fn test_documented_encoding_functions_match_registration() {
    let docs = silt::typechecker::builtin_docs();
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
            "encoding.{name} has no registered builtin doc. Round 62 \
             phase-2 attaches `super::docs::ENCODING_MD` (and its \
             json sibling) to every encoding.* binding via \
             `attach_module_overview` + `attach_module_docs`. Verify \
             both calls fire from \
             `src/typechecker/builtins/encoding.rs`."
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
