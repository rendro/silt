//! VM error type.

use crate::diagnostic::{Code, Diagnostic};
use crate::source::Span;

/// The handle of the task a join took a failure from
/// ([`VmError::taken_from`]).
#[derive(Clone)]
pub(crate) struct TakenFrom(pub(crate) std::sync::Arc<crate::runtime::handle::TaskHandle>);

impl std::fmt::Debug for TakenFrom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<handle:{}>", self.0.id)
    }
}

#[derive(Debug, Clone)]
pub struct VmError {
    pub message: String,
    /// What to do about it, one line each: the diagnostic's help.
    pub(crate) help: Vec<String>,
    /// The help lines that name a place of the program: the line at the
    /// index has `{at}` where the place of the span is shown (see
    /// `Diagnostic::help_at`).
    pub(crate) help_at: Box<[(usize, Span)]>,
    /// If true, an instruction met a value of a kind it cannot work on
    /// (see [`VmError::type_confusion`]).
    pub(crate) type_confusion: bool,
    /// If true, the error ends what it is raised in, and `task.guard`
    /// does not catch it ([`VmError::catchable`]): a `process.exit`
    /// (also one in a test), the step budget used up, a deadlock of the
    /// program's thread, a type confusion, an internal error of the VM,
    /// a panic of a builtin or a host function, a clock that failed, a
    /// VM or a runtime that is gone. A join of a task that failed with
    /// such an error raises it as it is, so it ends the joiner too.
    pub(crate) ends: bool,
    /// If true, the program had run the steps its embedder gave it
    /// ([`Vm::set_step_budget`](super::Vm::set_step_budget)) and was
    /// ended for it: this is the budget's error, the error of a join
    /// of a task it ended, or a deadlock that followed from it.
    pub out_of_steps: bool,
    /// The code the program called `process.exit` with, when that is
    /// what ended the run ([`Exit::EndsRun`](super::Exit::EndsRun)):
    /// the error of every run of the VM from then on.
    pub exit: Option<u8>,
    /// If true, this is a `process.exit` that a test, or a test file's
    /// top-level code, called ([`Exit::FailsTest`](super::Exit::FailsTest),
    /// [`Exit::FailsTestFile`](super::Exit::FailsTestFile)): it fails the test
    /// wherever it was raised, also where the failure of a callback is
    /// otherwise handled (an `http.serve` handler's).
    // (Read where a callback's failure is handled: in a build without
    // `http`, nowhere.)
    #[cfg_attr(not(feature = "http"), allow(dead_code))]
    pub(crate) exit_in_a_test: bool,
    /// The text of the `Err(e)` the program's own code returned when it ran
    /// to its end (`e` as `println` writes it): `main`
    /// ([`Vm::run_program`](super::Vm::run_program)), or a test function
    /// (`Vm::call_test`). The span is the
    /// function's name, and there is no call stack. `silt run` and the
    /// playground write a `main` that returned `Err` as the one line
    /// `error: <text>`: it is the program's message to its user, not
    /// silt's.
    pub returned: Option<String>,
    /// With [`VmError::returned`]: the `Display` impl that showed the
    /// `Err` failed, and the text is the payload's structure. What
    /// failed, for the note a front door writes after the text
    /// (`DisplayFailure::note`).
    pub display_failure: Option<Box<DisplayFailure>>,
    /// The task whose failure this is, when a `task.join_handle` took it from
    /// that task: a cancel that dismisses it hands it back to the task,
    /// whose failure is then reported as never joined
    /// (`Scheduler::cancel`).
    pub(crate) taken_from: Option<TakenFrom>,
    /// If true, the failure came through a `task.join_handle`, and its message
    /// says so: `joined task failed: <msg>`. A join of a task that
    /// failed this way keeps the message as it is, so the words are
    /// there once however many joins the failure went through; the call
    /// stack shows the tasks.
    pub(crate) through_a_join: bool,
    /// Source span where the error occurred (if available).
    pub span: Option<Span>,
    /// Call stack at the time of the error: (function_name, span).
    pub(crate) call_stack: Vec<(String, Span)>,
}

