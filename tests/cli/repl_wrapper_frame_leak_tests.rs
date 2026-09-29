//! Regression locks: the REPL runtime-error call stack must not leak the
//! synthetic expression-wrapper frame name `__repl_eval_<n>`.
//!
//! Background: round 74 renamed the REPL's per-expression wrapper from
//! `fn main` to a unique synthetic `fn __repl_eval_<n>` (to stop a
//! user-defined `fn main()` from shadowing the wrapper and
//! self-recursing). But `render_call_stack`'s synthetic-frame filter
//! only drops names starting with `<`, so the renamed wrapper frame
//! sailed through and the user saw an internal implementation name:
//!
//! ```text
//! error[runtime]: division by zero
//!  --> <declaration>
//!   -> boom  at <declaration>
//!   -> __repl_eval_0  at <declaration>
//! ```
//!
//! Fix (src/repl.rs::repl_call_stack_lines): relabel wrapper frames to
//! `<repl>` before rendering — keeping the frame (it marks the REPL
//! top-level call site, and dropping it would shrink two-frame stacks
//! below `render_call_stack`'s two-meaningful-frames threshold, erasing
//! the real user frame too). `render_call_stack` (src/vm/error.rs)
//! keeps the `<repl>` label explicitly, like `<module:...>`.
//!
//! These are unit tests against the `pub` production helper
//! `repl_call_stack_lines`, pinning the relabel and its precision
//! (numeric suffixes only). The end-to-end `silt repl` run of the exact
//! repro is the golden case
//! `tests/golden/cli/repl/repl_wrapper_frame_leak_tests__wrapper_name_never_shown`.

use silt::lexer::Span;
use silt::repl::repl_call_stack_lines;

fn span(line: usize, col: usize) -> Span {
    Span::new(line, col)
}

/// The exact shape from the bug report: a user frame plus the synthetic
/// wrapper frame. The rendered lines must show the user frame and a
/// `<repl>` frame — and must not contain the internal wrapper name.
#[test]
fn wrapper_frame_is_relabelled_repl_not_leaked() {
    let stack = vec![
        ("boom".to_string(), span(1, 13)),
        ("__repl_eval_3".to_string(), span(1, 1)),
    ];
    let lines = repl_call_stack_lines(&stack);
    let rendered = lines.join("\n");

    assert!(
        rendered.contains("-> boom"),
        "user frame `boom` must still render, got:\n{rendered}"
    );
    assert!(
        rendered.contains("-> <repl>"),
        "wrapper frame must render under the `<repl>` label, got:\n{rendered}"
    );
    assert!(
        !rendered.contains("__repl_eval"),
        "internal wrapper name `__repl_eval_<n>` leaked into rendered call stack:\n{rendered}"
    );
}

/// Relabelling must not swallow the user's real frames: with the
/// wrapper kept (as `<repl>`), a `boom -> wrapper` stack still has two
/// meaningful frames and renders. A fix that *dropped* the wrapper
/// instead would leave a single-frame stack, which `render_call_stack`
/// filters to nothing — erasing `boom` from the output.
#[test]
fn relabelled_stack_keeps_two_frames() {
    let stack = vec![
        ("boom".to_string(), span(2, 1)),
        ("__repl_eval_0".to_string(), span(1, 1)),
    ];
    let lines = repl_call_stack_lines(&stack);
    assert_eq!(
        lines.len(),
        2,
        "expected exactly 2 rendered frames (boom + <repl>), got {}: {:?}",
        lines.len(),
        lines
    );
}

/// Precision lock: only genuine wrapper names (`__repl_eval_` + digits)
/// are relabelled. A (pathological) user-defined function whose name
/// merely shares the prefix must render under its own name.
#[test]
fn non_numeric_suffix_is_not_relabelled() {
    let stack = vec![
        ("__repl_eval_helper".to_string(), span(1, 1)),
        ("caller".to_string(), span(2, 1)),
    ];
    let lines = repl_call_stack_lines(&stack);
    let rendered = lines.join("\n");
    assert!(
        rendered.contains("-> __repl_eval_helper"),
        "user-defined `__repl_eval_helper` must keep its own name, got:\n{rendered}"
    );
    assert!(
        !rendered.contains("-> <repl>"),
        "no frame should be relabelled `<repl>` here, got:\n{rendered}"
    );
}
