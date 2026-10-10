//! Entry points: the `main` a program starts from and the test functions
//! `silt test` calls, judged by their inferred types.

use std::collections::HashMap;

use crate::ast::{Decl, ExprKind, FnDecl, ImportTarget, PatternKind, Program};
use crate::diagnostic::{Code, Diagnostic};
use crate::intern::{Symbol, intern, resolve};
use crate::source::{FileId, Span};
use crate::types::Type;

/// The name of the function a program starts from.
pub const ENTRY_POINT: &str = "main";

/// The builtin module that holds the assertions.
const TEST_MODULE: &str = "test";

/// How `silt test` treats a top-level function, going by its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestKind {
    /// `test_*`: the function is called; it fails when it raises an error
    /// or returns `Err(..)`.
    Run,
    /// `skip_test_*`: the function is reported as skipped, not called.
    Skip,
}

/// Classify a top-level function name. `None`: not a test.
pub fn test_kind(name: &str) -> Option<TestKind> {
    if name.starts_with("skip_test_") {
        Some(TestKind::Skip)
    } else if name.starts_with("test_") {
        Some(TestKind::Run)
    } else {
        None
    }
}

/// The test functions that `program` declares, in source order.
pub fn test_functions(program: &Program) -> Vec<(String, TestKind)> {
    selected_tests(program, None)
        .map(|(_, name, kind)| (name, kind))
        .collect()
}

/// The test functions of `program` whose names contain `filter` (all of
/// them without one), in source order. This is the one selection: the
/// tests compiled for [`super::Entry::Tests`] are these, and `silt test
/// --filter` leaves a file alone when it has none.
pub fn selected_tests<'a>(
    program: &'a Program,
    filter: Option<&'a str>,
) -> impl Iterator<Item = (&'a FnDecl, String, TestKind)> + 'a {
    program.decls.iter().filter_map(move |decl| {
        let Decl::Fn(f) = decl else {
            return None;
        };
        let name = resolve(f.name);
        let kind = test_kind(&name)?;
        if filter.is_some_and(|pattern| !name.contains(pattern)) {
            return None;
        }
        Some((f, name, kind))
    })
}

/// Is `program` a test file: does it declare a test function, or import
/// the `test` module? Such a file has no `main` on purpose; it is run
/// with `silt test`.
pub fn looks_like_test_file(program: &Program) -> bool {
    !test_functions(program).is_empty()
        || program.decls.iter().any(|decl| match decl {
            Decl::Import(
                ImportTarget::Module(module)
                | ImportTarget::Items(module, _)
                | ImportTarget::Alias(module, ..),
                _,
            ) => resolve(*module) == TEST_MODULE,
            _ => false,
        })
}

/// Is `program` a library module: does it declare a `pub fn`? Such a file
/// has no `main` on purpose; it is imported, not run.
pub fn looks_like_library_module(program: &Program) -> bool {
    program
        .decls
        .iter()
        .any(|decl| matches!(decl, Decl::Fn(f) if f.is_pub))
}

/// A test function selected to run or to be reported as skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestFn {
    pub name: String,
    pub kind: TestKind,
    /// The span of the function's name.
    pub span: Span,
    /// The function's global slot, which the compiler gives it.
    pub slot: u16,
}

/// The diagnostic for an entry file `path` (as the user named it) that
/// binds no `main`. It is about the whole file, so it points at its
/// start. A test file gets a pointer to `silt test` instead of the advice
/// to add a `main`.
pub(super) fn missing_main(program: &Program, file: FileId, path: &str) -> Diagnostic {
    let d = Diagnostic::error(
        Code::MissingMain,
        Span::point(file, 0),
        "program has no main() function",
    );
    if looks_like_test_file(program) {
        d.with_note(format!(
            "This looks like a test file — run it with 'silt test {path}' instead."
        ))
    } else {
        d.with_note("add one as the entry point")
    }
}

