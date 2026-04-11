use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;

use silt::bytecode::Function;
use silt::compiler::Compiler;
use silt::disassemble::disassemble_function;
use silt::errors::SourceError;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::vm::Vm;

#[derive(Debug, Clone, Copy, PartialEq)]
enum OutputFormat {
    Human,
    Json,
}

// ── Shared compilation pipeline ─────────────────────────────────────

/// Result of running the full compilation pipeline (lex → parse → typecheck → compile).
struct CompilePipelineResult {
    /// The original source text.
    source: String,
    /// Parse errors (may be non-empty even when compilation proceeds).
    parse_errors: Vec<SourceError>,
    /// Type errors and warnings.
    type_errors: Vec<SourceError>,
    /// Whether any hard error (parse or type) was encountered.
    has_hard_errors: bool,
    /// Compiled functions — `None` if hard errors prevented compilation.
    functions: Option<Vec<Function>>,
    /// Compile errors (if compilation was attempted but failed).
    compile_errors: Vec<SourceError>,
    /// Compiler warnings (empty if compilation was not attempted).
    compile_warnings: Vec<SourceError>,
}

/// Run the full compilation pipeline for `path`: read file → lex → parse (recovering)
/// → typecheck → compile. Returns all diagnostics and compiled output without printing
/// anything or exiting, so callers can decide how to present results.
///
/// - `skip_compile`: skip the compilation step (used by `check_file` which only needs diagnostics).
/// - `typecheck_on_parse_errors`: run the type checker even when there are parse errors
///   (used by `check_file` to report as many diagnostics as possible).
fn run_compile_pipeline(
    path: &str,
    skip_compile: bool,
    typecheck_on_parse_errors: bool,
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
                has_hard_errors: true,
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

    // Skip the type checker when there are parse errors, unless the caller opted in
    // (e.g. `check_file` reports as many diagnostics as possible on partial programs).
    let (type_errors, has_type_hard_errors) = if !has_parse_errors || typecheck_on_parse_errors {
        let raw_type_errors = typechecker::check(&mut program);
        let hard = raw_type_errors
            .iter()
            .any(|e| e.severity == typechecker::Severity::Error);
        let errs: Vec<SourceError> = raw_type_errors
            .iter()
            .map(|e| SourceError::from_type_error(e, &source, path))
            .collect();
        (errs, hard)
    } else {
        (Vec::new(), false)
    };

    let has_hard_errors = has_parse_errors || has_type_hard_errors;

    // If there are parse errors or compilation is not requested, skip compile.
    // Type errors do NOT block compilation — the compiler resolves modules
    // during compilation, which fixes most "undefined" errors from the type
    // checker.  The test suite already relies on this behavior.
    if has_parse_errors || skip_compile {
        return CompilePipelineResult {
            source,
            parse_errors,
            type_errors,
            has_hard_errors,
            functions: None,
            compile_errors: Vec::new(),
            compile_warnings: Vec::new(),
        };
    }

    // Derive project root from the input file's directory.
    let project_root = Path::new(path)
        .canonicalize()
        .unwrap_or_else(|_| Path::new(path).to_path_buf())
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));

    // Compile.
    let mut compiler = Compiler::with_project_root(project_root);
    match compiler.compile_program(&program) {
        Ok(functions) => {
            let compile_warnings: Vec<SourceError> = compiler
                .warnings()
                .iter()
                .map(|w| SourceError::compile_warning(&w.message, w.span, &source, path))
                .collect();
            CompilePipelineResult {
                source,
                parse_errors,
                type_errors,
                has_hard_errors,
                functions: Some(functions),
                compile_errors: Vec::new(),
                compile_warnings,
            }
        }
        Err(e) => {
            let source_err = SourceError::from_compile_error(&e, &source, path);
            CompilePipelineResult {
                source,
                parse_errors,
                type_errors,
                has_hard_errors: true,
                functions: None,
                compile_errors: vec![source_err],
                compile_warnings: Vec::new(),
            }
        }
    }
}

