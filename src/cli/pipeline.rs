//! Shared compilation pipeline used by the `silt run`, `silt check`,
//! `silt fmt`, `silt disasm`, and `silt test` CLI paths.
//!
//! Centralized here so each subcommand renders identical diagnostics
//! for the same input — see `analyse_parsed_entry_file` for how the
//! type-checker's "unknown module" warning is reconciled with the
//! compiler's later resolution of those same imports.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process;

use silt::ast::{Decl, ImportTarget, Program};
use silt::bytecode::Function;
use silt::compiler::Compiler;
use silt::diagnostic::{Code, Diagnostic};
use silt::intern::{Symbol, resolve};
use silt::lexer::Lexer;
use silt::module;
use silt::parser::Parser;
use silt::source::{FileId, SourceMap, SourceName};
use silt::typechecker;

use crate::cli::package::package_setup_for_file;
use crate::cli::paths::ProgramFiles;
use crate::cli::source_scan::main_signature_error;

/// What the compile step of the pipeline emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Emit {
    /// Nothing: the compile step is skipped.
    Nothing,
    /// A script that registers the globals and then calls `main`. What
    /// `silt run`, `silt check` and `silt disasm` compile.
    Program,
    /// A script that registers the globals and calls nothing. What
    /// `silt test` compiles: it calls the test functions itself.
    Declarations,
}

/// The entry file after lexing and parsing: the first stage of the
/// pipeline. `silt test --filter` decides between the two stages whether
/// a file is worth analysing at all.
pub(crate) struct ParsedEntryFile {
    /// The original source text.
    pub(crate) source: String,
    /// The source map holding the entry file; the module files the
    /// compiler reads are added to it.
    pub(crate) sources: SourceMap,
    /// The declarations, as far as the parser could recover them. `None`
    /// when the text does not lex.
    pub(crate) program: Option<Program>,
    /// The lex error, or the parse errors.
    pub(crate) parse_errors: Vec<Diagnostic>,
}

/// Result of running the full compilation pipeline (lex → parse → typecheck → compile).
pub(crate) struct CompilePipelineResult {
    /// The original source text.
    pub(crate) source: String,
    /// The text of the entry file and of every module file read.
    pub(crate) sources: SourceMap,
    /// The parsed entry file, as far as the parser could recover it.
    /// `None` when the text does not lex. Questions about the file's
    /// declarations (does it define `main`, which functions are tests)
    /// are answered from here, see `cli::source_scan`; nothing scans
    /// `source` for them.
    pub(crate) program: Option<Program>,
    /// Parse errors (may be non-empty even when compilation proceeds).
    pub(crate) parse_errors: Vec<Diagnostic>,
    /// Type errors and warnings.
    pub(crate) type_errors: Vec<Diagnostic>,
    /// Compiled functions — `None` if hard errors prevented compilation.
    pub(crate) functions: Option<Vec<Function>>,
    /// Compile errors (if compilation was attempted but failed).
    pub(crate) compile_errors: Vec<Diagnostic>,
    /// Compiler warnings (empty if compilation was not attempted).
    pub(crate) compile_warnings: Vec<Diagnostic>,
}

/// Run the full compilation pipeline for `path`: read file → lex → parse (recovering)
/// → typecheck → compile. Returns all diagnostics and compiled output without printing
/// anything or exiting, so callers can decide how to present results.
///
/// - `skip_compile`: skip the compilation step (used by `check_file` which only needs diagnostics).
/// - `typecheck_on_parse_errors`: run the type checker even when there are parse errors
///   (used by `check_file` to report as many diagnostics as possible).
/// - `auto_update_lock`: when true and the file lives inside a silt
///   package, regenerate `silt.lock` if it's missing or stale before
///   compilation. Set to `false` for read-only commands like `silt
///   disasm` and `silt fmt` so they don't mutate user files.
pub(crate) fn run_compile_pipeline_with_options(
    path: &str,
    skip_compile: bool,
    typecheck_on_parse_errors: bool,
    auto_update_lock: bool,
) -> CompilePipelineResult {
    let source = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error reading {path}: {e}");
            process::exit(1);
        }
    };
    let emit = if skip_compile {
        Emit::Nothing
    } else {
        Emit::Program
    };
    analyse_parsed_entry_file(
        path,
        parse_entry_file(path, source),
        emit,
        typecheck_on_parse_errors,
        auto_update_lock,
    )
}

