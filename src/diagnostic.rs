//! Diagnostics: the one shape every phase reports problems in, and the
//! three ways of showing one.
//!
//! A [`Diagnostic`] is a value: a [`Code`], a severity, the [`Span`] it is
//! about, a one-line message, and optional labels (other places that
//! matter), notes, help and quick fixes. Nothing in it is rendered text.
//! The renderers read the same value: [`render_human`] for a terminal,
//! [`render_json`] for `silt check --format json`, and `to_lsp` for an
//! editor. A position becomes a line and a column only there, through a
//! [`SourceView`].

use std::fmt::Write as _;

use crate::source::{FileId, SourceMap, SourceName, Span};

// ── Severity, phase, code ───────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Severity {
    Error,
    Warning,
}

/// The part of silt that finds a problem. It is the word in the header
/// of a rendered diagnostic, `error[type]: ...`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Phase {
    Lex,
    Parse,
    Type,
    Compile,
    Package,
    Runtime,
}

impl Phase {
    pub fn word(self) -> &'static str {
        match self {
            Phase::Lex => "lex",
            Phase::Parse => "parse",
            Phase::Type => "type",
            Phase::Compile => "compile",
            Phase::Package => "package",
            Phase::Runtime => "runtime",
        }
    }
}

macro_rules! codes {
    ($( $(#[$doc:meta])* $name:ident = $id:literal, $phase:ident; )*) => {
        /// What a diagnostic is about. Each code has a stable id
        /// (`E0301`), whose hundreds name its phase: 0 lex, 1 parse,
        /// 3 type, 4 compile, 5 entry point, 6 package, 7 runtime.
        #[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
        pub enum Code {
            $( $(#[$doc])* $name, )*
        }

        impl Code {
            /// Every code, in id order.
            pub const ALL: &'static [Code] = &[$(Code::$name),*];

            /// The stable id, `E` and four digits.
            pub fn id(self) -> &'static str {
                match self { $(Code::$name => $id,)* }
            }

            pub fn phase(self) -> Phase {
                match self { $(Code::$name => Phase::$phase,)* }
            }
        }
    };
}

codes! {
    // ── lex ──
    UnexpectedChar = "E0001", Lex;
    UnterminatedString = "E0002", Lex;
    UnterminatedComment = "E0003", Lex;
    InvalidEscape = "E0004", Lex;
    InvalidNumber = "E0005", Lex;
    // ── parse ──
    /// A token other than the one the grammar needs here.
    ExpectedToken = "E0101", Parse;
    ExpectedIdentifier = "E0102", Parse;
    ExpectedDeclaration = "E0103", Parse;
    ExpectedExpression = "E0104", Parse;
    ExpectedPattern = "E0105", Parse;
    ExpectedType = "E0106", Parse;
    /// A list, block or call that is not closed where it ends.
    UnclosedDelimiter = "E0107", Parse;
    /// Two declarations, or two statements, on one line.
    MissingNewline = "E0108", Parse;
    /// Syntax of another language, or of an older silt, that silt does
    /// not have.
    UnsupportedSyntax = "E0109", Parse;
    /// A name bound twice at the top level of a file.
    DuplicateTopLevel = "E0110", Parse;
    /// A field named twice in an anonymous record type or literal.
    DuplicateField = "E0111", Parse;
    NestingTooDeep = "E0112", Parse;
    /// A declaration whose parts are not allowed together (a supertrait
    /// on an impl, an annotated `type` parameter, a lowercase type
    /// name, ...).
    InvalidDeclaration = "E0113", Parse;
    // ── type ──
    TypeMismatch = "E0301", Type;
    UndefinedVariable = "E0302", Type;
    UndefinedConstructor = "E0303", Type;
    UndefinedType = "E0304", Type;
    /// A field that no record in scope declares.
    UnknownField = "E0305", Type;
    /// A field that the record at hand does not declare.
    NoSuchField = "E0306", Type;
    MissingField = "E0307", Type;
    UnknownMethod = "E0308", Type;
    /// A method called in a way it cannot be (a method without `self` on
    /// a value).
    InvalidMethodCall = "E0309", Type;
    /// A method several traits provide.
    AmbiguousMethod = "E0310", Type;
    NoSuchVariant = "E0311", Type;
    /// A type name in an annotation that names no type.
    UnknownType = "E0312", Type;
    UnknownTrait = "E0313", Type;
    /// A type has no impl of a trait it is required to have.
    MissingTraitImpl = "E0314", Type;
    /// A type cannot have the derived `Equal`, `Compare` or `Hash` it is
    /// used with: one of its fields has none.
    NotDerivable = "E0315", Type;
    /// A call needs a trait bound the enclosing function does not
    /// declare.
    MissingConstraint = "E0316", Type;
    /// The wrong number of arguments, fields, bindings or type arguments.
    ArityMismatch = "E0317", Type;
    InfiniteType = "E0318", Type;
    /// A type that cannot be determined.
    AmbiguousType = "E0319", Type;
    NonExhaustive = "E0320", Type;
    UnreachablePattern = "E0321", Type;
    /// A pattern that cannot stand where it is (a refutable `let`, an
    /// or-pattern whose alternatives bind different names, ...).
    InvalidPatternUse = "E0322", Type;
    DuplicateBinding = "E0323", Type;
    /// `?` where it cannot propagate.
    InvalidQuestion = "E0324", Type;
    /// `loop(...)` or a `when` else body where it may not stand.
    InvalidControlFlow = "E0325", Type;
    /// An operation the operand types do not support.
    UnsupportedOperation = "E0326", Type;
    /// An import of a module the checker does not know. A warning: the
    /// compiler decides whether the module exists.
    UnknownModule = "E0327", Type;
    /// A use of a builtin module that is not imported.
    ModuleNotImported = "E0328", Type;
    /// A function or constant a builtin module does not have.
    UnknownModuleMember = "E0329", Type;
    /// A trait declaration that breaks a trait rule.
    InvalidTraitDeclaration = "E0330", Type;
    /// An impl that breaks a trait rule: a missing or undeclared method,
    /// a hand-written impl of a derived trait, ...
    InvalidTraitImpl = "E0331", Type;
    /// An impl of a foreign trait for a foreign type.
    OrphanImpl = "E0332", Type;
    DuplicateDeclaration = "E0333", Type;
    /// A record, enum or alias declaration that is not well-formed.
    InvalidTypeDeclaration = "E0334", Type;
    /// A type annotation or bound that is not well-formed.
    InvalidTypeAnnotation = "E0335", Type;
    /// A name that hides another one where both are needed.
    Shadowing = "E0336", Type;
    /// Recursion at another type than the function's inferred one. A
    /// warning.
    PolymorphicRecursion = "E0337", Type;
    /// A field named twice in a record type, literal, pattern or update.
    DuplicateRecordField = "E0338", Type;
    // ── compile ──
    /// An import of a module that is not there.
    ModuleNotFound = "E0401", Compile;
    ImportCycle = "E0402", Compile;
    /// A name an import asks for that the module does not export.
    NotExported = "E0403", Compile;
    /// A builtin module used without an import, found by the compiler.
    CompileModuleNotImported = "E0404", Compile;
    /// Something the bytecode cannot express: too many constants, locals,
    /// arguments, a jump too far.
    CompileLimit = "E0405", Compile;
    /// A construct the compiler rejects (a decoder for a type that has
    /// none, ...).
    InvalidConstruct = "E0406", Compile;
    /// A `loop(...)` with no enclosing loop in the same function.
    LoopCallOutsideLoop = "E0407", Compile;
    /// A variable named like a builtin module. A warning.
    ShadowsModule = "E0408", Compile;
    /// A defect in silt: the compiler lost track of its own state.
    CompilerBug = "E0409", Compile;
    // ── entry point ──
    MissingMain = "E0501", Compile;
    MainSignature = "E0502", Compile;
    /// A test function that cannot be called with no arguments.
    TestSignature = "E0503", Compile;
    // ── package ──
    ManifestInvalid = "E0601", Package;
    // ── runtime ──
    RuntimeError = "E0701", Runtime;
    /// `main` returned `Err(..)`.
    MainReturnedErr = "E0702", Runtime;
    /// Tasks failed and nobody joined them.
    UnjoinedTaskFailure = "E0703", Runtime;
}

// ── The diagnostic ──────────────────────────────────────────────────

/// A quick fix: a titled set of text edits, each replacing the text of a
/// span (an empty span inserts).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fix {
    pub title: String,
    pub edits: Vec<(Span, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
    pub code: Code,
    pub severity: Severity,
    /// Where the problem is. Every diagnostic has one.
    pub span: Span,
    /// One line: what is wrong.
    pub message: String,
    /// Other places that matter, each with what it is: "first bound here".
    /// The labels of a runtime error are its call stack, innermost frame
    /// first, each labelled with the function's name.
    pub labels: Vec<(Span, String)>,
    pub notes: Vec<String>,
    pub help: Vec<String>,
    pub fixes: Vec<Fix>,
}

impl Diagnostic {
    pub fn new(code: Code, severity: Severity, span: Span, message: impl Into<String>) -> Self {
        let message = message.into();
        debug_assert!(
            !message.contains('\n'),
            "a diagnostic message is one line: {message:?}"
        );
        Diagnostic {
            code,
            severity,
            span,
            message,
            labels: Vec::new(),
            notes: Vec::new(),
            help: Vec::new(),
            fixes: Vec::new(),
        }
    }

    pub fn error(code: Code, span: Span, message: impl Into<String>) -> Self {
        Diagnostic::new(code, Severity::Error, span, message)
    }

    pub fn warning(code: Code, span: Span, message: impl Into<String>) -> Self {
        Diagnostic::new(code, Severity::Warning, span, message)
    }

    pub fn with_label(mut self, span: Span, label: impl Into<String>) -> Self {
        self.labels.push((span, label.into()));
        self
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help.push(help.into());
        self
    }

    pub fn with_fix(mut self, title: impl Into<String>, edits: Vec<(Span, String)>) -> Self {
        self.fixes.push(Fix {
            title: title.into(),
            edits,
        });
        self
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }

    pub fn is_warning(&self) -> bool {
        self.severity == Severity::Warning
    }

    pub fn phase(&self) -> Phase {
        self.code.phase()
    }
}

// ── Where a span is shown ───────────────────────────────────────────

/// A position as it is shown: 1-based lines and columns counted in
/// characters, and the text of the start's line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Position {
    pub line: usize,
    pub col: usize,
    pub end_line: usize,
    pub end_col: usize,
    pub line_text: String,
}

/// A span as it is shown: the name of its file, and where in the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub file: String,
    /// `None` when the place has a name but no position that can be
    /// shown in the text at hand.
    pub position: Option<Position>,
}

/// How the spans of a front door's diagnostics are shown: which name each
/// file has, and how a byte offset becomes a line and a column.
pub trait SourceView {
    /// Where `span` is shown; `None` for a span in no file
    /// ([`Span::BUILTIN`]).
    fn locate(&self, span: Span) -> Option<Located>;

    /// The location of a call-stack frame at `span`, as one string.
    fn frame(&self, span: Span) -> String {
        match self.locate(span) {
            Some(Located {
                file,
                position: Some(p),
            }) => format!("{file}:{}:{}", p.line, p.col),
            Some(Located {
                file,
                position: None,
            }) => file,
            None => "<unknown location>".to_string(),
        }
    }
}

/// The name a file of a source map has when nobody renames it.
pub fn source_name_for_display(name: &SourceName) -> Option<String> {
    match name {
        SourceName::Path(p) | SourceName::Overlay(p) | SourceName::Manifest(p) => {
            Some(p.display().to_string())
        }
        SourceName::Repl(_) => Some("<repl>".to_string()),
        SourceName::Builtin => None,
    }
}

impl SourceMap {
    /// The position of `span` in its file, if this map has the file. A
    /// position past the last line break (where a parse error at the end
    /// of the file points) is moved back onto the last line, just after
    /// its last character, so it has text to show. Lines are shown
    /// without a `\r` before their line break. The text of a manifest is
    /// shown by the display rule (`git::escape_for_display`): it can be a
    /// dependency's.
    pub fn position(&self, span: Span) -> Option<Position> {
        let file = self.get(span.file)?;
        let text: &str = &file.text;
        let line_count = text.lines().count();
        let last_line_end = |line: usize| {
            text.lines()
                .nth(line - 1)
                .map_or(0, |l| l.trim_end_matches('\r').chars().count())
                + 1
        };
        let clamp = |(line, col): (u32, u32)| {
            let (line, col) = (line as usize, col as usize);
            if line_count > 0 && line > line_count {
                (line_count, last_line_end(line_count))
            } else {
                (line, col)
            }
        };
        let (line, col) = clamp(file.line_col(span.start));
        let (end_line, end_col) = clamp(file.line_col(span.end.max(span.start)));
        let mut line_text = text
            .lines()
            .nth(line - 1)
            .unwrap_or("")
            .trim_end_matches('\r')
            .to_string();
        if matches!(file.path, SourceName::Manifest(_)) {
            line_text = crate::git::escape_for_display(&line_text);
        }
        Some(Position {
            line,
            col,
            end_line,
            end_col,
            line_text,
        })
    }
}

impl SourceView for SourceMap {
    fn locate(&self, span: Span) -> Option<Located> {
        let file = source_name_for_display(&self.get(span.file)?.path)?;
        Some(Located {
            file,
            position: self.position(span),
        })
    }
}

// ── Color ───────────────────────────────────────────────────────────

/// Check whether stderr should receive ANSI color escapes.
///
/// Precedence (highest first):
/// 1. `NO_COLOR` set to any non-empty value → never color (per <https://no-color.org/>).
///    This is the kill-switch and wins over every other signal, including
///    `FORCE_COLOR` and a real tty.
/// 2. `FORCE_COLOR` set to any non-empty value → always color, even when
///    stderr is redirected (matches cargo's behavior).
/// 3. Otherwise: isatty(stderr).
pub(crate) fn use_color() -> bool {
    if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
        return false;
    }
    if std::env::var_os("FORCE_COLOR").is_some_and(|v| !v.is_empty()) {
        return true;
    }
    std::io::IsTerminal::is_terminal(&std::io::stderr())
}