/// All diagnostics from a `CompilePipelineResult`, in order.
fn all_diagnostics(result: &CompilePipelineResult) -> Vec<&SourceError> {
    result
        .parse_errors
        .iter()
        .chain(result.type_errors.iter())
        .chain(result.compile_errors.iter())
        .chain(result.compile_warnings.iter())
        .collect()
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
fn reportable_type_errors(result: &CompilePipelineResult) -> Vec<&SourceError> {
    result
        .type_errors
        .iter()
        .filter(|e| !is_unknown_module_warning(e))
        .collect()
}

/// Returns true iff `err` is the "unknown module" warning that the type
/// checker emits for imports the compiler will later resolve. We gate on
/// both the warning severity and the message prefix so a future real type
/// error that happens to mention those words isn't swallowed.
fn is_unknown_module_warning(err: &SourceError) -> bool {
    err.is_warning
        && err.kind == silt::errors::ErrorKind::Type
        && err.message.contains("unknown module")
}

/// Print all diagnostics to stderr and exit(1) if there are hard errors.
/// Returns the compiled functions and source on success.
fn compile_file(path: &str) -> (Vec<Function>, String) {
    let result = run_compile_pipeline(path, false, false);

    // Filter per-entry: drop the "unknown module" warnings the compiler will
    // resolve, but keep every other type diagnostic so real errors still
    // surface. See `reportable_type_errors` for the rationale.
    let reportable = reportable_type_errors(&result);
    // A hard error is real only if it's a parse/compile error or a
    // non-suppressed type error with severity Error.
    let has_real_type_error = reportable.iter().any(|e| !e.is_warning);
    let has_parse_errors = !result.parse_errors.is_empty();
    let has_real_hard_errors = has_parse_errors || has_real_type_error;

    for err in &result.parse_errors {
        eprintln!("{err}");
    }
    for err in &reportable {
        eprintln!("{err}");
    }
    for err in &result.compile_errors {
        eprintln!("{err}");
    }
    for err in &result.compile_warnings {
        eprintln!("{err}");
    }

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

/// Render the usage text shown by `silt --help` and the no-args screen.
///
/// Subcommands gated by Cargo features are annotated inline with the
/// feature they require, and the bottom line lists which features were
/// compiled in. This lets users discover missing features BEFORE running
/// a subcommand that would otherwise fail with "The 'X' feature is not
/// enabled" only after invocation.
fn usage_text() -> String {
    // Mark feature-gated subcommands with a `[feature: X]` suffix. The
    // marker is present regardless of whether the feature is compiled in —
    // that way `silt --help` is identical across builds and the user can
    // see what a richer build would offer.
    let mut out = String::new();
    out.push_str("silt — a statically-typed, expression-based language\n");
    out.push('\n');
    out.push_str("Usage:\n");
    out.push_str("  silt run [--watch] <file.silt>    Run a program");
    if !cfg!(feature = "watch") {
        out.push_str("  [--watch requires feature: watch]");
    }
    out.push('\n');
    out.push_str("  silt check [--watch] <file.silt>  Type-check without running\n");
    out.push_str("  silt test [--watch] [path]        Run test functions\n");
    out.push_str("  silt fmt [--check] [files...]       Format source code\n");
    out.push_str("  silt repl                         Interactive REPL  [feature: repl]\n");
    out.push_str("  silt init                         Create a new main.silt\n");
    out.push_str("  silt lsp                          Start the language server  [feature: lsp]\n");
    out.push_str("  silt disasm <file.silt>           Show bytecode disassembly\n");
    out.push('\n');
    out.push_str(&format!("Enabled features: {}\n", enabled_features()));
    out
}

/// Comma-separated list of Cargo features compiled into this binary.
/// Shown in `silt --help` so users can tell at a glance whether the
/// optional `repl`, `lsp`, and `watch` subcommands are available.
fn enabled_features() -> String {
    let mut feats: Vec<&'static str> = Vec::new();
    if cfg!(feature = "repl") {
        feats.push("repl");
    }
    if cfg!(feature = "lsp") {
        feats.push("lsp");
    }
    if cfg!(feature = "watch") {
        feats.push("watch");
    }
    if cfg!(feature = "local-clock") {
        feats.push("local-clock");
    }
    if cfg!(feature = "http") {
        feats.push("http");
    }
    if feats.is_empty() {
        "(none)".to_string()
    } else {
        feats.join(", ")
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprint!("{}", usage_text());
        process::exit(1);
    }

    // Handle --watch / -w flag: re-invoke without the flag on file changes
    #[cfg(feature = "watch")]
    if args.iter().any(|a| a == "--watch" || a == "-w") {
        let filtered: Vec<String> = args[1..]
            .iter()
            .filter(|a| *a != "--watch" && *a != "-w")
            .cloned()
            .collect();

        let watch_dir = filtered
            .iter()
            .filter_map(|a| {
                let path = Path::new(a.as_str());
                if a.ends_with(".silt") {
                    let parent = path.parent().unwrap_or(Path::new("."));
                    Some(if parent.as_os_str().is_empty() {
                        Path::new(".").to_path_buf()
                    } else {
                        parent.to_path_buf()
                    })
                } else if path.is_dir() {
                    Some(path.to_path_buf())
                } else {
                    None
                }
            })
            .next()
            .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| ".".into()));

        silt::watch::watch_and_rerun(&watch_dir, &filtered);
        return;
    }

    #[cfg(not(feature = "watch"))]
    if args.iter().any(|a| a == "--watch" || a == "-w") {
        eprintln!("The 'watch' feature is not enabled. Rebuild with: cargo build --features watch");
        process::exit(1);
    }

    match args[1].as_str() {
        "--version" | "-V" => {
            println!("silt {}", env!("CARGO_PKG_VERSION"));
            process::exit(0);
        }
        "--help" | "-h" | "help" => {
            print!("{}", usage_text());
            process::exit(0);
        }
        "run" => {
            if args[2..].iter().any(|a| a == "--help" || a == "-h") {
                println!("Usage: silt run [--watch] [--disassemble] <file.silt>");
                println!();
                println!("Options:");
                println!("  --watch, -w     Re-run on file changes");
                println!("  --disassemble   Show bytecode disassembly instead of running");
                println!();
                println!("Examples:");
                println!("  silt run main.silt");
                println!("  silt run --watch main.silt");
                println!("  silt run --disassemble main.silt");
                process::exit(0);
            }
            if args.len() < 3 {
                eprintln!("Usage: silt run <file.silt>");
                process::exit(1);
            }
            let mut disasm = false;
            let mut file: Option<String> = None;
            for arg in &args[2..] {
                if arg == "--disassemble" {
                    disasm = true;
                } else if arg.starts_with('-') {
                    let suggestion = match arg.as_str() {
                        "--disasm" | "--disassembly" | "-d" => " (did you mean --disassemble?)",
                        "--h" | "-help" => " (did you mean --help?)",
                        _ => "",
                    };
                    eprintln!("silt run: unknown flag '{arg}'{suggestion}");
                    eprintln!("Run 'silt run --help' for usage.");
                    process::exit(1);
                } else if file.is_none() {
                    file = Some(arg.clone());
                }
            }
            let Some(file) = file else {
                eprintln!("Usage: silt run <file.silt>");
                process::exit(1);
            };
            if disasm {
                disasm_file(&file);
            } else {
                vm_run_file(&file);
            }
        }
        "vm" => {
            // Legacy alias: `silt vm run <file>` -> same as `silt run <file>`
            match args.get(2).map(|s| s.as_str()) {
                Some("run") => {
                    let file = args.get(3).unwrap_or_else(|| {
                        eprintln!("Usage: silt vm run <file.silt>");
                        process::exit(1);
                    });
                    vm_run_file(file);
                }
                _ => {
                    eprintln!("Usage: silt vm run <file.silt>");
                    process::exit(1);
                }
            }
        }
        "disasm" => {
            if args[2..].iter().any(|a| a == "--help" || a == "-h") {
                println!("Usage: silt disasm <file.silt>");
                println!();
                println!("Prints the compiled bytecode disassembly for <file.silt>.");
                println!();
                println!("Example:");
                println!("  silt disasm main.silt");
                process::exit(0);
            }
            if args.len() < 3 {
                eprintln!("Usage: silt disasm <file.silt>");
                process::exit(1);
            }
            disasm_file(&args[2]);
        }
        "test" => {
            let mut file: Option<String> = None;
            let mut filter: Option<String> = None;
            let mut i = 2;
            while i < args.len() {
                if args[i] == "--filter" {
                    if i + 1 < args.len() {
                        filter = Some(args[i + 1].clone());
                        i += 2;
                    } else {
                        eprintln!("--filter requires a pattern");
                        process::exit(1);
                    }
                } else if args[i] == "--help" || args[i] == "-h" {
                    println!("Usage: silt test [--filter <pattern>] [--watch] [file]");
                    println!();
                    println!("Options:");
                    println!("  --filter <pat>   Only run tests whose name contains <pat>");
                    println!("  --watch, -w      Re-run on file changes");
                    println!();
                    println!("Auto-discovery: when no file is given, recursively runs tests");
                    println!("from files matching *_test.silt or *.test.silt.");
                    process::exit(0);
                } else if args[i].starts_with('-') {
                    // Unknown flag — don't silently treat as a filename.
                    let suggestion = match args[i].as_str() {
                        "--filters" | "-filter" | "-f" => " (did you mean --filter?)",
                        "--h" | "-help" => " (did you mean --help?)",
                        _ => "",
                    };
                    eprintln!("silt test: unknown flag '{}'{}", args[i], suggestion);
                    eprintln!("Run 'silt test --help' for usage.");
                    process::exit(1);
                } else {
                    file = Some(args[i].clone());
                    i += 1;
                }
            }
            run_tests(file.as_deref(), filter);
        }
        "check" => {
            let mut file: Option<String> = None;
            let mut format = OutputFormat::Human;
            let mut i = 2;
            while i < args.len() {
                if args[i] == "--format" {
                    if i + 1 < args.len() && args[i + 1] == "json" {
                        format = OutputFormat::Json;
                        i += 2;
                    } else {
                        eprintln!("--format requires 'json'");
                        process::exit(1);
                    }
                } else if args[i] == "--help" || args[i] == "-h" {
                    println!("Usage: silt check [--format json] [--watch] <file.silt>");
                    println!();
                    println!("Options:");
                    println!("  --format json   Emit diagnostics as JSON");
                    println!("  --watch, -w     Re-run on file changes");
                    process::exit(0);
                } else if args[i].starts_with('-') {
                    // Unknown flag — don't silently treat as a filename.
                    let suggestion = match args[i].as_str() {
                        "--formats" | "-format" | "-f" => " (did you mean --format?)",
                        "--h" | "-help" => " (did you mean --help?)",
                        _ => "",
                    };
                    eprintln!("silt check: unknown flag '{}'{}", args[i], suggestion);
                    eprintln!("Run 'silt check --help' for usage.");
                    process::exit(1);
                } else {
                    file = Some(args[i].clone());
                    i += 1;
                }
            }
            let Some(path) = file else {
                eprintln!("Usage: silt check [--format json] <file.silt>");
                process::exit(1);
            };
            check_file(&path, format);
        }
        #[cfg(feature = "lsp")]
        "lsp" => {
            for arg in &args[2..] {
                if arg == "--help" || arg == "-h" {
                    println!("Usage: silt lsp");
                    println!();
                    println!(
                        "Start the silt language server. Communicates over stdio using the"
                    );
                    println!(
                        "Language Server Protocol — invoked automatically by editor extensions"
                    );
                    println!(
                        "(VS Code, Vim/Neovim, etc.). Not typically run directly from a terminal."
                    );
                    process::exit(0);
                }
            }
            silt::lsp::run();
        }
        #[cfg(not(feature = "lsp"))]
        "lsp" => {
            eprintln!("The 'lsp' feature is not enabled. Rebuild with: cargo build --features lsp");
            process::exit(1);
        }
        #[cfg(feature = "repl")]
        "repl" => {
            for arg in &args[2..] {
                if arg == "--help" || arg == "-h" {
                    println!("Usage: silt repl");
                    println!();
                    println!("Start an interactive REPL session. Type :help inside for commands.");
                    process::exit(0);
                }
            }
            silt::repl::run_repl();
        }
        #[cfg(not(feature = "repl"))]
        "repl" => {
            eprintln!(
                "The 'repl' feature is not enabled. Rebuild with: cargo build --features repl"
            );
            process::exit(1);
        }
        "fmt" => {
            let mut check_mode = false;
            let mut files: Vec<String> = Vec::new();
            for arg in &args[2..] {
                if arg == "--check" {
                    check_mode = true;
                } else if arg == "--help" || arg == "-h" {
                    println!("Usage: silt fmt [--check] [files...]");
                    println!();
                    println!("Options:");
                    println!("  --check    Check formatting without modifying files");
                    process::exit(0);
                } else if arg.starts_with('-') {
                    // Unknown flag — don't silently treat as a filename.
                    let suggestion = match arg.as_str() {
                        "--checks" | "--Check" | "-check" | "-c" => " (did you mean --check?)",
                        "--h" | "-help" => " (did you mean --help?)",
                        _ => "",
                    };
                    eprintln!("silt fmt: unknown flag '{arg}'{suggestion}");
                    eprintln!("Run 'silt fmt --help' for usage.");
                    process::exit(1);
                } else {
                    files.push(arg.clone());
                }
            }
            // If no files given (or just an explicit `.`), find all .silt files
            // in the current directory recursively. This is risky if the user
            // happens to run `silt fmt` outside a project, so we require a
            // project anchor (silt.toml, .git) OR an explicit `.` argument,
            // and always emit a loud warning + file preview when the recursion
            // is triggered implicitly.
            let explicit_dot = files.iter().any(|f| f == "." || f == "./");
            if explicit_dot {
                // Strip the `.` marker; we'll treat it as the recursive sentinel.
                files.retain(|f| f != "." && f != "./");
            }
            let implicit_recursive = files.is_empty();
            if implicit_recursive {
                let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                let has_anchor = project_anchor(&cwd).is_some();
                files = find_silt_files(Path::new("."));
                if files.is_empty() {
                    eprintln!("no .silt files found in current directory");
                    process::exit(1);
                }
                if !has_anchor && !explicit_dot {
                    eprintln!(
                        "silt fmt: refusing to recursively format {} — no project anchor (silt.toml or .git) found",
                        cwd.display()
                    );
                    eprintln!("         pass an explicit `.` or file paths to format anyway.");
                    process::exit(1);
                }
                eprintln!(
                    "silt fmt: no files specified; recursively formatting all .silt files under {}",
                    cwd.display()
                );
                let preview = files.iter().take(5).collect::<Vec<_>>();
                for f in &preview {
                    eprintln!("  {f}");
                }
                if files.len() > preview.len() {
                    eprintln!("  ... ({} more)", files.len() - preview.len());
                }
            }
            if check_mode {
                let mut any_unformatted = false;
                for file in &files {
                    if !check_format(file) {
                        any_unformatted = true;
                    }
                }
                if any_unformatted {
                    process::exit(1);
                }
            } else {
                let mut any_failed = false;
                for file in &files {
                    if let Err(e) = format_file(file) {
                        eprintln!("{e}");
                        any_failed = true;
                    }
                }
                if any_failed {
                    process::exit(1);
                }
            }
        }
        "init" => {
            for arg in &args[2..] {
                if arg == "--help" || arg == "-h" {
                    println!("Usage: silt init");
                    println!();
                    println!("Create a new main.silt file in the current directory.");
                    process::exit(0);
                }
            }
            init_project();
        }
        // If the argument looks like a file, run it directly
        arg if arg.ends_with(".silt") => {
            vm_run_file(arg);
        }
        other => {
            eprintln!("Unknown command: {other}");
            eprintln!("Run 'silt' with no arguments to see available commands.");
            process::exit(1);
        }
    }
}

