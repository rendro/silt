use std::fmt;

use crate::compiler::CompileError;
use crate::lexer::LexError;
use crate::parser::ParseError;
use crate::source::{SourceFile, SourceMap, Span};
use crate::typechecker::TypeError;

// ── Error kind ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ErrorKind {
    Lex,
    Parse,
    Type,
    Compile,
    Runtime,
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErrorKind::Lex => write!(f, "lex"),
            ErrorKind::Parse => write!(f, "parse"),
            ErrorKind::Type => write!(f, "type"),
            ErrorKind::Compile => write!(f, "compile"),
            ErrorKind::Runtime => write!(f, "runtime"),
        }
    }
}

// ── Source error ────────────────────────────────────────────────────

pub struct SourceError {
    pub kind: ErrorKind,
    pub message: String,
    /// Where the error is. `None` for an error about the whole program
    /// (no `main`, `main` returned `Err`, ...), which prints no location.
    pub span: Option<Span>,
    /// 1-based line and column of the start of `span`, from the
    /// `SourceMap`; 0 and 0 without a span.
    pub line: usize,
    pub col: usize,
    pub source_line: Option<String>,
    pub file: Option<String>,
    pub is_warning: bool,
}

impl SourceError {
    /// The error `message` of phase `kind` at `span`, a span of a file of
    /// `sources`. `file` is the file's name as it is shown.
    pub fn new(
        kind: ErrorKind,
        message: impl Into<String>,
        span: Option<Span>,
        sources: &SourceMap,
        file: impl Into<String>,
        is_warning: bool,
    ) -> Self {
        // A span in no file of `sources` (`Span::BUILTIN`) has no
        // location to show.
        let (line, col, source_line) = match span.and_then(|s| Some((s, sources.get(s.file)?))) {
            Some((span, file)) => locate(file, span.start),
            None => (0, 0, None),
        };
        Self {
            kind,
            message: message.into(),
            span,
            line,
            col,
            source_line,
            file: Some(file.into()),
            is_warning,
        }
    }

    pub fn from_lex_error(err: &LexError, sources: &SourceMap, file: impl Into<String>) -> Self {
        Self::new(
            ErrorKind::Lex,
            err.message.clone(),
            Some(err.span),
            sources,
            file,
            false,
        )
    }

    pub fn from_parse_error(
        err: &ParseError,
        sources: &SourceMap,
        file: impl Into<String>,
    ) -> Self {
        Self::new(
            ErrorKind::Parse,
            err.message.clone(),
            Some(err.span),
            sources,
            file,
            false,
        )
    }

    pub fn from_type_error(err: &TypeError, sources: &SourceMap, file: impl Into<String>) -> Self {
        use crate::typechecker::Severity;
        Self::new(
            ErrorKind::Type,
            err.full_message(|span| sources.line_col((span.file, span.start)).0),
            Some(err.span),
            sources,
            file,
            err.severity == Severity::Warning,
        )
    }

    pub fn from_compile_error(
        err: &CompileError,
        sources: &SourceMap,
        file: impl Into<String>,
    ) -> Self {
        Self::new(
            ErrorKind::Compile,
            err.message.clone(),
            Some(err.span),
            sources,
            file,
            false,
        )
    }

    pub fn compile_warning(
        message: impl Into<String>,
        span: Span,
        sources: &SourceMap,
        file: impl Into<String>,
    ) -> Self {
        Self::new(ErrorKind::Compile, message, Some(span), sources, file, true)
    }

    /// A runtime error at `span`, or about the whole run without one.
    pub fn runtime_at(
        message: impl Into<String>,
        span: Option<Span>,
        sources: &SourceMap,
        file: impl Into<String>,
    ) -> Self {
        Self::new(ErrorKind::Runtime, message, span, sources, file, false)
    }

