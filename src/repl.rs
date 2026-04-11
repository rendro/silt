use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::history::DefaultHistory;
use rustyline::validate::Validator;
use rustyline::{Context, Editor, Helper};

use crate::ast::{Decl, Pattern};
use crate::compiler::{CompileError, Compiler};
use crate::errors::SourceError;
use crate::intern;
use crate::lexer::{LexError, Lexer, Span};
use crate::parser::{ParseError, Parser};
use crate::typechecker;
use crate::typechecker::ReplTypeContext;
use crate::value::Value;
use crate::vm::Vm;
use crate::vm::error::render_call_stack;

/// Compute the path to the REPL history file.
///
/// Resolution order (first match wins):
/// 1. `$SILT_HISTORY_FILE` if set — lets tests and power users override.
/// 2. `$XDG_DATA_HOME/silt/history` on Linux/macOS if `XDG_DATA_HOME` is set.
/// 3. `$HOME/.local/share/silt/history` on Linux/macOS.
/// 4. `%APPDATA%\silt\history` on Windows.
/// 5. `None` — caller should skip history load/save gracefully.
///
/// The parent directory is created on a best-effort basis; errors are ignored.
fn history_path() -> Option<PathBuf> {
    // Explicit override wins over everything else.
    if let Ok(p) = std::env::var("SILT_HISTORY_FILE")
        && !p.is_empty()
    {
        let path = PathBuf::from(p);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        return Some(path);
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA")
            && !appdata.is_empty()
        {
            let mut p = PathBuf::from(appdata);
            p.push("silt");
            let _ = std::fs::create_dir_all(&p);
            p.push("history");
            return Some(p);
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME")
            && !xdg.is_empty()
        {
            let mut p = PathBuf::from(xdg);
            p.push("silt");
            let _ = std::fs::create_dir_all(&p);
            p.push("history");
            return Some(p);
        }
        if let Ok(home) = std::env::var("HOME")
            && !home.is_empty()
        {
            let mut p = PathBuf::from(home);
            p.push(".local");
            p.push("share");
            p.push("silt");
            let _ = std::fs::create_dir_all(&p);
            p.push("history");
            return Some(p);
        }
    }

    None
}

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
        // IMPORTANT: `.` is deliberately NOT a word boundary here, so that
        // typing `string.` keeps the full `string.` as the completion
        // prefix and narrows candidates to `string.*` entries. Adding `.`
        // to this set re-introduces the DX2 regression — see
        // `test_repl_completion_filters_on_module_prefix`.
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

    let mut rl: Editor<SiltHelper, DefaultHistory> = match Editor::new() {
        Ok(editor) => editor,
        Err(err) => {
            eprintln!("silt repl: failed to initialize terminal: {err}");
            std::process::exit(1);
        }
    };
    rl.set_helper(Some(helper));
    let history_path = history_path();
    if let Some(ref p) = history_path {
        let _ = rl.load_history(p);
    }

    let mut vm = Vm::new();
    let mut type_ctx = ReplTypeContext::new();

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
                        other if other.starts_with(':') && !other.starts_with("::") => {
                            eprintln!(
                                "unknown REPL command '{other}'; type :help for the list"
                            );
                            continue;
                        }
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

                eval_input(&mut vm, &mut type_ctx, &input, &names);
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

    if let Some(ref p) = history_path {
        let _ = rl.save_history(p);
    }
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
    let mut depth_block_comment = 0i32;
    let mut in_string = false;
    let mut in_triple_string = false;
    let mut backslash_count = 0u32;

    let chars: Vec<char> = input.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        let ch = chars[i];

        // Inside a (possibly nested) block comment: look for closing `-}`
        // while still tracking nested `{-`.
        if depth_block_comment > 0 {
            if ch == '{' && i + 1 < len && chars[i + 1] == '-' {
                depth_block_comment += 1;
                i += 2;
                continue;
            }
            if ch == '-' && i + 1 < len && chars[i + 1] == '}' {
                depth_block_comment -= 1;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }

        // Inside a triple-quoted string: look for closing """
        if in_triple_string {
            if ch == '"' && i + 2 < len && chars[i + 1] == '"' && chars[i + 2] == '"' {
                in_triple_string = false;
                i += 3;
                continue;
            }
            i += 1;
            continue;
        }

        // Inside a regular string: track escapes and look for closing "
        if in_string {
            if ch == '"' && backslash_count.is_multiple_of(2) {
                in_string = false;
            }
            if ch == '\\' {
                backslash_count += 1;
            } else {
                backslash_count = 0;
            }
            i += 1;
            continue;
        }

        // Block comment opening: `{-` (nests, matching the real lexer).
        if ch == '{' && i + 1 < len && chars[i + 1] == '-' {
            depth_block_comment += 1;
            i += 2;
            continue;
        }

        // Skip line comments: -- to end of line
        if ch == '-' && i + 1 < len && chars[i + 1] == '-' {
            // Skip to end of line
            while i < len && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }

        // Check for triple-quoted string opening """
        if ch == '"' && i + 2 < len && chars[i + 1] == '"' && chars[i + 2] == '"' {
            in_triple_string = true;
            i += 3;
            continue;
        }

        // Regular string opening
        if ch == '"' {
            in_string = true;
            backslash_count = 0;
            i += 1;
            continue;
        }

        match ch {
            '{' => depth_brace += 1,
            '}' => depth_brace -= 1,
            '(' => depth_paren += 1,
            ')' => depth_paren -= 1,
            '[' => depth_bracket += 1,
            ']' => depth_bracket -= 1,
            _ => {}
        }
        i += 1;
    }

    depth_brace > 0
        || depth_paren > 0
        || depth_bracket > 0
        || depth_block_comment > 0
        || in_string
        || in_triple_string
}

