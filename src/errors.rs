use std::fmt;

use crate::compiler::CompileError;
use crate::lexer::{LexError, Span};
use crate::parser::ParseError;
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
    pub span: Span,
    pub source_line: Option<String>,
    pub file: Option<String>,
    pub is_warning: bool,
}

impl SourceError {
    pub fn from_lex_error(err: &LexError, source: &str, file: impl Into<String>) -> Self {
        let span = clamp_span_to_source(err.span, source);
        let source_line = get_source_line(source, span.line);
        Self {
            kind: ErrorKind::Lex,
            message: err.message.clone(),
            span,
            source_line,
            file: Some(file.into()),
            is_warning: false,
        }
    }

    pub fn from_parse_error(err: &ParseError, source: &str, file: impl Into<String>) -> Self {
        let span = clamp_span_to_source(err.span, source);
        let source_line = get_source_line(source, span.line);
        Self {
            kind: ErrorKind::Parse,
            message: err.message.clone(),
            span,
            source_line,
            file: Some(file.into()),
            is_warning: false,
        }
    }

    pub fn from_type_error(err: &TypeError, source: &str, file: impl Into<String>) -> Self {
        use crate::typechecker::Severity;
        let source_line = get_source_line(source, err.span.line);
        let is_warning = err.severity == Severity::Warning;
        Self {
            kind: ErrorKind::Type,
            message: err.message.clone(),
            span: err.span,
            source_line,
            file: Some(file.into()),
            is_warning,
        }
    }

    pub fn from_compile_error(err: &CompileError, source: &str, file: impl Into<String>) -> Self {
        let source_line = get_source_line(source, err.span.line);
        Self {
            kind: ErrorKind::Compile,
            message: err.message.clone(),
            span: err.span,
            source_line,
            file: Some(file.into()),
            is_warning: false,
        }
    }

    pub fn compile_warning(
        message: impl Into<String>,
        span: Span,
        source: &str,
        file: impl Into<String>,
    ) -> Self {
        let source_line = get_source_line(source, span.line);
        Self {
            kind: ErrorKind::Compile,
            message: message.into(),
            span,
            source_line,
            file: Some(file.into()),
            is_warning: true,
        }
    }

    pub fn runtime(message: impl Into<String>, file: Option<String>) -> Self {
        Self {
            kind: ErrorKind::Runtime,
            message: message.into(),
            span: Span::new(0, 0),
            source_line: None,
            file,
            is_warning: false,
        }
    }

    pub fn runtime_at(
        message: impl Into<String>,
        span: Span,
        source: &str,
        file: impl Into<String>,
    ) -> Self {
        let source_line = get_source_line(source, span.line);
        Self {
            kind: ErrorKind::Runtime,
            message: message.into(),
            span,
            source_line,
            file: Some(file.into()),
            is_warning: false,
        }
    }
}

/// Extract the source line for the given 1-based line number.
fn get_source_line(source: &str, line: usize) -> Option<String> {
    if line == 0 {
        return None;
    }
    source.lines().nth(line - 1).map(|s| s.to_string())
}

/// Clamp a span that points past the end of `source` back onto the last
/// real line. Parse/lex errors on unexpected EOF typically produce a span
/// pointing at the line *after* the final newline (or one column past the
/// last char), which renders with the `-->` locator but no source snippet
/// since `line - 1` is out of bounds. When that happens, we return a new
/// span pointing at the end of the last real line so the caret lands at
/// the visual "end of file" instead of disappearing. Mirrors the
/// adjustment done by `repl.rs::adjust_span` for the REPL path.
fn clamp_span_to_source(span: Span, source: &str) -> Span {
    if span.line == 0 {
        return span;
    }
    let line_count = source.lines().count();
    if line_count == 0 {
        return span;
    }
    if span.line <= line_count {
        return span;
    }
    // Past EOF — clamp onto the last real line, caret just after its last char.
    let last_line = source.lines().last().unwrap_or("");
    let last_col = last_line.chars().count().saturating_add(1);
    Span::with_offset(line_count, last_col, span.offset)
}

