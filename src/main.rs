use std::env;
use std::fs;
use std::path::Path;
use std::process;

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
            let file = args.get(2).map(|s| s.as_str());
            run_tests(file);
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

    let program = match Parser::new(tokens).parse_program() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{path}:{e}");
            process::exit(1);
        }
    };

    // Run the type checker (warnings only — does not block execution)
    let type_errors = typechecker::check(&program);
    for err in &type_errors {
        eprintln!("{path}:{err}");
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

fn run_tests(file: Option<&str>) {
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
                if f.name.starts_with("test_") {
                    total += 1;
                    match interp.run_test(&f.name) {
                        Ok(()) => {
                            println!("  PASS {path}::{}", f.name);
                            passed += 1;
                        }
                        Err(e) => {
                            println!("  FAIL {path}::{}: {e}", f.name);
                            failed += 1;
                        }
                    }
                }
            }
        }
    }

    println!("\n{total} tests: {passed} passed, {failed} failed");
    if failed > 0 {
        process::exit(1);
    }
}
