use rustyline::error::ReadlineError;
use rustyline::history::DefaultHistory;
use rustyline::Editor;

use crate::ast::{Decl, Stmt};
use crate::interpreter::Interpreter;
use crate::lexer::Lexer;
use crate::parser::Parser;

const HISTORY_FILE: &str = ".silt_history";

pub fn run_repl() {
    let mut rl: Editor<(), DefaultHistory> = Editor::new().expect("failed to create editor");
    let _ = rl.load_history(HISTORY_FILE);

    let mut interp = Interpreter::new();

    println!("Silt REPL (type :quit to exit, :help for commands)");

    let mut buffer = String::new();

    loop {
        let prompt = if buffer.is_empty() { "silt> " } else { "  ... " };

        match rl.readline(prompt) {
            Ok(line) => {
                let line = line.trim_end();

                if buffer.is_empty() {
                    match line.trim() {
                        ":quit" | ":q" => break,
                        ":help" | ":h" => {
                            print_help();
                            continue;
                        }
                        ":env" => {
                            print_env(&interp);
                            continue;
                        }
                        "" => continue,
                        _ => {}
                    }
                }

                if buffer.is_empty() {
                    buffer = line.to_string();
                } else {
                    buffer.push('\n');
                    buffer.push_str(line);
                }

                if has_unclosed_delimiters(&buffer) {
                    continue;
                }

                let input = buffer.trim().to_string();
                buffer.clear();

                if input.is_empty() {
                    continue;
                }

                let _ = rl.add_history_entry(&input);

                if is_declaration(&input) {
                    eval_declaration(&mut interp, &input);
                } else {
                    eval_expression(&mut interp, &input);
                }
            }
            Err(ReadlineError::Interrupted) => {
                // Ctrl-C: clear current buffer
                buffer.clear();
                println!("^C");
            }
            Err(ReadlineError::Eof) => {
                // Ctrl-D: exit
                break;
            }
            Err(err) => {
                eprintln!("error: {err}");
                break;
            }
        }
    }

    let _ = rl.save_history(HISTORY_FILE);
}

fn print_help() {
    println!("Commands:");
    println!("  :help, :h    Show this help");
    println!("  :env         Show defined names");
    println!("  :quit, :q    Exit the REPL");
    println!();
    println!("Enter expressions to evaluate, or declarations (fn, type, trait, import).");
    println!("Multi-line input: unclosed braces/parens/brackets continue on the next line.");
}

fn print_env(interp: &Interpreter) {
    let names = interp.defined_names();
    if names.is_empty() {
        println!("  (no user-defined names)");
    } else {
        for name in &names {
            println!("  {name}");
        }
    }
}

fn has_unclosed_delimiters(input: &str) -> bool {
    let mut depth_brace = 0i32;
    let mut depth_paren = 0i32;
    let mut depth_bracket = 0i32;
    let mut in_string = false;
    let mut prev = '\0';

    for ch in input.chars() {
        if in_string {
            if ch == '"' && prev != '\\' {
                in_string = false;
            }
            prev = ch;
            continue;
        }
        match ch {
            '"' if prev != '\\' => in_string = true,
            '{' => depth_brace += 1,
            '}' => depth_brace -= 1,
            '(' => depth_paren += 1,
            ')' => depth_paren -= 1,
            '[' => depth_bracket += 1,
            ']' => depth_bracket -= 1,
            _ => {}
        }
        prev = ch;
    }

    depth_brace > 0 || depth_paren > 0 || depth_bracket > 0
}

fn is_declaration(input: &str) -> bool {
    let trimmed = input.trim();
    trimmed.starts_with("fn ")
        || trimmed.starts_with("type ")
        || trimmed.starts_with("trait ")
        || trimmed.starts_with("import ")
        || trimmed.starts_with("pub ")
}

fn eval_declaration(interp: &mut Interpreter, input: &str) {
    let tokens = match Lexer::new(input).tokenize() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("lex error: {e}");
            return;
        }
    };
    let program = match Parser::new(tokens).parse_program() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("parse error: {e}");
            return;
        }
    };
    for decl in &program.decls {
        if let Err(e) = interp.register_decl(decl) {
            eprintln!("{e}");
            return;
        }
    }
}

fn eval_expression(interp: &mut Interpreter, input: &str) {
    let wrapped = format!("fn __repl__() {{\n{input}\n}}");
    let tokens = match Lexer::new(&wrapped).tokenize() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("lex error: {e}");
            return;
        }
    };
    let program = match Parser::new(tokens).parse_program() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("parse error: {e}");
            return;
        }
    };

    let stmts: Vec<Stmt> = program
        .decls
        .into_iter()
        .filter_map(|d| {
            if let Decl::Fn(f) = d {
                if f.name == "__repl__" {
                    if let crate::ast::ExprKind::Block(stmts) = f.body.kind {
                        return Some(stmts);
                    }
                }
            }
            None
        })
        .flatten()
        .collect();

    if stmts.is_empty() {
        return;
    }

    match interp.eval_in_global(&stmts) {
        Ok(val) => {
            if !matches!(val, crate::value::Value::Unit) {
                println!("{val}");
            }
        }
        Err(e) => {
            eprintln!("{e}");
        }
    }
}
