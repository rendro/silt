//! Round 56 item 4: typechecker rejects `<builtin>.X` without an import.
//!
//! The rest of this file's cases are golden cases
//! `tests/golden/lang/imports/missing_import_recommends__*`. This one stays
//! in Rust because `silt check` / `silt run` ACCEPT `list.length` after
//! `import list as l` (exit 0, prints 3), while the in-process typechecker
//! reports "module 'list' is not imported"; the discrepancy is reported to
//! the integrator rather than captured as a golden.

use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::types::Severity;

fn type_errors(input: &str) -> Vec<String> {
    let tokens = Lexer::new(silt::source::FileId::default(), input)
        .tokenize()
        .expect("lexer");
    let mut program = Parser::new(tokens, input).parse_program().expect("parse");
    typechecker::check(&mut program)
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

#[test]
fn aliased_import_does_not_expose_original_name() {
    // `import list as l` renames; the original bare `list` name must
    // still be treated as un-imported. This ensures the opaque-until-
    // imported rule isn't silently bypassed by any import form.
    let errs = type_errors(
        r#"
        import list as l
        fn main() -> Int {
            list.length([1, 2, 3])
        }
        "#,
    );
    let joined = errs.join("\n");
    assert!(
        joined.contains("module 'list' is not imported"),
        "aliased import should NOT expose original name, got:\n{joined}"
    );
}