fn init_project() {
    let path = "main.silt";
    if Path::new(path).exists() {
        eprintln!("main.silt already exists");
        process::exit(1);
    }
    let content = r#"fn main() {
  println("hello, silt!")
}
"#;
    if let Err(e) = fs::write(path, content) {
        eprintln!("error writing {path}: {e}");
        process::exit(1);
    }
    println!("created {path}");
    println!("  run:   silt run main.silt");
    println!("  test:  silt test");
}

fn format_file(path: &str) -> Result<(), String> {
    let source = fs::read_to_string(path).map_err(|e| format!("error reading {path}: {e}"))?;
    let formatted = silt::formatter::format(&source).map_err(|e| format!("{path}: {e}"))?;
    fs::write(path, formatted).map_err(|e| format!("error writing {path}: {e}"))?;
    Ok(())
}

/// Check if a file is already formatted. Returns true if it is, false otherwise.
/// Prints a message for files that would be changed.
fn check_format(path: &str) -> bool {
    let source = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error reading {path}: {e}");
            return false;
        }
    };
    match silt::formatter::format(&source) {
        Ok(formatted) => {
            if source == formatted {
                true
            } else {
                eprintln!("{path}: not formatted");
                false
            }
        }
        Err(e) => {
            eprintln!("{path}: {e}");
            false
        }
    }
}

