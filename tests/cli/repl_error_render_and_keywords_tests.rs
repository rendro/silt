//! Round-59 audit locks for REPL error rendering and keyword completion.
//!
//! Three bugs fixed in src/repl.rs — this file pins each one with an
//! exact-substring assertion so a regression would fail deterministically
//! rather than producing a visually-similar-but-wrong output.
//!
//! GAP #4 — REPL fallback leaked `"VM error:"` from `VmError::Display`.
//!          Fixed: route span-less runtime errors through
//!          `render_runtime_error_without_source` so the user-facing
//!          output carries the canonical `error[runtime]:` header.
//!
//! GAP #5 — REPL out-of-range-span path dumped multi-line messages
//!          inline BEFORE the `-->` locator. Fixed: split on the first
//!          `\n`, keep the first line in the header, emit the rest as
//!          `= note:` / `= help:` continuation AFTER the locator, the
//!          same shape `SourceError::Display` uses.
//!
//! GAP #13 — REPL tab completion's builtin keyword list was missing
//!           `as`, `else`, `mod`, `pub`, `where`. Fixed: mirror the LSP
//!           `KEYWORDS` list exactly.
//!
//! The first two tests call the `pub` helper `render_runtime_error_without_source`
//! directly, which is what both fallback branches in `eval_declaration` and
//! `eval_expression` now emit. The third test calls the `pub` helper
//! `completion_candidates_for_prefix`, which mirrors the `SiltHelper::complete`
//! filter logic over `builtin_names`.

use silt::repl::{
    builtin_names, completion_candidates_for_prefix, render_runtime_error_without_source,
};

// ── GAP #4: fallback path does not leak `"VM error:"` ──────────────

#[test]
fn test_repl_fallback_path_does_not_leak_vm_error_prefix() {
    // `show_declaration_locator == false` is the branch that the two
    // `e.span == None` REPL sites now funnel through. The rendered
    // output must NOT contain the internal `"VM error:"` prefix from
    // `VmError::Display` and MUST carry the canonical `error[runtime]:`
    // header used by every other runtime diagnostic in the tool.
    let rendered = render_runtime_error_without_source("stack overflow", false);

    assert!(
        !rendered.contains("VM error:"),
        "fallback output must not leak the internal `VM error:` prefix, got:\n{rendered}"
    );
    assert!(
        rendered.contains("error[runtime]: stack overflow"),
        "expected canonical `error[runtime]:` header in fallback output, got:\n{rendered}"
    );
    // And — important — the fallback branch does NOT emit a `-->`
    // locator, matching how `SourceError::Display` omits it when
    // `span.line == 0`. Round-59 GAP #4 locks this explicitly so a
    // future "helpful" addition of a bogus locator gets caught here.
    assert!(
        !rendered.contains("-->"),
        "fallback (no-span) output must not contain a `-->` locator, got:\n{rendered}"
    );
}

#[test]
fn test_repl_fallback_multiline_message_splits_into_note_continuation() {
    // Even the fallback (no-span) branch must split multi-line messages
    // so body lines render as `= note:` continuation rather than being
    // dumped into the header line.
    let msg = "top-level failure\nsomething went wrong mid-run";
    let rendered = render_runtime_error_without_source(msg, false);

    assert!(
        !rendered.contains("VM error:"),
        "fallback must not leak VM error prefix even for multi-line messages, got:\n{rendered}"
    );
    assert!(
        rendered.contains("error[runtime]: top-level failure"),
        "first line must go into the header, got:\n{rendered}"
    );
    assert!(
        rendered.contains("= note: something went wrong mid-run"),
        "body line must render as `= note:` continuation, got:\n{rendered}"
    );
}

// ── GAP #5: out-of-range-span path renders multi-line cleanly ──────

