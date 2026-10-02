//! Questions about a source file that decide how the CLI treats it: "does
//! it define `main`?", "is it a test file?", "which functions are tests?".
//!
//! Every answer is read from the parsed declarations, the same ones the
//! typechecker and the compiler work on, so `silt check`, `silt run` and
//! `silt test` cannot disagree with the parser or with each other. Nothing
//! here looks at source text: a text scan cannot tell a declaration from
//! the same words inside a comment or a string, and it breaks on spacing
//! the parser accepts.

use silt::ast::{Decl, ExprKind, ImportTarget, PatternKind, Program};
use silt::errors::SourceError;
use silt::intern::resolve;
use silt::source::SourceMap;

/// Name of the global that a compiled program calls as its entry point
/// (see `Compiler::compile_program`).
const ENTRY_POINT: &str = "main";

/// The builtin module that holds the assertions.
const TEST_MODULE: &str = "test";

/// How `silt test` treats a top-level function, going by its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TestKind {
    /// `test_*`: the function is called; it fails when it raises an error
    /// or returns `Err(..)`.
    Run,
    /// `skip_test_*`: the function is reported as skipped, not called.
    Skip,
}

/// Classify a top-level function name. `None`: not a test.
pub(crate) fn test_kind(name: &str) -> Option<TestKind> {
    if name.starts_with("skip_test_") {
        Some(TestKind::Skip)
    } else if name.starts_with("test_") {
        Some(TestKind::Run)
    } else {
        None
    }
}

/// The test functions that `program` declares, in source order.
pub(crate) fn test_functions(program: &Program) -> Vec<(String, TestKind)> {
    program
        .decls
        .iter()
        .filter_map(|decl| match decl {
            Decl::Fn(f) => {
                let name = resolve(f.name);
                let kind = test_kind(&name)?;
                Some((name, kind))
            }
            _ => None,
        })
        .collect()
}

/// Does `program` bind the top-level name `main`?
///
/// The entry point is looked up by name when the program starts, so every
/// declaration that puts `main` into the global scope counts: `fn main`,
/// `let main = ...` and `import m.{ main }`.
pub(crate) fn program_has_main(program: &Program) -> bool {
    program.decls.iter().any(|decl| match decl {
        Decl::Fn(f) => resolve(f.name) == ENTRY_POINT,
        Decl::Let { pattern, .. } => {
            matches!(&pattern.kind, PatternKind::Ident(name) if resolve(*name) == ENTRY_POINT)
        }
        Decl::Import(ImportTarget::Items(_, items), _) => {
            items.iter().any(|(item, _)| resolve(*item) == ENTRY_POINT)
        }
        _ => false,
    })
}

/// Is `program` a test file: does it declare a test function, or import
/// the `test` module? Such a file has no `main` on purpose; it is run
/// with `silt test`.
pub(crate) fn looks_like_test_file(program: &Program) -> bool {
    !test_functions(program).is_empty()
        || program.decls.iter().any(|decl| match decl {
            Decl::Import(
                ImportTarget::Module(module)
                | ImportTarget::Items(module, _)
                | ImportTarget::Alias(module, _),
                _,
            ) => resolve(*module) == TEST_MODULE,
            _ => false,
        })
}

/// Is `program` a library module: does it declare a `pub fn`? Such a file
/// has no `main` on purpose; it is imported, not run.
pub(crate) fn looks_like_library_module(program: &Program) -> bool {
    program
        .decls
        .iter()
        .any(|decl| matches!(decl, Decl::Fn(f) if f.is_pub))
}

/// The diagnostic for a `main` that declares parameters, if `program` has
/// one. The entry point is called without arguments, so such a program
/// can never start.
///
/// Library modules and test files are not entry points: they are
/// imported or run by `silt test`, never started through their `main`.
/// They are exempt here as they are from the missing-`main` error.
pub(crate) fn main_signature_error(
    program: &Program,
    sources: &SourceMap,
    path: &str,
) -> Option<SourceError> {
    if looks_like_library_module(program) || looks_like_test_file(program) {
        return None;
    }
    let (count, span) = program.decls.iter().find_map(|decl| {
        let (name, params) = match decl {
            Decl::Fn(f) => (f.name, &f.params),
            Decl::Let { pattern, value, .. } => match (&pattern.kind, &value.kind) {
                (PatternKind::Ident(name), ExprKind::Lambda { params, .. }) => (*name, params),
                _ => return None,
            },
            _ => return None,
        };
        if resolve(name) != ENTRY_POINT {
            return None;
        }
        let first = params.first()?;
        Some((params.len(), first.pattern.span))
    })?;
    let (these, them) = if count == 1 {
        ("1 parameter".to_string(), "it")
    } else {
        (format!("{count} parameters"), "them")
    };
    Some(SourceError::compile_error_at(
        format!(
            "the entry point 'main' must take no parameters, but it declares {these}\n\
             help: remove {them}; the command-line arguments are available from io.args()"
        ),
        Some(span),
        sources,
        path,
    ))
}

