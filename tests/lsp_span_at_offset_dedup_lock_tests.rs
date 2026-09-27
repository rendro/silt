//! Lock: LSP offset→Span conversion has exactly ONE implementation.
//!
//! Audit finding (LATENT, round 101/102 aftermath): the 4-line byte-offset
//! → lexer-`Span` conversion (line = newline count + 1; line_start =
//! `rfind('\n') + 1`; col = codepoint count + 1; `Span::with_offset`) was
//! being inlined verbatim by successive fix agents at each new raw-scan
//! rename site in `src/lsp/workspace.rs` (shorthand record binders,
//! qualified heads, loop binders). With no shared helper, a future fix to
//! the line/col semantics — e.g. the codepoint-vs-UTF-16 column trap that
//! `src/lsp/conversions.rs` documents for LSP `Position` — would have to
//! be applied N times, and missed copies would make rename spans silently
//! diverge between binder kinds.
//!
//! The conversion now lives in `text_utils::span_at_offset` (whose unit
//! tests pin the exact (line, col, offset) triples the inline math
//! produced, on multi-line and multibyte fixtures). This source lock
//! keeps `workspace.rs` from growing new inline copies: if you need a
//! Span from a byte offset there, call
//! `super::text_utils::span_at_offset` instead of re-deriving it.
//!
//! Note: `conversions.rs` legitimately contains similar-looking math —
//! it converts to LSP `Position` (0-based, UTF-16 code units), a
//! DIFFERENT convention. This lock is scoped to `workspace.rs` on
//! purpose.

const WORKSPACE_RS: &str = include_str!("../src/lsp/workspace.rs");
const TEXT_UTILS_RS: &str = include_str!("../src/lsp/text_utils.rs");

/// The shared helper must exist (and stay) in text_utils.rs.
#[test]
fn span_at_offset_helper_exists_in_text_utils() {
    assert!(
        TEXT_UTILS_RS.contains("fn span_at_offset(source: &str, off: usize) -> Span"),
        "text_utils.rs must define `span_at_offset(source: &str, off: usize) -> Span`; \
         if it was renamed/moved, update workspace.rs call sites and this lock together"
    );
}

/// workspace.rs must delegate to the helper, not re-inline the math.
#[test]
fn workspace_uses_shared_span_at_offset() {
    assert!(
        WORKSPACE_RS.contains("text_utils::span_at_offset("),
        "workspace.rs raw-scan span resolution must go through \
         text_utils::span_at_offset"
    );
}

/// No inline copies of the offset→Span math in workspace.rs. Each of
/// these three fragments is a distinctive line of the formerly-duplicated
/// block; none of them has any legitimate other use in workspace.rs.
#[test]
fn workspace_has_no_inline_offset_to_span_math() {
    for fragment in [
        // line_start recovery
        "rfind('\\n')",
        // 1-based line from newline count
        ".bytes().filter(|&b| b == b'\\n').count() + 1",
        // 1-based codepoint column
        ".chars().count() + 1",
    ] {
        assert!(
            !WORKSPACE_RS.contains(fragment),
            "workspace.rs contains an inline offset->Span conversion fragment \
             `{fragment}`; delegate to text_utils::span_at_offset instead of \
             duplicating the line/col math (audit lock: byte-identical 4-line \
             block was previously triplicated across rename sites)"
        );
    }
}
