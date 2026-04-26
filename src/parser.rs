use crate::ast::*;
use crate::intern::{self, Symbol};
use crate::lexer::{Span, SpannedToken, Token};
use std::fmt;

// ── Error type ───────────────────────────────────────────────────────

#[derive(Debug)]
pub struct ParseError {
    pub message: String,
    pub span: Span,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.span, self.message)
    }
}

type Result<T> = std::result::Result<T, ParseError>;

// ── Doc-comment scanner ──────────────────────────────────────────────
//
// The lexer drops every comment. We want doc comments to attach to the
// following top-level declaration (and to trait / impl methods) so hover,
// completion, and signature-help can surface Markdown documentation.
//
// Approach: scan the raw source once (independent of the lexer) and
// produce a per-source-line map `line -> doc_text`. For every line L
// that starts a decl (after lexer-delivered newline handling, we just
// use the Span::line of the decl's first token as the "decl start
// line"), we look up the doc block whose last-comment-line is `L - 1`
// with no blank line between the comment block and the decl.
//
// A doc comment is one or more contiguous comments — `--` lines and/or
// `{- ... -}` blocks — with no blank line between them or between the
// last of them and the decl. The collected segments are concatenated
// with `\n`, then leading whitespace common to all lines is stripped
// (dedent the markdown).

/// Per-line doc comment index: for each source line L that ends a doc
/// comment block, records the concatenated, dedented Markdown text.
///
/// Also tracks which source lines are "blank" (whitespace only) and
/// which lines are part of a comment so the parser can verify that the
/// decl on line L+1 is IMMEDIATELY adjacent to the doc block.
#[derive(Debug, Default, Clone)]
pub struct DocIndex {
    /// `docs_by_end_line[line]` = doc block ending at that line, if any.
    /// The block ends on the line whose `--`/`-}` closes the block. The
    /// decl that consumes this doc must begin on line `end_line + 1`.
    docs_by_end_line: std::collections::HashMap<usize, String>,
}

impl DocIndex {
    /// Build a doc-comment index from raw source text.
    ///
    /// The scan is string-aware (skips `"..."`, `"""..."""`, and
    /// interpolation braces) but otherwise independent of the lexer.
    pub fn from_source(source: &str) -> Self {
        let bytes = source.as_bytes();
        let n = bytes.len();

        // First pass: classify each byte as Code / InString / InBlockComment
        // so we correctly identify which `--` sequences are comments and
        // which are inside strings. We record comment spans as Segment
        // entries (see module-level `Segment` type below).
        let mut segments: Vec<Segment> = Vec::new();
        let mut line: usize = 1; // 1-based
        let mut i: usize = 0;

        // Mode stack for string/interp awareness. Simplified from
        // formatter.rs: we only need to know "am I inside any string
        // context" — if so, `--` is content, not a comment.
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum Mode {
            Code,
            InRegular,        // inside a "..." string (escape-aware)
            InTriple,         // inside a """...""" string
        }
        let mut stack: Vec<Mode> = vec![Mode::Code];
        // For `{...}` interp inside a regular string: when we open a
        // brace inside a string, we push Mode::Code so the inner
        // expression is parsed like normal code. Mirrors the lexer.
        let mut interp_depth_at_open: Vec<usize> = Vec::new();
        let mut brace_depth: usize = 0;

        while i < n {
            let c = bytes[i];
            let top = *stack.last().unwrap();

            if c == b'\n' {
                line += 1;
                i += 1;
                continue;
            }

            match top {
                Mode::Code => {
                    // Detect triple-quoted string first (three quotes).
                    if c == b'"' && i + 2 < n && bytes[i + 1] == b'"' && bytes[i + 2] == b'"' {
                        stack.push(Mode::InTriple);
                        i += 3;
                        continue;
                    }
                    if c == b'"' {
                        stack.push(Mode::InRegular);
                        i += 1;
                        continue;
                    }
                    // Line comment: -- ...
                    if c == b'-' && i + 1 < n && bytes[i + 1] == b'-' {
                        // Collect until end of line (or EOF).
                        let start = i + 2; // skip the `--`
                        let mut end = start;
                        while end < n && bytes[end] != b'\n' {
                            end += 1;
                        }
                        let raw = &source[start..end];
                        // Strip one leading space if present, to produce
                        // clean markdown. Further dedent happens later
                        // when joining segments.
                        let content = if let Some(stripped) = raw.strip_prefix(' ') {
                            stripped.to_string()
                        } else {
                            raw.to_string()
                        };
                        segments.push(Segment::LineComment {
                            line,
                            content,
                        });
                        i = end;
                        continue;
                    }
                    // Block comment: {- ... -} (nested)
                    if c == b'{' && i + 1 < n && bytes[i + 1] == b'-' {
                        let start_line = line;
                        i += 2;
                        let mut depth = 1;
                        let content_start = i;
                        while i < n && depth > 0 {
                            if i + 1 < n && bytes[i] == b'{' && bytes[i + 1] == b'-' {
                                depth += 1;
                                i += 2;
                            } else if i + 1 < n && bytes[i] == b'-' && bytes[i + 1] == b'}' {
                                depth -= 1;
                                if depth == 0 {
                                    // Content is [content_start .. i)
                                    let raw = &source[content_start..i];
                                    let end_line = line;
                                    i += 2; // consume -}
                                    let content_lines: Vec<String> = raw
                                        .split('\n')
                                        .map(|s| s.to_string())
                                        .collect();
                                    segments.push(Segment::BlockComment {
                                        start_line,
                                        end_line,
                                        content_lines,
                                    });
                                    break;
                                }
                                i += 2;
                            } else {
                                if bytes[i] == b'\n' {
                                    line += 1;
                                }
                                i += 1;
                            }
                        }
                        continue;
                    }
                    // Brace tracking for interp resumption
                    if c == b'{' {
                        brace_depth += 1;
                    } else if c == b'}' {
                        if let Some(&resume_at) = interp_depth_at_open.last()
                            && brace_depth == resume_at + 1
                        {
                            // Closing an interp `{`: return to the
                            // enclosing string.
                            interp_depth_at_open.pop();
                            brace_depth -= 1;
                            // The parent is InRegular (interp lives
                            // only inside regular strings). We pushed
                            // Code when we opened the interp — reverse
                            // it now (see open `{` side below).
                            stack.pop();
                            i += 1;
                            continue;
                        }
                        brace_depth = brace_depth.saturating_sub(1);
                    }
                    i += 1;
                }
                Mode::InRegular => {
                    if c == b'\\' && i + 1 < n {
                        // Escape — skip the next char (could be `{`, `"`, etc.)
                        i += 2;
                        continue;
                    }
                    if c == b'"' {
                        stack.pop();
                        i += 1;
                        continue;
                    }
                    if c == b'{' {
                        // Open interp: switch to Code mode (push frame).
                        interp_depth_at_open.push(brace_depth);
                        brace_depth += 1;
                        stack.push(Mode::Code);
                        i += 1;
                        continue;
                    }
                    i += 1;
                }
                Mode::InTriple => {
                    if c == b'"' && i + 2 < n && bytes[i + 1] == b'"' && bytes[i + 2] == b'"' {
                        stack.pop();
                        i += 3;
                        continue;
                    }
                    i += 1;
                }
            }
        }

        // Line strings for comment-only detection.
        let line_strs: Vec<&str> = source.split('\n').collect();

        // Also track: for a given line L, does L contain any NON-comment
        // code? If yes, comments on L cannot be a standalone doc block's
        // tail (e.g. `fn f() -- trailing` is not a doc comment for the
        // next decl — it's a trailing comment on `fn f()`). A comment
        // is only eligible to be part of a doc block if it's the ONLY
        // non-whitespace content on its line.
        //
        // Build a set of lines that are "comment-only lines".
        let mut comment_only_line: std::collections::HashSet<usize> =
            std::collections::HashSet::new();
        for seg in &segments {
            match seg {
                Segment::LineComment { line, .. } => {
                    let l = *line;
                    // Find the raw source line and verify everything
                    // before `--` is whitespace.
                    if l == 0 || l > line_strs.len() {
                        continue;
                    }
                    let src_line = line_strs[l - 1];
                    // Locate the `--` position (the first one OUTSIDE
                    // a string; but since this segment was produced by
                    // the main scanner we know this `--` is real. The
                    // test is: is everything before the first `--` pure
                    // whitespace? If yes, this line is comment-only.
                    // We approximate with: position of `--` in the line
                    // — the first one is safe because a doc line by
                    // convention has no code before it.
                    if let Some(idx) = src_line.find("--") {
                        let prefix = &src_line[..idx];
                        if prefix.bytes().all(|b| b == b' ' || b == b'\t' || b == b'\r') {
                            comment_only_line.insert(l);
                        }
                    }
                }
                Segment::BlockComment {
                    start_line,
                    end_line,
                    ..
                } => {
                    // Block comments are eligible if both the start
                    // line's prefix (before `{-`) and the end line's
                    // suffix (after `-}`) are whitespace-only. The
                    // interior lines are automatically eligible.
                    if *start_line == 0 || *start_line > line_strs.len() {
                        continue;
                    }
                    let start_src = line_strs[start_line - 1];
                    let start_ok = start_src
                        .find("{-")
                        .map(|idx| {
                            start_src[..idx]
                                .bytes()
                                .all(|b| b == b' ' || b == b'\t' || b == b'\r')
                        })
                        .unwrap_or(false);
                    let end_src = if *end_line <= line_strs.len() {
                        line_strs[*end_line - 1]
                    } else {
                        ""
                    };
                    let end_ok = end_src
                        .rfind("-}")
                        .map(|idx| {
                            end_src[idx + 2..]
                                .bytes()
                                .all(|b| b == b' ' || b == b'\t' || b == b'\r')
                        })
                        .unwrap_or(false);
                    if start_ok && end_ok {
                        for l in *start_line..=*end_line {
                            comment_only_line.insert(l);
                        }
                    }
                }
            }
        }

        // Now build doc blocks. Walk segments in source order; every
        // maximal run of comment-only-line segments with NO blank line
        // between adjacent segments forms a block. The block's end_line
        // is the last segment's last line.
        let mut docs_by_end_line: std::collections::HashMap<usize, String> =
            std::collections::HashMap::new();

        let mut i_seg = 0;
        while i_seg < segments.len() {
            // Start a run only if this segment is comment-only.
            let (first_line, _) = segment_line_range(&segments[i_seg]);
            if !comment_only_line.contains(&first_line) {
                i_seg += 1;
                continue;
            }

            // Collect contiguous segments.
            let mut run_end = i_seg;
            while run_end + 1 < segments.len() {
                let (_, prev_end) = segment_line_range(&segments[run_end]);
                let (next_start, _) = segment_line_range(&segments[run_end + 1]);
                // Must be comment-only on start line.
                if !comment_only_line.contains(&next_start) {
                    break;
                }
                // No blank line between them. next_start must be
                // prev_end + 1 (consecutive) or prev_end (same line,
                // only possible for two block comments on same line —
                // unusual but harmless).
                if next_start > prev_end + 1 {
                    break;
                }
                // Also verify all lines strictly between are NOT blank.
                // (With next_start <= prev_end + 1 there are no such
                // lines, so this is automatically true.)
                run_end += 1;
            }

            // Build the doc text by concatenating segment contents.
            let mut raw_lines: Vec<String> = Vec::new();
            for s in &segments[i_seg..=run_end] {
                match s {
                    Segment::LineComment { content, .. } => {
                        raw_lines.push(content.clone());
                    }
                    Segment::BlockComment { content_lines, .. } => {
                        // Block content: each interior line is a raw
                        // line. We drop a purely-empty leading line and
                        // a purely-empty trailing line (common pattern
                        // with `{-\n ... \n-}`).
                        let mut lines = content_lines.clone();
                        if lines.first().is_some_and(|s| s.trim().is_empty()) {
                            lines.remove(0);
                        }
                        if lines.last().is_some_and(|s| s.trim().is_empty()) {
                            lines.pop();
                        }
                        for l in lines {
                            raw_lines.push(l);
                        }
                    }
                }
            }

            // Dedent: find the minimum leading-whitespace prefix across
            // all non-blank lines and strip that common prefix from
            // each line (blank lines stay blank).
            let min_indent = raw_lines
                .iter()
                .filter(|l| !l.trim().is_empty())
                .map(|l| l.chars().take_while(|c| *c == ' ' || *c == '\t').count())
                .min()
                .unwrap_or(0);
            let dedented: Vec<String> = raw_lines
                .iter()
                .map(|l| {
                    if l.trim().is_empty() {
                        String::new()
                    } else {
                        // Strip min_indent leading whitespace chars.
                        let mut stripped = l.as_str();
                        let mut n = 0;
                        for ch in l.chars() {
                            if n >= min_indent {
                                break;
                            }
                            if ch == ' ' || ch == '\t' {
                                stripped = &stripped[ch.len_utf8()..];
                                n += 1;
                            } else {
                                break;
                            }
                        }
                        stripped.to_string()
                    }
                })
                .collect();

            let text = dedented.join("\n");
            let (_, end_line) = segment_line_range(&segments[run_end]);
            docs_by_end_line.insert(end_line, text);

            i_seg = run_end + 1;
        }

        DocIndex { docs_by_end_line }
    }

    /// Look up the doc comment block that ENDS on the line immediately
    /// before `decl_line`. Returns `None` if there's no such block, or
    /// if the line between is blank (meaning the comment isn't adjacent).
    pub fn doc_for_decl_at_line(&self, decl_line: usize) -> Option<String> {
        if decl_line == 0 {
            return None;
        }
        self.docs_by_end_line.get(&(decl_line - 1)).cloned()
    }
}

fn segment_line_range(seg: &Segment) -> (usize, usize) {
    match seg {
        Segment::LineComment { line, .. } => (*line, *line),
        Segment::BlockComment {
            start_line,
            end_line,
            ..
        } => (*start_line, *end_line),
    }
}

/// Comment segment discovered during the doc scan. Public only to the
/// parser module so `DocIndex::from_source` and `segment_line_range`
/// can share the type.
#[derive(Debug, Clone)]
enum Segment {
    LineComment {
        line: usize,
        content: String,
    },
    BlockComment {
        start_line: usize,
        end_line: usize,
        content_lines: Vec<String>,
    },
}

// ── Parser ───────────────────────────────────────────────────────────

const MAX_DEPTH: usize = 128;

pub struct Parser {
    tokens: Vec<SpannedToken>,
    pos: usize,
    in_match_scrutinee: bool,
    errors: Vec<ParseError>,
    depth: usize,
    /// Depth guard for recovery-stub generation. When recovery fires inside
    /// an already-stubbed declaration (e.g., two back-to-back malformed
    /// `fn` declarations where the second is encountered while still
    /// recovering from the first), we must not recursively emit another
    /// stub and call ourselves again. Incremented on entry to the recovery
    /// path, checked on re-entry.
    in_fn_recovery: bool,
    /// Optional doc-comment index. When `Some`, the parser attaches
    /// preceding doc comments to each top-level decl and each trait /
    /// impl method. When `None`, all `doc` fields are left as `None`
    /// (no source → no docs). The LSP pipeline and the `repl` / `cli`
    /// paths build the index eagerly; the bytecode VM paths that only
    /// see tokens don't bother.
    doc_index: Option<DocIndex>,
    /// Name of the trait whose body the parser is currently inside.
    /// Used by `Self::Item` projection sugar to fill in the implicit
    /// trait-name. `None` outside a trait/impl body. The parser sets
    /// this on entry to `parse_trait_or_impl`'s body and restores it on
    /// exit so nested-but-illegal forms (parser doesn't allow nested
    /// traits, so this is purely defensive) cannot leak between siblings.
    current_trait_name: Option<Symbol>,
}

impl Parser {
    pub fn new(tokens: Vec<SpannedToken>) -> Self {
        Self {
            tokens,
            pos: 0,
            in_match_scrutinee: false,
            errors: Vec::new(),
            depth: 0,
            in_fn_recovery: false,
            doc_index: None,
            current_trait_name: None,
        }
    }

    /// Construct a parser that also carries a doc-comment index built
    /// from the original source. When this variant is used, top-level
    /// decls (and trait / impl methods) will have their `doc` field
    /// populated from adjacent `--` / `{- -}` comments.
    pub fn new_with_source(tokens: Vec<SpannedToken>, source: &str) -> Self {
        Self {
            tokens,
            pos: 0,
            in_match_scrutinee: false,
            errors: Vec::new(),
            depth: 0,
            in_fn_recovery: false,
            doc_index: Some(DocIndex::from_source(source)),
            current_trait_name: None,
        }
    }

    /// Look up a doc comment for a decl whose first-token span is `span`.
    /// Returns `None` when no doc-index is attached or when no adjacent
    /// comment block precedes the decl line.
    fn doc_for_span(&self, span: Span) -> Option<String> {
        self.doc_index
            .as_ref()
            .and_then(|idx| idx.doc_for_decl_at_line(span.line))
    }

    // ── helpers ──────────────────────────────────────────────────────

    fn span(&self) -> Span {
        self.tokens
            .get(self.pos)
            .map(|(_, s)| *s)
            .unwrap_or(Span::new(0, 0))
    }

    fn peek(&self) -> &Token {
        &self.tokens[self.pos].0
    }

    fn at(&self, tok: &Token) -> bool {
        std::mem::discriminant(self.peek()) == std::mem::discriminant(tok)
    }

    fn at_newline(&self) -> bool {
        matches!(self.peek(), Token::Newline)
    }

    fn advance(&mut self) -> SpannedToken {
        let tok = self.tokens[self.pos].clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        tok
    }

    fn skip_nl(&mut self) {
        while self.at_newline() {
            self.pos += 1;
        }
    }

    /// Returns true if there is a newline token right at self.pos
    /// (i.e., between the previous real token and the next real token).
    fn has_newline_before(&self) -> bool {
        matches!(self.tokens.get(self.pos), Some((Token::Newline, _)))
    }

    fn expect(&mut self, expected: &Token) -> Result<SpannedToken> {
        self.skip_nl();
        if self.at(expected) {
            Ok(self.advance())
        } else {
            Err(ParseError {
                message: format!("expected {expected}, found {}", self.peek()),
                span: self.span(),
            })
        }
    }

    fn expect_ident(&mut self) -> Result<(Symbol, Span)> {
        self.skip_nl();
        match self.peek().clone() {
            Token::Ident(name) => {
                let span = self.span();
                self.advance();
                Ok((name, span))
            }
            _ => Err(ParseError {
                message: format!("expected identifier, found {}", self.peek()),
                span: self.span(),
            }),
        }
    }

    // ── Delimiter error helpers ──────────────────────────────────────
    //
    // These produce actionable errors when a bracketed/braced/parenthesized
    // construct isn't closed. Rather than the generic
    //     expected expression, found }
    // they report
    //     expected ']' or ',' to continue list literal starting at line N, found }
    // pointing at the current token. `construct` names the enclosing form
    // (e.g. "list literal") and `closer` is the expected closing delimiter.

    /// Build an "unclosed delimiter" error for a construct that uses commas
    /// to separate elements (list, tuple, map, set, call args, fn params).
    fn delim_unclosed_err(&self, construct: &str, closer: char, opener_span: Span) -> ParseError {
        ParseError {
            message: format!(
                "expected '{closer}' or ',' to continue {construct} starting at line {}, found {}",
                opener_span.line,
                self.peek()
            ),
            span: self.span(),
        }
    }