/// Return true if `e` is the "program has no main function" runtime error.
///
/// AUDIT-NOTE: this hint is keyed on a stringly-typed error; a proper fix
/// would introduce a typed error variant. Tests pinning this live in
/// tests/cli.rs. The matcher is intentionally more permissive than a single
/// exact-string compare so a future cosmetic tweak to the producing
/// `format!` in src/vm/execute.rs doesn't silently break the "silt test"
/// nudge.
fn is_missing_main_error(e: &silt::vm::VmError) -> bool {
    let msg = &e.message;
    msg.starts_with("undefined global: ") && msg.contains("main")
}

/// Heuristic: does this source look like a test-only file?
///
/// Returns true if the source defines any `fn test_...` function OR contains
/// a top-level `test.` call (e.g. `test.assert_eq(...)`). Used by `silt run`
/// to suggest `silt test` when there's no `main()`.
///
/// Conservative: we scan whole lines that start (after trimming whitespace)
/// with `fn test_`, `fn skip_test_`, or `test.` so commented-out code and
/// string literals containing those substrings don't trigger a false positive.
fn looks_like_test_file(source: &str) -> bool {
    for line in source.lines() {
        let t = line.trim_start();
        if t.starts_with("fn test_")
            || t.starts_with("fn skip_test_")
            || t.starts_with("pub fn test_")
            || t.starts_with("pub fn skip_test_")
            || t.starts_with("test.")
        {
            return true;
        }
    }
    false
}

