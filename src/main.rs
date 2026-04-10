use std::env;
use std::fs;
use std::path::Path;
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

/// Print all diagnostics to stderr and exit(1) if there are hard errors.
/// Returns the compiled functions and source on success.
fn compile_file(path: &str) -> (Vec<Function>, String) {
    let result = run_compile_pipeline(path, false, false);

    // Print all diagnostics to stderr.
    for err in all_diagnostics(&result) {
        eprintln!("{err}");
    }

    if result.has_hard_errors {
        process::exit(1);
    }

    let functions = result.functions.unwrap_or_else(|| {
        eprintln!("{path}: internal error: compilation produced no output");
        process::exit(1);
    });

    if functions.is_empty() {
        eprintln!("{path}: internal error: no functions compiled");
        process::exit(1);
    }

    (functions, result.source)
}

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("silt — a statically-typed, expression-based language");
        eprintln!();
        eprintln!("Usage:");
        eprintln!("  silt run [--watch] <file.silt>    Run a program");
        eprintln!("  silt check [--watch] <file.silt>  Type-check without running");
        eprintln!("  silt test [--watch] [path]        Run test functions");
        eprintln!("  silt fmt [--check] [files...]       Format source code");
        eprintln!("  silt repl                         Interactive REPL");
        eprintln!("  silt init                         Create a new main.silt");
        eprintln!("  silt lsp                          Start the language server");
        eprintln!("  silt disasm <file.silt>           Show bytecode disassembly");
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
            println!("silt — a statically-typed, expression-based language");
            println!();
            println!("Usage:");
            println!("  silt run [--watch] <file.silt>    Run a program");
            println!("  silt check [--watch] <file.silt>  Type-check without running");
            println!("  silt test [--watch] [path]        Run test functions");
            println!("  silt fmt [--check] [files...]       Format source code");
            println!("  silt repl                         Interactive REPL");
            println!("  silt init                         Create a new main.silt");
            println!("  silt lsp                          Start the language server");
            println!("  silt disasm <file.silt>           Show bytecode disassembly");
            process::exit(0);
        }
        "run" => {
            if args.len() < 3 {
                eprintln!("Usage: silt run <file.silt>");
                process::exit(1);
            }
            let disasm = args.iter().any(|a| a == "--disassemble");
            let file = args.iter().skip(2).find(|a| !a.starts_with("--")).cloned();
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
                    eprintln!("Usage: silt repl");
                    eprintln!();
                    eprintln!("Start an interactive REPL session. Type :help inside for commands.");
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
                    eprintln!("Usage: silt fmt [--check] [files...]");
                    eprintln!();
                    eprintln!("Options:");
                    eprintln!("  --check    Check formatting without modifying files");
                    process::exit(0);
                } else {
                    files.push(arg.clone());
                }
            }
            // If no files given, find all .silt files in the current directory recursively.
            if files.is_empty() {
                files = find_silt_files(Path::new("."));
                if files.is_empty() {
                    eprintln!("no .silt files found in current directory");
                    process::exit(1);
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
                    eprintln!("Usage: silt init");
                    eprintln!();
                    eprintln!("Create a new main.silt file in the current directory.");
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

/// Run a file using the bytecode VM (default path).
fn vm_run_file(path: &str) {
    silt::intern::reset();
    let (functions, source) = compile_file(path);

    let Some(script) = functions.into_iter().next() else {
        eprintln!("{path}: internal error: empty function list");
        process::exit(1);
    };
    let script = Arc::new(script);

    // Run via VM
    let mut vm = Vm::new();
    if let Err(e) = vm.run(script) {
        if let Some(span) = e.span {
            let source_err = SourceError::runtime_at(&e.message, span, &source, path);
            eprintln!("{source_err}");
            // Print call stack if there are meaningful frames beyond the error site.
            // Filter out synthetic frames like <script> and <call:...>.
            let meaningful: Vec<_> = e
                .call_stack
                .iter()
                .filter(|(name, span)| span.line > 0 && !name.starts_with('<'))
                .collect();
            if meaningful.len() > 1 {
                eprintln!("\ncall stack:");
                let head = 10;
                let tail = 5;
                if meaningful.len() <= head + tail {
                    for (name, frame_span) in &meaningful {
                        eprintln!(
                            "  -> {}  at {}:{}:{}",
                            name, path, frame_span.line, frame_span.col
                        );
                    }
                } else {
                    for (name, frame_span) in &meaningful[..head] {
                        eprintln!(
                            "  -> {}  at {}:{}:{}",
                            name, path, frame_span.line, frame_span.col
                        );
                    }
                    let omitted = meaningful.len() - head - tail;
                    eprintln!("  ... ({omitted} more frames)");
                    for (name, frame_span) in &meaningful[meaningful.len() - tail..] {
                        eprintln!(
                            "  -> {}  at {}:{}:{}",
                            name, path, frame_span.line, frame_span.col
                        );
                    }
                }
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
                        if name.starts_with("test_") && name.contains(filter.as_str()) {
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

    for path in &paths {
        let source = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error reading {path}: {e}");
                failed += 1;
                continue;
            }
        };

        let tokens = match Lexer::new(&source).tokenize() {
            Ok(t) => t,
            Err(e) => {
                let source_err = SourceError::from_lex_error(&e, &source, path.as_str());
                eprintln!("{source_err}");
                failed += 1;
                continue;
            }
        };

        let (mut program, parse_errors) = Parser::new(tokens).parse_program_recovering();
        if !parse_errors.is_empty() {
            for e in &parse_errors {
                let source_err = SourceError::from_parse_error(e, &source, path.as_str());
                eprintln!("{source_err}");
            }
            failed += 1;
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
            failed += 1;
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
                eprintln!("{source_err}");
                failed += 1;
                continue;
            }
        };

        // Run the setup script to register all globals in the VM
        let Some(first) = functions.into_iter().next() else {
            eprintln!("{path}: internal error: no functions compiled");
            failed += 1;
            continue;
        };
        let script = Arc::new(first);
        let mut vm = Vm::new();
        if let Err(e) = vm.run(script) {
            eprintln!("{path}: setup error: {e}");
            failed += 1;
            continue;
        }

        // Run each test function
        for decl in &program.decls {
            if let silt::ast::Decl::Fn(f) = decl {
                let name = silt::intern::resolve(f.name);
                if name.starts_with("skip_test_") {
                    total += 1;
                    println!("  SKIP {path}::{name}");
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
                            println!("  PASS {path}::{name}");
                            passed += 1;
                        }
                        Err(e) => {
                            println!("  FAIL {path}::{name}");
                            if let Some(span) = e.span {
                                let source_err =
                                    SourceError::runtime_at(&e.message, span, &source, path);
                                println!("    {source_err}");
                            } else {
                                println!("    Error: {e}");
                            }
                            failed += 1;
                        }
                    }
                }
            }
        }
    }

    println!("\n{total} tests: {passed} passed, {failed} failed, {skipped} skipped");
    if failed > 0 {
        process::exit(1);
    }
}
