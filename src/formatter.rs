use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::intern::{Symbol, resolve};
use crate::lexer::{Lexer, Span};
use crate::parser::Parser;

const INDENT: &str = "  ";

thread_local! {
    /// The source text of the file currently being formatted, used by
    /// `format_triple_string` to copy multi-line `"""..."""` content
    /// verbatim instead of reflowing it.
    static CURRENT_SOURCE: RefCell<Option<String>> = const { RefCell::new(None) };

    /// Shared comment delivery state for the declaration currently being
    /// formatted. Tracks every standalone and trailing comment that lives
    /// inside the declaration's source range along with which ones have
    /// already been emitted. All of the formatter functions that emit a
    /// nested block (match arm bodies, loop/lambda/block bodies, bare
    /// block expressions, match-as-RHS) read from and update this state
    /// as they walk the AST in source order. Using a single shared state
    /// — rather than per-decl comment slices that were only plumbed to
    /// the outermost fn body — ensures that comments whose source lines
    /// land inside nested constructs are emitted at the correct nested
    /// position instead of being hoisted to the enclosing fn body.
    static FMT_STATE: RefCell<Option<FmtState>> = const { RefCell::new(None) };
}

/// Formatter state for comment delivery during a single declaration's
/// formatting.
struct FmtState {
    /// All standalone comments for the currently-formatting declaration,
    /// sorted by source line.
    comments: Vec<Comment>,
    /// Parallel flags indicating which entries in `comments` have already
    /// been emitted somewhere in the output.
    consumed: Vec<bool>,
    /// Map from source line to trailing comment text (the raw `-- ...`).
    trailing_map: HashMap<usize, String>,
    /// Source lines whose trailing comment has already been emitted.
    trailing_consumed: HashSet<usize>,
    /// Raw source lines (access via line_idx = line - 1), used for
    /// computing block end lines on demand.
    source_lines: Vec<String>,
}

impl FmtState {
    fn new(comments: Vec<Comment>, trailing_map: HashMap<usize, String>, source: &str) -> Self {
        let consumed = vec![false; comments.len()];
        let source_lines: Vec<String> = source.lines().map(|s| s.to_string()).collect();
        Self {
            comments,
            consumed,
            trailing_map,
            trailing_consumed: HashSet::new(),
            source_lines,
        }
    }
}

/// Run `f` with a fresh `FmtState`. The previous state (if any) is
/// restored afterwards, so sibling decls see independent comment state.
fn with_fmt_state<R>(
    comments: Vec<Comment>,
    trailing: HashMap<usize, String>,
    source: &str,
    f: impl FnOnce() -> R,
) -> R {
    let state = FmtState::new(comments, trailing, source);
    let prev = FMT_STATE.with(|cell| cell.replace(Some(state)));
    let result = f();
    FMT_STATE.with(|cell| {
        *cell.borrow_mut() = prev;
    });
    result
}

/// Take the trailing comment attached to `line`, marking it consumed so
/// it is not also emitted later. Returns the raw comment text (including
/// the `-- ` prefix) or `None`.
fn take_trailing_for_line(line: usize) -> Option<String> {
    FMT_STATE.with(|cell| {
        let mut borrowed = cell.borrow_mut();
        let state = borrowed.as_mut()?;
        if state.trailing_consumed.contains(&line) {
            return None;
        }
        let text = state.trailing_map.get(&line).cloned()?;
        state.trailing_consumed.insert(line);
        Some(text)
    })
}

/// Consume and return every unconsumed standalone comment whose source
/// line is strictly less than `before_line`, in source order.
fn take_comments_before(before_line: usize) -> Vec<Comment> {
    FMT_STATE.with(|cell| {
        let mut borrowed = cell.borrow_mut();
        let Some(state) = borrowed.as_mut() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for i in 0..state.comments.len() {
            if state.consumed[i] {
                continue;
            }
            if state.comments[i].line < before_line {
                state.consumed[i] = true;
                out.push(state.comments[i].clone());
            }
        }
        out
    })
}

/// Consume and return every unconsumed standalone comment whose source
/// line is strictly greater than `after_line` AND strictly less than
/// `before_line`. Used at the close of a nested block to drain any
/// comments that sit between the last inner statement's line and the
/// block's closing brace so they stay inside the block.
fn take_comments_between(after_line: usize, before_line: usize) -> Vec<Comment> {
    FMT_STATE.with(|cell| {
        let mut borrowed = cell.borrow_mut();
        let Some(state) = borrowed.as_mut() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for i in 0..state.comments.len() {
            if state.consumed[i] {
                continue;
            }
            let line = state.comments[i].line;
            if line > after_line && line < before_line {
                state.consumed[i] = true;
                out.push(state.comments[i].clone());
            }
        }
        out
    })
}

/// Compute the 1-based line of the closing `}` for a block whose
/// opening `{` is at `span`. Mirrors the scan logic in
/// `resolve_decl_end_lines` but operates on a single block span.
/// Returns `span.line` as a safe fallback when the scan cannot find a
/// matching brace.
fn compute_block_end_line(span: Span) -> usize {
    FMT_STATE.with(|cell| {
        let borrowed = cell.borrow();
        let Some(state) = borrowed.as_ref() else {
            return span.line;
        };
        let source_lines = &state.source_lines;
        if span.line == 0 || span.line > source_lines.len() {
            return span.line;
        }
        let start_idx = span.line - 1;
        let mut depth: i32 = 0;
        let mut found_open = false;
        let mut in_string = false;
        let mut interp_depths: Vec<i32> = Vec::new();
        for (line_idx, line) in source_lines.iter().enumerate().skip(start_idx) {
            let mut chars = line.chars().peekable();
            while let Some(ch) = chars.next() {
                if in_string {
                    if ch == '\\' {
                        chars.next();
                    } else if ch == '"' {
                        in_string = false;
                    } else if ch == '{' {
                        interp_depths.push(0);
                        in_string = false;
                    }
                } else if ch == '"' {
                    in_string = true;
                } else if ch == '-' && chars.peek() == Some(&'-') {
                    break; // line comment
                } else if ch == '{' {
                    if let Some(d) = interp_depths.last_mut() {
                        *d += 1;
                    } else {
                        depth += 1;
                        found_open = true;
                    }
                } else if ch == '}' {
                    if let Some(d) = interp_depths.last_mut() {
                        if *d == 0 {
                            interp_depths.pop();
                            in_string = true;
                        } else {
                            *d -= 1;
                        }
                    } else {
                        depth -= 1;
                    }
                }
            }
            if found_open && depth == 0 {
                return line_idx + 1;
            }
        }
        span.line
    })
}