    /// Build an "unclosed delimiter" error for a construct that does not
    /// use commas internally (block expressions).
    fn delim_unclosed_err_no_comma(
        &self,
        construct: &str,
        closer: char,
        opener_span: Span,
    ) -> ParseError {
        ParseError {
            message: format!(
                "expected '{closer}' to close {construct} starting at line {}, found {}",
                opener_span.line,
                self.peek()
            ),
            span: self.span(),
        }
    }

    /// True if the current token is a closing delimiter that is NOT the
    /// one we expect — i.e., we're almost certainly inside a still-open
    /// enclosing delimited form.
    fn at_foreign_closer(&self, our_closer: &Token) -> bool {
        matches!(self.peek(), Token::RBrace | Token::RBracket | Token::RParen)
            && std::mem::discriminant(self.peek()) != std::mem::discriminant(our_closer)
    }

    /// True if the current token is `fn` and the next non-newline token is
    /// an identifier. That shape is unambiguously a top-level `fn NAME(...)`
    /// declaration — an anonymous-fn expression must be `fn(...)` with
    /// parens immediately after. Used inside delimited-list parsers so an
    /// unclosed `[`, `(`, or `{` whose next line begins a fresh `fn` decl
    /// is blamed on the opener rather than on the innards of a failed
    /// anon-fn shape. (Round-52 deferred item 3.)
    fn at_top_level_fn_start(&self) -> bool {
        if !matches!(self.peek(), Token::Fn) {
            return false;
        }
        // Look ahead past newlines to the next real token.
        let mut i = self.pos + 1;
        while i < self.tokens.len() && matches!(self.tokens[i].0, Token::Newline) {
            i += 1;
        }
        matches!(self.tokens.get(i), Some((Token::Ident(_), _)))
    }

    /// Wrap `parse_expr()` so that if it fails because the next token is
    /// EOF or a foreign closer, the error is upgraded to a contextual
    /// unclosed-delimiter message.
    fn parse_expr_in_delim(
        &mut self,
        construct: &str,
        our_closer: &Token,
        closer_char: char,
        opener_span: Span,
    ) -> Result<Expr> {
        // Pre-check: if we're already at EOF / foreign closer before parsing,
        // produce the contextual error immediately.
        self.skip_nl();
        if self.at(&Token::Eof) || self.at_foreign_closer(our_closer) {
            return Err(self.delim_unclosed_err(construct, closer_char, opener_span));
        }
        // If we see `fn NAME`, that's a top-level fn decl — never a valid
        // anon-fn expression (which must be `fn(...)`). Blame the unclosed
        // opener instead of letting parse_fn_expr consume the `fn` and
        // error on the trailing ident. (Round-52 deferred item 3.)
        if self.at_top_level_fn_start() {
            return Err(self.delim_unclosed_err(construct, closer_char, opener_span));
        }
        self.parse_expr()
    }

    /// After an element in a delimited list has been parsed, either
    /// consume the inter-element comma (and any newlines) or verify
    /// we're at the closing delimiter. Strict: absence of a comma
    /// when we are not at the closer is an error.
    ///
    /// silt intentionally does NOT allow whitespace-only separation
    /// between elements in any list-style construct; every fn param,
    /// list/tuple/map/set element, call argument, record field,
    /// pattern destructuring, import item, type parameter, and trait
    /// argument must be comma-separated. Trailing commas remain
    /// permitted — if `close_tok` is seen immediately after the
    /// comma-advance, the enclosing while-loop exits cleanly.
    fn expect_list_sep(&mut self, construct: &str, closer: char, close_tok: &Token) -> Result<()> {
        self.expect_list_sep_with_opener(construct, closer, close_tok, None)
    }

    /// Variant of `expect_list_sep` that carries the opener span so
    /// unclosed-delimiter errors can point back at the construct's
    /// opening bracket. Prefer this variant at call sites that
    /// already tracked the opener.
    fn expect_list_sep_with_opener(
        &mut self,
        construct: &str,
        closer: char,
        close_tok: &Token,
        opener: Option<Span>,
    ) -> Result<()> {
        self.skip_nl();
        if self.at(close_tok) {
            return Ok(());
        }
        if self.at(&Token::Comma) {
            self.advance();
            self.skip_nl();
            return Ok(());
        }
        // If the caller gave us an opener, emit the richer
        // "starting at line N" form so the error points back at the
        // opening delimiter — particularly valuable when the missing
        // comma is really a missing closer.
        if let Some(op) = opener {
            return Err(self.delim_unclosed_err(construct, closer, op));
        }
        Err(ParseError {
            message: format!(
                "expected '{closer}' or ',' to continue {construct}, found {}",
                self.peek()
            ),
            span: self.span(),
        })
    }

    fn save(&self) -> usize {
        self.pos
    }

    fn restore(&mut self, pos: usize) {
        self.pos = pos;
    }

    // ── Program ──────────────────────────────────────────────────────

    pub fn parse_program(&mut self) -> Result<Program> {
        let mut decls = Vec::new();
        self.skip_nl();
        while !self.at(&Token::Eof) {
            decls.push(self.parse_decl()?);
            self.skip_nl();
        }
        Ok(Program { decls })
    }

    /// Like `parse_program`, but recovers from errors and continues parsing.
    /// Returns the (possibly partial) program and all collected parse errors.
    ///
    /// When a malformed `fn` declaration is encountered, the parser uses
    /// `parse_fn_decl_recovering` to salvage whatever header prefix (name,
    /// params, return type) was parsed cleanly and emits a recovery-stub
    /// `FnDecl`. Downstream passes (typechecker) treat recovery stubs as
    /// a source of "trusted signature, unchecked body" so that later
    /// references to the stubbed name do not cascade into "undefined
    /// variable" errors (Option B).
    pub fn parse_program_recovering(&mut self) -> (Program, Vec<ParseError>) {
        let mut decls = Vec::new();
        self.skip_nl();
        while !self.at(&Token::Eof) {
            // Special-case `fn` and `pub fn` declarations so we can salvage
            // partial state on failure.
            if self.at(&Token::Fn) {
                match self.parse_fn_decl_recovering() {
                    Ok((decl, None)) => decls.push(Decl::Fn(decl)),
                    Ok((stub, Some(err))) => {
                        self.errors.push(err);
                        decls.push(Decl::Fn(stub));
                        self.synchronize();
                    }
                    Err(e) => {
                        self.errors.push(e);
                        self.synchronize();
                    }
                }
                self.skip_nl();
                continue;
            }
            if self.at(&Token::Pub) {
                // Look ahead: if this is `pub fn`, use the recovery path.
                let saved = self.save();
                let pub_span = self.span();
                let pub_doc = self.doc_for_span(pub_span);
                self.advance();
                self.skip_nl();
                if self.at(&Token::Fn) {
                    match self.parse_fn_decl_recovering() {
                        Ok((mut decl, None)) => {
                            decl.is_pub = true;
                            decl.span = pub_span;
                            if pub_doc.is_some() {
                                decl.doc = pub_doc;
                            }
                            decls.push(Decl::Fn(decl));
                        }
                        Ok((mut stub, Some(err))) => {
                            stub.is_pub = true;
                            stub.span = pub_span;
                            if pub_doc.is_some() {
                                stub.doc = pub_doc;
                            }
                            self.errors.push(err);
                            decls.push(Decl::Fn(stub));
                            self.synchronize();
                        }
                        Err(e) => {
                            self.errors.push(e);
                            self.synchronize();
                        }
                    }
                    self.skip_nl();
                    continue;
                }
                // Not `pub fn`: restore and fall through to normal decl parsing.
                self.restore(saved);
            }

            match self.parse_decl() {
                Ok(decl) => decls.push(decl),
                Err(e) => {
                    self.errors.push(e);
                    self.synchronize();
                }
            }
            self.skip_nl();
        }
        (Program { decls }, std::mem::take(&mut self.errors))
    }

    /// Skip tokens until we find one that could start a new declaration.
    fn synchronize(&mut self) {
        loop {
            match self.peek() {
                Token::Fn
                | Token::Type
                | Token::Trait
                | Token::Pub
                | Token::Import
                | Token::Let
                | Token::Eof => break,
                _ => {
                    self.advance();
                }
            }
        }
    }

    // ── Declarations ─────────────────────────────────────────────────

    fn parse_decl(&mut self) -> Result<Decl> {
        self.skip_nl();
        match self.peek().clone() {
            Token::Pub => {
                let span = self.span();
                // Doc comment is looked up relative to the `pub` line —
                // that IS the decl's start line from the user's POV.
                let pub_doc = self.doc_for_span(span);
                self.advance();
                self.skip_nl();
                match self.peek() {
                    Token::Fn => {
                        let mut f = self.parse_fn_decl()?;
                        f.is_pub = true;
                        f.span = span;
                        // If pub's line carries a doc, that takes
                        // precedence over a doc found adjacent to `fn`
                        // (which won't happen in practice — pub and fn
                        // are on the same line — but be explicit).
                        if pub_doc.is_some() {
                            f.doc = pub_doc;
                        }
                        Ok(Decl::Fn(f))
                    }
                    Token::Type => {
                        let mut t = self.parse_type_decl()?;
                        t.is_pub = true;
                        t.span = span;
                        if pub_doc.is_some() {
                            t.doc = pub_doc;
                        }
                        Ok(Decl::Type(t))
                    }
                    Token::Let => {
                        let decl = self.parse_let_decl()?;
                        match decl {
                            Decl::Let {
                                pattern,
                                ty,
                                value,
                                doc,
                                ..
                            } => Ok(Decl::Let {
                                pattern,
                                ty,
                                value,
                                is_pub: true,
                                span,
                                doc: pub_doc.or(doc),
                            }),
                            _ => unreachable!("parse_let_decl always returns Decl::Let"),
                        }
                    }
                    _ => Err(ParseError {
                        message: "expected fn, type, or let after pub".into(),
                        span: self.span(),
                    }),
                }
            }
            Token::Fn => Ok(Decl::Fn(self.parse_fn_decl()?)),
            Token::Type => Ok(Decl::Type(self.parse_type_decl()?)),
            Token::Trait => self.parse_trait_or_impl(),
            Token::Import => self.parse_import(),
            Token::Let => self.parse_let_decl(),
            _ => Err(ParseError {
                message: format!("expected declaration, found {}", self.peek()),
                span: self.span(),
            }),
        }
    }

    fn parse_fn_decl(&mut self) -> Result<FnDecl> {
        let span = self.span();
        let doc = self.doc_for_span(span);
        self.expect(&Token::Fn)?;
        let (name, _) = self.expect_ident()?;
        let params = self.parse_fn_params()?;

        let return_type = if self.peek_skip_nl() == &Token::Arrow {
            self.advance();
            Some(self.parse_type_expr()?)
        } else {
            None
        };

        let where_clauses = self.parse_where_clauses_opt()?;

        self.skip_nl();
        let (body, is_signature_only) = if self.at(&Token::Eq) {
            // Single-expression form: fn square(x) = x * x
            self.advance();
            self.skip_nl();
            (self.parse_expr()?, false)
        } else if self.at(&Token::LBrace) {
            (self.parse_block()?, false)
        } else {
            // Abstract method — no body (e.g. trait method declarations).
            // The Unit placeholder keeps the AST shape uniform; the
            // is_signature_only flag is the authoritative signal.
            (Expr::new(ExprKind::Unit, span), true)
        };

        Ok(FnDecl {
            name,
            params,
            return_type,
            where_clauses,
            body,
            is_pub: false,
            span,
            is_recovery_stub: false,
            is_signature_only,
            doc,
        })
    }

    /// Recovery-aware fn declaration parser used by `parse_program_recovering`.
    ///
    /// Tries to parse a function declaration; on error, attempts to salvage
    /// whatever header prefix was parsed (name, params, return type) and
    /// synthesizes a recovery-stub `FnDecl` whose body is an empty block.
    ///
    /// Returns:
    ///   * `Ok((fn_decl, None))` — normal parse succeeded.
    ///   * `Ok((stub_fn, Some(err)))` — parse failed after the name was
    ///     seen; `stub_fn.is_recovery_stub == true`. Caller should push the
    ///     error and then `synchronize()`.
    ///   * `Err(err)` — parse failed before a name was parsed, so no stub
    ///     can be synthesized. Caller should push the error and
    ///     synchronize.
    ///
    /// Implements the depth guard: if we're already inside recovery, no
    /// new stubs are emitted for nested failures.
    fn parse_fn_decl_recovering(&mut self) -> Result<(FnDecl, Option<ParseError>)> {
        // Depth guard: if we somehow re-entered during recovery (e.g. the
        // salvage path tried to keep parsing and hit another fn), bail to
        // the non-recovering path so the caller can handle it.
        if self.in_fn_recovery {
            return Ok((self.parse_fn_decl()?, None));
        }

        let span = self.span();
        let doc = self.doc_for_span(span);
        // `fn` keyword is mandatory. If this errors, we have nothing to
        // salvage.
        self.expect(&Token::Fn)?;

        // Name is mandatory. If the user wrote `fn (` with no name,
        // we skip stub creation: no call sites can match an unnamed stub.
        let name = match self.expect_ident() {
            Ok((n, _)) => n,
            Err(e) => return Err(e),
        };

        // From here on: errors can produce a stub.
        self.in_fn_recovery = true;
        let result = self.parse_fn_decl_tail(name, span, doc);
        self.in_fn_recovery = false;

        match result {
            Ok(decl) => Ok((decl, None)),
            Err(boxed) => {
                let (stub, err) = *boxed;
                Ok((stub, Some(err)))
            }
        }
    }

    /// Parse the tail of a function declaration (after `fn name`), with
    /// partial salvage on errors. On success, returns a complete FnDecl.
    /// On failure, returns `(stub_fn_decl, parse_error)` boxed to keep
    /// the `Err` variant small (clippy `result_large_err`).
    fn parse_fn_decl_tail(
        &mut self,
        name: Symbol,
        span: Span,
        doc: Option<String>,
    ) -> std::result::Result<FnDecl, Box<(FnDecl, ParseError)>> {
        // Try to parse params. On failure, emit a stub with empty params.
        let params = match self.parse_fn_params() {
            Ok(p) => p,
            Err(e) => {
                return Err(Box::new((
                    self.make_recovery_stub(name, Vec::new(), None, span, doc.clone()),
                    e,
                )));
            }
        };

        // Try return type annotation.
        let return_type = if self.peek_skip_nl() == &Token::Arrow {
            self.advance();
            match self.parse_type_expr() {
                Ok(t) => Some(t),
                Err(e) => {
                    return Err(Box::new((
                        self.make_recovery_stub(name, params, None, span, doc.clone()),
                        e,
                    )));
                }
            }
        } else {
            None
        };

        // Try where clauses.
        let where_clauses = if self.peek_skip_nl() == &Token::Where {
            self.advance();
            let mut clauses: Vec<WhereClause> = Vec::new();
            let result: Result<()> = (|| {
                loop {
                    self.skip_nl();
                    let (type_param, _) = self.expect_ident()?;
                    self.expect(&Token::Colon)?;
                    let (trait_name, trait_args) = self.parse_trait_ref()?;
                    clauses.push((type_param, trait_name, trait_args));
                    while self.at(&Token::Plus) {
                        self.advance();
                        let (trait_name, trait_args) = self.parse_trait_ref()?;
                        clauses.push((type_param, trait_name, trait_args));
                    }
                    self.skip_nl();
                    if self.at(&Token::Comma) {
                        self.advance();
                    } else {
                        break;
                    }
                }
                Ok(())
            })();
            if let Err(e) = result {
                return Err(Box::new((
                    self.make_recovery_stub(name, params, return_type, span, doc.clone()),
                    e,
                )));
            }
            clauses
        } else {
            Vec::new()
        };

        self.skip_nl();
        // Body. On failure, emit a stub that preserves the header.
        let (body, is_signature_only) = if self.at(&Token::Eq) {
            self.advance();
            self.skip_nl();
            match self.parse_expr() {
                Ok(e) => (e, false),
                Err(err) => {
                    return Err(Box::new((
                        self.make_recovery_stub(name, params, return_type, span, doc.clone()),
                        err,
                    )));
                }
            }
        } else if self.at(&Token::LBrace) {
            match self.parse_block() {
                Ok(b) => (b, false),
                Err(err) => {
                    return Err(Box::new((
                        self.make_recovery_stub(name, params, return_type, span, doc.clone()),
                        err,
                    )));
                }
            }
        } else {
            // Abstract method — no body.
            (Expr::new(ExprKind::Unit, span), true)
        };

        Ok(FnDecl {
            name,
            params,
            return_type,
            where_clauses,
            body,
            is_pub: false,
            span,
            is_recovery_stub: false,
            is_signature_only,
            doc,
        })
    }

    /// Build a recovery-stub `FnDecl` with an empty body. The body is a
    /// block containing no statements; the typechecker treats these as
    /// having `Type::Never`-style semantics (no body errors emitted).
    fn make_recovery_stub(
        &self,
        name: Symbol,
        params: Vec<Param>,
        return_type: Option<TypeExpr>,
        span: Span,
        doc: Option<String>,
    ) -> FnDecl {
        FnDecl {
            name,
            params,
            return_type,
            where_clauses: Vec::new(),
            body: Expr::new(ExprKind::Block(Vec::new()), span),
            is_pub: false,
            span,
            is_recovery_stub: true,
            is_signature_only: false,
            doc,
        }
    }

    fn parse_fn_params(&mut self) -> Result<Vec<Param>> {
        self.expect(&Token::LParen)?;
        let mut params = Vec::new();
        let mut first_type_param_span: Option<Span> = None;
        self.skip_nl();
        while !self.at(&Token::RParen) {
            if self.at(&Token::Type) {
                let type_kw_span = self.span();
                self.advance();
                let pattern = self.parse_simple_param_pattern()?;
                if self.peek_skip_nl() == &Token::Colon {
                    return Err(ParseError {
                        message: "'type' parameter cannot carry a type annotation; write `type a`"
                            .to_string(),
                        span: self.span(),
                    });
                }
                params.push(Param {
                    kind: ParamKind::Type,
                    pattern,
                    ty: None,
                });
                if first_type_param_span.is_none() {
                    first_type_param_span = Some(type_kw_span);
                }
            } else {
                if let Some(type_span) = first_type_param_span {
                    // Point at the MISPLACED `type` keyword rather than
                    // the innocent data param that follows. The reader
                    // then sees the arrow at the thing that needs to
                    // move, not at the thing sitting in a legal place.
                    return Err(ParseError {
                        message:
                            "'type' parameters must come after all data parameters; move the `type` param to the end of the parameter list"
                                .to_string(),
                        span: type_span,
                    });
                }
                let pattern = self.parse_simple_param_pattern()?;
                let ty = if self.peek_skip_nl() == &Token::Colon {
                    self.advance();
                    Some(self.parse_type_expr()?)
                } else {
                    None
                };
                params.push(Param {
                    kind: ParamKind::Data,
                    pattern,
                    ty,
                });
            }
            self.expect_list_sep("function parameter list", ')', &Token::RParen)?;
        }
        self.expect(&Token::RParen)?;
        Ok(params)
    }

