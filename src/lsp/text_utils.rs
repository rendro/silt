//! Text-level scanning helpers: identifier search within a byte range,
//! comment/string-aware brace matching, and expression-extent recovery.
//!
//! These helpers operate on raw source text and are reused by multiple
//! handlers (binding collection, signature help, etc.).

use crate::ast::*;

/// Return the approximate (end_offset, _) extent of an expression in the source.
/// For block expressions we scan forward to the matching `}` using a simple
/// brace/paren-aware walker that skips string literals and comments. For
/// other expressions we conservatively return the end of the source.
pub(super) fn expr_extent(expr: &Expr, source: &str) -> (usize, ()) {
    let start = expr.span.offset;
    if start >= source.len() {
        return (source.len(), ());
    }
    if matches!(&expr.kind, ExprKind::Block(_))
        && let Some(end) = match_closing_brace(source, start)
    {
        return (end, ());
    }
    (source.len(), ())
}

/// Given an offset at (or just before) a `{`, return the byte offset of the
/// matching `}` (exclusive end). Skips string literals, char escapes, and
/// line/block comments so we don't get fooled by `"}"` or `// }`.
pub(super) fn match_closing_brace(source: &str, start: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    // Find the first `{` at or after `start`.
    let mut i = start;
    while i < bytes.len() && bytes[i] != b'{' {
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
                i += 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            b'"' => {
                // Triple-quoted string?
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
                // Skip `--` line comment to end of line.
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    None
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