/// First stage of the pipeline: lex and parse (recovering) the text of the
/// entry file `path`. Touches nothing but its arguments.
pub(crate) fn parse_entry_file(path: &str, source: String) -> ParsedEntryFile {
    let mut sources = SourceMap::new();
    let file: FileId = sources.add(SourceName::Path(path.into()), source.as_str().into());
    let tokens = match Lexer::new(file, &source).tokenize() {
        Ok(t) => t,
        Err(e) => {
            // Lex errors are fatal for all callers. Return a result with the error
            // so that `check_file` can format it as JSON when needed.
            return ParsedEntryFile {
                source,
                sources,
                program: None,
                parse_errors: vec![e],
            };
        }
    };

    let (program, parse_errors) = Parser::new(tokens, &source).parse_program_recovering();
    ParsedEntryFile {
        source,
        sources,
        program: Some(program),
        parse_errors,
    }
}

/// Second stage of the pipeline: resolve the package, typecheck, and
/// compile what `emit` asks for. Every front door that compiles an entry
/// file goes through here, so they all report the same diagnostics for
/// it. The parameters are those of [`run_compile_pipeline_with_options`].
pub(crate) fn analyse_parsed_entry_file(
    path: &str,
    parsed: ParsedEntryFile,
    emit: Emit,
    typecheck_on_parse_errors: bool,
    auto_update_lock: bool,
) -> CompilePipelineResult {
    let ParsedEntryFile {
        source,
        sources,
        program,
        parse_errors,
    } = parsed;
    let Some(mut program) = program else {
        return CompilePipelineResult {
            source,
            sources,
            program: None,
            parse_errors,
            type_errors: Vec::new(),
            functions: None,
            compile_errors: Vec::new(),
            compile_warnings: Vec::new(),
        };
    };
    let has_parse_errors = !parse_errors.is_empty();

    // Derive the package_roots map: when `path` is inside a silt
    // package, this loads `silt.toml` and (for dep-resolving commands)
    // auto-regenerates `silt.lock` if stale before resolving the dep
    // tree. For ad-hoc scripts outside any package, falls back to a
    // synthetic local-only setup keyed off the file's parent directory.
    //
    // Resolved before typechecking so the typechecker can stamp this
    // module's trait/enum/record decls with their owning package and
    // enforce the orphan rule. Imported modules are typechecked
    // separately by the compiler with their own package context.
    //
    // The `auto_update_lock` flag distinguishes mutation-allowed
    // callers (`silt run`, `silt check`, `silt test`) from read-only
    // callers (`silt disasm`, `silt fmt`). Read-only callers still
    // need a dep map; they just resolve in-memory rather than writing
    // a refreshed lockfile to disk.
    let (local_pkg, package_roots) = package_setup_for_file(path, auto_update_lock);
    // Keep a copy of the dep map for the round-93 dep-export
    // registration below; the original moves into the compiler.
    let dep_roots = package_roots.clone();

    // Round 64 item 6A: build the compiler up-front so it can
    // pre-typecheck the entrypoint's user-module imports before the
    // entrypoint itself is typechecked. The cached exports map is
    // then threaded into both the entrypoint typecheck and the
    // compile pass — the latter reuses already-loaded module
    // typechecks via `compiled_modules` / `module_exports`.
    let mut compiler = Compiler::with_package_roots(local_pkg, package_roots);
    compiler.set_sources(sources);
    if !has_parse_errors {
        compiler.pre_typecheck_imports(&program);
    }
    let mut module_exports = compiler.module_exports_snapshot();
    if !has_parse_errors {
        // Round 93: register declared package dependencies' exports
        // under the names the entrypoint imports them by, so the
        // typechecker sees into deps the same way it sees into
        // same-package sibling modules. See the helper's doc comment.
        register_dep_import_exports(
            &mut compiler,
            &program,
            local_pkg,
            &dep_roots,
            &mut module_exports,
        );
    }

    // Skip the type checker when there are parse errors, unless the caller opted in
    // (e.g. `check_file` reports as many diagnostics as possible on partial programs).
    //
    // Thread the compiler's session-shared `Resolver` through the
    // entrypoint's typecheck so user aliases registered while
    // pre-typechecking imported modules stay visible. The resolver is
    // stitched back into the compiler afterward so the trait-impl
    // emission path (`Decl::TraitImpl` arm) sees the accumulated
    // alias state when canonicalising target-type symbols.
    //
    // When the program imports a module the type checker can't see, the
    // "unknown module" warning and every name that module would supply
    // surfacing as "undefined" are left out, so the user gets one clear
    // module-level error from the compile stage instead of a wall of
    // undefined-name noise (`typechecker::without_import_cascade`).
    // Resolvable imports (sibling modules, declared deps) are
    // pre-typechecked, so the warning never fires for them.
    //
    // The typechecker and the compiler both report "module 'X' is not
    // imported" for the same call site; the compiler's copy is the one
    // that blocks bytecode emission, so the typechecker's is left out.
    let mut type_errors: Vec<Diagnostic> = if !has_parse_errors || typecheck_on_parse_errors {
        let resolver = compiler.take_resolver();
        let (raw_type_errors, _entry_exports, resolver) =
            typechecker::check_with_package_and_imports_resolver(
                &mut program,
                Some(local_pkg),
                module_exports,
                Some(resolver),
            );
        compiler.put_resolver(resolver);
        typechecker::without_import_cascade(raw_type_errors)
            .into_iter()
            .filter(|d| d.code != Code::ModuleNotImported)
            .collect()
    } else {
        Vec::new()
    };

    // If there are parse errors or compilation is not requested, skip compile.
    // Type errors do NOT block compilation — the compiler resolves modules
    // during compilation, which fixes most "undefined" errors from the type
    // checker.  The test suite already relies on this behavior.
    if has_parse_errors || emit == Emit::Nothing {
        // Round 92: imported user modules' REAL type errors (harvested
        // during `pre_typecheck_imports`, already filtered so the
        // import-resolvable cascade stays suppressed) flow into the
        // result alongside the entrypoint's own diagnostics. Each one
        // is rendered against the imported module's source/path, so it
        // carries the right file and span. Previously these were
        // dropped wholesale and `silt check` exited 0 on a program
        // whose imported module fails its own direct check.
        type_errors.extend(compiler.take_module_type_errors());
        return CompilePipelineResult {
            source,
            sources: compiler.take_sources(),
            program: Some(program),
            parse_errors,
            type_errors,
            functions: None,
            compile_errors: Vec::new(),
            compile_warnings: Vec::new(),
        };
    }

    // Compile.
    let compile_result = if emit == Emit::Declarations {
        compiler.compile_declarations(&program)
    } else {
        compiler.compile_program(&program)
    };
    // Round 92: merge imported-module type errors (see the comment on
    // the skip-compile arm above). Taken after the compile pass so
    // modules first loaded during compilation are harvested too.
    type_errors.extend(compiler.take_module_type_errors());
    // A `main` that declares parameters compiles, but the program cannot
    // start: the entry point is called without arguments. Reported here,
    // with the compile errors, so that every front door rejects it
    // before anything runs.
    let entry_point_error = main_signature_error(&program);
    match compile_result {
        Ok(functions) => {
            let compile_warnings: Vec<Diagnostic> = compiler.warnings().to_vec();
            // An Ok compile can still have accumulated module parse
            // errors if a future refactor teaches the compiler to keep
            // going past a broken module. Today the first error short-
            // circuits, so this is defensive — but draining on both
            // arms keeps the "every diagnostic, one run" invariant
            // robust against that evolution.
            let mut compile_errors: Vec<Diagnostic> = compiler.module_parse_errors().to_vec();
            compile_errors.extend(entry_point_error);
            CompilePipelineResult {
                source,
                sources: compiler.take_sources(),
                program: Some(program),
                parse_errors,
                type_errors,
                functions: Some(functions),
                compile_errors,
                compile_warnings,
            }
        }
        Err(e) => {
            // Primary first, then the rest in source order. This matches
            // how the entrypoint's own parse errors flow (all pushed,
            // parse-source order) so the composite output is uniform.
            // A `loop(...)` outside its loop is already a type error at
            // the same place; report it once.
            let already_reported = e.code == Code::LoopCallOutsideLoop
                && type_errors
                    .iter()
                    .any(|t| t.is_error() && t.span.start == e.span.start);
            let mut compile_errors = Vec::new();
            if !already_reported {
                compile_errors.push(e);
            }
            compile_errors.extend(compiler.module_parse_errors().iter().cloned());
            compile_errors.extend(entry_point_error);
            CompilePipelineResult {
                source,
                sources: compiler.take_sources(),
                program: Some(program),
                parse_errors,
                type_errors,
                functions: None,
                compile_errors,
                compile_warnings: Vec::new(),
            }
        }
    }
}