fn is_declaration(input: &str) -> bool {
    let trimmed = input.trim();
    trimmed.starts_with("fn ")
        || trimmed.starts_with("let ")
        || trimmed.starts_with("type ")
        || trimmed.starts_with("trait ")
        || trimmed.starts_with("import ")
        || trimmed.starts_with("pub ")
}

/// Evaluate a single REPL input.  Declarations are compiled and loaded into
/// the persistent VM.  Expressions are wrapped in a throwaway function,
/// compiled, and run; the result is printed if it is not Unit.
fn eval_input(
    vm: &mut Vm,
    type_ctx: &mut ReplTypeContext,
    input: &str,
    names: &Rc<RefCell<Vec<String>>>,
) {
    if is_declaration(input) {
        eval_declaration(vm, type_ctx, input, names);
    } else {
        eval_expression(vm, type_ctx, input);
    }
}

fn eval_declaration(
    vm: &mut Vm,
    type_ctx: &mut ReplTypeContext,
    input: &str,
    names: &Rc<RefCell<Vec<String>>>,
) {
    let tokens = match Lexer::new(input).tokenize() {
        Ok(t) => t,
        Err(e) => {
            let source_err = SourceError::from_lex_error(&e, input, "<repl>");
            eprintln!("{source_err}");
            return;
        }
    };
    let mut program = match Parser::new(tokens).parse_program() {
        Ok(p) => p,
        Err(e) => {
            let source_err = SourceError::from_parse_error(&e, input, "<repl>");
            eprintln!("{source_err}");
            return;
        }
    };

    // Type-check using the persistent REPL context so that previously
    // defined names are visible to this input.
    let type_errors = type_ctx.check(&mut program);
    for te in &type_errors {
        let source_err = SourceError::from_type_error(te, input, "<repl>");
        eprintln!("{source_err}");
    }
    if type_errors
        .iter()
        .any(|e| e.severity == typechecker::Severity::Error)
    {
        return;
    }

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

    let Some(script) = functions.into_iter().next() else {
        eprintln!("internal error: empty function list");
        return;
    };
    let script = Arc::new(script);
    if let Err(e) = vm.run(script) {
        if let Some(span) = e.span {
            // Declarations are compiled un-wrapped, so the span is already
            // in `input` coordinates — no `adjust_span` needed. But if the
            // span doesn't fit this entry's source it must have come from
            // a chunk compiled in a *previous* REPL entry (e.g. a `let`
            // initializer that calls a prior `fn`). Render the primary
            // location as `<declaration>` in that case, matching the
            // call-stack frame treatment below.
            if span_fits_input(span, input) {
                let source_err = SourceError::runtime_at(&e.message, span, input, "<repl>");
                eprintln!("{source_err}");
            } else {
                eprintln!("error[runtime]: {}", e.message);
                eprintln!(" --> <declaration>");
            }
            // Print the call stack for the non-synthetic frames. Frame line
            // numbers come from the original REPL input buffer, so we label
            // them `<declaration>` rather than print misleading positions.
            for line in render_call_stack(&e.call_stack, |_name, _span| {
                "<declaration>".to_string()
            }) {
                eprintln!("{line}");
            }
        } else {
            eprintln!("{e}");
        }
        return;
    }

    // After successful evaluation, add newly defined names to the completion list.
    let mut new_names = Vec::new();
    for decl in &program.decls {
        match decl {
            Decl::Fn(f) => {
                new_names.push(intern::resolve(f.name));
            }
            Decl::Let { pattern, .. } => {
                collect_pattern_names(pattern, &mut new_names);
            }
            Decl::Type(t) => {
                new_names.push(intern::resolve(t.name));
            }
            Decl::Trait(t) => {
                new_names.push(intern::resolve(t.name));
            }
            _ => {}
        }
    }
    if !new_names.is_empty() {
        let mut names_ref = names.borrow_mut();
        for name in new_names {
            if !names_ref.contains(&name) {
                names_ref.push(name);
            }
        }
    }
}