impl VmError {
    pub fn new(message: String) -> Self {
        VmError {
            message,
            help: Vec::new(),
            help_at: Box::default(),
            type_confusion: false,
            ends: false,
            out_of_steps: false,
            exit: None,
            exit_in_a_test: false,
            returned: None,
            display_failure: None,
            taken_from: None,
            through_a_join: false,
            span: None,
            call_stack: Vec::new(),
        }
    }

    /// An instruction met a value of a kind it cannot work on (`+` a
    /// String, a call of an Int, a field of a record that has none of
    /// the name). The code a VM runs is verified, so the instruction is
    /// well-formed; the value is one the typechecker said could not be
    /// there. Every such error of the instruction loop is made here and
    /// has its own diagnostic code (`Code::TypeConfusion`): it is
    /// reachable exactly when the typechecker is unsound.
    pub(crate) fn type_confusion(message: impl Into<String>) -> Self {
        VmError {
            type_confusion: true,
            ..VmError::ending(message.into())
        }
    }

    /// An error that ends what it is raised in ([`VmError::ends`]).
    pub(crate) fn ending(message: String) -> Self {
        VmError {
            ends: true,
            ..VmError::new(message)
        }
    }

    /// An error of the VM itself, `internal VM error: <what>`: what the
    /// VM found in its own state cannot be, and the run ends.
    pub(crate) fn internal(what: impl std::fmt::Display) -> Self {
        VmError::ending(format!("internal VM error: {what}"))
    }

    /// Whether `task.guard` catches the error: a runtime error of the
    /// program, not one that ends what it is raised in
    /// ([`VmError::ends`]).
    pub(crate) fn catchable(&self) -> bool {
        !self.ends
    }

    /// The program's `panic(text)`: `panic: <text>`. The text is the
    /// program's, and the report is silt's: each of its lines is shown by
    /// the display rule of silt's messages (`git::escape_for_display`),
    /// each control character (a carriage return, a tab, ESC), unusual
    /// space or invisible character as an escape (`\r`, `\u{1b}`). So it
    /// cannot move the cursor, colour or clear the terminal, in every report
    /// of it (`silt run`, `silt test`, the REPL, a joined task, a task
    /// nobody joined). Its line ends stay: a panic's text may go on for
    /// several lines, the first the headline and the others a note under it
    /// (`VmError::to_diagnostic`). `main`'s `Err` is the program's own
    /// message and keeps its text (`VmError::of_a_returned_err`).
    pub(crate) fn panic(text: &str) -> Self {
        let lines: Vec<String> = text
            .split('\n')
            .map(crate::git::escape_for_display)
            .collect();
        VmError::new(format!("panic: {}", lines.join("\n")))
    }

    /// The step budget of the program is used up: what still runs of
    /// it ends with this.
    pub(crate) fn budget_used_up() -> Self {
        VmError {
            out_of_steps: true,
            ..VmError::ending("the step budget is used up".into())
        }
    }

    /// The program called `process.exit(code)`, which ended the run:
    /// what still runs of it ends with this.
    pub(crate) fn exited(code: u8) -> Self {
        VmError {
            exit: Some(code),
            ..VmError::ending(format!("process.exit({code}) ended the program"))
        }
    }

    /// A `process.exit(code)` in a test ([`Exit::FailsTest`](super::Exit::FailsTest)).
    pub(crate) fn exit_in_a_test(code: u8) -> Self {
        VmError {
            exit_in_a_test: true,
            ..VmError::ending(format!(
                "process.exit({code}) was called in a test: a test ends by returning, or by \
                 failing an assertion"
            ))
        }
    }

    /// A `process.exit(code)` in the top-level code of a test file
    /// ([`Exit::FailsTestFile`](super::Exit::FailsTestFile)).
    pub(crate) fn exit_in_a_test_file(code: u8) -> Self {
        VmError {
            exit_in_a_test: true,
            ..VmError::ending(format!(
                "process.exit({code}) was called in a test file's top-level code: a test file \
                 ends by returning"
            ))
        }
    }