    fn parse_simple_param_pattern(&mut self) -> Result<Pattern> {
        self.skip_nl();
        let start = self.span();
        match self.peek().clone() {
            Token::Ident(name) => {
                self.advance();
                Ok(Pattern::new(PatternKind::Ident(name), start))
            }
            _ => Err(ParseError {
                message: format!("expected parameter name, found {}", self.peek()),
                span: self.span(),
            }),
        }
    }

    fn parse_type_decl(&mut self) -> Result<TypeDecl> {
        let span = self.span();
        let doc = self.doc_for_span(span);
        self.expect(&Token::Type)?;
        let (name, _) = self.expect_ident()?;

        // Optional type parameters: type Result(a, e) { ... } or type Pair(a) = (a, a)
        let params = if self.peek_skip_nl() == &Token::LParen {
            self.advance();
            let mut ps = Vec::new();
            self.skip_nl();
            while !self.at(&Token::RParen) {
                let (p, _) = self.expect_ident()?;
                ps.push(p);
                self.expect_list_sep("type parameter list", ')', &Token::RParen)?;
            }
            self.expect(&Token::RParen)?;
            ps
        } else {
            Vec::new()
        };

        // Phase D: distinguish `type Foo = <type>` (alias) from
        // `type Foo { ... }` (record/enum). The lookahead is exact —
        // there is no other top-level use of `=` after a `type Name`
        // header in silt today.
        self.skip_nl();
        if self.at(&Token::Eq) {
            self.advance();
            self.skip_nl();
            let target = self.parse_type_expr()?;
            return Ok(TypeDecl {
                name,
                params,
                body: TypeBody::Alias(target),
                is_pub: false,
                span,
                doc,
            });
        }

        self.expect(&Token::LBrace)?;
        self.skip_nl();

        // Determine if this is an enum or record by peeking at the first field.
        // Record fields look like `name: Type`, enum variants look like `Name` or `Name(Type)`.
        let body = if self.is_record_body() {
            self.parse_record_body()?
        } else {
            self.parse_enum_body()?
        };

        self.skip_nl();
        self.expect(&Token::RBrace)?;

        Ok(TypeDecl {
            name,
            params,
            body,
            is_pub: false,
            span,
            doc,
        })
    }

    fn is_record_body(&self) -> bool {
        // Look ahead: if we see `ident :` it's a record. If we see `Ident(` or `Ident,` or `Ident }` it's enum.
        // Record field names start lowercase, enum variant names start uppercase.
        let mut i = self.pos;
        // skip newlines
        while i < self.tokens.len() && matches!(self.tokens[i].0, Token::Newline) {
            i += 1;
        }
        if let Token::Ident(ref name) = self.tokens[i].0 {
            // lowercase first char → likely record field
            intern::resolve(*name).starts_with(|c: char| c.is_lowercase())
        } else {
            false
        }
    }

    fn parse_record_body(&mut self) -> Result<TypeBody> {
        let mut fields = Vec::new();
        while !self.at(&Token::RBrace) {
            self.skip_nl();
            if self.at(&Token::RBrace) {
                break;
            }
            let (name, _) = self.expect_ident()?;
            self.expect(&Token::Colon)?;
            let ty = self.parse_type_expr()?;
            fields.push(RecordField { name, ty });
            self.expect_list_sep("record type fields", '}', &Token::RBrace)?;
        }
        Ok(TypeBody::Record(fields))
    }

    fn parse_enum_body(&mut self) -> Result<TypeBody> {
        let mut variants = Vec::new();
        while !self.at(&Token::RBrace) {
            self.skip_nl();
            if self.at(&Token::RBrace) {
                break;
            }
            let (name, _) = self.expect_ident()?;
            let fields = if self.peek() == &Token::LParen {
                self.advance();
                let mut fs = Vec::new();
                self.skip_nl();
                while !self.at(&Token::RParen) {
                    fs.push(self.parse_type_expr()?);
                    self.expect_list_sep("enum variant fields", ')', &Token::RParen)?;
                }
                self.expect(&Token::RParen)?;
                fs
            } else {
                Vec::new()
            };
            variants.push(EnumVariant { name, fields });
            self.expect_list_sep("enum variant list", '}', &Token::RBrace)?;
        }
        Ok(TypeBody::Enum(variants))
    }

    /// Parse an optional `where` clause list. Consumes the `where` token
    /// if present. Supports comma-separated clauses and `+`-separated
    /// multi-trait bounds per clause (`where a: Equal + Hash, b: Show`).
    /// Returns an empty Vec if no `where` token is present.
    ///
    /// Shared between `parse_fn_decl`'s non-recovering path and the new
    /// trait-impl parser. The recovery-variant fn-decl parser at
    /// `parse_fn_decl_tail` keeps its inline copy because each failure
    /// site must emit a recovery stub instead of propagating the error.
    fn parse_where_clauses_opt(&mut self) -> Result<Vec<WhereClause>> {
        if self.peek_skip_nl() != &Token::Where {
            return Ok(Vec::new());
        }
        self.advance(); // consume 'where'
        let mut clauses = Vec::new();
        loop {
            self.skip_nl();
            let (type_param, _) = self.expect_ident()?;
            self.expect(&Token::Colon)?;
            let (trait_name, trait_args) = self.parse_trait_ref()?;
            clauses.push((type_param, trait_name, trait_args));
            // Multi-trait bounds: `where a: Equal + Hash`
            while self.at(&Token::Plus) {
                self.advance();
                let (trait_name, trait_args) = self.parse_trait_ref()?;
                clauses.push((type_param, trait_name, trait_args));
            }
            self.skip_nl();
            if self.at(&Token::Comma) {
                self.advance();
            } else {
                break;
            }
        }
        Ok(clauses)
    }

    /// Parse a trait reference inside a where clause: either a bare
    /// identifier (`Display`) or a parameterized form
    /// (`TryInto(Int)`, `Convert(a, b)`).
    fn parse_trait_ref(&mut self) -> Result<(Symbol, Vec<TypeExpr>)> {
        let (name, _) = self.expect_ident()?;
        let args = if self.at(&Token::LParen) {
            self.advance();
            let mut args = Vec::new();
            self.skip_nl();
            while !self.at(&Token::RParen) {
                args.push(self.parse_type_expr()?);
                self.expect_list_sep("trait argument list", ')', &Token::RParen)?;
            }
            self.expect(&Token::RParen)?;
            args
        } else {
            Vec::new()
        };
        Ok((name, args))
    }

    fn parse_trait_or_impl(&mut self) -> Result<Decl> {
        let span = self.span();
        let doc = self.doc_for_span(span);
        self.expect(&Token::Trait)?;
        let (name, _) = self.expect_ident()?;

        // Parse optional trait-level parameters: `trait Foo(a, b) { ... }`
        // on the declaration side, or `trait Foo(Int, String) for T { ... }`
        // on the impl side. Both forms use the same `Name(...)` syntax;
        // the downstream `for`/`fn`/`{` dispatch tells us which.
        //
        // For the declaration form, args must be lowercase idents (fresh
        // type-var binders). For the impl form, args can be any type
        // expression (concrete types, generics, or tyvars from a
        // parameterized impl target).
        let trait_args: Vec<TypeExpr> = if self.at(&Token::LParen) {
            self.advance();
            let mut args = Vec::new();
            self.skip_nl();
            while !self.at(&Token::RParen) {
                args.push(self.parse_type_expr()?);
                self.expect_list_sep("trait argument list", ')', &Token::RParen)?;
            }
            self.expect(&Token::RParen)?;
            args
        } else {
            Vec::new()
        };

        // Parse optional supertrait bounds: `trait Ordered: Equal + Hash { ... }`
        // or parameterized forms `trait Sub(a): Super(a) + Other(Int)`.
        // Disambiguation: `:` after the trait name is unambiguous because impls
        // use `for` and decls use `{` or `fn`.
        let supertraits: Vec<(Symbol, Vec<TypeExpr>)> = if self.at(&Token::Colon) {
            self.advance();
            let mut traits = Vec::new();
            let (t, args) = self.parse_trait_ref()?;
            traits.push((t, args));
            while self.at(&Token::Plus) {
                self.advance();
                let (t, args) = self.parse_trait_ref()?;
                traits.push((t, args));
            }
            traits
        } else {
            Vec::new()
        };

        self.skip_nl();
        // `trait Display for User { ... }` is an impl
        // `trait Display { ... }` is a declaration. The disambiguation
        // accepts `where` too, for the form
        // `trait Foo(a) where a: Bound { ... }`.
        if self.at(&Token::Fn) || self.at(&Token::LBrace) || self.at(&Token::Where) {
            // Trait declaration. For the decl form, `trait_args` holds the
            // trait's type parameters; they must be lowercase idents.
            let mut params: Vec<Symbol> = Vec::new();
            for arg in &trait_args {
                // Round-52 deferred item 2: point the caret at the
                // offending argument's own span, not the enclosing
                // `trait` keyword. Each arg carries its own span now.
                let arg_span = arg.span;
                let TypeExprKind::Named(arg_sym) = &arg.kind else {
                    return Err(ParseError {
                        message: "trait declaration parameters must be lowercase type variables \
                             (e.g. `trait TryInto(b) { ... }`)"
                            .to_string(),
                        span: arg_span,
                    });
                };
                let arg_str = intern::resolve(*arg_sym);
                let first_char = arg_str.chars().next().unwrap_or('A');
                if !first_char.is_lowercase() {
                    return Err(ParseError {
                        message: format!(
                            "trait parameter '{arg_str}' must be a lowercase type variable"
                        ),
                        span: arg_span,
                    });
                }
                if params.contains(arg_sym) {
                    return Err(ParseError {
                        message: format!(
                            "duplicate type variable '{arg_str}' in trait declaration"
                        ),
                        span: arg_span,
                    });
                }
                params.push(*arg_sym);
            }
            // Trait-level where bounds on trait params:
            // `trait Foo(a) where a: Display { ... }`. Each impl must
            // supply a concrete type arg satisfying the bounds.
            let param_where_clauses = self.parse_where_clauses_opt()?;
            self.skip_nl();
            if self.at(&Token::LBrace) {
                self.advance();
            }
            // Trait body: methods (`fn ...`) interleaved with
            // associated-type declarations (`type Item` or
            // `type Item: Compare + Hash`). Both are accepted in any
            // order; the parser collects them into separate vecs so
            // downstream consumers don't have to re-classify.
            let prev_trait = self.current_trait_name.replace(name);
            let mut methods = Vec::new();
            let mut assoc_types: Vec<crate::ast::AssocTypeDecl> = Vec::new();
            self.skip_nl();
            while !self.at(&Token::RBrace) {
                if self.at(&Token::Type) {
                    let assoc = self.parse_assoc_type_decl()?;
                    assoc_types.push(assoc);
                    self.skip_nl();
                    continue;
                }
                methods.push(self.parse_fn_decl()?);
                self.skip_nl();
            }
            self.current_trait_name = prev_trait;
            self.expect(&Token::RBrace)?;
            Ok(Decl::Trait(TraitDecl {
                name,
                params,
                supertraits,
                param_where_clauses,
                methods,
                assoc_types,
                span,
                doc,
            }))
        } else {
            // Must be `for Type { ... }`  or  `for Type(params...) { ... }`.
            // Supertrait bounds (`trait X: A for Int { ... }`) are not allowed
            // on impls — supertraits live on the trait decl only.
            if !supertraits.is_empty() {
                return Err(ParseError {
                    message: "supertrait bounds (`: Trait`) are only allowed on trait \
                              declarations, not on trait impls"
                        .to_string(),
                    span,
                });
            }
            self.expect(&Token::Ident(intern::intern("for")))?;
            let target_span = self.span();
            let target_te = self.parse_type_expr()?;

            // Accept only `Named(head)` or `Generic(head, args)` as the
            // impl target. Reject tuple/fn/Unit targets — those have no
            // stable "head symbol" for method_table keying or for the
            // compiler's `TypeName.method_name` qualified-name form.
            let (target, target_type_args) = match target_te.kind {
                TypeExprKind::Named(sym) => (sym, Vec::new()),
                TypeExprKind::Generic(sym, args) => (sym, args),
                _ => {
                    return Err(ParseError {
                        message: "trait impl target must be a named type (e.g. `Box` or `Box(a)`)"
                            .to_string(),
                        span: target_span,
                    });
                }
            };

            // Extract lowercase type-var binders from the target args.
            // Enforce two rules:
            //   1. Every arg must be a lowercase `Named` ident (impl
            //      target arguments must be type variables — silt has no
            //      specialization, so `trait X for Box(Int)` is rejected).
            //   2. Binders must be distinct (no `Pair(a, a)` shadowing).
            let mut target_param_names: Vec<Symbol> = Vec::new();
            for arg in &target_type_args {
                let TypeExprKind::Named(arg_sym) = &arg.kind else {
                    return Err(ParseError {
                        message: "impl target arguments must be lowercase type variables; \
                                  silt has no trait specialization"
                            .to_string(),
                        span: target_span,
                    });
                };
                let arg_str = intern::resolve(*arg_sym);
                let first_char = arg_str.chars().next().unwrap_or('A');
                if !first_char.is_lowercase() {
                    return Err(ParseError {
                        message: format!(
                            "impl target argument '{arg_str}' must be a lowercase type variable; \
                             silt has no trait specialization"
                        ),
                        span: target_span,
                    });
                }
                if target_param_names.contains(arg_sym) {
                    return Err(ParseError {
                        message: format!(
                            "duplicate type variable '{arg_str}' in impl target; \
                             each binder must be distinct"
                        ),
                        span: target_span,
                    });
                }
                target_param_names.push(*arg_sym);
            }

            self.skip_nl();
            // Optional impl-level where clauses:
            //   trait Greet for Box(a) where a: Greet { ... }
            // Constraints here apply to every method in the impl body
            // (appended to each method's scheme during register_trait_impl).
            // Multi-trait bounds via `+` are supported: `where a: Show + Hash`.
            let where_clauses = self.parse_where_clauses_opt()?;
            self.skip_nl();
            self.expect(&Token::LBrace)?;
            // Impl body: methods (`fn ...`) interleaved with
            // associated-type bindings (`type Item = Int`). Each
            // binding must be a complete RHS — defaults / abstract
            // assoc types are illegal in impls.
            let prev_trait = self.current_trait_name.replace(name);
            let mut methods = Vec::new();
            let mut assoc_type_bindings: Vec<crate::ast::AssocTypeBinding> = Vec::new();
            self.skip_nl();
            while !self.at(&Token::RBrace) {
                if self.at(&Token::Type) {
                    let binding = self.parse_assoc_type_binding()?;
                    assoc_type_bindings.push(binding);
                    self.skip_nl();
                    continue;
                }
                methods.push(self.parse_fn_decl()?);
                self.skip_nl();
            }
            self.current_trait_name = prev_trait;
            self.expect(&Token::RBrace)?;
            Ok(Decl::TraitImpl(TraitImpl {
                trait_name: name,
                trait_args,
                target_type: target,
                target_type_args,
                target_param_names,
                where_clauses,
                methods,
                assoc_type_bindings,
                span,
                is_auto_derived: false,
            }))
        }
    }

    fn parse_import(&mut self) -> Result<Decl> {
        let (_, import_span) = self.expect(&Token::Import)?;
        let (name, _) = self.expect_ident()?;

        self.skip_nl();
        if self.at(&Token::Dot) {
            self.advance();
            self.expect(&Token::LBrace)?;
            let mut items = Vec::new();
            self.skip_nl();
            while !self.at(&Token::RBrace) {
                let (item, _) = self.expect_ident()?;
                items.push(item);
                self.expect_list_sep("selective import list", '}', &Token::RBrace)?;
            }
            self.expect(&Token::RBrace)?;
            Ok(Decl::Import(ImportTarget::Items(name, items), import_span))
        } else if self.at(&Token::As) {
            self.advance();
            let (alias, _) = self.expect_ident()?;
            Ok(Decl::Import(ImportTarget::Alias(name, alias), import_span))
        } else {
            Ok(Decl::Import(ImportTarget::Module(name), import_span))
        }
    }

    // ── Associated-type decl / binding ────────────────────────────────

    /// Parse an associated-type declaration inside a trait body.
    /// Accepted forms:
    ///   `type Item`
    ///   `type Item: Compare`
    ///   `type Item: Compare + Hash`
    /// Defaults (`type Item = Default`) are rejected; v1 reserves the
    /// syntax for a future extension.
    fn parse_assoc_type_decl(&mut self) -> Result<crate::ast::AssocTypeDecl> {
        let span = self.span();
        self.expect(&Token::Type)?;
        let (name, _) = self.expect_ident()?;
        let mut bounds: Vec<(Symbol, Vec<TypeExpr>)> = Vec::new();
        if self.at(&Token::Colon) {
            self.advance();
            let (tn, args) = self.parse_trait_ref()?;
            bounds.push((tn, args));
            while self.at(&Token::Plus) {
                self.advance();
                let (tn, args) = self.parse_trait_ref()?;
                bounds.push((tn, args));
            }
        }
        if self.at(&Token::Eq) {
            return Err(ParseError {
                message: "associated-type defaults are not supported in v1; \
                          declare the type abstractly (`type Item`) and bind it \
                          in each impl"
                    .to_string(),
                span,
            });
        }
        Ok(crate::ast::AssocTypeDecl { name, bounds, span })
    }

    /// Parse an associated-type binding inside a trait impl body.
    /// Form: `type Item = TypeExpr`.
    fn parse_assoc_type_binding(&mut self) -> Result<crate::ast::AssocTypeBinding> {
        let span = self.span();
        self.expect(&Token::Type)?;
        let (name, _) = self.expect_ident()?;
        self.expect(&Token::Eq)?;
        let ty = self.parse_type_expr()?;
        Ok(crate::ast::AssocTypeBinding { name, ty, span })
    }

    // ── Type expressions ─────────────────────────────────────────────

