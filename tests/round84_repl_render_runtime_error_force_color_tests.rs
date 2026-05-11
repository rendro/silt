//! Round 84 fix: the REPL helper
//! `silt::repl::render_runtime_error_without_source` previously always
//! emitted plain text, even with `FORCE_COLOR=1` set. That made the
//! REPL output asymmetric: span-in-range errors flowed through
//! `SourceError::Display` (which honors `FORCE_COLOR` / `NO_COLOR`)
//! and rendered colored, while span-less / out-of-range errors
//! emitted by the same REPL eval loop fell through this helper and
//! came out monochrome. Users who explicitly opt into color (via
//! `FORCE_COLOR` in a captured-stderr environment, or by running
//! interactively) saw a jarring mix of colored and plain runtime
//! errors in the same session.
//!
//! After the round-84 fix, the helper consults
//! `crate::errors::active_colors()` and emits ANSI escapes when
//! `use_color()` is true. This test locks the three env-var
//! configurations end-to-end so a future refactor can't silently
//! revert the behavior.
//!
//! Env-var safety: same pattern as
//! `tests/round83_use_color_env_vars_tests.rs` — all toggles live in
//! ONE `#[test]` function so cargo's per-test sequential model is
//! sufficient. We restore the inherited env-var state at the end so
//! the next test in the same binary isn't perturbed.

use silt::repl::render_runtime_error_without_source;

fn has_ansi(rendered: &str) -> bool {
    rendered.contains('\x1b')
}

/// SAFETY wrapper. The pre-1.80 contract for `set_var` requires no
/// concurrent readers; we satisfy that by sequencing all toggles in
/// a single `#[test]` and by knowing no other thread in the
/// per-test process reads `NO_COLOR` / `FORCE_COLOR` between
/// toggles.
fn set(key: &str, value: &str) {
    // SAFETY: see file-level note.
    unsafe { std::env::set_var(key, value) };
}

fn unset(key: &str) {
    // SAFETY: see `set`.
    unsafe { std::env::remove_var(key) };
}

#[test]
fn render_runtime_error_without_source_honors_no_color_and_force_color() {
    let saved_no_color = std::env::var_os("NO_COLOR");
    let saved_force_color = std::env::var_os("FORCE_COLOR");
    unset("NO_COLOR");
    unset("FORCE_COLOR");

    // Each case exercises both shapes the helper supports:
    //   * span-less (show_declaration_locator = false)
    //   * out-of-range (show_declaration_locator = true)
    // We probe both because the locator line itself has its own
    // color hook (the cyan `-->` arrow) and the `= note:` /
    // `= help:` continuation prefixes have another.
    let msg = "stack overflow\nhelp: increase recursion budget";

    // ── Case 1: FORCE_COLOR=1 forces ANSI escapes ──────────────────
    set("FORCE_COLOR", "1");
    let rendered_no_loc = render_runtime_error_without_source(msg, false);
    let rendered_with_loc = render_runtime_error_without_source(msg, true);
    assert!(
        has_ansi(&rendered_no_loc),
        "FORCE_COLOR=1 must produce ANSI escapes in the span-less branch, got:\n{rendered_no_loc:?}"
    );
    assert!(
        has_ansi(&rendered_with_loc),
        "FORCE_COLOR=1 must produce ANSI escapes in the locator branch, got:\n{rendered_with_loc:?}"
    );
    // Body content must still be present even with escapes interleaved.
    assert!(
        rendered_with_loc.contains("stack overflow"),
        "header text must survive colorization, got:\n{rendered_with_loc:?}"
    );
    assert!(
        rendered_with_loc.contains("<declaration>"),
        "locator text must survive colorization, got:\n{rendered_with_loc:?}"
    );
    assert!(
        rendered_with_loc.contains("increase recursion budget"),
        "help body must survive colorization, got:\n{rendered_with_loc:?}"
    );
    unset("FORCE_COLOR");

    // ── Case 2: NO_COLOR=1 suppresses ANSI even under FORCE_COLOR ──
    set("NO_COLOR", "1");
    set("FORCE_COLOR", "1");
    let rendered = render_runtime_error_without_source(msg, true);
    assert!(
        !has_ansi(&rendered),
        "NO_COLOR=1 must suppress escapes even when FORCE_COLOR=1, got:\n{rendered:?}"
    );
    // Plain text still carries the canonical header + locator + body.
    assert!(
        rendered.contains("error[runtime]: stack overflow"),
        "plain-text output must include canonical header, got:\n{rendered:?}"
    );
    assert!(
        rendered.contains("--> <declaration>"),
        "plain-text output must include locator, got:\n{rendered:?}"
    );
    assert!(
        rendered.contains("= help: increase recursion budget"),
        "plain-text output must include `= help:` continuation, got:\n{rendered:?}"
    );
    unset("NO_COLOR");
    unset("FORCE_COLOR");

    // ── Case 3: Both unset — fallthrough is well-formed ────────────
    // Under `cargo test`, stderr is captured (not a tty), so the
    // isatty branch returns false; we just assert the output is a
    // valid render and don't pin the escape decision.
    let rendered = render_runtime_error_without_source(msg, false);
    assert!(
        rendered.contains("stack overflow"),
        "fallthrough output must include the message body, got:\n{rendered:?}"
    );

    // Restore inherited env state.
    match saved_no_color {
        Some(v) => {
            // SAFETY: see `set`.
            unsafe { std::env::set_var("NO_COLOR", v) };
        }
        None => unset("NO_COLOR"),
    }
    match saved_force_color {
        Some(v) => {
            // SAFETY: see `set`.
            unsafe { std::env::set_var("FORCE_COLOR", v) };
        }
        None => unset("FORCE_COLOR"),
    }
}
