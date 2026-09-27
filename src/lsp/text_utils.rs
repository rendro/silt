//! Text-level scanning helpers: identifier search within a byte range,
//! comment/string-aware brace matching, and expression-extent recovery.
//!
//! These helpers operate on raw source text and are reused by multiple
//! handlers (binding collection, signature help, etc.).

use crate::ast::*;
use crate::lexer::Span;

/// Convert a byte offset in `source` into a lexer `Span` (1-based line,
/// 1-based **codepoint** column, byte offset) — the same convention the
/// lexer stamps on tokens (lexer.rs advances `col` once per `char`).
///
/// Single source of truth for the offset→Span math used when an LSP
/// handler re-derives a precise token span from a byte offset it found by
/// scanning raw source (shorthand record binders, rename target
/// resolution, …). Do NOT inline this computation at call sites: the
/// column convention here is codepoints, while `conversions.rs`'s
/// `offset_to_position` speaks LSP `Position` (0-based, UTF-16 code
/// units) — a documented trap. Keeping one copy means a future fix to
/// the line/col semantics lands everywhere at once.
///
/// `off` is clamped to `source.len()` and must lie on a `char` boundary
/// (offsets produced by `find_ident_in_range` always do).
///
/// Lock: tests/lsp_span_at_offset_dedup_lock_tests.rs + the unit tests
/// below.
pub(super) fn span_at_offset(source: &str, off: usize) -> Span {
    let off = off.min(source.len());
    let line = source[..off].bytes().filter(|&b| b == b'\n').count() + 1;
    let line_start = source[..off].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let col = source[line_start..off].chars().count() + 1;
    Span::with_offset(line, col, off)
}

/// Return the approximate end-offset extent of an expression in the source.
/// For block expressions we scan forward to the matching `}` using a simple
/// brace/paren-aware walker that skips string literals and comments. For
/// other expression kinds we recursively walk children via
/// `ast_walk::visit_expr_children` and take the maximum end offset, with a
/// per-kind self-extent fallback for leaves (ident length, literal length,
/// etc.). This was previously returning `source.len()` for non-Block kinds,
/// which defeated cursor-bound checks in callers like `selection_range` —
/// e.g. `fn add(a, b) = a + b` would claim to extend to EOF, dragging the
/// `fn add` decl into the selection chain of any later cursor in the file.
/// Round-84 LATENT fix.
pub(super) fn expr_extent(expr: &Expr, source: &str) -> usize {
    let start = expr.span.offset;
    if start >= source.len() {
        return source.len();
    }
    // Block bodies have a definitive close-brace; scan for it.
    if matches!(&expr.kind, ExprKind::Block(_))
        && let Some(end) = match_closing_brace(source, start)
    {
        return end;
    }
    // For other kinds: walk children recursively, take max end. The
    // per-kind self-extent below ensures leaves (which have no children)
    // still cover their own bytes.
    let mut max_end = self_extent(expr, source);
    super::ast_walk::visit_expr_children(expr, |child| {
        let child_end = expr_extent(child, source);
        if child_end > max_end {
            max_end = child_end;
        }
    });
    max_end.min(source.len())
}

/// Tight end-offset for an expression's own token(s), ignoring children.
/// Used as the fallback floor in `expr_extent` so leaves don't collapse to
/// a zero-width extent at `span.offset`. Estimates are intentionally
/// conservative — we never want to over-extend (that's the bug we're
/// fixing); under-extending the leaf is recovered by the caller walking
/// children via `visit_expr_children`.
fn self_extent(expr: &Expr, source: &str) -> usize {
    let start = expr.span.offset;
    if start >= source.len() {
        return source.len();
    }
    let bytes = source.as_bytes();
    match &expr.kind {
        ExprKind::Ident(name) => start + crate::intern::resolve(*name).len(),
        ExprKind::Int(_) | ExprKind::Float(_) => {
            // Scan forward through digits / `.` / `_` / sign chars.
            let mut i = start;
            // Optional leading `-` (unary literal forms get parsed as
            // `Unary(Neg, ...)` not as a negative `Int`, but be defensive).
            if i < bytes.len() && (bytes[i] == b'-' || bytes[i] == b'+') {
                i += 1;
            }
            while i < bytes.len() {
                let b = bytes[i];
                if b.is_ascii_digit() || b == b'.' || b == b'_' || b == b'e' || b == b'E' {
                    i += 1;
                } else {
                    break;
                }
            }
            i.max(start + 1)
        }
        ExprKind::Bool(b) => start + if *b { 4 } else { 5 },
        ExprKind::Unit => start + 2, // `()`
        ExprKind::StringLit(_, is_triple) => {
            // Walk to the matching closing quote(s), respecting escapes.
            let mut i = start;
            if *is_triple {
                // Skip opening `"""`.
                if i + 3 <= bytes.len() && &bytes[i..i + 3] == b"\"\"\"" {
                    i += 3;
                }
                while i + 2 < bytes.len()
                    && !(bytes[i] == b'"' && bytes[i + 1] == b'"' && bytes[i + 2] == b'"')
                {
                    i += 1;
                }
                (i + 3).min(bytes.len())
            } else {
                // Skip opening `"`.
                if i < bytes.len() && bytes[i] == b'"' {
                    i += 1;
                }
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                (i + 1).min(bytes.len())
            }
        }
        // For aggregate/structured kinds, the start byte itself is the
        // floor; children carry the actual extent. Returning `start` here
        // means "no self-bytes past my children" — the recursive walk in
        // `expr_extent` will lift it above `start` via child ends.
        _ => start,
    }
}

