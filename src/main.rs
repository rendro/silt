use std::env;
use std::fs;
use std::path::Path;
use std::process;

use silt::errors::SourceError;
use silt::interpreter::Interpreter;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: silt run <file.silt>");
        eprintln!("       silt test [file.silt]");
        process::exit(1);
    }

    match args[1].as_str() {
        "run" => {
            if args.len() < 3 {
                eprintln!("Usage: silt run <file.silt>");
                process::exit(1);
            }
            run_file(&args[2]);
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
        // If the argument looks like a file, run it directly
        arg if arg.ends_with(".silt") => {
            run_file(arg);
        }
        other => {
            eprintln!("Unknown command: {other}");
            eprintln!("Usage: silt run <file.silt>");
            process::exit(1);
        }
    }
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

fn run_file(path: &str) {
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

    let mut program = match Parser::new(tokens).parse_program() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{path}:{e}");
            process::exit(1);
        }
    };

    // Run the type checker
    let type_errors = typechecker::check(&mut program);
    let has_hard_errors = type_errors.iter().any(|e| e.severity == typechecker::Severity::Error);
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

    let mut interp = Interpreter::with_project_root(project_root);
    if let Err(e) = interp.run(&program) {
        eprintln!("{e}");
        process::exit(1);
    }
}

fn run_tests(file: Option<&str>, filter: Option<String>) {
    let paths: Vec<String> = if let Some(f) = file {
        vec![f.to_string()]
    } else {
        // Find all *_test.silt files in current directory
        match fs::read_dir(".") {
            Ok(entries) => entries
                .filter_map(|e| e.ok())
                .map(|e| e.path().to_string_lossy().to_string())
                .filter(|p| p.ends_with("_test.silt") || p.ends_with(".test.silt"))
                .collect(),
            Err(e) => {
                eprintln!("error reading directory: {e}");
                process::exit(1);
            }
        }
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

        // Find all test_ functions — derive project root from the test file path
        let test_root = Path::new(path.as_str())
            .canonicalize()
            .unwrap_or_else(|_| Path::new(path.as_str()).to_path_buf())
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));
        let mut interp = Interpreter::with_project_root(test_root);
        if let Err(e) = interp.run_test_setup(&program) {
            eprintln!("{path}: setup error: {e}");
            failed += 1;
            continue;
        }

        for decl in &program.decls {
            if let silt::ast::Decl::Fn(f) = decl {
                if f.name.starts_with("skip_test_") {
                    total += 1;
                    println!("  SKIP {path}::{}", f.name);
                    skipped += 1;
                    continue;
                }
                if f.name.starts_with("test_") {
                    if let Some(ref filter) = filter {
                        if !f.name.contains(filter.as_str()) {
                            continue;
                        }
                    }
                    total += 1;
                    match interp.run_test(&f.name) {
                        Ok(()) => {
                            println!("  PASS {path}::{}", f.name);
                            passed += 1;
                        }
                        Err(e) => {
                            println!("  FAIL {path}::{}", f.name);
                            println!("    Error: {e}");
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
