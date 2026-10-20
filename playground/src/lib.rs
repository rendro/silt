//! silt for a web page: what a playground needs to check, run, format
//! and colour one program, and to complete a name in it.
//!
//! Everything here is plain Rust on top of the embedding API
//! (`docs/ffi.md`), so the tests run it on the machine that builds it.
//! The module `wasm` hands the same functions to JavaScript; it is the
//! only part that needs a browser.
//!
//! The program is one file, `main.silt`, outside any package. Every
//! offset handed out counts UTF-16 code units, as a JavaScript string
//! and a browser's editor count.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::Serialize;
use silt::ast::{Decl, PatternKind, TypeBody};
use silt::diagnostic::{Code, Diagnostic, Severity, render_human};
use silt::intern::resolve;
use silt::lexer::{KEYWORD_LITERALS, KEYWORDS, Lexer, Tok, Token};
use silt::session::{Config, Entry, LockPolicy, Program, ProjectSetup, Session};
use silt::source::{FileId, SourceMap, SourceName, Span};
use silt::{Buffer, Clock, HostIo, Value, Vm, module};

#[cfg(target_arch = "wasm32")]
mod wasm;

/// The name of the program's file in every report.
const FILE: &str = "main.silt";

/// The steps a program may run before the playground ends it: a page has
/// one thread, and a program that loops for ever would hold it for ever.
pub const STEP_BUDGET: u64 = 200_000_000;

/// A session with the text `source` as its only file.
fn open(source: &str) -> (Session, FileId) {
    let mut session = Session::new(Config {
        project: ProjectSetup::None,
        lock: LockPolicy::ReadOnly,
        host: Vec::new(),
    });
    let file = session.set_overlay(Path::new(FILE), source.to_string());
    (session, file)
}

/// Every static diagnostic of `file`, in the order the `silt` command
/// prints them: the analysis's, then what compiling found. `compiled`
/// is what [`Session::compile`] returned for it.
fn static_diagnostics(
    session: &mut Session,
    file: FileId,
    compiled: &Result<Program, Vec<Diagnostic>>,
) -> Vec<Diagnostic> {
    let analysis = session.analyze(file);
    let analysed_with_errors = analysis.has_errors();
    let mut diagnostics = analysis.diagnostics.clone();
    match compiled {
        Ok(_) => {}
        // A program whose analysis has errors is not compiled: `compile`
        // hands back the analysis's errors, which are listed already.
        Err(_) if analysed_with_errors => {}
        Err(errors) => diagnostics.extend(errors.iter().cloned()),
    }
    diagnostics
}

/// `diagnostics` as the `silt` command prints them, a blank line between
/// two of them.
fn render(session: &Session, diagnostics: &[Diagnostic]) -> String {
    let texts: Vec<String> = diagnostics
        .iter()
        .map(|d| render_human(session.sources(), d))
        .collect();
    texts.join("\n\n")
}

// ── Run ───────────────────────────────────────────────────────────────

/// What running a program gave, in the three parts a terminal shows one
/// after the other.
#[derive(Debug, Serialize, PartialEq)]
pub struct Run {
    /// The static errors and warnings, as `silt run` prints them. With
    /// an error among them the program did not run.
    pub diagnostics: String,
    /// What the program printed, and what the runtime reported for it
    /// (a task that failed and that nobody joined).
    pub output: String,
    /// The runtime error that ended the program, or `main`'s `Err`, as
    /// `silt run` prints it; empty for a program that ended well.
    pub error: String,
    /// Whether the program ran, and neither failed nor returned `Err`.
    pub ok: bool,
}

/// The payload of `value` when it is the `Err(..)` of a `Result`: a
/// `main` that returns it has failed, as under `silt run`.
fn returned_err(value: &Value) -> Option<&Value> {
    let Value::Variant(variant) = value else {
        return None;
    };
    if !variant.is(silt::typeinfo::bv::ERR) {
        return None;
    }
    Some(variant.fields().first().unwrap_or(value))
}