/// Given an offset at (or just before) a `{`, return the byte offset of the
/// matching `}` (exclusive end). Skips string literals, char escapes, and
/// line/block comments so we don't get fooled by `"}"` or `// }`.
pub(super) fn match_closing_brace(source: &str, start: usize) -> Option<usize> {
    // Inclusive offset of the matching `}` from the scanner, +1 for
    // exclusive-end semantics.
    find_matching_close_brace(source, start).map(|(off, _)| off + 1)
}

/// Shared scanner used by both `match_closing_brace` (for offset-only
/// callers) and `folding::compute_span_end_line` (which needs the line
/// number of the matching `}`). Walks `source` from `start`, finds the
/// first real `{`, then advances depth until a real `}` brings depth back
/// to zero, skipping over silt comments and string literals:
///
///   * `--` line comments,
///   * `{- ... -}` block comments (nestable; the leading `{` and trailing
///     `}` do NOT count toward brace depth),
///   * `"..."` strings (with `\`-escapes),
///   * `"""..."""` triple-quoted strings.
///
/// Returns `(offset_of_matching_close_brace, newlines_consumed_from_start)`
/// where `offset_of_matching_close_brace` points AT the `}` byte (not one
/// past it). `newlines_consumed_from_start` counts `\n` bytes seen in
/// `source[start..=offset_of_matching_close_brace]`, including any inside
/// skipped comments or strings. Returns `None` if no `{` is ever found or
/// the input is unbalanced.
pub(super) fn find_matching_close_brace(source: &str, start: usize) -> Option<(usize, u32)> {
    let bytes = source.as_bytes();
    let mut i = start;
    let mut newlines: u32 = 0;
    // Find the first `{` at or after `start`. Count newlines we cross so
    // that callers tracking line numbers don't lose any before the body.
    while i < bytes.len() && bytes[i] != b'{' {
        if bytes[i] == b'\n' {
            newlines += 1;
        }
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }
    let mut depth = 0i32;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'{' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                // Skip `{- ... -}` block comment (with nesting).
                i += 2;
                let mut comment_depth = 1u32;
                while i < bytes.len() && comment_depth > 0 {
                    if i + 1 < bytes.len() && bytes[i] == b'{' && bytes[i + 1] == b'-' {
                        comment_depth += 1;
                        i += 2;
                    } else if i + 1 < bytes.len() && bytes[i] == b'-' && bytes[i + 1] == b'}' {
                        comment_depth -= 1;
                        i += 2;
                    } else {
                        if bytes[i] == b'\n' {
                            newlines += 1;
                        }
                        i += 1;
                    }
                }
            }
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((i, newlines));
                }
                i += 1;
            }
            b'"' => {
                // Triple-quoted string?
                if i + 2 < bytes.len() && bytes[i + 1] == b'"' && bytes[i + 2] == b'"' {
                    i += 3;
                    while i + 2 < bytes.len()
                        && !(bytes[i] == b'"' && bytes[i + 1] == b'"' && bytes[i + 2] == b'"')
                    {
                        if bytes[i] == b'\n' {
                            newlines += 1;
                        }
                        i += 1;
                    }
                    i = (i + 3).min(bytes.len());
                } else {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' && i + 1 < bytes.len() {
                            // Count any newline in a 2-byte escape window.
                            if bytes[i + 1] == b'\n' {
                                newlines += 1;
                            }
                            i += 2;
                        } else {
                            if bytes[i] == b'\n' {
                                newlines += 1;
                            }
                            i += 1;
                        }
                    }
                    if i < bytes.len() {
                        i += 1;
                    }
                }
            }
            b'-' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                // Skip `--` line comment to end of line. The trailing `\n`
                // (if any) is left for the outer loop to count via the
                // default arm.
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'\n' => {
                newlines += 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    None
}