pub(crate) struct Colors {
    pub(crate) red: &'static str,
    pub(crate) yellow: &'static str,
    pub(crate) cyan: &'static str,
    pub(crate) bold: &'static str,
    pub(crate) reset: &'static str,
}

pub(crate) const COLORS_ON: Colors = Colors {
    red: "\x1b[31m",
    yellow: "\x1b[33m",
    cyan: "\x1b[36m",
    bold: "\x1b[1m",
    reset: "\x1b[0m",
};

pub(crate) const COLORS_OFF: Colors = Colors {
    red: "",
    yellow: "",
    cyan: "",
    bold: "",
    reset: "",
};

/// The color palette for stderr in the current environment (see
/// [`use_color`]).
pub(crate) fn active_colors() -> &'static Colors {
    if use_color() { &COLORS_ON } else { &COLORS_OFF }
}

// ── Human rendering ─────────────────────────────────────────────────

/// `d` as a terminal shows it, colored when stderr takes color:
///
/// ```text
/// error[type]: type mismatch: expected Int, got String
///  --> main.silt:3:13
///   |
/// 3 |     let x = "a" + 1
///   |             ^^^ type mismatch: expected Int, got String
///   = help: ...
/// ```
///
/// Labels follow the snippet as snippets of their own, introduced by
/// `:::`, except for a runtime error, whose labels are its call stack.
pub fn render_human(view: &dyn SourceView, d: &Diagnostic) -> String {
    let c = active_colors();
    let (word, color) = match d.severity {
        Severity::Error => ("error", c.red),
        Severity::Warning => ("warning", c.yellow),
    };
    let mut out = format!(
        "{bold}{color}{word}[{phase}]{reset}{bold}: {msg}{reset}",
        bold = c.bold,
        reset = c.reset,
        phase = d.phase().word(),
        msg = d.message,
    );
    if let Some(located) = view.locate(d.span) {
        write_snippet(&mut out, c, "-->", &located, '^', &d.message, color);
    }
    let runtime = d.phase() == Phase::Runtime;
    if !runtime {
        for (span, label) in &d.labels {
            if let Some(located) = view.locate(*span) {
                write_snippet(&mut out, c, ":::", &located, '-', label, c.cyan);
            }
        }
    }
    for note in &d.notes {
        write_body(&mut out, c, "= note:", note);
    }
    for help in &d.help {
        write_body(&mut out, c, "= help:", help);
    }
    if runtime {
        let frames: Vec<(String, Span)> = d
            .labels
            .iter()
            .map(|(span, name)| (name.clone(), *span))
            .collect();
        let lines = crate::vm::error::render_call_stack(&frames, |_, span| view.frame(*span));
        if !lines.is_empty() {
            out.push_str("\n\ncall stack:");
            for line in lines {
                out.push('\n');
                out.push_str(&line);
            }
        }
    }
    out
}