#[test]
fn test_repl_out_of_range_span_multiline_renders_note_after_locator() {
    // The out-of-range-span branch emits `--> <declaration>` and then
    // must put the rest of a multi-line message AFTER that locator as
    // `= note:` continuation, not inline with the header. This mirrors
    // the `SourceError::Display` layout in src/errors.rs so CLI and
    // REPL multi-line diagnostics read identically.
    let msg = "division by zero\nthis came from a prior declaration";
    let rendered = render_runtime_error_without_source(msg, true);

    // Header line: only the first line of the message.
    assert!(
        rendered.contains("error[runtime]: division by zero"),
        "first line must be the header, got:\n{rendered}"
    );
    // Locator comes right after the header, still on its own line.
    assert!(
        rendered.contains("--> <declaration>"),
        "out-of-range branch must emit `--> <declaration>` locator, got:\n{rendered}"
    );
    // Continuation line must appear as `= note:` AFTER the locator.
    assert!(
        rendered.contains("= note: this came from a prior declaration"),
        "body line must render as `= note:` continuation, got:\n{rendered}"
    );

    // Ordering check: the locator line must come BEFORE the note line.
    // Pre-fix output dumped body lines inline BEFORE `-->`; the exact
    // byte positions lock that this no longer happens.
    let locator_pos = rendered
        .find("--> <declaration>")
        .expect("locator must be present");
    let note_pos = rendered
        .find("= note:")
        .expect("`= note:` continuation must be present");
    assert!(
        locator_pos < note_pos,
        "`--> <declaration>` must appear BEFORE `= note:` (locator then body), got:\n{rendered}"
    );
}

#[test]
fn test_repl_out_of_range_span_help_prefix_renders_as_help_continuation() {
    // A body line beginning with `help: ` must render as `= help:`
    // instead of `= note:`, matching the `SourceError::Display`
    // convention in src/errors.rs. This lets prior-entry runtime
    // diagnostics with actionable hints read as `= help: …` in the
    // REPL just as they do from `silt run`.
    let msg = "index out of range\nhelp: check the bounds of your slice";
    let rendered = render_runtime_error_without_source(msg, true);

    assert!(
        rendered.contains("= help: check the bounds of your slice"),
        "`help:`-prefixed body lines must render as `= help:` continuation, got:\n{rendered}"
    );
    // Sanity: the `= note:` prefix must NOT be applied to a `help:` line.
    assert!(
        !rendered.contains("= note: help:"),
        "`help:`-prefixed line must not be double-prefixed, got:\n{rendered}"
    );
}

// ── GAP #13: REPL completion includes all five missing keywords ────

#[test]
fn test_repl_builtin_names_includes_round59_missing_keywords() {
    // The five keywords that round-58 left out of the REPL's completion
    // list. Each one is present in `src/lsp/completion.rs::KEYWORDS` and
    // round-59 adds them to the REPL so the two completion UIs are in
    // sync. An exact-match-each-keyword lock beats a vector equality
    // check because it survives future additions to either list.
    let names = builtin_names();
    for kw in ["as", "else", "mod", "pub", "where"] {
        assert!(
            names.contains(&kw.to_string()),
            "round-59 keyword `{kw}` missing from REPL builtin_names, got:\n{names:?}"
        );
    }
}

#[test]
fn test_repl_completion_for_p_suggests_pub() {
    // When the user types `p` and hits Tab, `pub` must be in the
    // suggested completions. Pre-fix `pub` was missing, so the user
    // could only get `print`/`println`/`panic` — silently losing the
    // `pub fn …`/`pub type …` affordance entirely.
    let matches = completion_candidates_for_prefix("p");
    assert!(
        matches.iter().any(|s| s == "pub"),
        "expected `pub` in completions for prefix `p`, got:\n{matches:?}"
    );
}

#[test]
fn test_repl_completion_for_each_new_keyword_prefix_suggests_it() {
    // Each of the five round-59 keywords must be offered when the user
    // types its first character (or the whole keyword, in the case of
    // single-prefix overlaps). We check each prefix independently with
    // a tightly-scoped assertion so a regression that drops one keyword
    // is isolated to exactly one failing test rather than hidden in an
    // aggregate failure.
    for (prefix, keyword) in [
        ("a", "as"),
        ("e", "else"),
        ("m", "mod"),
        ("p", "pub"),
        ("w", "where"),
    ] {
        let matches = completion_candidates_for_prefix(prefix);
        assert!(
            matches.iter().any(|s| s == keyword),
            "expected `{keyword}` in completions for prefix `{prefix}`, got:\n{matches:?}"
        );
    }
}
