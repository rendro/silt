use std::io::{self, BufRead, Write};

use crate::ast::{Decl, Stmt};
use crate::interpreter::Interpreter;
use crate::lexer::Lexer;
use crate::parser::Parser;

pub fn run_repl() {
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let mut interp = Interpreter::new();

    println!("Silt REPL (type :quit to exit)");

    let mut buffer = String::new();
    let mut prompt = "silt> ";

    loop {
        print!("{prompt}");
        io::stdout().flush().ok();

        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {
                let line = line.trim_end_matches('\n').trim_end_matches('\r');

                if buffer.is_empty() && line.trim() == ":quit" {
                    break;
                }

                if buffer.is_empty() {
                    buffer = line.to_string();
                } else {
                    buffer.push('\n');
                    buffer.push_str(line);
                }

                // Check for unclosed delimiters
                if has_unclosed_delimiters(&buffer) {
                    prompt = "  ... ";
                    continue;
                }

                let input = buffer.trim().to_string();
                buffer.clear();
                prompt = "silt> ";

                if input.is_empty() {
                    continue;
                }

                if is_declaration(&input) {
                    eval_declaration(&mut interp, &input);
                } else {
                    eval_expression(&mut interp, &input);
                }
            }
            Err(err) => {
                eprintln!("error: {err}");
                break;
            }
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
    // Wrap expression in a function so it parses as statements
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

    // Extract the body statements from the wrapper function
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