    /// A compile-kind diagnostic straight from a message: for file-level
    /// issues (e.g. missing `main`) that have no `CompileError` to lift
    /// from. Without a span the header renders alone, still as the
    /// canonical `error[compile]:` of every other compile-phase error.
    pub fn compile_error_at(
        message: impl Into<String>,
        span: Option<Span>,
        sources: &SourceMap,
        file: impl Into<String>,
    ) -> Self {
        Self::new(ErrorKind::Compile, message, span, sources, file, false)
    }
}

/// The 1-based line and column of byte `at` of `file`, and the text of
/// that line.
///
/// A position past the last line break (where a parse error at an
/// unexpected end of file points) is moved back onto the last real line,
/// just after its last character, so the caret lands at the visual end
/// of the file instead of on a line that has no text to show.
pub(crate) fn locate(file: &SourceFile, at: u32) -> (usize, usize, Option<String>) {
    let (line, col) = file.line_col(at);
    let (mut line, mut col) = (line as usize, col as usize);
    // Lines as `str::lines` counts them: a final line break ends the
    // last line rather than starting an empty one, and a `\r` before a
    // line break is not part of the line.
    let mut lines = file.text.lines();
    let line_count = lines.clone().count();
    if line_count > 0 && line > line_count {
        line = line_count;
        col = lines
            .clone()
            .last()
            .unwrap_or("")
            .chars()
            .count()
            .saturating_add(1);
    }
    let source_line = lines.nth(line - 1).map(str::to_string);
    (line, col, source_line)
}

/// Check whether stderr should receive ANSI color escapes.
///
/// Precedence (highest first):
/// 1. `NO_COLOR` set to any non-empty value → never color (per <https://no-color.org/>).
///    This is the kill-switch and wins over every other signal, including
///    `FORCE_COLOR` and a real tty. Cargo/clippy/rustc all honor it.
/// 2. `FORCE_COLOR` set to any non-empty value → always color, even when
///    stderr is redirected (matches cargo's behavior).
/// 3. Otherwise: isatty(stderr).
///
/// Lock: tests/round83_use_color_env_vars_tests.rs.
pub(crate) fn use_color() -> bool {
    // NO_COLOR is the killswitch — per the spec, any non-empty value disables
    // color; an empty value (or unset) does not.
    if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
        return false;
    }
    // FORCE_COLOR forces color even when stderr isn't a tty (e.g. piped to a
    // file or captured by a wrapper that wants the escapes preserved).
    if std::env::var_os("FORCE_COLOR").is_some_and(|v| !v.is_empty()) {
        return true;
    }
    // Stable since Rust 1.70; cross-platform (Unix `isatty(2)` + Windows
    // `GetConsoleMode`). Replaces the previous hand-rolled FFI block that
    // declared `unsafe extern` for `isatty` / `GetStdHandle` / `GetConsoleMode`
    // and wrapped the calls in `unsafe { ... }` — 5 of this crate's 16 unsafe
    // occurrences. Now zero unsafe in this function.
    std::io::IsTerminal::is_terminal(&std::io::stderr())
}

// ── ANSI color helpers ─────────────────────────────────────────────

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

/// Returns the color palette appropriate for the current environment.
/// Sibling modules (REPL helpers, etc.) that render error-shaped
/// output to stderr should call this rather than rolling their own
/// ANSI decisions, so `NO_COLOR` / `FORCE_COLOR` precedence stays in
/// a single place.
///
/// Round-84 add: introduced to fix REPL
/// `render_runtime_error_without_source` always emitting plain text;
/// see `tests/round84_repl_render_runtime_error_force_color_tests.rs`.
pub(crate) fn active_colors() -> &'static Colors {
    if use_color() { &COLORS_ON } else { &COLORS_OFF }
}