/// Collect bound names from a pattern (for let bindings).
fn collect_pattern_names(pattern: &Pattern, names: &mut Vec<String>) {
    match pattern {
        Pattern::Ident(sym) => {
            names.push(intern::resolve(*sym));
        }
        Pattern::Tuple(pats) => {
            for p in pats {
                collect_pattern_names(p, names);
            }
        }
        Pattern::Constructor(_, pats) => {
            for p in pats {
                collect_pattern_names(p, names);
            }
        }
        Pattern::Record { fields, .. } => {
            for (field_name, sub) in fields {
                if let Some(p) = sub {
                    collect_pattern_names(p, names);
                } else {
                    // Shorthand field: `{ x }` binds `x`
                    names.push(intern::resolve(*field_name));
                }
            }
        }
        Pattern::List(pats, rest) => {
            for p in pats {
                collect_pattern_names(p, names);
            }
            if let Some(rest_pat) = rest {
                collect_pattern_names(rest_pat, names);
            }
        }
        _ => {} // Wildcard, Int, Float, Bool, StringLit, Or, Range, etc.
    }
}

/// Compile and run `input` as an expression, returning the resulting Value
/// (or an error message). This is the testable core of `eval_expression`;
/// the interactive version adds formatted error reporting and stdout output.
#[cfg(test)]
fn eval_expression_value(
    vm: &mut Vm,
    type_ctx: &mut ReplTypeContext,
    input: &str,
) -> Result<Value, String> {
    let wrapper_prefix = "fn main() {\n";
    let wrapped = format!("{wrapper_prefix}{input}\n}}");
    let tokens = Lexer::new(&wrapped)
        .tokenize()
        .map_err(|e| format!("lex error: {}", e.message))?;
    let mut program = Parser::new(tokens)
        .parse_program()
        .map_err(|e| format!("parse error: {}", e.message))?;
    let type_errors = type_ctx.check(&mut program);
    if let Some(err) = type_errors
        .iter()
        .find(|e| e.severity == typechecker::Severity::Error)
    {
        return Err(format!("type error: {}", err.message));
    }
    let mut compiler = Compiler::new();
    compiler.import_all_builtins();
    let functions = compiler
        .compile_program(&program)
        .map_err(|e| format!("compile error: {}", e.message))?;
    let script = functions
        .into_iter()
        .next()
        .ok_or_else(|| "internal error: empty function list".to_string())?;
    let script = Arc::new(script);
    vm.run(script)
        .map_err(|e| format!("runtime error: {}", e.message))
}