    fn parse_type_expr(&mut self) -> Result<TypeExpr> {
        self.skip_nl();
        // Round-52 deferred item 2: capture the start-of-type-expr span
        // so every TypeExpr node anchors its own span. Used by trait-
        // header diagnostics (parse_trait_or_impl) to point at the
        // offending argument rather than the outer `trait` keyword.
        let start = self.span();
        // Qualified projection: `<TypeExpr as TraitName>::IDENT`. The
        // `<` here is unambiguous because every other use of `<` is
        // an operator at expression position; in type-expr position
        // the only legal opener is the qualified form.
        if self.at(&Token::Lt) {
            self.advance();
            self.skip_nl();
            let receiver = self.parse_type_expr()?;
            self.skip_nl();
            self.expect(&Token::As)?;
            let (trait_name, _) = self.expect_ident()?;
            self.skip_nl();
            self.expect(&Token::Gt)?;
            self.expect(&Token::ColonColon)?;
            let (assoc_name, _) = self.expect_ident()?;
            return Ok(TypeExpr::new(
                TypeExprKind::AssocProj {
                    receiver: Box::new(receiver),
                    trait_name,
                    assoc_name,
                },
                start,
            ));
        }
        // Function type: Fn(A, B) -> C
        if matches!(self.peek(), Token::Ident(s) if *s == intern::intern("Fn")) {
            self.advance();
            self.expect(&Token::LParen)?;
            let mut params = Vec::new();
            self.skip_nl();
            while !self.at(&Token::RParen) {
                params.push(self.parse_type_expr()?);
                self.expect_list_sep("function type parameter list", ')', &Token::RParen)?;
            }
            self.expect(&Token::RParen)?;
            self.expect(&Token::Arrow)?;
            let ret = self.parse_type_expr()?;
            return Ok(TypeExpr::new(
                TypeExprKind::Function(params, Box::new(ret)),
                start,
            ));
        }
        // Tuple type: (A, B, ...)
        if self.at(&Token::LParen) {
            self.advance();
            let mut elems = Vec::new();
            self.skip_nl();
            while !self.at(&Token::RParen) {
                elems.push(self.parse_type_expr()?);
                self.expect_list_sep("tuple type", ')', &Token::RParen)?;
            }
            self.expect(&Token::RParen)?;
            return Ok(TypeExpr::new(TypeExprKind::Tuple(elems), start));
        }
        // Anonymous record type: `{name: Type, age: Type}` or open
        // `{name: Type, ...r}`.
        if self.at(&Token::LBrace) {
            self.advance();
            self.skip_nl();
            let mut fields: Vec<(Symbol, TypeExpr)> = Vec::new();
            let mut tail: Option<Symbol> = None;
            let mut seen: std::collections::HashSet<Symbol> = std::collections::HashSet::new();
            while !self.at(&Token::RBrace) {
                self.skip_nl();
                if self.at(&Token::DotDotDot) {
                    self.advance();
                    // Row variable name (e.g. `r` in `...r`). Required.
                    let (rname, _) = self.expect_ident()?;
                    tail = Some(rname);
                    self.skip_nl();
                    break;
                }
                let (fname, _) = self.expect_ident()?;
                self.expect(&Token::Colon)?;
                self.skip_nl();
                let fty = self.parse_type_expr()?;
                if !seen.insert(fname) {
                    return Err(ParseError {
                        message: format!("duplicate field '{}' in anon record type", fname),
                        span: self.span(),
                    });
                }
                fields.push((fname, fty));
                self.expect_list_sep("anon record type fields", '}', &Token::RBrace)?;
            }
            self.expect(&Token::RBrace)?;
            return Ok(TypeExpr::new(
                TypeExprKind::AnonRecord { fields, tail },
                start,
            ));
        }
        let (name, _) = self.expect_ident()?;
        // `Self::Item` — sugar for `<Self as <enclosing_trait>>::Item`.
        // The trait name is supplied by the parser's enclosing-trait
        // context (set by parse_trait_or_impl on entry to the body).
        // Outside a trait/impl body the projection is an error; it is
        // caught by the typechecker (current_trait_name absent).
        if name == intern::intern("Self") {
            if self.at(&Token::ColonColon) {
                self.advance();
                let (assoc_name, _) = self.expect_ident()?;
                let trait_name = self.current_trait_name.unwrap_or_else(|| {
                    // Outside a trait body — leave a sentinel so the
                    // typechecker can produce a clear diagnostic. The
                    // parser doesn't have enough context to error here
                    // (the token stream looks like a perfectly normal
                    // type expression). Use an interned placeholder.
                    intern::intern("__no_enclosing_trait__")
                });
                let recv = TypeExpr::new(TypeExprKind::SelfType, start);
                return Ok(TypeExpr::new(
                    TypeExprKind::AssocProj {
                        receiver: Box::new(recv),
                        trait_name,
                        assoc_name,
                    },
                    start,
                ));
            }
            return Ok(TypeExpr::new(TypeExprKind::SelfType, start));
        }
        if self.peek() == &Token::LParen {
            self.advance();
            let mut args = Vec::new();
            self.skip_nl();
            while !self.at(&Token::RParen) {
                args.push(self.parse_type_expr()?);
                self.expect_list_sep("generic type argument list", ')', &Token::RParen)?;
            }
            self.expect(&Token::RParen)?;
            Ok(TypeExpr::new(TypeExprKind::Generic(name, args), start))
        } else {
            Ok(TypeExpr::new(TypeExprKind::Named(name), start))
        }
    }

    // ── Block & statements ───────────────────────────────────────────

    fn parse_block(&mut self) -> Result<Expr> {
        let span = self.span();
        let opener = self.span();
        self.expect(&Token::LBrace)?;
        let stmts = self.parse_stmt_list(&Token::RBrace)?;
        self.skip_nl();
        if self.at(&Token::Eof) {
            return Err(self.delim_unclosed_err_no_comma("block", '}', opener));
        }
        self.expect(&Token::RBrace)?;
        Ok(Expr::new(ExprKind::Block(stmts), span))
    }

    fn parse_stmt_list(&mut self, terminator: &Token) -> Result<Vec<Stmt>> {
        let mut stmts = Vec::new();
        self.skip_nl();
        while !self.at(terminator) && !self.at(&Token::Eof) {
            stmts.push(self.parse_stmt()?);
            self.skip_nl();
        }
        Ok(stmts)
    }

    fn parse_stmt(&mut self) -> Result<Stmt> {
        self.skip_nl();

        // Emit targeted hints for keywords silt doesn't have (`if`, `while`,
        // `for`, `break`, `continue`) and for mutable reassignment (`x = ...`).
        // Only fires at statement-start positions to avoid hijacking legitimate
        // identifiers later in an expression. We also guard with a "looks like
        // the mistake we expect" lookahead so that e.g. `if(x)` as a function
        // call still works.
        if let Token::Ident(name) = self.peek().clone() {
            let text = intern::resolve(name).to_string();
            let next = self
                .tokens
                .get(self.pos + 1)
                .map(|t| t.0.clone())
                .unwrap_or(Token::Eof);
            let span = self.span();

            // G1: if / while / for
            //
            // `break` and `continue` used to be guarded here too, but the
            // check fired on ANY bare identifier reference (next is
            // Newline | RBrace | Eof), which made the formatter's
            // paren-stripping non-roundtrip: `(break)` → `break` would
            // re-parse as an error. Bare `break`/`continue` are valid
            // identifier references syntactically; if they're not
            // bound, the typechecker already produces an "undefined
            // variable" diagnostic. So we drop them from the G1 hint
            // and rely on the name-resolution error instead.
            let hint = match text.as_str() {
                "if" => Some(
                    "silt has no 'if' keyword — use 'match cond { true -> ..., false -> ... }'",
                ),
                "while" | "for" => Some(
                    "silt has no 'while'/'for' keywords — use tail-recursive 'loop' or 'list.each' / 'list.map'",
                ),
                _ => None,
            };
            if let Some(msg) = hint {
                // Fire only when the next token could plausibly start the
                // erroneous construct: an expression-start token (paren,
                // ident, literal, unary, brace).
                let looks_like_mistake = match text.as_str() {
                    "if" | "while" | "for" => matches!(
                        next,
                        Token::Ident(_)
                            | Token::Int(_)
                            | Token::Float(_)
                            | Token::Bool(_)
                            | Token::StringLit(..)
                            | Token::StringStart(_)
                            | Token::Minus
                            | Token::Not
                            | Token::LBrace
                            | Token::LBracket
                    ),
                    _ => false,
                };
                if looks_like_mistake {
                    return Err(ParseError {
                        message: msg.into(),
                        span,
                    });
                }
            }

            // G2: reassignment (`x = ...`) where `x` was previously `let`-bound.
            // We can't see the binding from here, but the pattern `ident = ...`
            // at a statement-start position is almost always a user expecting
            // mutation. Matching on `Ident` followed by `Eq` is precise enough
            // that it doesn't collide with any legitimate construct: a bare
            // `x = y` expression is already a parse error today ("expected
            // expression, found ="), so we're strictly improving the message.
            if matches!(next, Token::Eq) {
                return Err(ParseError {
                    message: format!(
                        "'let' bindings in silt are immutable — rebind with 'let {text} = ...' in a new scope"
                    ),
                    span,
                });
            }
        }

        match self.peek().clone() {
            Token::Let => self.parse_let_stmt(),
            Token::When => self.parse_when_stmt(),
            _ => {
                let expr = self.parse_expr()?;
                Ok(Stmt::Expr(expr))
            }
        }
    }

    fn parse_let_stmt(&mut self) -> Result<Stmt> {
        self.expect(&Token::Let)?;
        let pattern = self.parse_pattern()?;
        let ty = if self.peek_skip_nl() == &Token::Colon {
            self.advance();
            Some(self.parse_type_expr()?)
        } else {
            None
        };
        self.expect(&Token::Eq)?;
        self.skip_nl();
        let value = self.parse_expr()?;
        Ok(Stmt::Let { pattern, ty, value })
    }

    fn parse_let_decl(&mut self) -> Result<Decl> {
        let span = self.span();
        let doc = self.doc_for_span(span);
        self.expect(&Token::Let)?;
        let pattern = self.parse_pattern()?;
        let ty = if self.peek_skip_nl() == &Token::Colon {
            self.advance();
            Some(self.parse_type_expr()?)
        } else {
            None
        };
        self.expect(&Token::Eq)?;
        self.skip_nl();
        let value = self.parse_expr()?;
        Ok(Decl::Let {
            pattern,
            ty,
            value,
            is_pub: false,
            span,
            doc,
        })
    }

    fn parse_when_stmt(&mut self) -> Result<Stmt> {
        self.expect(&Token::When)?;

        // Pattern form: when let <pattern> = <expr> else { <block> }
        // The `let` keyword is an unambiguous lookahead — it cannot begin
        // a valid expression, so no backtracking is needed.
        if self.at(&Token::Let) {
            self.advance(); // consume `let`
            let pattern = self.parse_pattern()?;
            self.expect(&Token::Eq)?;
            self.skip_nl();
            // Use min_bp=11 to prevent `else` from being consumed as the
            // infix FloatElse operator (which has l_bp=10).
            let expr = self.parse_expr_bp(11)?;
            self.expect(&Token::Else)?;
            let else_body = self.parse_block()?;
            return Ok(Stmt::When {
                pattern,
                expr,
                else_body,
            });
        }

        // Boolean form: when <expr> else { <block> }
        // Use min_bp=11 to prevent `else` from being consumed as the
        // infix FloatElse operator (which has l_bp=10).
        let condition = self.parse_expr_bp(11)?;
        self.expect(&Token::Else)?;
        let else_body = self.parse_block()?;
        Ok(Stmt::WhenBool {
            condition,
            else_body,
        })
    }

    // ── Expressions (Pratt parser) ───────────────────────────────────

    pub fn parse_expr(&mut self) -> Result<Expr> {
        self.skip_nl();
        self.parse_expr_bp(0)
    }