/// ` --> file:line:col`, then the line with marks under the span and
/// `text` after them.
fn write_snippet(
    out: &mut String,
    c: &Colors,
    arrow: &str,
    located: &Located,
    mark: char,
    text: &str,
    mark_color: &str,
) {
    let file = crate::git::escape_for_display(&located.file);
    let Some(p) = &located.position else {
        let _ = write!(out, "\n {}{arrow}{} {file}", c.cyan, c.reset);
        return;
    };
    let _ = write!(
        out,
        "\n {}{arrow}{} {file}:{}:{}",
        c.cyan, c.reset, p.line, p.col
    );
    let full_col = p.col.saturating_sub(1);
    let (shown, col) = excerpt_around(&p.line_text, full_col);
    let width = line_num_width(p.line);
    let _ = write!(out, "\n {}{:>width$} |{}", c.cyan, "", c.reset);
    let _ = write!(out, "\n {}{:>width$} |{} {shown}", c.cyan, p.line, c.reset);
    let spacing = caret_spacing(&shown, col);
    // The marks cover the span on its first line: to its end, or to the
    // end of the line when it goes on.
    let shown_chars: Vec<char> = shown.chars().collect();
    let span_chars = if p.end_line == p.line {
        p.end_col.saturating_sub(p.col)
    } else {
        shown_chars.len().saturating_sub(col)
    };
    let covered = shown_chars.iter().skip(col).take(span_chars);
    let marks = mark_width(covered).max(1);
    let _ = write!(
        out,
        "\n {}{:>width$} |{} {spacing}{mark_color}{}{} {text}{}",
        c.cyan,
        "",
        c.reset,
        c.bold,
        mark.to_string().repeat(marks),
        c.reset
    );
}