/// Locate a shorthand record-field binder (`x` in `Point { a, x }` or
/// `{ a, x }`) by scanning forward from the record pattern's head.
///
/// Round-101 BROKEN fix: the previous strategy — scan to the FIRST `}`
/// after the head and search that window — truncated the range whenever a
/// nested braced sub-pattern preceded the binder (`Point { a: Inner { y },
/// x }`: the first `}` closes `Inner { y }`, so `x` was never found and
/// callers fell back to the record HEAD span, which rename then rewrote
/// into garbage). This scanner is brace-depth-aware instead: it enters the
/// record's own `{` (depth 1) and only matches identifier tokens sitting
/// at depth 1 — i.e. direct fields of THIS record — until the record's own
/// matching `}` closes it. Nested record / anon-record / map sub-patterns
/// (depth ≥ 2) are skipped entirely, as are `{- -}` block comments, `--`
/// line comments, and string literals (mirroring
/// [`find_matching_close_brace`]). Matches immediately followed by `:` are
/// rejected — those are field labels with an explicit sub-pattern, not
/// shorthand binders. Returns the absolute byte offset of the LAST
/// qualifying token (legal silt has at most one binder per name, so
/// first/last is moot; last matches [`find_ident_in_range`]'s historical
/// direction).
pub(super) fn find_shorthand_binder(source: &str, head_offset: usize, name: &str) -> Option<usize> {
    let bytes = source.as_bytes();
    let name_bytes = name.as_bytes();
    if name.is_empty() {
        return None;
    }
    let mut i = head_offset.min(bytes.len());
    let mut depth = 0usize;
    let mut found: Option<usize> = None;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'{' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                // `{- ... -}` block comment (nestable); no brace depth.
                i += 2;
                let mut comment_depth = 1u32;
                while i < bytes.len() && comment_depth > 0 {
                    if i + 1 < bytes.len() && bytes[i] == b'{' && bytes[i + 1] == b'-' {
                        comment_depth += 1;
                        i += 2;
                    } else if i + 1 < bytes.len() && bytes[i] == b'-' && bytes[i + 1] == b'}' {
                        comment_depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            }
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                if depth <= 1 {
                    // The record's own closing brace: the binder must
                    // precede it, so whatever we have is the answer.
                    return found;
                }
                depth -= 1;
                i += 1;
            }
            b'"' => {
                // String literal (regular or triple-quoted); skip it so a
                // `}` inside a string pattern doesn't close the record.
                if i + 2 < bytes.len() && bytes[i + 1] == b'"' && bytes[i + 2] == b'"' {
                    i += 3;
                    while i + 2 < bytes.len()
                        && !(bytes[i] == b'"' && bytes[i + 1] == b'"' && bytes[i + 2] == b'"')
                    {
                        i += 1;
                    }
                    i = (i + 3).min(bytes.len());
                } else {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' && i + 1 < bytes.len() {
                            i += 2;
                        } else {
                            i += 1;
                        }
                    }
                    if i < bytes.len() {
                        i += 1;
                    }
                }
            }
            b'-' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                // `--` line comment: skip to end of line.
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            _ if depth == 1 && (b.is_ascii_alphabetic() || b == b'_') => {
                // Identifier token directly inside the record's braces.
                // Guard against being the tail of a digit-led token
                // (`0xff`): a preceding ident byte means this is not a
                // token start.
                let tok_start = i;
                let prev_is_ident = tok_start > 0 && {
                    let p = bytes[tok_start - 1];
                    p.is_ascii_alphanumeric() || p == b'_'
                };
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                if !prev_is_ident && &bytes[tok_start..i] == name_bytes {
                    // Skip trailing whitespace; a `:` next means this is
                    // an explicit `field: sub-pattern` label, not a
                    // shorthand binder.
                    let mut j = i;
                    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    if j >= bytes.len() || bytes[j] != b':' {
                        found = Some(tok_start);
                    }
                }
            }
            _ => i += 1,
        }
    }
    found
}