/// Run `source` as `silt run` does, with `clock` as its clock and at
/// most `steps` steps (see [`STEP_BUDGET`]).
pub fn run(source: &str, clock: impl Clock + 'static, steps: u64) -> Run {
    let (mut session, file) = open(source);
    let compiled = session.compile(file, Entry::Main);
    let diagnostics = static_diagnostics(&mut session, file, &compiled);
    let rendered = render(&session, &diagnostics);
    let program = match compiled {
        Ok(program) if !diagnostics.iter().any(Diagnostic::is_error) => program,
        _ => {
            return Run {
                diagnostics: rendered,
                output: String::new(),
                error: String::new(),
                ok: false,
            };
        }
    };
    // Where a `main` that returns `Err` is reported: at its name.
    let main_span = session
        .module_analysis(session.module_of(file))
        .and_then(|analysis| {
            analysis.ast.decls.iter().find_map(|decl| match decl {
                Decl::Fn(f) if resolve(f.name) == "main" => Some(f.name_span),
                _ => None,
            })
        })
        .unwrap_or(Span::point(file, 0));

    let out = Buffer::new();
    let mut vm = Vm::new(HostIo::buffer(&out).clock(clock));
    vm.set_step_budget(steps);
    let failure = match vm.run_program(&program) {
        Ok(value) => match returned_err(&value) {
            Some(payload) => Some(Diagnostic::error(
                Code::MainReturnedErr,
                main_span,
                format!("main returned Err: {}", vm.show_text(payload)),
            )),
            // `main` has returned; the program ends when its tasks can
            // do no more.
            None => {
                vm.settle();
                None
            }
        },
        Err(e) if e.out_of_steps => Some(
            e.to_diagnostic()
                .with_note(format!("the playground ends a program after {steps} steps")),
        ),
        Err(e) => Some(e.to_diagnostic()),
    };
    // The tasks that have failed and that nobody joined are reported
    // into `out` here at the latest.
    drop(vm);
    Run {
        diagnostics: rendered,
        output: out.take(),
        ok: failure.is_none(),
        error: failure.map_or_else(String::new, |d| render_human(session.sources(), &d)),
    }
}

/// The clock of a page: the time of day, which a sleep moves on instead
/// of waiting for.
///
/// A page has one thread, and a sleep that blocked it would freeze the
/// page. So a program that sleeps for an hour goes on at once, and reads
/// a time one hour later from then on: what it prints is what it would
/// print after the wait.
pub struct PageClock {
    /// Milliseconds since the Unix epoch.
    now_ms: fn() -> f64,
    state: Mutex<PageTime>,
}

struct PageTime {
    /// `now_ms` when the clock was made.
    started_ms: f64,
    /// What the sleeps so far have skipped.
    skipped: Duration,
    /// The last monotonic reading: the time of day may go back.
    last: Duration,
}

impl PageClock {
    pub fn new(now_ms: fn() -> f64) -> PageClock {
        PageClock {
            now_ms,
            state: Mutex::new(PageTime {
                started_ms: now_ms(),
                skipped: Duration::ZERO,
                last: Duration::ZERO,
            }),
        }
    }
}

fn millis(ms: f64) -> Duration {
    Duration::try_from_secs_f64(ms / 1000.0).unwrap_or(Duration::ZERO)
}

impl Clock for PageClock {
    fn now(&self) -> Duration {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        millis((self.now_ms)()) + state.skipped
    }

    fn monotonic(&self) -> Duration {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let reading = millis((self.now_ms)() - state.started_ms) + state.skipped;
        state.last = state.last.max(reading);
        state.last
    }

    fn sleep(&self, duration: Duration) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.skipped = state.skipped.saturating_add(duration);
    }
}

// ── Offsets ───────────────────────────────────────────────────────────

/// Byte offsets of `source` as counts of UTF-16 code units. Cheapest
/// when asked in rising order.
struct Utf16<'a> {
    source: &'a str,
    byte: usize,
    units: u32,
}

impl<'a> Utf16<'a> {
    fn new(source: &'a str) -> Self {
        Utf16 {
            source,
            byte: 0,
            units: 0,
        }
    }

    fn at(&mut self, byte: u32) -> u32 {
        let mut byte = (byte as usize).min(self.source.len());
        while !self.source.is_char_boundary(byte) {
            byte -= 1;
        }
        if byte < self.byte {
            self.byte = 0;
            self.units = 0;
        }
        self.units += self.source[self.byte..byte].encode_utf16().count() as u32;
        self.byte = byte;
        self.units
    }
}

// ── Check ─────────────────────────────────────────────────────────────

/// A static error or warning, for an editor to underline.
#[derive(Debug, Serialize, PartialEq)]
pub struct Problem {
    pub from: u32,
    pub to: u32,
    /// `"error"` or `"warning"`.
    pub severity: &'static str,
    /// The message, then each note and each help on a line of its own.
    pub message: String,
}