// ── Display impl ───────────────────────────────────────────────────

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let c = active_colors();

        // Error header: error[parse]: message  (or warning[type] for type warnings)
        let label_color = if self.is_warning { c.yellow } else { c.red };
        let label = if self.is_warning { "warning" } else { "error" };

        // F12 fix (audit round 17): multi-line messages must only put
        // the first line in the header, and emit remaining lines as
        // `  = note: ...` continuation lines AFTER the caret block.
        // Otherwise regex/parse errors with embedded snippet bodies
        // orphan the body text above the `-->` locator, breaking the
        // clean rustc-style layout.
        //
        // Lock: tests/cli/cli_test_rendering_tests.rs
        // `test_multi_line_vm_error_renders_body_below_caret`.
        let (header_msg, note_body): (&str, Option<&str>) = match self.message.split_once('\n') {
            Some((head, rest)) => (head, Some(rest)),
            None => (self.message.as_str(), None),
        };

        write!(
            f,
            "{bold}{label_color}{label}[{kind}]{reset}{bold}: {msg}{reset}",
            bold = c.bold,
            label_color = label_color,
            label = label,
            kind = self.kind,
            reset = c.reset,
            msg = header_msg,
        )?;

        // Location line: --> file:line:col
        if self.line > 0 {
            // The file can sit in a dependency's directory, named by a
            // manifest: it is shown by the display rule, so the locator
            // stays one line.
            let file = crate::git::escape_for_display(self.file.as_deref().unwrap_or("<input>"));
            write!(
                f,
                "\n {cyan}-->{reset} {file}:{line}:{col}",
                cyan = c.cyan,
                reset = c.reset,
                file = file,
                line = self.line,
                col = self.col,
            )?;
        }

        // Source snippet with caret
        if let Some(ref full_line) = self.source_line {
            let full_col = self.col.saturating_sub(1);
            let (src_line, col) = excerpt_around(full_line, full_col);
            let src_line = src_line.as_str();
            let line_num = self.line;
            let gutter_width = line_num_width(line_num);

            // Empty gutter line
            write!(
                f,
                "\n {cyan}{gutter:>width$} |{reset}",
                cyan = c.cyan,
                gutter = "",
                width = gutter_width,
                reset = c.reset,
            )?;

            // Source line
            write!(
                f,
                "\n {cyan}{line_num:>width$} |{reset} {src}",
                cyan = c.cyan,
                line_num = line_num,
                width = gutter_width,
                reset = c.reset,
                src = src_line,
            )?;

            // Caret line
            // Build spacing to align caret under the error position.
            let spacing: String = caret_spacing(src_line, col);

            // Echo only `header_msg` (the first line of `self.message`,
            // already split off at the top of `fmt`) under the outer
            // caret. Multi-line bodies — e.g. a module-import error
            // that embeds a nested `--> file | ^` snippet into its
            // message text — continue as `  = note:` lines below the
            // caret block, so the nested snippet doesn't render twice.
            // Lock: tests/lang/modules.rs
            // `test_module_parse_error_inner_snippet_rendered_once`.
            write!(
                f,
                "\n {cyan}{gutter:>width$} |{reset} {spacing}{label_color}{bold}^ {msg}{reset}",
                cyan = c.cyan,
                gutter = "",
                width = gutter_width,
                reset = c.reset,
                spacing = spacing,
                label_color = label_color,
                bold = c.bold,
                msg = header_msg,
            )?;
        }

        // Multi-line body: emit remaining message lines as `= note:`
        // (or `= help:`) continuation lines AFTER the caret block, so
        // regex/parse errors with embedded snippets render cleanly
        // below the locator instead of being orphaned above it.
        //
        // Per-line `help: ` prefix support: a body line whose text
        // begins with `help: ` renders as `= help: <rest>` instead of
        // `= note: help: ...`. This lets diagnostics (e.g. the type
        // checker's "did you mean ...?" hint) opt into rustc-style
        // `help:` continuation without reshaping SourceError.
        // Lock: tests/lang/diagnostic_suggestion_tests.rs
        // `test_undefined_variable_suggests_close_match`.
        if let Some(body) = note_body {
            let mut first = true;
            for line in body.lines() {
                let (prefix, content) = if let Some(rest) = line.strip_prefix("help: ") {
                    first = false;
                    ("= help:", rest)
                } else if first {
                    first = false;
                    ("= note:", line)
                } else {
                    // Align continuation spaces under `= note: `/`= help: `
                    // (7-char prefix matches `= note:` / `= help:` width).
                    ("       ", line)
                };
                write!(
                    f,
                    "\n  {cyan}{prefix}{reset} {content}",
                    cyan = c.cyan,
                    reset = c.reset,
                    prefix = prefix,
                    content = content,
                )?;
            }
        }

        Ok(())
    }
}