/// Round 93: make declared package dependencies statically visible to
/// the entrypoint's typecheck.
///
/// `Compiler::pre_typecheck_imports` already resolves `import <dep>`
/// (a `silt.toml` path/git dependency) to the dep's `src/lib.silt` and
/// typechecks it — that is how dep-INTERNAL type errors reach
/// `silt check` (round 92). But it caches the resulting exports under
/// the dep's resolved *module* name (`"lib"`), not under the name the
/// importer wrote (e.g. `"mathutil"`). The entrypoint typecheck looks
/// imports up by the written name, misses, and emits the "unknown
/// module" warning — which used to trip the file-wide import-cascade
/// suppression and silently disable ALL static name/type checking in
/// the importing file (round-93 finding: an undefined variable in a
/// file that also imported a dep passed `silt check` AND `silt run`).
///
/// This helper closes the hole at the pipeline layer: for every direct
/// import that names a declared dependency (a key in the
/// lockfile-derived `package_roots` map other than the local package),
/// typecheck the dep's `lib.silt` with the dep's own package context
/// and register its exports under the *import name* in the snapshot
/// handed to the entrypoint typecheck. The unknown-module warning then
/// never fires for declared deps, the cascade filter stays dormant,
/// and `dep.nonexistent(...)` / wrong-argument-type calls against dep
/// exports are caught statically — matching how same-package sibling
/// modules have behaved since round 64/92.
///
/// Diagnostics from this extra typecheck are deliberately discarded:
/// the compiler's pre-pass has already harvested the dep's real type
/// errors against the dep's own file and spans (round 92,
/// `take_module_type_errors`), so surfacing them here would duplicate
/// every dep-internal diagnostic. Lex failures and unreadable files
/// are likewise skipped — the compile pass renders those with full
/// source context.
///
/// The session-shared resolver is threaded through each dep typecheck
/// (and restored on the compiler) so type aliases registered by deps
/// stay visible to the entrypoint typecheck and the later compile
/// pass, mirroring how the pipeline threads it around the entrypoint
/// check.
///
/// `silt test` gets this too: it compiles through
/// [`analyse_parsed_entry_file`] like run and check.
pub(crate) fn register_dep_import_exports(
    compiler: &mut Compiler,
    program: &Program,
    local_pkg: Symbol,
    package_roots: &HashMap<Symbol, PathBuf>,
    module_exports: &mut HashMap<Symbol, typechecker::ModuleExports>,
) {
    for decl in &program.decls {
        let module_sym = match decl {
            Decl::Import(ImportTarget::Module(m), _)
            | Decl::Import(ImportTarget::Items(m, _), _)
            | Decl::Import(ImportTarget::Alias(m, _), _) => *m,
            _ => continue,
        };
        // Already visible under the import name (sibling module, or a
        // dep registered by an earlier iteration)? Nothing to do.
        if module_exports.contains_key(&module_sym) {
            continue;
        }
        // `import <local_pkg_name>` from inside the package is a local
        // self-import, not a dep import — same disambiguation the
        // compiler's `resolve_import` applies.
        if module_sym == local_pkg {
            continue;
        }
        if module::is_builtin_module(&resolve(module_sym)) {
            continue;
        }
        // Only declared dependencies resolve here; a genuinely unknown
        // module keeps its "unknown module" warning + compile error.
        let Some(dep_root) = package_roots.get(&module_sym) else {
            continue;
        };
        let lib_path = dep_root.join("lib.silt");
        let Ok(dep_source) = fs::read_to_string(&lib_path) else {
            continue;
        };
        let file = compiler.sources_mut().add(
            SourceName::Path(lib_path.clone()),
            dep_source.as_str().into(),
        );
        let Ok(tokens) = Lexer::new(file, &dep_source).tokenize() else {
            continue;
        };
        // Recovery parse, like the compiler's pre-pass: a dep with
        // parse errors still yields a partial export surface; the
        // compile pass owns reporting the parse errors themselves.
        let (mut dep_program, _dep_parse_errors) =
            Parser::new(tokens, &dep_source).parse_program_recovering();
        let resolver = compiler.take_resolver();
        let (_dep_errors, dep_exports, resolver) =
            typechecker::check_with_package_and_imports_resolver(
                &mut dep_program,
                Some(module_sym),
                module_exports.clone(),
                Some(resolver),
            );
        compiler.put_resolver(resolver);
        module_exports.insert(module_sym, dep_exports);
    }
}