/// Render a list of standalone comments at the given indent depth, one
/// per line, separated by `\n`.
fn render_comments(comments: &[Comment], depth: usize) -> String {
    comments
        .iter()
        .map(|c| format!("{}{}", indent(depth), c.text.trim()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn with_current_source<R>(source: &str, f: impl FnOnce() -> R) -> R {
    CURRENT_SOURCE.with(|cell| *cell.borrow_mut() = Some(source.to_string()));
    let result = f();
    CURRENT_SOURCE.with(|cell| *cell.borrow_mut() = None);
    result
}

/// Extract the raw bytes of a triple-quoted string from the stashed source,
/// starting at byte offset `offset`. Returns the full `"""..."""` literal
/// (including the opening and closing `"""`) if found and well-formed.
fn extract_triple_string_raw(offset: usize) -> Option<String> {
    CURRENT_SOURCE.with(|cell| {
        let cell = cell.borrow();
        let source = cell.as_ref()?;
        let bytes = source.as_bytes();
        // Verify the span starts at `"""`.
        if offset + 3 > bytes.len() || &bytes[offset..offset + 3] != b"\"\"\"" {
            return None;
        }
        // Scan forward for the closing `"""`.
        let mut i = offset + 3;
        while i + 3 <= bytes.len() {
            if &bytes[i..i + 3] == b"\"\"\"" {
                return Some(source[offset..i + 3].to_string());
            }
            i += 1;
        }
        None
    })
}

// ── Comment extraction ──────────────────────────────────────────────

/// A standalone comment (on its own line) extracted from source.
#[derive(Debug, Clone)]
struct Comment {
    line: usize,  // 1-based line number where the comment starts
    text: String, // the raw comment text including `--` or `{- ... -}`
}

/// A trailing comment that shares a line with code (e.g., `let x = 42 -- note`).
#[derive(Debug, Clone)]
struct TrailingComment {
    line: usize,  // 1-based line number
    text: String, // the comment text including `--` prefix
}

/// Classification of each source line with respect to triple-quoted strings.
///
/// `Code` — the whole line is code (or comment, or blank).
/// `InsideTriple` — the line is entirely inside a `"""..."""` block (raw
///     content) and must never be classified as a comment.
/// `TripleEnds { opened_line, after_close }` — the line contains the closing
///     `"""` of a triple-string that opened on `opened_line` (1-based). The
///     portion of the line after the closing `"""` starts at byte index
///     `after_close` and may carry a trailing comment; that comment logically
///     belongs to the statement that started on `opened_line`.
#[derive(Debug, Clone, Copy)]
enum LineKind {
    Code,
    InsideTriple,
    TripleEnds {
        opened_line: usize,
        after_close: usize,
    },
}

/// Walk the source once and classify each line by triple-string state.
///
/// The scan follows the lexer's rules: a `"""` always opens or closes a
/// triple-quoted string (outside of any other context), and the content
/// between opening and closing `"""` is raw (no escapes, no interpolation,
/// no nested strings). Regular double-quoted strings are single-line in
/// Silt, so they cannot straddle lines.
fn classify_lines(source: &str) -> Vec<LineKind> {
    let lines: Vec<&str> = source.lines().collect();
    let mut result = vec![LineKind::Code; lines.len()];
    // 1-based line number where the currently-open triple-string started,
    // or None when not inside one.
    let mut open_at: Option<usize> = None;

    for (idx, line) in lines.iter().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let mut j = 0;

        if let Some(start_line) = open_at {
            // We began this line inside a triple-string. Scan for the
            // closing `"""`; everything before it is raw content.
            let mut closed_at: Option<usize> = None;
            while j + 3 <= chars.len() {
                if chars[j] == '"' && chars[j + 1] == '"' && chars[j + 2] == '"' {
                    closed_at = Some(j + 3);
                    break;
                }
                j += 1;
            }
            match closed_at {
                Some(end) => {
                    result[idx] = LineKind::TripleEnds {
                        opened_line: start_line,
                        after_close: end,
                    };
                    open_at = None;
                    j = end;
                }
                None => {
                    result[idx] = LineKind::InsideTriple;
                    continue;
                }
            }
        }

        // Scan the remainder of this line (outside any triple-string) for
        // the next `"""` that would open a new one. Regular `"..."` and
        // `{- ... -}` contexts must be respected so we don't mistake a `"""`
        // inside a line comment or regular string for the start of a raw
        // string. Since `--` and `{-` start contexts that end at end-of-line
        // (for `--`) or close later, we can be conservative: once we hit
        // `--`, stop. For `"`, skip characters until the matching `"`. For
        // `{-`, skip until matching `-}` on the same line (or end).
        let mut in_string = false;
        let mut block_depth: usize = 0;
        while j < chars.len() {
            let ch = chars[j];
            if in_string {
                if ch == '\\' && j + 1 < chars.len() {
                    j += 2;
                    continue;
                }
                if ch == '"' {
                    in_string = false;
                }
                j += 1;
                continue;
            }
            if block_depth > 0 {
                if ch == '-' && j + 1 < chars.len() && chars[j + 1] == '}' {
                    block_depth -= 1;
                    j += 2;
                    continue;
                }
                if ch == '{' && j + 1 < chars.len() && chars[j + 1] == '-' {
                    block_depth += 1;
                    j += 2;
                    continue;
                }
                j += 1;
                continue;
            }
            // Line comment — stop scanning this line.
            if ch == '-' && j + 1 < chars.len() && chars[j + 1] == '-' {
                break;
            }
            // Block comment start
            if ch == '{' && j + 1 < chars.len() && chars[j + 1] == '-' {
                block_depth += 1;
                j += 2;
                continue;
            }
            // Triple-quoted string: `"""`
            if ch == '"' && j + 3 <= chars.len() && chars[j + 1] == '"' && chars[j + 2] == '"' {
                // Opens a triple string on this line (idx+1 is 1-based).
                open_at = Some(idx + 1);
                j += 3;
                // Check if it also closes on the same line.
                let mut k = j;
                let mut same_line_close: Option<usize> = None;
                while k + 3 <= chars.len() {
                    if chars[k] == '"' && chars[k + 1] == '"' && chars[k + 2] == '"' {
                        same_line_close = Some(k + 3);
                        break;
                    }
                    k += 1;
                }
                if let Some(end) = same_line_close {
                    open_at = None;
                    j = end;
                    continue;
                } else {
                    // Stays open into next line. Remainder of this line is
                    // raw content, not code. We leave this line as-is in
                    // `result[idx]` (Code), because the opening portion up
                    // to `"""` is still code that may have e.g. a `let x =`
                    // prefix. The classifier only matters for detecting
                    // that subsequent lines are `InsideTriple`.
                    break;
                }
            }
            // Regular double-quoted string
            if ch == '"' {
                in_string = true;
                j += 1;
                continue;
            }
            j += 1;
        }
    }

    result
}

/// Extract standalone comments and trailing comments from source text.
///
/// A "standalone" comment is one that occupies its own line(s) — the line has
/// only whitespace before the comment marker and nothing after it (for line
/// comments) or the block comment starts on its own line.
///
/// A "trailing" comment shares a line with code (e.g., `let x = 42 -- note`).
fn extract_comments(source: &str) -> (Vec<Comment>, Vec<TrailingComment>) {
    let mut comments = Vec::new();
    let mut trailing = Vec::new();
    let lines: Vec<&str> = source.lines().collect();
    let line_kinds = classify_lines(source);
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        // Lines that are entirely inside a triple-quoted string are raw
        // content and never classify as comments.
        if matches!(line_kinds[i], LineKind::InsideTriple) {
            i += 1;
            continue;
        }

        // For a line that contains the closing `"""` of a triple-string,
        // the raw-content portion is before the `"""`. We still want to
        // extract any trailing comment that appears *after* the closing
        // `"""`, but attribute it to the *opening* line of the string so
        // that the statement that owns the string can pick it up even
        // after the formatter collapses the string.
        if let LineKind::TripleEnds {
            opened_line,
            after_close,
        } = line_kinds[i]
        {
            let tail: String = line.chars().skip(after_close).collect();
            if let Some(comment_text) = extract_trailing_comment_from_line(&tail) {
                trailing.push(TrailingComment {
                    line: opened_line,
                    text: comment_text,
                });
            }
            i += 1;
            continue;
        }

        // Line comment: entire line is `-- ...`
        if trimmed.starts_with("--") {
            comments.push(Comment {
                line: i + 1, // 1-based
                text: line.to_string(),
            });
            i += 1;
            continue;
        }

        // Block comment starting on its own line
        if trimmed.starts_with("{-") {
            let mut block = String::new();
            let start_line = i + 1; // 1-based
            // Accumulate lines until we close all nested block comments
            let mut depth: i32 = 0;
            let mut found_end = false;
            while i < lines.len() {
                if !block.is_empty() {
                    block.push('\n');
                }
                block.push_str(lines[i]);
                // Count openers and closers in this line
                let mut chars = lines[i].chars().peekable();
                while let Some(ch) = chars.next() {
                    if ch == '{' && chars.peek() == Some(&'-') {
                        chars.next();
                        depth += 1;
                    } else if ch == '-' && chars.peek() == Some(&'}') {
                        chars.next();
                        depth -= 1;
                        if depth == 0 {
                            // Check if there's only whitespace after the closer
                            let rest: String = chars.collect();
                            if rest.trim().is_empty() {
                                found_end = true;
                            } else {
                                // Not a standalone block comment — skip
                                found_end = false;
                            }
                            break;
                        }
                    }
                }
                i += 1;
                if depth == 0 {
                    break;
                }
            }
            if found_end || depth == 0 {
                comments.push(Comment {
                    line: start_line,
                    text: block,
                });
            }
            continue;
        }

        // Check for trailing comment: code followed by ` -- ...`
        if let Some(comment_text) = extract_trailing_comment_from_line(line) {
            trailing.push(TrailingComment {
                line: i + 1, // 1-based
                text: comment_text,
            });
        }

        i += 1;
    }
    (comments, trailing)
}

/// Extract the trailing comment from a line of code, if present.
/// Skips `--` that appear inside string literals.
fn extract_trailing_comment_from_line(line: &str) -> Option<String> {
    let mut in_string = false;
    let mut block_depth: usize = 0;
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if in_string {
            if ch == '\\' {
                i += 2;
                continue;
            }
            if ch == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        // Check for block comment start `{-`
        if ch == '{' && i + 1 < chars.len() && chars[i + 1] == '-' {
            block_depth += 1;
            i += 2;
            continue;
        }
        // Check for block comment end `-}`
        if ch == '-' && i + 1 < chars.len() && chars[i + 1] == '}' && block_depth > 0 {
            block_depth -= 1;
            i += 2;
            continue;
        }
        // Skip everything inside block comments
        if block_depth > 0 {
            i += 1;
            continue;
        }
        if ch == '"' {
            in_string = true;
            i += 1;
            continue;
        }
        if ch == '-' && i + 1 < chars.len() && chars[i + 1] == '-' {
            let comment: String = chars[i..].iter().collect();
            return Some(comment.trim_end().to_string());
        }
        i += 1;
    }
    None
}

/// Get the start line (1-based) of a declaration from its span, if available.
fn decl_start_line(decl: &Decl) -> Option<usize> {
    match decl {
        Decl::Fn(f) => Some(f.span.line),
        Decl::Type(t) => Some(t.span.line),
        Decl::Trait(t) => Some(t.span.line),
        Decl::TraitImpl(t) => Some(t.span.line),
        Decl::Import(_, span) => Some(span.line),
        Decl::Let { span, .. } => Some(span.line),
    }
}

/// Get the start line (1-based) of a statement from its contained expression spans.
fn stmt_start_line(stmt: &Stmt) -> usize {
    match stmt {
        Stmt::Let { value, .. } => value.span.line,
        Stmt::Expr(expr) => expr.span.line,
        Stmt::When { expr, .. } => expr.span.line,
        Stmt::WhenBool { condition, .. } => condition.span.line,
    }
}

/// Find 1-based line numbers of top-level `import` statements in source.
fn find_import_lines(source: &str) -> Vec<usize> {
    let mut result = Vec::new();
    for (i, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("import ") || trimmed == "import" {
            result.push(i + 1); // 1-based
        }
    }
    result
}

/// Resolve the start line for every declaration. For most decls we use the
/// AST span; for `Import` (which has no span) we match against source lines.
fn resolve_decl_lines(decls: &[Decl], source: &str) -> Vec<usize> {
    let import_lines = find_import_lines(source);
    let mut import_idx = 0;
    let mut result = Vec::with_capacity(decls.len());
    for decl in decls {
        if let Some(line) = decl_start_line(decl) {
            result.push(line);
        } else {
            // Import without span — use next available import line from source
            if import_idx < import_lines.len() {
                result.push(import_lines[import_idx]);
                import_idx += 1;
            } else {
                // Fallback: use 0 so comments before it won't be lost
                result.push(0);
            }
        }
    }
    result
}

/// Check whether a declaration has a block body (i.e. uses `{ ... }` not `= expr`).
fn decl_has_block_body(decl: &Decl) -> bool {
    match decl {
        Decl::Fn(f) => matches!(f.body.kind, ExprKind::Block(_)),
        Decl::Trait(_) | Decl::TraitImpl(_) | Decl::Type(_) => true,
        Decl::Import(..) | Decl::Let { .. } => false,
    }
}

/// Compute the end line (1-based, inclusive) for each declaration by scanning
/// the source for balanced braces starting from each declaration's start line.
/// For single-line declarations (simple fn, import, let), the end line equals
/// the start line.
fn resolve_decl_end_lines(decls: &[Decl], decl_lines: &[usize], source: &str) -> Vec<usize> {
    let source_lines: Vec<&str> = source.lines().collect();
    let mut result = Vec::with_capacity(decls.len());

    for (i, decl) in decls.iter().enumerate() {
        if !decl_has_block_body(decl) {
            // Single-line declaration — end is same as start
            result.push(decl_lines[i]);
            continue;
        }

        // Scan from the declaration's start line to find the matching closing brace.
        let start = decl_lines[i]; // 1-based
        let mut depth: i32 = 0;
        let mut end_line = start;
        let mut found_open = false;

        let mut in_string = false;
        let mut interp_depths: Vec<i32> = vec![];
        for (line_idx, line) in source_lines.iter().enumerate().skip(start - 1) {
            let mut chars = line.chars().peekable();
            while let Some(ch) = chars.next() {
                if in_string {
                    if ch == '\\' {
                        chars.next(); // skip escaped character
                    } else if ch == '"' {
                        in_string = false;
                    } else if ch == '{' {
                        // Start of string interpolation
                        interp_depths.push(0);
                        in_string = false;
                    }
                } else if ch == '"' {
                    in_string = true;
                } else if ch == '-' && chars.peek() == Some(&'-') {
                    break; // line comment, skip rest of line
                } else if ch == '{' {
                    if let Some(d) = interp_depths.last_mut() {
                        *d += 1; // nested brace inside interpolation
                    } else {
                        depth += 1;
                        found_open = true;
                    }
                } else if ch == '}' {
                    if let Some(d) = interp_depths.last_mut() {
                        if *d == 0 {
                            interp_depths.pop();
                            in_string = true; // back to string after interpolation
                        } else {
                            *d -= 1;
                        }
                    } else {
                        depth -= 1;
                    }
                }
            }
            if found_open && depth == 0 {
                end_line = line_idx + 1; // 1-based
                break;
            }
        }

        result.push(end_line);
    }

    result
}

// ── Public entry point ──────────────────────────────────────────────

pub fn format(source: &str) -> Result<String, String> {
    let tokens = Lexer::new(source)
        .tokenize()
        .map_err(|e| format!("lex error: {e}"))?;
    let program = Parser::new(tokens)
        .parse_program()
        .map_err(|e| format!("parse error: {e}"))?;
    Ok(with_current_source(source, || {
        format_program_with_comments(&program, source)
    }))
}

fn format_program_with_comments(program: &Program, source: &str) -> String {
    if program.decls.is_empty() {
        // Even with no declarations, there might be comments
        let (comments, _trailing) = extract_comments(source);
        if comments.is_empty() {
            return String::from("\n");
        }
        let mut result: String = comments
            .iter()
            .map(|c| c.text.clone())
            .collect::<Vec<_>>()
            .join("\n");
        if !result.ends_with('\n') {
            result.push('\n');
        }
        return result;
    }

    let (comments, trailing_comments) = extract_comments(source);
    let decl_lines = resolve_decl_lines(&program.decls, source);
    let decl_end_lines = resolve_decl_end_lines(&program.decls, &decl_lines, source);

    // Build a map from original source line number to trailing comment text.
    let trailing_map: HashMap<usize, String> = trailing_comments
        .into_iter()
        .map(|tc| (tc.line, tc.text))
        .collect();

    // Partition comments into:
    // - top-level buckets (between declarations)
    // - body comments (inside a declaration's body), delivered later via
    //   the thread-local `FmtState` during decl formatting.
    let n = program.decls.len();
    let mut buckets: Vec<Vec<Comment>> = vec![Vec::new(); n + 1];
    let mut body_comments: Vec<Vec<Comment>> = vec![Vec::new(); n];

    for comment in comments.iter().cloned() {
        // A comment is inside decl[i]'s body if its line is strictly between
        // the decl's start line and its end line (inclusive of end line, since
        // a comment before the closing `}` is still inside).
        let mut is_body = false;
        for i in 0..n {
            if comment.line > decl_lines[i] && comment.line <= decl_end_lines[i] {
                body_comments[i].push(comment.clone());
                is_body = true;
                break;
            }
        }
        if !is_body {
            // Top-level comment: place in the appropriate bucket.
            let mut placed = false;
            for (i, &dline) in decl_lines.iter().enumerate() {
                if comment.line < dline {
                    buckets[i].push(comment.clone());
                    placed = true;
                    break;
                }
            }
            if !placed {
                buckets[n].push(comment);
            }
        }
    }

    // Separate imports from other declarations, sort imports alphabetically.
    // Each import is paired with its preceding comments so they move together.
    let mut import_pairs: Vec<(String, String)> = Vec::new(); // (comments, import)
    let mut has_imports = false;

    // Format each decl under its own `FmtState` populated with the body
    // comments that belong to that decl. Scoping the comment state to the
    // decl ensures comments inside nested blocks (match arms, loops,
    // lambdas, block expressions) are emitted at the correct nested
    // position instead of being hoisted to the enclosing fn body.
    let formatted_decls: Vec<String> = program
        .decls
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let decl_body_comments = body_comments[i].clone();
            with_fmt_state(decl_body_comments, trailing_map.clone(), source, || {
                format_decl_with_comments(d, 0)
            })
        })
        .collect();

    // Collect and sort import strings; track which decl indices are imports.
    let mut is_import = vec![false; program.decls.len()];
    for (i, decl) in program.decls.iter().enumerate() {
        if matches!(decl, Decl::Import(..)) {
            // Gather preceding comments for this import (bucket[i], skip bucket[0]
            // which is emitted separately as pre-first-decl comments).
            let comment_block = if i > 0 && !buckets[i].is_empty() {
                let mut cb = String::new();
                for c in &buckets[i] {
                    cb.push_str(&c.text);
                    cb.push('\n');
                }
                cb
            } else {
                String::new()
            };
            import_pairs.push((comment_block, formatted_decls[i].clone()));
            is_import[i] = true;
            has_imports = true;
        }
    }
    import_pairs.sort_by(|a, b| a.1.cmp(&b.1));

    let mut result = String::new();

    // Comments before first declaration
    for c in &buckets[0] {
        result.push_str(&c.text);
        result.push('\n');
    }
    if !buckets[0].is_empty() {
        result.push('\n');
    }

    // Emit sorted imports grouped together (single newline between them).
    // Each import may have preceding comments that travel with it.
    for (i, (comment_block, imp)) in import_pairs.iter().enumerate() {
        if i > 0 {
            result.push('\n');
        }
        if !comment_block.is_empty() {
            result.push_str(comment_block);
        }
        result.push_str(imp);
    }

    // Emit non-import declarations with blank lines between them
    let mut first_non_import = true;
    for (i, decl_str) in formatted_decls.iter().enumerate() {
        if is_import[i] {
            continue;
        }
        if has_imports || !first_non_import {
            // Blank line separator
            result.push_str("\n\n");
        }
        // Insert any comments that belong before this declaration
        // (skip bucket[0] since it was already emitted above)
        if i > 0 && !buckets[i].is_empty() {
            for c in &buckets[i] {
                result.push_str(&c.text);
                result.push('\n');
            }
            result.push('\n');
        }
        result.push_str(decl_str);
        first_non_import = false;
    }

    // Comments after last declaration
    if !buckets[n].is_empty() {
        result.push_str("\n\n");
        for c in &buckets[n] {
            result.push_str(&c.text);
            result.push('\n');
        }
    }

    if !result.ends_with('\n') {
        result.push('\n');
    }
    result
}

