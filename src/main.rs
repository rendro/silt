use std::env;
use std::fs;
use std::path::Path;
use std::process;
use std::sync::Arc;

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

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("silt — a statically-typed, expression-based language");
        eprintln!();
        eprintln!("Usage:");
        eprintln!("  silt run [--watch] <file.silt>    Run a program");
        eprintln!("  silt check [--watch] <file.silt>  Type-check without running");
        eprintln!("  silt test [--watch] [path]        Run test functions");
        eprintln!("  silt fmt <file.silt>              Format source code");
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

    match args[1].as_str() {
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
        #[cfg(feature = "repl")]
        "repl" => {
            silt::repl::run_repl();
        }
        "fmt" => {
            if args.len() < 3 {
                eprintln!("Usage: silt fmt <file.silt>");
                process::exit(1);
            }
            format_file(&args[2]);
        }
        "init" => {
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

fn format_file(path: &str) {
    let source = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error reading {path}: {e}");
            process::exit(1);
        }
    };
    match silt::formatter::format(&source) {
        Ok(formatted) => {
            if let Err(e) = fs::write(path, formatted) {
                eprintln!("error writing {path}: {e}");
                process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("{path}: {e}");
            process::exit(1);
        }
    }
}

/// Run a file using the bytecode VM (default path).
fn vm_run_file(path: &str) {
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
            eprintln!("{path}:{e}");
            process::exit(1);
        }
    };

    let (mut program, parse_errors) = Parser::new(tokens).parse_program_recovering();

    if !parse_errors.is_empty() {
        for e in &parse_errors {
            let source_err = SourceError::from_parse_error(e, &source, path);
            eprintln!("{source_err}");
        }
        process::exit(1);
    }

    // Run the type checker
    let type_errors = typechecker::check(&mut program);
    let has_hard_errors = type_errors
        .iter()
        .any(|e| e.severity == typechecker::Severity::Error);
    for err in &type_errors {
        let source_err = SourceError::from_type_error(err, &source, path);
        eprintln!("{source_err}");
    }
    if has_hard_errors {
        process::exit(1);
    }

    // Derive project root from the input file's directory
    let project_root = Path::new(path)
        .canonicalize()
        .unwrap_or_else(|_| Path::new(path).to_path_buf())
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));

    // Compile
    let mut compiler = Compiler::with_project_root(project_root);
    let functions = match compiler.compile_program(&program) {
        Ok(f) => f,
        Err(e) => {
            let source_err = SourceError::from_compile_error(&e, &source, path);
            eprintln!("{source_err}");
            process::exit(1);
        }
    };

    // Print compiler warnings
    for w in compiler.warnings() {
        let source_err = SourceError::compile_warning(&w.message, w.span, &source, path);
        eprintln!("{source_err}");
    }

    let script = Arc::new(functions.into_iter().next().unwrap());

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
                for (name, frame_span) in &meaningful {
                    eprintln!(
                        "  -> {}  at {}:{}:{}",
                        name, path, frame_span.line, frame_span.col
                    );
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
            eprintln!("{path}:{e}");
            process::exit(1);
        }
    };

    let (mut program, parse_errors) = Parser::new(tokens).parse_program_recovering();

    if !parse_errors.is_empty() {
        for e in &parse_errors {
            let source_err = SourceError::from_parse_error(e, &source, path);
            eprintln!("{source_err}");
        }
        process::exit(1);
    }

    // Run the type checker
    let type_errors = typechecker::check(&mut program);
    let has_hard_errors = type_errors
        .iter()
        .any(|e| e.severity == typechecker::Severity::Error);
    for err in &type_errors {
        let source_err = SourceError::from_type_error(err, &source, path);
        eprintln!("{source_err}");
    }
    if has_hard_errors {
        process::exit(1);
    }

    // Derive project root
    let project_root = Path::new(path)
        .canonicalize()
        .unwrap_or_else(|_| Path::new(path).to_path_buf())
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));

    // Compile
    let mut compiler = Compiler::with_project_root(project_root);
    let functions = match compiler.compile_program(&program) {
        Ok(f) => f,
        Err(e) => {
            let source_err = SourceError::from_compile_error(&e, &source, path);
            eprintln!("{source_err}");
            process::exit(1);
        }
    };

    for w in compiler.warnings() {
        let source_err = SourceError::compile_warning(&w.message, w.span, &source, path);
        eprintln!("{source_err}");
    }

    // Print disassembly of each function
    for func in &functions {
        print!("{}", disassemble_function(func));
        println!();
    }
}

fn check_file(path: &str, format: OutputFormat) {
    let source = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error reading {path}: {e}");
            process::exit(1);
        }
    };

    let mut errors: Vec<SourceError> = Vec::new();

    let tokens = match Lexer::new(&source).tokenize() {
        Ok(t) => t,
        Err(e) => {
            if format == OutputFormat::Json {
                let source_err = SourceError::from_lex_error(&e, &source, path);
                errors.push(source_err);
                print_json_errors(&errors);
            } else {
                eprintln!("{path}:{e}");
            }
            process::exit(1);
        }
    };

    let (mut program, parse_errors) = Parser::new(tokens).parse_program_recovering();

    let mut has_parse_errors = false;
    for e in &parse_errors {
        has_parse_errors = true;
        let source_err = SourceError::from_parse_error(e, &source, path);
        errors.push(source_err);
    }

    // Run the type checker even if there were parse errors (on partial program)
    let type_errors = typechecker::check(&mut program);
    let has_hard_errors = has_parse_errors
        || type_errors
            .iter()
            .any(|e| e.severity == typechecker::Severity::Error);
    for err in &type_errors {
        let source_err = SourceError::from_type_error(err, &source, path);
        errors.push(source_err);
    }

    if format == OutputFormat::Json {
        print_json_errors(&errors);
    } else {
        for err in &errors {
            eprintln!("{err}");
        }
    }

    if has_hard_errors {
        process::exit(1);
    }
}

fn print_json_errors(errors: &[SourceError]) {
    let json_errors: Vec<serde_json::Value> = errors
        .iter()
        .map(|e| {
            serde_json::json!({
                "file": e.file.as_deref().unwrap_or("<unknown>"),
                "line": e.span.line,
                "col": e.span.col,
                "message": e.message,
                "severity": if e.is_warning { "warning" } else { "error" },
            })
        })
        .collect();
    println!("{}", serde_json::to_string(&json_errors).unwrap());
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
                eprintln!("{path}:{e}");
                failed += 1;
                continue;
            }
        };

        let program = match Parser::new(tokens).parse_program() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("{path}:{e}");
                failed += 1;
                continue;
            }
        };

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
        let script = Arc::new(functions.into_iter().next().unwrap());
        let mut vm = Vm::new();
        if let Err(e) = vm.run(script) {
            eprintln!("{path}: setup error: {e}");
            failed += 1;
            continue;
        }

        // Run each test function
        for decl in &program.decls {
            if let silt::ast::Decl::Fn(f) = decl {
                if f.name.starts_with("skip_test_") {
                    total += 1;
                    println!("  SKIP {path}::{}", f.name);
                    skipped += 1;
                    continue;
                }
                if f.name.starts_with("test_") {
                    if let Some(ref filter) = filter
                        && !f.name.contains(filter.as_str())
                    {
                        continue;
                    }
                    total += 1;
                    let caller = silt::bytecode::call_global_script(&f.name);
                    match vm.run(Arc::new(caller)) {
                        Ok(_) => {
                            println!("  PASS {path}::{}", f.name);
                            passed += 1;
                        }
                        Err(e) => {
                            println!("  FAIL {path}::{}", f.name);
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
