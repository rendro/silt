use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::history::DefaultHistory;
use rustyline::validate::Validator;
use rustyline::{Context, Editor, Helper};

use crate::defs::DefTable;
use crate::diagnostic::{Diagnostic, Located, SourceView, render_human};
use crate::intern;
use crate::session::{Config, Entry, LockPolicy, ModuleId, ProjectSetup, Session};
use crate::source::{SourceMap, SourceName, Span};
use crate::typechecker::names::{Binding, Exports, ModuleScope};
use crate::value::Value;
use crate::vm::Vm;

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

    // The failures of spawned tasks that nobody joins are taken and
    // shown when the session ends (`report_task_failures`), rendered for
    // the REPL instead of by the scheduler. Not after each input: the
    // handle may still be bound, and a later input may join or cancel it.
    crate::scheduler::collect_unjoined_failures();
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut repl = Repl::new(ProjectSetup::Discover(cwd));

    println!("Silt REPL (type :quit to exit, :help for commands)");
    for d in repl.project_problems() {
        eprintln!("{}", repl.render(&d));
    }

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
                            eprintln!("unknown REPL command '{other}'; type :help for the list");
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

                let evaluation = repl.eval(&input);
                for d in &evaluation.diagnostics {
                    eprintln!("{}", repl.render(d));
                }
                if let Some(value) = &evaluation.value
                    && !matches!(value, Value::Unit)
                {
                    println!("{value}");
                }
                if evaluation.committed {
                    let mut all = builtin_names();
                    all.extend(evaluation.names);
                    all.sort();
                    all.dedup();
                    *names.borrow_mut() = all;
                }
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

    // Tasks that failed and that no input joined or cancelled. A task
    // that is still running when the session ends is not reported.
    report_task_failures(&repl);

    if let Some(ref p) = history_path {
        let _ = rl.save_history(p);
    }
}

/// A REPL session: one compilation session, to which each input is
/// added as a cell of its own (`<repl:n>`), and the VM that runs the
/// cells. A cell sees what the cells before it that ran define; a cell
/// with an error, static or at run time, is dropped and leaves the
/// session as it was.
pub struct Repl {
    session: Session,
    vm: Vm,
    /// The working directory, against which the files of the session are
    /// named.
    cwd: Option<PathBuf>,
}

/// What evaluating one input gives.
pub struct Evaluation {
    /// Its diagnostics: the static ones, warnings too, and the runtime
    /// error that stopped it.
    pub diagnostics: Vec<Diagnostic>,
    /// The value it returned, when it ran: the value of its statements,
    /// or `()` for declarations.
    pub value: Option<Value>,
    /// Whether it ran to the end, so that later inputs see what it binds.
    pub committed: bool,
    /// When it ran: every name later inputs see, for completion, but
    /// the prelude's (see [`builtin_names`]).
    pub names: Vec<String>,
}

impl Repl {
    /// A REPL session in the project `project`.
    pub fn new(project: ProjectSetup) -> Repl {
        let cwd = match &project {
            ProjectSetup::Discover(dir) | ProjectSetup::Script(dir) => Some(dir.clone()),
            ProjectSetup::None => None,
        };
        Repl {
            session: Session::new(Config {
                project,
                lock: LockPolicy::Update,
                host: Vec::new(),
            }),
            vm: Vm::new(),
            cwd,
        }
    }

    /// The problems of the project the session is in, which no input
    /// reports.
    pub fn project_problems(&mut self) -> Vec<Diagnostic> {
        self.session.project_problems()
    }

    /// Check, compile and run `input` as the next cell.
    pub fn eval(&mut self, input: &str) -> Evaluation {
        let file = self.session.add_cell(input.to_string());
        let analysis = self.session.analyze(file);
        let mut evaluation = Evaluation {
            diagnostics: analysis.diagnostics.clone(),
            value: None,
            committed: false,
            names: Vec::new(),
        };
        if analysis.has_errors() {
            return evaluation;
        }
        let program = match self.session.compile(file, Entry::Cell) {
            Ok(program) => program,
            Err(errors) => {
                evaluation.diagnostics.extend(errors);
                return evaluation;
            }
        };
        evaluation.diagnostics.extend(program.warnings);
        self.vm.load_types(&program.types);
        let script = program
            .functions
            .into_iter()
            .next()
            .expect("a compiled program has a script");
        match self.vm.run(Arc::new(script)) {
            Ok(value) => {
                self.session.commit_cell(file);
                let module = self.session.module_of(file);
                if let Some(analysis) = self.session.module_analysis(module) {
                    let session = &self.session;
                    evaluation.names =
                        scope_completion_names(&analysis.scope, session.defs(), |id| {
                            session.module_analysis(id).map(|a| &a.scope.exports)
                        });
                }
                evaluation.value = Some(value);
                evaluation.committed = true;
            }
            Err(e) => evaluation.diagnostics.push(e.to_diagnostic()),
        }
        evaluation
    }

