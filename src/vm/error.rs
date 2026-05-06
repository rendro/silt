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

/// The canonical "frame location" formatter used by `VmError::Display`.
///
/// Production CLIs (`silt run`, `silt test`, REPL) supply their own
/// `format_frame` closure to `render_call_stack` so they can use absolute
/// file paths.  `VmError::Display` has no access to such paths (it's a
/// fallback formatter that may be invoked from arbitrary sinks), so it
/// uses a path-free `"line N, column M"` shape — but it MUST go through
/// the same `render_call_stack` helper as the production paths, applying
/// the same `<module:...>`-keep filter and the same `"  -> name  at …"`
/// line layout.  Any drift between this helper and `render_call_stack`
/// would re-introduce the round-74 GAP (Display dropping module frames
/// silently, plus a one-vs-two-space `at` separator divergence).
pub fn vm_error_display_frame(_name: &str, span: &Span) -> String {
    if span.line > 0 {
        format!("line {}, column {}", span.line, span.col)
    } else {
        "<unknown location>".to_string()
    }
}

impl std::fmt::Display for VmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Canonicalize to the same `error[runtime]: <msg>` shape produced
        // by `SourceError::Display` for runtime diagnostics. Production
        // paths route around this via `SourceError::runtime_at` (round 36
        // fix), but this Display is an attractive nuisance: any fallback
        // `eprintln!("{e}")` on a bare VmError would previously re-emit
        // the raw `"VM error: ..."` prefix, leaking an internal label to
        // users. Matching SourceError's header means any such fallback
        // produces a correctly-formed diagnostic instead of a second
        // dialect. (Audit LATENT L3.)
        //
        // No span → no `-->` locator line; no source snippet (we don't
        // hold the source here). Call-stack rendering delegates to the
        // shared `render_call_stack` helper so the filter + line shape
        // can never drift from `silt run` / `silt test` / REPL output.
        // Round-74 GAP: previously this method had its own filter
        // (`!name.starts_with('<')`, dropping `<module:...>` frames) and
        // its own format string (one space before `at`), so a bare
        // `format!("{e}")` would silently lose module-init provenance
        // and use a different line shape than the production CLIs.
        //
        // NOTE: this Display intentionally does NOT do ANSI coloring —
        // SourceError::Display gates color on `isatty(stderr)`, but a
        // bare VmError may be formatted to arbitrary sinks (test logs,
        // panic messages, operator audits). Plain text is the safe
        // lowest-common-denominator for a fallback.
        write!(f, "error[runtime]: {}", self.message)?;
        if let Some(span) = self.span
            && span.line > 0
        {
            write!(f, "\n --> <input>:{}:{}", span.line, span.col)?;
        }
        let stack_lines = render_call_stack(&self.call_stack, vm_error_display_frame);
        if !stack_lines.is_empty() {
            write!(f, "\ncall stack:")?;
            for line in &stack_lines {
                write!(f, "\n{line}")?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for VmError {}

/// Render a filtered view of a call stack as human-readable lines, applying
/// the same head/tail truncation used by `silt run`.  Synthetic frames
/// (`<script>`, `<call:...>`) are dropped, but `<module:...>` frames are
/// kept because they carry useful provenance for module-init errors —
/// the call site that triggered the module's load and the source file
/// that owns the failing top-level statement.  Each returned line is
/// already prefixed with "  -> " and has no trailing newline.
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
        .filter(|(name, _)| !name.starts_with('<') || name.starts_with("<module:"))
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
