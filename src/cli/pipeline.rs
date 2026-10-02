//! The compile step of `silt run`, `silt check`, `silt disasm` and
//! `silt test`: a thin layer over `silt::session::Session` that turns its
//! analysis and its compiled program into what each subcommand prints.
//!
//! Every door gets the diagnostics of the same analysis, so they report
//! the same ones for the same input.

use std::fs;
use std::path::Path;
use std::process;

use silt::ast::Program;
use silt::bytecode::Function;
use silt::diagnostic::Diagnostic;
use silt::package_graph::LockChange;
use silt::session::{
    Config, Entry, LockPolicy, ProjectSetup, Session, looks_like_library_module,
    looks_like_test_file,
};
use silt::source::{FileId, SourceMap};

use crate::cli::package::{PackageFailure, die_on_manifest_error};
use crate::cli::paths::ProgramFiles;

/// What the compile step compiles the entry file for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Emit {
    /// A program that starts at `main`: what `silt run` and `silt
    /// disasm` compile.
    Program,
    /// The declarations and the test functions, with nothing called:
    /// what `silt test` compiles.
    Declarations,
    /// What `silt check` compiles: a program that starts at `main`, or,
    /// for a library module or a test file, which have no `main` on
    /// purpose, the declarations.
    Check,
}

/// The entry file after it was read and parsed: the first stage. `silt
/// test --filter` decides between the two stages whether a file is worth
/// analysing at all.
pub(crate) struct ParsedEntryFile {
    session: Session,
    file: FileId,
    /// The declarations, as far as the parser could recover them. `None`
    /// when the text does not lex.
    pub(crate) program: Option<Program>,
}

/// Result of the compile step.
pub(crate) struct CompilePipelineResult {
    /// The text of the entry file and of every module file read.
    pub(crate) sources: SourceMap,
    /// The parsed entry file, as far as the parser could recover it.
    /// `None` when the text does not lex. Questions about the file's
    /// declarations (does it define `main`, which functions are tests)
    /// are answered from here, see `cli::source_scan`.
    pub(crate) program: Option<Program>,
    /// The entry file's lex or parse errors.
    pub(crate) parse_errors: Vec<Diagnostic>,
    /// The rest of the analysis: type diagnostics, the problems of the
    /// modules imported, import cycles.
    pub(crate) type_errors: Vec<Diagnostic>,
    /// Compiled functions — `None` if an error prevented compilation.
    pub(crate) functions: Option<Vec<Function>>,
    /// What compiling found: the entry point's errors, compile errors.
    pub(crate) compile_errors: Vec<Diagnostic>,
    /// Compiler warnings (empty if compilation was not attempted).
    pub(crate) compile_warnings: Vec<Diagnostic>,
}

/// A session for the entry file `path`: its project is found from the
/// file's directory.
fn session_for(path: &str, lock: LockPolicy) -> Session {
    let dir = Path::new(path)
        .canonicalize()
        .unwrap_or_else(|_| Path::new(path).to_path_buf())
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));
    let mut session = Session::new(Config {
        project: ProjectSetup::Discover(dir),
        lock,
        host: Vec::new(),
    });
    let failure = match session.packages() {
        Ok(packages) => {
            if packages.lock == LockChange::Updated {
                eprintln!("Updating silt.lock for new dependencies in silt.toml");
            }
            None
        }
        Err(diagnostics) => Some(diagnostics.to_vec()),
    };
    if let Some(diagnostics) = failure {
        die_on_manifest_error(PackageFailure {
            sources: session.into_sources(),
            diagnostics,
        });
    }
    session
}

/// First stage: read the project and parse `source`, the text of the
/// entry file `path`.
pub(crate) fn parse_entry_file(path: &str, source: String, lock: LockPolicy) -> ParsedEntryFile {
    let mut session = session_for(path, lock);
    let file = session.open_text(Path::new(path), &source);
    let program = session.graph().module(session.module_of(file)).ast.clone();
    ParsedEntryFile {
        session,
        file,
        program,
    }
}