fn format_decl_with_comments(decl: &Decl, depth: usize) -> String {
    match decl {
        Decl::Fn(f) => format_fn_with_comments(f, depth),
        Decl::Type(t) => format_type(t, depth),
        Decl::Trait(t) => format_trait_with_comments(t, depth),
        Decl::TraitImpl(t) => format_trait_impl_with_comments(t, depth),
        Decl::Import(i, _) => format_import(i, depth),
        Decl::Let {
            pattern,
            ty,
            value,
            is_pub,
            span,
        } => {
            let indent = "  ".repeat(depth);
            let pub_prefix = if *is_pub { "pub " } else { "" };
            let pat = format_pattern(pattern);
            let ty_str = if let Some(t) = ty {
                format!(": {}", format_type_expr(t))
            } else {
                String::new()
            };
            let val = format_expr(value, depth);
            let trailing = take_trailing_for_line(span.line)
                .map(|c| format!(" {c}"))
                .unwrap_or_default();
            format!("{indent}{pub_prefix}let {pat}{ty_str} = {val}{trailing}")
        }
    }
}

fn indent(depth: usize) -> String {
    INDENT.repeat(depth)
}

fn format_fn_with_comments(f: &FnDecl, depth: usize) -> String {
    let prefix = indent(depth);
    let pub_prefix = if f.is_pub { "pub " } else { "" };
    let params = f
        .params
        .iter()
        .map(format_param)
        .collect::<Vec<_>>()
        .join(", ");
    let ret = if let Some(ty) = &f.return_type {
        format!(" -> {}", format_type_expr(ty))
    } else {
        String::new()
    };
    let where_clause = if f.where_clauses.is_empty() {
        String::new()
    } else {
        // Group trait bounds by type param, preserving insertion order.
        let mut grouped: Vec<(Symbol, Vec<Symbol>)> = Vec::new();
        for (name, trait_name) in &f.where_clauses {
            if let Some(entry) = grouped.iter_mut().find(|(n, _)| n == name) {
                entry.1.push(*trait_name);
            } else {
                grouped.push((*name, vec![*trait_name]));
            }
        }
        let clauses: Vec<String> = grouped
            .iter()
            .map(|(name, traits)| {
                let trait_strs: Vec<String> = traits.iter().map(|t| resolve(*t)).collect();
                format!("{name}: {}", trait_strs.join(" + "))
            })
            .collect();
        format!(" where {}", clauses.join(", "))
    };

    // Check if body is a simple expression (single-expression function using =)
    if is_simple_body(&f.body) {
        let body_str = format_expr(&f.body, depth);
        let trailing = take_trailing_for_line(f.span.line)
            .map(|c| format!(" {c}"))
            .unwrap_or_default();
        return format!(
            "{prefix}{pub_prefix}fn {}({params}){ret}{where_clause} = {body_str}{trailing}",
            f.name,
        );
    }

    let body = format_body(&f.body, depth);
    format!(
        "{prefix}{pub_prefix}fn {}({params}){ret}{where_clause} {body}",
        f.name
    )
}

fn is_simple_body(expr: &Expr) -> bool {
    // A body is "simple" if it's not a block — single expression fn
    !matches!(expr.kind, ExprKind::Block(_))
}

fn format_param(p: &Param) -> String {
    let pat = format_pattern(&p.pattern);
    if let Some(ty) = &p.ty {
        format!("{pat}: {}", format_type_expr(ty))
    } else {
        pat
    }
}

/// Format a body expression (fn body, match arm body, loop body, lambda
/// body, bare block expression, block RHS of `let`/`when`/`match`). Uses
/// the thread-local `FmtState` to emit any standalone comments whose
/// source lines fall inside this block at the correct nested position,
/// and inline trailing comments from the source.
fn format_body(expr: &Expr, depth: usize) -> String {
    match &expr.kind {
        ExprKind::Block(stmts) => {
            let open_line = expr.span.line;
            let close_line = compute_block_end_line(expr.span);
            if stmts.is_empty() {
                // Drain comments that sit strictly between `{` and `}`.
                let inner = take_comments_between(open_line, close_line);
                if inner.is_empty() {
                    "{}".to_string()
                } else {
                    format!(
                        "{{\n{}\n{}}}",
                        render_comments(&inner, depth + 1),
                        indent(depth)
                    )
                }
            } else {
                let inner = format_stmts_with_comments(stmts, depth + 1, close_line);
                format!("{{\n{inner}\n{}}}", indent(depth))
            }
        }
        _ => format!(
            "{{\n{}{}\n{}}}",
            indent(depth + 1),
            format_expr(expr, depth + 1),
            indent(depth)
        ),
    }
}

/// Format a list of statements with interleaved comments, drawing from
/// the thread-local `FmtState`. `block_close_line` is the 1-based line
/// of the `}` closing the enclosing block, used to drain any comments
/// that sit between the last statement and the close brace so they are
/// emitted inside the block rather than hoisted to the enclosing scope.
fn format_stmts_with_comments(
    stmts: &[Stmt],
    depth: usize,
    block_close_line: usize,
) -> String {
    let mut result = Vec::new();

    let mut last_stmt_line = 0usize;
    for stmt in stmts {
        let stmt_line = stmt_start_line(stmt);

        // Emit any unconsumed standalone comments whose source line is
        // before this statement. The consumption flags ensure we don't
        // re-emit comments that were already consumed by a nested block
        // during a previous iteration.
        let pre = take_comments_before(stmt_line);
        for c in &pre {
            result.push(format!("{}{}", indent(depth), c.text.trim()));
        }

        let mut formatted = format_stmt(stmt, depth);
        if let Some(tc) = take_trailing_for_line(stmt_line) {
            formatted.push(' ');
            formatted.push_str(&tc);
        }
        result.push(formatted);
        last_stmt_line = stmt_line;
    }

    // Emit any remaining comments that sit between the last statement's
    // source line and the closing brace of the enclosing block.
    let tail = take_comments_between(last_stmt_line, block_close_line);
    for c in &tail {
        result.push(format!("{}{}", indent(depth), c.text.trim()));
    }

    result.join("\n")
}

/// Given the source lines, find the 1-based line number of the first
/// occurrence of `ident` as a whole word, starting at `search_from`
/// (1-based) and stopping before `search_until` (1-based exclusive).
/// Returns `None` if not found. Used to map variant/field/method
/// identifiers (which don't carry their own spans) back to source
/// lines so trailing comments can be looked up correctly.
fn find_ident_line(ident: &str, search_from: usize, search_until: usize) -> Option<usize> {
    FMT_STATE.with(|cell| {
        let borrowed = cell.borrow();
        let state = borrowed.as_ref()?;
        let lines = &state.source_lines;
        let start = search_from.saturating_sub(1);
        let end = search_until.saturating_sub(1).min(lines.len());
        for (idx, raw) in lines.iter().enumerate().skip(start).take(end.saturating_sub(start)) {
            if line_contains_word(raw, ident) {
                return Some(idx + 1);
            }
        }
        None
    })
}