/// Walk upward from `start` looking for a project anchor — `silt.toml` or a
/// `.git` directory. Returns the anchor path if found, else `None`. Used by
/// `silt fmt` to avoid recursively formatting everything when invoked outside
/// any recognisable project.
fn project_anchor(start: &Path) -> Option<PathBuf> {
    let mut cur = start.to_path_buf();
    loop {
        let toml = cur.join("silt.toml");
        if toml.exists() {
            return Some(toml);
        }
        let git = cur.join(".git");
        if git.exists() {
            return Some(git);
        }
        if !cur.pop() {
            return None;
        }
    }
}

/// Recursively find all .silt files in a directory.
fn find_silt_files(dir: &Path) -> Vec<String> {
    let mut results = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return results;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            results.extend(find_silt_files(&path));
        } else {
            let name = path.to_string_lossy().to_string();
            if name.ends_with(".silt") {
                results.push(name);
            }
        }
    }
    results.sort();
    results
}

/// Build a map from bare top-level function name → (file_path, source text)
/// for every module file that `main_path` transitively imports.
///
/// We scan `main_source` (and each imported module's source) for
/// `import <name>` statements, resolve them relative to the main file's
/// project root, and record each top-level `fn <name>` / `pub fn <name>`
/// we find in the resulting module file.
///
/// This is a *best-effort* mapping used solely to improve runtime-error
/// rendering when an error propagates out of an imported module. Name
/// collisions are handled by *exclusion*, not by winner-takes-all:
///
///   1. If a function name is ALSO defined at the top level of the main
///      source file, it is excluded from the map. The renderer then falls
///      back to the main source — which is correct, because the VM's
///      innermost frame name cannot distinguish `main::foo` from
///      `mod::foo`, and the main file is the safer guess.
///   2. If a function name appears in MORE THAN ONE imported module, it
///      is likewise excluded — we have no way to pick the right module.
///
/// In both cases a map miss causes the renderer to fall back to the main
/// source, which is the safe default: at worst the rendered snippet
/// points at main's line N, which is typically close to the call site
/// that invoked the module function.
///
/// See E1 in the audit for the original gap (runtime errors from module
/// code rendered against the main file), and the follow-up collision
/// case (`test_module_runtime_error_with_name_collision_renders_correct_file`)
/// which motivated the exclusion strategy here.
fn collect_module_function_sources(
    main_path: &str,
    main_source: &str,
) -> std::collections::HashMap<String, (PathBuf, String)> {
    use std::collections::{HashMap, HashSet};

    let mut out: HashMap<String, (PathBuf, String)> = HashMap::new();
    let project_root: PathBuf = Path::new(main_path)
        .canonicalize()
        .unwrap_or_else(|_| Path::new(main_path).to_path_buf())
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));

    // Names defined at the top level of the main source file. Any module
    // function sharing one of these names is ambiguous w.r.t. the VM's
    // bare-name call frame, so we exclude it from the map and let the
    // renderer fall back to the main source.
    let main_fn_names: HashSet<String> =
        extract_top_level_fn_names(main_source).into_iter().collect();

    // First pass: walk the import graph, recording every (fn_name,
    // module_file, module_source) tuple we encounter. We can't decide
    // inclusion until we've seen the full graph — a name that appears in
    // one module might also appear in another, in which case it must be
    // excluded from the final map.
    let mut candidates: Vec<(String, PathBuf, String)> = Vec::new();
    let mut name_module_count: HashMap<String, usize> = HashMap::new();

    // BFS from main source: scan import statements, load each module file,
    // repeat for transitive imports.
    let mut queue: Vec<(String, String)> = vec![(main_path.to_string(), main_source.to_string())];
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(main_path.to_string());

    while let Some((_cur_path, cur_source)) = queue.pop() {
        for import_name in extract_imports(&cur_source) {
            // Skip builtin modules — they're not file-backed.
            if silt::module::is_builtin_module(&import_name) {
                continue;
            }
            let file_path = project_root.join(format!("{import_name}.silt"));
            let file_key = file_path.display().to_string();
            if !seen.insert(file_key.clone()) {
                continue;
            }
            let Ok(mod_source) = fs::read_to_string(&file_path) else {
                continue;
            };
            // Per-module dedupe: a function name appearing twice in the
            // SAME file still counts as a single module for collision
            // purposes.
            let mut local_names: HashSet<String> = HashSet::new();
            for fn_name in extract_top_level_fn_names(&mod_source) {
                if local_names.insert(fn_name.clone()) {
                    *name_module_count.entry(fn_name.clone()).or_insert(0) += 1;
                    candidates.push((fn_name, file_path.clone(), mod_source.clone()));
                }
            }
            queue.push((file_key, mod_source));
        }
    }

    // Second pass: build the final map, excluding any name that either
    // collides with main or is defined in more than one module.
    for (fn_name, file_path, mod_source) in candidates {
        if main_fn_names.contains(&fn_name) {
            continue;
        }
        if name_module_count.get(&fn_name).copied().unwrap_or(0) > 1 {
            continue;
        }
        // At this point the name is unique to a single module and not
        // shadowed by the main file, so recording it is unambiguous.
        out.entry(fn_name).or_insert((file_path, mod_source));
    }
    out
}