/// What `silt check` finds in `source`.
pub fn check(source: &str) -> Vec<Problem> {
    let (mut session, file) = open(source);
    let compiled = session.compile(file, Entry::Tests { filter: None });
    let diagnostics = static_diagnostics(&mut session, file, &compiled);
    diagnostics
        .iter()
        .map(|d| {
            let mut offsets = Utf16::new(source);
            // What is wrong outside the text (there is no such place
            // for one file without a package) is shown at its start.
            let (from, to) = match d.span.file == file {
                true => (offsets.at(d.span.start), offsets.at(d.span.end)),
                false => (0, 0),
            };
            let mut message = d.message.clone();
            for note in &d.notes {
                message.push_str(&format!("\nnote: {note}"));
            }
            for help in &d.help {
                message.push_str(&format!("\nhelp: {help}"));
            }
            Problem {
                from,
                to,
                severity: match d.severity {
                    Severity::Error => "error",
                    Severity::Warning => "warning",
                },
                message,
            }
        })
        .collect()
}

// ── Format ────────────────────────────────────────────────────────────

/// `source` as `silt fmt` writes it, or why it is not formatted, as
/// `silt fmt` reports it.
pub fn format(source: &str) -> Result<String, String> {
    let (session, file) = open(source);
    silt::format::format(file, source).map_err(|d| render_human(session.sources(), &d))
}

// ── Complete ──────────────────────────────────────────────────────────

/// A name an editor can offer.
#[derive(Debug, Serialize, PartialEq)]
pub struct Completion {
    /// What is inserted: `match`, `println`, `list.map`, `Shape`.
    pub label: String,
    /// `"keyword"`, `"function"`, `"constant"`, `"constructor"`,
    /// `"module"`, `"type"` or `"variable"`.
    pub kind: &'static str,
}

/// The names a program can use: the keywords, what silt declares
/// without an import, each builtin module of this build and its members
/// (as `module.member`), and what `source` declares at its top level.
pub fn complete(source: &str) -> Vec<Completion> {
    let mut items = Vec::new();
    let mut add = |label: String, kind: &'static str| items.push(Completion { label, kind });

    for word in KEYWORDS.iter().chain(KEYWORD_LITERALS) {
        add(word.to_string(), "keyword");
    }
    for name in module::builtin_free_function_names() {
        add(name.to_string(), "function");
    }
    for (_, variants) in module::builtin_prelude_enum_variants_with_arity() {
        for (variant, _) in *variants {
            add(variant.to_string(), "constructor");
        }
    }
    for name in module::builtin_modules() {
        if module::missing_feature(name).is_some() {
            continue;
        }
        add(name.to_string(), "module");
        for member in module::builtin_module_functions(name) {
            add(format!("{name}.{member}"), "function");
        }
        for member in module::builtin_module_constants(name) {
            add(format!("{name}.{member}"), "constant");
        }
        for member in module::builtin_module_variants(name) {
            add(format!("{name}.{member}"), "constructor");
        }
    }

    // What the text declares, also when it has errors.
    let (mut session, file) = open(source);
    session.analyze(file);
    if let Some(analysis) = session.module_analysis(session.module_of(file)) {
        for decl in &analysis.ast.decls {
            match decl {
                Decl::Fn(f) => add(resolve(f.name), "function"),
                Decl::Type(t) => {
                    add(resolve(t.name), "type");
                    if let TypeBody::Enum(variants) = &t.body {
                        for variant in variants {
                            add(resolve(variant.name), "constructor");
                        }
                    }
                }
                Decl::Trait(t) => add(resolve(t.name), "type"),
                Decl::Let { pattern, .. } => {
                    if let PatternKind::Ident(name) = &pattern.kind {
                        add(resolve(*name), "variable");
                    }
                }
                Decl::TraitImpl(_) | Decl::Import(..) => {}
            }
        }
    }
    items
}

// ── Tokens ────────────────────────────────────────────────────────────

/// The kinds of token [`tokens`] tells apart, by their number. They are
/// the kinds the editors' grammars colour (`editors/`).
pub const TOKEN_KINDS: &[&str] = &[
    "keyword",
    "string",
    "number",
    "literal",
    "type",
    "constructor",
    "builtin",
    "function",
    "operator",
    "punctuation",
    "comment",
];

const KEYWORD: u32 = 0;
const STRING: u32 = 1;
const NUMBER: u32 = 2;
const LITERAL: u32 = 3;
const TYPE: u32 = 4;
const CONSTRUCTOR: u32 = 5;
const BUILTIN: u32 = 6;
const FUNCTION: u32 = 7;
const OPERATOR: u32 = 8;
const PUNCTUATION: u32 = 9;
const COMMENT: u32 = 10;

