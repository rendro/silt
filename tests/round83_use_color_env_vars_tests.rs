//! Audit round 83 — lock the `NO_COLOR` / `FORCE_COLOR` env-var handling in
//! `src/errors.rs::use_color()`.
//!
//! Before this round, `use_color()` only looked at `isatty(stderr)`, ignoring
//! the widely-adopted `NO_COLOR` convention (<https://no-color.org/>) and the
//! cargo-style `FORCE_COLOR` override. That broke users running silt under
//! wrappers, CI environments faking a tty, and accessibility tools that need
//! to disable ANSI escapes even when stderr looks interactive.
//!
//! These tests exercise the actual user-visible surface — `SourceError`'s
//! `Display` impl — by inspecting whether ANSI escapes (`\x1b[`) appear in
//! the rendered output under each env-var configuration. This is more robust
//! than poking at `use_color()` directly: it locks the behavior the user
//! actually sees, not the internal predicate.
//!
//! Precedence the tests assert:
//!   1. `NO_COLOR=<nonempty>` → no escapes, regardless of `FORCE_COLOR`.
//!   2. `NO_COLOR` unset/empty + `FORCE_COLOR=<nonempty>` → escapes present.
//!   3. Both unset → fallthrough to isatty (which is false under `cargo test`
//!      since stderr is captured); we don't assert a specific value here,
//!      just that the call succeeds and the output is a valid render.
//!
//! NOTE on env-var test safety: `std::env::set_var` / `remove_var` are not
//! thread-safe with concurrent readers. Since `serial_test` is not a
//! dev-dependency in `Cargo.toml`, we keep ALL toggles inside ONE `#[test]`
//! function so the per-test sequential model is sufficient. Other tests in
//! the suite that race with this one would need to read `NO_COLOR` /
//! `FORCE_COLOR` to be affected — silt code only reads these vars from
//! `use_color()`, and no other test exercises that path concurrently.

use silt::errors::{ErrorKind, SourceError};
use silt::lexer::Span;

/// Build a minimal `SourceError` whose `Display` output exercises every
/// color hook (header label, locator arrow, gutter, caret). The exact
/// content doesn't matter — we only inspect for ANSI escape bytes.
fn make_error() -> SourceError {
    SourceError {
        kind: ErrorKind::Compile,
        message: "round83 probe".to_string(),
        span: Span::new(1, 1),
        source_line: Some("x".to_string()),
        file: Some("<round83>".to_string()),
        is_warning: false,
    }
}

/// Does the rendered output contain at least one ANSI CSI escape?
fn has_ansi(rendered: &str) -> bool {
    rendered.contains('\x1b')
}

/// SAFETY wrapper around `std::env::set_var`. The pre-1.80 contract requires
/// no concurrent readers; we satisfy that by serializing all env mutations
/// in this single test function and by knowing no other thread in the
/// per-test process reads these specific vars between toggles.
fn set(key: &str, value: &str) {
    // SAFETY: see file-level NOTE — all toggles are sequential within this
    // one #[test] fn, no concurrent readers.
    unsafe { std::env::set_var(key, value) };
}

fn unset(key: &str) {
    // SAFETY: see `set`.
    unsafe { std::env::remove_var(key) };
}

#[test]
fn use_color_honors_no_color_and_force_color_env_vars() {
    // Save and clear at the start so we don't inherit whatever the caller's
    // shell had set (CI runners sometimes set NO_COLOR globally).
    let saved_no_color = std::env::var_os("NO_COLOR");
    let saved_force_color = std::env::var_os("FORCE_COLOR");
    unset("NO_COLOR");
    unset("FORCE_COLOR");

    // ── Case 1: NO_COLOR=1 disables color unconditionally ─────────────
    // Even if stderr were a tty (we can't fake that easily here, but the
    // kill-switch must win regardless), NO_COLOR=<nonempty> must drop
    // escapes from the rendered output.
    set("NO_COLOR", "1");
    let rendered = format!("{}", make_error());
    assert!(
        !has_ansi(&rendered),
        "NO_COLOR=1 must suppress ANSI escapes, got:\n{rendered}"
    );

    // ── Case 2: NO_COLOR set to empty string does NOT disable ─────────
    // Per the no-color.org spec, an empty value is equivalent to unset.
    // With both NO_COLOR="" and FORCE_COLOR=1, we expect escapes (the
    // FORCE_COLOR branch takes over once NO_COLOR is effectively absent).
    set("NO_COLOR", "");
    set("FORCE_COLOR", "1");
    let rendered = format!("{}", make_error());
    assert!(
        has_ansi(&rendered),
        "NO_COLOR=\"\" must NOT disable; FORCE_COLOR=1 should force escapes, got:\n{rendered}"
    );
    unset("FORCE_COLOR");

    // ── Case 3: FORCE_COLOR=1 alone forces color even without a tty ───
    // Under `cargo test`, stderr is captured (not a tty), so the
    // isatty fallthrough would normally return false. FORCE_COLOR must
    // override that and produce escapes.
    unset("NO_COLOR");
    set("FORCE_COLOR", "1");
    let rendered = format!("{}", make_error());
    assert!(
        has_ansi(&rendered),
        "FORCE_COLOR=1 (NO_COLOR unset) must produce escapes, got:\n{rendered}"
    );

    // ── Case 4: NO_COLOR wins over FORCE_COLOR ────────────────────────
    // Per the spec, NO_COLOR is the killswitch — it must beat
    // FORCE_COLOR even when both are set.
    set("NO_COLOR", "1");
    set("FORCE_COLOR", "1");
    let rendered = format!("{}", make_error());
    assert!(
        !has_ansi(&rendered),
        "NO_COLOR must beat FORCE_COLOR (killswitch precedence), got:\n{rendered}"
    );

    // ── Case 5: Both unset → fallthrough is well-formed ───────────────
    // Don't assert a specific color/no-color outcome (depends on isatty,
    // which is false under captured-stderr `cargo test` but may differ
    // under other runners). Just exercise the path and make sure the
    // render doesn't panic and produces *some* output.
    unset("NO_COLOR");
    unset("FORCE_COLOR");
    let rendered = format!("{}", make_error());
    assert!(
        rendered.contains("round83 probe"),
        "fallthrough render must include the error message body, got:\n{rendered}"
    );

    // ── Restore inherited env-var state for downstream tests ──────────
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
