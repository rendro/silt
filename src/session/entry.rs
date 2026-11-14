//! Entry points: the `main` a program starts from and the test functions
//! `silt test` calls, judged by their inferred types.

use std::collections::HashMap;

use crate::ast::{Decl, ExprKind, FnDecl, PatternKind, Program, Stmt};
use crate::diagnostic::{Code, Diagnostic};
use crate::intern::{Symbol, intern, resolve};
use crate::program::{TestFn, TestKind};
use crate::source::{FileId, Span};
use crate::types::Type;

/// The name of the function a program starts from.
pub const ENTRY_POINT: &str = "main";

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

/// Whether the file at `path` is a test file: its name ends with
/// `_test.silt`, as the files `silt test` looks for do.
pub(super) fn is_test_file(path: &std::path::Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().ends_with("_test.silt"))
}

/// The diagnostic for an entry file `path` (as the user named it) that
/// binds no `main`. It is about the whole file, so it points at its
/// start. A test file (`is_test_file`), which has no `main` on purpose,
/// gets a pointer to `silt test` instead of the advice to add a `main`.
pub(super) fn missing_main(file: FileId, path: &str) -> Diagnostic {
    let d = Diagnostic::error(
        Code::MissingMain,
        Span::point(file, 0),
        "program has no main() function",
    );
    if is_test_file(std::path::Path::new(path)) {
        d.with_note(format!(
            "This looks like a test file — run it with 'silt test {path}' instead."
        ))
    } else {
        d.with_note("add one as the entry point")
    }
}