/// Every diagnostic of `result` that is shown to the user, in the order it
/// is printed: parse errors, type diagnostics, compile errors, compile
/// warnings. One list for `silt run`, `silt check`, `silt disasm` and
/// `silt test`.
pub(crate) fn reportable_diagnostics(result: &CompilePipelineResult) -> Vec<&Diagnostic> {
    result
        .parse_errors
        .iter()
        .chain(result.type_errors.iter())
        .chain(result.compile_errors.iter())
        .chain(result.compile_warnings.iter())
        .collect()
}

/// Exit gate for `compile_file_with_options`: does `result` carry a
/// real (non-suppressed) hard error that must abort the run?
///
/// A hard error is real only if it's a parse error, a compile error,
/// or a non-suppressed type diagnostic with severity Error. Warnings
/// (type or compile) never trip the gate.
///
/// `compile_errors` is part of the gate even though, today, it is
/// non-empty only when `compile_program` returned `Err` (in which case
/// `functions` is `None` and the caller exits anyway): the Ok-arm
/// `module_parse_errors()` drain in `run_compile_pipeline_with_options`
/// exists precisely so a future compiler that keeps going past a broken
/// imported module can still return `Some(functions)` alongside its
/// `error[compile]` diagnostics. Without this term, that evolution
/// would PRINT the compile error but then execute the program and exit
/// 0. Pure (no printing, no `process::exit`) so it is unit-testable —
/// lock: `tests::compile_error_with_functions_still_trips_the_gate`.
pub(crate) fn pipeline_has_real_hard_errors(result: &CompilePipelineResult) -> bool {
    let has_real_type_error = result.type_errors.iter().any(Diagnostic::is_error);
    !result.parse_errors.is_empty() || has_real_type_error || !result.compile_errors.is_empty()
}