    /// `d` rendered for the terminal.
    pub fn render(&self, d: &Diagnostic) -> String {
        render_human(
            &ReplFiles {
                sources: self.session.sources(),
                cwd: self.cwd.as_deref(),
            },
            d,
        )
    }
}

/// How the REPL names the files of its diagnostics: a cell as
/// `<repl:n>`, a module file by its path from the working directory.
struct ReplFiles<'a> {
    sources: &'a SourceMap,
    cwd: Option<&'a Path>,
}

impl ReplFiles<'_> {
    /// `path` from the working directory, when it is under it. Module
    /// paths can be canonical (on Windows in the `\\?\` form) while the
    /// working directory is not, so both are canonicalized when the plain
    /// comparison fails.
    fn relative(&self, path: &Path) -> Option<String> {
        let cwd = self.cwd?;
        if let Ok(rel) = path.strip_prefix(cwd) {
            return Some(rel.display().to_string());
        }
        let (path, cwd) = (
            std::fs::canonicalize(path).ok()?,
            std::fs::canonicalize(cwd).ok()?,
        );
        path.strip_prefix(&cwd)
            .ok()
            .map(|rel| rel.display().to_string())
    }
}

impl SourceView for ReplFiles<'_> {
    fn locate(&self, span: Span) -> Option<Located> {
        let file = match &self.sources.get(span.file)?.path {
            SourceName::Path(p) | SourceName::Overlay(p) | SourceName::Manifest(p) => {
                self.relative(p).unwrap_or_else(|| p.display().to_string())
            }
            other => crate::diagnostic::source_name_for_display(other)?,
        };
        Some(Located {
            file,
            position: self.sources.position(span),
        })
    }
}

/// Report on stderr the spawned tasks that have failed so far and that
/// nobody joined or cancelled. The REPL calls it when the session ends:
/// until then a handle may still be bound, and an input may join or
/// cancel it.
fn report_task_failures(repl: &Repl) {
    let taken = crate::scheduler::take_unjoined_failures();
    for failure in &taken.failures {
        eprintln!("{}", repl.render(&failure.report_error().to_diagnostic()));
    }
    for (_, count) in &taken.not_kept {
        eprintln!(
            "{}",
            repl.render(&Diagnostic::error(
                crate::diagnostic::Code::UnjoinedTaskFailure,
                Span::BUILTIN,
                crate::scheduler::UnjoinedFailures::not_kept_message(*count),
            ))
        );
    }
}

/// The names `<Tab>` offers before any input: the REPL's commands, the
/// keywords and the prelude's names. A builtin module's members are
/// offered once it is imported (see [`scope_completion_names`]).
pub fn builtin_names() -> Vec<String> {
    let mut names: Vec<String> = vec![":quit", ":q", ":help", ":h"]
        .into_iter()
        .map(String::from)
        .collect();
    // Language keywords + reserved-word-shaped literals, from the lexer.
    // Parity lock: `tests/cli/repl_keyword_parity_with_lexer_tests.rs`.
    for kw in crate::lexer::KEYWORDS {
        names.push((*kw).to_string());
    }
    for kw in crate::lexer::KEYWORD_LITERALS {
        names.push((*kw).to_string());
    }
    let (_, scopes) = crate::typechecker::names::builtins();
    for name in scopes
        .prelude
        .values
        .keys()
        .chain(scopes.prelude.types.keys())
    {
        names.push(intern::resolve(*name));
    }
    names.sort();
    names.dedup();
    names
}