/// Check that `main`, of the inferred type `ty`, can be called with no
/// arguments: its type must be `() -> a`. A type the checker could not
/// infer is let through: the error that made it so is reported already.
pub(super) fn check_main(program: &Program, ty: &Type) -> Option<Diagnostic> {
    let main = intern(ENTRY_POINT);
    let binder = binder_span(program, main);
    match ty {
        Type::Fun(params, _) if params.is_empty() => None,
        Type::Fun(params, _) => {
            let count = params.len();
            let (these, them) = if count == 1 {
                ("1 parameter".to_string(), "it")
            } else {
                (format!("{count} parameters"), "them")
            };
            let (verb, span) = match declared_params(program, main) {
                Some(span) => ("declares", span),
                None => ("takes", binder),
            };
            Some(
                Diagnostic::error(
                    Code::MainSignature,
                    span,
                    format!("the entry point 'main' must take no parameters, but it {verb} {these}"),
                )
                .with_help(format!(
                    "remove {them}; the command-line arguments are available from io.args()"
                )),
            )
        }
        Type::Var(_) | Type::Error | Type::Never => None,
        other => Some(
            Diagnostic::error(
                Code::MainSignature,
                binder,
                format!("the entry point 'main' must be a function that takes no parameters, but it is a value of type {other}"),
            )
            .with_help("write `fn main() { ... }`"),
        ),
    }
}

/// The test functions of `program` whose names `filter` selects, in
/// source order, with an error for each whose inferred type is not
/// `() -> a`: such a function cannot be called.
pub(super) fn select_tests(
    program: &Program,
    top_level: &HashMap<Symbol, Type>,
    filter: Option<&str>,
) -> (Vec<TestFn>, Vec<Diagnostic>) {
    let mut tests = Vec::new();
    let mut errors = Vec::new();
    for (f, name, kind) in selected_tests(program, filter) {
        if let Some(Type::Fun(params, _)) = top_level.get(&f.name)
            && !params.is_empty()
        {
            let span = f.params.first().map_or(f.name_span, |p| p.pattern.span);
            errors.push(
                Diagnostic::error(
                    Code::TestSignature,
                    span,
                    format!(
                        "the test function '{name}' must take no parameters, but it declares {}",
                        if params.len() == 1 {
                            "1 parameter".to_string()
                        } else {
                            format!("{} parameters", params.len())
                        }
                    ),
                )
                .with_help("a test is called with no arguments"),
            );
            continue;
        }
        tests.push(TestFn {
            name,
            kind,
            span: f.name_span,
            slot: 0,
        });
    }
    (tests, errors)
}

/// The span of what binds the top-level name `name` in `program`: a
/// function's name, a `let`'s pattern, an imported item.
fn binder_span(program: &Program, name: Symbol) -> Span {
    program
        .decls
        .iter()
        .find_map(|decl| match decl {
            Decl::Fn(f) if f.name == name => Some(f.name_span),
            Decl::Let { pattern, .. } => match &pattern.kind {
                PatternKind::Ident(n) if *n == name => Some(pattern.span),
                _ => None,
            },
            Decl::Import(ImportTarget::Items(_, items), _) => items
                .iter()
                .find(|(item, _)| *item == name)
                .map(|(_, span)| *span),
            _ => None,
        })
        .unwrap_or(Span::BUILTIN)
}

/// The span of the first parameter `name` declares, when `program`
/// defines it as a function or a `let` of a lambda that has parameters.
fn declared_params(program: &Program, name: Symbol) -> Option<Span> {
    program.decls.iter().find_map(|decl| {
        let params = match decl {
            Decl::Fn(f) if f.name == name => &f.params,
            Decl::Let { pattern, value, .. } => match (&pattern.kind, &value.kind) {
                (PatternKind::Ident(n), ExprKind::Lambda { params, .. }) if *n == name => params,
                _ => return None,
            },
            _ => return None,
        };
        params.first().map(|p| p.pattern.span)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn parse(source: &str) -> Program {
        let tokens = Lexer::new(FileId::default(), source)
            .tokenize()
            .checked()
            .expect("the text must lex");
        let (program, errors) = Parser::new(tokens, source).parse_program_recovering();
        assert!(errors.is_empty(), "the text must parse");
        program
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
}
