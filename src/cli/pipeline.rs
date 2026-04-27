//! Shared compilation pipeline used by the `silt run`, `silt check`,
//! `silt fmt`, `silt disasm`, and `silt test` CLI paths.
//!
//! Centralized here so each subcommand renders identical diagnostics
//! for the same input — see `reportable_type_errors` for the dance
//! we do to reconcile the type-checker's "unknown module" warning
//! with the compiler's later resolution of those same imports.

use std::fs;
use std::process;

use silt::bytecode::Function;
use silt::compiler::Compiler;
use silt::errors::SourceError;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;

use crate::cli::package::package_setup_for_file;

/// Result of running the full compilation pipeline (lex → parse → typecheck → compile).
pub(crate) struct CompilePipelineResult {
    /// The original source text.
    pub(crate) source: String,
    /// Parse errors (may be non-empty even when compilation proceeds).
    pub(crate) parse_errors: Vec<SourceError>,
    /// Type errors and warnings.
    pub(crate) type_errors: Vec<SourceError>,
    /// Compiled functions — `None` if hard errors prevented compilation.
    pub(crate) functions: Option<Vec<Function>>,
    /// Compile errors (if compilation was attempted but failed).
    pub(crate) compile_errors: Vec<SourceError>,
    /// Compiler warnings (empty if compilation was not attempted).
    pub(crate) compile_warnings: Vec<SourceError>,
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
pub(crate) fn run_compile_pipeline(
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

    let tokens = match Lexer::new(&source).tokenize() {
        Ok(t) => t,
        Err(e) => {
            // Lex errors are fatal for all callers. Return a result with the error
            // so that `check_file` can format it as JSON when needed.
            let source_err = SourceError::from_lex_error(&e, &source, path);
            return CompilePipelineResult {
                source,
                parse_errors: vec![source_err],
                type_errors: Vec::new(),
                functions: None,
                compile_errors: Vec::new(),
                compile_warnings: Vec::new(),
            };
        }
    };

    let (mut program, raw_parse_errors) = Parser::new(tokens).parse_program_recovering();

    let parse_errors: Vec<SourceError> = raw_parse_errors
        .iter()
        .map(|e| SourceError::from_parse_error(e, &source, path))
        .collect();
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

    // Round 64 item 6A: build the compiler up-front so it can
    // pre-typecheck the entrypoint's user-module imports before the
    // entrypoint itself is typechecked. The cached exports map is
    // then threaded into both the entrypoint typecheck and the
    // compile pass — the latter reuses already-loaded module
    // typechecks via `compiled_modules` / `module_exports`.
    let mut compiler = Compiler::with_package_roots(local_pkg, package_roots);
    if !has_parse_errors {
        compiler.pre_typecheck_imports(&program);
    }
    let module_exports = compiler.module_exports_snapshot();

    // Skip the type checker when there are parse errors, unless the caller opted in
    // (e.g. `check_file` reports as many diagnostics as possible on partial programs).
    let type_errors: Vec<SourceError> = if !has_parse_errors || typecheck_on_parse_errors {
        let (raw_type_errors, _entry_exports) = typechecker::check_with_package_and_imports(
            &mut program,
            Some(local_pkg),
            module_exports,
        );
        raw_type_errors
            .iter()
            .map(|e| SourceError::from_type_error(e, &source, path))
            .collect()
    } else {
        Vec::new()
    };

    // If there are parse errors or compilation is not requested, skip compile.
    // Type errors do NOT block compilation — the compiler resolves modules
    // during compilation, which fixes most "undefined" errors from the type
    // checker.  The test suite already relies on this behavior.
    if has_parse_errors || skip_compile {
        return CompilePipelineResult {
            source,
            parse_errors,
            type_errors,
            functions: None,
            compile_errors: Vec::new(),
            compile_warnings: Vec::new(),
        };
    }

    // Compile.
    match compiler.compile_program(&program) {
        Ok(functions) => {
            let compile_warnings: Vec<SourceError> = compiler
                .warnings()
                .iter()
                .map(|w| SourceError::compile_warning(&w.message, w.span, &source, path))
                .collect();
            // An Ok compile can still have accumulated module parse
            // errors if a future refactor teaches the compiler to keep
            // going past a broken module. Today the first error short-
            // circuits, so this is defensive — but draining on both
            // arms keeps the "every diagnostic, one run" invariant
            // robust against that evolution.
            let module_extras: Vec<SourceError> = compiler
                .module_parse_errors()
                .iter()
                .map(|e| SourceError::from_compile_error(e, &source, path))
                .collect();
            CompilePipelineResult {
                source,
                parse_errors,
                type_errors,
                functions: Some(functions),
                compile_errors: module_extras,
                compile_warnings,
            }
        }
        Err(e) => {
            // Primary first, then the rest in source order. This matches
            // how the entrypoint's own parse errors flow (all pushed,
            // parse-source order) so the composite output is uniform.
            let mut compile_errors = vec![SourceError::from_compile_error(&e, &source, path)];
            compile_errors.extend(
                compiler
                    .module_parse_errors()
                    .iter()
                    .map(|extra| SourceError::from_compile_error(extra, &source, path)),
            );
            CompilePipelineResult {
                source,
                parse_errors,
                type_errors,
                functions: None,
                compile_errors,
                compile_warnings: Vec::new(),
            }
        }
    }
}

/// Return the type-checker diagnostics that should still be reported for `result`
/// after dropping noise that the compiler will resolve.
///
/// Why: the type checker runs before module resolution (which happens during
/// compilation). When a program imports from an unknown module, the checker
/// emits an "unknown module" *warning* for that import. We want to drop that
/// warning (and ONLY that warning) from the `compile_file` path so the user
/// isn't told about an import they actually wrote correctly. All other
/// diagnostics — real type errors, other warnings — must flow through
/// untouched so they continue to abort the run.
///
/// Previously this helper did substring matching on every entry and
/// suppressed ALL type diagnostics whenever any of them mentioned "unknown
/// module", which silently masked real type errors in any file that also
/// happened to import a user module. Filtering per-entry fixes that while
/// keeping the clean UX for importers.
pub(crate) fn reportable_type_errors(result: &CompilePipelineResult) -> Vec<&SourceError> {
    let has_user_import_warning = result.type_errors.iter().any(is_unknown_module_warning);
    result
        .type_errors
        .iter()
        .filter(|e| !is_unknown_module_warning(e))
        // When the program imports a user module the type checker can't
        // see, every name it exports surfaces as "undefined". The
        // compiler does resolve those at link time, so we demote them
        // here and let the compile-or-runtime stage be the source of
        // truth for name resolution.
        .filter(|e| !(has_user_import_warning && is_user_import_resolvable_error(e)))
        // B9 (round 60): the typechecker and the compiler both emit
        // "module 'X' is not imported" for the same call site. Without
        // this filter, `silt check main.silt` prints the identical
        // sentence twice — once as `error[type]`, once as
        // `error[compile]`. The compiler's version is the authoritative
        // one (it's what actually blocks bytecode emission), so drop
        // the typechecker's copy in the CLI pipeline when compilation
        // will re-surface it. The typechecker-only callers (LSP,
        // `missing_import_recommends_tests`) still see the diagnostic
        // via `typechecker::check` directly.
        .filter(|e| !is_module_not_imported_typecheck_error(e))
        .collect()
}

/// Returns true iff `err` is the "unknown module" warning that the type
/// checker emits for imports the compiler will later resolve. We gate on
/// both the warning severity and the message prefix so a future real type
/// error that happens to mention those words isn't swallowed.
pub(crate) fn is_unknown_module_warning(err: &SourceError) -> bool {
    err.is_warning
        && err.kind == silt::errors::ErrorKind::Type
        && err.message.contains("unknown module")
}

/// Returns true iff `err` is the typechecker's "module 'X' is not
/// imported" error. The compiler emits the same diagnostic (with
/// identical wording, see `src/compiler/mod.rs:1923`, `:2029`, `:2782`)
/// as a hard compile error that actually blocks bytecode emission, so
/// the CLI pipeline drops the typechecker's copy to avoid rendering the
/// same sentence twice. See `reportable_type_errors` for the call site.
pub(crate) fn is_module_not_imported_typecheck_error(err: &SourceError) -> bool {
    err.kind == silt::errors::ErrorKind::Type
        && !err.is_warning
        && err.message.contains("is not imported")
        && err.message.contains("add `import ")
}

/// Returns true iff `err` is a typechecker diagnostic that the
/// compiler is likely to resolve at link time (because the name comes
/// from a user-module import that the type checker can't see into).
///
/// The type checker only registers selective imports for builtin
/// modules; for user modules it emits an "unknown module" warning and
/// every imported name then surfaces as "undefined variable" /
/// "undefined constructor" / "undefined type" / "unknown field". We
/// demote those to warnings so the run proceeds; if the name truly is
/// undefined the compiler will emit a hard runtime/link error.
///
/// Trait-impl cascades: when the unknown module is the one that would
/// have supplied a type's trait impl (or defined a supertrait), the
/// typechecker emits one of three shapes, all containing
/// `"does not implement"`:
///   - `"type '<X>' does not implement trait '<Y>'"`
///   - `"type '<X>' does not implement Display (required for string interpolation)"`
///   - `"type '<X>' implements '<T>' but does not implement supertrait '<S>'"`
/// We match those via the narrow substring `"does not implement"`
/// rather than `starts_with("type ")`, which would also swallow every
/// real `"type mismatch: expected ..., got ..."` and
/// `"type argument count mismatch ..."` produced by the typechecker —
/// the old prefix silently demoted real type errors in any file that
/// also happened to import a user module (GAP #7).
pub(crate) fn is_user_import_resolvable_error(err: &SourceError) -> bool {
    err.kind == silt::errors::ErrorKind::Type
        && !err.is_warning
        && (err.message.starts_with("undefined variable")
            || err.message.starts_with("undefined constructor")
            || err.message.starts_with("undefined type")
            || err.message.starts_with("unknown field")
            || err.message.contains("does not implement"))
}

/// Print all diagnostics to stderr and exit(1) if there are hard errors.
/// Returns the compiled functions and source on success.
pub(crate) fn compile_file(path: &str) -> (Vec<Function>, String) {
    compile_file_with_options(path, true)
}

/// Like [`compile_file`] but lets the caller opt out of lockfile
/// auto-regeneration. `silt disasm` is the only read-only caller that
/// uses `false` here — it inspects bytecode without the side effect of
/// writing `silt.lock`.
pub(crate) fn compile_file_with_options(
    path: &str,
    auto_update_lock: bool,
) -> (Vec<Function>, String) {
    let result = run_compile_pipeline(path, false, false, auto_update_lock);

    // Filter per-entry: drop the "unknown module" warnings the compiler will
    // resolve, but keep every other type diagnostic so real errors still
    // surface. See `reportable_type_errors` for the rationale.
    let reportable = reportable_type_errors(&result);
    // A hard error is real only if it's a parse/compile error or a
    // non-suppressed type error with severity Error.
    let has_real_type_error = reportable.iter().any(|e| !e.is_warning);
    let has_parse_errors = !result.parse_errors.is_empty();
    let has_real_hard_errors = has_parse_errors || has_real_type_error;

    // F14 (audit round 17): print diagnostics with a blank line between
    // consecutive errors so multi-error output doesn't form a solid wall
    // of text. Matches rustc/gcc convention.
    // Lock: tests/cli_test_rendering_tests.rs
    // `test_multiple_errors_render_with_blank_separator`.
    let all_errs: Vec<&SourceError> = result
        .parse_errors
        .iter()
        .chain(reportable.iter().copied())
        .chain(result.compile_errors.iter())
        .chain(result.compile_warnings.iter())
        .collect();
    silt::errors::eprintln_errors_with_separator(&all_errs);

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

    (functions, result.source)
}