/// True if `line` contains `ident` as a whole-word token (preceded and
/// followed by a non-identifier character or start/end of line, and not
/// inside a string literal or line comment).
fn line_contains_word(line: &str, ident: &str) -> bool {
    let bytes = line.as_bytes();
    let needle = ident.as_bytes();
    if needle.is_empty() {
        return false;
    }
    let mut i = 0;
    let mut in_string = false;
    while i + needle.len() <= bytes.len() {
        let ch = bytes[i] as char;
        if in_string {
            if ch == '\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if ch == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if ch == '"' {
            in_string = true;
            i += 1;
            continue;
        }
        // Line comment: stop searching this line.
        if ch == '-' && i + 1 < bytes.len() && bytes[i + 1] == b'-' {
            return false;
        }
        // Check whole-word match.
        if &bytes[i..i + needle.len()] == needle {
            let before_ok = i == 0 || !is_ident_char(bytes[i - 1] as char);
            let after_ok = i + needle.len() == bytes.len()
                || !is_ident_char(bytes[i + needle.len()] as char);
            if before_ok && after_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn format_type(t: &TypeDecl, depth: usize) -> String {
    let prefix = indent(depth);
    let pub_prefix = if t.is_pub { "pub " } else { "" };
    let params = if t.params.is_empty() {
        String::new()
    } else {
        let param_strs: Vec<String> = t.params.iter().map(|p| resolve(*p)).collect();
        format!("({})", param_strs.join(", "))
    };

    // Resolve the closing `}` of this type body so we can look up the
    // trailing comments attached to each variant/field line.
    let close_line = compute_block_end_line(t.span);

    match &t.body {
        TypeBody::Enum(variants) => {
            // Map each variant to its source line so we can fetch any
            // trailing comment the user wrote on that line. The scan
            // starts just after the type's opening line and walks forward
            // in order, which matches how the parser produced the
            // variants list.
            let mut cursor = t.span.line + 1;
            let mut lines: Vec<String> = Vec::with_capacity(variants.len());
            let last_idx = variants.len().saturating_sub(1);
            for (i, v) in variants.iter().enumerate() {
                let name = resolve(v.name);
                let src_line = find_ident_line(&name, cursor, close_line);
                if let Some(l) = src_line {
                    cursor = l + 1;
                }
                let head = if v.fields.is_empty() {
                    format!("{}{}", indent(depth + 1), v.name)
                } else {
                    let fields: Vec<String> = v.fields.iter().map(format_type_expr).collect();
                    format!("{}{}({})", indent(depth + 1), v.name, fields.join(", "))
                };
                // Original enum formatting omits the trailing comma on
                // the last variant. Preserve that behavior unless the
                // last variant has an attached trailing comment, in
                // which case we need a comma to separate `entry ,-- c`.
                let trailing = src_line.and_then(take_trailing_for_line);
                let needs_comma = i < last_idx || trailing.is_some();
                let comma = if needs_comma { "," } else { "" };
                let tail = match trailing {
                    Some(tc) => format!("{head}{comma} {tc}"),
                    None => format!("{head}{comma}"),
                };
                lines.push(tail);
            }
            format!(
                "{prefix}{pub_prefix}type {}{params} {{\n{}\n{prefix}}}",
                t.name,
                lines.join("\n")
            )
        }
        TypeBody::Record(fields) => {
            let mut cursor = t.span.line + 1;
            let mut lines: Vec<String> = Vec::with_capacity(fields.len());
            for f in fields {
                let name = resolve(f.name);
                let src_line = find_ident_line(&name, cursor, close_line);
                if let Some(l) = src_line {
                    cursor = l + 1;
                }
                let head = format!(
                    "{}{}: {}",
                    indent(depth + 1),
                    f.name,
                    format_type_expr(&f.ty)
                );
                let trailing = src_line.and_then(take_trailing_for_line);
                // Record fields always receive a trailing comma so the
                // syntax is uniform whether or not a trailing comment
                // follows.
                let tail = match trailing {
                    Some(tc) => format!("{head}, {tc}"),
                    None => format!("{head},"),
                };
                lines.push(tail);
            }
            format!(
                "{prefix}{pub_prefix}type {}{params} {{\n{}\n{prefix}}}",
                t.name,
                lines.join("\n")
            )
        }
    }
}

fn format_trait_with_comments(t: &TraitDecl, depth: usize) -> String {
    let prefix = indent(depth);
    let close_line = compute_block_end_line(t.span);
    let body = format_trait_methods(&t.methods, depth + 1, close_line);
    format!("{prefix}trait {} {{\n{}\n{prefix}}}", t.name, body)
}

fn format_trait_impl_with_comments(t: &TraitImpl, depth: usize) -> String {
    let prefix = indent(depth);
    let close_line = compute_block_end_line(t.span);
    let body = format_trait_methods(&t.methods, depth + 1, close_line);
    format!(
        "{prefix}trait {} for {} {{\n{}\n{prefix}}}",
        t.trait_name, t.target_type, body
    )
}

/// Format a list of trait/trait-impl methods with interleaved standalone
/// separator comments drawn from the thread-local `FmtState`.
///
/// - Comments before the first method become leading comments on the
///   first method.
/// - Comments strictly between two methods (i.e. on lines between the
///   previous method's last source line and the next method's start
///   line) are emitted as standalone separator lines.
/// - Any remaining comments before the closing `}` are emitted after
///   the last method and before the `}`.
fn format_trait_methods(methods: &[FnDecl], depth: usize, close_line: usize) -> String {
    let mut out = String::new();
    for (i, m) in methods.iter().enumerate() {
        if i > 0 {
            out.push_str("\n\n");
        }
        // Emit any comments that come before this method's `fn` line.
        let pre = take_comments_before(m.span.line);
        for c in &pre {
            out.push_str(&indent(depth));
            out.push_str(c.text.trim());
            out.push('\n');
        }
        out.push_str(&format_fn_with_comments(m, depth));
    }
    // Drain any remaining comments between the last method and the `}`.
    let last_line = methods
        .last()
        .map(|m| compute_block_end_line(m.span))
        .unwrap_or(0);
    let tail = take_comments_between(last_line, close_line);
    for c in &tail {
        out.push('\n');
        out.push_str(&indent(depth));
        out.push_str(c.text.trim());
    }
    out
}

fn format_import(i: &ImportTarget, depth: usize) -> String {
    let prefix = indent(depth);
    match i {
        ImportTarget::Module(name) => format!("{prefix}import {name}"),
        ImportTarget::Items(module, items) => {
            let item_strs: Vec<String> = items.iter().map(|i| resolve(*i)).collect();
            format!("{prefix}import {module}.{{ {} }}", item_strs.join(", "))
        }
        ImportTarget::Alias(module, alias) => {
            format!("{prefix}import {module} as {alias}")
        }
    }
}

fn format_stmt(stmt: &Stmt, depth: usize) -> String {
    let prefix = indent(depth);
    match stmt {
        Stmt::Let { pattern, ty, value } => {
            let pat = format_pattern(pattern);
            let ty_str = if let Some(t) = ty {
                format!(": {}", format_type_expr(t))
            } else {
                String::new()
            };
            format!("{prefix}let {pat}{ty_str} = {}", format_expr(value, depth))
        }
        Stmt::When {
            pattern,
            expr,
            else_body,
        } => {
            let pat = format_pattern(pattern);
            format!(
                "{prefix}when {pat} = {} else {}",
                format_expr(expr, depth),
                format_body(else_body, depth)
            )
        }
        Stmt::WhenBool {
            condition,
            else_body,
        } => {
            format!(
                "{prefix}when {} else {}",
                format_expr(condition, depth),
                format_body(else_body, depth)
            )
        }
        Stmt::Expr(expr) => {
            format!("{prefix}{}", format_expr(expr, depth))
        }
    }
}

fn format_expr(expr: &Expr, depth: usize) -> String {
    // Preserve multi-line triple-quoted strings byte-for-byte from the
    // original source so users' chosen indentation / whitespace survive
    // formatting unchanged.
    if let ExprKind::StringLit(s, true) = &expr.kind
        && s.contains('\n')
        && let Some(raw) = extract_triple_string_raw(expr.span.offset)
    {
        return raw;
    }
    // Block expressions (including the body of a nested `let x = { ... }`,
    // a match arm body, a loop body, a lambda body, or a bare block RHS)
    // must go through `format_body`, which consults the thread-local
    // `FmtState` to emit interleaved comments at the correct nested
    // position. The other `format_expr_inner` arms already recurse
    // through `format_expr` for their sub-expressions, so nested blocks
    // inside e.g. a call or a match arm's body will reach this branch.
    if matches!(expr.kind, ExprKind::Block(_)) {
        return format_body(expr, depth);
    }
    // Match expressions may also own a standalone range of lines with
    // comments in between their arms, so route them through a
    // dedicated helper that knows to consult `FmtState`.
    if matches!(expr.kind, ExprKind::Match { .. }) {
        return format_match_expr(expr, depth);
    }
    // Pipe chains may carry per-stage trailing comments that need to
    // be emitted next to the originating stage.
    if matches!(expr.kind, ExprKind::Pipe(..)) {
        return format_pipe_chain_expr(expr, depth);
    }
    format_expr_inner(&expr.kind, depth)
}

/// Format a pipe chain expression, preserving any trailing comments
/// attached to individual pipe stages.
fn format_pipe_chain_expr(expr: &Expr, depth: usize) -> String {
    let mut stages: Vec<&Expr> = Vec::new();
    collect_pipe_stages_expr(expr, &mut stages);
    if stages.is_empty() {
        return String::new();
    }
    let first = format_expr(stages[0], depth);
    let mut result = first;
    // Trailing comment on the first stage's source line.
    if let Some(tc) = take_trailing_for_line(stages[0].span.line) {
        result.push(' ');
        result.push_str(&tc);
    }
    for stage in &stages[1..] {
        result.push('\n');
        result.push_str(&indent(depth));
        result.push_str("|> ");
        result.push_str(&format_expr(stage, depth));
        if let Some(tc) = take_trailing_for_line(stage.span.line) {
            result.push(' ');
            result.push_str(&tc);
        }
    }
    result
}

/// Like `collect_pipe_stages` but walks the `Expr` wrapper so callers
/// get span information for each stage.
fn collect_pipe_stages_expr<'a>(expr: &'a Expr, stages: &mut Vec<&'a Expr>) {
    if let ExprKind::Pipe(left, right) = &expr.kind {
        collect_pipe_stages_expr(left, stages);
        stages.push(right);
    } else {
        stages.push(expr);
    }
}

/// Format an `ExprKind::Match` with state-aware comment delivery for
/// each arm. Standalone comments that fall between arms are emitted as
/// separator lines before the relevant arm; trailing comments sharing
/// an arm's source line are inlined after the arm.
fn format_match_expr(expr: &Expr, depth: usize) -> String {
    let ExprKind::Match { expr: scrutinee, arms } = &expr.kind else {
        unreachable!()
    };
    let close_line = compute_block_end_line(expr.span);
    let header = match scrutinee {
        Some(s) => format!("match {} ", format_expr(s, depth)),
        None => "match ".to_string(),
    };
    let guardless = scrutinee.is_none();

    let mut lines: Vec<String> = Vec::new();
    let mut last_arm_line = 0usize;
    for arm in arms.iter() {
        let arm_line = arm.body.span.line;
        // Standalone comments before this arm become leading comment lines.
        let pre = take_comments_before(arm_line);
        for c in &pre {
            lines.push(format!("{}{}", indent(depth + 1), c.text.trim()));
        }
        let mut arm_str = format_match_arm(arm, depth + 1, guardless);
        if let Some(tc) = take_trailing_for_line(arm_line) {
            arm_str.push(' ');
            arm_str.push_str(&tc);
        }
        lines.push(arm_str);
        last_arm_line = arm_line;
    }
    // Drain comments between the last arm and the closing `}` of the match.
    let tail = take_comments_between(last_arm_line, close_line);
    for c in &tail {
        lines.push(format!("{}{}", indent(depth + 1), c.text.trim()));
    }

    format!(
        "{header}{{\n{}\n{}}}",
        lines.join("\n"),
        indent(depth)
    )
}

fn format_expr_inner(kind: &ExprKind, depth: usize) -> String {
    match kind {
        ExprKind::Int(n) => n.to_string(),
        ExprKind::Float(n) => {
            let s = n.to_string();
            if s.contains('.') { s } else { format!("{s}.0") }
        }
        ExprKind::Bool(b) => b.to_string(),
        ExprKind::StringLit(s, triple) => {
            if *triple {
                format_triple_string(s, depth)
            } else {
                format!("\"{}\"", escape_string(s))
            }
        }
        ExprKind::StringInterp(parts) => {
            let mut result = String::from('"');
            for part in parts {
                match part {
                    StringPart::Literal(s) => result.push_str(&escape_string(s)),
                    StringPart::Expr(e) => {
                        result.push('{');
                        result.push_str(&format_expr(e, depth));
                        result.push('}');
                    }
                }
            }
            result.push('"');
            result
        }
        ExprKind::Unit => "()".to_string(),
        ExprKind::Ident(name) => resolve(*name),

        ExprKind::List(elems) => {
            if elems.is_empty() {
                "[]".to_string()
            } else {
                let items: Vec<String> = elems
                    .iter()
                    .map(|elem| match elem {
                        ListElem::Single(e) => format_expr(e, depth),
                        ListElem::Spread(e) => format!("..{}", format_expr(e, depth)),
                    })
                    .collect();
                format!("[{}]", items.join(", "))
            }
        }

        ExprKind::Map(pairs) => {
            if pairs.is_empty() {
                "#{}".to_string()
            } else {
                let items: Vec<String> = pairs
                    .iter()
                    .map(|(k, v)| format!("{}: {}", format_expr(k, depth), format_expr(v, depth)))
                    .collect();
                format!("#{{ {} }}", items.join(", "))
            }
        }

        ExprKind::SetLit(elems) => {
            if elems.is_empty() {
                "#[]".to_string()
            } else {
                let items: Vec<String> = elems.iter().map(|e| format_expr(e, depth)).collect();
                format!("#[{}]", items.join(", "))
            }
        }

        ExprKind::Tuple(elems) => {
            let items: Vec<String> = elems.iter().map(|e| format_expr(e, depth)).collect();
            format!("({})", items.join(", "))
        }

        ExprKind::Binary(left, op, right) => {
            let l = format_expr_with_parens(left, *op, true, depth);
            let r = format_expr_with_parens(right, *op, false, depth);
            format!("{l} {op} {r}")
        }

        ExprKind::Unary(op, expr) => {
            let inner = format_expr(expr, depth);
            match op {
                UnaryOp::Neg => format!("-{inner}"),
                UnaryOp::Not => format!("!{inner}"),
            }
        }

        ExprKind::Pipe(..) => {
            // Unreachable: `format_expr` intercepts `Pipe` expressions
            // and delegates to `format_pipe_chain_expr`, which walks
            // the AST at the `Expr` level so it can look up trailing
            // comments per stage via their spans.
            unreachable!("pipe should be handled by format_pipe_chain_expr")
        }

        ExprKind::Range(start, end) => {
            format!("{}..{}", format_expr(start, depth), format_expr(end, depth))
        }

        ExprKind::QuestionMark(expr) => {
            format!("{}?", format_expr(expr, depth))
        }

        ExprKind::Ascription(expr, ty) => {
            format!("{} as {}", format_expr(expr, depth), format_type_expr(ty))
        }

        ExprKind::Call(callee, args) => {
            let callee_str = format_expr(callee, depth);
            // Trailing closure detection: if last arg is a lambda, format differently
            if let Some((last, init)) = args.split_last()
                && matches!(last.kind, ExprKind::Lambda { .. })
            {
                let lambda_str = format_trailing_closure(last, depth);
                if init.is_empty() {
                    return format!("{callee_str} {lambda_str}");
                } else {
                    let arg_strs: Vec<String> =
                        init.iter().map(|a| format_expr(a, depth)).collect();
                    return format!("{callee_str}({}) {lambda_str}", arg_strs.join(", "));
                }
            }
            let arg_strs: Vec<String> = args.iter().map(|a| format_expr(a, depth)).collect();
            format!("{callee_str}({})", arg_strs.join(", "))
        }

        ExprKind::Lambda { params, body } => {
            let param_strs: Vec<String> = params.iter().map(format_param).collect();
            let params_str = param_strs.join(", ");
            format!("fn({params_str}) {}", format_body(body, depth))
        }

        ExprKind::FieldAccess(expr, field) => {
            format!("{}.{field}", format_expr(expr, depth))
        }

        ExprKind::RecordCreate { name, fields } => {
            let field_strs: Vec<String> = fields
                .iter()
                .map(|(fname, fexpr)| format!("{fname}: {}", format_expr(fexpr, depth)))
                .collect();
            format!("{name} {{ {} }}", field_strs.join(", "))
        }

        ExprKind::RecordUpdate { expr, fields } => {
            let field_strs: Vec<String> = fields
                .iter()
                .map(|(fname, fexpr)| format!("{fname}: {}", format_expr(fexpr, depth)))
                .collect();
            format!(
                "{}.{{ {} }}",
                format_expr(expr, depth),
                field_strs.join(", ")
            )
        }

        ExprKind::Match { expr, arms } => match expr {
            Some(scrutinee) => {
                let scrutinee_str = format_expr(scrutinee, depth);
                let arm_strs: Vec<String> = arms
                    .iter()
                    .map(|arm| format_match_arm(arm, depth + 1, false))
                    .collect();
                format!(
                    "match {scrutinee_str} {{\n{}\n{}}}",
                    arm_strs.join("\n"),
                    indent(depth)
                )
            }
            None => {
                let arm_strs: Vec<String> = arms
                    .iter()
                    .map(|arm| format_match_arm(arm, depth + 1, true))
                    .collect();
                format!("match {{\n{}\n{}}}", arm_strs.join("\n"), indent(depth))
            }
        },

        ExprKind::Return(val) => {
            if let Some(e) = val {
                format!("return {}", format_expr(e, depth))
            } else {
                "return".to_string()
            }
        }

        ExprKind::Block(stmts) => {
            if stmts.is_empty() {
                "{}".to_string()
            } else {
                let inner: Vec<String> = stmts.iter().map(|s| format_stmt(s, depth + 1)).collect();
                format!("{{\n{}\n{}}}", inner.join("\n"), indent(depth))
            }
        }

        ExprKind::Loop { bindings, body } => {
            let body_str = format_expr(body, depth);
            if bindings.is_empty() {
                format!("loop {body_str}")
            } else {
                let binding_strs: Vec<String> = bindings
                    .iter()
                    .map(|(name, init)| format!("{name} = {}", format_expr(init, depth)))
                    .collect();
                format!("loop {} {body_str}", binding_strs.join(", "))
            }
        }

        ExprKind::Recur(args) => {
            if args.is_empty() {
                "loop()".to_string()
            } else {
                let arg_strs: Vec<String> = args.iter().map(|a| format_expr(a, depth)).collect();
                format!("loop({})", arg_strs.join(", "))
            }
        }
        ExprKind::FloatElse(expr, fallback) => {
            format!(
                "{} else {}",
                format_expr(expr, depth),
                format_expr(fallback, depth)
            )
        }
    }
}

fn format_trailing_closure(expr: &Expr, depth: usize) -> String {
    if let ExprKind::Lambda { params, body } = &expr.kind {
        let param_strs: Vec<String> = params.iter().map(format_param).collect();
        let params_str = param_strs.join(", ");
        if let ExprKind::Block(stmts) = &body.kind {
            if stmts.len() == 1
                && let Stmt::Expr(inner) = &stmts[0]
            {
                return format!("{{ {params_str} -> {} }}", format_expr(inner, depth));
            }
            // Multi-statement trailing closure. Use the state-aware
            // statement formatter so any comments inside the closure's
            // body block are emitted at the correct nested position.
            let close_line = compute_block_end_line(body.span);
            let inner = format_stmts_with_comments(stmts, depth + 1, close_line);
            return format!(
                "{{ {params_str} ->\n{}\n{}}}",
                inner,
                indent(depth)
            );
        }
        format!("{{ {params_str} -> {} }}", format_expr(body, depth))
    } else {
        format_expr(expr, depth)
    }
}

fn format_match_arm(arm: &MatchArm, depth: usize, guardless: bool) -> String {
    let prefix = indent(depth);
    if guardless {
        // Guardless match: print the condition expression or `_` for bare wildcard
        if let Some(g) = &arm.guard {
            format!(
                "{prefix}{} -> {}",
                format_expr(g, depth),
                format_expr(&arm.body, depth)
            )
        } else {
            format!("{prefix}_ -> {}", format_expr(&arm.body, depth))
        }
    } else {
        let pat = format_pattern(&arm.pattern);
        let guard = if let Some(g) = &arm.guard {
            format!(" when {}", format_expr(g, depth))
        } else {
            String::new()
        };
        format!("{prefix}{pat}{guard} -> {}", format_expr(&arm.body, depth))
    }
}

fn format_pattern(pattern: &Pattern) -> String {
    match pattern {
        Pattern::Wildcard => "_".to_string(),
        Pattern::Ident(name) => resolve(*name),
        Pattern::Int(n) => n.to_string(),
        Pattern::Float(n) => {
            let s = n.to_string();
            if s.contains('.') { s } else { format!("{s}.0") }
        }
        Pattern::Bool(b) => b.to_string(),
        Pattern::StringLit(s, triple) => {
            if *triple {
                format_triple_string(s, 0)
            } else {
                format!("\"{}\"", escape_string(s))
            }
        }
        Pattern::Tuple(pats) => {
            let items: Vec<String> = pats.iter().map(format_pattern).collect();
            format!("({})", items.join(", "))
        }
        Pattern::Constructor(name, pats) => {
            if pats.is_empty() {
                resolve(*name)
            } else {
                let items: Vec<String> = pats.iter().map(format_pattern).collect();
                format!("{name}({})", items.join(", "))
            }
        }
        Pattern::Record {
            name,
            fields,
            has_rest,
        } => {
            let field_strs: Vec<String> = fields
                .iter()
                .map(|(fname, sub)| {
                    if let Some(p) = sub {
                        format!("{fname}: {}", format_pattern(p))
                    } else {
                        resolve(*fname)
                    }
                })
                .collect();
            let rest = if *has_rest { ", .." } else { "" };
            match name {
                Some(n) => format!("{n} {{ {}{rest} }}", field_strs.join(", ")),
                None => format!("{{ {}{rest} }}", field_strs.join(", ")),
            }
        }
        Pattern::List(pats, rest) => {
            let mut items: Vec<String> = pats.iter().map(format_pattern).collect();
            if let Some(rest_pat) = rest {
                items.push(format!("..{}", format_pattern(rest_pat)));
            }
            format!("[{}]", items.join(", "))
        }
        Pattern::Or(alts) => {
            let items: Vec<String> = alts.iter().map(format_pattern).collect();
            items.join(" | ")
        }
        Pattern::Range(start, end) => format!("{start}..{end}"),
        Pattern::FloatRange(start, end) => format!("{start}..{end}"),
        Pattern::Map(entries) => {
            let items: Vec<String> = entries
                .iter()
                .map(|(key, pat)| format!("\"{key}\": {}", format_pattern(pat)))
                .collect();
            format!("#{{ {} }}", items.join(", "))
        }
        Pattern::Pin(name) => format!("^{name}"),
    }
}

fn format_type_expr(ty: &TypeExpr) -> String {
    match ty {
        TypeExpr::Named(name) => resolve(*name),
        TypeExpr::Generic(name, args) => {
            let arg_strs: Vec<String> = args.iter().map(format_type_expr).collect();
            format!("{name}({})", arg_strs.join(", "))
        }
        TypeExpr::Tuple(elems) => {
            let items: Vec<String> = elems.iter().map(format_type_expr).collect();
            format!("({})", items.join(", "))
        }
        TypeExpr::Function(params, ret) => {
            let param_strs: Vec<String> = params.iter().map(format_type_expr).collect();
            format!("Fn({}) -> {}", param_strs.join(", "), format_type_expr(ret))
        }
        TypeExpr::SelfType => "Self".to_string(),
    }
}

/// Format a triple-quoted string (`"""..."""`).
///
/// For single-line content (no newlines), emits `"""content"""`.
/// For multi-line content, emits:
///   """
///   <indent>line1
///   <indent>line2
///   <indent>"""
/// where `<indent>` is `(depth + 1)` levels of indentation so the lexer's
/// indentation-stripping algorithm recovers the original content.
fn format_triple_string(s: &str, depth: usize) -> String {
    if !s.contains('\n') {
        // Single-line triple-quoted string
        return format!("\"\"\"{}\"\"\"", s);
    }

    // Multi-line: add indentation so the lexer strips it back
    let inner_indent = INDENT.repeat(depth + 1);
    let mut result = String::from("\"\"\"\n");
    for line in s.split('\n') {
        if line.is_empty() {
            result.push('\n');
        } else {
            result.push_str(&inner_indent);
            result.push_str(line);
            result.push('\n');
        }
    }
    result.push_str(&inner_indent);
    result.push_str("\"\"\"");
    result
}

fn escape_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
        .replace('{', "\\{")
        .replace('}', "\\}")
}

fn precedence(op: BinOp) -> u8 {
    match op {
        BinOp::Or => 1,
        BinOp::And => 2,
        BinOp::Eq | BinOp::Neq => 3,
        BinOp::Lt | BinOp::Gt | BinOp::Leq | BinOp::Geq => 4,
        BinOp::Add | BinOp::Sub => 5,
        BinOp::Mul | BinOp::Div | BinOp::Mod => 6,
    }
}

fn format_expr_with_parens(expr: &Expr, parent_op: BinOp, is_left: bool, depth: usize) -> String {
    if let ExprKind::Binary(_, child_op, _) = &expr.kind {
        let parent_prec = precedence(parent_op);
        let child_prec = precedence(*child_op);
        // Need parens if child has lower precedence, or same precedence on the right
        // (for left-associative operators)
        if child_prec < parent_prec || (child_prec == parent_prec && !is_left) {
            return format!("({})", format_expr(expr, depth));
        }
    }
    format_expr(expr, depth)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_comment_between_decls() {
        let source = r#"fn foo() = 1

-- helper function
fn bar() = 2
"#;
        let result = format(source).unwrap();
        assert!(
            result.contains("-- helper function"),
            "comment should be preserved"
        );
        assert!(result.contains("fn foo() = 1"));
        assert!(result.contains("fn bar() = 2"));
    }

    #[test]
    fn test_triple_quoted_string_preserved_idempotent() {
        // Non-canonical wide indentation inside a triple-quoted string
        // must survive formatting byte-for-byte.
        let source = "fn main() {\n  let x = \"\"\"\n      hello\n        nested\n      world\n      \"\"\"\n  println(x)\n}\n";
        let once = format(source).unwrap();
        assert!(
            once.contains("      hello"),
            "expected 6-space indented content to survive; got:\n{once}"
        );
        assert!(
            once.contains("        nested"),
            "expected 8-space indented line to survive; got:\n{once}"
        );
        let twice = format(&once).unwrap();
        assert_eq!(once, twice, "formatter must be idempotent");
    }

    #[test]
    fn test_triple_quoted_string_unusual_indent_preserved() {
        // A triple string whose interior has completely unusual indentation
        // relative to the declaration should not be rewritten.
        let source = "fn main() {\n  let s = \"\"\"\nA\n B\n  C\n\"\"\"\n  println(s)\n}\n";
        let result = format(source).unwrap();
        assert!(
            result.contains("\nA\n B\n  C\n"),
            "unusual indent must be preserved; got:\n{result}"
        );
    }

    #[test]
    fn test_comment_before_first_decl() {
        let source = r#"-- module header
fn main() = 42
"#;
        let result = format(source).unwrap();
        assert!(
            result.starts_with("-- module header\n"),
            "header comment should be at top"
        );
        assert!(result.contains("fn main() = 42"));
    }

    #[test]
    fn test_multiple_comments_between_decls() {
        let source = r#"fn a() = 1

-- first comment
-- second comment
fn b() = 2
"#;
        let result = format(source).unwrap();
        assert!(
            result.contains("-- first comment\n-- second comment"),
            "multiple comments preserved"
        );
    }

    #[test]
    fn test_block_comment_preserved() {
        let source = r#"fn a() = 1

{- block comment -}
fn b() = 2
"#;
        let result = format(source).unwrap();
        assert!(
            result.contains("{- block comment -}"),
            "block comment should be preserved"
        );
    }

    #[test]
    fn test_no_comments_unchanged() {
        let source = r#"fn a() = 1

fn b() = 2
"#;
        let result = format(source).unwrap();
        let expected = "fn a() = 1\n\nfn b() = 2\n";
        assert_eq!(result, expected);
    }

    #[test]
    fn test_idempotent_with_comments() {
        let source = r#"fn foo() = 1

-- a comment
fn bar() = 2
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "formatting should be idempotent");
    }

    #[test]
    fn test_idempotent_with_header_comment() {
        let source = r#"-- header
fn foo() = 1
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "formatting should be idempotent");
    }

    #[test]
    fn test_idempotent_with_multiple_comments() {
        let source = r#"-- header

fn a() = 1

-- between
fn b() = 2

-- another
fn c() = 3
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "formatting should be idempotent");
    }

    #[test]
    fn test_comment_after_last_decl() {
        let source = r#"fn foo() = 1

-- trailing comment
"#;
        let result = format(source).unwrap();
        assert!(
            result.contains("-- trailing comment"),
            "trailing comment should be preserved"
        );
    }

    #[test]
    fn test_extract_comments_basic() {
        let (comments, _trailing) = extract_comments("-- hello\nfn foo() = 1\n-- bye");
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].line, 1);
        assert_eq!(comments[0].text, "-- hello");
        assert_eq!(comments[1].line, 3);
        assert_eq!(comments[1].text, "-- bye");
    }

    #[test]
    fn test_extract_block_comment() {
        let (comments, _trailing) = extract_comments("{- block\ncomment -}\nfn foo() = 1");
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].line, 1);
        assert!(comments[0].text.contains("{- block"));
        assert!(comments[0].text.contains("comment -}"));
    }

    // ── Idempotency tests ──────────────────────────────────────────

    #[test]
    fn test_idempotent_simple_fn() {
        let source = "fn add(a, b) = a + b\n";
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "formatting should be idempotent");
    }

    #[test]
    fn test_idempotent_block_fn() {
        let source = r#"fn main() {
  let x = 42
  println(x)
  x
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "formatting should be idempotent");
    }

    #[test]
    fn test_idempotent_imports_sorted() {
        let source = r#"import list
import channel
import string

fn main() = 42
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "formatting should be idempotent");
        // Verify imports are sorted alphabetically
        let channel_pos = first.find("import channel").unwrap();
        let list_pos = first.find("import list").unwrap();
        let string_pos = first.find("import string").unwrap();
        assert!(channel_pos < list_pos, "channel should come before list");
        assert!(list_pos < string_pos, "list should come before string");
    }

    #[test]
    fn test_idempotent_match_expression() {
        let source = r#"fn classify(x) {
  match x {
    0 -> "zero"
    1 -> "one"
    _ -> "other"
  }
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "formatting should be idempotent");
    }

    #[test]
    fn test_idempotent_nested_match() {
        let source = r#"type Shape {
  Circle(Float),
  Rect(Float, Float),
}

