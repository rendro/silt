//! VM error type.

use crate::diagnostic::{Code, Diagnostic};
use crate::source::Span;

#[derive(Debug, Clone)]
pub struct VmError {
    pub message: String,
    /// What to do about it, one line each: the diagnostic's help.
    pub help: Vec<String>,
    /// If true, an instruction met a value of a kind it cannot work on
    /// (see [`VmError::type_confusion`]).
    pub type_confusion: bool,
    /// If true, the program was ended because it had run the steps its
    /// embedder gave it ([`Vm::set_step_budget`](super::Vm::set_step_budget)).
    pub out_of_steps: bool,
    /// Source span where the error occurred (if available).
    pub span: Option<Span>,
    /// Call stack at the time of the error: (function_name, span).
    pub call_stack: Vec<(String, Span)>,
}

impl VmError {
    pub fn new(message: String) -> Self {
        VmError {
            message,
            help: Vec::new(),
            type_confusion: false,
            out_of_steps: false,
            span: None,
            call_stack: Vec::new(),
        }
    }

    /// An instruction met a value of a kind it cannot work on (`+` a
    /// String, a call of an Int, a field of a record that has none of
    /// the name). The code a VM runs is verified, so the instruction is
    /// well-formed; the value is one the typechecker said could not be
    /// there. Every such error of the instruction loop is made here and
    /// has its own diagnostic code ([`Code::TypeConfusion`]): it is
    /// reachable exactly when the typechecker is unsound.
    pub fn type_confusion(message: impl Into<String>) -> Self {
        VmError {
            type_confusion: true,
            ..VmError::new(message.into())
        }
    }

    /// The step budget of the program is used up: what still runs of
    /// it ends with this.
    pub(crate) fn out_of_steps() -> Self {
        VmError {
            out_of_steps: true,
            ..VmError::new("the step budget is used up".into())
        }
    }

    /// The error with `help` as a help line.
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help.push(help.into());
        self
    }

    /// The error as a diagnostic: the first line of the message is its
    /// message, and the rest of the message, as it is (a `panic` text or
    /// a builtin's report can go on for several lines), its one note.
    /// The labels are the call stack, innermost frame first. An error
    /// with no span is about no place of the program, and has
    /// [`Span::BUILTIN`].
    pub fn to_diagnostic(&self) -> Diagnostic {
        let (head, rest) = match self.message.split_once('\n') {
            Some((head, rest)) => (head, Some(rest)),
            None => (self.message.as_str(), None),
        };
        let code = match self.type_confusion {
            true => Code::TypeConfusion,
            false => Code::RuntimeError,
        };
        let mut d = Diagnostic::error(code, self.span.unwrap_or(Span::BUILTIN), head);
        d.notes.extend(rest.map(str::to_string));
        d.help = self.help.clone();
        d.labels = self
            .call_stack
            .iter()
            .map(|(name, span)| (*span, name.clone()))
            .collect();
        d
    }
}

/// The message alone: a front door renders a runtime error through
/// [`VmError::to_diagnostic`] and its source map, which give it a place.
impl std::fmt::Display for VmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "error[runtime]: {}", self.message)
    }
}

impl std::error::Error for VmError {}

/// Render a filtered view of a call stack as human-readable lines, applying
/// the same head/tail truncation used by `silt run`.  Synthetic frames
/// (`<script>`, `<call:...>`) are dropped, but `<module:...>` frames are
/// kept because they carry useful provenance for module-init errors —
/// the call site that triggered the module's load and the source file
/// that owns the failing top-level statement.  `<repl:n>` frames are kept
/// for the same reason: the function that holds the statements of the
/// REPL's `n`th entry marks the call site the user typed.  Each returned
/// line is already prefixed with "  -> " and has no trailing newline.
///
/// `format_frame` turns a (name, span) pair into its location string —
/// callers pass the exact formatting they want (e.g. `file:line:col`).
///
/// Returns an empty vec when the filtered stack is too short to be
/// informative (a single-frame stack would just restate the error site).
pub fn render_call_stack<F>(call_stack: &[(String, Span)], mut format_frame: F) -> Vec<String>
where
    F: FnMut(&str, &Span) -> String,
{
    let meaningful: Vec<&(String, Span)> = call_stack
        .iter()
        .filter(|(name, _)| {
            !name.starts_with('<') || name.starts_with("<module:") || name.starts_with("<repl:")
        })
        .collect();
    let any_real_span = meaningful.iter().any(|(_, s)| s.is_in_source());
    if meaningful.len() < 2 || !any_real_span {
        return Vec::new();
    }
    let head = 10;
    let tail = 5;
    // A frame's location can be a dependency's path, which comes from a
    // manifest: it goes through the same display rule as other printed
    // manifest values.
    let mut line = |name: &str, span: &Span| {
        let at = format_frame(name, span);
        format!(
            "  -> {}  at {}",
            crate::git::escape_for_display(name),
            crate::git::escape_for_display(&at)
        )
    };
    let mut out = Vec::new();
    if meaningful.len() <= head + tail {
        for (name, span) in &meaningful {
            out.push(line(name, span));
        }
    } else {
        for (name, span) in &meaningful[..head] {
            out.push(line(name, span));
        }
        let omitted = meaningful.len() - head - tail;
        out.push(format!("  ... ({omitted} more frames)"));
        for (name, span) in &meaningful[meaningful.len() - tail..] {
            out.push(line(name, span));
        }
    }
    out
}
