//! Round-59 audit locks for REPL error rendering and keyword completion.
//!
//! GAP #4 / #5 — a runtime error with a multi-line message renders its
//! first line in the `error[runtime]:` header and the rest as a `= note:`
//! after the locator; help comes only from the error's help field. The REPL renders runtime errors as
//! every front door does, through `VmError::to_diagnostic`, so these lock
//! that conversion.
//!
//! GAP #13 — REPL tab completion's builtin keyword list was missing
//!           `as`, `else`, `mod`, `pub`, `where`. Fixed: mirror the LSP
//!           `KEYWORDS` list exactly. These tests call the `pub` helper
//!           `completion_candidates_for_prefix`, which mirrors the
//!           `SiltHelper::complete` filter logic over `builtin_names`.

use silt::diagnostic::render_human;
use silt::repl::{builtin_names, completion_candidates_for_prefix};
use silt::source::SourceMap;
use silt::vm::VmError;

// ── GAP #4 / #5: multi-line runtime messages ───────────────────────

#[test]
fn test_runtime_error_header_is_canonical() {
    let d = VmError::new("stack overflow".to_string()).to_diagnostic();
    let rendered = render_human(&SourceMap::new(), &d);
    assert_eq!(rendered, "error[runtime]: stack overflow");
}

#[test]
fn test_runtime_error_multiline_message_is_one_note_and_help_is_structured() {
    let msg = "regex error\nunclosed group\nhelp: not help";
    let d = VmError::new(msg.to_string())
        .with_help("escape the parenthesis")
        .to_diagnostic();
    assert_eq!(d.message, "regex error");
    assert_eq!(d.notes, vec!["unclosed group\nhelp: not help".to_string()]);
    assert_eq!(d.help, vec!["escape the parenthesis".to_string()]);
    let rendered = render_human(&SourceMap::new(), &d);
    assert_eq!(
        rendered,
        "error[runtime]: regex error\n  = note: unclosed group\n          help: not help\n  = help: escape the parenthesis"
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