fn describe(s) {
  match s {
    Circle(r) -> match r > 10.0 {
      true -> "big circle"
      false -> "small circle"
    }
    Rect(w, h) -> match w == h {
      true -> "square"
      false -> "rectangle"
    }
  }
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "formatting should be idempotent");
    }

    #[test]
    fn test_idempotent_pipe_chain() {
        let source = r#"import list

fn main() {
  [1, 2, 3]
  |> list.map(fn(x) { x * 2 })
  |> list.filter(fn(x) { x > 2 })
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "formatting should be idempotent");
    }

    #[test]
    fn test_idempotent_trait_and_impl() {
        let source = r#"trait Printable {
  fn show(self) -> String
}

trait Printable for Int {
  fn show(self) = "{self}"
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "formatting should be idempotent");
    }

    #[test]
    fn test_idempotent_record_type() {
        let source = r#"type User {
  name: String,
  age: Int,
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "formatting should be idempotent");
    }

    #[test]
    fn test_idempotent_lambda_in_call() {
        let source = r#"import list

fn main() {
  list.map([1, 2, 3], fn(x) {
    x * 2
  })
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "formatting should be idempotent");
    }

    #[test]
    fn test_idempotent_where_clause() {
        let source = "fn show(x) where x: Display = x.display()\n";
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "formatting should be idempotent");
    }

    #[test]
    fn test_idempotent_complex_program() {
        let source = r#"-- Module header comment
import list
import string

-- A type definition
type Color {
  Red,
  Green,
  Blue,
}

-- Main function
fn main() {
  let colors = [Red, Green, Blue]
  colors
  |> list.map(fn(c) {
    match c {
      Red -> "red"
      Green -> "green"
      Blue -> "blue"
    }
  })
  |> string.join(", ")
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "formatting should be idempotent");
    }

    // ── Edge case tests ────────────────────────────────────────────

    #[test]
    fn test_format_empty_source() {
        let result = format("").unwrap();
        assert_eq!(result, "\n");
    }

    #[test]
    fn test_format_only_comments() {
        let result = format("-- just a comment").unwrap();
        assert!(result.contains("-- just a comment"));
        assert!(result.ends_with('\n'));
    }

    #[test]
    fn test_format_only_block_comment() {
        let result = format("{- a block comment -}").unwrap();
        assert!(result.contains("{- a block comment -}"));
        assert!(result.ends_with('\n'));
    }

    #[test]
    fn test_format_single_expression_fn() {
        let result = format("fn add(a, b) = a + b\n").unwrap();
        assert_eq!(result, "fn add(a, b) = a + b\n");
    }

    #[test]
    fn test_format_empty_fn_body() {
        let result = format("fn noop() {}\n").unwrap();
        assert!(result.contains("fn noop()"));
    }

    #[test]
    fn test_format_pub_fn() {
        let result = format("pub fn add(a, b) = a + b\n").unwrap();
        assert!(result.starts_with("pub fn add"));
    }

    #[test]
    fn test_format_return_type_annotation() {
        let result = format("fn add(a: Int, b: Int) -> Int = a + b\n").unwrap();
        assert!(result.contains("-> Int"));
        assert!(result.contains("a: Int, b: Int"));
    }

    // ── Complex expression formatting ──────────────────────────────

    #[test]
    fn test_format_nested_match() {
        let source = r#"fn foo(x) {
  match x {
    Some(v) -> match v {
      1 -> "one"
      _ -> "other"
    }
    None -> "none"
  }
}
"#;
        let result = format(source).unwrap();
        assert!(result.contains("match x"));
        assert!(result.contains("Some(v) ->"));
        assert!(result.contains("None ->"));
    }

    #[test]
    fn test_format_pipe_chain() {
        let source = r#"import list
fn main() { [1, 2, 3] |> list.map(fn(x) { x * 2 }) |> list.filter(fn(x) { x > 2 }) }
"#;
        let result = format(source).unwrap();
        assert!(result.contains("|>"), "pipe operator should be preserved");
    }

    #[test]
    fn test_format_trailing_closure() {
        let source = r#"import list
fn main() {
  list.map([1, 2], fn(x) { x * 2 })
}
"#;
        let result = format(source).unwrap();
        // Should produce a trailing closure format
        assert!(result.contains("list.map"));
    }

    #[test]
    fn test_format_deeply_nested_block() {
        let source = r#"fn main() {
  let x = {
    let y = {
      let z = 42
      z
    }
    y
  }
  x
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "deeply nested blocks should be idempotent");
    }

    #[test]
    fn test_format_loop_expression() {
        let source = "fn countdown(n) = loop i = n { match i { 0 -> 0 _ -> loop(i - 1) } }\n";
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "loop formatting should be idempotent");
    }

    #[test]
    fn test_format_record_create() {
        let source = r#"type Point { x: Int, y: Int }
fn main() = Point { x: 1, y: 2 }
"#;
        let result = format(source).unwrap();
        assert!(result.contains("Point { x: 1, y: 2 }"));
    }

    #[test]
    fn test_format_map_literal() {
        let source = r#"fn main() = #{"a": 1, "b": 2}
"#;
        let result = format(source).unwrap();
        assert!(result.contains("#{ "));
    }

    #[test]
    fn test_format_list_literal() {
        let result = format("fn main() = [1, 2, 3]\n").unwrap();
        assert!(result.contains("[1, 2, 3]"));
    }

    #[test]
    fn test_format_empty_list() {
        let result = format("fn main() = []\n").unwrap();
        assert!(result.contains("[]"));
    }

    #[test]
    fn test_format_tuple() {
        let result = format("fn main() = (1, 2, 3)\n").unwrap();
        assert!(result.contains("(1, 2, 3)"));
    }

    #[test]
    fn test_format_unary_ops() {
        let result = format("fn main() = -42\n").unwrap();
        assert!(result.contains("-42"));
    }

    #[test]
    fn test_format_not_op() {
        let result = format("fn main() = !true\n").unwrap();
        assert!(result.contains("!true"));
    }

    #[test]
    fn test_format_binary_precedence_parens() {
        // Ensure parentheses are added when needed for precedence
        let source = "fn main() = (1 + 2) * 3\n";
        let result = format(source).unwrap();
        assert!(result.contains("(1 + 2) * 3"));
    }

    #[test]
    fn test_format_string_interpolation() {
        let source = r#"fn greet(name) = "hello {name}"
"#;
        let result = format(source).unwrap();
        assert!(result.contains("{name}"));
    }

    #[test]
    fn test_format_question_mark() {
        let source = "fn try_it(x) = x?\n";
        let result = format(source).unwrap();
        assert!(result.contains("x?"));
    }

    #[test]
    fn test_format_range() {
        let result = format("fn main() = 1..10\n").unwrap();
        assert!(result.contains("1..10"));
    }

    #[test]
    fn test_format_when_bool_stmt() {
        let source = r#"fn main() {
  when true else {
    return 0
  }
  42
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "when-bool formatting should be idempotent");
    }

    #[test]
    fn test_format_when_pattern_stmt() {
        let source = r#"fn main() {
  when Some(x) = Some(42) else {
    return 0
  }
  x
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(
            first, second,
            "when-pattern formatting should be idempotent"
        );
    }

    #[test]
    fn test_format_enum_type() {
        let result = format("type Color { Red, Green, Blue }\n").unwrap();
        assert!(result.contains("Red"));
        assert!(result.contains("Green"));
        assert!(result.contains("Blue"));
    }

    #[test]
    fn test_format_enum_with_fields() {
        let source = "type Shape { Circle(Float), Rect(Float, Float) }\n";
        let result = format(source).unwrap();
        assert!(result.contains("Circle(Float)"));
        assert!(result.contains("Rect(Float, Float)"));
    }

    #[test]
    fn test_format_import_sorting() {
        let source = r#"import string
import list
import channel

fn main() = 1
"#;
        let result = format(source).unwrap();
        let channel_pos = result.find("import channel").unwrap();
        let list_pos = result.find("import list").unwrap();
        let string_pos = result.find("import string").unwrap();
        assert!(
            channel_pos < list_pos,
            "imports should be sorted alphabetically"
        );
        assert!(
            list_pos < string_pos,
            "imports should be sorted alphabetically"
        );
    }

    #[test]
    fn test_format_selective_import() {
        let result = format("import list.{ map, filter }\nfn main() = 1\n").unwrap();
        assert!(result.contains("import list.{ map, filter }"));
    }

    #[test]
    fn test_format_alias_import() {
        let result = format("import list as l\nfn main() = 1\n").unwrap();
        assert!(result.contains("import list as l"));
    }

    #[test]
    fn test_format_guardless_match() {
        let source = r#"fn classify(x) {
  match {
    x > 100 -> "big"
    x > 0 -> "positive"
    _ -> "other"
  }
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(
            first, second,
            "guardless match formatting should be idempotent"
        );
    }

    #[test]
    fn test_format_match_with_guard() {
        let source = r#"fn main() {
  match 42 {
    x when x > 0 -> "positive"
    _ -> "non-positive"
  }
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(
            first, second,
            "match with guard formatting should be idempotent"
        );
    }

    #[test]
    fn test_format_record_update() {
        let source = r#"type Point { x: Int, y: Int }
fn main() {
  let p = Point { x: 1, y: 2 }
  p.{ x: 10 }
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(
            first, second,
            "record update formatting should be idempotent"
        );
    }

    #[test]
    fn test_format_let_with_type() {
        let source = r#"fn main() {
  let x: Int = 42
  x
}
"#;
        let first = format(source).unwrap();
        assert!(first.contains("let x: Int = 42"));
        let second = format(&first).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn test_format_pub_let() {
        let source = "pub let version = 1\n";
        let result = format(source).unwrap();
        assert!(result.contains("pub let version = 1"));
    }

    // ── Pattern formatting ──────────────────────────────────────────

    #[test]
    fn test_format_or_pattern() {
        let source = r#"fn main() {
  match 1 {
    1 | 2 | 3 -> "low"
    _ -> "high"
  }
}
"#;
        let first = format(source).unwrap();
        assert!(first.contains("1 | 2 | 3"));
        let second = format(&first).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn test_format_list_pattern() {
        let source = r#"fn main() {
  match [1, 2, 3] {
    [h, ..rest] -> h
    [] -> 0
  }
}
"#;
        let first = format(source).unwrap();
        assert!(first.contains("[h, ..rest]"));
        let second = format(&first).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn test_format_pin_pattern() {
        let source = r#"fn main() {
  let x = 42
  match 42 {
    ^x -> "match"
    _ -> "no match"
  }
}
"#;
        let result = format(source).unwrap();
        assert!(result.contains("^x"));
    }

    #[test]
    fn test_format_field_access() {
        let result = format("fn main() = foo.bar.baz\n").unwrap();
        assert!(result.contains("foo.bar.baz"));
    }

    #[test]
    fn test_format_return_expression() {
        let source = r#"fn main() {
  return 42
}
"#;
        let result = format(source).unwrap();
        assert!(result.contains("return 42"));
    }

    #[test]
    fn test_format_return_void() {
        let source = r#"fn main() {
  return
}
"#;
        let result = format(source).unwrap();
        assert!(result.contains("return"));
    }

    // ── Idempotency: string interpolation ───────────────────────────

    #[test]
    fn test_idempotent_string_interpolation() {
        let source = r#"fn main() {
  let name = "world"
  println("hello {name}, count={1 + 2}")
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "string interpolation should be idempotent");
    }

    // ── Idempotency: map literals ───────────────────────────────────

    #[test]
    fn test_idempotent_map_literal() {
        let source = r#"fn main() {
  let m = #{"a": 1, "b": 2, "c": 3}
  m
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "map literal should be idempotent");
    }

    // ── Idempotency: tuple literals and patterns ────────────────────

    #[test]
    fn test_idempotent_tuple() {
        let source = r#"fn main() {
  let t = (1, "hello", true)
  match t {
    (1, s, _) -> s
    _ -> "other"
  }
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(
            first, second,
            "tuple literal and pattern should be idempotent"
        );
    }

    // ── Idempotency: map patterns ───────────────────────────────────

    #[test]
    fn test_idempotent_map_pattern() {
        let source = r#"fn check(m) {
  match m {
    #{"key": v} -> v
    _ -> "missing"
  }
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "map pattern should be idempotent");
    }

    // ── Idempotency: constructor patterns ───────────────────────────

    #[test]
    fn test_idempotent_constructor_pattern() {
        let source = r#"fn unwrap(opt) {
  match opt {
    Some(x) -> x
    None -> 0
  }
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "constructor pattern should be idempotent");
    }

    // ── Idempotency: negative number patterns ───────────────────────

    #[test]
    fn test_idempotent_negative_pattern() {
        let source = r#"fn classify(n) {
  match n {
    -1 -> "neg one"
    0 -> "zero"
    1 -> "one"
    _ -> "other"
  }
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "negative pattern should be idempotent");
    }

    // ── Idempotency: range patterns ─────────────────────────────────

    #[test]
    fn test_idempotent_range_pattern() {
        let source = r#"fn classify(n) {
  match n {
    1..10 -> "small"
    10..100 -> "medium"
    _ -> "large"
  }
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "range pattern should be idempotent");
    }

    // ── Idempotency: complex type annotations ───────────────────────

    #[test]
    fn test_idempotent_type_annotations() {
        let source = r#"fn add(a: Int, b: Int) -> Int {
  a + b
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "type annotations should be idempotent");
    }

    // ── Idempotency: loop with multiple bindings ────────────────────

    #[test]
    fn test_idempotent_loop_bindings() {
        let source = r#"fn main() {
  loop i = 0, acc = 0 {
    match i >= 10 {
      true -> acc
      _ -> loop(i + 1, acc + i)
    }
  }
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "loop with bindings should be idempotent");
    }

    // ── Idempotency: record pattern ─────────────────────────────────

    #[test]
    fn test_idempotent_record_pattern() {
        let source = r#"type Point { x: Int, y: Int }

fn origin(p) {
  match p {
    Point { x: 0, y: 0 } -> true
    _ -> false
  }
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "record pattern should be idempotent");
    }

    // ── Idempotency: chained method calls ───────────────────────────

    #[test]
    fn test_idempotent_chained_calls() {
        let source = r#"import list

fn main() {
  [1, 2, 3, 4, 5]
  |> list.filter { x -> x > 2 }
  |> list.map { x -> x * 10 }
  |> list.fold(0) { acc, x -> acc + x }
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second, "chained calls should be idempotent");
    }

    // ── Body comment tests ──────────────────────────────────────────

    #[test]
    fn test_comment_inside_fn_body() {
        // Regression lock for e78d6d9 "Preserve comments inside function bodies".
        // Must verify POSITION, not just presence — a bug that moves the comment
        // outside the fn body (e.g. to after the closing brace) would still
        // make a .contains() check pass.
        let source = r#"fn main() {
  -- setup variables
  let x = 42
  x
}
"#;
        let result = format(source).unwrap();
        let expected = "fn main() {\n  -- setup variables\n  let x = 42\n  x\n}\n";
        assert_eq!(
            result, expected,
            "comment must remain between `{{` and `let x`, got: {result}"
        );
        // Extra defensive check: the comment must appear before the closing
        // brace of main, not after it.
        let comment_pos = result.find("-- setup variables").unwrap();
        let close_pos = result.find("\n}").unwrap();
        assert!(
            comment_pos < close_pos,
            "body comment was hoisted outside fn body, got: {result}"
        );
    }

    #[test]
    fn test_multiple_comments_inside_fn_body() {
        // Regression lock for e78d6d9 — must verify each comment stays at its
        // original position relative to the statements, not just that they
        // survive somewhere in the output.
        let source = r#"fn main() {
  -- first comment
  let x = 1
  -- second comment
  let y = 2
  x + y
}
"#;
        let result = format(source).unwrap();
        let expected = "fn main() {\n  -- first comment\n  let x = 1\n  -- second comment\n  let y = 2\n  x + y\n}\n";
        assert_eq!(
            result, expected,
            "body comments must interleave with statements, got: {result}"
        );
        // Defensive: neither comment may appear after the closing brace.
        let close_pos = result.find("\n}").unwrap();
        assert!(result.find("-- first comment").unwrap() < close_pos);
        assert!(result.find("-- second comment").unwrap() < close_pos);
    }

    #[test]
    fn test_body_comment_and_between_comment() {
        // Regression lock for e78d6d9 — the "inside foo" comment must stay
        // inside the foo body, and the "between functions" comment must stay
        // between the two decls (not collapse into foo's body or be hoisted
        // to the end of the file).
        let source = r#"fn foo() {
  -- inside foo
  let x = 1
  x
}