/// The names an input sees after it ran, as the session's scope of the
/// input binds them: what it and the earlier inputs declared, the items
/// they imported, and for each module bound by an import (`import list`,
/// `import list as l`) its members, `l.map`, and for each enum its
/// variants, `C.Red`, `time.Weekday.Monday`. A variant name that is ambiguous for the next input
/// (the input's own `type D { Red }` beside an earlier `type C { Red }`)
/// and a name of a module that failed to load are left out.
pub fn scope_completion_names<'a>(
    scope: &ModuleScope,
    defs: &DefTable,
    exports_of: impl Fn(ModuleId) -> Option<&'a Exports>,
) -> Vec<String> {
    let (_, builtin_scopes) = crate::typechecker::names::builtins();
    let mut names = Vec::new();
    for (name, binding) in scope
        .values
        .iter()
        .chain(&scope.types)
        .chain(scope.implied())
    {
        match binding {
            Binding::Def(_)
                if matches!(scope.exports.values.get(name), Some(Binding::Ambiguous(_))) => {}
            Binding::Def(id) => {
                names.push(intern::resolve(*name));
                for v in defs.variants(*id) {
                    names.push(format!("{name}.{}", defs.get(*v).name));
                }
            }
            Binding::Module(id) => {
                names.push(intern::resolve(*name));
                let exports = if id.is_builtin() {
                    builtin_scopes.modules.get(id)
                } else {
                    exports_of(*id)
                };
                for member in exports
                    .into_iter()
                    .flat_map(|e| e.values.keys().chain(e.types.keys()))
                {
                    names.push(format!("{name}.{member}"));
                }
                // An enum of the module offers its variants:
                // `time.Weekday.Monday`.
                for (ty, binding) in exports.into_iter().flat_map(|e| &e.types) {
                    if let Binding::Def(ty_id) = binding {
                        for v in defs.variants(*ty_id) {
                            names.push(format!("{name}.{ty}.{}", defs.get(*v).name));
                        }
                    }
                }
            }
            Binding::Ambiguous(_) | Binding::Poisoned => {}
        }
    }
    names.sort();
    names.dedup();
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

/// Completion candidates for a given prefix, using the REPL's builtin name
/// list. Mirrors the logic inside `SiltHelper::complete` but takes just a
/// prefix string so integration tests can exercise it without building a
/// full `rustyline` editor. Intentionally does not re-implement the
/// word-boundary search — callers pass the already-extracted prefix.
pub fn completion_candidates_for_prefix(prefix: &str) -> Vec<String> {
    if prefix.is_empty() {
        return Vec::new();
    }
    builtin_names()
        .into_iter()
        .filter(|n| n.starts_with(prefix))
        .collect()
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
    fn builtin_names_hold_no_module_member_or_module_variant() {
        let names = builtin_names();
        for absent in [
            "list.map",
            "string.split",
            "math.pi",
            "Message",
            "Monday",
            "GET",
        ] {
            assert!(!names.contains(&absent.to_string()), "offered {absent}");
        }
        for present in ["Int", "Option", "Some", "None", "Ok", "Err", "println"] {
            assert!(names.contains(&present.to_string()), "missing {present}");
        }
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

    // ── Evaluation ────────────────────────────────────────────────

    fn repl() -> Repl {
        Repl::new(ProjectSetup::None)
    }

    /// The value `input` returns, which must run.
    fn value(repl: &mut Repl, input: &str) -> String {
        let evaluation = repl.eval(input);
        let messages: Vec<&str> = evaluation
            .diagnostics
            .iter()
            .map(|d| d.message.as_str())
            .collect();
        assert!(evaluation.committed, "`{input}` must run: {messages:?}");
        format!(
            "{}",
            evaluation.value.expect("a committed cell has a value")
        )
    }

    /// The error messages of `input`, which must not run.
    fn errors(repl: &mut Repl, input: &str) -> Vec<String> {
        let evaluation = repl.eval(input);
        assert!(!evaluation.committed, "`{input}` must fail");
        evaluation
            .diagnostics
            .iter()
            .filter(|d| d.is_error())
            .map(|d| d.message.clone())
            .collect()
    }

    #[test]
    fn eval_simple_values() {
        let mut repl = repl();
        assert_eq!(value(&mut repl, "1 + 2"), "3");
        assert_eq!(value(&mut repl, r#""hello""#), "hello");
        assert_eq!(value(&mut repl, "true"), "true");
    }

    #[test]
    fn a_field_of_an_earlier_record_value() {
        let mut repl = repl();
        value(&mut repl, "type Point { x: Int, y: Int }");
        value(&mut repl, "let p = Point { x: 42, y: 7 }");
        assert_eq!(value(&mut repl, "p.x"), "42");
    }

    #[test]
    fn a_call_of_a_function_field_of_an_earlier_record_value() {
        let mut repl = repl();
        value(&mut repl, "type P { d: Fn(Int) -> Int }");
        value(&mut repl, "let p = P { d: { x -> x + 1 } }");
        assert_eq!(value(&mut repl, "p.d(5)"), "6");
    }

    #[test]
    fn an_earlier_import_stays() {
        let mut repl = repl();
        value(&mut repl, "import list");
        assert_eq!(value(&mut repl, "list.sum([1, 2, 3])"), "6");
        value(&mut repl, "import string.{ to_upper }");
        assert_eq!(value(&mut repl, r#"to_upper("a")"#), "A");
    }

    #[test]
    fn an_inferred_return_type_is_seen_by_later_cells() {
        let mut repl = repl();
        value(&mut repl, "fn f() { 9 }");
        value(&mut repl, "fn g() { f() + 1 }");
        value(&mut repl, "let h = g()");
        assert_eq!(value(&mut repl, "h"), "10");
    }

    #[test]
    fn an_inferred_polymorphic_function_stays_polymorphic() {
        let mut repl = repl();
        value(&mut repl, "fn id(x) { x }");
        assert_eq!(value(&mut repl, "id(5)"), "5");
        assert_eq!(value(&mut repl, r#"id("hi")"#), "hi");
    }

    #[test]
    fn variants_of_an_earlier_enum_qualified_and_bare() {
        let mut repl = repl();
        value(&mut repl, "type S { C(Int) }");
        value(&mut repl, "let c = S.C(1)");
        assert_eq!(value(&mut repl, "match c { S.C(n) -> n }"), "1");
        value(&mut repl, "type Color { Red, Green }");
        value(&mut repl, "let col = Color.Red");
        assert_eq!(
            value(&mut repl, "match col { Color.Red -> 1, Green -> 2 }"),
            "1"
        );
    }

    #[test]
    fn an_error_leaves_the_session_as_it_was() {
        let mut repl = repl();
        value(&mut repl, "let y = 1");
        assert!(!errors(&mut repl, "let x =").is_empty());
        assert!(!errors(&mut repl, "1 + 2.5").is_empty());
        // A redefinition with a type error does not replace `y`.
        assert!(!errors(&mut repl, r#"let y = 1 + "a""#).is_empty());
        assert_eq!(value(&mut repl, "y"), "1");
        // Nor does one that fails when it runs.
        assert!(!errors(&mut repl, "let y = 1 / 0").is_empty());
        assert_eq!(value(&mut repl, "y + 1"), "2");
        // A cell that failed binds nothing.
        assert!(!errors(&mut repl, "let z = 1 / 0").is_empty());
        let messages = errors(&mut repl, "z");
        assert!(
            messages.iter().any(|m| m.contains("'z'")),
            "`z` must be undefined: {messages:?}"
        );
    }

    #[test]
    fn a_redefinition_is_early_bound() {
        let mut repl = repl();
        value(&mut repl, "fn f() { 1 }");
        value(&mut repl, "fn g() { f() }");
        value(&mut repl, "let call_f = { -> f() }");
        value(&mut repl, r#"fn f() { "two" }"#);
        assert_eq!(value(&mut repl, "g()"), "1");
        assert_eq!(value(&mut repl, "call_f()"), "1");
        assert_eq!(value(&mut repl, "f()"), "two");
        // A `let` too, with a new type.
        value(&mut repl, "let x = 1");
        value(&mut repl, "fn get_x() { x }");
        value(&mut repl, r#"let x = "s""#);
        assert_eq!(value(&mut repl, "get_x() + 1"), "2");
        assert_eq!(value(&mut repl, "x"), "s");
        // A `let` that binds a name again reads its old value.
        value(&mut repl, "let n = 10");
        value(&mut repl, "let n = n + 1");
        assert_eq!(value(&mut repl, "n"), "11");
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

        let mut repl = Repl::new(ProjectSetup::None);
        let evaluation = repl.eval("import string");
        assert!(evaluation.committed);
        let mut all = builtin_names();
        all.extend(evaluation.names);
        let names = Rc::new(RefCell::new(all));
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