    /// `function` (`main`, a test function), named at `at`, returned
    /// `Err(payload)`; `shown` is the payload as `println` writes it, or
    /// its structure where its `Display` impl failed (`failure`).
    pub(crate) fn of_a_returned_err(
        function: &str,
        shown: &str,
        at: Span,
        failure: Option<DisplayFailure>,
    ) -> Self {
        VmError {
            returned: Some(shown.to_string()),
            display_failure: failure.map(Box::new),
            span: Some(at),
            ..VmError::new(format!("{function} returned Err: {shown}"))
        }
    }

    /// The error with `help` as a help line.
    pub(crate) fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help.push(help.into());
        self
    }

    /// The error with `help` as a help line that names the place `at`:
    /// its `{at}` shows where that is.
    pub(crate) fn with_help_at(mut self, help: impl Into<String>, at: Span) -> Self {
        let mut help_at = std::mem::take(&mut self.help_at).into_vec();
        help_at.push((self.help.len(), at));
        self.help_at = help_at.into_boxed_slice();
        self.help.push(help.into());
        self
    }

    /// The error as `silt run` prints it, its places read from the files
    /// of `session`: the header, the place with its source line, and the
    /// call stack when it has more than one frame; for a `main` that
    /// returned `Err`, the one line `error: <text>` (and the note of its
    /// `Display` impl where that failed).
    pub fn render(&self, session: &crate::session::Session) -> String {
        self.render_in(session.sources(), &crate::diagnostic::COLORS_OFF)
    }

    /// [`VmError::render`], the files named by `view`, in the colours `c`:
    /// what `silt run` prints for the error that ended `main` (in its
    /// colours, `active_colors`).
    pub(crate) fn render_in(
        &self,
        view: &dyn crate::diagnostic::SourceView,
        c: &crate::diagnostic::Colors,
    ) -> String {
        match &self.returned {
            Some(text) => self.returned_text(text, view),
            None => self.diagnostic_text(view, c),
        }
    }

    /// The `Err` that `main` returned, as `silt run` writes it: the line
    /// `error: <text>`, and where the payload's `Display` impl failed and
    /// `text` is its structure, the line `note: <what failed>`.
    pub(crate) fn returned_text(
        &self,
        text: &str,
        view: &dyn crate::diagnostic::SourceView,
    ) -> String {
        match &self.display_failure {
            Some(failure) => format!("error: {text}\nnote: {}", failure.note(view)),
            None => format!("error: {text}"),
        }
    }

    /// The error as a diagnostic of `silt run` is printed, the files named
    /// by `view`: the header, the place with its source line, then the
    /// call stack when it has more than one meaningful frame; a `Display`
    /// impl that failed (of a test that returned `Err`) as a note; in the
    /// colours `c`.
    pub(crate) fn diagnostic_text(
        &self,
        view: &dyn crate::diagnostic::SourceView,
        c: &crate::diagnostic::Colors,
    ) -> String {
        let mut d = self.to_diagnostic();
        if let Some(failure) = &self.display_failure {
            d.notes.push(failure.note(view));
        }
        crate::diagnostic::render_human_in(view, &d, c)
    }

    /// The error as a diagnostic: the first line of the message is its
    /// message, and the rest of the message, as it is (a `panic` text or
    /// a builtin's report can go on for several lines), its one note.
    /// The labels are the call stack, innermost frame first. An error
    /// with no span is about no place of the program, and has
    /// `Span::BUILTIN`.
    pub(crate) fn to_diagnostic(&self) -> Diagnostic {
        let (head, rest) = match self.message.split_once('\n') {
            Some((head, rest)) => (head, Some(rest)),
            None => (self.message.as_str(), None),
        };
        let code = if self.type_confusion {
            Code::TypeConfusion
        } else if self.returned.is_some() {
            Code::MainReturnedErr
        } else {
            Code::RuntimeError
        };
        let mut d = Diagnostic::error(code, self.span.unwrap_or(Span::BUILTIN), head);
        d.notes.extend(rest.map(str::to_string));
        d.help = self.help.clone();
        d.help_at = self.help_at.clone();
        d.labels = self
            .call_stack
            .iter()
            .map(|(name, span)| (*span, name.clone()))
            .collect();
        d
    }
}