/// Second stage: analyse the entry file and its modules, and compile what
/// `emit` asks for when the analysis has no error.
pub(crate) fn analyse_parsed_entry_file(
    parsed: ParsedEntryFile,
    emit: Emit,
) -> CompilePipelineResult {
    let ParsedEntryFile {
        mut session,
        file,
        program,
    } = parsed;
    let entry = session.module_of(file);
    let problems = session.graph().module(entry).problems.clone();
    let analysis = session.analyze(file).clone();
    let (parse_errors, type_errors): (Vec<Diagnostic>, Vec<Diagnostic>) = analysis
        .diagnostics
        .into_iter()
        .partition(|d| problems.contains(d));
    let target = match emit {
        Emit::Program => Entry::Main,
        Emit::Declarations => Entry::Tests { filter: None },
        Emit::Check => match &program {
            Some(p) if looks_like_library_module(p) || looks_like_test_file(p) => {
                Entry::Tests { filter: None }
            }
            _ => Entry::Main,
        },
    };
    let (functions, compile_errors, compile_warnings) = match session.compile(file, target) {
        Ok(compiled) => (Some(compiled.functions), Vec::new(), compiled.warnings),
        Err(errors) => (None, errors, Vec::new()),
    };
    CompilePipelineResult {
        sources: session.into_sources(),
        program,
        parse_errors,
        type_errors,
        functions,
        compile_errors,
        compile_warnings,
    }
}

/// Read, analyse and compile the entry file `path`. Returns every
/// diagnostic and the compiled output without printing them, so the
/// caller decides how to present them. An unreadable file is reported
/// and the process exits.
pub(crate) fn run_compile_pipeline(
    path: &str,
    emit: Emit,
    lock: LockPolicy,
) -> CompilePipelineResult {
    let source = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error reading {path}: {e}");
            process::exit(1);
        }
    };
    analyse_parsed_entry_file(parse_entry_file(path, source, lock), emit)
}

/// Every diagnostic of `result` that is shown to the user, in the order it
/// is printed: parse errors, the rest of the analysis, compile errors,
/// compile warnings. One list for `silt run`, `silt check`, `silt disasm`
/// and `silt test`.
pub(crate) fn reportable_diagnostics(result: &CompilePipelineResult) -> Vec<&Diagnostic> {
    result
        .parse_errors
        .iter()
        .chain(result.type_errors.iter())
        .chain(result.compile_errors.iter())
        .chain(result.compile_warnings.iter())
        .collect()
}

/// Does `result` carry an error that must stop the run? Warnings never
/// do; an error of any phase does.
pub(crate) fn pipeline_has_real_hard_errors(result: &CompilePipelineResult) -> bool {
    reportable_diagnostics(result)
        .into_iter()
        .any(Diagnostic::is_error)
}

/// A file that went through the whole pipeline without an error.
pub(crate) struct CompiledFile {
    /// The compiled functions; the first one is the top-level script.
    pub(crate) functions: Vec<Function>,
    /// The text of the file and of every module file it imports: the
    /// spans of the compiled code point into it.
    pub(crate) sources: SourceMap,
    /// The parsed declarations of the file.
    pub(crate) program: Program,
}

/// Compile the program that starts at `main` in the file `path`,
/// printing the diagnostics and exiting on an error. `lock` says whether
/// a stale lockfile may be rewritten (`silt disasm` passes `ReadOnly`).
pub(crate) fn compile_file(path: &str, lock: LockPolicy) -> CompiledFile {
    let result = run_compile_pipeline(path, Emit::Program, lock);

    // F14 (audit round 17): print diagnostics with a blank line between
    // consecutive errors so multi-error output doesn't form a solid wall
    // of text. Matches rustc/gcc convention.
    // Lock: tests/cli/cli_test_rendering_tests.rs
    // `test_multiple_errors_render_with_blank_separator`.
    silt::diagnostic::eprint_all(
        &ProgramFiles::new(path, &result.sources),
        reportable_diagnostics(&result),
    );
    if pipeline_has_real_hard_errors(&result) {
        process::exit(1);
    }
    match (result.functions, result.program) {
        (Some(functions), Some(program)) if !functions.is_empty() => CompiledFile {
            functions,
            sources: result.sources,
            program,
        },
        _ => {
            eprintln!("{path}: internal error: no functions compiled");
            process::exit(1);
        }
    }
}