/// Check whether stderr is a terminal (for ANSI color support).
fn use_color() -> bool {
    #[cfg(unix)]
    {
        unsafe extern "C" {
            #[link_name = "isatty"]
            fn libc_isatty(fd: i32) -> i32;
        }
        unsafe { libc_isatty(2) != 0 }
    }
    #[cfg(windows)]
    {
        unsafe extern "system" {
            fn GetStdHandle(nStdHandle: u32) -> *mut core::ffi::c_void;
            fn GetConsoleMode(hConsoleHandle: *mut core::ffi::c_void, lpMode: *mut u32) -> i32;
        }
        const STD_ERROR_HANDLE: u32 = -12i32 as u32;
        unsafe {
            let handle = GetStdHandle(STD_ERROR_HANDLE);
            let mut mode: u32 = 0;
            GetConsoleMode(handle, &mut mode) != 0
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

// ── ANSI color helpers ─────────────────────────────────────────────

#[allow(dead_code)]
struct Colors {
    red: &'static str,
    yellow: &'static str,
    cyan: &'static str,
    bold: &'static str,
    dim: &'static str,
    reset: &'static str,
}

const COLORS_ON: Colors = Colors {
    red: "\x1b[31m",
    yellow: "\x1b[33m",
    cyan: "\x1b[36m",
    bold: "\x1b[1m",
    dim: "\x1b[2m",
    reset: "\x1b[0m",
};

const COLORS_OFF: Colors = Colors {
    red: "",
    yellow: "",
    cyan: "",
    bold: "",
    dim: "",
    reset: "",
};

// ── Display impl ───────────────────────────────────────────────────

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let c = if use_color() { &COLORS_ON } else { &COLORS_OFF };

        // Error header: error[parse]: message  (or warning[type] for type warnings)
        let label_color = if self.is_warning { c.yellow } else { c.red };
        let label = if self.is_warning { "warning" } else { "error" };

        write!(
            f,
            "{bold}{label_color}{label}[{kind}]{reset}{bold}: {msg}{reset}",
            bold = c.bold,
            label_color = label_color,
            label = label,
            kind = self.kind,
            reset = c.reset,
            msg = self.message,
        )?;

        // Location line: --> file:line:col
        if self.span.line > 0 {
            let file = self.file.as_deref().unwrap_or("<input>");
            write!(
                f,
                "\n {cyan}-->{reset} {file}:{line}:{col}",
                cyan = c.cyan,
                reset = c.reset,
                file = file,
                line = self.span.line,
                col = self.span.col,
            )?;
        }

        // Source snippet with caret
        if let Some(ref src_line) = self.source_line {
            let line_num = self.span.line;
            let gutter_width = line_num_width(line_num + 1);

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
            let col = if self.span.col > 0 {
                self.span.col - 1
            } else {
                0
            };
            // Build spacing to align caret under the error position
            let spacing: String = src_line
                .chars()
                .take(col)
                .map(|ch| if ch == '\t' { '\t' } else { ' ' })
                .collect();

            // If `self.message` is a multi-line blob (e.g. a
            // module-import error that already embeds a nested
            // `--> file | ^` snippet into its message text), only
            // echo the first line under the outer caret — otherwise
            // the nested snippet would render twice: once inside
            // the header at the top of the block, and once again
            // here after the outer caret. Lock: tests/modules.rs
            // `test_module_parse_error_inner_snippet_rendered_once`.
            let caret_msg = self
                .message
                .split_once('\n')
                .map(|(head, _)| head)
                .unwrap_or(self.message.as_str());
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
                msg = caret_msg,
            )?;
        }

        Ok(())
    }
}

/// Compute the display width needed for a line number.
fn line_num_width(n: usize) -> usize {
    if n == 0 {
        return 1;
    }
    ((n as f64).log10().floor() as usize) + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_source_line() {
        let src = "line one\nline two\nline three";
        assert_eq!(get_source_line(src, 1), Some("line one".to_string()));
        assert_eq!(get_source_line(src, 2), Some("line two".to_string()));
        assert_eq!(get_source_line(src, 3), Some("line three".to_string()));
        assert_eq!(get_source_line(src, 4), None);
        assert_eq!(get_source_line(src, 0), None);
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
        let err = SourceError {
            kind: ErrorKind::Parse,
            message: "expected expression".to_string(),
            span: Span::with_offset(5, 12, 0),
            source_line: Some("    Err(e) -> println(\"error\")".to_string()),
            file: Some("test.silt".to_string()),
            is_warning: false,
        };
        let output = format!("{err}");
        assert!(output.contains("error[parse]"));
        assert!(output.contains("expected expression"));
        assert!(output.contains("test.silt:5:12"));
        assert!(output.contains("Err(e) -> println(\"error\")"));
        assert!(output.contains("^"));
    }

    #[test]
    fn test_source_error_type_warning() {
        let err = SourceError {
            kind: ErrorKind::Type,
            message: "type mismatch".to_string(),
            span: Span::with_offset(3, 5, 0),
            source_line: Some("let x = true + 1".to_string()),
            file: Some("test.silt".to_string()),
            is_warning: true,
        };
        let output = format!("{err}");
        assert!(output.contains("warning[type]"));
        assert!(output.contains("type mismatch"));
    }

    #[test]
    fn test_source_error_type_error() {
        let err = SourceError {
            kind: ErrorKind::Type,
            message: "type mismatch".to_string(),
            span: Span::with_offset(3, 5, 0),
            source_line: Some("let x = true + 1".to_string()),
            file: Some("test.silt".to_string()),
            is_warning: false,
        };
        let output = format!("{err}");
        assert!(output.contains("error[type]"));
        assert!(output.contains("type mismatch"));
    }

    #[test]
    fn test_source_error_runtime_no_span() {
        let err = SourceError::runtime("division by zero", Some("test.silt".to_string()));
        let output = format!("{err}");
        assert!(output.contains("error[runtime]"));
        assert!(output.contains("division by zero"));
        // No source line or location for runtime errors without spans
        assert!(!output.contains("-->"));
    }

    #[test]
    fn test_from_lex_error() {
        let lex_err = LexError {
            message: "unexpected character: '@'".to_string(),
            span: Span::with_offset(1, 5, 4),
        };
        let source = "let @x = 42";
        let err = SourceError::from_lex_error(&lex_err, source, "test.silt");
        assert_eq!(err.kind, ErrorKind::Lex);
        assert_eq!(err.source_line, Some("let @x = 42".to_string()));
        assert_eq!(err.file, Some("test.silt".to_string()));
    }

    #[test]
    fn test_from_parse_error() {
        let parse_err = ParseError {
            message: "expected identifier, found +".to_string(),
            span: Span::with_offset(2, 7, 15),
        };
        let source = "let x = 42\nlet + = 1";
        let err = SourceError::from_parse_error(&parse_err, source, "test.silt");
        assert_eq!(err.kind, ErrorKind::Parse);
        assert_eq!(err.source_line, Some("let + = 1".to_string()));
    }
}
