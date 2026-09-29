//! Caret-alignment regression tests for wide characters (CJK, emoji,
//! etc.) and baseline ASCII/tab handling. These lock the fix in
//! `src/errors.rs::caret_spacing` (and its mirror use in
//! `src/compiler/mod.rs::format_module_source_error`): one space per
//! *display cell* rather than one space per `char`, so CJK / emoji
//! double-wide chars don't push the caret one column to the left of
//! its intended position.
//!
//! Tests drive the public `Display` path on `SourceError` and count
//! the run of spaces before the `^` on the caret line. `cargo test`
//! captures stderr, so `use_color()` returns false and the rendered
//! output is plain ASCII with no ANSI escapes — parsing stays simple.

use silt::errors::{ErrorKind, SourceError};
use silt::lexer::Span;

/// Build a `SourceError` that renders a caret under `col` (1-based)
/// on `src_line`. Kind is irrelevant for caret alignment; we pick
/// `Compile` for concreteness.
fn caret_err(src_line: &str, col: usize) -> SourceError {
    SourceError {
        kind: ErrorKind::Compile,
        message: "test".to_string(),
        span: Span::new(1, col),
        source_line: Some(src_line.to_string()),
        file: Some("<test>".to_string()),
        is_warning: false,
    }
}

/// Extract the caret line (the last non-empty rendered line) and
/// return the literal prefix before the first `^`. Panics if no
/// caret is found — that's a test failure, not a valid rendering.
fn caret_prefix(rendered: &str) -> String {
    // The caret line looks like:  "   |  <spacing>^ test"
    // We want <spacing> — i.e. everything between the " | " gutter
    // separator and the first `^`.
    let caret_line = rendered
        .lines()
        .rev()
        .find(|l| l.contains('^'))
        .expect("no caret line in rendered output");
    // Split on " | " (gutter). The spacing + caret + message live
    // after the last " | " on the caret line.
    let after_gutter = caret_line
        .rsplit_once(" | ")
        .map(|(_, rest)| rest)
        .unwrap_or(caret_line);
    let caret_idx = after_gutter
        .find('^')
        .expect("no `^` after gutter on caret line");
    after_gutter[..caret_idx].to_string()
}

#[test]
fn caret_ascii_baseline_n_spaces_for_column_n() {
    // Baseline regression lock: for a pure-ASCII source line, the
    // caret's leading spaces should equal (col - 1).
    let src = "let x = 42;";
    // Point at the `4` of `42` — that's column 9 (1-based).
    let rendered = format!("{}", caret_err(src, 9));
    let prefix = caret_prefix(&rendered);
    assert_eq!(
        prefix.len(),
        8,
        "ASCII caret prefix should be 8 spaces for col 9, got {:?}",
        prefix
    );
    assert!(
        prefix.chars().all(|c| c == ' '),
        "ASCII prefix should be all spaces, got {:?}",
        prefix
    );
}

#[test]
fn caret_cjk_accounts_for_double_width_cells() {
    // A Chinese char renders in 2 terminal cells. If we point the
    // caret at the char *after* one CJK char (i.e. col 2, since the
    // source line starts with the CJK char), the spacing under the
    // caret must be 2 cells wide — otherwise the caret lands a cell
    // to the left of its target.
    //
    // Source line: "中x"
    //   chars: ['中', 'x']  (2 chars)
    //   cells: '中' = 2 cells, 'x' = 1 cell
    //   col = 2 (1-based) points at 'x'
    //   expected spacing = 2 spaces (the 2 cells occupied by '中')
    let src = "\u{4e2d}x"; // 中x
    let rendered = format!("{}", caret_err(src, 2));
    let prefix = caret_prefix(&rendered);
    assert_eq!(
        prefix.len(),
        2,
        "CJK caret prefix should be 2 cells wide for col 2, got {:?} (len={})",
        prefix,
        prefix.len()
    );
    assert!(
        prefix.chars().all(|c| c == ' '),
        "CJK prefix should be all spaces, got {:?}",
        prefix
    );
}

#[test]
fn caret_cjk_multiple_wide_chars() {
    // Three CJK chars then an ASCII target: col 4 points at 'x'.
    // Cells before 'x' = 3 * 2 = 6.
    let src = "\u{4e2d}\u{6587}\u{5b57}x"; // 中文字x
    let rendered = format!("{}", caret_err(src, 4));
    let prefix = caret_prefix(&rendered);
    assert_eq!(
        prefix.len(),
        6,
        "3 CJK chars should yield 6 cells of caret padding, got {:?} (len={})",
        prefix,
        prefix.len()
    );
}

#[test]
fn caret_emoji_accounts_for_double_width_cells() {
    // Most common emoji render as 2 terminal cells (East Asian Wide /
    // Emoji Presentation). Point the caret at the char after the
    // emoji.
    //
    // Source line: "😀x"
    //   chars: ['😀', 'x']
    //   cells: '😀' = 2 cells, 'x' = 1 cell
    //   col = 2 (1-based) points at 'x'
    //   expected spacing = 2 spaces
    let src = "\u{1f600}x"; // 😀x
    let rendered = format!("{}", caret_err(src, 2));
    let prefix = caret_prefix(&rendered);
    assert_eq!(
        prefix.len(),
        2,
        "Emoji caret prefix should be 2 cells wide for col 2, got {:?} (len={})",
        prefix,
        prefix.len()
    );
    assert!(
        prefix.chars().all(|c| c == ' '),
        "Emoji prefix should be all spaces, got {:?}",
        prefix
    );
}

#[test]
fn caret_tab_mixed_still_works_alongside_width_fix() {
    // Regression: the tab special-case must survive the unicode-width
    // refactor. A source line starting with a tab followed by ASCII
    // should produce a caret prefix whose first character is still a
    // literal `\t` — the terminal expands it to the next tab stop,
    // matching the source line above.
    //
    // Source line: "\tfoo"
    //   col = 2 (1-based) points at 'f' (the char after the tab)
    //   expected prefix = "\t" (one literal tab char)
    let src = "\tfoo";
    let rendered = format!("{}", caret_err(src, 2));
    let prefix = caret_prefix(&rendered);
    assert_eq!(
        prefix, "\t",
        "tab-prefixed caret padding must be a single `\\t`, got {:?}",
        prefix
    );
}

#[test]
fn caret_tab_mixed_with_cjk() {
    // Combined: tab + CJK char before the caret target. The prefix
    // should be "\t" + "  " (tab preserved, 2 spaces for the CJK
    // char's display cells).
    //
    // Source line: "\t中x"
    //   col = 3 (1-based) points at 'x'
    //   expected prefix = "\t  " (tab + 2 spaces)
    let src = "\t\u{4e2d}x"; // \t中x
    let rendered = format!("{}", caret_err(src, 3));
    let prefix = caret_prefix(&rendered);
    assert_eq!(
        prefix, "\t  ",
        "tab + CJK caret padding should be `\\t` + 2 spaces, got {:?}",
        prefix
    );
}