-- between functions
fn bar() = 2
"#;
        let result = format(source).unwrap();
        // foo's closing brace must come after "inside foo" and before
        // "between functions".
        let inside_pos = result.find("-- inside foo").unwrap();
        let foo_close = result.find("}\n").unwrap();
        let between_pos = result.find("-- between functions").unwrap();
        let bar_pos = result.find("fn bar").unwrap();
        assert!(
            inside_pos < foo_close,
            "inside-foo comment was hoisted out of foo, got: {result}"
        );
        assert!(
            foo_close < between_pos,
            "between comment fell inside foo, got: {result}"
        );
        assert!(
            between_pos < bar_pos,
            "between comment was hoisted past bar, got: {result}"
        );
    }

    #[test]
    fn test_idempotent_body_comment() {
        let source = r#"fn main() {
  -- setup
  let x = 42
  -- use it
  println(x)
  x
}
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(
            first, second,
            "formatting with body comments should be idempotent"
        );
    }

    #[test]
    fn test_idempotent_body_and_toplevel_comments() {
        let source = r#"-- header
fn foo() {
  -- body comment
  let x = 1
  x
}

-- between
fn bar() = 2
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(
            first, second,
            "formatting with mixed comments should be idempotent"
        );
    }

    #[test]
    fn test_comment_after_last_stmt_in_body() {
        // Regression lock: the comment immediately before the closing brace
        // must stay inside the fn body, not be hoisted after the `}`.
        let source = r#"fn main() {
  let x = 42
  -- trailing body comment
}
"#;
        let result = format(source).unwrap();
        let comment_pos = result.find("-- trailing body comment").unwrap();
        let close_pos = result.find("\n}").unwrap();
        assert!(
            comment_pos < close_pos,
            "trailing body comment was hoisted outside fn body, got: {result}"
        );
    }

    #[test]
    fn test_comments_move_with_imports_during_sort() {
        let source = r#"import b
-- This explains why we need a
import a
"#;
        let result = format(source).unwrap();
        // After sorting, `import a` should come first and its comment should precede it
        assert!(
            result.contains("-- This explains why we need a\nimport a"),
            "comment should move with its associated import, got: {result}"
        );
        let a_pos = result.find("import a").unwrap();
        let b_pos = result.find("import b").unwrap();
        assert!(
            a_pos < b_pos,
            "import a should come before import b after sorting, got: {result}"
        );
    }

    #[test]
    fn test_comments_between_imports_idempotent() {
        let source = r#"-- This explains why we need a
import a
-- This explains why we need b
import b
"#;
        let first = format(source).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(
            first, second,
            "formatting imports with comments should be idempotent, got: {first}"
        );
    }

    #[test]
    fn test_multiple_comments_move_with_import() {
        let source = r#"import z
-- first comment for a
-- second comment for a
import a
"#;
        let result = format(source).unwrap();
        assert!(
            result.contains("-- first comment for a\n-- second comment for a\nimport a"),
            "multiple comments should move with their import, got: {result}"
        );
        let a_pos = result.find("import a").unwrap();
        let z_pos = result.find("import z").unwrap();
        assert!(
            a_pos < z_pos,
            "import a should come before import z after sorting, got: {result}"
        );
    }

    #[test]
    fn test_block_comment_with_double_dash_not_extracted_as_trailing() {
        // A block comment containing `--` should NOT be treated as a trailing line comment.
        let line = r#"let x = 1 {- not -- a comment -} + 2"#;
        let result = extract_trailing_comment_from_line(line);
        assert!(
            result.is_none(),
            "block comment containing -- should not be extracted as trailing comment, got: {result:?}"
        );
    }

    #[test]
    fn test_trailing_comment_after_block_comment() {
        // A real trailing comment after a block comment should still be extracted.
        let line = r#"let x = 1 {- block -} + 2 -- real comment"#;
        let result = extract_trailing_comment_from_line(line);
        assert_eq!(
            result.as_deref(),
            Some("-- real comment"),
            "trailing comment after block comment should be extracted"
        );
    }

    #[test]
    fn test_nested_block_comment_with_double_dash() {
        // Nested block comments with `--` inside should not be extracted.
        let line = r#"x {- outer {- inner -- nested -} end -} + y"#;
        let result = extract_trailing_comment_from_line(line);
        assert!(
            result.is_none(),
            "nested block comment containing -- should not be extracted, got: {result:?}"
        );
    }

    // ── F1: comments inside nested blocks must stay inside them ────
    //
    // Each of the five BROKEN repros from the audit gets its own
    // regression test. Every assertion uses `assert_eq!` on the full
    // expected output OR byte-offset ordering — never a bare
    // `.contains()` — so a regression that hoists a nested-block
    // comment to the end of the enclosing fn body will be caught.

    #[test]
    fn test_comment_inside_match_arm_block_stays_nested() {
        // F1 repro 1: comment inside a match arm's block body.
        let source = "fn main() {\n  let x = 5\n  match x {\n    1 -> {\n      -- comment in match arm\n      println(\"one\")\n    }\n    _ -> println(\"other\")\n  }\n}\n";
        let result = format(source).unwrap();
        // The comment must appear BEFORE the closing `}` of the match
        // arm, which itself is before the closing `}` of main.
        let comment_pos = result
            .find("-- comment in match arm")
            .expect("comment should survive formatting");
        let println_one_pos = result
            .find("println(\"one\")")
            .expect("arm body should survive formatting");
        let println_other_pos = result
            .find("println(\"other\")")
            .expect("second arm should survive formatting");
        assert!(
            comment_pos < println_one_pos,
            "comment must precede its sibling statement, got: {result}"
        );
        assert!(
            println_one_pos < println_other_pos,
            "first arm must precede second arm, got: {result}"
        );
        // The closing `}` of main is the very last `}` in the output.
        let main_close = result.rfind('}').unwrap();
        assert!(
            comment_pos < main_close,
            "comment must not be hoisted past the closing brace of main, got: {result}"
        );
        // Verify idempotency so the fixed behavior doesn't regress on
        // a second formatting pass.
        let twice = format(&result).unwrap();
        assert_eq!(result, twice, "formatter must be idempotent, got:\n{twice}");
    }

    #[test]
    fn test_comment_inside_loop_body_stays_nested() {
        // F1 repro 2: comment inside a loop body.
        let source = "fn main() {\n  loop i = 0 {\n    -- loop body comment\n    match i < 3 {\n      true -> loop(i + 1)\n      false -> ()\n    }\n  }\n}\n";
        let result = format(source).unwrap();
        let comment_pos = result
            .find("-- loop body comment")
            .expect("comment should survive formatting");
        let match_pos = result
            .find("match ")
            .expect("match should survive formatting");
        assert!(
            comment_pos < match_pos,
            "comment must precede its sibling match statement inside the loop body, got: {result}"
        );
        // The comment must not appear after the very last `}` of main.
        let last_brace = result.rfind('}').unwrap();
        assert!(
            comment_pos < last_brace,
            "comment must stay inside the loop body, got: {result}"
        );
    }

    #[test]
    fn test_comment_inside_lambda_body_stays_nested() {
        // F1 repro 3: comment inside a lambda body.
        let source = "fn main() {\n  let f = fn(x) {\n    -- inside lambda\n    x + 1\n  }\n  println(f(3))\n}\n";
        let result = format(source).unwrap();
        let comment_pos = result
            .find("-- inside lambda")
            .expect("comment should survive formatting");
        let body_expr_pos = result
            .find("x + 1")
            .expect("lambda body should survive formatting");
        let println_pos = result
            .find("println(f(3))")
            .expect("outer statement should survive formatting");
        assert!(
            comment_pos < body_expr_pos,
            "comment must precede `x + 1` inside the lambda, got: {result}"
        );
        assert!(
            body_expr_pos < println_pos,
            "lambda body must come before the println call, got: {result}"
        );
    }

    #[test]
    fn test_comment_inside_bare_block_expression_stays_nested() {
        // F1 repro 4: comment inside a bare block expression RHS of a let.
        let source = "fn foo() {\n  let x = {\n    -- inside block expr\n    42\n  }\n  x\n}\n";
        let result = format(source).unwrap();
        let comment_pos = result
            .find("-- inside block expr")
            .expect("comment should survive formatting");
        let value_pos = result
            .find("\n    42\n")
            .or_else(|| result.find("42"))
            .expect("block value should survive formatting");
        assert!(
            comment_pos < value_pos,
            "comment must precede the block's value, got: {result}"
        );
        // The `x` on its own line should come after the block.
        let x_pos = result
            .rfind("\n  x\n")
            .expect("trailing `x` statement should survive formatting");
        assert!(
            value_pos < x_pos,
            "block value must come before the trailing `x`, got: {result}"
        );
    }

    #[test]
    fn test_comment_inside_match_as_rhs_stays_nested() {
        // F1 repro 5: comment inside a match arm block when the
        // match itself is the RHS of a `let`.
        let source = "fn main() {\n  let x = match true {\n    true -> {\n      -- nested in assignment\n      42\n    }\n    false -> 0\n  }\n  println(x)\n}\n";
        let result = format(source).unwrap();
        let comment_pos = result
            .find("-- nested in assignment")
            .expect("comment should survive formatting");
        let value_pos = result
            .find("42")
            .expect("arm body value should survive formatting");
        let println_pos = result
            .find("println(x)")
            .expect("outer println should survive formatting");
        assert!(
            comment_pos < value_pos,
            "comment must precede `42` inside the nested match arm body, got: {result}"
        );
        assert!(
            value_pos < println_pos,
            "arm body must come before the println call, got: {result}"
        );
    }

    // ── F2: trailing comments on nested constructs are preserved ────

    #[test]
    fn test_trailing_comment_on_enum_variant() {
        // F2 repro 1: trailing comment on an enum variant.
        let source = "type Shape {\n  Circle(Float)  -- a round one\n  Square(Int)\n}\n";
        let result = format(source).unwrap();
        let comment_pos = result
            .find("-- a round one")
            .expect("enum variant trailing comment should be preserved");
        let circle_pos = result
            .find("Circle(Float)")
            .expect("Circle variant should survive");
        let square_pos = result
            .find("Square(Int)")
            .expect("Square variant should survive");
        assert!(
            circle_pos < comment_pos,
            "trailing comment must follow `Circle(Float)`, got: {result}"
        );
        assert!(
            comment_pos < square_pos,
            "trailing comment must precede the next variant `Square(Int)`, got: {result}"
        );
    }

    #[test]
    fn test_trailing_comment_on_record_fields() {
        // F2 repro 2: trailing comments on record fields.
        let source = "type Point {\n  x: Int,  -- horizontal\n  y: Int,  -- vertical\n}\n";
        let result = format(source).unwrap();
        let horiz_pos = result
            .find("-- horizontal")
            .expect("first record field trailing comment should be preserved");
        let vert_pos = result
            .find("-- vertical")
            .expect("second record field trailing comment should be preserved");
        let x_pos = result.find("x: Int").expect("x field should survive");
        let y_pos = result.find("y: Int").expect("y field should survive");
        assert!(x_pos < horiz_pos, "`-- horizontal` must follow `x: Int`, got: {result}");
        assert!(horiz_pos < y_pos, "`-- horizontal` must precede `y: Int`, got: {result}");
        assert!(y_pos < vert_pos, "`-- vertical` must follow `y: Int`, got: {result}");
    }

    #[test]
    fn test_trailing_comment_on_match_arm() {
        // F2 repro 3: trailing comment on a match arm.
        let source = "fn main() {\n  match 1 {\n    1 -> 1 -- trailing in arm\n    _ -> 0\n  }\n}\n";
        let result = format(source).unwrap();
        let comment_pos = result
            .find("-- trailing in arm")
            .expect("match arm trailing comment should be preserved");
        let arm_1_pos = result
            .find("1 -> 1")
            .expect("first arm should survive");
        let arm_wildcard_pos = result
            .find("_ -> 0")
            .expect("second arm should survive");
        assert!(
            arm_1_pos < comment_pos,
            "trailing comment must follow `1 -> 1`, got: {result}"
        );
        assert!(
            comment_pos < arm_wildcard_pos,
            "trailing comment must precede the next arm `_ -> 0`, got: {result}"
        );
    }

    #[test]
    fn test_trailing_comment_on_pipe_step() {
        // F2 repro 4: trailing comment on a pipe step.
        let source = "import list\nfn main() {\n  [1, 2, 3]\n    |> list.map { x -> x * 2 }  -- double each\n    |> list.sum()\n}\n";
        let result = format(source).unwrap();
        let comment_pos = result
            .find("-- double each")
            .expect("pipe step trailing comment should be preserved");
        let map_pos = result
            .find("list.map")
            .expect("list.map step should survive");
        let sum_pos = result
            .find("list.sum")
            .expect("list.sum step should survive");
        assert!(
            map_pos < comment_pos,
            "trailing comment must follow `list.map`, got: {result}"
        );
        assert!(
            comment_pos < sum_pos,
            "trailing comment must precede the next pipe stage `list.sum`, got: {result}"
        );
    }

    // ── F3: trait and trait-impl separator comments are preserved ───

    #[test]
    fn test_trait_abstract_method_preceding_comment() {
        // F3 repro 1: comment before an abstract trait method must
        // stay attached to the method (not absorbed elsewhere).
        let source = "trait Show {\n  -- render this value\n  fn show(self) -> String\n  fn debug(self) -> String  -- low-level\n}\n";
        let result = format(source).unwrap();
        let render_pos = result
            .find("-- render this value")
            .expect("leading comment on abstract method should be preserved");
        let show_pos = result.find("fn show").expect("show signature should survive");
        let low_level_pos = result
            .find("-- low-level")
            .expect("trailing comment on abstract method should be preserved");
        let debug_pos = result.find("fn debug").expect("debug signature should survive");
        assert!(
            render_pos < show_pos,
            "`-- render this value` must precede `fn show`, got: {result}"
        );
        assert!(
            show_pos < debug_pos,
            "`fn show` must precede `fn debug`, got: {result}"
        );
        assert!(
            debug_pos < low_level_pos,
            "`-- low-level` must follow `fn debug`, got: {result}"
        );
    }

    #[test]
    fn test_trait_separator_comment_between_block_body_methods() {
        // F3 repro 2: separator comment between two block-body methods
        // must stay between the methods, not get absorbed into the
        // previous method's body above its closing brace.
        let source = "trait Show {\n  fn one(self) -> Int { 1 }\n  -- separator comment\n  fn two(self) -> Int { 2 }\n}\n";
        let result = format(source).unwrap();
        let separator_pos = result
            .find("-- separator comment")
            .expect("separator comment should be preserved");
        let one_pos = result.find("fn one").expect("fn one should survive");
        let two_pos = result.find("fn two").expect("fn two should survive");
        // The separator must come between the two methods, not
        // inside either body.
        assert!(
            one_pos < separator_pos,
            "separator comment must follow `fn one`, got: {result}"
        );
        assert!(
            separator_pos < two_pos,
            "separator comment must precede `fn two`, got: {result}"
        );
        // Additionally, the separator must NOT appear between
        // `fn one(self) -> Int {` and its `1 }` — that would mean the
        // comment was absorbed into one's body.
        //
        // The fn one body contains `1`. The separator should come AFTER
        // the byte offset of that `1` (plus the closing `}` of one).
        let body_1_pos = result
            .find("1 }")
            .or_else(|| result.find("{ 1 }"))
            .or_else(|| result.find("1\n  }"))
            .expect("fn one body should survive");
        assert!(
            body_1_pos < separator_pos,
            "separator comment was absorbed into fn one's body, got: {result}"
        );
    }

    // ── R2: lock B4 brace-counting behavior against string literals,
    //        line comments, and string interpolation. Every branch of
    //        the `resolve_decl_end_lines` scanner must be exercised.

    #[test]
    fn test_format_brace_counting_skips_string_literals() {
        // A `}` inside a plain string literal must not terminate the
        // brace-counting scan early in `resolve_decl_end_lines`. To
        // prove the string-tracking logic is required, we place a
        // body comment AFTER the line that contains the fake `}`;
        // a naive scan would compute a too-short `decl_end_line`,
        // which would re-classify the comment as a top-level comment
        // and emit it outside the fn body.
        let source = "fn main() {\n  let s = \"text with } closing brace\"\n  -- body comment after string\n  println(s)\n}\n";
        let result = format(source).unwrap();
        let s_pos = result.find("let s =").expect("let statement should survive");
        let comment_pos = result
            .find("-- body comment after string")
            .expect("body comment should survive formatting");
        let println_pos = result
            .find("println(s)")
            .expect("println call should survive");
        assert!(
            s_pos < comment_pos,
            "comment must follow `let s =`, got: {result}"
        );
        assert!(
            comment_pos < println_pos,
            "comment must precede `println(s)`, got: {result}"
        );
        // And the comment must be INSIDE the function body, not after
        // the closing brace of main.
        let final_brace = result.rfind('}').unwrap();
        assert!(
            comment_pos < final_brace,
            "body comment must stay inside main's body (not hoisted past `}}`), got: {result}"
        );
        let twice = format(&result).unwrap();
        assert_eq!(result, twice, "formatter must be idempotent, got:\n{twice}");
    }

    #[test]
    fn test_format_brace_counting_skips_line_comments() {
        // A `}` inside a `--` line comment must not terminate the
        // brace-counting scan early. A body comment is placed AFTER
        // the line containing the commented-out `}` so any regression
        // that lets `}` inside a line comment decrement depth will
        // misclassify the after-comment.
        let source = "fn main() {\n  let x = 1\n  -- stray } brace in comment\n  -- after-comment\n  let y = 2\n  x + y\n}\n";
        let result = format(source).unwrap();
        let x_pos = result.find("let x = 1").expect("let x should survive");
        let stray_pos = result
            .find("-- stray } brace in comment")
            .expect("first comment should survive");
        let after_pos = result
            .find("-- after-comment")
            .expect("second comment should survive");
        let y_pos = result.find("let y = 2").expect("let y should survive");
        let sum_pos = result.find("x + y").expect("x + y should survive");
        assert!(
            x_pos < stray_pos,
            "stray comment must follow `let x = 1`, got: {result}"
        );
        assert!(
            stray_pos < after_pos,
            "after-comment must follow stray comment, got: {result}"
        );
        assert!(
            after_pos < y_pos,
            "after-comment must precede `let y = 2`, got: {result}"
        );
        assert!(y_pos < sum_pos, "`let y = 2` must precede `x + y`, got: {result}");
        let final_brace = result.rfind('}').unwrap();
        assert!(
            after_pos < final_brace,
            "after-comment must stay inside main's body, got: {result}"
        );
    }

    #[test]
    fn test_format_brace_counting_tracks_interpolation() {
        // `{` and `}` inside a string interpolation like
        // `"hello {name}, count={1 + 2}"` must not unbalance the
        // brace counter in `resolve_decl_end_lines`. A body comment
        // is placed after the interpolated string so any regression
        // that mis-tracks the interpolation will hoist the comment
        // outside the fn body.
        let source = "fn main() {\n  let name = \"world\"\n  let msg = \"hello {name}, count={1 + 2}\"\n  -- body comment after interpolation\n  println(msg)\n}\n";
        let result = format(source).unwrap();
        let name_pos = result.find("let name").expect("let name should survive");
        let msg_pos = result.find("let msg").expect("let msg should survive");
        let comment_pos = result
            .find("-- body comment after interpolation")
            .expect("body comment should survive");
        let println_pos = result.find("println(msg)").expect("println should survive");
        assert!(
            name_pos < msg_pos,
            "let name must precede let msg, got: {result}"
        );
        assert!(
            msg_pos < comment_pos,
            "comment must follow `let msg`, got: {result}"
        );
        assert!(
            comment_pos < println_pos,
            "comment must precede println, got: {result}"
        );
        let final_brace = result.rfind('}').unwrap();
        assert!(
            comment_pos < final_brace,
            "body comment must stay inside main's body, got: {result}"
        );
        let twice = format(&result).unwrap();
        assert_eq!(result, twice, "formatter must be idempotent, got:\n{twice}");
    }

    // ── LATENT R4: strengthen the two weak `.contains()` tests ──────

    #[test]
    fn test_format_string_interpolation_exact_output() {
        // Replacement for the previous `.contains("{name}")` assertion.
        let source = r#"fn greet(name) = "hello {name}"
"#;
        let result = format(source).unwrap();
        assert_eq!(result, "fn greet(name) = \"hello {name}\"\n");
    }

    #[test]
    fn test_format_question_mark_exact_output() {
        // Replacement for the previous `.contains("x?")` assertion.
        let source = "fn try_it(x) = x?\n";
        let result = format(source).unwrap();
        assert_eq!(result, "fn try_it(x) = x?\n");
    }
}