/// Scan `source[start..end]` for the LAST occurrence of `name` as a whole
/// word (not surrounded by identifier characters). Returns the absolute byte
/// offset in `source`.
pub(super) fn find_ident_in_range(
    source: &str,
    start: usize,
    end: usize,
    name: &str,
) -> Option<usize> {
    if name.is_empty() || start >= source.len() || end > source.len() || start >= end {
        return None;
    }
    let hay = &source[start..end];
    let bytes = hay.as_bytes();
    let name_bytes = name.as_bytes();
    let name_len = name_bytes.len();
    if name_len > bytes.len() {
        return None;
    }
    // Walk from the end backward for the LAST match.
    let mut i = bytes.len().saturating_sub(name_len);
    loop {
        if &bytes[i..i + name_len] == name_bytes {
            let before_ok = i == 0 || {
                let b = bytes[i - 1];
                !(b.is_ascii_alphanumeric() || b == b'_')
            };
            let after_ok = i + name_len == bytes.len() || {
                let b = bytes[i + name_len];
                !(b.is_ascii_alphanumeric() || b == b'_')
            };
            if before_ok && after_ok {
                return Some(start + i);
            }
        }
        if i == 0 {
            return None;
        }
        i -= 1;
    }
}

/// Resolve the byte offset of the head NAME token in a module-qualified
/// head (`util.Pt { .. }`, `shapes.Circle(r)`). `head_offset` sits on the
/// module qualifier's first byte (the AST span for qualified record
/// creates and qualified constructor/record patterns points at the
/// qualifier, not the name — parser.rs keeps `left.span`/`start`); the
/// name token follows the first `.`. Returns `None` when the token cannot
/// be located — callers treat that as "no reference here" (conservative:
/// better to miss an edit than to corrupt the qualifier).
pub(super) fn qualified_head_name_offset(
    source: &str,
    head_offset: usize,
    name: &str,
) -> Option<usize> {
    if name.is_empty() || head_offset >= source.len() {
        return None;
    }
    let dot = source[head_offset..].find('.')? + head_offset;
    let bytes = source.as_bytes();
    let mut off = dot + 1;
    while off < bytes.len() && bytes[off].is_ascii_whitespace() {
        off += 1;
    }
    if !source[off..].starts_with(name) {
        return None;
    }
    // Whole-token check: the match must not continue as a longer ident.
    let end = off + name.len();
    if let Some(&b) = bytes.get(end)
        && (b.is_ascii_alphanumeric() || b == b'_')
    {
        return None;
    }
    Some(off)
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── match_closing_brace ──────────────────────────────────────

    #[test]
    fn test_match_closing_brace_skips_silt_line_comment() {
        // The `-- }` line comment should NOT count as a closing brace.
        let source = "fn foo() { -- }\n  42\n}";
        //           0         1
        //           0123456789012345678901
        // Opening `{` is at index 9.  Real closing `}` is at index 21.
        let result = match_closing_brace(source, 9);
        assert_eq!(result, Some(22), "line comment `-- }}` should be skipped");
    }

    #[test]
    fn test_match_closing_brace_skips_silt_block_comment() {
        // The `{- } -}` block comment should NOT count as a closing brace
        // and the `{-` should NOT count as an opening brace.
        let source = "fn foo() { {- } -}\n  42\n}";
        //           0         1         2
        //           0123456789012345678901234
        // Opening `{` is at index 9.  Real closing `}` is at index 24.
        let result = match_closing_brace(source, 9);
        assert_eq!(
            result,
            Some(25),
            "block comment `{{- }} -}}` should be skipped"
        );
    }

    // ── find_shorthand_binder ────────────────────────────────────

    #[test]
    fn test_find_shorthand_binder_flat() {
        let source = "Point { x, y }";
        //            0123456789
        assert_eq!(find_shorthand_binder(source, 0, "x"), Some(8));
        assert_eq!(find_shorthand_binder(source, 0, "y"), Some(11));
    }

    #[test]
    fn test_find_shorthand_binder_after_nested_braced_subpattern() {
        // Round-101 BROKEN: the first-`}` scan stopped at the `}` closing
        // `Inner { y }`, so the shorthand binder `x` after it was never
        // found. The depth-aware scan must find it.
        let source = "Point { a: Inner { y }, x }";
        //            0         1         2
        //            0123456789012345678901234567
        assert_eq!(find_shorthand_binder(source, 0, "x"), Some(24));
        // `y` is NOT a direct field of the outer record (depth 2) — the
        // outer scan must not claim it; the nested pattern's own scan
        // (head at `Inner`, offset 11) must.
        assert_eq!(find_shorthand_binder(source, 0, "y"), None);
        assert_eq!(find_shorthand_binder(source, 11, "y"), Some(19));
    }

    #[test]
    fn test_find_shorthand_binder_rejects_explicit_field_label() {
        // `a` is an explicit `field: sub` label, not a shorthand binder.
        let source = "Point { a: b, x }";
        assert_eq!(find_shorthand_binder(source, 0, "a"), None);
        assert_eq!(find_shorthand_binder(source, 0, "x"), Some(14));
    }

    #[test]
    fn test_find_shorthand_binder_skips_string_and_comment_braces() {
        // A `}` inside a string-literal sub-pattern or a block comment
        // must not close the record early.
        let source = "Point { s: \"}\", x }";
        assert_eq!(find_shorthand_binder(source, 0, "x"), Some(16));
        let source2 = "Point { {- } -} x }";
        assert_eq!(find_shorthand_binder(source2, 0, "x"), Some(16));
    }

    #[test]
    fn test_find_shorthand_binder_anon_record_head() {
        // Anon-record patterns: the head span sits AT the `{`.
        let source = "{ a: { y }, x }";
        assert_eq!(find_shorthand_binder(source, 0, "x"), Some(12));
    }

    #[test]
    fn test_find_shorthand_binder_stops_at_own_close_brace() {
        // A same-named ident AFTER the record's own `}` must not match.
        let source = "Point { a } x";
        assert_eq!(find_shorthand_binder(source, 0, "x"), None);
    }

    // ── span_at_offset ───────────────────────────────────────────
    //
    // These fixtures assert the exact (line, col, offset) triples the
    // previously-inlined math produced, so delegating call sites to
    // `span_at_offset` is provably a semantic no-op.

    #[test]
    fn test_span_at_offset_first_line() {
        let source = "let alpha = 1";
        //            0123456789
        // `alpha` starts at byte 4 → line 1, col 5.
        let span = span_at_offset(source, 4);
        assert_eq!((span.line, span.col, span.offset), (1, 5, 4));
    }

    #[test]
    fn test_span_at_offset_start_of_file() {
        let span = span_at_offset("abc", 0);
        assert_eq!((span.line, span.col, span.offset), (1, 1, 0));
    }

    #[test]
    fn test_span_at_offset_multiline() {
        let source = "let a = 1\nlet b = 2\n  point";
        // Byte layout: line 1 = bytes 0..9, '\n' at 9; line 2 = 10..19,
        // '\n' at 19; line 3 starts at 20. `point` starts at byte 22 →
        // line 3, col 3.
        let off = source.find("point").unwrap();
        assert_eq!(off, 22);
        let span = span_at_offset(source, off);
        assert_eq!((span.line, span.col, span.offset), (3, 3, 22));
    }

    #[test]
    fn test_span_at_offset_multibyte_column_counts_codepoints() {
        // `é` is 2 bytes / 1 char, `日` is 3 bytes / 1 char. Column is
        // a CODEPOINT count (matching the lexer), not a byte count and
        // not UTF-16 units.
        let source = "-- é日\nlet x = 1";
        let off = source.find('x').unwrap();
        // Line 2 starts after the '\n'; `let ` = 4 chars before `x`.
        let span = span_at_offset(source, off);
        assert_eq!((span.line, span.col, span.offset), (2, 5, off));
        // Multibyte chars BEFORE the offset on the same line compress:
        // byte offset of `日` is 5 (after 2-byte `é`), but it is only
        // the 5th codepoint (`-`, `-`, ` `, `é` precede it) → col 5.
        let off_cjk = source.find('日').unwrap();
        assert_eq!(off_cjk, 5);
        let span = span_at_offset(source, off_cjk);
        assert_eq!((span.line, span.col, span.offset), (1, 5, 5));
    }

    #[test]
    fn test_span_at_offset_clamps_past_eof() {
        let source = "ab\ncd";
        let span = span_at_offset(source, 999);
        assert_eq!((span.line, span.col, span.offset), (2, 3, 5));
    }

    #[test]
    fn test_match_closing_brace_normal() {
        // Basic matching of braces without any comments.
        let source = "fn foo() { let x = { 1 }; x }";
        //           0         1         2
        //           0123456789012345678901234567890
        // Opening `{` at index 9.  Real closing `}` at index 29.
        let result = match_closing_brace(source, 9);
        assert_eq!(result, Some(29), "should match the outermost closing brace");
    }
}