/// `= note: ...` / `= help: ...` below the snippet. A text of several
/// lines continues under the first line's text.
fn write_body(out: &mut String, c: &Colors, prefix: &str, text: &str) {
    for (i, line) in text.lines().enumerate() {
        if i == 0 {
            let _ = write!(out, "\n  {}{prefix}{} {line}", c.cyan, c.reset);
        } else {
            let _ = write!(out, "\n  {:width$} {line}", "", width = prefix.len());
        }
    }
}

/// The diagnostics `ds`, rendered for a terminal on stderr, with a blank
/// line between two of them.
pub fn eprint_all<'a>(view: &dyn SourceView, ds: impl IntoIterator<Item = &'a Diagnostic>) {
    for (i, d) in ds.into_iter().enumerate() {
        if i > 0 {
            eprintln!();
        }
        eprintln!("{}", render_human(view, d));
    }
}

/// The display width needed for the line number `n`.
pub(crate) fn line_num_width(n: usize) -> usize {
    if n == 0 {
        return 1;
    }
    ((n as f64).log10().floor() as usize) + 1
}

/// The part of `line` to show above the caret, and the caret's column in
/// it. A line of at most `EXCERPT_CHARS` characters is shown whole; a
/// longer one (a generated 8000-character expression, say) is cut to a
/// window around `col`, with `…` where text was left out.
pub(crate) fn excerpt_around(line: &str, col: usize) -> (String, usize) {
    const EXCERPT_CHARS: usize = 160;
    const BEFORE_CARET: usize = 60;
    let chars: Vec<char> = line.chars().collect();
    if chars.len() <= EXCERPT_CHARS {
        return (line.to_string(), col);
    }
    let col = col.min(chars.len());
    let start = col
        .saturating_sub(BEFORE_CARET)
        .min(chars.len() - EXCERPT_CHARS);
    let end = start + EXCERPT_CHARS;
    let mut shown = String::new();
    let mut shown_col = col - start;
    if start > 0 {
        shown.push('…');
        shown_col += 1;
    }
    shown.extend(&chars[start..end]);
    if end < chars.len() {
        shown.push('…');
    }
    (shown, shown_col)
}