/// Extract the bare module names referenced by `import <name>` statements
/// in `source`. Supports both `import foo` and `import foo.{ Bar, baz }`
/// forms — we just need the module name, not the item list.
fn extract_imports(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw_line in source.lines() {
        let line = raw_line.trim_start();
        let Some(rest) = line.strip_prefix("import ") else {
            continue;
        };
        // Module name runs to the first `.`, whitespace, `{`, or `as`.
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() {
            out.push(name);
        }
    }
    out
}

/// Extract the names of top-level `fn <name>` (optionally `pub fn`)
/// declarations in `source`. This is a purely textual scan — we only
/// need it to correlate a runtime frame's function name with a module
/// file, so missing an edge case (e.g. an `fn` inside a multi-line
/// comment) just means falling back to the main file for rendering.
fn extract_top_level_fn_names(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw_line in source.lines() {
        let line = raw_line.trim_start();
        let rest = match line.strip_prefix("pub fn ") {
            Some(r) => r,
            None => match line.strip_prefix("fn ") {
                Some(r) => r,
                None => continue,
            },
        };
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() {
            out.push(name);
        }
    }
    out
}

/// Run a file using the bytecode VM (default path).
fn vm_run_file(path: &str) {
    silt::intern::reset();
    let (functions, source) = compile_file(path);

    // Build a name → (module_file, source) map so runtime errors from
    // imported modules are rendered against the correct file.  See
    // `collect_module_function_sources` for the rationale.
    let module_sources = collect_module_function_sources(path, &source);

    let Some(script) = functions.into_iter().next() else {
        eprintln!("{path}: internal error: empty function list");
        process::exit(1);
    };
    let script = Arc::new(script);

    // Run via VM
    let mut vm = Vm::new();
    if let Err(e) = vm.run(script) {
        if let Some(span) = e.span {
            // Determine which source text & file path to render against.
            // Prefer the innermost non-synthetic frame's function name,
            // falling back to the main file when the frame isn't from an
            // imported module.
            let innermost_fn_name: Option<&str> = e
                .call_stack
                .iter()
                .find(|(n, _)| !n.starts_with('<'))
                .map(|(n, _)| n.as_str());
            let (err_source, err_path): (&str, &str) =
                match innermost_fn_name.and_then(|n| module_sources.get(n)) {
                    Some((module_path, module_source)) => {
                        (module_source.as_str(), module_path.to_str().unwrap_or(path))
                    }
                    None => (source.as_str(), path),
                };
            let source_err = SourceError::runtime_at(&e.message, span, err_source, err_path);
            eprintln!("{source_err}");
            // Print call stack if there are user frames beyond the error site.
            // Drop synthetic entry-point frames (<script>, <call:...>) by name
            // rather than by span — a zero-spanned frame inside an otherwise
            // good stack shouldn't cause the whole stack to be discarded.
            let meaningful: Vec<_> = e
                .call_stack
                .iter()
                .filter(|(name, _)| !name.starts_with('<'))
                .collect();
            // Only show the stack if it adds information beyond the error
            // site the user already sees above. A single-frame "stack"
            // would just restate that location, which is noisy.
            let any_real_span = meaningful.iter().any(|(_, s)| s.line > 0);
            if meaningful.len() >= 2 && any_real_span {
                eprintln!("\ncall stack:");
                let head = 10;
                let tail = 5;
                let print_frame = |name: &str, frame_span: &silt::lexer::Span| {
                    // Each frame uses its own function's source file for
                    // file labels — this matters when the call crosses a
                    // module boundary.
                    let frame_path: &str = module_sources
                        .get(name)
                        .map(|(p, _)| p.to_str().unwrap_or(path))
                        .unwrap_or(path);
                    if frame_span.line > 0 {
                        eprintln!(
                            "  -> {}  at {}:{}:{}",
                            name, frame_path, frame_span.line, frame_span.col
                        );
                    } else {
                        eprintln!("  -> {name}  at {frame_path}:<unknown location>");
                    }
                };
                if meaningful.len() <= head + tail {
                    for (name, frame_span) in &meaningful {
                        print_frame(name, frame_span);
                    }
                } else {
                    for (name, frame_span) in &meaningful[..head] {
                        print_frame(name, frame_span);
                    }
                    let omitted = meaningful.len() - head - tail;
                    eprintln!("  ... ({omitted} more frames)");
                    for (name, frame_span) in &meaningful[meaningful.len() - tail..] {
                        print_frame(name, frame_span);
                    }
                }
            }
        } else if is_missing_main_error(&e) {
            // Detect test-only files so we can nudge the user toward `silt test`
            // instead of the generic "add a main()" error. We do a conservative
            // text scan for `fn test_` function definitions or top-level `test.`
            // builtin calls — both strong signals the user meant `silt test`.
            if looks_like_test_file(&source) {
                eprintln!(
                    "{path}: program has no main() function. This looks like a test file — run it with 'silt test {path}' instead."
                );
            } else {
                eprintln!("{path}: program has no main() function — add one as the entry point");
            }
        } else {
            eprintln!("{path}: {e}");
        }
        process::exit(1);
    }
}