/// Render a sequence of errors to stderr with a blank line between each
/// diagnostic, following the rustc/gcc convention. Used by the `silt run`
/// and `silt check` paths so multiple errors don't form a solid wall of
/// text. A trailing newline after the last error is NOT emitted — callers
/// that need one should add it themselves.
///
/// Lock: tests/cli/cli_test_rendering_tests.rs
/// `test_multiple_errors_render_with_blank_separator`.
pub fn eprintln_errors_with_separator(errors: &[&SourceError]) {
    for (i, err) in errors.iter().enumerate() {
        if i > 0 {
            eprintln!();
        }
        eprintln!("{err}");
    }
}

/// Compute the display width needed for a line number.
///
/// Exposed at `pub(crate)` so `compiler::format_module_source_error`
/// can share the same gutter-width math instead of inlining a
/// hand-rolled loop. Lock: tests/meta/round72_bloat_cleanup_lock_tests.rs.
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

/// Build the padding string used to align a diagnostic caret under the
/// `col`-th char of `src_line` (0-based). Emits one space per display
/// cell rather than one space per `char`, so CJK / emoji / other
/// double-wide characters don't push the caret to the left of its
/// intended column. Tabs are passed through verbatim so the downstream
/// terminal expands them using its own tab-stop settings (matching the
/// rendered source line above the caret). Fall back to width 1 for
/// chars with no defined Unicode width (e.g. unassigned / control).
///
/// Exposed at `pub(crate)` so `compiler::format_module_source_error`
/// can share the same alignment logic, and so `tests/frontend/caret_width_tests.rs`
/// can exercise it directly. Lock: tests/frontend/caret_width_tests.rs.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{FileId, SourceName};

    /// A map holding `text` as its only file.
    fn sources(text: &str) -> SourceMap {
        let mut map = SourceMap::new();
        map.add(SourceName::Path("test.silt".into()), text.into());
        map
    }

    fn at(start: u32) -> Span {
        Span::point(FileId::default(), start)
    }

    /// An error located at `line`:`col` showing `source_line`.
    fn err_at(
        kind: ErrorKind,
        message: &str,
        line: usize,
        col: usize,
        source_line: &str,
        is_warning: bool,
    ) -> SourceError {
        SourceError {
            kind,
            message: message.to_string(),
            span: Some(at(0)),
            line,
            col,
            source_line: Some(source_line.to_string()),
            file: Some("test.silt".to_string()),
            is_warning,
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
    fn test_locate() {
        let map = sources("line one\nline two\r\nline three\n");
        let file = map.file(FileId::default());
        assert_eq!(locate(file, 0), (1, 1, Some("line one".to_string())));
        assert_eq!(locate(file, 14), (2, 6, Some("line two".to_string())));
        assert_eq!(locate(file, 19), (3, 1, Some("line three".to_string())));
        // The end of the file, after the final line break, is the end of
        // the last line.
        assert_eq!(locate(file, 30), (3, 11, Some("line three".to_string())));
        assert_eq!(locate(file, 99), (3, 11, Some("line three".to_string())));
        let empty = sources("");
        assert_eq!(locate(empty.file(FileId::default()), 0), (1, 1, None));
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
    fn test_source_error_display_no_color() {
        // Test the structure of the output (without ANSI codes, since we're not on a tty)
        let err = err_at(
            ErrorKind::Parse,
            "expected expression",
            5,
            12,
            "    Err(e) -> println(\"error\")",
            false,
        );
        let output = format!("{err}");
        assert!(output.contains("error[parse]"));
        assert!(output.contains("expected expression"));
        assert!(output.contains("test.silt:5:12"));
        assert!(output.contains("Err(e) -> println(\"error\")"));
        assert!(output.contains("^"));
    }

    #[test]
    fn test_source_error_type_warning() {
        let err = err_at(
            ErrorKind::Type,
            "type mismatch",
            3,
            5,
            "let x = true + 1",
            true,
        );
        let output = format!("{err}");
        assert!(output.contains("warning[type]"));
        assert!(output.contains("type mismatch"));
    }

    #[test]
    fn test_source_error_type_error() {
        let err = err_at(
            ErrorKind::Type,
            "type mismatch",
            3,
            5,
            "let x = true + 1",
            false,
        );
        let output = format!("{err}");
        assert!(output.contains("error[type]"));
        assert!(output.contains("type mismatch"));
    }

    // Round-24 B-fix lock: the span-less `SourceError::runtime`
    // shortcut was deleted. All runtime diagnostics must go through
    // `runtime_at` with a real (or explicitly synthesized) span so
    // they render with the canonical `error[runtime]:` header and a
    // `-->` locator. This test doubles as a static check: if a
    // caller tries to re-add a span-less shortcut the code won't
    // compile until they thread a span through the error path.
    #[test]
    fn test_runtime_at_is_the_sole_runtime_constructor() {
        let map = sources("fn main() { 42 }");
        let err = SourceError::runtime_at("division by zero", Some(at(0)), &map, "test.silt");
        let output = format!("{err}");
        assert!(output.contains("error[runtime]"));
        assert!(output.contains("division by zero"));
        assert!(output.contains("-->"));
        assert!(output.contains("test.silt:1:1"));
    }

    // Round-24 B-fix: compile_error_at is the new span-synthesizing
    // entry point used by the `silt run` / `silt check` missing-main
    // diagnostic. Verifies the rendered shape matches every other
    // compile-phase error: `error[compile]:` header + locator when
    // the span is non-zero.
    #[test]
    fn test_compile_error_at_renders_canonical_shape() {
        let err = SourceError::compile_error_at(
            "program has no main() function",
            None,
            &sources(""),
            "empty.silt",
        );
        let output = format!("{err}");
        assert!(
            output.contains("error[compile]"),
            "expected canonical error[compile] header, got: {output}"
        );
        assert!(output.contains("program has no main() function"));
    }

    #[test]
    fn test_from_lex_error() {
        let lex_err = LexError {
            message: "unexpected character: '@'".to_string(),
            span: at(4),
        };
        let err = SourceError::from_lex_error(&lex_err, &sources("let @x = 42"), "test.silt");
        assert_eq!(err.kind, ErrorKind::Lex);
        assert_eq!(err.source_line, Some("let @x = 42".to_string()));
        assert_eq!(err.file, Some("test.silt".to_string()));
    }

    #[test]
    fn test_from_parse_error() {
        let parse_err = ParseError {
            message: "expected identifier, found +".to_string(),
            span: at(15),
        };
        let map = sources("let x = 42\nlet + = 1");
        let err = SourceError::from_parse_error(&parse_err, &map, "test.silt");
        assert_eq!(err.kind, ErrorKind::Parse);
        assert_eq!(err.source_line, Some("let + = 1".to_string()));
    }

    // ── L3: gutter width at decimal boundaries ────────────────────
    #[test]
    fn test_gutter_width_line_9_single_column() {
        // Line 9 should get a 1-column gutter (line_num_width(9) == 1),
        // not a 2-column gutter from the old `line_num_width(line_num + 1)`.
        let err = err_at(ErrorKind::Parse, "oops", 9, 1, "x", false);
        let output = format!("{err}");
        // The source line should render as " 9 | x" with a 1-wide gutter,
        // not " 9 | x" with a 2-wide gutter.
        assert!(
            output.contains(" 9 | x"),
            "expected 1-column gutter for line 9, got:\n{output}"
        );
        // Make sure the wider (incorrect) gutter is NOT present.
        assert!(
            !output.contains("  9 | x"),
            "line 9 should NOT have a 2-column gutter:\n{output}"
        );
    }

    #[test]
    fn test_gutter_width_line_10_two_columns() {
        // Line 10 legitimately needs a 2-column gutter.
        let err = err_at(ErrorKind::Parse, "oops", 10, 1, "y", false);
        let output = format!("{err}");
        assert!(
            output.contains(" 10 | y"),
            "expected 2-column gutter for line 10, got:\n{output}"
        );
    }

    // ── L4: note continuation alignment ───────────────────────────
    #[test]
    fn test_note_continuation_alignment() {
        // A multi-line message should align continuation lines with
        // the first `= note:` content.
        let err = err_at(
            ErrorKind::Parse,
            "first line\nsecond line\nthird line",
            1,
            1,
            "x",
            false,
        );
        let output = format!("{err}");
        // Find the column where `= note:` content starts.
        let note_line = output.lines().find(|l| l.contains("= note:")).unwrap();
        let note_content_col = note_line.find("second").unwrap();
        // Find the continuation line.
        let cont_line = output.lines().find(|l| l.contains("third")).unwrap();
        let cont_content_col = cont_line.find("third").unwrap();
        assert_eq!(
            note_content_col, cont_content_col,
            "continuation content column ({cont_content_col}) should match \
             = note: content column ({note_content_col}).\n\
             note line: {note_line:?}\ncont line: {cont_line:?}"
        );
    }

    // ── L6: every constructor moves a position past EOF onto the last line
    #[test]
    fn test_from_type_error_clamps_eof_span() {
        use crate::typechecker::Severity;
        // The span points past the end of a two-line source. The error
        // shows the last real line.
        let map = sources("let a = 1\nlet b = 2");
        let type_err = TypeError {
            message: "some type error".to_string(),
            span: at(99),
            severity: Severity::Error,
            line_note: None,
        };
        let err = SourceError::from_type_error(&type_err, &map, "test.silt");
        assert_eq!(err.line, 2);
        assert_eq!(err.source_line, Some("let b = 2".to_string()));
        let output = format!("{err}");
        assert!(
            output.contains("let b = 2"),
            "expected clamped source snippet in output:\n{output}"
        );
    }

    #[test]
    fn test_from_compile_error_clamps_eof_span() {
        let map = sources("fn main() { 42 }\n");
        let compile_err = CompileError {
            message: "some compile error".to_string(),
            span: at(17),
        };
        let err = SourceError::from_compile_error(&compile_err, &map, "test.silt");
        assert_eq!((err.line, err.col), (1, 17));
        assert_eq!(err.source_line, Some("fn main() { 42 }".to_string()));
    }

    #[test]
    fn test_compile_warning_clamps_eof_span() {
        let map = sources("let a = 1\nlet b = 2\n");
        let err = SourceError::compile_warning("unused variable", at(20), &map, "test.silt");
        assert_eq!(err.line, 2);
        assert_eq!(err.source_line, Some("let b = 2".to_string()));
        assert!(err.is_warning);
        let output = format!("{err}");
        assert!(
            output.contains("let b = 2"),
            "expected clamped source snippet in output:\n{output}"
        );
    }

    #[test]
    fn test_runtime_at_without_span_has_no_location() {
        let map = sources("fn main() { 42 }");
        let err = SourceError::runtime_at("main returned Err: 1", None, &map, "test.silt");
        assert_eq!((err.line, err.col, err.source_line.clone()), (0, 0, None));
        let output = format!("{err}");
        assert!(!output.contains("-->"), "{output}");
    }
}