fn eval_expression(vm: &mut Vm, type_ctx: &mut ReplTypeContext, input: &str) {
    // Wrap the expression in a fn main() so the compiler can handle it.
    let wrapper_prefix = "fn main() {\n";
    let wrapped = format!("{wrapper_prefix}{input}\n}}");
    // Total lines in the user's real input (minimum 1), used to clamp errors
    // that land on synthetic tokens past the user's text.
    let input_line_count = input.lines().count().max(1);
    let input_byte_len = input.len();
    // Length (in columns) of the final user-input line, used when clamping
    // past-end errors so the caret points at the end of the last real line
    // instead of column 1 of a synthetic `}`.
    let last_line_cols = input.lines().last().map(|l| l.chars().count()).unwrap_or(0);
    let tokens = match Lexer::new(&wrapped).tokenize() {
        Ok(t) => t,
        Err(e) => {
            let adjusted = adjust_error_span_lex(
                &e,
                wrapper_prefix.len(),
                input_line_count,
                input_byte_len,
                last_line_cols,
            );
            let source_err = SourceError::from_lex_error(&adjusted, input, "<repl>");
            eprintln!("{source_err}");
            return;
        }
    };
    let mut program = match Parser::new(tokens).parse_program() {
        Ok(p) => p,
        Err(e) => {
            let adjusted = adjust_error_span_parse(
                &e,
                wrapper_prefix.len(),
                input_line_count,
                input_byte_len,
                last_line_cols,
            );
            let source_err = SourceError::from_parse_error(&adjusted, input, "<repl>");
            eprintln!("{source_err}");
            return;
        }
    };

    // Type-check using the persistent REPL context so that previously
    // defined names are visible to this input.
    let type_errors = type_ctx.check(&mut program);
    for te in &type_errors {
        let adjusted = adjust_error_span_type(
            te,
            wrapper_prefix.len(),
            input_line_count,
            input_byte_len,
            last_line_cols,
        );
        let source_err = SourceError::from_type_error(&adjusted, input, "<repl>");
        eprintln!("{source_err}");
    }
    if type_errors
        .iter()
        .any(|e| e.severity == typechecker::Severity::Error)
    {
        return;
    }

    // Use compile_program which emits GetGlobal "main"; Call 0; Return
    let mut compiler = Compiler::new();
    compiler.set_repl_mode(true);
    compiler.import_all_builtins();
    let functions = match compiler.compile_program(&program) {
        Ok(f) => f,
        Err(e) => {
            let adjusted = adjust_error_span_compile(
                &e,
                wrapper_prefix.len(),
                input_line_count,
                input_byte_len,
                last_line_cols,
            );
            let source_err = SourceError::from_compile_error(&adjusted, input, "<repl>");
            eprintln!("{source_err}");
            return;
        }
    };

    let Some(script) = functions.into_iter().next() else {
        eprintln!("internal error: empty function list");
        return;
    };
    let script = Arc::new(script);
    match vm.run(script) {
        Ok(val) => {
            if !matches!(val, Value::Unit) {
                println!("{val}");
            }
        }
        Err(e) => {
            if let Some(span) = e.span {
                let adjusted = adjust_span(
                    span,
                    wrapper_prefix.len(),
                    input_line_count,
                    input_byte_len,
                    last_line_cols,
                );
                // If the adjusted span doesn't land on a real character in
                // the current entry's source, it belongs to a chunk that
                // was compiled in an earlier REPL entry (e.g. a `fn a() = 1/0`
                // called from a later `c()`). Printing a caret against
                // *this* entry's text would point into phantom whitespace,
                // so we label the primary location `<declaration>` —
                // matching the treatment the call-stack frames below
                // already apply to prior-entry frames.
                if span_fits_input(adjusted, input) {
                    let source_err =
                        SourceError::runtime_at(&e.message, adjusted, input, "<repl>");
                    eprintln!("{source_err}");
                } else {
                    eprintln!("error[runtime]: {}", e.message);
                    eprintln!(" --> <declaration>");
                }
                // Print the filtered call stack. Frame line numbers come
                // from the wrapped input and don't survive `adjust_span`
                // for anything but the error site itself, so we print
                // function names with a `<declaration>` label rather than
                // misleading file positions.
                for line in render_call_stack(&e.call_stack, |_name, _span| {
                    "<declaration>".to_string()
                }) {
                    eprintln!("{line}");
                }
            } else {
                eprintln!("{e}");
            }
        }
    }
}