/// Disassemble a file's bytecode without running it.
fn disasm_file(path: &str) {
    silt::intern::reset();
    let (functions, _source) = compile_file(path);

    // Print disassembly of each function
    for func in &functions {
        print!("{}", disassemble_function(func));
        println!();
    }
}

fn check_file(path: &str, format: OutputFormat) {
    silt::intern::reset();
    let result = run_compile_pipeline(path, true, true);

    let errors: Vec<&SourceError> = all_diagnostics(&result);

    if format == OutputFormat::Json {
        print_json_errors(&errors);
    } else {
        for err in &errors {
            eprintln!("{err}");
        }
    }

    if result.has_hard_errors {
        process::exit(1);
    }
}

fn print_json_errors(errors: &[&SourceError]) {
    let json_errors: Vec<serde_json::Value> = errors
        .iter()
        .map(|e| {
            serde_json::json!({
                "file": e.file.as_deref().unwrap_or("<unknown>"),
                "line": e.span.line,
                "col": e.span.col,
                "message": e.message,
                "severity": if e.is_warning { "warning" } else { "error" },
                "kind": e.kind.to_string(),
            })
        })
        .collect();
    match serde_json::to_string(&json_errors) {
        Ok(json) => println!("{json}"),
        Err(e) => {
            eprintln!("internal error: failed to serialize diagnostics: {e}");
            process::exit(1);
        }
    }
}

fn find_test_files(dir: &Path) -> Vec<String> {
    let mut results = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return results;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            results.extend(find_test_files(&path));
        } else {
            let name = path.to_string_lossy().to_string();
            if name.ends_with("_test.silt") || name.ends_with(".test.silt") {
                results.push(name);
            }
        }
    }
    results.sort();
    results
}