/// The diagnostic for a program that binds no `main`. With
/// `suggest_silt_test`, a test file gets a pointer to `silt test` instead
/// of the advice to add a `main`.
pub(crate) fn missing_main_error(
    program: &Program,
    sources: &SourceMap,
    path: &str,
    suggest_silt_test: bool,
) -> SourceError {
    let message = if suggest_silt_test && looks_like_test_file(program) {
        format!(
            "program has no main() function\nThis looks like a test file — run it with 'silt test {path}' instead."
        )
    } else {
        "program has no main() function\nadd one as the entry point".to_string()
    };
    // There is no source location for "the file has no main": without a
    // span the renderer prints the header and the note, and no locator.
    SourceError::compile_error_at(message, None, sources, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use silt::lexer::Lexer;
    use silt::parser::Parser;
    use silt::source::SourceName;

    /// A map holding `source` as its only file.
    fn sources(source: &str) -> SourceMap {
        let mut map = SourceMap::new();
        map.add(SourceName::Path("main.silt".into()), source.into());
        map
    }

    fn parse(source: &str) -> Program {
        let tokens = Lexer::new(silt::source::FileId::default(), source)
            .tokenize()
            .expect("the text must lex");
        let (program, errors) = Parser::new(tokens, source).parse_program_recovering();
        assert!(errors.is_empty(), "the text must parse");
        program
    }

    #[test]
    fn main_is_found_whatever_the_spacing() {
        assert!(program_has_main(&parse("fn main() { 1 }")));
        assert!(program_has_main(&parse("fn  main() { 1 }")));
        assert!(program_has_main(&parse("fn\nmain\n() { 1 }")));
        assert!(program_has_main(&parse("pub  fn   main() { 1 }")));
    }

    #[test]
    fn main_in_a_comment_or_a_string_is_not_a_main() {
        assert!(!program_has_main(&parse(
            "{-\nfn main() {}\n-}\nfn helper() { 1 }"
        )));
        assert!(!program_has_main(&parse(
            "-- fn main() {}\nfn helper() { 1 }"
        )));
        assert!(!program_has_main(&parse(
            "let s = \"\"\"\nfn main() {}\n\"\"\"\nfn helper() { s }"
        )));
        assert!(!program_has_main(&parse("fn main_helper() { 1 }")));
        assert!(!program_has_main(&parse("")));
    }

    #[test]
    fn main_bound_by_let_or_import_is_a_main() {
        assert!(program_has_main(&parse("let main = { -> 1 }")));
        assert!(program_has_main(&parse("import helper.{ main }")));
        assert!(!program_has_main(&parse("import main")));
    }

    #[test]
    fn test_functions_are_found_whatever_the_spacing() {
        let program = parse(
            "fn  test_spaced() { 1 }\nfn skip_test_later() { 2 }\nfn helper() { 3 }\npub fn test_pub() { 4 }",
        );
        assert_eq!(
            test_functions(&program),
            vec![
                ("test_spaced".to_string(), TestKind::Run),
                ("skip_test_later".to_string(), TestKind::Skip),
                ("test_pub".to_string(), TestKind::Run),
            ]
        );
    }

    #[test]
    fn test_file_and_library_module() {
        assert!(looks_like_test_file(&parse("fn test_a() { 1 }")));
        assert!(looks_like_test_file(&parse(
            "import test\nfn helper() { 1 }"
        )));
        assert!(!looks_like_test_file(&parse(
            "-- fn test_a() { 1 }\nfn helper() { 1 }"
        )));
        assert!(looks_like_library_module(&parse(
            "pub fn double(x) { x * 2 }"
        )));
        assert!(!looks_like_library_module(&parse(
            "-- pub fn double(x) { x * 2 }\nfn helper() { 1 }"
        )));
    }

    #[test]
    fn main_with_parameters_is_an_error() {
        let source = "fn main(x: Int) { x }";
        let error = main_signature_error(&parse(source), &sources(source), "main.silt")
            .expect("a main with a parameter is an error");
        assert!(
            error.message.contains("declares 1 parameter\n"),
            "{}",
            error.message
        );
        assert_eq!((error.line, error.col), (1, 9));

        let source = "let main = { a, b -> a + b }";
        let error = main_signature_error(&parse(source), &sources(source), "main.silt")
            .expect("a closure main with parameters is an error");
        assert!(
            error.message.contains("declares 2 parameters\n"),
            "{}",
            error.message
        );

        for source in ["fn main() { 1 }", "fn helper(x) { x }", "let main = 3", ""] {
            assert!(main_signature_error(&parse(source), &sources(source), "main.silt").is_none());
        }

        // Library modules and test files are not entry points.
        for source in [
            "pub fn greet() { 1 }\npub fn main(x: Int) { x }",
            "fn main(args: List(String)) { () }\nfn test_a() { 1 }",
            "import test\nfn main(x: Int) { x }",
        ] {
            assert!(main_signature_error(&parse(source), &sources(source), "lib.silt").is_none());
        }
    }
}
