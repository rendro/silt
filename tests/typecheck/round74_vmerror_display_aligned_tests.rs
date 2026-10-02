//! Round 74 regression — `VmError::Display` must render its call stack
//! through the same `render_call_stack` helper as the production CLIs.
//!
//! ## The bug
//!
//! Two divergent renderings used to live in `src/vm/error.rs`:
//!
//! 1. `VmError::Display` filtered the stack with `!name.starts_with('<')`,
//!    silently dropping any `<module:...>` frame, and used the format
//!    string `"  -> {name} at line {N}, column {M}"` (ONE space before
//!    `at`).
//!
//! 2. `render_call_stack` (used by `silt run`, `silt test`, and the
//!    REPL) filtered with `!name.starts_with('<') ||
//!    name.starts_with("<module:")`, KEEPING module frames, and used
//!    the format string `"  -> {name}  at {format_frame(...)}"` (TWO
//!    spaces before `at`).
//!
//! A bare `format!("{}", vm_err)` therefore lost module-init provenance
//! and produced a different line shape than the canonical CLI output.
//!
//! ## The fix
//!
//! `VmError::Display` now delegates its call-stack rendering to
//! `render_call_stack` with a path-free closure
//! (`vm_error_display_frame`).  Both paths share one filter and one
//! line shape; this test pins both invariants.

use silt::source::{FileId, Span};
use silt::vm::VmError;
use silt::vm::error::{render_call_stack, vm_error_display_frame};

/// A span at byte `at` of a file.
fn span(at: u32) -> Span {
    Span::point(FileId::default(), at)
}

/// Builds a `VmError` whose call stack mixes a real user frame, a
/// `<module:...>` frame (which the OLD Display would silently drop),
/// and a `<script>` synthetic frame (which both helpers should drop).
fn err_with_mixed_stack() -> VmError {
    let mut e = VmError::new("boom".to_string());
    e.span = Some(span(73));
    e.call_stack = vec![
        ("inner".to_string(), span(73)),
        ("<module:foo>".to_string(), span(21)),
        ("main".to_string(), span(11)),
        ("<script>".to_string(), Span::BUILTIN),
    ];
    e
}

/// The Display path and `render_call_stack(..., vm_error_display_frame)`
/// must agree byte-for-byte on the call-stack section.
#[test]
fn display_call_stack_matches_render_call_stack_byte_equal() {
    let e = err_with_mixed_stack();

    // Canonical lines, as produced by the shared helper.
    let canonical_lines = render_call_stack(&e.call_stack, vm_error_display_frame);
    assert!(
        !canonical_lines.is_empty(),
        "test fixture should yield ≥2 meaningful frames; got {canonical_lines:?}"
    );
    let canonical_section = format!("call stack:\n{}", canonical_lines.join("\n"));

    // Display rendering — extract the call-stack section.
    let displayed = format!("{e}");
    let cs_idx = displayed
        .find("call stack:")
        .expect("Display must emit a `call stack:` section for a multi-frame stack");
    let display_section = &displayed[cs_idx..];

    assert_eq!(
        display_section, canonical_section,
        "VmError::Display call-stack section must be byte-equal to \
         render_call_stack(..., vm_error_display_frame).  This is the \
         round-74 GAP: any drift here means a bare `format!(\"{{e}}\")` \
         renders a different stack than `silt run`."
    );
}

/// Positive assertion: `<module:...>` frames are no longer dropped by
/// the Display filter.  This is the user-visible half of the GAP — the
/// OLD code would silently lose module-init provenance.
#[test]
fn display_keeps_module_frames() {
    let e = err_with_mixed_stack();
    let displayed = format!("{e}");

    assert!(
        displayed.contains("<module:foo>"),
        "VmError::Display must keep `<module:...>` frames (round-74 GAP); \
         got:\n{displayed}"
    );
    // And the synthetic `<script>` frame must still be filtered out.
    assert!(
        !displayed.contains("<script>"),
        "VmError::Display must still drop synthetic `<script>` frames; \
         got:\n{displayed}"
    );
}

/// Lock the canonical line shape: TWO spaces before `at`, and a
/// path-free `"byte N"` location (the Display has no source map to turn
/// an offset into a line and column).
/// This is the second half of the GAP — the OLD Display used ONE space
/// and the format `"  -> {name} at line N, column M"`.
#[test]
fn display_uses_two_space_at_separator() {
    let e = err_with_mixed_stack();
    let displayed = format!("{e}");

    assert!(
        displayed.contains("  -> inner  at byte 73"),
        "expected canonical two-space `  at ` separator; got:\n{displayed}"
    );
    assert!(
        displayed.contains("  -> <module:foo>  at byte 21"),
        "expected canonical rendering for module frame; got:\n{displayed}"
    );
    // The OLD one-space format must NOT appear.
    assert!(
        !displayed.contains(" -> inner at line"),
        "OLD one-space `at` separator must not reappear; got:\n{displayed}"
    );
}

/// The Display header is unchanged by this fix; pin it so a future
/// edit doesn't regress the round-36 `error[runtime]:` canonicalization
/// while reshuffling the call-stack rendering.
#[test]
fn display_header_unchanged() {
    let e = err_with_mixed_stack();
    let displayed = format!("{e}");
    assert!(
        displayed.starts_with("error[runtime]: boom"),
        "Display header must remain `error[runtime]: <msg>`; got:\n{displayed}"
    );
    assert!(
        displayed.contains("\n --> <input> byte 73"),
        "Display must emit a `-->` locator line for an error with a span; got:\n{displayed}"
    );
}

/// Frames of code silt adds itself (`Span::BUILTIN`) get the path-free
/// `"<unknown location>"` placeholder rather than the OLD bare-name
/// fallback (`"  -> name"` with no `at` clause).  This keeps every
/// frame line shape-consistent.
#[test]
fn display_zero_line_frame_uses_unknown_location() {
    let mut e = VmError::new("boom".to_string());
    e.span = Some(span(51));
    // Two real frames so the helper doesn't filter the stack as
    // single-frame, plus one zero-span user frame in the middle.
    e.call_stack = vec![
        ("inner".to_string(), span(51)),
        ("middle".to_string(), Span::BUILTIN),
        ("outer".to_string(), span(11)),
    ];
    let displayed = format!("{e}");

    assert!(
        displayed.contains("  -> middle  at <unknown location>"),
        "zero-line frame should render with `<unknown location>`; got:\n{displayed}"
    );
    // And the byte-equal invariant still holds for zero-line frames.
    let canonical_lines = render_call_stack(&e.call_stack, vm_error_display_frame);
    let canonical_section = format!("call stack:\n{}", canonical_lines.join("\n"));
    let cs_idx = displayed.find("call stack:").unwrap();
    assert_eq!(&displayed[cs_idx..], canonical_section);
}