fn run_tests(file: Option<&str>, filter: Option<String>) {
    silt::intern::reset();
    let paths: Vec<String> = if let Some(f) = file {
        let p = Path::new(f);
        if p.is_dir() {
            // silt test dir/ — find all test files in directory recursively
            find_test_files(p)
        } else {
            // silt test file.silt — single file
            vec![f.to_string()]
        }
    } else {
        // silt test — find all test files in current directory recursively
        find_test_files(Path::new("."))
    };

    if paths.is_empty() {
        println!("no test files found");
        return;
    }

    // When a filter is provided, skip files that can't possibly contain matching tests.
    // We do a quick text scan for `fn test_` / `fn skip_test_` names rather than a full parse.
    let paths: Vec<String> = if let Some(ref filter) = filter {
        paths
            .into_iter()
            .filter(|path| {
                let source = match fs::read_to_string(path) {
                    Ok(s) => s,
                    Err(_) => return true, // keep the file so the error is reported later
                };
                // Scan for function names like `fn test_...` or `fn skip_test_...`
                for line in source.lines() {
                    let trimmed = line.trim_start();
                    if let Some(rest) = trimmed.strip_prefix("fn ") {
                        let name: String = rest
                            .chars()
                            .take_while(|c| c.is_alphanumeric() || *c == '_')
                            .collect();
                        if (name.starts_with("test_") || name.starts_with("skip_test_"))
                            && name.contains(filter.as_str())
                        {
                            return true;
                        }
                    }
                }
                false
            })
            .collect()
    } else {
        paths
    };

    if paths.is_empty() {
        println!("no matching test files found");
        return;
    }

    let mut total = 0;
    let mut passed = 0;
    let mut failed = 0;
    let mut skipped = 0;
    // Count files that failed to lex / parse / type-check / compile.
    // These are tracked separately from the per-test failure counter so
    // that `X tests: Y passed, Z failed` still reflects what actually
    // ran. Previously a single file compile error was booked as one
    // "failed test", which was misleading — that file may have contained
    // dozens of tests we couldn't even count.
    let mut file_errors: usize = 0;

    for path in &paths {
        let source = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{path}: failed to read — {e}");
                file_errors += 1;
                continue;
            }
        };

        let tokens = match Lexer::new(&source).tokenize() {
            Ok(t) => t,
            Err(e) => {
                let source_err = SourceError::from_lex_error(&e, &source, path.as_str());
                eprintln!("{path}: failed to compile — {source_err}");
                file_errors += 1;
                continue;
            }
        };

        let (mut program, parse_errors) = Parser::new(tokens).parse_program_recovering();
        if !parse_errors.is_empty() {
            eprintln!("{path}: failed to compile — parse errors:");
            for e in &parse_errors {
                let source_err = SourceError::from_parse_error(e, &source, path.as_str());
                eprintln!("{source_err}");
            }
            file_errors += 1;
            continue;
        }

        // Type-check before compiling so type errors fail the test.
        let type_errors = typechecker::check(&mut program);
        let mut has_type_error = false;
        for te in &type_errors {
            let source_err = SourceError::from_type_error(te, &source, path);
            eprintln!("{source_err}");
            if te.severity == typechecker::Severity::Error {
                has_type_error = true;
            }
        }
        if has_type_error {
            eprintln!("{path}: failed to compile — type errors (see above)");
            file_errors += 1;
            continue;
        }

        // Compile all declarations (without calling main)
        let test_root = Path::new(path.as_str())
            .canonicalize()
            .unwrap_or_else(|_| Path::new(path.as_str()).to_path_buf())
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));
        let mut compiler = Compiler::with_project_root(test_root);
        let functions = match compiler.compile_declarations(&program) {
            Ok(f) => f,
            Err(e) => {
                let source_err = SourceError::from_compile_error(&e, &source, path);
                eprintln!("{path}: failed to compile — {source_err}");
                file_errors += 1;
                continue;
            }
        };

        // Run the setup script to register all globals in the VM
        let Some(first) = functions.into_iter().next() else {
            eprintln!("{path}: internal error: no functions compiled");
            file_errors += 1;
            continue;
        };
        let script = Arc::new(first);
        let mut vm = Vm::new();
        if let Err(e) = vm.run(script) {
            eprintln!("{path}: setup error: {e}");
            file_errors += 1;
            continue;
        }

        // Run each test function
        for decl in &program.decls {
            if let silt::ast::Decl::Fn(f) = decl {
                let name = silt::intern::resolve(f.name);
                if name.starts_with("skip_test_") {
                    if let Some(ref filter) = filter
                        && !name.contains(filter.as_str())
                    {
                        continue;
                    }
                    total += 1;
                    eprintln!("  SKIP {path}::{name}");
                    skipped += 1;
                    continue;
                }
                if name.starts_with("test_") {
                    if let Some(ref filter) = filter
                        && !name.contains(filter.as_str())
                    {
                        continue;
                    }
                    total += 1;
                    let caller = silt::bytecode::call_global_script(&name);
                    match vm.run(Arc::new(caller)) {
                        Ok(_) => {
                            eprintln!("  PASS {path}::{name}");
                            passed += 1;
                        }
                        Err(e) => {
                            eprintln!("  FAIL {path}::{name}");
                            if let Some(span) = e.span {
                                let source_err =
                                    SourceError::runtime_at(&e.message, span, &source, path);
                                eprintln!("    {source_err}");
                            } else {
                                eprintln!("    Error: {e}");
                            }
                            failed += 1;
                        }
                    }
                }
            }
        }
    }

    if file_errors > 0 {
        eprintln!(
            "\n{total} tests: {passed} passed, {failed} failed, {skipped} skipped ({file_errors} file{} failed to compile)",
            if file_errors == 1 { "" } else { "s" }
        );
    } else {
        eprintln!("\n{total} tests: {passed} passed, {failed} failed, {skipped} skipped");
    }
    if total == 0 && file_errors == 0 {
        eprintln!(
            "hint: test functions must be named 'fn test_*'; test files should end with '_test.silt'"
        );
    }
    if failed > 0 || file_errors > 0 {
        process::exit(1);
    }
}
