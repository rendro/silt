use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::history::DefaultHistory;
use rustyline::validate::Validator;
use rustyline::{Context, Editor, Helper};

use crate::builtins::registry::docs::signature_block;
use crate::defs::DefTable;
use crate::diagnostic::{Diagnostic, Located, SourceView, render_human};
use crate::intern;
use crate::lexer::Lexer;
use crate::session::{Config, Entry, LockPolicy, ModuleId, ProjectSetup, Session};
use crate::source::{FileId, SourceMap, SourceName, Span};
use crate::typechecker::names::{Binding, Exports, ModuleScope};
use crate::types::{Pred, RigidId, Scheme, TyVar, Type, free_vars_in, substitute_vars};
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
    /// The session, asked for its names when `<Tab>` is pressed: an
    /// entry does not pay for a list that nobody may ask for.
    repl: Rc<RefCell<Repl>>,
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

        let matches: Vec<Pair> = (self.repl.borrow().completion_names().into_iter())
            .filter(|n| n.starts_with(prefix))
            .map(|n| Pair {
                display: n.clone(),
                replacement: n,
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
    let cwd = std::env::current_dir().unwrap_or_default();
    let repl = Rc::new(RefCell::new(Repl::new(ProjectSetup::Discover(cwd))));
    let helper = SiltHelper { repl: repl.clone() };

    let mut rl: Editor<SiltHelper, DefaultHistory> = match Editor::new() {
        Ok(editor) => editor,
        Err(err) => {
            eprintln!("silt repl: failed to initialize terminal: {err}");
            crate::scheduler::exit_with_status(1);
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

    println!("Silt REPL (type :quit to exit, :help for commands)");
    let problems = repl.borrow_mut().project_problems();
    for d in problems {
        eprintln!("{}", repl.borrow().render(&d));
    }

    let mut buffer = String::new();
    // Whether the entry being read is open because a delimiter, a
    // string or a block comment of it is (not because a line ended in
    // an operator).
    let mut delimiter_open = false;
    // Whether the entry being read is the expression of a `:type`
    // command, which more lines can finish as they can any entry.
    let mut typing = false;

    loop {
        let prompt = if buffer.is_empty() {
            "silt> "
        } else {
            "  ... "
        };

        // (A panic of the helper, which the editor calls while it holds
        // the terminal, unwinds through the editor: the terminal is put
        // back before the process ends.)
        match crate::scheduler::unwinding(|| rl.readline(prompt)) {
            Ok(line) => {
                let mut line = line.trim_end();

                if buffer.is_empty() {
                    match line.trim() {
                        ":quit" | ":q" => break,
                        ":help" | ":h" => {
                            print_help();
                            continue;
                        }
                        "" => continue,
                        other if command(other, ":type") == Some("") => {
                            show_type(&mut rl, &mut repl.borrow_mut(), "");
                            continue;
                        }
                        other if command(other, ":type").is_some() => {
                            typing = true;
                            line = command(other, ":type").unwrap_or_default();
                        }
                        other if command(other, ":doc").is_some() => {
                            let _ = rl.add_history_entry(other);
                            show_doc(command(other, ":doc").unwrap_or_default());
                            continue;
                        }
                        other if other.starts_with(':') && !other.starts_with("::") => {
                            crate::scheduler::report(|| {
                                eprintln!("unknown REPL command '{other}'; type :help for the list")
                            });
                            continue;
                        }
                        _ => {}
                    }
                }

                // An entry that is being read was open at the end of the
                // line before. A line without a closer and without a
                // quote leaves it open: it is not lexed again for such a
                // line, so a list of thousands of lines that is pasted
                // is read once, at its last line.
                let still_open = delimiter_open && !line.contains([')', ']', '}', '"']);
                if buffer.is_empty() {
                    buffer = line.to_string();
                } else {
                    buffer.push('\n');
                    buffer.push_str(line);
                }
                if still_open {
                    continue;
                }

                // An input that more lines can finish (an open
                // delimiter of code, a string or a block comment that
                // is not closed, or a last line that ends in an
                // operator) goes on in the next line: the lexer says so.
                let lexed = Lexer::new(FileId::default(), &buffer).tokenize();
                delimiter_open = lexed.ends_open();
                if delimiter_open || lexed.ends_in_operator() {
                    continue;
                }

                let input = std::mem::take(&mut buffer);
                match std::mem::take(&mut typing) {
                    true => show_type(&mut rl, &mut repl.borrow_mut(), input.trim()),
                    false => evaluate(&mut rl, &mut repl.borrow_mut(), input.trim()),
                }
            }
            Err(ReadlineError::Interrupted) => {
                buffer.clear();
                delimiter_open = false;
                typing = false;
                println!("^C");
            }
            // The input ends: an entry that was still being read is
            // evaluated (or typed) as it is, and says what it lacks.
            Err(ReadlineError::Eof) => {
                let input = std::mem::take(&mut buffer);
                match typing {
                    true => show_type(&mut rl, &mut repl.borrow_mut(), input.trim()),
                    false => evaluate(&mut rl, &mut repl.borrow_mut(), input.trim()),
                }
                break;
            }
            Err(err) => {
                let text = format!("error: {err}");
                crate::scheduler::report(|| eprintln!("{text}"));
                break;
            }
        }
    }

    // Tasks that failed and that no input joined or cancelled. A task
    // that is still running when the session ends is not reported.
    report_task_failures(&repl.borrow());

    if let Some(ref p) = history_path {
        let _ = rl.save_history(p);
    }
}

/// What follows the command `name` on the line `line` (`:type 1 + 2`
/// for `:type`: `1 + 2`), if the line is that command.
fn command<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(name)?;
    match rest.chars().next() {
        None => Some(""),
        Some(c) if c.is_whitespace() => Some(rest.trim()),
        Some(_) => None,
    }
}

/// `:type input`: the diagnostics of the expression `input` and its
/// type.
fn show_type(rl: &mut Editor<SiltHelper, DefaultHistory>, repl: &mut Repl, input: &str) {
    let _ = rl.add_history_entry(format!(":type {input}").trim_end());
    if input.is_empty() {
        crate::scheduler::report(|| eprintln!(":type needs an expression, as in :type [1, 2]"));
        return;
    }
    let Ok(typed) = repl.type_of(input) else {
        crate::scheduler::report(|| eprintln!(":type takes an expression, not a declaration"));
        return;
    };
    let errors: Vec<String> = typed.diagnostics.iter().map(|d| repl.render(d)).collect();
    crate::scheduler::report(|| {
        for error in &errors {
            eprintln!("{error}");
        }
        if let Some(ty) = &typed.ty {
            println!("{ty}");
        }
    });
}

/// `:doc name`: what [`doc_text`] says of the builtin `name`.
fn show_doc(name: &str) {
    crate::scheduler::report(|| match (name, doc_text(name)) {
        ("", _) => eprintln!(":doc needs a name, as in :doc list.map"),
        (_, Some(text)) => println!("{text}"),
        (_, None) => eprintln!("no builtin named '{name}'"),
    });
}

/// Evaluate the entry `input`, and show its diagnostics and its value.
fn evaluate(rl: &mut Editor<SiltHelper, DefaultHistory>, repl: &mut Repl, input: &str) {
    if input.is_empty() {
        return;
    }
    let _ = rl.add_history_entry(input);
    let evaluation = repl.eval(input);
    let errors: Vec<String> = (evaluation.diagnostics.iter())
        .map(|d| repl.render(d))
        .collect();
    let shown = match &evaluation.value {
        Some(value) if !matches!(value, Value::Unit) => Some(repl.show(value)),
        _ => None,
    };
    // (What an entry was told by a runtime that has panicked is not
    // its error: the process is ending.)
    crate::scheduler::report(|| {
        for error in &errors {
            eprintln!("{error}");
        }
        if let Some(shown) = &shown {
            println!("{shown}");
        }
    });
}

/// What `:type` found: the diagnostics of the expression, warnings too,
/// and its type, when it has no error.
pub struct Typed {
    pub diagnostics: Vec<Diagnostic>,
    pub ty: Option<String>,
}

/// `:type` was given declarations, not an expression.
#[derive(Debug)]
pub struct NotAnExpression;

/// The type `scheme` stands for as `:type` writes it: its type variables
/// as letters, in the order they first appear, and what it asks of them
/// (`Fn(List(a)) -> List(a) where a: Compare`), the bounds of one
/// variable joined by `+` as the reference pages write them (`where a:
/// Equal + Display`). A variable the scheme does not quantify is `_`, as
/// in a diagnostic.
pub fn written_scheme(scheme: &Scheme) -> String {
    let letters: HashMap<TyVar, Type> = free_vars_in(&scheme.ty)
        .into_iter()
        .filter(|var| scheme.vars.contains(var))
        .zip(b'a'..=b'z')
        .map(|(var, letter)| {
            let name = intern::intern(&char::from(letter).to_string());
            (var, Type::Rigid(RigidId { var, name }))
        })
        .collect();
    let mut text = substitute_vars(&scheme.ty, &letters).to_string();
    // Each lettered variable's bounds, in the order of the letters.
    let mut bounds: Vec<(String, Vec<String>)> = Vec::new();
    for pred in &scheme.preds {
        let Pred::Trait {
            tr, args, subject, ..
        } = pred
        else {
            continue;
        };
        let subject = substitute_vars(subject, &letters);
        if !matches!(subject, Type::Rigid(_)) {
            continue;
        }
        let mut bound = tr.to_string();
        if !args.is_empty() {
            let args: Vec<String> = (args.iter())
                .map(|arg| substitute_vars(arg, &letters).to_string())
                .collect();
            bound.push_str(&format!("({})", args.join(", ")));
        }
        let subject = subject.to_string();
        match bounds.iter_mut().find(|(s, _)| *s == subject) {
            Some((_, traits)) if traits.contains(&bound) => {}
            Some((_, traits)) => traits.push(bound),
            None => bounds.push((subject, vec![bound])),
        }
    }
    bounds.sort_by(|(a, _), (b, _)| a.cmp(b));
    if !bounds.is_empty() {
        let bounds: Vec<String> = (bounds.iter())
            .map(|(subject, traits)| format!("{subject}: {}", traits.join(" + ")))
            .collect();
        text.push_str(&format!(" where {}", bounds.join(", ")));
    }
    text
}

/// What `:doc name` shows of the builtin `name`, a line that states it
/// and, where the docs have one, a line that says what it is:
/// - a function or a constant: its signature and its summary;
/// - a module (`string`): its types, traits and opaque types, then the
///   signature of each of its functions, in the order of their names;
/// - a type (`Option`, `json.Value`) or a trait (`Display`,
///   `json.Encode`): its declaration, as the language card writes it,
///   and the first paragraph of its section of the reference pages;
/// - a variant (`Some`, `channel.Message`): the declaration of its
///   enum, and the first paragraph of its section, or else its enum's;
/// - a type of the language (`Int`, `List`): its name and arguments,
///   and what the page of its module says first.
///
/// Of a function or a module whose cargo feature is not built, it also
/// says what it needs. `None` when no builtin has the name.
pub fn doc_text(name: &str) -> Option<String> {
    use crate::builtins::registry::docs;
    let registry = crate::builtins::registry::registry();
    let with_summary = |line: String, summary: Option<String>| match summary {
        Some(summary) => format!("{line}\n{summary}"),
        None => line,
    };
    if let Some(module) = registry.module(name) {
        let mut text = docs::module_lines(module).join("\n");
        if let Some(feature) = module.feature.filter(|_| !module.enabled) {
            let needs = crate::module::needs_feature(&format!("`{name}`"), feature);
            text.push_str(&format!("\n{needs}"));
        }
        return Some(text);
    }
    let row = match name.split_once('.') {
        Some((module, function)) => (registry.module(module))
            .and_then(|m| m.rows.iter().find(|row| row.name == function))
            .and_then(|row| Some((signature_block(module, function)?, row))),
        None => (registry.prelude.iter())
            .find(|row| row.name == name)
            .map(|row| (row.signature.trim_start_matches("fn ").to_string(), row)),
    };
    if let Some((signature, row)) = row {
        let mut text = format!("{signature}\n{}", row.summary);
        if !row.enabled {
            let module = registry.module(row.module);
            let feature = match module.is_some_and(|m| !m.enabled) {
                true => module.and_then(|m| m.feature),
                false => row.feature,
            };
            if let Some(feature) = feature {
                let needs = crate::module::needs_feature(&format!("`{name}`"), feature);
                text.push_str(&format!("\n{needs}"));
            }
        }
        return Some(text);
    }
    if let Some(line) = docs::type_line(name) {
        return Some(with_summary(line, docs::type_summary(name)));
    }
    if let Some(line) = docs::trait_line(name) {
        let summary = match name.contains('.') {
            true => docs::section_summary(name),
            false => docs::prelude_trait_summary(name),
        };
        return Some(with_summary(line, summary));
    }
    if let Some((line, enum_name)) = docs::variant_enum(name) {
        let variant = name.rsplit('.').next().unwrap_or(name);
        let summary = (docs::section_summary(name))
            .or_else(|| docs::variant_meaning(&enum_name, variant))
            .or_else(|| docs::type_summary(&enum_name));
        return Some(with_summary(line, summary));
    }
    let builtin = (crate::types::builtins::BUILTIN_TYPES.iter())
        .find(|t| t.name == name && t.name != "Unit" && t.name.chars().all(char::is_alphabetic))?;
    let params: Vec<String> = (b'a'..)
        .take(builtin.arity? as usize)
        .map(|c| char::from(c).to_string())
        .collect();
    let line = match params.is_empty() {
        true => format!("type {name}  -- builtin"),
        false => format!("type {name}({})  -- builtin", params.join(", ")),
    };
    let summary = docs::module_docs()
        .get(name.to_lowercase().as_str())
        .cloned();
    Some(with_summary(line, summary))
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
    /// The last cell that ran to its end: later inputs see what its
    /// scope binds.
    committed: Option<ModuleId>,
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
            vm: Vm::new(crate::HostIo::process()),
            cwd,
            committed: None,
        }
    }

    /// The problems of the project the session is in, which no input
    /// reports.
    pub fn project_problems(&mut self) -> Vec<Diagnostic> {
        self.session.project_problems()
    }

    /// The text of a value an entry gave, as `println` would write it.
    pub fn show(&mut self, value: &Value) -> String {
        self.vm.show_text(value)
    }

    /// Check, compile and run `input` as the next cell.
    pub fn eval(&mut self, input: &str) -> Evaluation {
        let file = self.session.add_cell(input.to_string());
        let analysis = self.session.analyze(file);
        let mut evaluation = Evaluation {
            diagnostics: analysis.diagnostics.clone(),
            value: None,
            committed: false,
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
        match self.vm.run_program(&program) {
            Ok(value) => {
                self.session.commit_cell(file);
                self.committed = Some(self.session.module_of(file));
                evaluation.value = Some(value);
                evaluation.committed = true;
            }
            Err(e) => evaluation.diagnostics.push(e.to_diagnostic()),
        }
        evaluation
    }

    /// What `:type input` shows: `input` checked as the next entry, and
    /// the type of its value if it has no error. The entry is not run,
    /// and no later entry sees it. `Err` when it is declarations, which
    /// have no value.
    pub fn type_of(&mut self, input: &str) -> Result<Typed, NotAnExpression> {
        let file = self.session.add_cell(input.to_string());
        let analysis = self.session.analyze(file);
        let diagnostics = analysis.diagnostics.clone();
        if analysis.has_errors() {
            return Ok(Typed {
                diagnostics,
                ty: None,
            });
        }
        let scheme = self
            .session
            .cell_value_scheme(file)
            .ok_or(NotAnExpression)?;
        Ok(Typed {
            diagnostics,
            ty: Some(written_scheme(&scheme)),
        })
    }

    /// The names `<Tab>` offers now: those of [`builtin_names`], and
    /// every name the next input sees of the inputs that ran (see
    /// [`scope_completion_names`]). An input that did not run to its
    /// end adds none and takes none away.
    pub fn completion_names(&self) -> Vec<String> {
        let mut names = builtin_names();
        let session = &self.session;
        if let Some(analysis) = self.committed.and_then(|id| session.module_analysis(id)) {
            names.extend(scope_completion_names(
                &analysis.scope,
                session.defs(),
                |id| session.module_analysis(id).map(|a| &a.scope.exports),
            ));
        }
        names.sort();
        names.dedup();
        names
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
    let mut texts: Vec<String> = Vec::new();
    for failure in &taken.failures {
        texts.push(repl.render(&failure.report_error().to_diagnostic()));
    }
    for (_, count) in &taken.not_kept {
        texts.push(repl.render(&Diagnostic::error(
            crate::diagnostic::Code::UnjoinedTaskFailure,
            Span::BUILTIN,
            crate::scheduler::UnjoinedFailures::not_kept_message(*count),
        )));
    }
    crate::scheduler::report(|| {
        for text in &texts {
            eprintln!("{text}");
        }
    });
}

/// The names `<Tab>` offers before any input: the REPL's commands, the
/// keywords and the prelude's names. A builtin module's members are
/// offered once it is imported (see [`scope_completion_names`]).
pub fn builtin_names() -> Vec<String> {
    let mut names: Vec<String> = vec![":quit", ":q", ":help", ":h", ":type", ":doc"]
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
    println!("  :help, :h      Show this help");
    println!("  :quit, :q      Exit the REPL");
    println!("  :type <expr>   Show the type of an expression without running it");
    println!("  :doc <name>    Show a builtin's signature and summary (list.map, Option, string)");
    println!("  <Tab>          Autocomplete builtins and user-defined names");
    println!();
    println!("Enter expressions to evaluate, or declarations (fn, type, trait, import).");
    println!(
        "Multi-line input: an open brace, paren, bracket, string or block comment, or a line that ends in an operator (+, |>, =, -> ...), continues on the next line."
    );
    println!("A brace in a string starts an interpolation: write \\{{ for the character itself.");
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

    // ── Evaluation ────────────────────────────────────────────────

    fn repl() -> Repl {
        Repl::new(ProjectSetup::None)
    }

    /// A bound an entry leaves to a later entry, which decides the type
    /// it is of (what is sent on a channel), gets its evidence from the
    /// later entry: the earlier entry's evidence names it as pending, the
    /// later entry's has it, the impl for the type it decided.
    #[test]
    fn a_bound_a_later_entry_decides_gets_its_evidence_there() {
        let mut repl = repl();
        let setup = [
            "trait Greet { fn greet(self) -> String }",
            "trait Greet for Int { fn greet(self) -> String { \"int\" } }",
            "fn hello(x: a) -> String where a: Greet { Greet.greet(x) }",
            "import channel",
            "let ch = channel.new(4)",
            "fn f() -> String {\n  match channel.receive(ch) {\n    Some(m) -> hello(m)\n    None -> \"none\"\n  }\n}",
        ];
        for entry in setup {
            let evaluation = repl.eval(entry);
            assert!(
                evaluation.committed,
                "`{entry}` must run: {:?}",
                evaluation.diagnostics
            );
        }
        let earlier = repl.committed.expect("a committed entry");
        let evaluation = repl.eval("channel.send(ch, 3)");
        assert!(evaluation.committed, "{:?}", evaluation.diagnostics);
        let later = repl.committed.expect("the entry that decides");
        let late = (repl.session.module_analysis(later))
            .map(|analysis| analysis.ast.late_evidence.clone())
            .unwrap_or_default();
        match late.as_slice() {
            [(pending, crate::evidence::Evidence::Impl { .. })] => {
                assert_eq!(pending.cell, earlier, "{late:?}")
            }
            other => panic!("expected the impl of Greet for Int, got {other:?}"),
        }
        assert_eq!(value(&mut repl, "f()"), "int");
    }

    /// An entry whose value is general in a bound of a program's trait
    /// shows it as it is, of no type: no dictionary is passed for the
    /// bound, and a method taken as a value is shown as the trait's.
    #[test]
    fn an_entry_general_in_a_bound_shows_its_value() {
        let mut repl = repl();
        let setup = [
            "trait Nm { fn nm(self) -> String }",
            "trait Nm for Int { fn nm(self) -> String { \"i\" } }",
            "fn greet2(x: a) -> String where a: Nm { Nm.nm(x) }",
            "let shout = { x -> \"{Nm.nm(x)}!\" }",
            "let (h, n) = (greet2, 1)",
        ];
        for entry in setup {
            let evaluation = repl.eval(entry);
            assert!(
                evaluation.committed,
                "`{entry}` must run: {:?}",
                evaluation.diagnostics
            );
        }
        assert_eq!(value(&mut repl, "greet2"), "<fn:greet2>");
        assert_eq!(value(&mut repl, "Nm.nm"), "<fn:Nm.nm>");
        assert_eq!(value(&mut repl, "(greet2, 1)"), "(<fn:greet2>, 1)");
        assert_eq!(value(&mut repl, "Some(greet2)"), "Some(<fn:greet2>)");
        assert_eq!(value(&mut repl, "shout"), "<fn:closure in shout>");
        assert_eq!(value(&mut repl, "h"), "<fn:greet2>");
        assert_eq!(value(&mut repl, "h(n)"), "i");
        assert_eq!(value(&mut repl, "shout(n)"), "i!");
    }

    /// A later entry adds an impl of a trait for a type at another trait
    /// argument; one that fails takes its impl back, and the impl is
    /// added again by a later entry.
    #[test]
    fn a_later_entry_adds_an_impl_at_another_trait_argument() {
        let mut repl = repl();
        let setup = [
            "trait From(e) { fn from(source: e) -> Self }",
            "type A { A(Int), S(String) }",
            "trait From(Int) for A { fn from(e: Int) -> A { A(e) } }",
            "fn sh(a: A) -> String {\n  match a {\n    A(n) -> \"a{n}\"\n    S(s) -> \"s{s}\"\n  }\n}",
        ];
        for entry in setup {
            let evaluation = repl.eval(entry);
            assert!(
                evaluation.committed,
                "`{entry}` must run: {:?}",
                evaluation.diagnostics
            );
        }
        assert_eq!(value(&mut repl, "sh(A.from(1))"), "a1");
        let failing =
            "trait From(String) for A { fn from(e: String) -> A { S(e) } }\nlet z: Int = \"s\"";
        assert!(!repl.eval(failing).committed, "`{failing}` must fail");
        assert_eq!(value(&mut repl, "sh(A.from(2))"), "a2");
        let added = "trait From(String) for A { fn from(e: String) -> A { S(e) } }";
        assert!(repl.eval(added).committed, "`{added}` must run");
        assert_eq!(value(&mut repl, "sh(A.from(\"x\"))"), "sx");
        assert_eq!(value(&mut repl, "sh(A.from(3))"), "a3");
    }

    /// An entry is its own definition: a trait argument nothing in it
    /// decides is an error there, never one a later entry decides.
    #[test]
    fn an_entry_whose_trait_argument_nothing_in_it_decides_fails() {
        let mut repl = repl();
        let setup = [
            "trait Conv(b) { fn conv(self) -> b }",
            "trait Conv(String) for Int { fn conv(self) -> String { \"s{self}\" } }",
            "trait Conv(Bool) for Int { fn conv(self) -> Bool { true } }",
        ];
        for entry in setup {
            let evaluation = repl.eval(entry);
            assert!(
                evaluation.committed,
                "`{entry}` must run: {:?}",
                evaluation.diagnostics
            );
        }
        let undecided = repl.eval("let x = Conv.conv(6)");
        assert!(!undecided.committed, "the entry must fail");
        let messages: Vec<&str> = (undecided.diagnostics.iter())
            .map(|d| d.message.as_str())
            .collect();
        assert!(
            messages
                .iter()
                .any(|m| m.starts_with("cannot decide which impl of 'Conv(_)'")),
            "{messages:?}"
        );
        assert!(
            !messages
                .iter()
                .any(|m| m.contains("compiler bug") || m.contains("pending")),
            "{messages:?}"
        );
        let decided = "let y: String = Conv.conv(6)";
        assert!(repl.eval(decided).committed, "`{decided}` must run");
        assert_eq!(value(&mut repl, "y"), "s6");
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

    /// An entry that fails leaves every table of the session as it was
    /// before it: what its check entered is forgotten when the next
    /// entry is added, and it wrote over nothing. One failing entry of
    /// each kind that enters rows, in a session that has a type, two
    /// traits with a method of one name, an impl, a trait with an
    /// associated type, a function and a `let`.
    #[test]
    fn a_failed_entry_leaves_the_session_tables_as_they_were() {
        let mut repl = repl();
        let setup = [
            "type U { U(Int) }",
            "trait A { fn m(self) -> String }\ntrait A for U { fn m(self) -> String { \"a\" } }",
            "trait B { fn m(self) -> Int }",
            "trait Has {\n  type Item\n  fn item(self) -> Self::Item\n}",
            "fn f(u: U) -> String { \"{u}\" }",
            "let n = 1",
        ];
        for entry in setup {
            let evaluation = repl.eval(entry);
            assert!(evaluation.committed, "`{entry}` must run");
        }
        let failing = [
            "trait Display for U { fn display(self) -> String { 3 } }",
            "trait Display for U { fn display(self) -> String { \"x\" } }\nlet z: Int = \"s\"",
            "trait B for U { fn m(self) -> Int { \"bad\" } }",
            "trait Has for U {\n  type Item = Int\n  fn item(self) -> Int { \"bad\" }\n}",
            "type U { U(Int), V }\nlet z: Int = \"s\"",
            "type Pair = (U, U)\nlet z: Int = \"s\"",
            "fn f(u: U) -> String { 3 }",
            "type W { w: U }\ntrait C { fn c(self) -> Int }\ntrait C for W { fn c(self) -> Int { nope } }",
            "let n: String = 2",
            "f(1)",
        ];
        for entry in failing {
            let before = repl.session.tables().fingerprint();
            let evaluation = repl.eval(entry);
            assert!(!evaluation.committed, "`{entry}` must fail");
            assert_ne!(
                repl.session.tables().fingerprint(),
                before,
                "the check of `{entry}` enters rows (or the test shows nothing)"
            );
            // The next entry drops what the failed one declared.
            repl.session.add_cell(String::new());
            assert_eq!(
                repl.session.tables().fingerprint(),
                before,
                "after the failed entry `{entry}`"
            );
        }
        // The session is what it was.
        assert_eq!(value(&mut repl, "A.m(U(1))"), "a");
        assert_eq!(value(&mut repl, "f(U(2))"), "U(2)");
    }

    /// The session keeps the analysis of the last entry that ran, which
    /// the next entry imports, and of no entry before it: an entry's
    /// analysis holds every name the entry saw, so keeping all of them
    /// cost the square of the session's length. What an earlier entry
    /// declared is still seen, and an error that points into an earlier
    /// entry still shows that entry's text and place.
    #[test]
    fn the_session_keeps_the_analysis_of_one_entry() {
        /// Evaluate `input` and note its cell: that of an entry that
        /// ran is the session's last, that of one that failed is where
        /// its error points.
        fn eval(repl: &mut Repl, cells: &mut Vec<ModuleId>, input: &str) -> Evaluation {
            let evaluation = repl.eval(input);
            cells.push(match evaluation.committed {
                true => repl.committed.expect("an entry ran"),
                false => (repl.session).module_of(evaluation.diagnostics[0].span.file),
            });
            evaluation
        }
        let analysed = |repl: &Repl, cells: &[ModuleId]| -> Vec<bool> {
            (cells.iter())
                .map(|cell| repl.session.module_analysis(*cell).is_some())
                .collect()
        };
        let (mut repl, mut cells) = (repl(), Vec::new());
        assert!(eval(&mut repl, &mut cells, "fn inverse(x) { 100 / x }").committed);
        assert!(eval(&mut repl, &mut cells, "type P { x: Int }").committed);
        assert_eq!(analysed(&repl, &cells), [false, true]);
        // An entry that fails has its analysis until the next entry.
        assert!(!eval(&mut repl, &mut cells, "let wrong: Int = \"s\"").committed);
        assert_eq!(analysed(&repl, &cells), [false, true, true]);
        assert!(!eval(&mut repl, &mut cells, "1 / 0").committed);
        assert_eq!(analysed(&repl, &cells), [false, true, false, true]);
        assert!(eval(&mut repl, &mut cells, "let p = P { x: 4 }").committed);
        assert_eq!(analysed(&repl, &cells), [false, false, false, false, true]);

        // What the first entries declared is there, by name and by type.
        assert_eq!(value(&mut repl, "inverse(p.x)"), "25");
        // An error in the code of the first entry: its text, its place.
        let evaluation = repl.eval("inverse(p.x - 4)");
        assert!(!evaluation.committed);
        let shown: Vec<String> = (evaluation.diagnostics.iter())
            .map(|d| repl.render(d))
            .collect();
        let shown = shown.join("\n");
        assert!(shown.contains("division by zero"), "{shown}");
        assert!(shown.contains("<repl:1>:1:17"), "{shown}");
        assert!(shown.contains("1 | fn inverse(x) { 100 / x }"), "{shown}");
        // A type error that names a type of the second entry.
        let wrong = errors(&mut repl, "let q: P = 1");
        assert_eq!(wrong, ["type mismatch: expected P, got Int"]);
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

    /// `:type` checks its input as the next entry, but does not run it,
    /// and no later entry sees what it declares.
    #[test]
    fn a_typed_entry_is_not_run_and_binds_nothing() {
        let mut repl = repl();
        value(&mut repl, "let n = 1");
        let typed = repl.type_of("n + 1").expect("an expression");
        assert_eq!(typed.ty.as_deref(), Some("Int"));
        assert!(repl.type_of("fn f() { 1 }").is_err());
        assert_eq!(errors(&mut repl, "f()"), ["undefined variable 'f'"]);
        assert_eq!(value(&mut repl, "n"), "1");
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

    /// Several imports in one entry are initialised like a file's: in
    /// the order `silt fmt` writes them, whichever line stands first.
    /// (An entry of two lines is what a pasted text gives.) Of two
    /// modules whose initialisers both fail, the one that comes first
    /// in that order is the one that is reached.
    #[test]
    fn the_imports_of_one_entry_are_initialised_in_their_one_order() {
        for entry in ["import a\nimport b", "import b\nimport a"] {
            let dir = std::path::PathBuf::from("/silt-repl-test");
            let mut repl = Repl::new(ProjectSetup::Script(dir.clone()));
            for module in ["a", "b"] {
                repl.session.set_overlay(
                    &dir.join(format!("{module}.silt")),
                    format!("pub let v: Int = panic(\"{module} is initialised\")\n"),
                );
            }
            let evaluation = repl.eval(entry);
            let messages: Vec<&str> = evaluation
                .diagnostics
                .iter()
                .map(|d| d.message.as_str())
                .collect();
            assert_eq!(messages, ["panic: a is initialised"], "`{entry}`");
        }
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
            value(&mut repl, "match col {\n  Color.Red -> 1\n  Green -> 2\n}"),
            "1"
        );
    }

    #[test]
    fn a_declaration_behind_an_empty_line_or_a_comment_is_a_declaration() {
        let mut repl = repl();
        value(&mut repl, "\nfn f() { 7 }");
        assert_eq!(value(&mut repl, "f()"), "7");
        value(&mut repl, "-- eight\n\nfn g() { 8 }");
        assert_eq!(value(&mut repl, "g()"), "8");
        value(&mut repl, "\n\nlet n = 9");
        assert_eq!(value(&mut repl, "\nn"), "9");
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

        let repl = Rc::new(RefCell::new(Repl::new(ProjectSetup::None)));
        assert!(repl.borrow_mut().eval("import string").committed);
        let helper = SiltHelper { repl: repl.clone() };
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
