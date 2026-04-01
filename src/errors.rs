use std::fmt;

use crate::lexer::{LexError, Span};
use crate::parser::ParseError;
use crate::typechecker::TypeError;

// ── Error kind ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ErrorKind {
    Lex,
    Parse,
    Type,
    Runtime,
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErrorKind::Lex => write!(f, "lex"),
            ErrorKind::Parse => write!(f, "parse"),
            ErrorKind::Type => write!(f, "type"),
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
        let source_line = get_source_line(source, err.span.line);
        Self {
            kind: ErrorKind::Lex,
            message: err.message.clone(),
            span: err.span,
            source_line,
            file: Some(file.into()),
            is_warning: false,
        }
    }

    pub fn from_parse_error(err: &ParseError, source: &str, file: impl Into<String>) -> Self {
        let source_line = get_source_line(source, err.span.line);
        Self {
            kind: ErrorKind::Parse,
            message: err.message.clone(),
            span: err.span,
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

    pub fn runtime_at(message: impl Into<String>, span: Span, source: &str, file: impl Into<String>) -> Self {
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

/// Check whether stderr is a terminal (for ANSI color support).
fn use_color() -> bool {
    unsafe { libc_isatty(2) != 0 }
}

unsafe extern "C" {
    #[link_name = "isatty"]
    fn libc_isatty(fd: i32) -> i32;
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

            write!(
                f,
                "\n {cyan}{gutter:>width$} |{reset} {spacing}{label_color}{bold}{caret} {msg}{reset}",
                cyan = c.cyan,
                gutter = "",
                width = gutter_width,
                reset = c.reset,
                spacing = spacing,
                label_color = label_color,
                bold = c.bold,
                caret = "^",
                msg = self.message,
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