    fn parse_expr_bp(&mut self, min_bp: u8) -> Result<Expr> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            self.depth -= 1;
            return Err(ParseError {
                message: "expression nesting exceeds maximum depth".into(),
                span: self.span(),
            });
        }
        let result = self.parse_expr_bp_inner(min_bp);
        self.depth -= 1;
        result
    }

    fn parse_expr_bp_inner(&mut self, min_bp: u8) -> Result<Expr> {
        let mut left = self.parse_unary()?;

        loop {
            // First, try postfix operators — newline-sensitive.
            // If a newline precedes the token, don't treat it as postfix.
            if !self.has_newline_before() {
                match self.peek() {
                    Token::Question => {
                        // `?` binds looser than `|>` (pipe l_bp = 55) so
                        // a pipeline followed by `?` parses as
                        // `(x |> f(y))?` — i.e. `?` applies to the full
                        // piped result, not just to the call-inside-the-pipe.
                        // Historically `?` was at 110 (higher than every
                        // infix op), which made `x |> f(y)?` desugar to
                        // `x |> (f(y)?)` and fail type-check because the
                        // `?` was attached to a half-applied fn value.
                        //
                        // Side effect: expressions like `x + y?` now parse
                        // as `(x + y)?` instead of the old `x + (y?)`.
                        // That change is safe in practice — silt's `?`
                        // requires its LHS to be `Result` or `Option`, so
                        // `(x + y)?` with non-Result operands is a type
                        // error either way; users who wanted the old shape
                        // always needed parens on the RHS anyway.
                        let bp = 54;
                        if bp < min_bp {
                            break;
                        }
                        let span = left.span;
                        self.advance();
                        left = Expr::new(ExprKind::QuestionMark(Box::new(left)), span);
                        continue;
                    }
                    Token::LParen => {
                        let bp = 120;
                        if bp < min_bp {
                            break;
                        }
                        left = self.parse_call_expr(left)?;
                        continue;
                    }
                    Token::LBracket => {
                        let bp = 120;
                        if bp < min_bp {
                            break;
                        }
                        left = self.parse_index_expr(left)?;
                        continue;
                    }
                    Token::LBrace if self.is_trailing_closure() => {
                        let bp = 115; // lower than call (120) so match scrutinee can suppress it
                        if bp < min_bp {
                            break;
                        }
                        let closure = self.parse_trailing_closure()?;
                        // Append the closure to the call or wrap ident in a call
                        left = self.attach_trailing_closure(left, closure);
                        continue;
                    }
                    _ => {}
                }
            }

            // Save position, skip newlines, try infix operators.
            let saved = self.save();
            let had_newline = self.has_newline_before();
            self.skip_nl();

            match self.peek() {
                // Field access / record update (always allowed across newlines)
                Token::Dot => {
                    let bp = 130;
                    if bp < min_bp {
                        self.restore(saved);
                        break;
                    }
                    self.advance();
                    self.skip_nl();
                    if self.at(&Token::LBrace) {
                        // Record update: expr.{ field: value }
                        let span = left.span;
                        self.advance(); // {
                        let fields = self.parse_record_fields()?;
                        self.expect(&Token::RBrace)?;
                        left = Expr::new(
                            ExprKind::RecordUpdate {
                                expr: Box::new(left),
                                fields,
                            },
                            span,
                        );
                    } else if let Token::Int(n) = self.peek() {
                        // Tuple index access: expr.0, expr.1, etc.
                        let field = intern::intern(&n.to_string());
                        self.advance();
                        let span = left.span;
                        left = Expr::new(ExprKind::FieldAccess(Box::new(left), field), span);
                    } else {
                        let (field, _) = self.expect_ident()?;
                        let span = left.span;
                        left = Expr::new(ExprKind::FieldAccess(Box::new(left), field), span);
                    }
                    continue;
                }

                // Pipe operator — binds tighter than comparison/boolean operators
                // so `x |> f() == y` parses as `(x |> f()) == y`,
                // but looser than range so `1..10 |> f()` parses as `(1..10) |> f()`
                Token::Pipe => {
                    let (l_bp, r_bp) = (55, 56);
                    if l_bp < min_bp {
                        self.restore(saved);
                        break;
                    }
                    self.advance();
                    self.skip_nl();
                    // Allow trailing closures in the pipe RHS even inside a
                    // match scrutinee.  The match-body `{` appears *after*
                    // the entire pipe expression, not inside the RHS, so it
                    // is safe to re-enable trailing closures here.  Example:
                    //   match items |> list.any { x -> x > 5 } { true -> … }
                    //                           ^^^^^^^^^^^^^^^  <- trailing closure
                    //                                           ^^^^^^^^^^^^^^^^ <- match body
                    let prev_match = self.in_match_scrutinee;
                    self.in_match_scrutinee = false;
                    let right = self.parse_expr_bp(r_bp)?;
                    self.in_match_scrutinee = prev_match;
                    let span = left.span;
                    left = Expr::new(ExprKind::Pipe(Box::new(left), Box::new(right)), span);
                    continue;
                }

                // Range — binds tighter than pipe so `1..10 |> f()` works
                Token::DotDot => {
                    let (l_bp, r_bp) = (60, 61);
                    if l_bp < min_bp {
                        self.restore(saved);
                        break;
                    }
                    self.advance();
                    self.skip_nl();
                    let right = self.parse_expr_bp(r_bp)?;
                    let span = left.span;
                    left = Expr::new(ExprKind::Range(Box::new(left), Box::new(right)), span);
                    continue;
                }

                // Binary operators
                Token::OrOr => {
                    let (l_bp, r_bp) = (20, 21);
                    if l_bp < min_bp {
                        self.restore(saved);
                        break;
                    }
                    self.advance();
                    self.skip_nl();
                    let right = self.parse_expr_bp(r_bp)?;
                    let span = left.span;
                    left = Expr::new(
                        ExprKind::Binary(Box::new(left), BinOp::Or, Box::new(right)),
                        span,
                    );
                    continue;
                }
                Token::AndAnd => {
                    let (l_bp, r_bp) = (30, 31);
                    if l_bp < min_bp {
                        self.restore(saved);
                        break;
                    }
                    self.advance();
                    self.skip_nl();
                    let right = self.parse_expr_bp(r_bp)?;
                    let span = left.span;
                    left = Expr::new(
                        ExprKind::Binary(Box::new(left), BinOp::And, Box::new(right)),
                        span,
                    );
                    continue;
                }
                Token::EqEq | Token::NotEq => {
                    let op = if self.peek() == &Token::EqEq {
                        BinOp::Eq
                    } else {
                        BinOp::Neq
                    };
                    let (l_bp, r_bp) = (40, 41);
                    if l_bp < min_bp {
                        self.restore(saved);
                        break;
                    }
                    self.advance();
                    self.skip_nl();
                    let right = self.parse_expr_bp(r_bp)?;
                    let span = left.span;
                    left = Expr::new(ExprKind::Binary(Box::new(left), op, Box::new(right)), span);
                    continue;
                }
                Token::Lt | Token::Gt | Token::LtEq | Token::GtEq => {
                    let op = match self.peek() {
                        Token::Lt => BinOp::Lt,
                        Token::Gt => BinOp::Gt,
                        Token::LtEq => BinOp::Leq,
                        Token::GtEq => BinOp::Geq,
                        _ => unreachable!(
                            "guarded by Token::Lt | Token::Gt | Token::LtEq | Token::GtEq arm"
                        ),
                    };
                    let (l_bp, r_bp) = (50, 51);
                    if l_bp < min_bp {
                        self.restore(saved);
                        break;
                    }
                    self.advance();
                    self.skip_nl();
                    let right = self.parse_expr_bp(r_bp)?;
                    let span = left.span;
                    left = Expr::new(ExprKind::Binary(Box::new(left), op, Box::new(right)), span);
                    continue;
                }
                Token::Plus | Token::Minus if !had_newline => {
                    // + and - are newline-sensitive: they are ambiguous with
                    // unary +/- at the start of the next statement, so a
                    // newline before them terminates the current expression.
                    let op = if self.peek() == &Token::Plus {
                        BinOp::Add
                    } else {
                        BinOp::Sub
                    };
                    let (l_bp, r_bp) = (70, 71);
                    if l_bp < min_bp {
                        self.restore(saved);
                        break;
                    }
                    self.advance();
                    self.skip_nl();
                    let right = self.parse_expr_bp(r_bp)?;
                    let span = left.span;
                    left = Expr::new(ExprKind::Binary(Box::new(left), op, Box::new(right)), span);
                    continue;
                }
                Token::Star | Token::Slash | Token::Percent => {
                    let op = match self.peek() {
                        Token::Star => BinOp::Mul,
                        Token::Slash => BinOp::Div,
                        Token::Percent => BinOp::Mod,
                        _ => unreachable!(
                            "guarded by Token::Star | Token::Slash | Token::Percent arm"
                        ),
                    };
                    let (l_bp, r_bp) = (80, 81);
                    if l_bp < min_bp {
                        self.restore(saved);
                        break;
                    }
                    self.advance();
                    self.skip_nl();
                    let right = self.parse_expr_bp(r_bp)?;
                    let span = left.span;
                    left = Expr::new(ExprKind::Binary(Box::new(left), op, Box::new(right)), span);
                    continue;
                }

                // Type ascription: expr as Type
                Token::As => {
                    let bp = 95;
                    if bp < min_bp {
                        self.restore(saved);
                        break;
                    }
                    self.advance();
                    self.skip_nl();
                    let type_expr = self.parse_type_expr()?;
                    let span = left.span;
                    left = Expr::new(ExprKind::Ascription(Box::new(left), type_expr), span);
                    continue;
                }

                // Float narrowing: expr else fallback
                Token::Else => {
                    let (l_bp, r_bp) = (10, 11);
                    if l_bp < min_bp {
                        self.restore(saved);
                        break;
                    }
                    self.advance();
                    self.skip_nl();
                    let fallback = self.parse_expr_bp(r_bp)?;
                    let span = left.span;
                    left = Expr::new(
                        ExprKind::FloatElse(Box::new(left), Box::new(fallback)),
                        span,
                    );
                    continue;
                }

                _ => {
                    self.restore(saved);
                    break;
                }
            }
        }

        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr> {
        self.skip_nl();
        match self.peek() {
            Token::Minus => {
                let span = self.span();
                self.advance();
                let expr = self.parse_expr_bp(90)?;
                Ok(Expr::new(
                    ExprKind::Unary(UnaryOp::Neg, Box::new(expr)),
                    span,
                ))
            }
            Token::Not => {
                let span = self.span();
                self.advance();
                let expr = self.parse_expr_bp(90)?;
                Ok(Expr::new(
                    ExprKind::Unary(UnaryOp::Not, Box::new(expr)),
                    span,
                ))
            }
            _ => self.parse_atom(),
        }
    }

    fn parse_atom(&mut self) -> Result<Expr> {
        self.skip_nl();
        let span = self.span();

        match self.peek().clone() {
            Token::Int(n) => {
                self.advance();
                Ok(Expr::new(ExprKind::Int(n), span))
            }
            Token::Float(n) => {
                self.advance();
                Ok(Expr::new(ExprKind::Float(n), span))
            }
            Token::Bool(b) => {
                self.advance();
                Ok(Expr::new(ExprKind::Bool(b), span))
            }
            Token::StringLit(s, triple) => {
                self.advance();
                Ok(Expr::new(ExprKind::StringLit(s, triple), span))
            }
            Token::StringStart(s) => {
                self.advance();
                self.parse_string_interp(s, span)
            }
            Token::Ident(ref name) if is_constructor(*name) => {
                let name = *name;
                self.advance();
                // Could be: Constructor, Constructor(args), or RecordCreate { fields }
                if !self.has_newline_before() && self.at(&Token::LParen) {
                    let args = self.parse_call_args()?;
                    Ok(Expr::new(
                        ExprKind::Call(Box::new(Expr::new(ExprKind::Ident(name), span)), args),
                        span,
                    ))
                } else if !self.has_newline_before()
                    && self.at(&Token::LBrace)
                    && (!self.in_match_scrutinee || self.scrutinee_lbrace_is_record_literal())
                    && !self.is_trailing_closure()
                {
                    // Record creation: User { name: "Alice", ... }
                    self.advance(); // {
                    let fields = self.parse_record_fields()?;
                    self.expect(&Token::RBrace)?;
                    Ok(Expr::new(ExprKind::RecordCreate { name, fields }, span))
                } else {
                    Ok(Expr::new(ExprKind::Ident(name), span))
                }
            }
            Token::Ident(name) => {
                self.advance();
                Ok(Expr::new(ExprKind::Ident(name), span))
            }
            Token::LParen => {
                let opener = self.span();
                self.advance();
                self.skip_nl();
                // Unit: ()
                if self.at(&Token::RParen) {
                    self.advance();
                    return Ok(Expr::new(ExprKind::Unit, span));
                }
                // Parse first expression
                let first = self.parse_expr_in_delim(
                    "parenthesized expression",
                    &Token::RParen,
                    ')',
                    opener,
                )?;
                self.skip_nl();
                if self.at(&Token::Comma) {
                    // Tuple: (a, b, c)
                    self.advance();
                    let mut elems = vec![first];
                    self.skip_nl();
                    while !self.at(&Token::RParen) {
                        if self.at(&Token::Eof) || self.at_foreign_closer(&Token::RParen) {
                            return Err(self.delim_unclosed_err("tuple", ')', opener));
                        }
                        elems.push(self.parse_expr_in_delim(
                            "tuple",
                            &Token::RParen,
                            ')',
                            opener,
                        )?);
                        self.expect_list_sep_with_opener(
                            "tuple literal",
                            ')',
                            &Token::RParen,
                            Some(opener),
                        )?;
                    }
                    self.expect(&Token::RParen)?;
                    Ok(Expr::new(ExprKind::Tuple(elems), span))
                } else if self.at(&Token::Eof) || self.at_foreign_closer(&Token::RParen) {
                    Err(self.delim_unclosed_err_no_comma("parenthesized expression", ')', opener))
                } else {
                    // Parenthesized expression
                    self.expect(&Token::RParen)?;
                    Ok(first)
                }
            }
            Token::LBracket => {
                let opener = self.span();
                self.advance();
                let mut elems = Vec::new();
                self.skip_nl();
                while !self.at(&Token::RBracket) {
                    if self.at(&Token::Eof) || self.at_foreign_closer(&Token::RBracket) {
                        return Err(self.delim_unclosed_err("list literal", ']', opener));
                    }
                    if self.at(&Token::DotDot) {
                        self.advance();
                        elems.push(ListElem::Spread(self.parse_expr_in_delim(
                            "list literal",
                            &Token::RBracket,
                            ']',
                            opener,
                        )?));
                    } else {
                        elems.push(ListElem::Single(self.parse_expr_in_delim(
                            "list literal",
                            &Token::RBracket,
                            ']',
                            opener,
                        )?));
                    }
                    self.expect_list_sep_with_opener(
                        "list literal",
                        ']',
                        &Token::RBracket,
                        Some(opener),
                    )?;
                }
                self.expect(&Token::RBracket)?;
                Ok(Expr::new(ExprKind::List(elems), span))
            }
            Token::HashBrace => {
                let opener = self.span();
                self.advance();
                let mut pairs = Vec::new();
                self.skip_nl();
                while !self.at(&Token::RBrace) {
                    if self.at(&Token::Eof) || self.at_foreign_closer(&Token::RBrace) {
                        return Err(self.delim_unclosed_err("map literal", '}', opener));
                    }
                    let key =
                        self.parse_expr_in_delim("map literal", &Token::RBrace, '}', opener)?;
                    self.expect(&Token::Colon)?;
                    self.skip_nl();
                    let value =
                        self.parse_expr_in_delim("map literal", &Token::RBrace, '}', opener)?;
                    pairs.push((key, value));
                    self.expect_list_sep_with_opener(
                        "map literal",
                        '}',
                        &Token::RBrace,
                        Some(opener),
                    )?;
                }
                self.expect(&Token::RBrace)?;
                Ok(Expr::new(ExprKind::Map(pairs), span))
            }
            Token::HashBracket => {
                let opener = self.span();
                self.advance();
                let mut elems = Vec::new();
                self.skip_nl();
                while !self.at(&Token::RBracket) {
                    if self.at(&Token::Eof) || self.at_foreign_closer(&Token::RBracket) {
                        return Err(self.delim_unclosed_err("set literal", ']', opener));
                    }
                    elems.push(self.parse_expr_in_delim(
                        "set literal",
                        &Token::RBracket,
                        ']',
                        opener,
                    )?);
                    self.expect_list_sep_with_opener(
                        "set literal",
                        ']',
                        &Token::RBracket,
                        Some(opener),
                    )?;
                }
                self.expect(&Token::RBracket)?;
                Ok(Expr::new(ExprKind::SetLit(elems), span))
            }
            Token::LBrace => {
                // Could be a trailing closure, an anonymous record
                // literal, or a block. Disambiguation order:
                //   1. trailing-closure heuristic (existing) — `{ x -> ... }`
                //   2. anon-record literal — `{ Ident: ... }` or `{ ...spread, ... }`
                //   3. block — fallthrough
                if self.is_trailing_closure() {
                    self.parse_trailing_closure_as_lambda()
                } else if self.is_anon_record_literal() {
                    self.parse_anon_record_literal()
                } else {
                    self.parse_block()
                }
            }
            Token::Match => self.parse_match_expr(),
            Token::Loop => self.parse_loop_expr(),
            Token::Fn => self.parse_fn_expr(),
            Token::Return => {
                self.advance();
                // Return may or may not have a value
                if self.has_newline_before() || self.at(&Token::RBrace) || self.at(&Token::Eof) {
                    Ok(Expr::new(ExprKind::Return(None), span))
                } else {
                    let val = self.parse_expr()?;
                    Ok(Expr::new(ExprKind::Return(Some(Box::new(val))), span))
                }
            }
            // select is no longer a keyword; use channel.select([...])
            _ => Err(ParseError {
                message: format!("expected expression, found {}", self.peek()),
                span: self.span(),
            }),
        }
    }

    // ── String interpolation ─────────────────────────────────────────

    fn parse_string_interp(&mut self, start_text: String, span: Span) -> Result<Expr> {
        let mut parts = Vec::new();
        if !start_text.is_empty() {
            parts.push(StringPart::Literal(start_text));
        }

        // Parse expression inside interpolation
        let expr = self.parse_expr()?;
        parts.push(StringPart::Expr(expr));

        // Now we should see StringMiddle or StringEnd
        loop {
            match self.peek().clone() {
                Token::StringMiddle(text) => {
                    self.advance();
                    if !text.is_empty() {
                        parts.push(StringPart::Literal(text));
                    }
                    let expr = self.parse_expr()?;
                    parts.push(StringPart::Expr(expr));
                }
                Token::StringEnd(text) => {
                    self.advance();
                    if !text.is_empty() {
                        parts.push(StringPart::Literal(text));
                    }
                    break;
                }
                _ => {
                    return Err(ParseError {
                        message: "invalid expression in string interpolation; use \\{ for a literal brace".into(),
                        span: self.span(),
                    });
                }
            }
        }

        Ok(Expr::new(ExprKind::StringInterp(parts), span))
    }

    // ── Function calls ───────────────────────────────────────────────

    fn parse_call_expr(&mut self, callee: Expr) -> Result<Expr> {
        let span = callee.span;
        let args = self.parse_call_args()?;
        // Trailing closures are handled by the postfix loop in parse_expr_bp,
        // which respects min_bp and correctly suppresses them in match scrutinees.
        Ok(Expr::new(ExprKind::Call(Box::new(callee), args), span))
    }

    fn parse_call_args(&mut self) -> Result<Vec<Expr>> {
        let opener = self.span();
        self.expect(&Token::LParen)?;
        let mut args = Vec::new();
        self.skip_nl();
        while !self.at(&Token::RParen) {
            if self.at(&Token::Eof) || self.at_foreign_closer(&Token::RParen) {
                return Err(self.delim_unclosed_err("function call argument list", ')', opener));
            }
            args.push(self.parse_expr_in_delim(
                "function call argument list",
                &Token::RParen,
                ')',
                opener,
            )?);
            self.expect_list_sep_with_opener(
                "function call argument list",
                ')',
                &Token::RParen,
                Some(opener),
            )?;
        }
        self.expect(&Token::RParen)?;
        Ok(args)
    }

    fn parse_index_expr(&mut self, left: Expr) -> Result<Expr> {
        // Postfix indexing (`xs[i]`) is reserved syntax but not yet implemented.
        // Reject it with an actionable error message pointing at the typed
        // accessors users should reach for instead.
        let _ = left;
        let bracket_span = self.span();
        Err(ParseError {
            message: "postfix indexing is not supported; use list.get(xs, i), \
                      map.get(m, k), or string.char_at(s, i)"
                .to_string(),
            span: bracket_span,
        })
    }

    // ── Trailing closures ────────────────────────────────────────────

    fn is_trailing_closure(&self) -> bool {
        // When parsing a match scrutinee, the `{` is always the match body,
        // never a trailing closure.
        if self.in_match_scrutinee {
            return false;
        }
        // Check if the current `{` starts a trailing closure by looking for `->`.
        if self.peek() != &Token::LBrace {
            return false;
        }
        let mut i = self.pos + 1; // skip `{`
        // Skip leading newlines to find the first real token
        while i < self.tokens.len() && matches!(self.tokens[i].0, Token::Newline) {
            i += 1;
        }
        // If the first real token is a literal, this is a match body
        // (patterns like `0 ->`, `true ->`), not a trailing closure.
        // Note: `_` is NOT excluded here because it is a valid closure
        // parameter name (meaning "ignore this argument"). Match bodies
        // are consumed directly by parse_match_expr via expect(LBrace),
        // so they never reach this heuristic.
        if i < self.tokens.len() {
            match &self.tokens[i].0 {
                Token::Int(_) | Token::Float(_) | Token::Bool(_) => return false,
                _ => {}
            }
        }
        let mut depth = 0;
        while i < self.tokens.len() {
            match &self.tokens[i].0 {
                Token::Arrow if depth == 0 => return true,
                Token::LParen => depth += 1,
                Token::RParen if depth > 0 => depth -= 1,
                Token::Newline | Token::Ident(_) | Token::Comma => {}
                _ => return false,
            }
            i += 1;
        }
        false
    }

    /// In match-scrutinee position, the `{` after a bare constructor is
    /// normally suppressed so the match-body `{` isn't consumed as part
    /// of the scrutinee expression. But a record literal
    /// `Ctor { field: v, ... }` is syntactically distinct from a match
    /// body `{ pattern -> body }`: the former has `Ident Colon`
    /// immediately after `{`; the latter has `Pattern Arrow`. This
    /// bounded lookahead lets a record literal through inside scrutinee
    /// position without breaking match-body suppression.
    fn scrutinee_lbrace_is_record_literal(&self) -> bool {
        if self.peek() != &Token::LBrace {
            return false;
        }
        let mut i = self.pos + 1;
        while i < self.tokens.len() && matches!(self.tokens[i].0, Token::Newline) {
            i += 1;
        }
        if !matches!(self.tokens.get(i).map(|t| &t.0), Some(Token::Ident(_))) {
            return false;
        }
        i += 1;
        while i < self.tokens.len() && matches!(self.tokens[i].0, Token::Newline) {
            i += 1;
        }
        matches!(self.tokens.get(i).map(|t| &t.0), Some(Token::Colon))
    }

    fn parse_trailing_closure(&mut self) -> Result<Expr> {
        self.parse_trailing_closure_as_lambda()
    }

    fn parse_trailing_closure_as_lambda(&mut self) -> Result<Expr> {
        let span = self.span();
        self.expect(&Token::LBrace)?;
        self.skip_nl();
        let params = self.parse_closure_params()?;
        self.expect(&Token::Arrow)?;
        self.skip_nl();

        // Parse body statements
        let stmts = self.parse_stmt_list(&Token::RBrace)?;
        self.expect(&Token::RBrace)?;

        let body = if stmts.len() == 1 {
            if let Stmt::Expr(e) = &stmts[0] {
                e.clone()
            } else {
                Expr::new(ExprKind::Block(stmts), span)
            }
        } else {
            Expr::new(ExprKind::Block(stmts), span)
        };

        Ok(Expr::new(
            ExprKind::Lambda {
                params,
                body: Box::new(body),
            },
            span,
        ))
    }

    fn parse_closure_params(&mut self) -> Result<Vec<Param>> {
        let mut params = Vec::new();
        loop {
            self.skip_nl();
            match self.peek() {
                Token::Arrow => break,
                Token::LParen => {
                    // Destructuring pattern like (a, b)
                    let pattern = self.parse_pattern()?;
                    params.push(Param {
                        kind: ParamKind::Data,
                        pattern,
                        ty: None,
                    });
                }
                Token::Ident(_) => {
                    let pattern = self.parse_pattern()?;
                    params.push(Param {
                        kind: ParamKind::Data,
                        pattern,
                        ty: None,
                    });
                }
                _ => break,
            }
            // Closure params terminate at `->`, not a closing bracket; we
            // reuse the list-sep helper with the arrow as the "closer".
            self.skip_nl();
            if self.at(&Token::Arrow) {
                break;
            }
            if self.at(&Token::Comma) {
                self.advance();
                self.skip_nl();
            } else {
                return Err(ParseError {
                    message: format!(
                        "expected '->' or ',' to continue closure parameter list, found {}",
                        self.peek()
                    ),
                    span: self.span(),
                });
            }
        }
        Ok(params)
    }

    fn attach_trailing_closure(&mut self, callee: Expr, closure: Expr) -> Expr {
        let span = callee.span;
        match callee.kind {
            ExprKind::Call(f, mut args) => {
                args.push(closure);
                Expr::new(ExprKind::Call(f, args), span)
            }
            _ => {
                // Wrap as a call: `f { x -> body }` → f(closure)
                Expr::new(ExprKind::Call(Box::new(callee), vec![closure]), span)
            }
        }
    }

    // ── Match ────────────────────────────────────────────────────────

    fn parse_match_expr(&mut self) -> Result<Expr> {
        let span = self.span();
        self.expect(&Token::Match)?;
        self.skip_nl();

        // Guardless match: `match { cond -> body, ... }`
        let guardless = self.at(&Token::LBrace);
        let scrutinee = if guardless {
            None
        } else {
            // Set flag so is_trailing_closure returns false while parsing the
            // scrutinee. This allows comparison/equality/boolean operators (which
            // have lower bp than the old threshold of 116) while still preventing
            // the match body `{` from being consumed as a trailing closure.
            // Save and restore the flag to handle nested match expressions.
            let prev = self.in_match_scrutinee;
            self.in_match_scrutinee = true;
            let expr = self.parse_expr()?;
            self.in_match_scrutinee = prev;
            Some(Box::new(expr))
        };

        self.expect(&Token::LBrace)?;
        self.skip_nl();

        let mut arms = Vec::new();
        while !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
            arms.push(self.parse_match_arm(guardless)?);
            // Allow optional comma between match arms
            if self.at(&Token::Comma) {
                self.advance();
            }
            self.skip_nl();
        }
        self.expect(&Token::RBrace)?;

        Ok(Expr::new(
            ExprKind::Match {
                expr: scrutinee,
                arms,
            },
            span,
        ))
    }

    fn parse_match_arm(&mut self, guardless: bool) -> Result<MatchArm> {
        self.skip_nl();

        if guardless {
            // Guardless match: each arm's LHS is a boolean expression or `_`
            let arm_start = self.span();
            let is_wildcard =
                matches!(self.peek(), Token::Ident(name) if *name == intern::intern("_"));
            if is_wildcard {
                self.advance();
                self.expect(&Token::Arrow)?;
                self.skip_nl();
                let body = self.parse_expr()?;
                return Ok(MatchArm {
                    pattern: Pattern::new(PatternKind::Wildcard, arm_start),
                    guard: None,
                    body,
                });
            }
            let condition = self.parse_expr()?;
            self.expect(&Token::Arrow)?;
            self.skip_nl();
            let body = self.parse_expr()?;
            return Ok(MatchArm {
                pattern: Pattern::new(PatternKind::Wildcard, arm_start),
                guard: Some(Box::new(condition)),
                body,
            });
        }

        let pattern = self.parse_pattern()?;

        // Optional guard: `when condition`
        self.skip_nl();
        let guard = if self.at(&Token::When) {
            self.advance();
            self.skip_nl();
            Some(Box::new(self.parse_expr()?))
        } else {
            None
        };

        self.expect(&Token::Arrow)?;
        self.skip_nl();
        let body = self.parse_expr()?;

        Ok(MatchArm {
            pattern,
            guard,
            body,
        })
    }

    // ── Loop expression ──────────────────────────────────────────────

    fn parse_loop_expr(&mut self) -> Result<Expr> {
        let span = self.span();
        self.expect(&Token::Loop)?;

        // Check for recur: `loop(args)` — LParen immediately (no newline)
        if !self.has_newline_before() && self.at(&Token::LParen) {
            let args = self.parse_call_args()?;
            return Ok(Expr::new(ExprKind::Recur(args), span));
        }

        self.skip_nl();

        // Zero-binding variant: `loop { body }`
        if self.at(&Token::LBrace) {
            let body = self.parse_block()?;
            return Ok(Expr::new(
                ExprKind::Loop {
                    bindings: Vec::new(),
                    body: Box::new(body),
                },
                span,
            ));
        }

        // Binding variant: `loop x = init, y = init { body }`
        let mut bindings = Vec::new();
        loop {
            self.skip_nl();
            let (name, _) = self.expect_ident()?;
            self.expect(&Token::Eq)?;
            self.skip_nl();
            let init = self.parse_expr()?;
            bindings.push((name, init));
            self.skip_nl();
            if self.at(&Token::Comma) {
                self.advance();
            } else {
                break;
            }
        }

        self.skip_nl();
        let body = self.parse_block()?;

        Ok(Expr::new(
            ExprKind::Loop {
                bindings,
                body: Box::new(body),
            },
            span,
        ))
    }

    // ── Fn expression ────────────────────────────────────────────────

    fn parse_fn_expr(&mut self) -> Result<Expr> {
        let span = self.span();
        self.expect(&Token::Fn)?;
        let params = self.parse_fn_params()?;
        self.skip_nl();
        let body = self.parse_block()?;
        Ok(Expr::new(
            ExprKind::Lambda {
                params,
                body: Box::new(body),
            },
            span,
        ))
    }

    // ── Record fields ────────────────────────────────────────────────

    /// Lookahead: is the current `{` the start of an anonymous-record
    /// literal? Two shapes match:
    ///   - `{ Ident COLON ... }` — closed anon record literal
    ///   - `{ DotDotDot ... }` — spread head (extend op `{...other, ...}`)
    /// Disambiguation runs after `is_trailing_closure()` returned false,
    /// so the lookahead can be aggressive without needing to defer to
    /// the closure heuristic.
    fn is_anon_record_literal(&self) -> bool {
        if self.peek() != &Token::LBrace {
            return false;
        }
        let mut i = self.pos + 1;
        while i < self.tokens.len() && matches!(self.tokens[i].0, Token::Newline) {
            i += 1;
        }
        // Spread head is unambiguous.
        if matches!(self.tokens.get(i).map(|t| &t.0), Some(Token::DotDotDot)) {
            return true;
        }
        // `Ident COLON` (with possible newlines between) — anon record.
        if !matches!(self.tokens.get(i).map(|t| &t.0), Some(Token::Ident(_))) {
            return false;
        }
        i += 1;
        while i < self.tokens.len() && matches!(self.tokens[i].0, Token::Newline) {
            i += 1;
        }
        matches!(self.tokens.get(i).map(|t| &t.0), Some(Token::Colon))
    }

    /// Parse `{name: expr, ...}` or `{...spread, name: expr, ...}` after
    /// the caller has confirmed via `is_anon_record_literal` that the
    /// current `{` opens an anon-record literal. Consumes the closing
    /// `}` itself.
    fn parse_anon_record_literal(&mut self) -> Result<Expr> {
        let span = self.span();
        self.expect(&Token::LBrace)?;
        self.skip_nl();
        let mut spread: Option<Box<Expr>> = None;
        if self.at(&Token::DotDotDot) {
            self.advance();
            self.skip_nl();
            let e = self.parse_expr()?;
            spread = Some(Box::new(e));
            // After the spread expression, expect a comma if more
            // fields follow, else closing brace.
            self.skip_nl();
            if self.at(&Token::Comma) {
                self.advance();
                self.skip_nl();
            } else if !self.at(&Token::RBrace) {
                return Err(ParseError {
                    message: "expected ',' or '}' after spread expression in anon record literal".into(),
                    span: self.span(),
                });
            }
        }
        let mut fields: Vec<(Symbol, Expr)> = Vec::new();
        let mut seen: std::collections::HashSet<Symbol> = std::collections::HashSet::new();
        while !self.at(&Token::RBrace) {
            self.skip_nl();
            if self.at(&Token::DotDotDot) {
                return Err(ParseError {
                    message: "v1 row polymorphism allows only one spread head per anon record literal".into(),
                    span: self.span(),
                });
            }
            let (name, _) = self.expect_ident()?;
            self.expect(&Token::Colon)?;
            self.skip_nl();
            let value = self.parse_expr()?;
            if !seen.insert(name) {
                return Err(ParseError {
                    message: format!("duplicate field '{}' in anon record literal", name),
                    span: self.span(),
                });
            }
            fields.push((name, value));
            self.expect_list_sep("anon record literal fields", '}', &Token::RBrace)?;
        }
        self.expect(&Token::RBrace)?;
        Ok(Expr::new(ExprKind::AnonRecord { spread, fields }, span))
    }

    fn parse_record_fields(&mut self) -> Result<Vec<(Symbol, Expr)>> {
        let mut fields = Vec::new();
        self.skip_nl();
        while !self.at(&Token::RBrace) {
            if self.at(&Token::DotDot) || self.at(&Token::Dot) {
                return Err(ParseError {
                    message: "spread syntax is not supported; use `value.{ field: expr }` for record updates".into(),
                    span: self.span(),
                });
            }
            let (name, _) = self.expect_ident()?;
            self.expect(&Token::Colon)?;
            self.skip_nl();
            let value = self.parse_expr()?;
            fields.push((name, value));
            self.expect_list_sep("record literal fields", '}', &Token::RBrace)?;
        }
        Ok(fields)
    }

    // ── Patterns ─────────────────────────────────────────────────────

    fn parse_pattern(&mut self) -> Result<Pattern> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            self.depth -= 1;
            return Err(ParseError {
                message: "pattern nesting exceeds maximum depth".into(),
                span: self.span(),
            });
        }
        let result = self.parse_pattern_inner();
        self.depth -= 1;
        result
    }

    fn parse_pattern_inner(&mut self) -> Result<Pattern> {
        let first = self.parse_primary_pattern()?;
        // Check for or-pattern: pat1 | pat2 | ...
        if self.at(&Token::Bar) {
            let or_span = first.span;
            let mut alts = vec![first];
            while self.at(&Token::Bar) {
                self.advance();
                alts.push(self.parse_primary_pattern()?);
            }
            Ok(Pattern::new(PatternKind::Or(alts), or_span))
        } else {
            Ok(first)
        }
    }

    fn parse_primary_pattern(&mut self) -> Result<Pattern> {
        self.skip_nl();
        let start = self.span();
        let mk = |kind: PatternKind| Pattern::new(kind, start);
        match self.peek().clone() {
            Token::Ident(ref name) if *name == intern::intern("_") => {
                self.advance();
                Ok(mk(PatternKind::Wildcard))
            }
            Token::Ident(ref name) if is_constructor(*name) => {
                let mut name = *name;
                self.advance();
                // Qualified variant pattern: `EnumName.Variant(args)` or
                // `EnumName.Variant { fields }` or bare `EnumName.Variant`.
                // Silt's variants resolve by bare name globally (see
                // `variant_to_enum`), so the qualifier is disambiguation
                // only. We consume it and parse the variant as the
                // effective constructor name. Same-name variant
                // collisions across enums are already reported; if the
                // user names the wrong enum here we surface a targeted
                // error in the typechecker's pattern resolution path.
                if self.at(&Token::Dot) {
                    self.advance();
                    let (variant, _) = self.expect_ident()?;
                    if !is_constructor(variant) {
                        return Err(ParseError {
                            message: format!(
                                "expected a variant name after '{}.', found '{}'",
                                intern::resolve(name),
                                intern::resolve(variant)
                            ),
                            span: self.span(),
                        });
                    }
                    name = variant;
                }
                // Constructor pattern: Some(x), Ok(value), Rect(w, h)
                if self.at(&Token::LParen) {
                    self.advance();
                    let mut pats = Vec::new();
                    self.skip_nl();
                    while !self.at(&Token::RParen) {
                        pats.push(self.parse_pattern()?);
                        self.expect_list_sep("constructor pattern", ')', &Token::RParen)?;
                    }
                    self.expect(&Token::RParen)?;
                    Ok(mk(PatternKind::Constructor(name, pats)))
                } else if self.at(&Token::LBrace) {
                    // Record pattern: User { name, age, .. }
                    self.advance();
                    self.skip_nl();
                    let mut fields = Vec::new();
                    let mut has_rest = false;
                    while !self.at(&Token::RBrace) {
                        self.skip_nl();
                        if self.at(&Token::DotDot) {
                            self.advance();
                            has_rest = true;
                            self.skip_nl();
                            break;
                        }
                        let (field_name, _) = self.expect_ident()?;
                        // Optional sub-pattern: `name: pat`
                        let sub = if self.peek_skip_nl() == &Token::Colon {
                            self.advance();
                            Some(self.parse_pattern()?)
                        } else {
                            None
                        };
                        fields.push((field_name, sub));
                        self.expect_list_sep("record pattern fields", '}', &Token::RBrace)?;
                    }
                    self.expect(&Token::RBrace)?;
                    Ok(mk(PatternKind::Record {
                        name: Some(name),
                        fields,
                        has_rest,
                    }))
                } else {
                    Ok(mk(PatternKind::Constructor(name, Vec::new())))
                }
            }
            Token::Ident(name) => {
                self.advance();
                Ok(mk(PatternKind::Ident(name)))
            }
            Token::Int(n) => {
                self.advance();
                // Check for range pattern: n..m
                if self.at(&Token::DotDot) {
                    self.advance();
                    match self.peek().clone() {
                        Token::Int(m) => {
                            self.advance();
                            Ok(mk(PatternKind::Range(n, m)))
                        }
                        Token::Minus => {
                            self.advance();
                            match self.peek().clone() {
                                Token::Int(m) => {
                                    self.advance();
                                    Ok(mk(PatternKind::Range(n, -m)))
                                }
                                _ => Err(ParseError {
                                    message: "expected integer after - in range pattern".into(),
                                    span: self.span(),
                                }),
                            }
                        }
                        _ => Err(ParseError {
                            message: "expected integer end for range pattern".into(),
                            span: self.span(),
                        }),
                    }
                } else {
                    Ok(mk(PatternKind::Int(n)))
                }
            }
            Token::Float(n) => {
                self.advance();
                if self.at(&Token::DotDot) {
                    self.advance();
                    let end = if self.at(&Token::Minus) {
                        self.advance();
                        match self.peek().clone() {
                            Token::Float(m) => {
                                self.advance();
                                -m
                            }
                            _ => {
                                return Err(ParseError {
                                    message: "expected float after - in range pattern".into(),
                                    span: self.span(),
                                });
                            }
                        }
                    } else {
                        match self.peek().clone() {
                            Token::Float(m) => {
                                self.advance();
                                m
                            }
                            _ => {
                                return Err(ParseError {
                                    message: "expected float end for range pattern".into(),
                                    span: self.span(),
                                });
                            }
                        }
                    };
                    Ok(mk(PatternKind::FloatRange(n, end)))
                } else {
                    Ok(mk(PatternKind::Float(n)))
                }
            }
            Token::Bool(b) => {
                self.advance();
                Ok(mk(PatternKind::Bool(b)))
            }
            Token::StringLit(s, triple) => {
                self.advance();
                Ok(mk(PatternKind::StringLit(s, triple)))
            }
            Token::LParen => {
                self.advance();
                self.skip_nl();
                if self.at(&Token::RParen) {
                    self.advance();
                    return Ok(mk(PatternKind::Tuple(Vec::new())));
                }
                let first = self.parse_pattern()?;
                self.skip_nl();
                if self.at(&Token::Comma) {
                    self.advance();
                    let mut pats = vec![first];
                    self.skip_nl();
                    while !self.at(&Token::RParen) {
                        pats.push(self.parse_pattern()?);
                        self.expect_list_sep("tuple pattern", ')', &Token::RParen)?;
                    }
                    self.expect(&Token::RParen)?;
                    Ok(mk(PatternKind::Tuple(pats)))
                } else {
                    self.expect(&Token::RParen)?;
                    // Single-element parenthesized pattern
                    Ok(first)
                }
            }
            Token::LBrace => {
                // Anonymous record pattern: `{name: n, age: a}` or
                // `{name: n, ...rest}`. Only entered in pattern position
                // (parse_primary_pattern), so we don't conflict with
                // block / trailing-closure disambiguation in expression
                // contexts.
                self.advance();
                self.skip_nl();
                let mut fields: Vec<(Symbol, Option<Pattern>)> = Vec::new();
                let mut rest: Option<Symbol> = None;
                while !self.at(&Token::RBrace) {
                    self.skip_nl();
                    if self.at(&Token::DotDotDot) {
                        self.advance();
                        // Named rest binding required (B6: no unnamed rest).
                        let (rname, _) = self.expect_ident()?;
                        rest = Some(rname);
                        self.skip_nl();
                        break;
                    }
                    let (field_name, _) = self.expect_ident()?;
                    let sub = if self.peek_skip_nl() == &Token::Colon {
                        self.advance();
                        Some(self.parse_pattern()?)
                    } else {
                        None
                    };
                    fields.push((field_name, sub));
                    self.expect_list_sep("anon record pattern fields", '}', &Token::RBrace)?;
                }
                self.expect(&Token::RBrace)?;
                Ok(mk(PatternKind::AnonRecord { fields, rest }))
            }
            Token::LBracket => {
                self.advance(); // consume [
                self.skip_nl();
                if self.at(&Token::RBracket) {
                    self.advance();
                    return Ok(mk(PatternKind::List(vec![], None))); // empty list pattern
                }
                let mut patterns = Vec::new();
                let mut rest = None;
                loop {
                    self.skip_nl();
                    if self.at(&Token::DotDot) {
                        // ...rest pattern
                        self.advance(); // consume ..
                        let rest_pat = self.parse_pattern()?;
                        rest = Some(Box::new(rest_pat));
                        self.skip_nl();
                        break;
                    }
                    patterns.push(self.parse_pattern()?);
                    self.skip_nl();
                    if self.at(&Token::Comma) {
                        self.advance();
                        self.skip_nl();
                        // Check if next is ..rest after comma
                        if self.at(&Token::DotDot) {
                            self.advance();
                            let rest_pat = self.parse_pattern()?;
                            rest = Some(Box::new(rest_pat));
                            self.skip_nl();
                            break;
                        }
                    } else {
                        break;
                    }
                }
                self.expect(&Token::RBracket)?;
                Ok(mk(PatternKind::List(patterns, rest)))
            }
            Token::HashBrace => {
                // Map pattern: #{ "key": pattern, ... }
                self.advance();
                self.skip_nl();
                let mut entries = Vec::new();
                while !self.at(&Token::RBrace) {
                    self.skip_nl();
                    let key = match self.peek().clone() {
                        Token::StringLit(s, _) => {
                            self.advance();
                            s
                        }
                        _ => {
                            return Err(ParseError {
                                message: "expected string key in map pattern".into(),
                                span: self.span(),
                            });
                        }
                    };
                    self.expect(&Token::Colon)?;
                    let pat = self.parse_pattern()?;
                    entries.push((key, pat));
                    self.expect_list_sep("map pattern", '}', &Token::RBrace)?;
                }
                self.expect(&Token::RBrace)?;
                Ok(mk(PatternKind::Map(entries)))
            }
            Token::Minus => {
                // Negative number pattern
                self.advance();
                match self.peek().clone() {
                    Token::Int(n) => {
                        self.advance();
                        // Check for range pattern: -n..m
                        if self.at(&Token::DotDot) {
                            self.advance();
                            match self.peek().clone() {
                                Token::Int(m) => {
                                    self.advance();
                                    Ok(mk(PatternKind::Range(-n, m)))
                                }
                                Token::Minus => {
                                    self.advance();
                                    match self.peek().clone() {
                                        Token::Int(m) => {
                                            self.advance();
                                            Ok(mk(PatternKind::Range(-n, -m)))
                                        }
                                        _ => Err(ParseError {
                                            message: "expected integer after - in range pattern"
                                                .into(),
                                            span: self.span(),
                                        }),
                                    }
                                }
                                _ => Err(ParseError {
                                    message: "expected integer end for range pattern".into(),
                                    span: self.span(),
                                }),
                            }
                        } else {
                            Ok(mk(PatternKind::Int(-n)))
                        }
                    }
                    Token::Float(n) => {
                        self.advance();
                        if self.at(&Token::DotDot) {
                            self.advance();
                            match self.peek().clone() {
                                Token::Float(m) => {
                                    self.advance();
                                    Ok(mk(PatternKind::FloatRange(-n, m)))
                                }
                                Token::Minus => {
                                    self.advance();
                                    match self.peek().clone() {
                                        Token::Float(m) => {
                                            self.advance();
                                            Ok(mk(PatternKind::FloatRange(-n, -m)))
                                        }
                                        _ => Err(ParseError {
                                            message: "expected float after - in range pattern"
                                                .into(),
                                            span: self.span(),
                                        }),
                                    }
                                }
                                _ => Err(ParseError {
                                    message: "expected float end for range pattern".into(),
                                    span: self.span(),
                                }),
                            }
                        } else {
                            Ok(mk(PatternKind::Float(-n)))
                        }
                    }
                    _ => Err(ParseError {
                        message: "expected number after -".into(),
                        span: self.span(),
                    }),
                }
            }
            Token::Caret => {
                self.advance();
                match self.peek().clone() {
                    Token::Ident(name) => {
                        self.advance();
                        Ok(mk(PatternKind::Pin(name)))
                    }
                    _ => Err(ParseError {
                        message: "expected identifier after ^ in pin pattern".into(),
                        span: self.span(),
                    }),
                }
            }
            _ => Err(ParseError {
                message: format!("expected pattern, found {}", self.peek()),
                span: self.span(),
            }),
        }
    }

    // ── Utility ──────────────────────────────────────────────────────

    fn peek_skip_nl(&mut self) -> &Token {
        self.skip_nl();
        self.peek()
    }
}

