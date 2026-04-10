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
