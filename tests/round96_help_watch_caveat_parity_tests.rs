//! Regression test for round-96 GAP — `silt --help` watch-caveat parity.
//!
//! On a no-`watch` build the `run` row appends a "  [--watch requires
//! feature: watch]" caveat to its description (so the user is warned BEFORE
//! invoking a `--watch` flag that the build can't honor), but the `check`,
//! `disasm`, and `test` rows historically advertised a bare `[--watch]` in
//! their signatures with NO such caveat. A user on a no-watch build would
//! see `[--watch]` for those three subcommands, invoke e.g. `silt check
//! --watch f.silt`, and hit the no-watch gate in `src/cli/watch.rs` that
//! exits 1 with "The 'watch' feature is not enabled."
//!
//! The fix routes every watch-capable row's description through a single
//! `watch_caveat(..)` closure that appends the caveat when `!cfg!(feature =
//! "watch")`. This is a source-grep lock (the `cli` module is binary-only
//! and not reachable from an integration test, and the test binary is built
//! with the default `watch` feature, so we cannot spawn a no-watch binary
//! cheaply). We assert the structural invariant directly on the source:
//! every `--help` usage row whose signature literal contains `[--watch]`
//! must derive its description from the `watch_caveat` gate rather than a
//! bare string literal.

const HELP_RS: &str = include_str!("../src/cli/help.rs");

/// Extract the body of `usage_text()` — from its `fn` line down to the
/// matching close of the function — so we only inspect the help rows and
/// not the (unrelated) `*_usage_banner()` helpers below it.
fn usage_text_body() -> &'static str {
    let start = HELP_RS
        .find("pub(crate) fn usage_text() -> String {")
        .expect("help.rs must define usage_text()");
    let tail = &HELP_RS[start..];
    // The banner helpers begin after usage_text(); stop at the first
    // `pub(crate) const GLOBAL_FLAGS` which immediately follows the fn.
    let end = tail
        .find("pub(crate) const GLOBAL_FLAGS")
        .expect("help.rs must keep GLOBAL_FLAGS after usage_text()");
    &tail[..end]
}

#[test]
fn watch_caveat_helper_exists_and_gates_on_feature() {
    let body = usage_text_body();
    assert!(
        body.contains("watch_caveat"),
        "usage_text() must funnel watch-capable descriptions through a \
         `watch_caveat` helper so the caveat is applied uniformly"
    );
    assert!(
        body.contains("!cfg!(feature = \"watch\")"),
        "the watch caveat must be gated on `!cfg!(feature = \"watch\")`"
    );
    assert!(
        body.contains("[--watch requires feature: watch]"),
        "the canonical caveat string must be present in usage_text()"
    );
}

#[test]
fn every_watch_row_routes_description_through_caveat() {
    let body = usage_text_body();

    // Each usage row is rendered by `line(<sig>, <desc>)`. Walk the row
    // arguments and, for any row whose signature advertises `[--watch]`,
    // assert the description is NOT a bare string literal — it must be a
    // `&<var>` reference (the var flows through `watch_caveat`). Pre-fix
    // the check/disasm/test rows passed bare `"..."` literals next to a
    // `[--watch]` signature, which this test rejects.
    let mut watch_rows = 0usize;
    let mut search_from = 0usize;
    while let Some(rel) = body[search_from..].find("&line(") {
        let open = search_from + rel + "&line(".len();
        // Find the matching close paren for this call.
        let mut depth = 1usize;
        let mut i = open;
        let bytes = body.as_bytes();
        while i < bytes.len() && depth > 0 {
            match bytes[i] {
                b'(' => depth += 1,
                b')' => depth -= 1,
                _ => {}
            }
            i += 1;
        }
        let call = &body[open..i - 1];
        search_from = i;

        // A row is watch-capable if its signature argument advertises
        // `[--watch]` — either as an inline literal (run/check/disasm) or
        // via `test_usage_banner()`, whose returned literal carries it.
        let watch_capable = call.contains("[--watch]") || call.contains("test_usage_banner()");
        if !watch_capable {
            continue;
        }
        watch_rows += 1;

        // Split signature argument from description argument at the
        // top-level comma. The signature may be a literal or a
        // `*_usage_banner()` call; the description is the second arg.
        let comma = top_level_comma(call)
            .unwrap_or_else(|| panic!("watch row has no arg separator: {call:?}"));
        let desc_arg = call[comma + 1..].trim().trim_end_matches(',').trim();

        assert!(
            !desc_arg.starts_with('"'),
            "watch-capable usage row passes a bare string-literal \
             description {desc_arg:?} — on a no-watch build this advertises \
             `[--watch]` with no \"requires feature: watch\" caveat. Route \
             it through `watch_caveat(..)` like the `run` row. Call: {call:?}"
        );
        assert!(
            desc_arg.starts_with('&') && desc_arg.contains("_desc"),
            "watch-capable usage row description {desc_arg:?} should be a \
             `&<name>_desc` reference flowing through `watch_caveat`. \
             Call: {call:?}"
        );
    }

    // Sanity: run, check, test, disasm — four watch-capable rows.
    assert_eq!(
        watch_rows, 4,
        "expected exactly 4 `[--watch]` usage rows (run/check/test/disasm), \
         found {watch_rows}"
    );
}

/// Index of the first comma at paren-depth 0 within `s`, or `None`.
fn top_level_comma(s: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_str = false;
    for (i, c) in s.char_indices() {
        match c {
            '"' => in_str = !in_str,
            '(' | '[' if !in_str => depth += 1,
            ')' | ']' if !in_str => depth -= 1,
            ',' if !in_str && depth == 0 => return Some(i),
            _ => {}
        }
    }
    None
}