/// The padding that puts a caret under the `col`-th char of `src_line`
/// (0-based): one space per display cell, so CJK and emoji do not push
/// the caret left. Tabs are passed through so the terminal expands them
/// as it expands the source line above.
pub(crate) fn caret_spacing(src_line: &str, col: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    let mut out = String::new();
    for ch in src_line.chars().take(col) {
        if ch == '\t' {
            out.push('\t');
        } else {
            let w = UnicodeWidthChar::width(ch).unwrap_or(1);
            for _ in 0..w {
                out.push(' ');
            }
        }
    }
    out
}

/// The number of display cells of `chars`, a tab counted as one.
fn mark_width<'a>(chars: impl Iterator<Item = &'a char>) -> usize {
    use unicode_width::UnicodeWidthChar;
    chars
        .map(|&ch| {
            if ch == '\t' {
                1
            } else {
                UnicodeWidthChar::width(ch).unwrap_or(1)
            }
        })
        .sum()
}

// ── JSON rendering ──────────────────────────────────────────────────

/// `d` as `silt check --format json` prints it: the file, the start and
/// end of the span, the message, the code, and labels, notes and help as
/// data. A span in no file has file `"<unknown>"` and line 0.
pub fn render_json(view: &dyn SourceView, d: &Diagnostic) -> serde_json::Value {
    let place = |span: Span| -> serde_json::Map<String, serde_json::Value> {
        let located = view.locate(span);
        let file = located
            .as_ref()
            .map_or("<unknown>".to_string(), |l| l.file.clone());
        let p = located.and_then(|l| l.position);
        let (line, col, end_line, end_col) =
            p.map_or((0, 0, 0, 0), |p| (p.line, p.col, p.end_line, p.end_col));
        let mut map = serde_json::Map::new();
        map.insert("file".into(), file.into());
        map.insert("line".into(), line.into());
        map.insert("col".into(), col.into());
        map.insert("end_line".into(), end_line.into());
        map.insert("end_col".into(), end_col.into());
        map
    };
    let mut out = place(d.span);
    out.insert("message".into(), d.message.clone().into());
    out.insert(
        "severity".into(),
        match d.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
        .into(),
    );
    out.insert("kind".into(), d.phase().word().into());
    out.insert("code".into(), d.code.id().into());
    let labels: Vec<serde_json::Value> = d
        .labels
        .iter()
        .map(|(span, label)| {
            let mut l = place(*span);
            l.insert("message".into(), label.clone().into());
            serde_json::Value::Object(l)
        })
        .collect();
    out.insert("labels".into(), labels.into());
    out.insert("notes".into(), d.notes.clone().into());
    out.insert("help".into(), d.help.clone().into());
    serde_json::Value::Object(out)
}