/// A `Display` impl that failed while a value was shown where its
/// structure is shown instead (the `Err` that `main` or a test returned,
/// a REPL entry's value): the type whose impl it was, as values of it
/// are shown, what its failure said, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayFailure {
    pub type_name: String,
    /// The failure's message: its first line, cut at
    /// [`DisplayFailure::MOST`] bytes, and `…` where more is left out.
    pub message: String,
    pub span: Option<Span>,
}

impl DisplayFailure {
    /// The most of the failure's message a note gives: it is one line
    /// under the text it is about.
    pub(crate) const MOST: usize = 1_000;

    /// The note, with the place as `view` shows it: `the Display impl
    /// of E failed: panic: display panics (main.silt:3:32)`.
    pub fn note(&self, view: &dyn crate::diagnostic::SourceView) -> String {
        let place = (self.span)
            .filter(|span| *span != Span::BUILTIN)
            .map(|span| view.frame(span));
        let head = format!(
            "the Display impl of {} failed: {}",
            self.type_name, self.message
        );
        match place {
            Some(place) => format!("{head} ({place})"),
            None => head,
        }
    }
}

/// The message alone: a front door renders a runtime error through
/// `VmError::to_diagnostic` and its source map, which give it a place.
impl std::fmt::Display for VmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "error[runtime]: {}", self.message)
    }
}

impl std::error::Error for VmError {}

/// A list of too many elements to be made, or to be written out.
impl From<crate::value::TooLong> for VmError {
    fn from(too_long: crate::value::TooLong) -> VmError {
        VmError::new(too_long.to_string())
    }
}

/// Render a filtered view of a call stack as human-readable lines, applying
/// the same head/tail truncation used by `silt run`.  Synthetic frames
/// (`<script>`, `<call:...>`) are dropped, but `<module:...>` frames are
/// kept because they carry useful provenance for module-init errors —
/// the call site that triggered the module's load and the source file
/// that owns the failing top-level statement.  `<repl:n>` frames are kept
/// for the same reason: the function that holds the statements of the
/// REPL's `n`th entry marks the call site the user typed.  A closure is
/// listed like a function, under the name the compiler gave it
/// (`closure in main`).  Each returned
/// line is already prefixed with "  -> " and has no trailing newline.
///
/// `format_frame` turns a (name, span) pair into its location string —
/// callers pass the exact formatting they want (e.g. `file:line:col`).
///
/// Returns an empty vec when the filtered stack is too short to be
/// informative (a single-frame stack would just restate the error site).
pub(crate) fn render_call_stack<F>(
    call_stack: &[(String, Span)],
    mut format_frame: F,
) -> Vec<String>
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

#[cfg(test)]
mod tests {
    use super::*;

    /// What ends what it is raised in is made so where it is made; a
    /// runtime error of the program is caught.
    #[test]
    fn the_errors_that_end_what_they_are_raised_in_are_not_catchable() {
        let ending = [
            VmError::type_confusion("cannot call value of type Int"),
            VmError::budget_used_up(),
            VmError::exited(3),
            VmError::exit_in_a_test(3),
            VmError::exit_in_a_test_file(3),
            VmError::internal("a frame was resumed with its wait open"),
            VmError::ending("deadlock on main thread: channel receive with no counterparty".into()),
        ];
        for error in &ending {
            assert!(!error.catchable(), "{}", error.message);
        }
        assert!(VmError::new("division by zero".into()).catchable());
        assert!(VmError::panic("boom").catchable());
        assert_eq!(VmError::internal("x").message, "internal VM error: x",);
    }
}
