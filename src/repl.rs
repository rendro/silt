use std::cell::RefCell;
use std::rc::Rc;

use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::history::DefaultHistory;
use rustyline::validate::Validator;
use rustyline::{Editor, Helper, Context};

use crate::ast::{Decl, Stmt};
use crate::interpreter::Interpreter;
use crate::lexer::Lexer;
use crate::parser::Parser;

const HISTORY_FILE: &str = ".silt_history";

// ── Tab completion helper ───────────────────────────────────────────

struct SiltHelper {
    names: Rc<RefCell<Vec<String>>>,
}

impl Completer for SiltHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        // Find the word being completed (go back from cursor to whitespace/delimiter).
        let start = line[..pos]
            .rfind(|c: char| c.is_whitespace() || c == '(' || c == ',' || c == '|')
            .map(|i| i + 1)
            .unwrap_or(0);
        let prefix = &line[start..pos];

        if prefix.is_empty() {
            return Ok((pos, Vec::new()));
        }

        let names = self.names.borrow();
        let matches: Vec<Pair> = names
            .iter()
            .filter(|n| n.starts_with(prefix))
            .map(|n| Pair {
                display: n.clone(),
                replacement: n.clone(),
            })
            .collect();

        Ok((start, matches))
    }
}

impl Hinter for SiltHelper {
    type Hint = String;
}
impl Highlighter for SiltHelper {}
impl Validator for SiltHelper {}
impl Helper for SiltHelper {}

// ── REPL ────────────────────────────────────────────────────────────

pub fn run_repl() {
    let names = Rc::new(RefCell::new(builtin_names()));
    let helper = SiltHelper { names: names.clone() };

    let mut rl: Editor<SiltHelper, DefaultHistory> = Editor::new().expect("failed to create editor");
    rl.set_helper(Some(helper));
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

                // Update completions with newly defined names
                let mut all = builtin_names();
                all.extend(interp.defined_names());
                *names.borrow_mut() = all;
            }
            Err(ReadlineError::Interrupted) => {
                buffer.clear();
                println!("^C");
            }
            Err(ReadlineError::Eof) => break,
            Err(err) => {
                eprintln!("error: {err}");
                break;
            }
        }
    }

    let _ = rl.save_history(HISTORY_FILE);
}

fn builtin_names() -> Vec<String> {
    let mut names = vec![
        // Keywords / commands
        ":quit", ":help", ":env",
        "fn", "let", "type", "trait", "match", "when", "return",
        "import", "loop", "true", "false",
        // Globals
        "print", "println", "panic", "try",
        "Ok", "Err", "Some", "None", "Stop", "Continue",
        "Message", "Closed", "Empty",
        // Modules
        "list.map", "list.filter", "list.fold", "list.each", "list.find",
        "list.sort", "list.sort_by", "list.reverse", "list.head", "list.tail",
        "list.last", "list.length", "list.contains", "list.append", "list.concat",
        "list.zip", "list.flatten", "list.flat_map", "list.filter_map", "list.any", "list.all",
        "list.get", "list.take", "list.drop", "list.enumerate", "list.group_by",
        "list.fold_until", "list.unfold",
        "string.split", "string.trim", "string.join", "string.length",
        "string.contains", "string.replace", "string.to_upper", "string.to_lower",
        "string.starts_with", "string.ends_with", "string.chars", "string.repeat",
        "string.index_of", "string.slice", "string.pad_left", "string.pad_right",
        "int.parse", "int.abs", "int.min", "int.max", "int.to_float", "int.to_string",
        "float.parse", "float.round", "float.ceil", "float.floor", "float.abs",
        "float.to_string", "float.to_int", "float.min", "float.max",
        "map.get", "map.set", "map.delete", "map.keys", "map.values",
        "map.length", "map.merge", "map.filter", "map.map", "map.entries", "map.from_entries",
        "result.unwrap_or", "result.map_ok", "result.map_err", "result.flatten",
        "result.is_ok", "result.is_err",
        "option.map", "option.unwrap_or", "option.to_result", "option.is_some", "option.is_none",
        "io.read_file", "io.write_file", "io.read_line", "io.inspect", "io.args",
        "math.sqrt", "math.pow", "math.log", "math.log10",
        "math.sin", "math.cos", "math.tan", "math.asin", "math.acos", "math.atan", "math.atan2",
        "math.pi", "math.e",
        "channel.new", "channel.send", "channel.receive", "channel.close",
        "channel.try_send", "channel.try_receive", "channel.select",
        "task.spawn", "task.join", "task.cancel",
        "regex.is_match", "regex.find", "regex.find_all", "regex.split",
        "regex.replace", "regex.replace_all", "regex.captures",
        "json.parse", "json.stringify", "json.pretty",
        "test.assert", "test.assert_eq", "test.assert_ne",
    ];
    names.sort();
    names.into_iter().map(String::from).collect()
}

fn print_help() {
    println!("Commands:");
    println!("  :help, :h    Show this help");
    println!("  :env         Show defined names");
    println!("  :quit, :q    Exit the REPL");
    println!("  <Tab>        Autocomplete builtins and user-defined names");
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
