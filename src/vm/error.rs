//! VM error type.

use crate::lexer::Span;

#[derive(Debug, Clone)]
pub struct VmError {
    pub message: String,
    /// If true, this error signals a cooperative yield, not a real error.
    pub is_yield: bool,
    /// Source span where the error occurred (if available).
    pub span: Option<Span>,
    /// Call stack at the time of the error: (function_name, span).
    pub call_stack: Vec<(String, Span)>,
}

impl VmError {
    pub fn new(message: String) -> Self {
        VmError {
            message,
            is_yield: false,
            span: None,
            call_stack: Vec::new(),
        }
    }

    pub(crate) fn yield_signal() -> Self {
        VmError {
            message: String::new(),
            is_yield: true,
            span: None,
            call_stack: Vec::new(),
        }
    }
}

impl std::fmt::Display for VmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "VM error: {}", self.message)?;
        if let Some(span) = self.span {
            write!(f, " at line {}, column {}", span.line, span.col)?;
        }
        // Only show the call stack if it has at least two meaningful (non-synthetic)
        // frames — a single-frame "stack" would just restate the error site above.
        let meaningful: Vec<_> = self
            .call_stack
            .iter()
            .filter(|(name, _)| !name.starts_with('<'))
            .collect();
        if meaningful.len() >= 2 {
            write!(f, "\ncall stack:")?;
            for (name, frame_span) in &meaningful {
                if frame_span.line > 0 {
                    write!(
                        f,
                        "\n  -> {} at line {}, column {}",
                        name, frame_span.line, frame_span.col
                    )?;
                } else {
                    write!(f, "\n  -> {name}")?;
                }
            }
        }
        Ok(())
    }
}

impl std::error::Error for VmError {}

/// Render a filtered view of a call stack as human-readable lines, applying
/// the same head/tail truncation used by `silt run`.  Synthetic frames
/// (`<script>`, `<call:...>`) are dropped.  Each returned line is already
/// prefixed with "  -> " and has no trailing newline.
///
/// `format_frame` turns a (name, span) pair into its location string —
/// callers pass the exact formatting they want (e.g. `file:line:col` for
/// `silt run`, `<declaration>` for REPL frames whose line numbers would
/// be misleading after span adjustment).
///
/// Returns an empty vec when the filtered stack is too short to be
/// informative (a single-frame stack would just restate the error site).
pub fn render_call_stack<F>(call_stack: &[(String, Span)], mut format_frame: F) -> Vec<String>
where
    F: FnMut(&str, &Span) -> String,
{
    let meaningful: Vec<&(String, Span)> = call_stack
        .iter()
        .filter(|(name, _)| !name.starts_with('<'))
        .collect();
    let any_real_span = meaningful.iter().any(|(_, s)| s.line > 0);
    if meaningful.len() < 2 || !any_real_span {
        return Vec::new();
    }
    let head = 10;
    let tail = 5;
    let mut out = Vec::new();
    if meaningful.len() <= head + tail {
        for (name, span) in &meaningful {
            out.push(format!("  -> {}  at {}", name, format_frame(name, span)));
        }
    } else {
        for (name, span) in &meaningful[..head] {
            out.push(format!("  -> {}  at {}", name, format_frame(name, span)));
        }
        let omitted = meaningful.len() - head - tail;
        out.push(format!("  ... ({omitted} more frames)"));
        for (name, span) in &meaningful[meaningful.len() - tail..] {
            out.push(format!("  -> {}  at {}", name, format_frame(name, span)));
        }
    }
    out
}