/// The kind of the name `name`, if it is coloured: the name a `fn`
/// declares (`declared`), a primitive type (`Int`, which `json.Int`
/// also names), a variant silt declares (`Ok`, `Monday`), a function
/// that needs no import (`println`), or a capitalised name: a type, a
/// trait or a variant.
fn name_kind(name: &str, declared: bool) -> Option<u32> {
    static CONSTRUCTORS: OnceLock<HashSet<&'static str>> = OnceLock::new();
    let constructors =
        CONSTRUCTORS.get_or_init(|| module::all_builtin_constructor_names().collect());
    if declared {
        Some(FUNCTION)
    } else if module::BUILTIN_PRIMITIVE_NAMES.contains(&name) {
        Some(TYPE)
    } else if constructors.contains(name) {
        Some(CONSTRUCTOR)
    } else if module::builtin_free_function_names().contains(&name) {
        Some(BUILTIN)
    } else if name.starts_with(|c: char| c.is_uppercase()) {
        Some(TYPE)
    } else {
        None
    }
}

/// The kind of `token`, for the tokens that are coloured whatever
/// stands around them; a name is [`name_kind`]'s.
fn kind_of(token: &Token) -> Option<u32> {
    Some(match token {
        Token::Let
        | Token::Fn
        | Token::Type
        | Token::Trait
        | Token::Match
        | Token::When
        | Token::Return
        | Token::Pub
        | Token::Import
        | Token::As
        | Token::Else
        | Token::Where
        | Token::Loop => KEYWORD,
        Token::Int(_) | Token::Float(_) => NUMBER,
        Token::Bool(_) => LITERAL,
        // The parts of a string with interpolations: each from its
        // quote or brace to its brace or quote.
        Token::StringLit(_)
        | Token::StringStart(_)
        | Token::StringMiddle(_)
        | Token::StringEnd(_) => STRING,
        Token::Plus
        | Token::Minus
        | Token::Star
        | Token::Slash
        | Token::Percent
        | Token::EqEq
        | Token::NotEq
        | Token::Lt
        | Token::Gt
        | Token::LtEq
        | Token::GtEq
        | Token::AndAnd
        | Token::OrOr
        | Token::Not
        | Token::Pipe
        | Token::Bar
        | Token::Question
        | Token::Caret
        | Token::DotDot
        | Token::DotDotDot
        | Token::Arrow => OPERATOR,
        Token::LParen
        | Token::RParen
        | Token::LBrace
        | Token::RBrace
        | Token::LBracket
        | Token::RBracket
        | Token::HashBrace
        | Token::HashBracket
        | Token::Comma
        | Token::Colon
        | Token::ColonColon
        | Token::Dot
        | Token::Eq => PUNCTUATION,
        // Text that is no token, and the end.
        Token::Ident(_) | Token::Error | Token::Eof => return None,
    })
}

/// The span of `tok`, and for the part of a string behind an
/// interpolation the brace that ends the interpolation too: the lexer's
/// span has the brace that opens one and not the brace that closes it,
/// and a string is coloured with both.
fn string_part(source: &str, tok: &Tok) -> Span {
    let mut span = tok.span;
    let closes = matches!(tok.kind, Token::StringMiddle(_) | Token::StringEnd(_));
    if closes && span.start > 0 && source.as_bytes().get(span.start as usize - 1) == Some(&b'}') {
        span.start -= 1;
    }
    span
}

/// The coloured tokens and the comments of `source`, in the order of
/// the text, as the lexer reads them: three numbers each, where it
/// starts, where it ends, and its kind (an index of [`TOKEN_KINDS`]).
/// A text with errors has tokens too.
pub fn tokens(source: &str) -> Vec<u32> {
    let file = SourceMap::new().add(SourceName::Overlay(PathBuf::from(FILE)), source.into());
    let lexed = Lexer::new(file, source).tokenize();
    let mut spans: Vec<(Span, u32)> = Vec::new();
    let mut after_fn = false;
    for tok in &lexed.tokens {
        let kind = match &tok.kind {
            Token::Ident(name) => name_kind(&resolve(*name), after_fn),
            token => kind_of(token),
        };
        after_fn = tok.kind == Token::Fn;
        if let Some(kind) = kind {
            spans.push((string_part(source, tok), kind));
        }
    }
    spans.extend(lexed.comments.iter().map(|c| (c.span, COMMENT)));
    spans.sort_by_key(|(span, _)| span.start);
    let mut offsets = Utf16::new(source);
    let mut out = Vec::with_capacity(spans.len() * 3);
    for (span, kind) in spans {
        out.extend([offsets.at(span.start), offsets.at(span.end), kind]);
    }
    out
}

/// The version of silt this is.
pub fn version() -> &'static str {
    silt::VERSION
}