// ── LSP ─────────────────────────────────────────────────────────────

/// `d` as an LSP diagnostic. Spans are in files of `sources`; `uri` names
/// the document of a file, and a label in a file without one is left out.
/// The message carries the notes and the help as lines of their own. The
/// quick fixes travel in `data`, as `[{"title", "edits": [{"range",
/// "newText"}]}]`, for the code-action request to hand back.
#[cfg(feature = "lsp")]
pub fn to_lsp(
    sources: &SourceMap,
    d: &Diagnostic,
    uri: &dyn Fn(FileId) -> Option<lsp_types::Uri>,
) -> lsp_types::Diagnostic {
    use lsp_types::{
        DiagnosticRelatedInformation, DiagnosticSeverity, Location, NumberOrString, Range,
    };
    let range = |span: Span| -> Range {
        match sources.get(span.file) {
            Some(file) => Range::new(
                file.lsp_position(span.start),
                file.lsp_position(span.end.max(span.start)),
            ),
            None => Range::default(),
        }
    };
    let mut message = d.message.clone();
    for note in &d.notes {
        message.push_str("\nnote: ");
        message.push_str(note);
    }
    for help in &d.help {
        message.push_str("\nhelp: ");
        message.push_str(help);
    }
    let related: Vec<DiagnosticRelatedInformation> = d
        .labels
        .iter()
        .filter_map(|(span, label)| {
            Some(DiagnosticRelatedInformation {
                location: Location::new(uri(span.file)?, range(*span)),
                message: label.clone(),
            })
        })
        .collect();
    let fixes: Vec<serde_json::Value> = d
        .fixes
        .iter()
        .map(|fix| {
            let edits: Vec<serde_json::Value> = fix
                .edits
                .iter()
                .map(|(span, text)| serde_json::json!({ "range": range(*span), "newText": text }))
                .collect();
            serde_json::json!({ "title": fix.title, "edits": edits })
        })
        .collect();
    lsp_types::Diagnostic {
        range: range(d.span),
        severity: Some(match d.severity {
            Severity::Error => DiagnosticSeverity::ERROR,
            Severity::Warning => DiagnosticSeverity::WARNING,
        }),
        code: Some(NumberOrString::String(d.code.id().to_string())),
        source: Some("silt".to_string()),
        message,
        related_information: (!related.is_empty()).then_some(related),
        data: (!fixes.is_empty()).then_some(serde_json::Value::Array(fixes)),
        ..lsp_types::Diagnostic::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sources(text: &str) -> SourceMap {
        let mut map = SourceMap::new();
        map.add(SourceName::Path("test.silt".into()), text.into());
        map
    }

    fn span(start: u32, end: u32) -> Span {
        Span {
            file: FileId::default(),
            start,
            end,
        }
    }

    #[test]
    fn code_ids_are_unique_and_in_their_phase_range() {
        let mut seen = std::collections::HashSet::new();
        for code in Code::ALL {
            assert!(seen.insert(code.id()), "duplicate id {}", code.id());
            let hundreds = &code.id()[2..3];
            let expected = match code.phase() {
                Phase::Lex => &["0"][..],
                Phase::Parse => &["1"],
                Phase::Type => &["3"],
                Phase::Compile => &["4", "5"],
                Phase::Package => &["6"],
                Phase::Runtime => &["7"],
            };
            assert!(
                expected.contains(&hundreds),
                "{code:?} has id {}",
                code.id()
            );
        }
    }

    #[test]
    fn a_long_line_is_cut_to_a_window_around_the_caret() {
        let short = "let x = 1";
        assert_eq!(excerpt_around(short, 4), (short.to_string(), 4));

        let long: String = "1 + ".repeat(2000);
        let (shown, col) = excerpt_around(&long, 4000);
        assert!(shown.starts_with('…') && shown.ends_with('…'), "{shown}");
        assert_eq!(shown.chars().count(), 162);
        let caret_char = shown.chars().nth(col).unwrap();
        assert_eq!(caret_char, long.chars().nth(4000).unwrap());

        let (shown, col) = excerpt_around(&long, 0);
        assert!(!shown.starts_with('…') && shown.ends_with('…'));
        assert_eq!(col, 0);
    }

    #[test]
    fn positions_past_the_end_are_on_the_last_line() {
        let map = sources("line one\nline two\r\nline three\n");
        let p = map.position(span(14, 16)).unwrap();
        assert_eq!((p.line, p.col, p.end_col), (2, 6, 8));
        assert_eq!(p.line_text, "line two");
        let p = map.position(span(30, 30)).unwrap();
        assert_eq!((p.line, p.col), (3, 11));
        assert_eq!(p.line_text, "line three");
        let p = map.position(span(99, 99)).unwrap();
        assert_eq!((p.line, p.col), (3, 11));
        let empty = sources("");
        let p = empty.position(span(0, 0)).unwrap();
        assert_eq!((p.line, p.col, p.line_text.as_str()), (1, 1, ""));
    }

    #[test]
    fn test_line_num_width() {
        assert_eq!(line_num_width(1), 1);
        assert_eq!(line_num_width(9), 1);
        assert_eq!(line_num_width(10), 2);
        assert_eq!(line_num_width(99), 2);
        assert_eq!(line_num_width(100), 3);
    }

    #[test]
    fn carets_cover_the_span() {
        let map = sources("let x = true + 1\n");
        let d = Diagnostic::error(Code::TypeMismatch, span(8, 12), "type mismatch");
        let out = render_human(&map, &d);
        assert_eq!(
            out,
            "error[type]: type mismatch\n --> test.silt:1:9\n   |\n 1 | let x = true + 1\n   |         ^^^^ type mismatch"
        );
    }

    #[test]
    fn a_span_over_several_lines_is_marked_to_the_end_of_its_first_line() {
        let map = sources("let x = [\n  1,\n]\n");
        let d = Diagnostic::error(Code::TypeMismatch, span(8, 16), "m");
        let out = render_human(&map, &d);
        assert!(out.ends_with("   |         ^ m"), "{out}");
        let d = Diagnostic::error(Code::TypeMismatch, span(4, 16), "m");
        assert!(render_human(&map, &d).ends_with("   |     ^^^^^ m"));
    }

    #[test]
    fn gutters_are_as_wide_as_the_line_number() {
        let text = "x\n".repeat(9) + "y\n";
        let map = sources(&text);
        let out = render_human(
            &map,
            &Diagnostic::error(Code::ExpectedToken, span(16, 17), "o"),
        );
        assert!(out.contains("\n 9 | x"), "{out}");
        assert!(!out.contains("  9 | x"), "{out}");
        let out = render_human(
            &map,
            &Diagnostic::error(Code::ExpectedToken, span(18, 19), "o"),
        );
        assert!(out.contains("\n 10 | y"), "{out}");
    }

    #[test]
    fn notes_help_and_labels_follow_the_snippet() {
        let map = sources("let a = 1\nlet a = 2\n");
        let d = Diagnostic::error(Code::DuplicateTopLevel, span(14, 15), "bound twice")
            .with_label(span(4, 5), "first bound here")
            .with_note("first line\nsecond line")
            .with_help("rename one");
        let out = render_human(&map, &d);
        assert_eq!(
            out,
            "error[parse]: bound twice\n --> test.silt:2:5\n   |\n 2 | let a = 2\n   |     ^ bound twice\n \
             ::: test.silt:1:5\n   |\n 1 | let a = 1\n   |     - first bound here\n  = note: first line\n          \
             second line\n  = help: rename one"
        );
    }

    #[test]
    fn a_warning_has_a_warning_header() {
        let map = sources("let x = 1");
        let d = Diagnostic::warning(Code::PolymorphicRecursion, span(4, 5), "unused");
        assert!(render_human(&map, &d).starts_with("warning[type]: unused"));
    }

    #[test]
    fn a_builtin_span_has_no_locator() {
        let map = sources("let x = 1");
        let d = Diagnostic::error(Code::TypeMismatch, Span::BUILTIN, "m");
        assert_eq!(render_human(&map, &d), "error[type]: m");
    }

    #[test]
    fn json_carries_the_code_the_end_and_the_labels() {
        let map = sources("let a = 1\nlet a = 2\n");
        let d = Diagnostic::error(Code::DuplicateTopLevel, span(14, 15), "bound twice")
            .with_label(span(4, 5), "first bound here")
            .with_help("rename one");
        let json = render_json(&map, &d);
        assert_eq!(json["code"], "E0110");
        assert_eq!(json["kind"], "parse");
        assert_eq!(
            (json["line"].as_u64(), json["col"].as_u64()),
            (Some(2), Some(5))
        );
        assert_eq!(
            (json["end_line"].as_u64(), json["end_col"].as_u64()),
            (Some(2), Some(6))
        );
        assert_eq!(json["labels"][0]["message"], "first bound here");
        assert_eq!(json["labels"][0]["line"], 1);
        assert_eq!(json["help"][0], "rename one");
    }
}