/// Check that `main`, of the inferred type `ty`, can be called with no
/// arguments and returns nothing: its type must be `() -> ()` or
/// `() -> Result((), e)`. A type the checker could not infer is let
/// through: the error that made it so is reported already.
pub(super) fn check_main(program: &Program, ty: &Type) -> Option<Diagnostic> {
    let main = intern(ENTRY_POINT);
    let binder = binder_span(program, main);
    match ty {
        Type::Fun(params, ret) if params.is_empty() => (!returns_nothing(ret)).then(|| {
            let span = fn_decl(program, main).map_or(binder, returned_span);
            value_returned(Code::MainSignature, span, "the entry point 'main'", EntryFn::Main, ret)
        }),
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
/// source order, with an error for each whose inferred type is neither
/// `() -> ()` nor `() -> Result((), e)`: such a function cannot be
/// called, or returns a value that goes nowhere.
pub(super) fn select_tests(
    program: &Program,
    top_level: &HashMap<Symbol, Type>,
    filter: Option<&str>,
) -> (Vec<TestFn>, Vec<Diagnostic>) {
    let mut tests = Vec::new();
    let mut errors = Vec::new();
    for (f, name, kind) in selected_tests(program, filter) {
        if let Some(Type::Fun(params, ret)) = top_level.get(&f.name)
            && params.is_empty()
            && !returns_nothing(ret)
        {
            errors.push(value_returned(
                Code::TestSignature,
                returned_span(f),
                &format!("the test function '{name}'"),
                EntryFn::Test,
                ret,
            ));
            continue;
        }
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

/// Whether `ret`, what an entry point returns, is what one may return:
/// `()` or `Result((), e)`. A type the checker could not infer is let
/// through, as is `Never`: a `main` that ends the program itself.
fn returns_nothing(ret: &Type) -> bool {
    let nothing = |t: &Type| matches!(t, Type::Unit | Type::Never | Type::Var(_) | Type::Error);
    match ret {
        Type::Generic(name, args) if name.is_builtin("Result") => args.first().is_none_or(nothing),
        other => nothing(other),
    }
}

/// Which entry point a value is returned from.
#[derive(Clone, Copy)]
enum EntryFn {
    Main,
    Test,
}

/// The error of an entry point that returns `ret`, a value: a `main`,
/// or a test function, named by `who`, with the way out for what it
/// returns.
fn value_returned(code: Code, span: Span, who: &str, entry: EntryFn, ret: &Type) -> Diagnostic {
    // A type of the program's that is named like the builtin `Result`.
    let shown = ret.to_string();
    let builtin_result = matches!(ret, Type::Generic(name, _) if name.is_builtin("Result"));
    if !builtin_result && shown.split('(').next() == Some("Result") {
        return Diagnostic::error(
            code,
            span,
            format!(
                "{who} must return () or Result((), e), but it returns {ret}, a type of this \
                 program that is not the builtin Result"
            ),
        )
        .with_help("give the type another name");
    }
    let help = match (ret, entry) {
        (Type::Generic(name, _), EntryFn::Main) if name.is_builtin("Option") => {
            "to end the program with a failure when there is none, make main return a \
             Result and write `option.to_result(x, \"why\")?`"
        }
        (Type::Generic(name, _), EntryFn::Test) if name.is_builtin("Option") => {
            "to fail the test when there is none, make the test return a Result and write \
             `option.to_result(x, \"why\")?`"
        }
        (Type::Generic(name, _), EntryFn::Main) if name.is_builtin("Result") => {
            "end main with `Ok(())` once the value is handed on; the Ok of main carries nothing"
        }
        (Type::Generic(name, _), EntryFn::Test) if name.is_builtin("Result") => {
            "end the test with `Ok(())` once the value is checked; the Ok of a test carries \
             nothing"
        }
        (_, EntryFn::Main) => {
            "hand the value on (println, a file, a host function), or drop it with `let _ = ...`"
        }
        (Type::Bool, EntryFn::Test) => {
            "check the value with an assertion (`test.assert(x)`), or drop it with `let _ = ...`"
        }
        (_, EntryFn::Test) => {
            "check the value with an assertion (`test.assert_eq(x, expected)`), or drop it \
             with `let _ = ...`"
        }
    };
    let error = Diagnostic::error(
        code,
        span,
        format!("{who} must return () or Result((), e), but it returns {ret}"),
    )
    .with_help(help);
    match entry {
        EntryFn::Main => error,
        // (Every `test_*` and `skip_test_*` function of the file is a test.)
        EntryFn::Test => error.with_help(
            "a function that is not a test needs a name that starts with neither `test_` nor \
             `skip_test_`",
        ),
    }
}

/// The function `name` declares in `program`, if it is a `fn`.
fn fn_decl(program: &Program, name: Symbol) -> Option<&FnDecl> {
    program.decls.iter().find_map(|decl| match decl {
        Decl::Fn(f) if f.name == name => Some(f),
        _ => None,
    })
}

/// Where the value that `f` returns is written: its return type, else
/// the last expression of its body, else its name. A last expression
/// that returns nothing (`panic(..)`, a `return`) is not where the
/// value is: the name is then.
fn returned_span(f: &FnDecl) -> Span {
    if let Some(written) = &f.return_type {
        return written.span;
    }
    let last = match &f.body.kind {
        ExprKind::Block(stmts) => match stmts.last() {
            Some(Stmt::Expr(last)) => last,
            _ => return f.name_span,
        },
        _ => &f.body,
    };
    match last.ty {
        Some(Type::Never) => f.name_span,
        _ => last.span,
    }
}

/// Where `main` is bound: the name of its `fn`, its name in the pattern
/// of its `let`, its name in an import. [`Span::BUILTIN`] when nothing
/// binds it.
pub(super) fn main_span(program: &Program) -> Span {
    binder_span(program, intern(ENTRY_POINT))
}

/// The span of what binds the top-level name `name` in `program`: a
/// function's name, the name in a `let`'s pattern, an imported item.
fn binder_span(program: &Program, name: Symbol) -> Span {
    (program.decls.iter())
        .flat_map(crate::parser::top_level_binders)
        .find_map(|(bound, span, _)| (bound == name).then_some(span))
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
            "fn  test_spaced() { 1 }\nfn skip_test_later() { let _ = 2 }\nfn helper() { 3 }\npub fn test_pub() { let _ = 4 }",
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
    fn test_file() {
        assert!(is_test_file(std::path::Path::new("math_test.silt")));
        assert!(is_test_file(std::path::Path::new("src/a/b_test.silt")));
        assert!(!is_test_file(std::path::Path::new("tests.silt")));
        assert!(!is_test_file(std::path::Path::new("helpers.silt")));
        assert!(!is_test_file(std::path::Path::new("-")));
    }
}
