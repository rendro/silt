use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::history::DefaultHistory;
use rustyline::validate::Validator;
use rustyline::{Context, Editor, Helper};

use crate::compiler::Compiler;
use crate::errors::SourceError;
use crate::lexer::Lexer;
use crate::parser::Parser;
use crate::value::Value;
use crate::vm::Vm;

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
    let helper = SiltHelper {
        names: names.clone(),
    };

    let mut rl: Editor<SiltHelper, DefaultHistory> =
        Editor::new().expect("failed to create editor");
    rl.set_helper(Some(helper));
    let _ = rl.load_history(HISTORY_FILE);

    let mut vm = Vm::new();

    println!("Silt REPL (type :quit to exit, :help for commands)");

    let mut buffer = String::new();

    loop {
        let prompt = if buffer.is_empty() {
            "silt> "
        } else {
            "  ... "
        };

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

                eval_input(&mut vm, &input);
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
    let mut names: Vec<String> = vec![
        // Keywords / commands
        ":quit", ":help", "fn", "let", "type", "trait", "match", "when", "return", "import", "loop",
        "true", "false", // Globals
        "print", "println", "panic", "Ok", "Err", "Some", "None", "Stop", "Continue", "Message",
        "Closed", "Empty",
    ]
    .into_iter()
    .map(String::from)
    .collect();

    // Generate module completions from the registry.
    for &module in crate::module::BUILTIN_MODULES {
        for func in crate::module::builtin_module_functions(module) {
            names.push(format!("{module}.{func}"));
        }
        for constant in crate::module::builtin_module_constants(module) {
            names.push(format!("{module}.{constant}"));
        }
    }

    names.sort();
    names
}

fn print_help() {
    println!("Commands:");
    println!("  :help, :h    Show this help");
    println!("  :quit, :q    Exit the REPL");
    println!("  <Tab>        Autocomplete builtins and user-defined names");
    println!();
    println!("Enter expressions to evaluate, or declarations (fn, type, trait, import).");
    println!("Multi-line input: unclosed braces/parens/brackets continue on the next line.");
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

    depth_brace > 0 || depth_paren > 0 || depth_bracket > 0 || in_string
}

fn is_declaration(input: &str) -> bool {
    let trimmed = input.trim();
    trimmed.starts_with("fn ")
        || trimmed.starts_with("type ")
        || trimmed.starts_with("trait ")
        || trimmed.starts_with("import ")
        || trimmed.starts_with("pub ")
}

/// Evaluate a single REPL input.  Declarations are compiled and loaded into
/// the persistent VM.  Expressions are wrapped in a throwaway function,
/// compiled, and run; the result is printed if it is not Unit.
fn eval_input(vm: &mut Vm, input: &str) {
    if is_declaration(input) {
        eval_declaration(vm, input);
    } else {
        eval_expression(vm, input);
    }
}

fn eval_declaration(vm: &mut Vm, input: &str) {
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

    // Compile declarations only (no main call)
    let mut compiler = Compiler::new();
    compiler.import_all_builtins();
    let functions = match compiler.compile_declarations(&program) {
        Ok(f) => f,
        Err(e) => {
            let source_err = SourceError::from_compile_error(&e, input, "<repl>");
            eprintln!("{source_err}");
            return;
        }
    };

    let script = Arc::new(functions.into_iter().next().unwrap());
    if let Err(e) = vm.run(script) {
        eprintln!("{e}");
    }
}

fn eval_expression(vm: &mut Vm, input: &str) {
    // Wrap the expression in a fn main() so the compiler can handle it.
    let wrapped = format!("fn main() {{\n{input}\n}}");
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

    // Use compile_program which emits GetGlobal "main"; Call 0; Return
    let mut compiler = Compiler::new();
    compiler.import_all_builtins();
    let functions = match compiler.compile_program(&program) {
        Ok(f) => f,
        Err(e) => {
            let source_err = SourceError::from_compile_error(&e, &wrapped, "<repl>");
            eprintln!("{source_err}");
            return;
        }
    };

    let script = Arc::new(functions.into_iter().next().unwrap());
    match vm.run(script) {
        Ok(val) => {
            if !matches!(val, Value::Unit) {
                println!("{val}");
            }
        }
        Err(e) => {
            eprintln!("{e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── builtin_names tests ────────────────────────────────────────

    #[test]
    fn builtin_names_non_empty_and_sorted() {
        let names = builtin_names();
        assert!(!names.is_empty(), "builtin_names should not be empty");
        for window in names.windows(2) {
            assert!(
                window[0] <= window[1],
                "builtin_names not sorted: {:?} > {:?}",
                window[0],
                window[1]
            );
        }
    }

    #[test]
    fn builtin_names_contains_module_entries() {
        let names = builtin_names();
        assert!(names.contains(&"list.map".to_string()), "missing list.map");
        assert!(
            names.contains(&"string.split".to_string()),
            "missing string.split"
        );
        assert!(names.contains(&"math.pi".to_string()), "missing math.pi");
    }

    #[test]
    fn builtin_names_contains_keywords() {
        let names = builtin_names();
        for kw in [":quit", "fn", "let"] {
            assert!(names.contains(&kw.to_string()), "missing keyword: {kw}");
        }
    }

    #[test]
    fn builtin_names_contains_globals() {
        let names = builtin_names();
        for g in ["print", "println", "Ok", "None"] {
            assert!(names.contains(&g.to_string()), "missing global: {g}");
        }
    }

    #[test]
    fn builtin_names_no_duplicates() {
        let names = builtin_names();
        let mut seen = std::collections::HashSet::new();
        for name in &names {
            assert!(seen.insert(name), "duplicate entry: {name}");
        }
    }

    // ── has_unclosed_delimiters tests ──────────────────────────────

    #[test]
    fn unclosed_brace() {
        assert!(has_unclosed_delimiters("let x = {"));
    }

    #[test]
    fn balanced_braces() {
        assert!(!has_unclosed_delimiters("let x = {}"));
    }

    #[test]
    fn unclosed_paren() {
        assert!(has_unclosed_delimiters("fn foo("));
    }

    #[test]
    fn balanced_parens() {
        assert!(!has_unclosed_delimiters("fn foo(x)"));
    }

    #[test]
    fn unclosed_bracket() {
        assert!(has_unclosed_delimiters("[1, 2"));
    }

    #[test]
    fn balanced_brackets() {
        assert!(!has_unclosed_delimiters("[1, 2]"));
    }

    #[test]
    fn empty_input() {
        assert!(!has_unclosed_delimiters(""));
    }

    #[test]
    fn complete_statement() {
        assert!(!has_unclosed_delimiters("let x = 1"));
    }

    #[test]
    fn unclosed_string() {
        assert!(has_unclosed_delimiters("\"unclosed string"));
    }

    #[test]
    fn nested_unclosed() {
        assert!(has_unclosed_delimiters("{ ( ["));
    }

    #[test]
    fn nested_balanced() {
        assert!(!has_unclosed_delimiters("{ ( [] ) }"));
    }
}