/// Compile a file end-to-end (lex → parse → typecheck → compile),
/// printing diagnostics and exiting on hard errors.
/// `auto_update_lock` controls whether stale lockfiles are silently
/// regenerated (read-only callers like `silt disasm` pass `false`).
pub(crate) fn compile_file_with_options(
    path: &str,
    auto_update_lock: bool,
) -> (Vec<Function>, String) {
    let compiled = compile_file(path, auto_update_lock);
    (compiled.functions, compiled.source)
}

/// A file that went through the whole pipeline without a hard error.
pub(crate) struct CompiledFile {
    /// The compiled functions; the first one is the top-level script.
    pub(crate) functions: Vec<Function>,
    /// The original source text.
    pub(crate) source: String,
    /// The text of the file and of every module file it imports: the
    /// spans of the compiled code point into it.
    pub(crate) sources: SourceMap,
    /// The parsed declarations of the file.
    pub(crate) program: Program,
}

/// [`compile_file_with_options`], for callers that also ask questions
/// about the file's declarations.
pub(crate) fn compile_file(path: &str, auto_update_lock: bool) -> CompiledFile {
    let result = run_compile_pipeline_with_options(path, false, false, auto_update_lock);

    // See `pipeline_has_real_hard_errors` for what counts as "real".
    let has_real_hard_errors = pipeline_has_real_hard_errors(&result);

    // F14 (audit round 17): print diagnostics with a blank line between
    // consecutive errors so multi-error output doesn't form a solid wall
    // of text. Matches rustc/gcc convention.
    // Lock: tests/cli/cli_test_rendering_tests.rs
    // `test_multiple_errors_render_with_blank_separator`.
    silt::diagnostic::eprint_all(
        &ProgramFiles::new(path, &result.sources),
        reportable_diagnostics(&result),
    );

    // Exit gate: abort iff a real (non-suppressed) hard error exists.
    if has_real_hard_errors {
        process::exit(1);
    }

    let functions = match result.functions {
        Some(f) => f,
        None => process::exit(1),
    };

    if functions.is_empty() {
        eprintln!("{path}: internal error: no functions compiled");
        process::exit(1);
    }

    // A file that compiled has lexed, so its declarations are there.
    let Some(program) = result.program else {
        eprintln!("{path}: internal error: compiled without a parsed program");
        process::exit(1);
    };

    CompiledFile {
        functions,
        source: result.source,
        sources: result.sources,
        program,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use silt::source::Span;

    fn diag(code: Code, message: &str, is_warning: bool) -> Diagnostic {
        let span = Span::point(FileId::default(), 0);
        if is_warning {
            Diagnostic::warning(code, span, message)
        } else {
            Diagnostic::error(code, span, message)
        }
    }

    /// A fully clean pipeline result whose compile step succeeded
    /// (`functions: Some`). Tests mutate one field at a time.
    fn clean_ok_result() -> CompilePipelineResult {
        CompilePipelineResult {
            source: String::new(),
            sources: SourceMap::new(),
            program: Some(Program { decls: Vec::new() }),
            parse_errors: Vec::new(),
            type_errors: Vec::new(),
            functions: Some(Vec::new()),
            compile_errors: Vec::new(),
            compile_warnings: Vec::new(),
        }
    }

    /// Lock for the LATENT exit-gate hole: a result carrying BOTH
    /// compiled functions AND an `error[compile]` diagnostic (the shape
    /// the Ok-arm `module_parse_errors()` drain produces if the
    /// compiler ever keeps going past a broken imported module) must
    /// trip the hard-error gate.
    #[test]
    fn compile_error_with_functions_still_trips_the_gate() {
        let mut result = clean_ok_result();
        result.compile_errors.push(diag(
            Code::ModuleNotFound,
            "cannot load module 'broken'",
            false,
        ));
        assert!(
            pipeline_has_real_hard_errors(&result),
            "an error[compile] diagnostic must abort the run even when \
             the compiler still produced functions"
        );
    }

    /// Positive control: warnings alone (compile warnings and type
    /// warnings) must NOT trip the gate — the program should still run.
    #[test]
    fn warnings_alone_do_not_trip_the_gate() {
        let mut result = clean_ok_result();
        result
            .compile_warnings
            .push(diag(Code::ShadowsModule, "variable 'list' shadows", true));
        result
            .type_errors
            .push(diag(Code::PolymorphicRecursion, "'f' is recursing", true));
        assert!(
            !pipeline_has_real_hard_errors(&result),
            "warnings must not abort the run"
        );
    }

    /// Controls pinning the pre-existing arms of the gate.
    #[test]
    fn parse_and_type_errors_trip_the_gate() {
        let mut with_parse = clean_ok_result();
        with_parse.parse_errors.push(diag(
            Code::ExpectedExpression,
            "unexpected token '}'",
            false,
        ));
        assert!(pipeline_has_real_hard_errors(&with_parse));

        let mut with_type = clean_ok_result();
        with_type.type_errors.push(diag(
            Code::TypeMismatch,
            "type mismatch: expected Int, got String",
            false,
        ));
        assert!(pipeline_has_real_hard_errors(&with_type));

        assert!(!pipeline_has_real_hard_errors(&clean_ok_result()));
    }
}