/// Test whether `span` points at a real (line, column) position inside
/// `input`. Used to detect runtime-error spans that came from a chunk
/// compiled in a *previous* REPL entry: such spans will have line/col
/// pairs that don't correspond to any character of the current entry's
/// source, because each REPL entry is lexed in its own coordinate space
/// starting from line 1 column 1.
///
/// A span "fits" when:
///   - its line is 1-based and exists in `input`, AND
///   - its column is 1-based and lies within (or one past the end of)
///     that line's character count — one past end is valid because it
///     corresponds to the position right after the last character, e.g.
///     a trailing expected-token caret.
fn span_fits_input(span: Span, input: &str) -> bool {
    if span.line == 0 || span.col == 0 {
        return false;
    }
    let Some(line_text) = input.lines().nth(span.line - 1) else {
        return false;
    };
    let line_chars = line_text.chars().count();
    span.col <= line_chars + 1
}

/// Adjust a span from `wrapped` coordinates to `input` coordinates.
///
/// The wrapper adds one line (`fn main() {\n`) before the user input, so line
/// numbers are off by 1 and byte offsets are off by `prefix_len`. When an
/// error lands on the synthetic closing `}` — i.e. past the last line of the
/// user's real input — we clamp it to the last line (and end-of-line column)
/// so the error pointer stays inside the user's text rather than printing a
/// phantom line.
fn adjust_span(
    span: Span,
    prefix_len: usize,
    input_lines: usize,
    input_bytes: usize,
    last_line_cols: usize,
) -> Span {
    let raw_line = span.line.saturating_sub(1);
    let (line, col) = if raw_line == 0 {
        (1, span.col)
    } else if raw_line > input_lines {
        // Error lands past the user's input (typically on the synthetic `}`).
        // Clamp to the end of the last real line.
        (input_lines, last_line_cols.max(1))
    } else {
        (raw_line, span.col)
    };
    let raw_offset = span.offset.saturating_sub(prefix_len);
    let offset = raw_offset.min(input_bytes);
    Span::with_offset(line, col, offset)
}

fn adjust_error_span_lex(
    e: &LexError,
    prefix_len: usize,
    input_lines: usize,
    input_bytes: usize,
    last_line_cols: usize,
) -> LexError {
    LexError {
        message: e.message.clone(),
        span: adjust_span(e.span, prefix_len, input_lines, input_bytes, last_line_cols),
    }
}

fn adjust_error_span_parse(
    e: &ParseError,
    prefix_len: usize,
    input_lines: usize,
    input_bytes: usize,
    last_line_cols: usize,
) -> ParseError {
    ParseError {
        message: e.message.clone(),
        span: adjust_span(e.span, prefix_len, input_lines, input_bytes, last_line_cols),
    }
}

fn adjust_error_span_compile(
    e: &CompileError,
    prefix_len: usize,
    input_lines: usize,
    input_bytes: usize,
    last_line_cols: usize,
) -> CompileError {
    CompileError {
        message: e.message.clone(),
        span: adjust_span(e.span, prefix_len, input_lines, input_bytes, last_line_cols),
    }
}