fn is_constructor(name: Symbol) -> bool {
    intern::resolve(name).starts_with(|c: char| c.is_uppercase())
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intern;
    use crate::lexer::Lexer;

    fn parse(input: &str) -> Program {
        let tokens = Lexer::new(input).tokenize().unwrap();
        Parser::new(tokens).parse_program().unwrap()
    }

    #[test]
    fn test_hello_world() {
        let prog = parse(
            r#"
            fn main() {
                println("hello, world")
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        assert!(matches!(prog.decls[0], Decl::Fn(_)));
    }

    #[test]
    fn test_fizzbuzz() {
        let prog = parse(
            r#"
fn fizzbuzz(n) {
  match (n % 3, n % 5) {
    (0, 0) -> "FizzBuzz"
    (0, _) -> "Fizz"
    (_, 0) -> "Buzz"
    _      -> "{n}"
  }
}

fn main() {
  1..101
  |> map { n -> fizzbuzz(n) }
  |> each { s -> println(s) }
}
        "#,
        );
        assert_eq!(prog.decls.len(), 2);
    }

    #[test]
    fn test_type_decl_record() {
        let prog = parse(
            r#"
            type User {
                name: String,
                age: Int,
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Type(ref td) = prog.decls[0] {
            assert_eq!(td.name, intern::intern("User"));
            assert!(matches!(td.body, TypeBody::Record(_)));
        } else {
            panic!("expected type decl");
        }
    }

    #[test]
    fn test_type_decl_enum() {
        let prog = parse(
            r#"
            type Shape {
                Circle(Float),
                Rect(Float, Float),
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Type(ref td) = prog.decls[0] {
            assert_eq!(td.name, intern::intern("Shape"));
            if let TypeBody::Enum(ref variants) = td.body {
                assert_eq!(variants.len(), 2);
            } else {
                panic!("expected enum");
            }
        }
    }

    #[test]
    fn test_pipe_and_trailing_closure() {
        let prog = parse(
            r#"
            fn main() {
                [1, 2, 3] |> map { x -> x * 2 }
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
    }

    #[test]
    fn test_record_create_and_update() {
        let prog = parse(
            r#"
            fn main() {
                let u = User { name: "Alice", age: 30 }
                let u2 = u.{ age: 31 }
                u2
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
    }

    #[test]
    fn test_when_stmt() {
        let prog = parse(
            r#"
            fn main() {
                when let Some(x) = find(42) else {
                    return None
                }
                x
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
    }

    #[test]
    fn test_trait_impl() {
        let prog = parse(
            r#"
            trait Display for Shape {
                fn display(self) -> String {
                    match self {
                        Circle(r) -> "circle"
                        Rect(w, h) -> "rect"
                    }
                }
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        assert!(matches!(prog.decls[0], Decl::TraitImpl(_)));
    }

    #[test]
    fn test_import() {
        let prog = parse(
            r#"
            import io
            import math.{ add, Point }
            import math as m
        "#,
        );
        assert_eq!(prog.decls.len(), 3);
    }

    #[test]
    fn test_question_mark() {
        let prog = parse(
            r#"
            fn main() {
                let x = foo()?
                x
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
    }

    #[test]
    fn test_match_with_guard() {
        let prog = parse(
            r#"
            fn classify(n) {
                match n {
                    0 -> "zero"
                    x when x > 0 -> "positive"
                    _ -> "negative"
                }
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
    }

    #[test]
    fn test_string_interp() {
        let prog = parse(
            r#"
            fn main() {
                let name = "world"
                println("hello {name}")
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
    }

    #[test]
    fn test_where_clause() {
        let prog = parse(
            r#"
            fn show(x) where x: Display {
                x
            }
            fn main() { 0 }
        "#,
        );
        if let Decl::Fn(f) = &prog.decls[0] {
            assert_eq!(f.where_clauses.len(), 1);
            let (var, trait_name, args) = &f.where_clauses[0];
            assert_eq!(*var, intern::intern("x"));
            assert_eq!(*trait_name, intern::intern("Display"));
            assert!(args.is_empty());
        } else {
            panic!("expected fn decl");
        }
    }

    #[test]
    fn test_where_clause_multiple() {
        let prog = parse(
            r#"
            fn compare_show(a, b) where a: Display, b: Compare {
                a
            }
            fn main() { 0 }
        "#,
        );
        if let Decl::Fn(f) = &prog.decls[0] {
            assert_eq!(f.where_clauses.len(), 2);
        } else {
            panic!("expected fn decl");
        }
    }

    #[test]
    fn test_abstract_trait_method() {
        let prog = parse(
            r#"
            trait Display {
                fn display(self) -> String
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Trait(ref td) = prog.decls[0] {
            assert_eq!(td.name, intern::intern("Display"));
            assert_eq!(td.methods.len(), 1);
            assert_eq!(td.methods[0].name, intern::intern("display"));
        } else {
            panic!("expected trait decl");
        }
    }

    #[test]
    fn test_fn_without_where_still_works() {
        // Regression test: functions without where should still parse
        let prog = parse(
            r#"
            fn add(a, b) { a + b }
            fn main() { add(1, 2) }
        "#,
        );
        assert_eq!(prog.decls.len(), 2);
    }

    #[test]
    fn test_match_with_trailing_closure_in_pipe() {
        // Trailing closures in pipe RHS should work inside match scrutinees.
        // The `{ x -> x > 5 }` is a trailing closure for `list.any`, while
        // `{ true -> ... }` is the match body.
        let prog = parse(
            r#"
            fn main() {
                let items = [1, 2, 3, 6]
                match items |> list.any { x -> x > 5 } {
                    true -> "has big"
                    _ -> "all small"
                }
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        // Verify the match has a scrutinee with a pipe expression
        if let Decl::Fn(ref f) = prog.decls[0] {
            let stmts = match &f.body.kind {
                ExprKind::Block(stmts) => stmts,
                _ => panic!("expected block body"),
            };
            // The match expression is the last statement
            let match_expr = match stmts.last().unwrap() {
                Stmt::Expr(e) => e,
                _ => panic!("expected expression statement"),
            };
            if let ExprKind::Match {
                expr: Some(scrutinee),
                arms,
            } = &match_expr.kind
            {
                // Scrutinee should be a Pipe
                assert!(
                    matches!(scrutinee.kind, ExprKind::Pipe(_, _)),
                    "expected Pipe scrutinee, got {:?}",
                    scrutinee.kind
                );
                // Should have 2 arms
                assert_eq!(arms.len(), 2);
            } else {
                panic!("expected match expression with scrutinee");
            }
        }
    }

    #[test]
    fn test_match_with_chained_pipes_and_trailing_closures() {
        // Multiple pipes with trailing closures in a match scrutinee
        let prog = parse(
            r#"
            fn main() {
                match items |> filter { x -> x > 0 } |> map { x -> x * 2 } {
                    [] -> "empty"
                    _ -> "non-empty"
                }
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
    }

    #[test]
    fn test_when_bool_stmt() {
        let prog = parse(
            r#"
            fn main() {
                when x > 0 else {
                    return "negative"
                }
                x
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
    }

    #[test]
    fn test_when_bool_mixed_with_pattern() {
        let prog = parse(
            r#"
            fn main() {
                when let Ok(value) = parse(input) else {
                    return Err("failed")
                }
                when value > 0 else {
                    return Err("must be positive")
                }
                value
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
    }

    // ── Error-recovery helpers ──────────────────────────────────────

    fn parse_err(input: &str) -> ParseError {
        let tokens = Lexer::new(input).tokenize().unwrap();
        Parser::new(tokens).parse_program().unwrap_err()
    }

    fn parse_recovering(input: &str) -> (Program, Vec<ParseError>) {
        let tokens = Lexer::new(input).tokenize().unwrap();
        Parser::new(tokens).parse_program_recovering()
    }

    // ── 1. Error recovery ───────────────────────────────────────────

    #[test]
    fn test_recovery_skips_bad_decl_and_continues() {
        let (prog, errs) = parse_recovering(
            r#"
            fn good1() { 1 }
            fn { broken }
            fn good2() { 2 }
        "#,
        );
        assert!(!errs.is_empty(), "expected at least one error");
        // Recovery should still produce at least the two valid decls
        assert!(
            prog.decls.len() >= 2,
            "expected at least 2 decls, got {}",
            prog.decls.len()
        );
    }

    #[test]
    fn test_recovery_collects_multiple_errors() {
        let (prog, errs) = parse_recovering(
            r#"
            fn { broken1 }
            fn { broken2 }
            fn ok() { 0 }
        "#,
        );
        assert!(
            errs.len() >= 2,
            "expected at least 2 errors, got {}",
            errs.len()
        );
        assert!(!prog.decls.is_empty());
    }

    // ── 2. Pattern parsing ──────────────────────────────────────────

    #[test]
    fn test_or_pattern() {
        let prog = parse(
            r#"
            fn classify(n) {
                match n {
                    1 | 2 | 3 -> "small"
                    _ -> "big"
                }
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Fn(ref f) = prog.decls[0] {
            let stmts = match &f.body.kind {
                ExprKind::Block(stmts) => stmts,
                _ => panic!("expected block"),
            };
            let match_expr = match stmts.last().unwrap() {
                Stmt::Expr(e) => e,
                _ => panic!("expected expr stmt"),
            };
            if let ExprKind::Match { arms, .. } = &match_expr.kind {
                assert!(matches!(&arms[0].pattern.kind, PatternKind::Or(pats) if pats.len() == 3));
            } else {
                panic!("expected match");
            }
        }
    }

    #[test]
    fn test_range_pattern() {
        let prog = parse(
            r#"
            fn classify(n) {
                match n {
                    1..10 -> "small"
                    _ -> "other"
                }
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Fn(ref f) = prog.decls[0] {
            let stmts = match &f.body.kind {
                ExprKind::Block(stmts) => stmts,
                _ => panic!("expected block"),
            };
            let match_expr = match stmts.last().unwrap() {
                Stmt::Expr(e) => e,
                _ => panic!("expected expr stmt"),
            };
            if let ExprKind::Match { arms, .. } = &match_expr.kind {
                assert!(matches!(&arms[0].pattern.kind, PatternKind::Range(1, 10)));
            } else {
                panic!("expected match");
            }
        }
    }

    #[test]
    fn test_pin_pattern() {
        let prog = parse(
            r#"
            fn check(x, y) {
                match y {
                    ^x -> "equal"
                    _ -> "different"
                }
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Fn(ref f) = prog.decls[0] {
            let stmts = match &f.body.kind {
                ExprKind::Block(stmts) => stmts,
                _ => panic!("expected block"),
            };
            let match_expr = match stmts.last().unwrap() {
                Stmt::Expr(e) => e,
                _ => panic!("expected expr stmt"),
            };
            if let ExprKind::Match { arms, .. } = &match_expr.kind {
                assert!(
                    matches!(&arms[0].pattern.kind, PatternKind::Pin(name) if *name == intern::intern("x"))
                );
            } else {
                panic!("expected match");
            }
        }
    }

    #[test]
    fn test_map_pattern() {
        let prog = parse(
            r#"
            fn get_name(m) {
                match m {
                    #{ "key": v } -> v
                    _ -> "none"
                }
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Fn(ref f) = prog.decls[0] {
            let stmts = match &f.body.kind {
                ExprKind::Block(stmts) => stmts,
                _ => panic!("expected block"),
            };
            let match_expr = match stmts.last().unwrap() {
                Stmt::Expr(e) => e,
                _ => panic!("expected expr stmt"),
            };
            if let ExprKind::Match { arms, .. } = &match_expr.kind {
                if let PatternKind::Map(ref entries) = arms[0].pattern.kind {
                    assert_eq!(entries.len(), 1);
                    assert_eq!(entries[0].0, "key");
                    assert!(
                        matches!(entries[0].1.kind, PatternKind::Ident(ref v) if *v == intern::intern("v"))
                    );
                } else {
                    panic!("expected map pattern");
                }
            } else {
                panic!("expected match");
            }
        }
    }

    #[test]
    fn test_nested_constructor_pattern() {
        let prog = parse(
            r#"
            fn extract(x) {
                match x {
                    Some((a, b)) -> a
                    None -> 0
                }
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Fn(ref f) = prog.decls[0] {
            let stmts = match &f.body.kind {
                ExprKind::Block(stmts) => stmts,
                _ => panic!("expected block"),
            };
            let match_expr = match stmts.last().unwrap() {
                Stmt::Expr(e) => e,
                _ => panic!("expected expr stmt"),
            };
            if let ExprKind::Match { arms, .. } = &match_expr.kind {
                if let PatternKind::Constructor(ref name, ref inner) = arms[0].pattern.kind {
                    assert_eq!(*name, intern::intern("Some"));
                    assert_eq!(inner.len(), 1);
                    assert!(matches!(&inner[0].kind, PatternKind::Tuple(pats) if pats.len() == 2));
                } else {
                    panic!("expected constructor pattern, got {:?}", arms[0].pattern);
                }
            } else {
                panic!("expected match");
            }
        }
    }

    #[test]
    fn test_list_pattern_with_rest() {
        let prog = parse(
            r#"
            fn head_tail(xs) {
                match xs {
                    [h, ..t] -> h
                    [] -> 0
                }
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Fn(ref f) = prog.decls[0] {
            let stmts = match &f.body.kind {
                ExprKind::Block(stmts) => stmts,
                _ => panic!("expected block"),
            };
            let match_expr = match stmts.last().unwrap() {
                Stmt::Expr(e) => e,
                _ => panic!("expected expr stmt"),
            };
            if let ExprKind::Match { arms, .. } = &match_expr.kind {
                if let PatternKind::List(ref pats, ref rest) = arms[0].pattern.kind {
                    assert_eq!(pats.len(), 1);
                    assert!(
                        matches!(&pats[0].kind, PatternKind::Ident(n) if *n == intern::intern("h"))
                    );
                    assert!(rest.is_some());
                    assert!(
                        matches!(&rest.as_deref().unwrap().kind, PatternKind::Ident(n) if *n == intern::intern("t"))
                    );
                } else {
                    panic!("expected list pattern");
                }
                // Second arm: empty list
                assert!(
                    matches!(&arms[1].pattern.kind, PatternKind::List(pats, None) if pats.is_empty())
                );
            } else {
                panic!("expected match");
            }
        }
    }

    #[test]
    fn test_record_shorthand_pattern() {
        let prog = parse(
            r#"
            fn greet(u) {
                match u {
                    User { name, age } -> name
                    _ -> "unknown"
                }
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Fn(ref f) = prog.decls[0] {
            let stmts = match &f.body.kind {
                ExprKind::Block(stmts) => stmts,
                _ => panic!("expected block"),
            };
            let match_expr = match stmts.last().unwrap() {
                Stmt::Expr(e) => e,
                _ => panic!("expected expr stmt"),
            };
            if let ExprKind::Match { arms, .. } = &match_expr.kind {
                if let PatternKind::Record {
                    ref name,
                    ref fields,
                    has_rest,
                } = arms[0].pattern.kind
                {
                    assert_eq!(*name, Some(intern::intern("User")));
                    assert_eq!(fields.len(), 2);
                    assert_eq!(fields[0].0, intern::intern("name"));
                    assert!(fields[0].1.is_none()); // shorthand
                    assert_eq!(fields[1].0, intern::intern("age"));
                    assert!(fields[1].1.is_none());
                    assert!(!has_rest);
                } else {
                    panic!("expected record pattern");
                }
            } else {
                panic!("expected match");
            }
        }
    }

    // ── 3. Expression parsing ───────────────────────────────────────

    #[test]
    fn test_empty_list() {
        let prog = parse(
            r#"
            fn main() {
                []
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Fn(ref f) = prog.decls[0] {
            let stmts = match &f.body.kind {
                ExprKind::Block(stmts) => stmts,
                _ => panic!("expected block"),
            };
            let expr = match stmts.last().unwrap() {
                Stmt::Expr(e) => e,
                _ => panic!("expected expr stmt"),
            };
            if let ExprKind::List(ref elems) = expr.kind {
                assert!(elems.is_empty());
            } else {
                panic!("expected empty list, got {:?}", expr.kind);
            }
        }
    }

    #[test]
    fn test_map_literal() {
        let prog = parse(
            r#"
            fn main() {
                #{ "a": 1, "b": 2 }
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Fn(ref f) = prog.decls[0] {
            let stmts = match &f.body.kind {
                ExprKind::Block(stmts) => stmts,
                _ => panic!("expected block"),
            };
            let expr = match stmts.last().unwrap() {
                Stmt::Expr(e) => e,
                _ => panic!("expected expr stmt"),
            };
            if let ExprKind::Map(ref entries) = expr.kind {
                assert_eq!(entries.len(), 2);
            } else {
                panic!("expected map literal, got {:?}", expr.kind);
            }
        }
    }

    #[test]
    fn test_set_literal() {
        let prog = parse(
            r#"
            fn main() {
                #[1, 2, 3]
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Fn(ref f) = prog.decls[0] {
            let stmts = match &f.body.kind {
                ExprKind::Block(stmts) => stmts,
                _ => panic!("expected block"),
            };
            let expr = match stmts.last().unwrap() {
                Stmt::Expr(e) => e,
                _ => panic!("expected expr stmt"),
            };
            if let ExprKind::SetLit(ref elems) = expr.kind {
                assert_eq!(elems.len(), 3);
            } else {
                panic!("expected set literal, got {:?}", expr.kind);
            }
        }
    }

    #[test]
    fn test_range_expression() {
        let prog = parse(
            r#"
            fn main() {
                1..10
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Fn(ref f) = prog.decls[0] {
            let stmts = match &f.body.kind {
                ExprKind::Block(stmts) => stmts,
                _ => panic!("expected block"),
            };
            let expr = match stmts.last().unwrap() {
                Stmt::Expr(e) => e,
                _ => panic!("expected expr stmt"),
            };
            assert!(matches!(&expr.kind, ExprKind::Range(_, _)));
        }
    }

    #[test]
    fn test_nested_pipes() {
        let prog = parse(
            r#"
            fn main() {
                a |> f |> g
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Fn(ref f) = prog.decls[0] {
            let stmts = match &f.body.kind {
                ExprKind::Block(stmts) => stmts,
                _ => panic!("expected block"),
            };
            let expr = match stmts.last().unwrap() {
                Stmt::Expr(e) => e,
                _ => panic!("expected expr stmt"),
            };
            // Should be Pipe(Pipe(a, f), g) — left-associative
            if let ExprKind::Pipe(ref left, ref right) = expr.kind {
                assert!(matches!(&right.kind, ExprKind::Ident(n) if *n == intern::intern("g")));
                assert!(matches!(&left.kind, ExprKind::Pipe(_, _)));
            } else {
                panic!("expected pipe expression, got {:?}", expr.kind);
            }
        }
    }

    #[test]
    fn test_question_mark_wraps_full_pipe() {
        // `x |> f(y)?` must parse as `(x |> f(y))?` — `?` applies to the
        // piped result, not to the inner call `f(y)`. Historically `?`
        // bound tighter than `|>`, which made error-conversion pipelines
        // like `io.read_file(p) |> result.map_err(Wrap)?` fail to
        // type-check because `?` was stuck on a half-applied call.
        let prog = parse(
            r#"
            fn main() {
                a |> f(b)?
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Fn(ref f) = prog.decls[0] {
            let stmts = match &f.body.kind {
                ExprKind::Block(stmts) => stmts,
                _ => panic!("expected block"),
            };
            let expr = match stmts.last().unwrap() {
                Stmt::Expr(e) => e,
                _ => panic!("expected expr stmt"),
            };
            // Top-level must be QuestionMark, not Pipe.
            let ExprKind::QuestionMark(inner) = &expr.kind else {
                panic!("expected `?` at top of expression, got {:?}", expr.kind);
            };
            // Inner must be a Pipe.
            assert!(
                matches!(&inner.kind, ExprKind::Pipe(_, _)),
                "expected Pipe inside `?`, got {:?}",
                inner.kind
            );
        }
    }

    #[test]
    fn test_question_mark_binds_looser_than_plus() {
        // New precedence: `a + b?` parses as `(a + b)?`. The old high-bp
        // `?` made this `a + (b?)`. Either way silt's typechecker then
        // enforces `Result` / `Option` on the `?` LHS, so the surface
        // meaning of valid programs is unaffected — but the AST shape
        // flipped and we lock that here.
        let prog = parse(
            r#"
            fn main() {
                a + b?
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Fn(ref f) = prog.decls[0] {
            let stmts = match &f.body.kind {
                ExprKind::Block(stmts) => stmts,
                _ => panic!("expected block"),
            };
            let expr = match stmts.last().unwrap() {
                Stmt::Expr(e) => e,
                _ => panic!("expected expr stmt"),
            };
            let ExprKind::QuestionMark(inner) = &expr.kind else {
                panic!("expected `?` at top, got {:?}", expr.kind);
            };
            assert!(
                matches!(&inner.kind, ExprKind::Binary(_, _, _)),
                "expected Binary inside `?`, got {:?}",
                inner.kind
            );
        }
    }

    #[test]
    fn test_question_mark_binds_looser_than_range() {
        // `a..b?` parses as `(a..b)?` under the new precedence. Checked
        // separately from `+` because the range operator has its own bp
        // and lives between `|>` and arithmetic.
        let prog = parse(
            r#"
            fn main() {
                a..b?
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Fn(ref f) = prog.decls[0] {
            let stmts = match &f.body.kind {
                ExprKind::Block(stmts) => stmts,
                _ => panic!("expected block"),
            };
            let expr = match stmts.last().unwrap() {
                Stmt::Expr(e) => e,
                _ => panic!("expected expr stmt"),
            };
            let ExprKind::QuestionMark(inner) = &expr.kind else {
                panic!("expected `?` at top, got {:?}", expr.kind);
            };
            assert!(
                matches!(&inner.kind, ExprKind::Range(_, _)),
                "expected Range inside `?`, got {:?}",
                inner.kind
            );
        }
    }

    #[test]
    fn test_question_mark_binds_tighter_than_eq() {
        // `a == b?` still parses as `a == (b?)` — `?` binds tighter than
        // comparison operators, matching the old behavior. This is
        // load-bearing for patterns like `parse(x)? == expected_value`
        // where `?` is meant to unwrap the LHS of the comparison. Locks
        // that the new-lower `?` bp (54) still exceeds `==` r_bp (41).
        let prog = parse(
            r#"
            fn main() {
                a == b?
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Fn(ref f) = prog.decls[0] {
            let stmts = match &f.body.kind {
                ExprKind::Block(stmts) => stmts,
                _ => panic!("expected block"),
            };
            let expr = match stmts.last().unwrap() {
                Stmt::Expr(e) => e,
                _ => panic!("expected expr stmt"),
            };
            // Top-level is Binary(==), NOT QuestionMark — `?` was pulled
            // into the RHS of `==`.
            let ExprKind::Binary(_, op, rhs) = &expr.kind else {
                panic!("expected Binary at top, got {:?}", expr.kind);
            };
            assert_eq!(*op, BinOp::Eq);
            assert!(
                matches!(&rhs.kind, ExprKind::QuestionMark(_)),
                "expected QuestionMark on RHS of ==, got {:?}",
                rhs.kind
            );
        }
    }

    #[test]
    fn test_question_mark_binds_looser_than_mul() {
        // Multiplication's (l_bp=80, r_bp=81) are higher than `?`'s
        // new bp (54), so `a * b?` parses as `(a * b)?`.
        let prog = parse(
            r#"
            fn main() {
                a * b?
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Fn(ref f) = prog.decls[0] {
            let stmts = match &f.body.kind {
                ExprKind::Block(stmts) => stmts,
                _ => panic!("expected block"),
            };
            let expr = match stmts.last().unwrap() {
                Stmt::Expr(e) => e,
                _ => panic!("expected expr stmt"),
            };
            let ExprKind::QuestionMark(inner) = &expr.kind else {
                panic!("expected `?` at top, got {:?}", expr.kind);
            };
            let ExprKind::Binary(_, op, _) = &inner.kind else {
                panic!("expected Binary inside `?`, got {:?}", inner.kind);
            };
            assert_eq!(*op, BinOp::Mul);
        }
    }

    #[test]
    fn test_question_mark_binds_tighter_than_and() {
        // `&&` at (30, 31) is lower than `?` (54), so `a && b?` parses
        // as `a && (b?)`. Locks short-circuit semantics: the RHS of
        // `&&` must be a fully-formed boolean, so `?` on the RHS
        // unwraps just b's Result and the && fires over the unwrapped
        // value.
        let prog = parse(
            r#"
            fn main() {
                a && b?
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Fn(ref f) = prog.decls[0] {
            let stmts = match &f.body.kind {
                ExprKind::Block(stmts) => stmts,
                _ => panic!("expected block"),
            };
            let expr = match stmts.last().unwrap() {
                Stmt::Expr(e) => e,
                _ => panic!("expected expr stmt"),
            };
            let ExprKind::Binary(_, op, rhs) = &expr.kind else {
                panic!("expected Binary at top, got {:?}", expr.kind);
            };
            assert_eq!(*op, BinOp::And);
            assert!(
                matches!(&rhs.kind, ExprKind::QuestionMark(_)),
                "expected `?` on RHS of &&, got {:?}",
                rhs.kind
            );
        }
    }

    #[test]
    fn test_question_mark_binds_looser_than_as() {
        // `as` at bp=95 is higher than `?`'s new bp (54), so
        // `x as Int?` parses as `(x as Int)?`. Minor edge case worth
        // pinning; the old behavior produced the same shape via a
        // different path because `?` was also a postfix that happened
        // to attach to the `as` result.
        let prog = parse(
            r#"
            fn main() {
                x as Int?
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Fn(ref f) = prog.decls[0] {
            let stmts = match &f.body.kind {
                ExprKind::Block(stmts) => stmts,
                _ => panic!("expected block"),
            };
            let expr = match stmts.last().unwrap() {
                Stmt::Expr(e) => e,
                _ => panic!("expected expr stmt"),
            };
            let ExprKind::QuestionMark(inner) = &expr.kind else {
                panic!("expected `?` at top, got {:?}", expr.kind);
            };
            assert!(
                matches!(&inner.kind, ExprKind::Ascription(_, _)),
                "expected Ascription inside `?`, got {:?}",
                inner.kind
            );
        }
    }

    #[test]
    fn test_question_mark_still_binds_before_pipe_when_on_left() {
        // `f(a)? |> g` must still parse as `(f(a)?) |> g` — `?` binds to
        // the preceding call atom before the pipe takes its LHS. This
        // was the natural reading under the old high-bp `?` and must
        // stay correct under the new looser `?`.
        let prog = parse(
            r#"
            fn main() {
                f(a)? |> g
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Fn(ref f) = prog.decls[0] {
            let stmts = match &f.body.kind {
                ExprKind::Block(stmts) => stmts,
                _ => panic!("expected block"),
            };
            let expr = match stmts.last().unwrap() {
                Stmt::Expr(e) => e,
                _ => panic!("expected expr stmt"),
            };
            // Top-level must be Pipe, with LHS being QuestionMark.
            let ExprKind::Pipe(lhs, _rhs) = &expr.kind else {
                panic!("expected Pipe at top, got {:?}", expr.kind);
            };
            assert!(
                matches!(&lhs.kind, ExprKind::QuestionMark(_)),
                "expected QuestionMark on pipe LHS, got {:?}",
                lhs.kind
            );
        }
    }

    #[test]
    fn test_return_with_value() {
        let prog = parse(
            r#"
            fn main() {
                return 42
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Fn(ref f) = prog.decls[0] {
            let stmts = match &f.body.kind {
                ExprKind::Block(stmts) => stmts,
                _ => panic!("expected block"),
            };
            let expr = match stmts.last().unwrap() {
                Stmt::Expr(e) => e,
                _ => panic!("expected expr stmt"),
            };
            if let ExprKind::Return(ref val) = expr.kind {
                assert!(val.is_some());
            } else {
                panic!("expected return");
            }
        }
    }

    #[test]
    fn test_return_without_value() {
        let prog = parse(
            r#"
            fn main() {
                return
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Fn(ref f) = prog.decls[0] {
            let stmts = match &f.body.kind {
                ExprKind::Block(stmts) => stmts,
                _ => panic!("expected block"),
            };
            let expr = match stmts.last().unwrap() {
                Stmt::Expr(e) => e,
                _ => panic!("expected expr stmt"),
            };
            if let ExprKind::Return(ref val) = expr.kind {
                assert!(val.is_none());
            } else {
                panic!("expected return");
            }
        }
    }

    #[test]
    fn test_loop_with_bindings() {
        let prog = parse(
            r#"
            fn main() {
                loop i = 0, acc = 0 {
                    loop(i + 1, acc + i)
                }
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Fn(ref f) = prog.decls[0] {
            let stmts = match &f.body.kind {
                ExprKind::Block(stmts) => stmts,
                _ => panic!("expected block"),
            };
            let expr = match stmts.last().unwrap() {
                Stmt::Expr(e) => e,
                _ => panic!("expected expr stmt"),
            };
            if let ExprKind::Loop {
                ref bindings,
                ref body,
            } = expr.kind
            {
                assert_eq!(bindings.len(), 2);
                assert_eq!(bindings[0].0, intern::intern("i"));
                assert_eq!(bindings[1].0, intern::intern("acc"));
                // body should contain a Recur
                if let ExprKind::Block(ref inner_stmts) = body.kind {
                    let recur_expr = match inner_stmts.last().unwrap() {
                        Stmt::Expr(e) => e,
                        _ => panic!("expected expr in loop body"),
                    };
                    if let ExprKind::Recur(ref args) = recur_expr.kind {
                        assert_eq!(args.len(), 2);
                    } else {
                        panic!("expected recur, got {:?}", recur_expr.kind);
                    }
                } else {
                    panic!("expected block body");
                }
            } else {
                panic!("expected loop, got {:?}", expr.kind);
            }
        }
    }

    #[test]
    fn test_recur_in_loop() {
        let prog = parse(
            r#"
            fn sum(n) {
                loop i = 0, acc = 0 {
                    match i == n {
                        true -> acc
                        _ -> loop(i + 1, acc + i)
                    }
                }
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
    }

    // ── 4. Declaration parsing ──────────────────────────────────────

    #[test]
    fn test_pub_fn() {
        let prog = parse(
            r#"
            pub fn add(a, b) { a + b }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Fn(ref f) = prog.decls[0] {
            assert!(f.is_pub);
            assert_eq!(f.name, intern::intern("add"));
        } else {
            panic!("expected fn decl");
        }
    }

    #[test]
    fn test_pub_type() {
        let prog = parse(
            r#"
            pub type Color {
                Red,
                Green,
                Blue,
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Type(ref td) = prog.decls[0] {
            assert!(td.is_pub);
            assert_eq!(td.name, intern::intern("Color"));
        } else {
            panic!("expected type decl");
        }
    }

    #[test]
    fn test_let_decl() {
        let prog = parse(
            r#"
            let x = 42
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Let {
            ref pattern,
            ref value,
            is_pub,
            ..
        } = prog.decls[0]
        {
            assert!(!is_pub);
            assert!(matches!(&pattern.kind, PatternKind::Ident(n) if *n == intern::intern("x")));
            assert!(matches!(&value.kind, ExprKind::Int(42)));
        } else {
            panic!("expected let decl");
        }
    }

    #[test]
    fn test_abstract_trait_with_multiple_methods() {
        let prog = parse(
            r#"
            trait Comparable {
                fn compare(self, other: Self) -> Int
                fn equal(self, other: Self) -> Bool
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Trait(ref td) = prog.decls[0] {
            assert_eq!(td.name, intern::intern("Comparable"));
            assert_eq!(td.methods.len(), 2);
            assert_eq!(td.methods[0].name, intern::intern("compare"));
            assert_eq!(td.methods[1].name, intern::intern("equal"));
        } else {
            panic!("expected trait decl");
        }
    }

    #[test]
    fn test_multiple_imports() {
        let prog = parse(
            r#"
            import io
            import math.{ add, sub }
            import http as h
        "#,
        );
        assert_eq!(prog.decls.len(), 3);
        assert!(
            matches!(&prog.decls[0], Decl::Import(ImportTarget::Module(m), _) if *m == intern::intern("io"))
        );
        assert!(
            matches!(&prog.decls[1], Decl::Import(ImportTarget::Items(m, items), _) if *m == intern::intern("math") && items.len() == 2)
        );
        assert!(
            matches!(&prog.decls[2], Decl::Import(ImportTarget::Alias(m, a), _) if *m == intern::intern("http") && *a == intern::intern("h"))
        );
    }

    // ── 5. Error cases ──────────────────────────────────────────────

    #[test]
    fn test_error_missing_closing_brace() {
        let err = parse_err(
            r#"
            fn main() {
                42
        "#,
        );
        assert!(!err.message.is_empty());
    }

    #[test]
    fn test_error_missing_closing_paren() {
        let err = parse_err(
            r#"
            fn main(a, b {
                a
            }
        "#,
        );
        assert!(!err.message.is_empty());
    }

    #[test]
    fn test_error_invalid_token_in_expression() {
        let err = parse_err(
            r#"
            fn main() {
                ,,
            }
        "#,
        );
        assert!(!err.message.is_empty());
    }

    #[test]
    fn test_error_missing_arrow_in_match_arm() {
        let err = parse_err(
            r#"
            fn main() {
                match x {
                    1 "oops"
                }
            }
        "#,
        );
        assert!(!err.message.is_empty());
    }

    // ── 6. Edge cases ───────────────────────────────────────────────

    #[test]
    fn test_fn_with_where_clause_and_return_type() {
        let prog = parse(
            r#"
            fn show(x) -> String where x: Display {
                x
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Fn(ref f) = prog.decls[0] {
            assert_eq!(f.name, intern::intern("show"));
            assert!(f.return_type.is_some());
            assert_eq!(f.where_clauses.len(), 1);
            let (var, trait_name, args) = &f.where_clauses[0];
            assert_eq!(*var, intern::intern("x"));
            assert_eq!(*trait_name, intern::intern("Display"));
            assert!(args.is_empty());
        } else {
            panic!("expected fn decl");
        }
    }

    #[test]
    fn test_lambda_with_typed_params() {
        let prog = parse(
            r#"
            fn main() {
                fn(x: Int, y: Int) { x + y }
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Fn(ref f) = prog.decls[0] {
            let stmts = match &f.body.kind {
                ExprKind::Block(stmts) => stmts,
                _ => panic!("expected block"),
            };
            let expr = match stmts.last().unwrap() {
                Stmt::Expr(e) => e,
                _ => panic!("expected expr stmt"),
            };
            if let ExprKind::Lambda { ref params, .. } = expr.kind {
                assert_eq!(params.len(), 2);
                assert!(params[0].ty.is_some());
                assert!(params[1].ty.is_some());
            } else {
                panic!("expected lambda, got {:?}", expr.kind);
            }
        }
    }

    #[test]
    fn test_deeply_nested_blocks() {
        let prog = parse(
            r#"
            fn main() {
                {
                    {
                        {
                            42
                        }
                    }
                }
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
    }

    #[test]
    fn test_multiple_match_arms_with_guards() {
        let prog = parse(
            r#"
            fn classify(n) {
                match n {
                    x when x < 0 -> "negative"
                    0 -> "zero"
                    x when x < 10 -> "small"
                    x when x < 100 -> "medium"
                    _ -> "large"
                }
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Fn(ref f) = prog.decls[0] {
            let stmts = match &f.body.kind {
                ExprKind::Block(stmts) => stmts,
                _ => panic!("expected block"),
            };
            let match_expr = match stmts.last().unwrap() {
                Stmt::Expr(e) => e,
                _ => panic!("expected expr stmt"),
            };
            if let ExprKind::Match { ref arms, .. } = match_expr.kind {
                assert_eq!(arms.len(), 5);
                assert!(arms[0].guard.is_some());
                assert!(arms[1].guard.is_none());
                assert!(arms[2].guard.is_some());
                assert!(arms[3].guard.is_some());
                assert!(arms[4].guard.is_none());
            } else {
                panic!("expected match");
            }
        }
    }

    #[test]
    fn test_pub_let_decl() {
        let prog = parse(
            r#"
            pub let VERSION = "1.0"
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Let { is_pub, .. } = prog.decls[0] {
            assert!(is_pub);
        } else {
            panic!("expected pub let decl");
        }
    }

    #[test]
    fn test_record_pattern_with_rest() {
        let prog = parse(
            r#"
            fn name_only(u) {
                match u {
                    User { name, .. } -> name
                    _ -> "unknown"
                }
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Fn(ref f) = prog.decls[0] {
            let stmts = match &f.body.kind {
                ExprKind::Block(stmts) => stmts,
                _ => panic!("expected block"),
            };
            let match_expr = match stmts.last().unwrap() {
                Stmt::Expr(e) => e,
                _ => panic!("expected expr stmt"),
            };
            if let ExprKind::Match { arms, .. } = &match_expr.kind {
                if let PatternKind::Record {
                    ref name,
                    ref fields,
                    has_rest,
                } = arms[0].pattern.kind
                {
                    assert_eq!(*name, Some(intern::intern("User")));
                    assert_eq!(fields.len(), 1);
                    assert!(has_rest);
                } else {
                    panic!("expected record pattern with rest");
                }
            }
        }
    }

    #[test]
    fn test_loop_zero_bindings() {
        // loop { body } with no bindings
        let prog = parse(
            r#"
            fn main() {
                loop {
                    loop()
                }
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Fn(ref f) = prog.decls[0] {
            let stmts = match &f.body.kind {
                ExprKind::Block(stmts) => stmts,
                _ => panic!("expected block"),
            };
            let expr = match stmts.last().unwrap() {
                Stmt::Expr(e) => e,
                _ => panic!("expected expr stmt"),
            };
            if let ExprKind::Loop { ref bindings, .. } = expr.kind {
                assert!(bindings.is_empty());
            } else {
                panic!("expected loop, got {:?}", expr.kind);
            }
        }
    }

    #[test]
    fn test_error_bad_decl_keyword() {
        let err = parse_err(
            r#"
            123
        "#,
        );
        assert!(err.message.contains("expected declaration"));
    }

    #[test]
    fn test_single_expression_fn() {
        let prog = parse(
            r#"
            fn square(x) = x * x
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Fn(ref f) = prog.decls[0] {
            assert_eq!(f.name, intern::intern("square"));
            // Body should be a binary expression, not a block
            assert!(matches!(&f.body.kind, ExprKind::Binary(_, BinOp::Mul, _)));
        } else {
            panic!("expected fn decl");
        }
    }

    #[test]
    fn test_negative_range_pattern() {
        let prog = parse(
            r#"
            fn classify(n) {
                match n {
                    -10..10 -> "small"
                    _ -> "big"
                }
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        if let Decl::Fn(ref f) = prog.decls[0] {
            let stmts = match &f.body.kind {
                ExprKind::Block(stmts) => stmts,
                _ => panic!("expected block"),
            };
            let match_expr = match stmts.last().unwrap() {
                Stmt::Expr(e) => e,
                _ => panic!("expected expr stmt"),
            };
            if let ExprKind::Match { arms, .. } = &match_expr.kind {
                assert!(matches!(&arms[0].pattern.kind, PatternKind::Range(-10, 10)));
            } else {
                panic!("expected match");
            }
        }
    }
}