fn adjust_error_span_type(
    e: &typechecker::TypeError,
    prefix_len: usize,
    input_lines: usize,
    input_bytes: usize,
    last_line_cols: usize,
) -> typechecker::TypeError {
    typechecker::TypeError {
        message: e.message.clone(),
        span: adjust_span(e.span, prefix_len, input_lines, input_bytes, last_line_cols),
        severity: e.severity,
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

    #[test]
    fn string_with_two_trailing_backslashes_is_closed() {
        // "path\\\\" in Rust source is the string: "path\\" (two backslashes).
        // The final " is unescaped, so the string is closed.
        assert!(!has_unclosed_delimiters(r#""path\\""#));
    }

    #[test]
    fn string_with_one_trailing_backslash_is_open() {
        // "path\\" in Rust source is the string: "path\" — the quote is escaped,
        // so the string is still open.
        assert!(has_unclosed_delimiters(r#""path\"#));
    }

    #[test]
    fn escaped_quote_inside_string_keeps_it_open() {
        // "hello\"" in Rust source is the string: hello" — the inner quote is
        // escaped, and there is no closing quote, so the string is open.
        assert!(has_unclosed_delimiters(r#""hello\""#));
    }

    #[test]
    fn three_trailing_backslashes_string_is_open() {
        // "hello\\\" in Rust source is the string: hello\\\ — three backslashes
        // before the final quote means the quote IS escaped (odd count), so open.
        assert!(has_unclosed_delimiters(r#""hello\\\"#));
    }

    #[test]
    fn four_trailing_backslashes_string_is_closed() {
        // "hello\\\\" in Rust source is the string: hello\\\\ (four backslashes).
        // Even count before the final " means the quote is unescaped — closed.
        assert!(!has_unclosed_delimiters(r#""hello\\\\""#));
    }

    // ── is_declaration ────────────────────────────────────────────

    #[test]
    fn is_declaration_recognizes_fn() {
        assert!(is_declaration("fn foo() {}"));
        assert!(is_declaration("fn add(a, b) { a + b }"));
    }

    #[test]
    fn is_declaration_recognizes_let() {
        assert!(is_declaration("let x = 42"));
    }

    #[test]
    fn is_declaration_recognizes_type_and_trait() {
        assert!(is_declaration("type Color { Red, Green }"));
        assert!(is_declaration("trait Show { fn show(self) -> String }"));
    }

    #[test]
    fn is_declaration_rejects_expression() {
        assert!(!is_declaration("1 + 2"));
        assert!(!is_declaration("foo(42)"));
    }

    #[test]
    fn is_declaration_recognizes_pub() {
        assert!(is_declaration("pub fn foo() {}"));
    }

    // ── Multi-line input continuation ─────────────────────────────
    //
    // The REPL reads lines until `has_unclosed_delimiters` returns false.
    // These tests assert the condition the interactive loop uses to decide
    // whether to keep accumulating input rather than evaluating.

    #[test]
    fn unclosed_brace_continues_input() {
        // `let x = {` on its own should make the REPL prompt for more.
        let buffer = "let x = {";
        assert!(
            has_unclosed_delimiters(buffer),
            "unclosed `{{` should trigger multi-line continuation"
        );
    }

    #[test]
    fn unclosed_bracket_continues_input() {
        let buffer = "let xs = [1, 2,";
        assert!(
            has_unclosed_delimiters(buffer),
            "unclosed `[` should trigger multi-line continuation"
        );
    }

    #[test]
    fn closed_braces_do_not_continue() {
        let buffer = "let x = { 1 }";
        assert!(
            !has_unclosed_delimiters(buffer),
            "balanced `{{}}` should NOT trigger multi-line continuation"
        );
    }

    // ── Expression evaluation ─────────────────────────────────────

    #[test]
    fn eval_simple_arithmetic() {
        let mut vm = Vm::new();
        let mut ctx = ReplTypeContext::new();
        let value = eval_expression_value(&mut vm, &mut ctx, "1 + 2").unwrap();
        assert_eq!(format!("{value}"), "3");
    }

    #[test]
    fn eval_string_literal() {
        let mut vm = Vm::new();
        let mut ctx = ReplTypeContext::new();
        let value = eval_expression_value(&mut vm, &mut ctx, r#""hello""#).unwrap();
        assert_eq!(format!("{value}"), "hello");
    }

    #[test]
    fn eval_bool_expression() {
        let mut vm = Vm::new();
        let mut ctx = ReplTypeContext::new();
        let value = eval_expression_value(&mut vm, &mut ctx, "true").unwrap();
        assert_eq!(format!("{value}"), "true");
    }

    // ── Error recovery ────────────────────────────────────────────
    //
    // A syntax error on one line should not corrupt the persistent REPL
    // state — a valid input on the next turn must still evaluate correctly.

    #[test]
    fn syntax_error_does_not_break_later_input() {
        let mut vm = Vm::new();
        let mut ctx = ReplTypeContext::new();
        // First: garbage input → error.
        let err = eval_expression_value(&mut vm, &mut ctx, "let x =");
        assert!(err.is_err(), "malformed input should return Err");

        // Second: valid input after the error. The VM and type context
        // must still be usable.
        let value = eval_expression_value(&mut vm, &mut ctx, "10 * 10").unwrap();
        assert_eq!(format!("{value}"), "100");
    }

    #[test]
    fn type_error_does_not_crash() {
        let mut vm = Vm::new();
        let mut ctx = ReplTypeContext::new();
        // `1 + "hi"` is a type error.
        let err = eval_expression_value(&mut vm, &mut ctx, r#"1 + "hi""#);
        assert!(err.is_err(), "type error should return Err");
        // Next input must still work.
        let value = eval_expression_value(&mut vm, &mut ctx, "7").unwrap();
        assert_eq!(format!("{value}"), "7");
    }

    // ── adjust_span clamping ──────────────────────────────────────
    //
    // When a parse error lands past the user's input (on the synthetic
    // closing `}` the REPL appends), `adjust_span` clamps the span back
    // to the last column of the last real line — not one past it.

    #[test]
    fn adjust_span_clamps_synthetic_brace_to_last_real_column() {
        // Simulate input `42 +` (4 cols on line 1 of user input), wrapped
        // as `fn main() {\n42 +\n}`. A parse error on the synthetic `}`
        // arrives as line 3 in wrapped coordinates. After adjustment it
        // should sit on the last real column (4, the `+`), not column 5.
        let prefix_len = "fn main() {\n".len();
        let input = "42 +";
        let input_bytes = input.len();
        let last_line_cols = 4; // `+` is column 4
        let wrapped_span = Span::with_offset(3, 1, prefix_len + input_bytes + 1);

        let adjusted = adjust_span(wrapped_span, prefix_len, 1, input_bytes, last_line_cols);

        assert_eq!(adjusted.line, 1, "line should clamp to last real line");
        assert_eq!(
            adjusted.col, 4,
            "col should sit on the last real column (the `+`), not one past it"
        );
        assert_eq!(
            adjusted.offset, input_bytes,
            "offset should clamp to end of real input"
        );
    }

    #[test]
    fn adjust_span_preserves_in_range_spans() {
        // An error on line 1 col 3 of wrapped input maps back to line 1
        // col 3 of user input (line is offset by 1 in wrapped form, but
        // raw_line == 0 falls through to the first arm unchanged).
        let prefix_len = "fn main() {\n".len();
        let input = "foo";
        let wrapped_span = Span::with_offset(2, 3, prefix_len + 2);

        let adjusted = adjust_span(wrapped_span, prefix_len, 1, input.len(), 3);

        assert_eq!(adjusted.line, 1);
        assert_eq!(adjusted.col, 3);
        assert_eq!(adjusted.offset, 2);
    }

    // ── DX2: completion filters on module prefix ──────────────────
    //
    // When the user types a module-qualified prefix like `string.` and
    // hits Tab, the REPL must offer ONLY entries beginning with that
    // module name — never entries from other modules. The key invariant
    // is that `.` is NOT in the word-boundary set used by
    // `SiltHelper::complete`, so the prefix stays `string.` and matches
    // narrow to `string.*`.

    #[test]
    fn test_repl_completion_filters_on_module_prefix() {
        use rustyline::Context;
        use rustyline::completion::Completer;
        use rustyline::history::DefaultHistory;

        let names = Rc::new(RefCell::new(builtin_names()));
        let helper = SiltHelper {
            names: names.clone(),
        };
        let history = DefaultHistory::new();
        let ctx = Context::new(&history);

        // Case 1: `string.` — bare module prefix with trailing dot.
        let line = "string.";
        let (_start, matches) = helper
            .complete(line, line.len(), &ctx)
            .expect("complete must not fail");
        let replacements: Vec<String> = matches.iter().map(|p| p.replacement.clone()).collect();
        assert!(
            !replacements.is_empty(),
            "expected at least one completion for `string.`, got none"
        );
        assert!(
            replacements.iter().any(|r| r == "string.trim"),
            "expected `string.trim` in completions for `string.`, got: {replacements:?}"
        );
        assert!(
            replacements.iter().all(|r| r.starts_with("string.")),
            "all completions for `string.` must begin with `string.`, got: {replacements:?}"
        );
        assert!(
            !replacements.iter().any(|r| r.starts_with("list.")),
            "completions for `string.` must NOT include `list.*`, got: {replacements:?}"
        );

        // Case 2: `string.tr` — narrower prefix; only tr-prefixed suffixes.
        let line = "string.tr";
        let (_start, matches) = helper
            .complete(line, line.len(), &ctx)
            .expect("complete must not fail");
        let replacements: Vec<String> = matches.iter().map(|p| p.replacement.clone()).collect();
        assert!(
            replacements.iter().any(|r| r == "string.trim"),
            "expected `string.trim` in completions for `string.tr`, got: {replacements:?}"
        );
        assert!(
            replacements.iter().all(|r| r.starts_with("string.tr")),
            "all completions for `string.tr` must begin with `string.tr`, got: {replacements:?}"
        );
    }
}
