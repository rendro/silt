use crate::ast::*;
use crate::diagnostic::{Code, Diagnostic};
use crate::intern::{self, Symbol};
use crate::lexer::{Lexed, Tok, Token};
use crate::source::Span;

type Result<T> = std::result::Result<T, Diagnostic>;

// ── Doc-comment scanner ──────────────────────────────────────────────
//
// The lexer drops every comment. We want doc comments to attach to the
// following top-level declaration (and to trait / impl methods) so hover,
// completion, and signature-help can surface Markdown documentation.
//
// Approach: scan the raw source once (independent of the lexer) and
// produce a per-source-line map `line -> doc_text`. For every line L
// that starts a decl (after lexer-delivered newline handling, we just
// use the line of the decl's first token as the "decl start line"), we
// look up the doc block whose last-comment-line is `L - 1`
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
pub(crate) struct DocIndex {
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
    pub(crate) fn from_source(source: &str) -> Self {
        let bytes = source.as_bytes();
        let n = bytes.len();

        // First pass: classify each byte as Code / InString / InBlockComment
        // so we correctly identify which `--` sequences are comments and
        // which are inside strings. We record comment spans as Segment
        // entries (see module-level `Segment` type below).
        let mut segments: Vec<Segment> = Vec::new();
        let mut line: usize = 1; // 1-based
        let mut i: usize = 0;

        // Mode stack for string/interp awareness: we only need to know
        // "am I inside any string
        // context" — if so, `--` is content, not a comment.
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum Mode {
            Code,
            InRegular, // inside a "..." string (escape-aware)
            InTriple,  // inside a """...""" string
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
                        segments.push(Segment::LineComment { line, content });
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
                                    let content_lines: Vec<String> =
                                        raw.split('\n').map(|s| s.to_string()).collect();
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
                        if prefix
                            .bytes()
                            .all(|b| b == b' ' || b == b'\t' || b == b'\r')
                        {
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
    pub(crate) fn doc_for_decl_at_line(&self, decl_line: usize) -> Option<String> {
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

// ── Top-level names ──────────────────────────────────────────────────

/// The names a top-level declaration binds, each with the span to report
/// it at and a word for the kind of declaration.
pub(crate) fn top_level_binders(decl: &Decl) -> Vec<(Symbol, Span, &'static str)> {
    match decl {
        // A recovery stub stands in for a broken declaration the user is
        // still fixing; it binds nothing of its own.
        Decl::Fn(f) if f.is_recovery_stub => Vec::new(),
        Decl::Fn(f) => vec![(f.name, f.name_span, "function")],
        Decl::Type(t) => vec![(t.name, t.name_span, "type")],
        Decl::Trait(t) => vec![(t.name, t.name_span, "trait")],
        Decl::TraitImpl(_) => Vec::new(),
        Decl::Import(target, span) => match target {
            ImportTarget::Module(m) => vec![(*m, *span, "import")],
            ImportTarget::Items(_, items) => items
                .iter()
                .map(|(item, item_span)| (*item, *item_span, "import"))
                .collect(),
            ImportTarget::Alias(_, alias, alias_span) => vec![(*alias, *alias_span, "import")],
        },
        Decl::Let { pattern, .. } => {
            let mut names = Vec::new();
            pattern_binders(pattern, &mut names);
            names
                .into_iter()
                .map(|(name, span)| (name, span, "let binding"))
                .collect()
        }
    }
}

/// The names `pattern` binds, with their spans.
pub(crate) fn pattern_binders(pattern: &Pattern, out: &mut Vec<(Symbol, Span)>) {
    match &pattern.kind {
        PatternKind::Ident(name) => out.push((*name, pattern.span)),
        PatternKind::Tuple(parts) | PatternKind::Constructor { args: parts, .. } => {
            for part in parts {
                pattern_binders(part, out);
            }
        }
        PatternKind::Record { fields, .. } | PatternKind::AnonRecord { fields, .. } => {
            for (field, _, sub) in fields {
                match sub {
                    Some(sub) => pattern_binders(sub, out),
                    None => out.push((*field, pattern.span)),
                }
            }
            if let PatternKind::AnonRecord {
                rest: Some((rest, _)),
                ..
            } = &pattern.kind
            {
                out.push((*rest, pattern.span));
            }
        }
        PatternKind::List(elems, rest) => {
            for elem in elems {
                pattern_binders(elem, out);
            }
            if let Some(rest) = rest {
                pattern_binders(rest, out);
            }
        }
        // Every alternative binds the same names.
        PatternKind::Or(alts) => {
            if let Some(first) = alts.first() {
                pattern_binders(first, out);
            }
        }
        PatternKind::Map(entries) => {
            for (_, sub) in entries {
                pattern_binders(sub, out);
            }
        }
        PatternKind::Wildcard
        | PatternKind::Int(_)
        | PatternKind::Float(_)
        | PatternKind::Bool(_)
        | PatternKind::StringLit(..)
        | PatternKind::Range(..)
        | PatternKind::FloatRange(..)
        | PatternKind::Pin(_) => {}
    }
}

/// One error for every top-level name bound a second time. A top-level
/// name is bound once: two imports of the same name, an import and a
/// declaration, or two declarations may not share it, so which one a use
/// refers to never depends on their order. (Shadowing inside a function
/// body is unaffected.) Two items of `import m.{ ... }` lines are left
/// to the resolver: they are one binding when they name one definition
/// (`int.{ ParseError }` and `float.{ ParseError }`).
fn top_level_name_errors(decls: &[Decl]) -> Vec<Diagnostic> {
    let mut first: std::collections::HashMap<Symbol, (Span, &'static str, bool)> =
        std::collections::HashMap::new();
    let mut errors = Vec::new();
    for decl in decls {
        let is_item = matches!(decl, Decl::Import(ImportTarget::Items(..), _));
        for (name, span, kind) in top_level_binders(decl) {
            if intern::resolve(name) == "_" {
                continue;
            }
            match first.get(&name) {
                Some(&(_, _, true)) if is_item => {}
                Some(&(first_span, first_kind, _)) => errors.push(
                    Diagnostic::error(
                        Code::DuplicateTopLevel,
                        span,
                        format!(
                            "'{name}' is bound twice at the top level: by the {first_kind} \
                             and by the {kind} here"
                        ),
                    )
                    .with_label(first_span, format!("first bound here, by the {first_kind}"))
                    .with_note("a top-level name can be bound only once"),
                ),
                None => {
                    first.insert(name, (span, kind, is_item));
                }
            }
        }
    }
    errors
}

// ── Parser ───────────────────────────────────────────────────────────

const MAX_DEPTH: usize = 128;

/// Upper bound on the number of operations one expression tree may chain
/// or nest (see `Parser::expr_height`). Every operator, pipe, call, index,
/// field access, record update and ascription is one operation; a method
/// call `x.f()` is two (a field access and a call).
///
/// `MAX_DEPTH` bounds the parser's own recursion. It does not bound the
/// tree: the operator loop in `parse_expr_bp_inner` builds `a + b + c + ...`,
/// `x |> f |> g`, `f()()()` and `a.b.c` iteratively, one tree level per
/// link, and every later pass (typechecker, compiler, formatter) recurses
/// once per level.
///
/// How the value is chosen. All checking and compiling runs on the
/// `silt-main` thread, which reserves 256 MiB of stack. In a debug build
/// the most expensive pass spends about 40 KiB of stack per tree level
/// (operator, pipe, call and field chains all overflow between 6,000 and
/// 7,000 levels); 80 KiB is the pessimistic figure. Statement blocks,
/// lambdas and loops add up to three levels per nesting step that this
/// count does not see, at most 3 * `MAX_DEPTH` = 384. The worst accepted
/// tree is therefore 2,048 operations over one operand plus 384 levels:
/// about 95 MiB at the measured cost and 190 MiB at the pessimistic one,
/// both inside the reserve.
const MAX_EXPR_OPERATIONS: usize = 2048;

/// The construct whose block follows a header expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeaderKind {
    /// `match <scrutinee> { arms }`
    MatchScrutinee,
    /// `loop name = <initialiser>, ... { body }`
    LoopInit,
}

/// Parser context for "an expression that is followed by a block": a
/// `match` scrutinee or a `loop` binding initialiser. Inside one, a `{`
/// that directly follows an expression may be the block of the header
/// rather than a trailing closure or a record literal.
///
/// The context only applies to tokens at the header's own delimiter depth
/// (`depth`). Inside parentheses, brackets, braces or a string
/// interpolation the `{` cannot be the header's block, so the ordinary
/// expression rules apply there.
#[derive(Debug, Clone, Copy)]
struct BlockHeader {
    kind: HeaderKind,
    /// Delimiter depth (see `Parser::delim_depth`) of the first token of
    /// the header expression.
    depth: i32,
    /// True while the right operand of a `|>` in the header is parsed.
    in_pipe_rhs: bool,
}

/// How a comma-separated list ends (see `Parser::comma_list`).
#[derive(Clone, Copy)]
enum ListEnd<'t> {
    /// At its closing delimiter, which `comma_list` consumes: `)`, `]`
    /// or `}`.
    Close(&'t Token),
    /// In front of the token that follows the list, which is left for
    /// the caller: the `->` after closure parameters, the `{` after
    /// `loop` bindings.
    Before(&'t Token),
    /// As `Before`, for a list that may also end with its last element:
    /// the `where` clauses of a function, whose body `{` is missing in a
    /// trait's method signature.
    BeforeOrNothing(&'t Token),
}

impl<'t> ListEnd<'t> {
    fn token(self) -> &'t Token {
        match self {
            ListEnd::Close(token) | ListEnd::Before(token) | ListEnd::BeforeOrNothing(token) => {
                token
            }
        }
    }
}

pub struct Parser<'src> {
    tokens: Vec<Tok>,
    /// The text the tokens came from, for the line numbers some messages
    /// name and for doc comments. Borrowed: the source map that holds the
    /// file owns the text and its line table.
    source: &'src str,
    /// For each token, the number of delimiters that are open before it:
    /// `(`, `[`, `{`, `#{`, `#[` and the start of a string interpolation
    /// open one, their closers close one. A closer has the depth of the
    /// tokens it encloses. Derived from the token positions alone, so
    /// backtracking (`restore`) cannot put it out of step.
    delim_depth: Vec<i32>,
    pos: usize,
    /// The innermost "expression followed by a block" header being
    /// parsed, if any.
    header: Option<BlockHeader>,
    /// Operations on the longest path of the expression that is being
    /// parsed: the height of its tallest operand plus one per operation
    /// already chained onto it. A bare operand is 0; `a + b + c` and
    /// `x.f` count one per operator or access. `parse_expr_bp` saves and
    /// resets it on entry and folds the finished expression's height,
    /// plus one for the nesting, into the enclosing one on exit, so a
    /// nested expression (a call argument, a list element, a
    /// parenthesised group) counts one more than its operations: the
    /// count errs on the high side. Checked against
    /// `MAX_EXPR_OPERATIONS` by `check_expr_height`.
    expr_height: usize,
    errors: Vec<Diagnostic>,
    depth: usize,
    /// Depth guard for recovery-stub generation. When recovery fires inside
    /// an already-stubbed declaration (e.g., two back-to-back malformed
    /// `fn` declarations where the second is encountered while still
    /// recovering from the first), we must not recursively emit another
    /// stub and call ourselves again. Incremented on entry to the recovery
    /// path, checked on re-entry.
    in_fn_recovery: bool,
    /// Optional doc-comment index. When `Some` (see `with_docs`), the
    /// parser attaches preceding doc comments to each top-level decl and
    /// each trait / impl method. When `None`, all `doc` fields are left
    /// as `None`. The LSP builds the index; the paths that only compile
    /// don't bother.
    doc_index: Option<DocIndex>,
    /// Name of the trait whose body the parser is currently inside.
    /// Used by `Self::Item` projection sugar to fill in the implicit
    /// trait-name. `None` outside a trait/impl body. The parser sets
    /// this on entry to `parse_trait_or_impl`'s body and restores it on
    /// exit so nested-but-illegal forms (parser doesn't allow nested
    /// traits, so this is purely defensive) cannot leak between siblings.
    current_trait_name: Option<Symbol>,
    /// What a top-level item is called in the same-line error: a
    /// "declaration" in a file, a "statement" in the REPL, whose entries
    /// are statements (see `parse_cell`).
    top_level_item: &'static str,
    /// The offset each line of the source after the first starts at, in
    /// order: `line_of` finds a line without reading the source again.
    line_starts: Vec<usize>,
}

/// Delimiter depth before each token; see `Parser::delim_depth`.
fn delimiter_depths(tokens: &[Tok]) -> Vec<i32> {
    let mut depths = Vec::with_capacity(tokens.len());
    let mut depth: i32 = 0;
    for tok in tokens {
        match tok.kind {
            Token::LParen
            | Token::LBracket
            | Token::LBrace
            | Token::HashBrace
            | Token::HashBracket
            | Token::StringStart(_) => {
                depths.push(depth);
                depth += 1;
            }
            Token::RParen | Token::RBracket | Token::RBrace | Token::StringEnd(_) => {
                depths.push(depth);
                depth -= 1;
            }
            _ => depths.push(depth),
        }
    }
    depths
}

impl<'src> Parser<'src> {
    /// A parser for `lexed`, the tokens of `source`.
    pub fn new(lexed: Lexed, source: &'src str) -> Self {
        let tokens = lexed.tokens;
        let delim_depth = delimiter_depths(&tokens);
        Self {
            tokens,
            delim_depth,
            source,
            pos: 0,
            header: None,
            expr_height: 0,
            errors: Vec::new(),
            depth: 0,
            in_fn_recovery: false,
            doc_index: None,
            current_trait_name: None,
            top_level_item: "declaration",
            line_starts: source
                .bytes()
                .enumerate()
                .filter(|(_, b)| *b == b'\n')
                .map(|(at, _)| at + 1)
                .collect(),
        }
    }

    /// Also attach doc comments: top-level decls (and trait / impl
    /// methods) get their `doc` field from the adjacent `--` / `{- -}`
    /// comments of the source.
    pub fn with_docs(mut self) -> Self {
        self.doc_index = Some(DocIndex::from_source(self.source));
        self
    }

    /// Parse a REPL entry. One that starts with a declaration keyword is
    /// declarations, parsed as a file is; any other is statements, parsed
    /// as a function body is, and given as the body of a function named
    /// `wrapper` (a name no program can write), whose value the REPL
    /// shows. Either way the entry's top-level items are statements, so
    /// two on one line get "each statement must start on its own line".
    pub fn parse_cell(&mut self, wrapper: Symbol) -> (Program, Vec<Diagnostic>) {
        self.top_level_item = "statement";
        self.skip_nl();
        if matches!(
            self.peek(),
            Token::Fn
                | Token::Type
                | Token::Trait
                | Token::Pub
                | Token::Import
                | Token::Let
                | Token::Mod
        ) {
            return self.parse_program_recovering();
        }
        let start = self.span();
        let stmts = match self.parse_stmt_list(&Token::Eof) {
            Ok(stmts) => stmts,
            Err(e) => return (Program { decls: Vec::new() }, vec![e]),
        };
        let span = self.close(start);
        let body = Expr::new(ExprKind::Block(stmts), span);
        let wrapper = FnDecl {
            name: wrapper,
            params: Vec::new(),
            return_type: None,
            where_clauses: Vec::new(),
            body,
            is_pub: false,
            span,
            name_span: span,
            is_recovery_stub: false,
            is_signature_only: false,
            doc: None,
        };
        (
            Program {
                decls: vec![Decl::Fn(wrapper)],
            },
            Vec::new(),
        )
    }

    /// Delimiter depth of the token at `index` (see `delim_depth`).
    fn delim_depth_at(&self, index: usize) -> i32 {
        self.delim_depth.get(index).copied().unwrap_or(0)
    }

    /// The header context that governs the current token: the innermost
    /// header, provided the current token sits at that header's own
    /// delimiter depth. `None` outside any header and inside any
    /// delimiters nested in one.
    fn header_here(&self) -> Option<BlockHeader> {
        self.header
            .filter(|h| h.depth == self.delim_depth_at(self.pos))
    }

    /// Parse one header expression (a `match` scrutinee or a `loop`
    /// binding initialiser) with the header context set. The previous
    /// context is put back on success and on failure, so a parse error in
    /// a header cannot leak the context into the rest of the file.
    fn parse_header_expr(&mut self, kind: HeaderKind) -> Result<Expr> {
        self.skip_nl();
        let depth = self.delim_depth_at(self.pos);
        let prev = self.header.replace(BlockHeader {
            kind,
            depth,
            in_pipe_rhs: false,
        });
        let result = self.parse_expr();
        self.header = prev;
        result
    }

    /// Fail when the expression under construction, which starts at
    /// `start`, chains or nests more than `MAX_EXPR_OPERATIONS`
    /// operations. The error points at the start of the expression,
    /// because the whole expression is what has to be split up.
    fn check_expr_height(&self, start: Span) -> Result<()> {
        if self.expr_height > MAX_EXPR_OPERATIONS {
            return Err(Diagnostic::error(
                Code::NestingTooDeep,
                start,
                format!(
                    "expression is too deep: it is more than {MAX_EXPR_OPERATIONS} levels \
                     deep (each operator, pipe, call or field access in a chain adds a \
                     level, a method call `x.f()` adds two, and so does each enclosing \
                     bracket, call, list, string interpolation or `match`); split it up \
                     with intermediate `let` bindings"
                ),
            ));
        }
        Ok(())
    }

    /// Look up a doc comment for a decl whose first-token span is `span`.
    /// Returns `None` when no doc-index is attached or when no adjacent
    /// comment block precedes the decl line.
    fn doc_for_span(&self, span: Span) -> Option<String> {
        self.doc_index
            .as_ref()
            .and_then(|idx| idx.doc_for_decl_at_line(self.line_of(span) as usize))
    }

    // ── helpers ──────────────────────────────────────────────────────

    fn span(&self) -> Span {
        self.tokens[self.pos].span
    }

    /// The 1-based line `span` starts on, for messages that name a line.
    fn line_of(&self, span: Span) -> u32 {
        let at = span.start_offset().min(self.source.len());
        self.line_starts.partition_point(|start| *start <= at) as u32 + 1
    }

    /// End of the last token consumed: the token before `pos`, newlines
    /// skipped (a newline token sits where the next line's first token
    /// starts, so it says nothing about where the previous one ended).
    fn prev_end(&self) -> u32 {
        self.tokens[..self.pos]
            .iter()
            .rev()
            .find(|tok| !matches!(tok.kind, Token::Newline))
            .map_or(0, |tok| tok.span.end)
    }

    /// The extent of a construct that starts at `start` and whose last
    /// token is the one just consumed.
    fn close(&self, start: Span) -> Span {
        Span {
            end: self.prev_end().max(start.end),
            ..start
        }
    }

    fn mk_expr(&mut self, kind: ExprKind, start: Span) -> Expr {
        Expr::new(kind, self.close(start))
    }

    fn mk_pattern(&mut self, kind: PatternKind, start: Span) -> Pattern {
        Pattern::new(kind, self.close(start))
    }

    fn mk_type(&mut self, kind: TypeExprKind, start: Span) -> TypeExpr {
        TypeExpr::new(kind, self.close(start))
    }

    fn peek(&self) -> &Token {
        &self.tokens[self.pos].kind
    }

    /// The token `n` places after the current one, if any.
    fn peek_at(&self, n: usize) -> Option<&Token> {
        self.tokens.get(self.pos + n).map(|t| &t.kind)
    }

    fn at(&self, tok: &Token) -> bool {
        std::mem::discriminant(self.peek()) == std::mem::discriminant(tok)
    }

    fn at_newline(&self) -> bool {
        matches!(self.peek(), Token::Newline)
    }

    fn advance(&mut self) -> Tok {
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

    /// Undo `skip_nl` (and the newline skipping of `peek_skip_nl`): step
    /// back to just after the last real token, so the newline that ends a
    /// body-less declaration is seen by the same-line check.
    fn unskip_nl(&mut self) {
        while self.pos > 0 && matches!(self.tokens[self.pos - 1].kind, Token::Newline) {
            self.pos -= 1;
        }
    }

    /// Returns true if there is a newline token right at self.pos
    /// (i.e., between the previous real token and the next real token).
    fn has_newline_before(&self) -> bool {
        matches!(
            self.tokens.get(self.pos),
            Some(Tok {
                kind: Token::Newline,
                ..
            })
        )
    }

    /// Round-93 hint guard: true when the current token is a `/` that
    /// is part of an adjacent `//` pair (no gap, same line) — the shape
    /// of a C-style `//` line comment. silt comments are `--`, so `//`
    /// always lexes as two division tokens and dies with a bare
    /// "expected expression/declaration, found /". Two error positions
    /// reach this guard:
    ///   * line-start comment (`// hello`): the error is at the FIRST
    ///     slash, with the second immediately following;
    ///   * trailing comment (`let a = 1 // hello`): the first slash is
    ///     consumed as division and the error is at the SECOND slash,
    ///     with the first immediately preceding.
    /// The adjacency check (byte offsets exactly 1 apart) keeps
    /// spaced-out division like `a / / b` — already an error, but not
    /// a comment attempt — on the generic message, mirroring how the
    /// G1 foreign-keyword hints only fire on the precise mistake shape.
    fn at_double_slash(&self) -> bool {
        let Some(Tok {
            kind: Token::Slash,
            span: cur,
            ..
        }) = self.tokens.get(self.pos)
        else {
            return false;
        };
        let adjacent = |a: &Span, b: &Span| b.start == a.start + 1;
        if matches!(
            self.tokens.get(self.pos + 1),
            Some(Tok { kind: Token::Slash, span: next, .. }) if adjacent(cur, next)
        ) {
            return true;
        }
        self.pos > 0
            && matches!(
                self.tokens.get(self.pos - 1),
                Some(Tok { kind: Token::Slash, span: prev, .. }) if adjacent(prev, cur)
            )
    }

    /// The user-facing message for `at_double_slash` sites. One string,
    /// two emission points (declaration level and expression level).
    const DOUBLE_SLASH_HINT: &'static str =
        "silt line comments use '--', not '//' (block comments are '{- ... -}')";

    fn expect(&mut self, expected: &Token) -> Result<Tok> {
        self.skip_nl();
        if self.at(expected) {
            Ok(self.advance())
        } else {
            Err(Diagnostic::error(
                Code::ExpectedToken,
                self.span(),
                format!("expected {expected}, found {}", self.peek()),
            ))
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
            _ => Err(Diagnostic::error(
                Code::ExpectedIdentifier,
                self.span(),
                format!("expected identifier, found {}", self.peek()),
            )),
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

    /// Build an "unclosed delimiter" error for a construct that does not
    /// use commas internally (block expressions).
    fn delim_unclosed_err_no_comma(
        &self,
        construct: &str,
        closer: char,
        opener_span: Span,
    ) -> Diagnostic {
        Diagnostic::error(
            Code::UnclosedDelimiter,
            self.span(),
            format!(
                "expected '{closer}' to close {construct}, found {}",
                self.peek()
            ),
        )
        .with_label(opener_span, format!("the {construct} starts here"))
    }

    /// True if the current token is a closing delimiter that is NOT the
    /// one we expect — i.e., we're almost certainly inside a still-open
    /// enclosing delimited form.
    fn at_foreign_closer(&self, our_closer: &Token) -> bool {
        matches!(self.peek(), Token::RBrace | Token::RBracket | Token::RParen)
            && std::mem::discriminant(self.peek()) != std::mem::discriminant(our_closer)
    }

    /// The one parser of comma-separated lists: elements parsed by `elem`
    /// and separated by commas, up to `end`. Line breaks may stand
    /// anywhere between elements, and a comma may follow the last one.
    ///
    /// `what` names the list in the error for a list that is not closed
    /// ("expected ')' or ',' to continue function call argument list"),
    /// which is reported where an element or the end of the list should
    /// stand and labels `open`, the token that opened the list (already
    /// consumed by the caller). It is given when the input ends, when a
    /// closing delimiter of an enclosing form or a `fn` declaration comes
    /// instead of an element, and when an element is followed by neither
    /// a comma nor the end of the list.
    fn comma_list<T>(
        &mut self,
        what: &str,
        open: Span,
        end: ListEnd,
        mut elem: impl FnMut(&mut Self) -> Result<T>,
    ) -> Result<Vec<T>> {
        let close = end.token();
        let mut items = Vec::new();
        loop {
            self.skip_nl();
            if self.at(close) {
                break;
            }
            if self.at(&Token::Eof) || self.at_foreign_closer(close) || self.at_fn_decl() {
                return Err(self.unclosed_list_err(what, open, close));
            }
            items.push(elem(self)?);
            self.skip_nl();
            if self.at(&Token::Comma) {
                self.advance();
            } else if self.at(close) {
                break;
            } else if matches!(end, ListEnd::BeforeOrNothing(_)) {
                return Ok(items);
            } else {
                return Err(self.unclosed_list_err(what, open, close));
            }
        }
        if matches!(end, ListEnd::Close(_)) {
            self.advance();
        }
        Ok(items)
    }

    /// The error for a comma-separated list that is not closed; see
    /// `comma_list`.
    fn unclosed_list_err(&self, what: &str, open: Span, close: &Token) -> Diagnostic {
        Diagnostic::error(
            Code::UnclosedDelimiter,
            self.span(),
            format!(
                "expected '{close}' or ',' to continue {what}, found {}",
                self.peek()
            ),
        )
        .with_label(open, format!("the {what} starts here"))
    }

    /// True at a `fn` that starts a declaration (`fn name`), which no
    /// list element does: the list before it was not closed. `fn(` is
    /// left to the element parser, which has a hint for it in a type.
    fn at_fn_decl(&self) -> bool {
        self.at(&Token::Fn) && !matches!(self.peek_at(1), Some(Token::LParen))
    }

    /// The error for a function type written `(A -> B)`, at its `->`,
    /// with the quick fix that rewrites it to `Fn(A) -> B` when the rest
    /// is a type and a `)`. `start` is the `(`; `param` is `A`.
    fn arrow_fn_type_error(&mut self, start: Span, param: &TypeExpr) -> Diagnostic {
        let err = Diagnostic::error(
            Code::UnclosedDelimiter,
            self.span(),
            format!(
                "expected ')' or ',' to continue tuple type, found {}",
                self.peek()
            ),
        );
        self.advance();
        let Ok(ret) = self.parse_type_expr() else {
            return err;
        };
        if !self.at(&Token::RParen) {
            return err;
        }
        let close = self.span();
        let text = |span: Span| &self.source[span.start_offset()..span.end_offset()];
        let replacement = format!("Fn({}) -> {}", text(param.span), text(ret.span));
        err.with_fix(
            "Change `(a -> b)` to `Fn(a) -> b`",
            vec![(start.to(close), replacement)],
        )
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
            if let Some(err) = self.same_line_decl_err() {
                return Err(err);
            }
            self.skip_nl();
        }
        if let Some(err) = top_level_name_errors(&decls).into_iter().next() {
            return Err(err);
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
    pub fn parse_program_recovering(&mut self) -> (Program, Vec<Diagnostic>) {
        let mut decls = Vec::new();
        self.skip_nl();
        while !self.at(&Token::Eof) {
            // Special-case `fn` and `pub fn` declarations so we can salvage
            // partial state on failure.
            if self.at(&Token::Fn) {
                match self.parse_fn_decl_recovering() {
                    Ok((decl, None)) => {
                        decls.push(Decl::Fn(decl));
                        self.recover_same_line_decl();
                    }
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
                            decl.span = pub_span.to(decl.span);
                            if pub_doc.is_some() {
                                decl.doc = pub_doc;
                            }
                            decls.push(Decl::Fn(decl));
                            self.recover_same_line_decl();
                        }
                        Ok((mut stub, Some(err))) => {
                            stub.is_pub = true;
                            stub.span = pub_span.to(stub.span);
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
                Ok(decl) => {
                    decls.push(decl);
                    self.recover_same_line_decl();
                }
                Err(e) => {
                    self.errors.push(e);
                    self.synchronize();
                }
            }
            self.skip_nl();
        }
        self.errors.extend(top_level_name_errors(&decls));
        (Program { decls }, std::mem::take(&mut self.errors))
    }

    /// The recovering counterpart of the same-line check in
    /// `parse_program`: record the error and parse the next declaration
    /// where it starts.
    fn recover_same_line_decl(&mut self) {
        if let Some(err) = self.same_line_decl_err() {
            self.errors.push(err);
        }
    }

    /// The same-line check after a top-level declaration. Only a token
    /// that starts a declaration is a second declaration on the line;
    /// any other token (`with`, `5`, `!`, ...) is left to the plain
    /// "expected declaration" error the next `parse_decl` reports.
    fn same_line_decl_err(&self) -> Option<Diagnostic> {
        let starts_decl = matches!(
            self.peek(),
            Token::Fn | Token::Type | Token::Trait | Token::Pub | Token::Import | Token::Let
        );
        let after_foreign_keyword = matches!(
            self.pos.checked_sub(1).and_then(|i| self.tokens.get(i)),
            Some(Tok { kind: Token::Ident(prev), .. }) if Self::foreign_keyword_hint(&intern::resolve(*prev)).is_some()
        );
        (starts_decl || after_foreign_keyword && Self::starts_statement(self.peek()))
            .then(|| self.same_line_err(self.top_level_item))
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
                        f.span = span.to(f.span);
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
                        t.span = span.to(t.span);
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
                                name_span,
                                ..
                            } => Ok(Decl::Let {
                                pattern,
                                ty,
                                value,
                                is_pub: true,
                                span: self.close(span),
                                name_span,
                                doc: pub_doc.or(doc),
                            }),
                            _ => unreachable!("parse_let_decl always returns Decl::Let"),
                        }
                    }
                    Token::Trait => match self.parse_trait_or_impl()? {
                        Decl::Trait(mut t) => {
                            t.is_pub = true;
                            t.span = span.to(t.span);
                            if pub_doc.is_some() {
                                t.doc = pub_doc;
                            }
                            Ok(Decl::Trait(t))
                        }
                        _ => Err(Diagnostic::error(
                            Code::ExpectedDeclaration,
                            span,
                            "an impl is not exported: `pub` goes on the trait declaration, \
                             and impls apply wherever the type and the trait are visible",
                        )),
                    },
                    _ => Err(Diagnostic::error(
                        Code::ExpectedDeclaration,
                        self.span(),
                        "expected fn, type, trait, or let after pub",
                    )),
                }
            }
            Token::Fn => Ok(Decl::Fn(self.parse_fn_decl()?)),
            Token::Type => Ok(Decl::Type(self.parse_type_decl()?)),
            Token::Trait => self.parse_trait_or_impl(),
            Token::Import => self.parse_import(),
            Token::Let => self.parse_let_decl(),
            _ => {
                if self.at_double_slash() {
                    return Err(Diagnostic::error(
                        Code::UnsupportedSyntax,
                        self.span(),
                        Self::DOUBLE_SLASH_HINT,
                    ));
                }
                Err(Diagnostic::error(
                    Code::ExpectedDeclaration,
                    self.span(),
                    format!("expected declaration, found {}", self.peek()),
                ))
            }
        }
    }

    fn parse_fn_decl(&mut self) -> Result<FnDecl> {
        let span = self.span();
        let doc = self.doc_for_span(span);
        self.expect(&Token::Fn)?;
        let (name, name_span) = self.expect_ident()?;
        let params = self.parse_fn_params()?;

        let return_type = if self.peek_skip_nl() == &Token::Arrow {
            self.advance();
            Some(self.parse_type_expr()?)
        } else {
            None
        };

        let where_clauses = self.parse_where_clauses_opt()?;

        self.skip_nl();
        let (body, is_signature_only) = if self.at(&Token::LBrace) {
            (self.parse_block()?, false)
        } else {
            self.unskip_nl();
            // Abstract method — no body (e.g. trait method declarations).
            // The Unit placeholder keeps the AST shape uniform; the
            // is_signature_only flag is the authoritative signal.
            (self.mk_expr(ExprKind::Unit, span), true)
        };

        Ok(FnDecl {
            name,
            params,
            return_type,
            where_clauses,
            body,
            is_pub: false,
            span: self.close(span),
            name_span,
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
    fn parse_fn_decl_recovering(&mut self) -> Result<(FnDecl, Option<Diagnostic>)> {
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
        let (name, name_span) = match self.expect_ident() {
            Ok((n, s)) => (n, s),
            Err(e) => return Err(e),
        };

        // From here on: errors can produce a stub.
        self.in_fn_recovery = true;
        let result = self.parse_fn_decl_tail(name, name_span, span, doc);
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
        name_span: Span,
        span: Span,
        doc: Option<String>,
    ) -> std::result::Result<FnDecl, Box<(FnDecl, Diagnostic)>> {
        // Try to parse params. On failure, emit a stub with empty params.
        let params = match self.parse_fn_params() {
            Ok(p) => p,
            Err(e) => {
                return Err(Box::new((
                    self.make_recovery_stub(name, name_span, Vec::new(), None, span, doc.clone()),
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
                        self.make_recovery_stub(name, name_span, params, None, span, doc.clone()),
                        e,
                    )));
                }
            }
        } else {
            None
        };

        // Try where clauses.
        let where_clauses = match self.parse_where_clauses_opt() {
            Ok(clauses) => clauses,
            Err(e) => {
                return Err(Box::new((
                    self.make_recovery_stub(
                        name,
                        name_span,
                        params,
                        return_type,
                        span,
                        doc.clone(),
                    ),
                    e,
                )));
            }
        };

        self.skip_nl();
        // Body. On failure, emit a stub that preserves the header.
        let (body, is_signature_only) = if self.at(&Token::LBrace) {
            match self.parse_block() {
                Ok(b) => (b, false),
                Err(err) => {
                    return Err(Box::new((
                        self.make_recovery_stub(
                            name,
                            name_span,
                            params,
                            return_type,
                            span,
                            doc.clone(),
                        ),
                        err,
                    )));
                }
            }
        } else {
            self.unskip_nl();
            // Abstract method — no body.
            (self.mk_expr(ExprKind::Unit, span), true)
        };

        Ok(FnDecl {
            name,
            params,
            return_type,
            where_clauses,
            body,
            is_pub: false,
            span: self.close(span),
            name_span,
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
        name_span: Span,
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
            body: Expr::new(ExprKind::Block(Vec::new()), self.close(span)),
            is_pub: false,
            span: self.close(span),
            name_span,
            is_recovery_stub: true,
            is_signature_only: false,
            doc,
        }
    }

    fn parse_fn_params(&mut self) -> Result<Vec<Param>> {
        let open = self.expect(&Token::LParen)?.span;
        let mut first_type_param_span: Option<Span> = None;
        self.comma_list(
            "function parameter list",
            open,
            ListEnd::Close(&Token::RParen),
            |p| {
                if p.at(&Token::Type) {
                    let type_kw_span = p.span();
                    p.advance();
                    let pattern = p.parse_simple_param_pattern()?;
                    if p.peek_skip_nl() == &Token::Colon {
                        return Err(Diagnostic::error(
                            Code::InvalidDeclaration,
                            p.span(),
                            "'type' parameter cannot carry a type annotation; write `type a`",
                        ));
                    }
                    if first_type_param_span.is_none() {
                        first_type_param_span = Some(type_kw_span);
                    }
                    return Ok(Param {
                        kind: ParamKind::Type,
                        pattern,
                        ty: None,
                    });
                }
                if let Some(type_span) = first_type_param_span {
                    // Point at the MISPLACED `type` keyword rather than
                    // the innocent data param that follows. The reader
                    // then sees the arrow at the thing that needs to
                    // move, not at the thing sitting in a legal place.
                    return Err(Diagnostic::error(
                        Code::InvalidDeclaration,
                        type_span,
                        "'type' parameters must come after all data parameters; move the `type` param to the end of the parameter list",
                    ));
                }
                p.parse_data_param()
            },
        )
    }

    /// A data parameter of a named function or a closure: `pattern` or
    /// `pattern: Type`.
    fn parse_data_param(&mut self) -> Result<Param> {
        let pattern = self.parse_param_pattern()?;
        let ty = if self.peek_skip_nl() == &Token::Colon {
            self.advance();
            self.skip_nl();
            Some(self.parse_type_expr()?)
        } else {
            None
        };
        Ok(Param {
            kind: ParamKind::Data,
            pattern,
            ty,
        })
    }

    /// A data parameter of a named function, trait method or closure: a
    /// name or a destructuring pattern (the type annotation is parsed
    /// by the caller). A token that cannot start a pattern is reported
    /// as a missing parameter name.
    fn parse_param_pattern(&mut self) -> Result<Pattern> {
        self.skip_nl();
        let start = self.pos;
        self.parse_pattern().map_err(|err| {
            if self.pos == start && err.code == Code::ExpectedPattern {
                Diagnostic::error(
                    Code::ExpectedIdentifier,
                    err.span,
                    format!("expected parameter name, found {}", self.peek()),
                )
            } else {
                err
            }
        })
    }

    fn parse_simple_param_pattern(&mut self) -> Result<Pattern> {
        self.skip_nl();
        let start = self.span();
        match self.peek().clone() {
            Token::Ident(name) => {
                self.advance();
                Ok(self.mk_pattern(PatternKind::Ident(name), start))
            }
            _ => Err(Diagnostic::error(
                Code::ExpectedIdentifier,
                self.span(),
                format!("expected parameter name, found {}", self.peek()),
            )),
        }
    }

    fn parse_type_decl(&mut self) -> Result<TypeDecl> {
        let span = self.span();
        let doc = self.doc_for_span(span);
        self.expect(&Token::Type)?;
        let (name, name_span) = self.expect_ident()?;

        // Optional type parameters: type Result(a, e) { ... } or type Pair(a) = (a, a)
        let params = if self.peek_skip_nl() == &Token::LParen {
            let open = self.advance().span;
            self.comma_list(
                "type parameter list",
                open,
                ListEnd::Close(&Token::RParen),
                |p| Ok(p.expect_ident()?.0),
            )?
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
            let body = TypeBody::Alias(target);
            check_type_decl_names(name, name_span, &body)?;
            return Ok(TypeDecl {
                name,
                params,
                body,
                is_pub: false,
                span: self.close(span),
                name_span,
                doc,
            });
        }

        let open = self.expect(&Token::LBrace)?.span;
        self.skip_nl();

        // Determine if this is an enum or record by peeking at the first field.
        // Record fields look like `name: Type`, enum variants look like `Name` or `Name(Type)`.
        let body = if self.is_record_body() {
            self.parse_record_body(open)?
        } else {
            self.parse_enum_body(open)?
        };
        check_type_decl_names(name, name_span, &body)?;

        Ok(TypeDecl {
            name,
            params,
            body,
            is_pub: false,
            span: self.close(span),
            name_span,
            doc,
        })
    }

    fn is_record_body(&self) -> bool {
        // Look ahead: if we see `ident :` it's a record. If we see `Ident(` or `Ident,` or `Ident }` it's enum.
        // Record field names start lowercase, enum variant names start uppercase.
        let mut i = self.pos;
        // skip newlines
        while i < self.tokens.len() && matches!(self.tokens[i].kind, Token::Newline) {
            i += 1;
        }
        if let Token::Ident(ref name) = self.tokens[i].kind {
            // lowercase first char → likely record field
            intern::resolve(*name).starts_with(|c: char| c.is_lowercase())
        } else {
            false
        }
    }

    /// The fields of a record type and its closing brace; `open` is the
    /// `{` in front of them.
    fn parse_record_body(&mut self, open: Span) -> Result<TypeBody> {
        let mut first = true;
        let fields = self.comma_list("record type", open, ListEnd::Close(&Token::RBrace), |p| {
            let (name, name_span) = p.expect_ident()?;
            // The body was taken for a record because its first name is
            // lower case. A first name without `:` that looks like an
            // enum variant may be a variant spelled in lower case.
            if std::mem::take(&mut first) && p.peek_skip_nl() != &Token::Colon {
                let text = intern::resolve(name);
                return Err(Diagnostic::error(
                    Code::ExpectedToken,
                    name_span,
                    format!(
                        "expected `:` after record field '{text}'; if '{text}' is meant as an \
                             enum variant, variant names start with an uppercase letter, e.g. `{}`",
                        capitalized(&text)
                    ),
                ));
            }
            p.expect(&Token::Colon)?;
            let ty = p.parse_type_expr()?;
            Ok(RecordField {
                name,
                name_span,
                ty,
            })
        })?;
        Ok(TypeBody::Record(fields))
    }

    /// The variants of an enum and its closing brace; `open` is the `{`
    /// in front of them.
    fn parse_enum_body(&mut self, open: Span) -> Result<TypeBody> {
        let variants = self.comma_list(
            "enum variant list",
            open,
            ListEnd::Close(&Token::RBrace),
            |p| {
                let (name, name_span) = p.expect_ident()?;
                let fields = if p.peek() == &Token::LParen {
                    let open = p.advance().span;
                    p.comma_list(
                        "enum variant field list",
                        open,
                        ListEnd::Close(&Token::RParen),
                        Self::parse_type_expr,
                    )?
                } else {
                    Vec::new()
                };
                Ok(EnumVariant {
                    name,
                    name_span,
                    fields,
                })
            },
        )?;
        Ok(TypeBody::Enum(variants))
    }

    /// Parse an optional `where` clause list. Consumes the `where` token
    /// if present. Supports comma-separated clauses and `+`-separated
    /// multi-trait bounds per clause (`where a: Equal + Hash, b: Show`).
    /// Returns an empty Vec if no `where` token is present.
    ///
    /// The list ends in front of the `{` of the body, or with its last
    /// clause where there is no body (a trait's method signature).
    fn parse_where_clauses_opt(&mut self) -> Result<Vec<WhereClause>> {
        if self.peek_skip_nl() != &Token::Where {
            return Ok(Vec::new());
        }
        let open = self.advance().span;
        let end = ListEnd::BeforeOrNothing(&Token::LBrace);
        let clauses = self.comma_list("where clause", open, end, |p| {
            let (type_param, _) = p.expect_ident()?;
            p.expect(&Token::Colon)?;
            let mut bounds = vec![p.parse_trait_ref()?.bound_on(type_param)];
            // Multi-trait bounds: `where a: Equal + Hash`
            while p.at(&Token::Plus) {
                p.advance();
                bounds.push(p.parse_trait_ref()?.bound_on(type_param));
            }
            Ok(bounds)
        })?;
        if clauses.is_empty() {
            // `where {`: the first clause is missing.
            self.expect_ident()?;
        }
        Ok(clauses.into_iter().flatten().collect())
    }

    /// Parse a name that may carry one module qualifier: `Shape` or
    /// `m.Shape`. Returns the qualifier, the name and the name's span.
    /// `what` names the thing for the error about a second qualifier
    /// (module paths have one segment).
    fn parse_qualified_name(&mut self, what: &str) -> Result<(Option<Qualifier>, Symbol, Span)> {
        let (first, first_span) = self.expect_ident()?;
        if !self.at(&Token::Dot) || !matches!(self.peek_at(1), Some(Token::Ident(_))) {
            return Ok((None, first, first_span));
        }
        self.advance();
        let (name, name_span) = self.expect_ident()?;
        if self.at(&Token::Dot) && matches!(self.peek_at(1), Some(Token::Ident(_))) {
            return Err(Diagnostic::error(
                Code::UnsupportedSyntax,
                self.span(),
                format!(
                    "a {what} has at most one qualifier: '{}.{}.' has more than one segment",
                    intern::resolve(first),
                    intern::resolve(name)
                ),
            ));
        }
        let module = Qualifier {
            name: first,
            span: first_span,
        };
        Ok((Some(module), name, name_span))
    }

    /// Parse a trait reference in a where clause, a supertrait list or
    /// an associated-type bound: a bare or qualified name (`Display`,
    /// `m.Describe`) with an optional argument list (`TryInto(Int)`,
    /// `Convert(a, b)`).
    fn parse_trait_ref(&mut self) -> Result<TraitRef> {
        let (module, name, name_span) = self.parse_qualified_name("trait name")?;
        let args = if self.at(&Token::LParen) {
            self.parse_trait_args()?
        } else {
            Vec::new()
        };
        Ok(TraitRef {
            module,
            name,
            args,
            span: name_span,
            res: None,
        })
    }

    /// The argument list of a trait: `(Int)` in `TryInto(Int)`, `(a, b)`
    /// in `trait Convert(a, b)`.
    fn parse_trait_args(&mut self) -> Result<Vec<TypeExpr>> {
        let open = self.expect(&Token::LParen)?.span;
        self.comma_list(
            "trait argument list",
            open,
            ListEnd::Close(&Token::RParen),
            Self::parse_type_expr,
        )
    }

    fn parse_trait_or_impl(&mut self) -> Result<Decl> {
        let span = self.span();
        let doc = self.doc_for_span(span);
        self.expect(&Token::Trait)?;
        // `trait m.Describe for T` names an imported trait; a declaration's
        // name has no qualifier (checked below, once the form is known).
        let (trait_module, name, name_span) = self.parse_qualified_name("trait name")?;

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
            self.parse_trait_args()?
        } else {
            Vec::new()
        };

        // Parse optional supertrait bounds: `trait Ordered: Equal + Hash { ... }`
        // or parameterized forms `trait Sub(a): Super(a) + Other(Int)`.
        // Disambiguation: `:` after the trait name is unambiguous because impls
        // use `for` and decls use `{` or `fn`.
        let supertraits: Vec<TraitRef> = if self.at(&Token::Colon) {
            self.advance();
            let mut traits = vec![self.parse_trait_ref()?];
            while self.at(&Token::Plus) {
                self.advance();
                traits.push(self.parse_trait_ref()?);
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
            // Reported once the declaration is parsed, so that parsing
            // resumes after it.
            let qualified_name_error = trait_module.map(|module| {
                Diagnostic::error(
                    Code::InvalidDeclaration,
                    module.span,
                    format!(
                        "a trait declaration's name has no qualifier: declare `trait {}` \
                         in its own module",
                        intern::resolve(name)
                    ),
                )
            });
            let mut params: Vec<Symbol> = Vec::new();
            for arg in &trait_args {
                // Round-52 deferred item 2: point the caret at the
                // offending argument's own span, not the enclosing
                // `trait` keyword. Each arg carries its own span now.
                let arg_span = arg.span;
                let TypeExprKind::Named {
                    module: None,
                    name: arg_sym,
                    ..
                } = &arg.kind
                else {
                    return Err(Diagnostic::error(
                        Code::InvalidDeclaration,
                        arg_span,
                        "trait declaration parameters must be lowercase type variables \
                             (e.g. `trait TryInto(b) { ... }`)",
                    ));
                };
                let arg_str = intern::resolve(*arg_sym);
                let first_char = arg_str.chars().next().unwrap_or('A');
                if !first_char.is_lowercase() {
                    return Err(Diagnostic::error(
                        Code::InvalidDeclaration,
                        arg_span,
                        format!("trait parameter '{arg_str}' must be a lowercase type variable"),
                    ));
                }
                if params.contains(arg_sym) {
                    return Err(Diagnostic::error(
                        Code::InvalidDeclaration,
                        arg_span,
                        format!("duplicate type variable '{arg_str}' in trait declaration"),
                    ));
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
            if let Some(error) = qualified_name_error {
                return Err(error);
            }
            Ok(Decl::Trait(TraitDecl {
                name,
                name_span,
                params,
                supertraits,
                param_where_clauses,
                methods,
                assoc_types,
                is_pub: false,
                span: self.close(span),
                doc,
            }))
        } else {
            // Must be `for Type { ... }`  or  `for Type(params...) { ... }`.
            // Supertrait bounds (`trait X: A for Int { ... }`) are not allowed
            // on impls — supertraits live on the trait decl only.
            if !supertraits.is_empty() {
                return Err(Diagnostic::error(
                    Code::InvalidDeclaration,
                    span,
                    "supertrait bounds (`: Trait`) are only allowed on trait \
                     declarations, not on trait impls",
                ));
            }
            self.expect(&Token::Ident(intern::intern("for")))?;
            let target_span = self.span();
            let target_te = self.parse_type_expr()?;

            // Accept only `Named(head)` or `Generic(head, args)` as the
            // impl target. Reject tuple/fn/Unit targets — those have no
            // stable "head symbol" for method_table keying or for the
            // compiler's `TypeName.method_name` qualified-name form.
            // The head-name span: for `for Int` it's `Int`'s span; for
            // `for m.Box(a)` it's `Box`'s span.
            let (target_module, target, target_type_span, target_type_args) = match target_te.kind {
                TypeExprKind::Named {
                    module,
                    name,
                    name_span,
                } => (module, name, name_span, Vec::new()),
                TypeExprKind::Generic {
                    module,
                    name,
                    name_span,
                    args,
                } => (module, name, name_span, args),
                _ => {
                    return Err(Diagnostic::error(
                        Code::InvalidDeclaration,
                        target_span,
                        "trait impl target must be a named type (e.g. `Box` or `Box(a)`)",
                    ));
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
                let TypeExprKind::Named {
                    module: None,
                    name: arg_sym,
                    ..
                } = &arg.kind
                else {
                    return Err(Diagnostic::error(
                        Code::UnsupportedSyntax,
                        target_span,
                        "impl target arguments must be lowercase type variables; \
                                  silt has no trait specialization",
                    ));
                };
                let arg_str = intern::resolve(*arg_sym);
                let first_char = arg_str.chars().next().unwrap_or('A');
                if !first_char.is_lowercase() {
                    return Err(Diagnostic::error(
                        Code::UnsupportedSyntax,
                        target_span,
                        format!(
                            "impl target argument '{arg_str}' must be a lowercase type variable; \
                             silt has no trait specialization"
                        ),
                    ));
                }
                if target_param_names.contains(arg_sym) {
                    return Err(Diagnostic::error(
                        Code::InvalidDeclaration,
                        target_span,
                        format!(
                            "duplicate type variable '{arg_str}' in impl target; \
                             each binder must be distinct"
                        ),
                    ));
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
                trait_module,
                trait_name: name,
                trait_res: None,
                trait_name_span: name_span,
                trait_args,
                target_module,
                target_type: target,
                target_res: None,
                target_type_span,
                target_type_args,
                target_param_names,
                where_clauses,
                methods,
                assoc_type_bindings,
                span: self.close(span),
                is_auto_derived: false,
            }))
        }
    }

    fn parse_import(&mut self) -> Result<Decl> {
        let import_span = self.expect(&Token::Import)?.span;
        let (name, _) = self.expect_ident()?;

        // `.{ ... }` and `as` may continue the import on the next line;
        // anything else there starts the next declaration.
        let saved = self.save();
        self.skip_nl();
        if !self.at(&Token::Dot) && !self.at(&Token::As) {
            self.restore(saved);
        }
        if self.at(&Token::Dot) {
            self.advance();
            let open = self.expect(&Token::LBrace)?.span;
            let items = self.comma_list(
                "selective import list",
                open,
                ListEnd::Close(&Token::RBrace),
                Self::expect_ident,
            )?;
            Ok(Decl::Import(
                ImportTarget::Items(name, items),
                self.close(import_span),
            ))
        } else if self.at(&Token::As) {
            self.advance();
            let (alias, alias_span) = self.expect_ident()?;
            Ok(Decl::Import(
                ImportTarget::Alias(name, alias, alias_span),
                self.close(import_span),
            ))
        } else {
            Ok(Decl::Import(
                ImportTarget::Module(name),
                self.close(import_span),
            ))
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
        let mut bounds: Vec<TraitRef> = Vec::new();
        if self.at(&Token::Colon) {
            self.advance();
            bounds.push(self.parse_trait_ref()?);
            while self.at(&Token::Plus) {
                self.advance();
                bounds.push(self.parse_trait_ref()?);
            }
        }
        if self.at(&Token::Eq) {
            return Err(Diagnostic::error(
                Code::UnsupportedSyntax,
                span,
                "associated-type defaults are not supported in v1",
            )
            .with_help("declare the type abstractly (`type Item`) and bind it in each impl"));
        }
        Ok(crate::ast::AssocTypeDecl {
            name,
            bounds,
            span: self.close(span),
        })
    }

    /// Parse an associated-type binding inside a trait impl body.
    /// Form: `type Item = TypeExpr`.
    fn parse_assoc_type_binding(&mut self) -> Result<crate::ast::AssocTypeBinding> {
        let span = self.span();
        self.expect(&Token::Type)?;
        let (name, _) = self.expect_ident()?;
        self.expect(&Token::Eq)?;
        let ty = self.parse_type_expr()?;
        Ok(crate::ast::AssocTypeBinding {
            name,
            ty,
            span: self.close(span),
        })
    }

    // ── Type expressions ─────────────────────────────────────────────

    fn parse_type_expr(&mut self) -> Result<TypeExpr> {
        // Depth guard mirroring parse_expr_bp / parse_pattern: type-expr
        // recursion (nested generics, Fn types, qualified projections,
        // anon record types) was the one unguarded sibling — deep
        // nesting overflowed even the 256MiB silt-main stack.
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            self.depth -= 1;
            return Err(Diagnostic::error(
                Code::NestingTooDeep,
                self.span(),
                "type nesting exceeds maximum depth",
            ));
        }
        let result = self.parse_type_expr_inner();
        self.depth -= 1;
        result
    }

    /// The rest of a function type after its `Fn`: `(A, B) -> C`.
    fn parse_fn_type_rest(&mut self, start: Span) -> Result<TypeExpr> {
        let open = self.expect(&Token::LParen)?.span;
        let params = self.comma_list(
            "function type parameter list",
            open,
            ListEnd::Close(&Token::RParen),
            Self::parse_type_expr,
        )?;
        self.expect(&Token::Arrow)?;
        let ret = self.parse_type_expr()?;
        Ok(self.mk_type(TypeExprKind::Function(params, Box::new(ret)), start))
    }

    fn parse_type_expr_inner(&mut self) -> Result<TypeExpr> {
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
            let (trait_module, trait_name, _) = self.parse_qualified_name("trait name")?;
            self.skip_nl();
            self.expect(&Token::Gt)?;
            self.expect(&Token::ColonColon)?;
            let (assoc_name, _) = self.expect_ident()?;
            return Ok(self.mk_type(
                TypeExprKind::AssocProj {
                    receiver: Box::new(receiver),
                    trait_module,
                    trait_name,
                    assoc_name,
                },
                start,
            ));
        }
        // Function type: Fn(A, B) -> C
        //
        // Only commit to the function-type form when `Fn` is followed by
        // `(`. Bare `Fn` (e.g. `trait Show for Fn { ... }`) must fall
        // through to the regular Named-type path so `Fn` lands as
        // `TypeExprKind::Named("Fn")` and the trait-impl machinery
        // registers under the canonical `("T", "Fn")` key. Without the
        // peek-ahead, `for Fn { ... }` wedged the parser at `expected (`.
        // Round 71 follow-up canonical-name unification.
        if matches!(self.peek(), Token::Ident(s) if *s == intern::intern("Fn"))
            && self
                .tokens
                .get(self.pos + 1)
                .map(|t| matches!(t.kind, Token::LParen))
                .unwrap_or(false)
        {
            self.advance();
            return self.parse_fn_type_rest(start);
        }
        // `fn(Int) -> Int` in a type: the keyword spelling of `Fn`.
        if self.at(&Token::Fn)
            && matches!(
                self.tokens.get(self.pos + 1),
                Some(Tok {
                    kind: Token::LParen,
                    ..
                })
            )
        {
            self.advance();
            // The type as it is written, behind `Fn` instead of `fn`.
            let rest = self.parse_fn_type_rest(start).ok().and_then(|ty| {
                self.source
                    .get(start.end as usize..ty.span.end as usize)
                    .filter(|rest| !rest.contains('\n'))
            });
            let hint = match rest {
                Some(rest) => format!("`Fn{rest}`"),
                None => "`Fn(...) -> ...`".to_string(),
            };
            return Err(Diagnostic::error(
                Code::ExpectedType,
                start,
                format!(
                    "expected a type, found fn: a function type is written with `Fn`; \
                     did you mean {hint}?"
                ),
            ));
        }
        // Tuple type: (A, B, ...)
        // Parentheses, as in an expression: `()` is the unit type, `(T)`
        // is `T`, and a comma makes a tuple, `(T,)` the tuple of one.
        if self.at(&Token::LParen) {
            self.advance();
            self.skip_nl();
            if self.at(&Token::RParen) {
                self.advance();
                return Ok(self.mk_type(TypeExprKind::Tuple(Vec::new()), start));
            }
            let close = Token::RParen;
            if self.at(&Token::Eof) || self.at_foreign_closer(&close) || self.at_fn_decl() {
                return Err(self.unclosed_list_err("tuple type", start, &close));
            }
            let first = self.parse_type_expr()?;
            if self.at(&Token::Arrow) {
                return Err(self.arrow_fn_type_error(start, &first));
            }
            self.skip_nl();
            if self.at(&Token::RParen) {
                self.advance();
                return Ok(first);
            }
            if !self.at(&Token::Comma) {
                return Err(self.unclosed_list_err("tuple type", start, &close));
            }
            self.advance();
            let mut elems = vec![first];
            elems.extend(self.comma_list(
                "tuple type",
                start,
                ListEnd::Close(&close),
                Self::parse_type_expr,
            )?);
            return Ok(self.mk_type(TypeExprKind::Tuple(elems), start));
        }
        // Anonymous record type: `{name: Type, age: Type}` or open
        // `{name: Type, ...r}`.
        if self.at(&Token::LBrace) {
            self.advance();
            let mut tail: Option<Symbol> = None;
            let mut seen: std::collections::HashSet<Symbol> = std::collections::HashSet::new();
            let fields = self.comma_list(
                "anon record type",
                start,
                ListEnd::Close(&Token::RBrace),
                |p| {
                    p.nothing_after_rest(tail.is_some(), "...", '}', "an anon record type")?;
                    if p.at(&Token::DotDotDot) {
                        p.advance();
                        // Row variable name (e.g. `r` in `...r`). Required.
                        tail = Some(p.expect_ident()?.0);
                        return Ok(None);
                    }
                    let (fname, fname_span) = p.expect_ident()?;
                    p.expect(&Token::Colon)?;
                    p.skip_nl();
                    let fty = p.parse_type_expr()?;
                    if !seen.insert(fname) {
                        return Err(Diagnostic::error(
                            Code::DuplicateField,
                            fname_span,
                            format!("duplicate field '{}' in anon record type", fname),
                        ));
                    }
                    Ok(Some((fname, fty)))
                },
            )?;
            let fields = fields.into_iter().flatten().collect();
            return Ok(self.mk_type(TypeExprKind::AnonRecord { fields, tail }, start));
        }
        let (module, name, name_span) = self.parse_qualified_name("type name")?;
        // `Self::Item` — sugar for `<Self as <enclosing_trait>>::Item`.
        // The trait name is supplied by the parser's enclosing-trait
        // context (set by parse_trait_or_impl on entry to the body).
        // Outside a trait/impl body the projection is an error; it is
        // caught by the typechecker (current_trait_name absent).
        if module.is_none() && name == intern::intern("Self") {
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
                return Ok(self.mk_type(
                    TypeExprKind::AssocProj {
                        receiver: Box::new(recv),
                        trait_module: None,
                        trait_name,
                        assoc_name,
                    },
                    start,
                ));
            }
            return Ok(self.mk_type(TypeExprKind::SelfType, start));
        }
        if self.peek() == &Token::LParen {
            let open = self.advance().span;
            let args = self.comma_list(
                "generic type argument list",
                open,
                ListEnd::Close(&Token::RParen),
                Self::parse_type_expr,
            )?;
            Ok(self.mk_type(
                TypeExprKind::Generic {
                    module,
                    name,
                    name_span,
                    args,
                },
                start,
            ))
        } else {
            Ok(self.mk_type(
                TypeExprKind::Named {
                    module,
                    name,
                    name_span,
                },
                start,
            ))
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
        Ok(self.mk_expr(ExprKind::Block(stmts), span))
    }

    fn parse_stmt_list(&mut self, terminator: &Token) -> Result<Vec<Stmt>> {
        let mut stmts = Vec::new();
        self.skip_nl();
        while !self.at(terminator) && !self.at(&Token::Eof) {
            stmts.push(self.parse_stmt()?);
            if Self::starts_statement(self.peek()) && !self.at_lowercase_record_literal_brace() {
                return Err(self.same_line_err("statement"));
            }
            self.skip_nl();
        }
        Ok(stmts)
    }

    /// True at the `{` of `point { x: 1 }` or `util.pt { x: 1 }`: a
    /// statement that ended at a name followed by `{` on the same line.
    /// That is a record literal with a lowercase type name, not two
    /// statements on one line, so the newline error is skipped and the
    /// name is left to the typechecker's "undefined variable" (and the
    /// lowercase type-name error where a `type point` exists).
    fn at_lowercase_record_literal_brace(&self) -> bool {
        self.at(&Token::LBrace)
            && matches!(
                self.pos.checked_sub(1).and_then(|i| self.tokens.get(i)),
                Some(Tok {
                    kind: Token::Ident(_),
                    ..
                })
            )
    }

    /// True when `tok` could begin a statement or a declaration. Such a
    /// token right after a complete statement, on the same line, is a
    /// second statement on that line (see `same_line_err`). Any other
    /// token there (a stray closer, `,`, `else`, ...) is left to the
    /// error the next parse step reports for it.
    fn starts_statement(tok: &Token) -> bool {
        matches!(
            tok,
            Token::Let
                | Token::When
                | Token::Match
                | Token::Return
                | Token::Loop
                | Token::Fn
                | Token::Type
                | Token::Trait
                | Token::Pub
                | Token::Import
                | Token::Int(_)
                | Token::Float(_)
                | Token::Bool(_)
                | Token::StringLit(..)
                | Token::StringStart(_)
                | Token::Ident(_)
                | Token::Minus
                | Token::Not
                | Token::LParen
                | Token::LBrace
                | Token::LBracket
                | Token::HashBrace
                | Token::HashBracket
        )
    }

    /// Refuses the integer token `n` where no minus sign precedes it and
    /// it is the magnitude 2^63 (see `Token::Int`): the largest Int is
    /// one less.
    fn int_in_range(&self, n: i64) -> Result<()> {
        if n == i64::MIN {
            let span = self.span();
            let digits = &self.source[span.start_offset()..span.end_offset()];
            let kind = match digits.get(..2) {
                Some("0x" | "0X") => "hex",
                Some("0b" | "0B") => "binary",
                _ => "number",
            };
            return Err(Diagnostic::error(
                Code::InvalidNumber,
                span,
                format!("{kind} literal too large"),
            ));
        }
        Ok(())
    }

    /// The error for a token that follows a complete statement (or
    /// top-level declaration) on the same line. Statements are separated
    /// by a newline, so `let a = 1 let b = 2` and `let t = price quantity`
    /// are rejected here instead of being read as two statements.
    fn same_line_err(&self, what: &str) -> Diagnostic {
        // `let r = if x { ... }`: the statement ended at a foreign keyword
        // read as an identifier, so point at the silt equivalent instead.
        if let Some(Tok {
            kind: Token::Ident(prev),
            span: prev_span,
            ..
        }) = self.pos.checked_sub(1).and_then(|i| self.tokens.get(i))
            && let Some(hint) = Self::foreign_keyword_hint(&intern::resolve(*prev))
        {
            return Diagnostic::error(Code::UnsupportedSyntax, *prev_span, hint);
        }
        Diagnostic::error(
            Code::MissingNewline,
            self.span(),
            format!(
                "expected a newline before '{}': each {what} must start on its own line",
                self.peek()
            ),
        )
    }

    /// G1 hint table: messages for C-family control-flow keywords that
    /// silt deliberately lacks. `if`/`while`/`for` lex as ordinary
    /// identifiers, so a user porting code gets a baffling generic parse
    /// error unless we recognize the shape and point at the silt
    /// equivalent. Used by the statement-level guard in `parse_stmt`.
    fn foreign_keyword_hint(text: &str) -> Option<&'static str> {
        match text {
            "if" => Some(
                "silt has no 'if' keyword — use 'match cond { ... }' with a 'true -> ...' and a 'false -> ...' arm",
            ),
            "while" | "for" => Some(
                "silt has no 'while'/'for' keywords — use tail-recursive 'loop' or 'list.each' / 'list.map'",
            ),
            _ => None,
        }
    }

    /// True when `tok` could plausibly start the erroneous construct
    /// following a foreign keyword: an expression-start token (ident,
    /// literal, unary, brace, bracket). Deliberately excludes `(`
    /// because `if(...)` is a syntactically valid call of a variable
    /// named `if`, and excludes Newline/EOF/closers because a bare
    /// identifier reference there is valid silt.
    fn g1_next_starts_expression(tok: &Token) -> bool {
        matches!(
            tok,
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
        )
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
                .map(|t| t.kind.clone())
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
            if let Some(msg) = Self::foreign_keyword_hint(&text) {
                // Fire only when the next token could plausibly start the
                // erroneous construct: an expression-start token (paren,
                // ident, literal, unary, brace).
                if Self::g1_next_starts_expression(&next) {
                    return Err(Diagnostic::error(Code::UnsupportedSyntax, span, msg));
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
                return Err(Diagnostic::error(
                    Code::UnsupportedSyntax,
                    span,
                    format!(
                        "'let' bindings in silt are immutable — rebind with 'let {text} = ...' in a new scope"
                    ),
                ));
            }
        }

        // A statement that starts with `else` is almost always the tail of
        // an `if ... { } else { }` ported from another language: `if`
        // parses as an identifier and the statement ends before `else`.
        if self.at(&Token::Else) {
            return Err(Diagnostic::error(
                Code::UnsupportedSyntax,
                self.span(),
                "'else' only follows a 'when' condition; silt has no 'if' keyword — \
                          for a conditional value use 'match cond { ... }' with a 'true -> ...' and a \
                          'false -> ...' arm",
            ));
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
        // Capture the binding's name-identifier span when the pattern is a
        // bare `Ident` — needed by LSP rename / references / definition
        // (round-71 fix: without this, the `let` keyword span gets used as
        // the edit range and the rename clobbers `let` / `pub`).
        // Destructuring patterns get `None`; rename through them bails.
        let name_span = match &pattern.kind {
            PatternKind::Ident(_) => Some(pattern.span),
            _ => None,
        };
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
            span: self.close(span),
            name_span,
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
            let expr = self.parse_expr()?;
            self.expect(&Token::Else)?;
            let else_body = self.parse_block()?;
            return Ok(Stmt::When {
                pattern,
                expr,
                else_body,
            });
        }

        // Boolean form: when <expr> else { <block> }
        let condition = self.parse_expr()?;
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

    /// Shared tail for the infix-operator arms of the Pratt loop
    /// (round-93 dedup: this exact sequence was copied verbatim across
    /// the pipe / range / binary arms).
    ///
    ///   * `l_bp < min_bp` → restore `saved` (undoing the speculative
    ///     newline skip) and return `Ok(None)`; the caller breaks out
    ///     of the loop with `left` unchanged.
    ///   * otherwise consume the operator token, skip newlines, parse
    ///     the right-hand side at `r_bp`, and return it; the caller
    ///     wraps `left` and the RHS in its own node kind.
    ///
    /// `pipe_rhs` marks the RHS as the right operand of `|>`. Inside a
    /// header (see `BlockHeader`) that is the one place where a trailing
    /// closure is allowed at the header's own depth:
    /// `match xs |> list.any { x -> x > 5 } { true -> ... }`.
    fn parse_infix_rhs(
        &mut self,
        saved: usize,
        min_bp: u8,
        l_bp: u8,
        r_bp: u8,
        pipe_rhs: bool,
    ) -> Result<Option<Expr>> {
        if l_bp < min_bp {
            self.restore(saved);
            return Ok(None);
        }
        self.advance();
        self.skip_nl();
        // The right operand becomes a child of the node the caller
        // builds, and the operator loop counts that node. `parse_expr_bp`
        // adds one level for the operand as a nested expression; take it
        // off again, so an operator chain counts exactly one per operator.
        let left_height = std::mem::replace(&mut self.expr_height, 0);
        let right = if pipe_rhs {
            let prev = self.header;
            if let Some(header) = self.header.as_mut() {
                header.in_pipe_rhs = true;
            }
            let right = self.parse_expr_bp(r_bp);
            self.header = prev;
            right?
        } else {
            self.parse_expr_bp(r_bp)?
        };
        self.expr_height = left_height.max(self.expr_height.saturating_sub(1));
        Ok(Some(right))
    }

    fn parse_expr_bp(&mut self, min_bp: u8) -> Result<Expr> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            self.depth -= 1;
            return Err(Diagnostic::error(
                Code::NestingTooDeep,
                self.span(),
                "expression nesting exceeds maximum depth",
            ));
        }
        // Height accounting (see `expr_height`): start from zero for this
        // expression's own operands, then fold the finished height into
        // the enclosing expression's tally.
        let enclosing_height = std::mem::replace(&mut self.expr_height, 0);
        let mut result = self.parse_expr_bp_inner(min_bp);
        let too_deep = match &result {
            Ok(expr) => self.check_expr_height(expr.span).err(),
            Err(_) => None,
        };
        if let Some(too_deep) = too_deep {
            result = Err(too_deep);
        }
        self.expr_height = enclosing_height.max(self.expr_height + 1);
        self.depth -= 1;
        result
    }

    fn parse_expr_bp_inner(&mut self, min_bp: u8) -> Result<Expr> {
        let mut left = self.parse_unary()?;

        // Every pass through the loop after the first has wrapped `left`
        // in one more node (operator, call, field access, ...), so the
        // tree is one level taller than on the pass before.
        let mut chained = false;
        loop {
            if chained {
                self.expr_height += 1;
                self.check_expr_height(left.span)?;
            }
            chained = true;

            // First, try postfix operators — newline-sensitive.
            // If a newline precedes the token, don't treat it as postfix.
            if !self.has_newline_before() {
                match self.peek() {
                    Token::Question => {
                        // `?` is a tight postfix operator: it binds like a
                        // call, so `int.parse(a)? + int.parse(b)?` unwraps
                        // each operand and `-x?` negates the unwrapped
                        // value. The one exception is a pipe stage: a `?`
                        // that ends a stage (parsed at exactly the pipe's
                        // right binding power) is left for the pipe loop,
                        // so `x |> f |> g?` means `(x |> f |> g)?`.
                        if min_bp == prec::PIPE.1 {
                            break;
                        }
                        let span = left.span;
                        self.advance();
                        left = self.mk_expr(ExprKind::QuestionMark(Box::new(left)), span);
                        continue;
                    }
                    Token::LParen => {
                        if prec::CALL < min_bp {
                            break;
                        }
                        left = self.parse_call_expr(left)?;
                        continue;
                    }
                    Token::LBracket => {
                        if prec::CALL < min_bp {
                            break;
                        }
                        left = self.parse_index_expr(left)?;
                        continue;
                    }
                    Token::LBrace if self.is_trailing_closure() => {
                        if prec::TRAILING_CLOSURE < min_bp {
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

            // Binary operators. + and - are newline-sensitive: `-` is
            // ambiguous with unary negation starting the next statement
            // (`+` is treated the same; silt has no unary plus), so a
            // newline terminates the expression in front of them.
            let binary = BinOp::from_token(self.peek())
                .filter(|op| !(had_newline && matches!(op, BinOp::Add | BinOp::Sub)));
            if let Some(op) = binary {
                let (l_bp, r_bp) = op.binding_power();
                let Some(right) = self.parse_infix_rhs(saved, min_bp, l_bp, r_bp, false)? else {
                    break;
                };
                let span = left.span;
                left = self.mk_expr(ExprKind::Binary(Box::new(left), op, Box::new(right)), span);
                continue;
            }

            match self.peek() {
                // Field access / record update (always allowed across newlines)
                Token::Dot => {
                    if prec::FIELD < min_bp {
                        self.restore(saved);
                        break;
                    }
                    self.advance();
                    self.skip_nl();
                    if self.at(&Token::LBrace) {
                        // Record update: expr.{ field: value }
                        let span = left.span;
                        let fields = self.parse_record_fields()?;
                        left = self.mk_expr(
                            ExprKind::RecordUpdate {
                                expr: Box::new(left),
                                fields,
                            },
                            span,
                        );
                    } else if matches!(self.peek(), Token::Int(_) | Token::Float(_)) {
                        // `t.0`, and `t.0.1`, whose `0.1` is one token.
                        let index = self.span();
                        let index = &self.source[index.start_offset()..index.end_offset()];
                        return Err(Diagnostic::error(
                            Code::UnsupportedSyntax,
                            self.span(),
                            format!(
                                "tuple indexing ('t.{index}') is not supported; \
                                 destructure instead: 'let (a, b) = t'"
                            ),
                        ));
                    } else {
                        let (field, field_span) = self.expect_ident()?;
                        // Round 94: qualified record construction —
                        // `util.Pt { x: 1, y: 2 }` builds the same value as
                        // bare `Pt { x: 1, y: 2 }`, with the qualifier
                        // selecting which module's record type the literal
                        // is checked against. (History: pre-round-93 this
                        // shape silently misparsed as a field access plus a
                        // discarded anonymous-record statement; round 93
                        // made it a parse error as a stopgap; round 94 adds
                        // the real form.)
                        //
                        // Gating mirrors the bare `Pt { ... }` atom exactly
                        // (same-line `{`, match-body suppression via the
                        // record-literal lookahead, trailing-closure
                        // exclusion) so the two spellings accept the same
                        // brace shapes:
                        //   * `util.f { x -> x }` stays a trailing closure;
                        //   * `match util.Pt { _ -> 1 }` keeps its match
                        //     body;
                        //   * a `{` on the next line stays a separate
                        //     statement.
                        if is_constructor(field)
                            && !self.has_newline_before()
                            && self.at(&Token::LBrace)
                            && !self.is_trailing_closure()
                        {
                            if let ExprKind::Ident(module) = &left.kind
                                && (!self.lbrace_may_be_header_block()
                                    || self.scrutinee_lbrace_is_record_literal())
                            {
                                let module = Qualifier {
                                    name: *module,
                                    span: left.span,
                                };
                                let fields = self.parse_record_fields()?;
                                let span = left.span;
                                left = self.mk_expr(
                                    ExprKind::RecordCreate {
                                        module: Some(module),
                                        name: field,
                                        name_span: field_span,
                                        fields,
                                    },
                                    span,
                                );
                                continue;
                            }
                            // Imports take a single module ident, so only a
                            // one-segment qualifier can ever resolve. A
                            // deeper dotted path (`a.b.Pt { ... }`) or an
                            // arbitrary expression qualifier keeps the
                            // round-93 conservative error instead of the old
                            // silent misparse — but only when the brace
                            // contents actually look like record fields
                            // (`ident :`), so blocks and match bodies after
                            // a capitalized field access stay untouched.
                            if self.scrutinee_lbrace_is_record_literal() {
                                let ty = intern::resolve(field);
                                let message = match Self::dotted_path_text(&left) {
                                    Some(path) => format!(
                                        "qualified record construction '{path}.{ty} {{ ... }}' is not supported — \
                                         a record literal takes a single module qualifier ('mod.{ty} {{ ... }}'); \
                                         import the type and use '{ty} {{ ... }}'"
                                    ),
                                    None => format!(
                                        "qualified record construction '{ty} {{ ... }}' after a field access is not supported — \
                                         import the type and use '{ty} {{ ... }}'"
                                    ),
                                };
                                return Err(Diagnostic::error(
                                    Code::UnsupportedSyntax,
                                    field_span,
                                    message,
                                ));
                            }
                        }
                        let span = left.span;
                        left = self.mk_expr(
                            ExprKind::FieldAccess(Box::new(left), field, field_span),
                            span,
                        );
                    }
                    continue;
                }

                // Pipe operator — binds tighter than comparison/boolean operators
                // so `x |> f() == y` parses as `(x |> f()) == y`,
                // but looser than range so `1..10 |> f()` parses as `(1..10) |> f()`
                Token::Pipe => {
                    // Allow trailing closures in the pipe RHS even inside a
                    // match scrutinee (`pipe_rhs: true`). Example:
                    //   match items |> list.any { x -> x > 5 } { true -> … }
                    //                           ^^^^^^^^^^^^^^^  <- trailing closure
                    //                                           ^^^^^^^^^^^^^^^^ <- match body
                    // Braces with nothing after them that could be the
                    // match body are the match body themselves (see
                    // `is_trailing_closure`):
                    //   match items |> list.head { Some(x) -> … }
                    let Some(right) =
                        self.parse_infix_rhs(saved, min_bp, prec::PIPE.0, prec::PIPE.1, true)?
                    else {
                        break;
                    };
                    let span = left.span;
                    left = self.mk_expr(ExprKind::Pipe(Box::new(left), Box::new(right)), span);
                    continue;
                }

                // Range — binds tighter than pipe so `1..10 |> f()` works
                Token::DotDot => {
                    let (l_bp, r_bp) = prec::RANGE;
                    let Some(right) = self.parse_infix_rhs(saved, min_bp, l_bp, r_bp, false)?
                    else {
                        break;
                    };
                    let span = left.span;
                    left = self.mk_expr(ExprKind::Range(Box::new(left), Box::new(right)), span);
                    continue;
                }

                // Type ascription: expr as Type
                Token::As => {
                    if prec::AS < min_bp {
                        self.restore(saved);
                        break;
                    }
                    self.advance();
                    self.skip_nl();
                    let type_expr = self.parse_type_expr()?;
                    let span = left.span;
                    left = self.mk_expr(ExprKind::Ascription(Box::new(left), type_expr), span);
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
                // The smallest Int: its magnitude is no Int, so the minus
                // sign and the digits are one literal.
                if matches!(self.peek(), Token::Int(i64::MIN)) {
                    self.advance();
                    return Ok(self.mk_expr(ExprKind::Int(i64::MIN), span));
                }
                let expr = self.parse_expr_bp(prec::UNARY)?;
                Ok(self.mk_expr(ExprKind::Unary(UnaryOp::Neg, Box::new(expr)), span))
            }
            Token::Not => {
                let span = self.span();
                self.advance();
                let expr = self.parse_expr_bp(prec::UNARY)?;
                Ok(self.mk_expr(ExprKind::Unary(UnaryOp::Not, Box::new(expr)), span))
            }
            _ => self.parse_atom(),
        }
    }

    fn parse_atom(&mut self) -> Result<Expr> {
        self.skip_nl();
        let span = self.span();

        match self.peek().clone() {
            Token::Int(n) => {
                self.int_in_range(n)?;
                self.advance();
                Ok(self.mk_expr(ExprKind::Int(n), span))
            }
            Token::Float(n) => {
                self.advance();
                Ok(self.mk_expr(ExprKind::Float(n), span))
            }
            Token::Bool(b) => {
                self.advance();
                Ok(self.mk_expr(ExprKind::Bool(b), span))
            }
            Token::StringLit(s, triple) => {
                self.advance();
                Ok(self.mk_expr(ExprKind::StringLit(s, triple), span))
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
                    let callee = self.mk_expr(ExprKind::Ident(name), span);
                    let args = self.parse_call_args()?;
                    Ok(self.mk_expr(ExprKind::Call(Box::new(callee), args), span))
                } else if !self.has_newline_before()
                    && self.at(&Token::LBrace)
                    && (!self.lbrace_may_be_header_block()
                        || self.scrutinee_lbrace_is_record_literal())
                    && !self.is_trailing_closure()
                {
                    // Record creation: User { name: "Alice", ... }
                    let fields = self.parse_record_fields()?;
                    Ok(self.mk_expr(
                        ExprKind::RecordCreate {
                            module: None,
                            name,
                            name_span: span,
                            fields,
                        },
                        span,
                    ))
                } else {
                    Ok(self.mk_expr(ExprKind::Ident(name), span))
                }
            }
            Token::Ident(name) => {
                self.advance();
                Ok(self.mk_expr(ExprKind::Ident(name), span))
            }
            Token::LParen => {
                self.advance();
                self.skip_nl();
                // Unit: ()
                if self.at(&Token::RParen) {
                    self.advance();
                    return Ok(self.mk_expr(ExprKind::Unit, span));
                }
                let unclosed = |p: &Self| {
                    p.at(&Token::Eof) || p.at_foreign_closer(&Token::RParen) || p.at_fn_decl()
                };
                if unclosed(self) {
                    return Err(self.delim_unclosed_err_no_comma(
                        "parenthesized expression",
                        ')',
                        span,
                    ));
                }
                let first = self.parse_expr()?;
                self.skip_nl();
                if self.at(&Token::Comma) {
                    // Tuple: (a, b, c)
                    self.advance();
                    let mut elems = vec![first];
                    elems.extend(self.comma_list(
                        "tuple literal",
                        span,
                        ListEnd::Close(&Token::RParen),
                        Self::parse_expr,
                    )?);
                    Ok(self.mk_expr(ExprKind::Tuple(elems), span))
                } else if unclosed(self) {
                    Err(self.delim_unclosed_err_no_comma("parenthesized expression", ')', span))
                } else {
                    // Parenthesized expression
                    self.expect(&Token::RParen)?;
                    Ok(first)
                }
            }
            Token::LBracket => {
                self.advance();
                let elems = self.comma_list(
                    "list literal",
                    span,
                    ListEnd::Close(&Token::RBracket),
                    |p| {
                        if p.at(&Token::DotDot) {
                            p.advance();
                            Ok(ListElem::Spread(p.parse_expr()?))
                        } else {
                            Ok(ListElem::Single(p.parse_expr()?))
                        }
                    },
                )?;
                Ok(self.mk_expr(ExprKind::List(elems), span))
            }
            Token::HashBrace => {
                self.advance();
                let pairs =
                    self.comma_list("map literal", span, ListEnd::Close(&Token::RBrace), |p| {
                        let key = p.parse_expr()?;
                        p.expect(&Token::Colon)?;
                        Ok((key, p.parse_expr()?))
                    })?;
                Ok(self.mk_expr(ExprKind::Map(pairs), span))
            }
            Token::HashBracket => {
                self.advance();
                let elems = self.comma_list(
                    "set literal",
                    span,
                    ListEnd::Close(&Token::RBracket),
                    Self::parse_expr,
                )?;
                Ok(self.mk_expr(ExprKind::SetLit(elems), span))
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
            Token::Return => {
                self.advance();
                // Return may or may not have a value
                if self.has_newline_before() || self.at(&Token::RBrace) || self.at(&Token::Eof) {
                    Ok(self.mk_expr(ExprKind::Return(None), span))
                } else {
                    let val = self.parse_expr()?;
                    Ok(self.mk_expr(ExprKind::Return(Some(Box::new(val))), span))
                }
            }
            // select is no longer a keyword; use channel.select([...])
            _ => {
                if self.at_double_slash() {
                    return Err(Diagnostic::error(
                        Code::UnsupportedSyntax,
                        self.span(),
                        Self::DOUBLE_SLASH_HINT,
                    ));
                }
                Err(Diagnostic::error(
                    Code::ExpectedExpression,
                    self.span(),
                    format!("expected expression, found {}", self.peek()),
                ))
            }
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
                    return Err(Diagnostic::error(
                        Code::ExpectedExpression,
                        self.span(),
                        "invalid expression in string interpolation; use \\{ for a literal brace",
                    ));
                }
            }
        }

        Ok(self.mk_expr(ExprKind::StringInterp(parts), span))
    }

    // ── Function calls ───────────────────────────────────────────────

    fn parse_call_expr(&mut self, callee: Expr) -> Result<Expr> {
        let span = callee.span;
        let args = self.parse_call_args()?;
        // Trailing closures are handled by the postfix loop in parse_expr_bp,
        // which respects min_bp and correctly suppresses them in match scrutinees.
        Ok(self.mk_expr(ExprKind::Call(Box::new(callee), args), span))
    }

    fn parse_call_args(&mut self) -> Result<Vec<Expr>> {
        let open = self.expect(&Token::LParen)?.span;
        self.comma_list(
            "function call argument list",
            open,
            ListEnd::Close(&Token::RParen),
            Self::parse_expr,
        )
    }

    fn parse_index_expr(&mut self, left: Expr) -> Result<Expr> {
        // Postfix indexing (`xs[i]`) is reserved syntax but not yet implemented.
        // Reject it with an actionable error message pointing at the typed
        // accessors users should reach for instead.
        let _ = left;
        let bracket_span = self.span();
        Err(Diagnostic::error(
            Code::UnsupportedSyntax,
            bracket_span,
            "postfix indexing is not supported; use list.get(xs, i), \
                      map.get(m, k), or string.slice(s, i, i + 1)",
        ))
    }

    // ── Trailing closures ────────────────────────────────────────────

    fn is_trailing_closure(&self) -> bool {
        if !self.lbrace_starts_closure() {
            return false;
        }
        match self.header_here() {
            None => true,
            Some(header) => match header.kind {
                // The body of a `loop` is a block, and a block never has
                // the `params ->` shape, so braces that have it are a
                // closure.
                HeaderKind::LoopInit => true,
                // A match body has exactly the closure shape
                // (`{ x -> ... }`), so in a scrutinee the braces are the
                // match body. The one exception is the right operand of
                // `|>`, where a closure is allowed as long as something
                // that can be the rest of the scrutinee, or the match
                // body, comes after it.
                HeaderKind::MatchScrutinee => {
                    header.in_pipe_rhs && self.scrutinee_continues_after_braces()
                }
            },
        }
    }

    /// True when a `{` at the current position could be the block of the
    /// enclosing header. A constructor in front of such a `{` starts a
    /// record literal only if the braces hold `field: value` pairs (see
    /// `scrutinee_lbrace_is_record_literal`).
    fn lbrace_may_be_header_block(&self) -> bool {
        self.header_here().is_some()
    }

    /// For closure-shaped braces at the current position: true when the
    /// token after the matching `}` can continue the expression the
    /// braces belong to, or open a further block. False when nothing of
    /// the kind follows: in a match scrutinee the braces then have to be
    /// the match body itself, as in
    /// `match xs |> list.head { Some(x) -> x ... }`.
    fn scrutinee_continues_after_braces(&self) -> bool {
        let inside = self.delim_depth_at(self.pos) + 1;
        let mut close = self.pos + 1;
        while close < self.tokens.len() {
            let is_closer = matches!(
                self.tokens[close].kind,
                Token::RBrace | Token::RParen | Token::RBracket | Token::StringEnd(_)
            );
            if is_closer && self.delim_depth_at(close) == inside {
                break;
            }
            close += 1;
        }
        if close >= self.tokens.len() {
            // Unclosed braces: leave the report to the closure parser.
            return true;
        }
        let mut next = close + 1;
        let mut crossed_newline = false;
        while matches!(
            self.tokens.get(next),
            Some(Tok {
                kind: Token::Newline,
                ..
            })
        ) {
            crossed_newline = true;
            next += 1;
        }
        match self.tokens.get(next).map(|t| &t.kind) {
            // A further block: the match body, or another closure.
            Some(Token::LBrace) => true,
            // Infix operators continue an expression across a line break.
            Some(
                Token::Dot
                | Token::Pipe
                | Token::DotDot
                | Token::OrOr
                | Token::AndAnd
                | Token::EqEq
                | Token::NotEq
                | Token::Lt
                | Token::Gt
                | Token::LtEq
                | Token::GtEq
                | Token::Star
                | Token::Slash
                | Token::Percent
                | Token::As,
            ) => true,
            // `+`, `-` and the postfix forms continue it on the same
            // line only.
            Some(
                Token::Plus | Token::Minus | Token::Question | Token::LParen | Token::LBracket,
            ) => !crossed_newline,
            _ => false,
        }
    }

    /// True when the current token is a `{` whose contents start like a
    /// closure: `params ->`. A parameter is a pattern with an optional
    /// `: Type` annotation, so the scan accepts, at the brace's own
    /// depth, identifiers, `,`, `:`, `::`, `.`, `type` and the openers
    /// of nested groups (whose contents it skips), and answers true at
    /// the first `->`. Anything else at that depth, including the
    /// closing `}`, means a block or a record literal: neither can put
    /// a `->` at its own depth after only those tokens. `{ a: Int -> a }`
    /// is a closure; `{ a: Int }` and `{ a: 1 }` are record literals.
    /// A function type in an annotation (`{ f: Fn(Int) -> Int -> ... }`)
    /// answers true at its own arrow, which is the right answer too.
    fn lbrace_starts_closure(&self) -> bool {
        if self.peek() != &Token::LBrace {
            return false;
        }
        let inside = self.delim_depth_at(self.pos) + 1;
        let mut i = self.pos + 1; // skip `{`
        // Skip leading newlines to find the first real token
        while i < self.tokens.len() && matches!(self.tokens[i].kind, Token::Newline) {
            i += 1;
        }
        // If the first real token is a literal, this is a match body
        // (patterns like `0 ->`, `true ->`), not a trailing closure.
        // Note: `_` is NOT excluded here because it is a valid closure
        // parameter name (meaning "ignore this argument"). Match bodies
        // are consumed directly by parse_match_expr via expect(LBrace),
        // so they never reach this heuristic.
        if i < self.tokens.len() {
            match &self.tokens[i].kind {
                Token::Int(_) | Token::Float(_) | Token::Bool(_) => return false,
                _ => {}
            }
        }
        while i < self.tokens.len() {
            if self.delim_depth_at(i) > inside {
                i += 1;
                continue;
            }
            match &self.tokens[i].kind {
                Token::Arrow => return true,
                Token::Newline
                | Token::Ident(_)
                | Token::Comma
                | Token::Colon
                | Token::ColonColon
                | Token::Dot
                | Token::Type
                | Token::LParen
                | Token::LBracket
                | Token::LBrace
                | Token::HashBrace => {}
                // The closer of a nested group sits one level deeper
                // than the brace's own contents and was skipped above;
                // a closer at this depth ends the braces.
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
        while i < self.tokens.len() && matches!(self.tokens[i].kind, Token::Newline) {
            i += 1;
        }
        if !matches!(self.tokens.get(i).map(|t| &t.kind), Some(Token::Ident(_))) {
            return false;
        }
        i += 1;
        while i < self.tokens.len() && matches!(self.tokens[i].kind, Token::Newline) {
            i += 1;
        }
        matches!(self.tokens.get(i).map(|t| &t.kind), Some(Token::Colon))
    }

    /// Render an `Ident` / dotted `FieldAccess` chain (`util`,
    /// `a.b.util`) back to source text for diagnostics. Returns `None`
    /// for anything that isn't a simple dotted path. Used by the
    /// deep-path qualified-record-construction error (round 93; kept
    /// in round 94 — only a SINGLE module qualifier is real syntax) to
    /// echo the path the user wrote.
    fn dotted_path_text(expr: &Expr) -> Option<String> {
        match &expr.kind {
            ExprKind::Ident(name) => Some(intern::resolve(*name)),
            ExprKind::FieldAccess(base, field, _) => Some(format!(
                "{}.{}",
                Self::dotted_path_text(base)?,
                intern::resolve(*field)
            )),
            _ => None,
        }
    }

    fn parse_trailing_closure(&mut self) -> Result<Expr> {
        self.parse_trailing_closure_as_lambda()
    }

    fn parse_trailing_closure_as_lambda(&mut self) -> Result<Expr> {
        let span = self.span();
        self.expect(&Token::LBrace)?;
        let params = self.parse_closure_params(span)?;
        self.expect(&Token::Arrow)?;
        self.skip_nl();

        // Parse body statements
        let stmts = self.parse_stmt_list(&Token::RBrace)?;
        self.expect(&Token::RBrace)?;

        let body = if stmts.len() == 1 {
            if let Stmt::Expr(e) = &stmts[0] {
                e.clone()
            } else {
                self.mk_expr(ExprKind::Block(stmts), span)
            }
        } else {
            self.mk_expr(ExprKind::Block(stmts), span)
        };

        Ok(self.mk_expr(
            ExprKind::Lambda {
                params,
                body: Box::new(body),
            },
            span,
        ))
    }

    /// Closure parameters: the data-parameter grammar of a named
    /// function, `pattern` or `pattern: Type`, separated by commas and
    /// ended by `->`. The pattern may destructure (`(a, b)`,
    /// `Point { x, y }`, `User { name, .. }`); there is no return-type
    /// annotation, the body's type is the closure's return type.
    /// `open` is the `{` of the closure; the `->` is left for the caller.
    fn parse_closure_params(&mut self, open: Span) -> Result<Vec<Param>> {
        // Closure params terminate at `->`, not a closing bracket.
        let end = ListEnd::Before(&Token::Arrow);
        self.comma_list("closure parameter list", open, end, |p| {
            if p.at(&Token::Type) {
                return Err(Diagnostic::error(
                    Code::UnsupportedSyntax,
                    p.span(),
                    "a closure cannot take a 'type' parameter; declare a named function",
                ));
            }
            p.parse_data_param()
        })
    }

    fn attach_trailing_closure(&mut self, callee: Expr, closure: Expr) -> Expr {
        let span = callee.span;
        match callee.kind {
            ExprKind::Call(f, mut args) => {
                args.push(closure);
                self.mk_expr(ExprKind::Call(f, args), span)
            }
            _ => {
                // Wrap as a call: `f { x -> body }` → f(closure)
                self.mk_expr(ExprKind::Call(Box::new(callee), vec![closure]), span)
            }
        }
    }

    // ── Match ────────────────────────────────────────────────────────

    fn parse_match_expr(&mut self) -> Result<Expr> {
        let span = self.span();
        self.expect(&Token::Match)?;
        self.skip_nl();

        // Guardless match: `match { cond -> body ... }`
        let guardless = self.at(&Token::LBrace);
        let scrutinee = if guardless {
            None
        } else {
            // The scrutinee is a header expression (see `BlockHeader`):
            // the match body `{` that follows it must not be consumed as
            // a trailing closure or as the braces of a record literal.
            let expr = self.parse_header_expr(HeaderKind::MatchScrutinee)?;
            Some(Box::new(expr))
        };

        self.expect(&Token::LBrace)?;
        self.skip_nl();

        let mut arms = Vec::new();
        while !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
            arms.push(self.parse_match_arm(guardless)?);
            // Arms are separated by line breaks, as statements are.
            if !self.at_newline() && !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
                return Err(self.same_line_err("match arm"));
            }
            self.skip_nl();
        }
        self.expect(&Token::RBrace)?;

        Ok(self.mk_expr(
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
                pattern: Pattern::new(PatternKind::Wildcard, condition.span),
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
            return Ok(self.mk_expr(ExprKind::Recur(args), span));
        }

        self.skip_nl();

        // Zero-binding variant: `loop { body }`
        if self.at(&Token::LBrace) {
            let body = self.parse_block()?;
            return Ok(self.mk_expr(
                ExprKind::Loop {
                    bindings: Vec::new(),
                    body: Box::new(body),
                },
                span,
            ));
        }

        // Binding variant: `loop x = init, y = init { body }`
        let end = ListEnd::Before(&Token::LBrace);
        let bindings = self.comma_list("loop binding list", span, end, |p| {
            let (name, name_span) = p.expect_ident()?;
            p.expect(&Token::Eq)?;
            p.skip_nl();
            // An initialiser is a header expression (see `BlockHeader`):
            // in `loop i = n, acc = Nil { ... }` the `{` opens the loop
            // body, it does not make `Nil { ... }` a record literal.
            let init = p.parse_header_expr(HeaderKind::LoopInit)?;
            Ok((name, name_span, init))
        })?;
        let body = self.parse_block()?;

        Ok(self.mk_expr(
            ExprKind::Loop {
                bindings,
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
        while i < self.tokens.len() && matches!(self.tokens[i].kind, Token::Newline) {
            i += 1;
        }
        // Spread head is unambiguous.
        if matches!(self.tokens.get(i).map(|t| &t.kind), Some(Token::DotDotDot)) {
            return true;
        }
        // `Ident COLON` (with possible newlines between) — anon record.
        if !matches!(self.tokens.get(i).map(|t| &t.kind), Some(Token::Ident(_))) {
            return false;
        }
        i += 1;
        while i < self.tokens.len() && matches!(self.tokens[i].kind, Token::Newline) {
            i += 1;
        }
        matches!(self.tokens.get(i).map(|t| &t.kind), Some(Token::Colon))
    }

    /// Parse `{name: expr, ...}` or `{...spread, name: expr, ...}` after
    /// the caller has confirmed via `is_anon_record_literal` that the
    /// current `{` opens an anon-record literal. Consumes the closing
    /// `}` itself.
    fn parse_anon_record_literal(&mut self) -> Result<Expr> {
        let span = self.span();
        self.expect(&Token::LBrace)?;
        let mut spread: Option<Box<Expr>> = None;
        let mut first = true;
        let mut seen: std::collections::HashSet<Symbol> = std::collections::HashSet::new();
        let fields = self.comma_list(
            "anon record literal",
            span,
            ListEnd::Close(&Token::RBrace),
            |p| {
                let first = std::mem::take(&mut first);
                if p.at(&Token::DotDotDot) {
                    if !first {
                        return Err(Diagnostic::error(
                            Code::UnsupportedSyntax,
                            p.span(),
                            "v1 row polymorphism allows only one spread head per anon record literal",
                        ));
                    }
                    p.advance();
                    spread = Some(Box::new(p.parse_expr()?));
                    return Ok(None);
                }
                let (name, name_span) = p.expect_ident()?;
                p.expect(&Token::Colon)?;
                let value = p.parse_expr()?;
                if !seen.insert(name) {
                    return Err(Diagnostic::error(
                        Code::DuplicateField,
                        name_span,
                        format!("duplicate field '{}' in anon record literal", name),
                    ));
                }
                Ok(Some((name, value)))
            },
        )?;
        let fields = fields.into_iter().flatten().collect();
        Ok(self.mk_expr(ExprKind::AnonRecord { spread, fields }, span))
    }

    /// The braces of a record literal or a record update, at the `{`:
    /// `{ name: expr, ... }`.
    fn parse_record_fields(&mut self) -> Result<Vec<(Symbol, Expr)>> {
        let open = self.expect(&Token::LBrace)?.span;
        self.comma_list(
            "record literal",
            open,
            ListEnd::Close(&Token::RBrace),
            |p| {
                if p.at(&Token::DotDot) || p.at(&Token::Dot) {
                    return Err(Diagnostic::error(
                        Code::UnsupportedSyntax,
                        p.span(),
                        "spread syntax is not supported; use `value.{ field: expr }` for record updates",
                    ));
                }
                let (name, _) = p.expect_ident()?;
                p.expect(&Token::Colon)?;
                Ok((name, p.parse_expr()?))
            },
        )
    }

    /// The error for an element behind the rest of a pattern (`..`,
    /// `..tail`, `...rest`) or the row variable of a record type, when
    /// `seen` says there was one: the rest stands last.
    fn nothing_after_rest(&self, seen: bool, rest: &str, close: char, list: &str) -> Result<()> {
        if !seen {
            return Ok(());
        }
        Err(Diagnostic::error(
            Code::ExpectedToken,
            self.span(),
            format!(
                "expected '{close}' after `{rest}` in {list}, found {}",
                self.peek()
            ),
        ))
    }

    // ── Patterns ─────────────────────────────────────────────────────

    fn parse_pattern(&mut self) -> Result<Pattern> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            self.depth -= 1;
            return Err(Diagnostic::error(
                Code::NestingTooDeep,
                self.span(),
                "pattern nesting exceeds maximum depth",
            ));
        }
        let result = self.parse_pattern_inner();
        self.depth -= 1;
        result
    }

    fn parse_pattern_inner(&mut self) -> Result<Pattern> {
        let first = self.parse_primary_pattern()?;
        // Check for or-pattern: pat1 | pat2 | ...
        // An alternative may start the next line with its `|`.
        if self.at_bar_skip_nl() {
            let or_span = first.span;
            let mut alts = vec![first];
            while self.at_bar_skip_nl() {
                self.advance();
                alts.push(self.parse_primary_pattern()?);
            }
            Ok(self.mk_pattern(PatternKind::Or(alts), or_span))
        } else {
            Ok(first)
        }
    }

    /// Parse the tail of an integer range pattern after the `..` token
    /// has been consumed: `[-]N`. The caller passes the already-signed
    /// start bound. Used by both the positive (`N..`) and negated
    /// (`-N..`) head paths in `parse_primary_pattern` so the four
    /// `..[-]N` exits stay in lock-step.
    fn parse_range_tail_int(&mut self, start: i64) -> Result<PatternKind> {
        match self.peek().clone() {
            Token::Int(m) => {
                self.int_in_range(m)?;
                self.advance();
                Ok(PatternKind::Range(start, m))
            }
            Token::Minus => {
                self.advance();
                match self.peek().clone() {
                    Token::Int(m) => {
                        self.advance();
                        Ok(PatternKind::Range(start, m.wrapping_neg()))
                    }
                    _ => Err(Diagnostic::error(
                        Code::ExpectedPattern,
                        self.span(),
                        "expected integer after - in range pattern",
                    )),
                }
            }
            _ => Err(Diagnostic::error(
                Code::ExpectedPattern,
                self.span(),
                "expected integer end for range pattern",
            )),
        }
    }

    /// Parse the tail of a float range pattern after the `..` token
    /// has been consumed: `[-]F`. Mirrors `parse_range_tail_int` but
    /// for `PatternKind::FloatRange`. Caller passes the already-signed
    /// start bound.
    fn parse_range_tail_float(&mut self, start: f64) -> Result<PatternKind> {
        match self.peek().clone() {
            Token::Float(m) => {
                self.advance();
                Ok(PatternKind::FloatRange(start, m))
            }
            Token::Minus => {
                self.advance();
                match self.peek().clone() {
                    Token::Float(m) => {
                        self.advance();
                        Ok(PatternKind::FloatRange(start, -m))
                    }
                    _ => Err(Diagnostic::error(
                        Code::ExpectedPattern,
                        self.span(),
                        "expected float after - in range pattern",
                    )),
                }
            }
            _ => Err(Diagnostic::error(
                Code::ExpectedPattern,
                self.span(),
                "expected float end for range pattern",
            )),
        }
    }

    /// Shared tail for (possibly qualified) constructor-shaped patterns,
    /// entered after the head segments are consumed. Three shapes:
    /// `Name(args)` → constructor, `Name { fields }` → record pattern,
    /// bare `Name` → unit-variant constructor. Used by both the
    /// capitalized head (`Circle(..)`, `Shape.Circle(..)`) and the
    /// round-94 module-qualified head (`shapes.Circle(..)`).
    fn parse_constructor_pattern_tail(
        &mut self,
        qualifier: Vec<Qualifier>,
        head: Qualifier,
        start: Span,
    ) -> Result<Pattern> {
        let mut pattern = self.parse_constructor_pattern_tail_open(qualifier, head, start)?;
        pattern.span = self.close(pattern.span);
        Ok(pattern)
    }

    /// `parse_constructor_pattern_tail`, with the pattern's span not yet
    /// closed.
    fn parse_constructor_pattern_tail_open(
        &mut self,
        qualifier: Vec<Qualifier>,
        head: Qualifier,
        start: Span,
    ) -> Result<Pattern> {
        let mk = |kind: PatternKind| Pattern::new(kind, start);
        let Qualifier {
            name,
            span: name_span,
        } = head;
        // Constructor pattern: Some(x), Ok(value), Rect(w, h)
        if self.at(&Token::LParen) {
            let open = self.advance().span;
            let pats = self.comma_list(
                "constructor pattern",
                open,
                ListEnd::Close(&Token::RParen),
                Self::parse_pattern,
            )?;
            Ok(mk(PatternKind::Constructor {
                qualifier,
                name,
                name_span,
                args: pats,
            }))
        } else if self.at(&Token::LBrace) {
            // Record pattern: User { name, age, .. }
            let open = self.advance().span;
            let mut has_rest = false;
            let fields = self.comma_list(
                "record pattern",
                open,
                ListEnd::Close(&Token::RBrace),
                |p| {
                    p.nothing_after_rest(has_rest, "..", '}', "a record pattern")?;
                    if p.at(&Token::DotDot) {
                        p.advance();
                        has_rest = true;
                        return Ok(None);
                    }
                    p.parse_field_pattern().map(Some)
                },
            )?;
            let fields = fields.into_iter().flatten().collect();
            // A record type is reached through at most its module.
            if qualifier.len() > 1 {
                return Err(Diagnostic::error(
                    Code::UnsupportedSyntax,
                    qualifier[1].span,
                    format!(
                        "a record pattern has at most one qualifier, its module: write '{}.{} {{ ... }}'",
                        intern::resolve(qualifier[0].name),
                        intern::resolve(name)
                    ),
                ));
            }
            Ok(mk(PatternKind::Record {
                module: qualifier.first().copied(),
                name: Some(name),
                name_span,
                fields,
                has_rest,
            }))
        } else {
            Ok(mk(PatternKind::Constructor {
                qualifier,
                name,
                name_span,
                args: Vec::new(),
            }))
        }
    }

    /// A field of a record pattern: `name` or `name: pattern`.
    fn parse_field_pattern(&mut self) -> Result<(Symbol, Span, Option<Pattern>)> {
        let (field_name, field_span) = self.expect_ident()?;
        let sub = if self.peek_skip_nl() == &Token::Colon {
            self.advance();
            Some(self.parse_pattern()?)
        } else {
            None
        };
        Ok((field_name, field_span, sub))
    }

    fn parse_primary_pattern(&mut self) -> Result<Pattern> {
        let mut pattern = self.parse_primary_pattern_open()?;
        pattern.span = self.close(pattern.span);
        Ok(pattern)
    }

    /// `parse_primary_pattern`, with the pattern's span not yet closed.
    fn parse_primary_pattern_open(&mut self) -> Result<Pattern> {
        self.skip_nl();
        let start = self.span();
        let mk = |kind: PatternKind| Pattern::new(kind, start);
        match self.peek().clone() {
            Token::Ident(ref name) if *name == intern::intern("_") => {
                self.advance();
                Ok(mk(PatternKind::Wildcard))
            }
            Token::Ident(name) => {
                let name_span = self.advance().span;
                // A lowercase name not followed by `.` binds a variable.
                if !is_constructor(name) && !self.at(&Token::Dot) {
                    return Ok(mk(PatternKind::Ident(name)));
                }
                // A constructor or record head, with up to two segments
                // before it: the owning enum (`Shape.Circle(r)`), an
                // imported module or alias (`shapes.Circle(r)`,
                // `util.Pt { x }`, the unit variant `color.Red`), or both
                // (`shapes.Shape.Circle(r)`). Variants resolve by bare name,
                // so the qualifier is carried on the pattern node and
                // validated by the typechecker.
                let mut segments = vec![Qualifier {
                    name,
                    span: name_span,
                }];
                while self.at(&Token::Dot) {
                    self.advance();
                    let (segment, segment_span) = self.expect_ident()?;
                    if !is_constructor(segment) {
                        let written: Vec<String> =
                            segments.iter().map(|q| intern::resolve(q.name)).collect();
                        return Err(Diagnostic::error(
                            Code::ExpectedIdentifier,
                            segment_span,
                            format!(
                                "expected a type or variant name after '{}.' in pattern, found '{}'",
                                written.join("."),
                                intern::resolve(segment)
                            ),
                        ));
                    }
                    segments.push(Qualifier {
                        name: segment,
                        span: segment_span,
                    });
                }
                if segments.len() > 3 {
                    let written: Vec<String> =
                        segments.iter().map(|q| intern::resolve(q.name)).collect();
                    return Err(Diagnostic::error(
                        Code::UnsupportedSyntax,
                        segments[3].span,
                        format!(
                            "a pattern head has at most two qualifiers ('{}' has more); \
                             write 'module.Variant', 'Enum.Variant' or 'module.Enum.Variant'",
                            written.join(".")
                        ),
                    ));
                }
                let head = segments.pop().expect("one segment at least");
                self.parse_constructor_pattern_tail(segments, head, start)
            }
            Token::Int(n) => {
                self.int_in_range(n)?;
                self.advance();
                // Check for range pattern: n..m
                if self.at(&Token::DotDot) {
                    self.advance();
                    self.parse_range_tail_int(n).map(mk)
                } else {
                    Ok(mk(PatternKind::Int(n)))
                }
            }
            Token::Float(n) => {
                self.advance();
                if self.at(&Token::DotDot) {
                    self.advance();
                    self.parse_range_tail_float(n).map(mk)
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
                    pats.extend(self.comma_list(
                        "tuple pattern",
                        start,
                        ListEnd::Close(&Token::RParen),
                        Self::parse_pattern,
                    )?);
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
                let mut rest: Option<(Symbol, Span)> = None;
                let fields = self.comma_list(
                    "anon record pattern",
                    start,
                    ListEnd::Close(&Token::RBrace),
                    |p| {
                        p.nothing_after_rest(rest.is_some(), "...", '}', "an anon record pattern")?;
                        if p.at(&Token::DotDotDot) {
                            p.advance();
                            // Named rest binding required (B6: no unnamed rest).
                            rest = Some(p.expect_ident()?);
                            return Ok(None);
                        }
                        p.parse_field_pattern().map(Some)
                    },
                )?;
                let fields = fields.into_iter().flatten().collect();
                Ok(mk(PatternKind::AnonRecord { fields, rest }))
            }
            Token::LBracket => {
                self.advance(); // consume [
                let mut rest = None;
                let patterns = self.comma_list(
                    "list pattern",
                    start,
                    ListEnd::Close(&Token::RBracket),
                    |p| {
                        p.nothing_after_rest(rest.is_some(), "..", ']', "a list pattern")?;
                        if p.at(&Token::DotDot) {
                            p.advance(); // consume ..
                            rest = Some(Box::new(p.parse_pattern()?));
                            return Ok(None);
                        }
                        p.parse_pattern().map(Some)
                    },
                )?;
                let patterns = patterns.into_iter().flatten().collect();
                Ok(mk(PatternKind::List(patterns, rest)))
            }
            Token::HashBrace => {
                // Map pattern: #{ "key": pattern, ... }
                self.advance();
                let entries =
                    self.comma_list("map pattern", start, ListEnd::Close(&Token::RBrace), |p| {
                        let key = match p.peek().clone() {
                            Token::StringLit(s, _) => {
                                p.advance();
                                s
                            }
                            _ => {
                                return Err(Diagnostic::error(
                                    Code::ExpectedPattern,
                                    p.span(),
                                    "expected string key in map pattern",
                                ));
                            }
                        };
                        p.expect(&Token::Colon)?;
                        Ok((key, p.parse_pattern()?))
                    })?;
                Ok(mk(PatternKind::Map(entries)))
            }
            Token::Minus => {
                // Negative number pattern
                self.advance();
                match self.peek().clone() {
                    Token::Int(n) => {
                        self.advance();
                        // Check for range pattern: -n..m
                        // `wrapping_neg`: the magnitude 2^63 is
                        // `i64::MIN` already (see `Token::Int`).
                        if self.at(&Token::DotDot) {
                            self.advance();
                            self.parse_range_tail_int(n.wrapping_neg()).map(mk)
                        } else {
                            Ok(mk(PatternKind::Int(n.wrapping_neg())))
                        }
                    }
                    Token::Float(n) => {
                        self.advance();
                        if self.at(&Token::DotDot) {
                            self.advance();
                            self.parse_range_tail_float(-n).map(mk)
                        } else {
                            Ok(mk(PatternKind::Float(-n)))
                        }
                    }
                    _ => Err(Diagnostic::error(
                        Code::ExpectedPattern,
                        self.span(),
                        "expected number after -",
                    )),
                }
            }
            Token::Caret => {
                self.advance();
                match self.peek().clone() {
                    Token::Ident(name) => {
                        self.advance();
                        Ok(mk(PatternKind::Pin(name)))
                    }
                    _ => Err(Diagnostic::error(
                        Code::ExpectedIdentifier,
                        self.span(),
                        "expected identifier after ^ in pin pattern",
                    )),
                }
            }
            _ => Err(Diagnostic::error(
                Code::ExpectedPattern,
                self.span(),
                format!("expected pattern, found {}", self.peek()),
            )),
        }
    }

    // ── Utility ──────────────────────────────────────────────────────

    /// Is the next token, past any line breaks, a `|`? Skips the line
    /// breaks only if so.
    fn at_bar_skip_nl(&mut self) -> bool {
        let mut n = 0;
        while matches!(self.peek_at(n), Some(Token::Newline)) {
            n += 1;
        }
        let bar = matches!(self.peek_at(n), Some(Token::Bar));
        if bar {
            self.pos += n;
        }
        bar
    }

    fn peek_skip_nl(&mut self) -> &Token {
        self.skip_nl();
        self.peek()
    }
}

fn is_constructor(name: Symbol) -> bool {
    intern::resolve(name).starts_with(|c: char| c.is_uppercase())
}

/// `name` with its first character in upper case, for "did you mean"
/// suggestions.
fn capitalized(name: &str) -> String {
    // `_red` and `_Red` both suggest `Red`: a leading underscore does not
    // make a name start with an upper-case letter.
    let mut chars = name.trim_start_matches('_').chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// A declared type name and its enum variant names must start with an
/// upper-case letter. Everywhere a type is written, a lower-case name is
/// a type variable, and in a pattern a lower-case name binds a variable,
/// so a lower-case type could never be named and a lower-case variant
/// could never be matched.
fn check_type_decl_names(name: Symbol, name_span: Span, body: &TypeBody) -> Result<()> {
    // A type name is refused only when it starts with a lower-case letter:
    // `_Meters` resolves as a named type wherever it is written. A variant
    // must start with an upper-case letter, because `_Red ->` in a pattern
    // binds a variable.
    let starts_lowercase = intern::resolve(name)
        .chars()
        .next()
        .is_some_and(char::is_lowercase);
    if starts_lowercase {
        let text = intern::resolve(name);
        return Err(Diagnostic::error(
            Code::InvalidDeclaration,
            name_span,
            format!(
                "type name '{text}' must start with an uppercase letter, e.g. `type {}`: \
                 a lowercase name where a type is expected is a type variable, so this \
                 type could never be referred to",
                capitalized(&text)
            ),
        ));
    }
    if let TypeBody::Enum(variants) = body {
        for variant in variants {
            if !is_constructor(variant.name) {
                let text = intern::resolve(variant.name);
                return Err(Diagnostic::error(
                    Code::InvalidDeclaration,
                    variant.name_span,
                    format!(
                        "enum variant '{text}' must start with an uppercase letter, e.g. `{}`: \
                         a lowercase name in a pattern binds a variable, so this variant \
                         could never be matched",
                        capitalized(&text)
                    ),
                ));
            }
        }
    }
    Ok(())
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intern;
    use crate::lexer::Lexer;

    fn parse(input: &str) -> Program {
        let tokens = Lexer::new(crate::source::FileId::default(), input)
            .tokenize()
            .unwrap();
        Parser::new(tokens, input).parse_program().unwrap()
    }

    // ── Test helpers ────────────────────────────────────────────────
    //
    // The parser test bodies repeatedly decode `prog.decls[0]` as a
    // `Decl::Fn`, drill into its `body: ExprKind::Block(stmts)`, and pull
    // the last `Stmt::Expr` to inspect the resulting `ExprKind`. That
    // five-line decode appeared ~28 times verbatim before round 76, all
    // with the same panic messages. The helpers below collapse the
    // decode to a single call so test bodies focus on the assertion they
    // actually care about (e.g. "is the top-level expr a `?`").
    //
    // The helpers are panic-on-mismatch (test-only), and assume the
    // first decl is a `fn` whose body is a block — this is the shape
    // every site that used the inline decode required.

    /// Decode the last expression of the first declaration (which must
    /// be a `fn` with a block body) and return a reference to it.
    /// Panics if the AST shape doesn't match.
    fn last_expr_of_main(prog: &Program) -> &Expr {
        let f = match &prog.decls[0] {
            Decl::Fn(f) => f,
            other => panic!("expected fn decl, got {:?}", other),
        };
        let stmts = match &f.body.kind {
            ExprKind::Block(stmts) => stmts,
            other => panic!("expected block body, got {:?}", other),
        };
        match stmts.last().unwrap() {
            Stmt::Expr(e) => e,
            other => panic!("expected expression statement, got {:?}", other),
        }
    }

    /// Decode the last expression of the first fn decl, asserting it is
    /// a `Match`, and return its `(scrutinee?, arms)`.
    fn last_match_of_main(prog: &Program) -> (Option<&Expr>, &[MatchArm]) {
        let expr = last_expr_of_main(prog);
        match &expr.kind {
            ExprKind::Match { expr: scrut, arms } => (scrut.as_deref(), arms.as_slice()),
            other => panic!("expected match expression, got {:?}", other),
        }
    }

    /// Convenience: first arm of the last match in `main`.
    fn first_match_arm_of_main(prog: &Program) -> &MatchArm {
        let (_, arms) = last_match_of_main(prog);
        &arms[0]
    }

    // ── Helper contract pin ─────────────────────────────────────────
    //
    // Locks the helpers' contracts so future edits cannot silently
    // drift. Compares helper output to a hand-decoded reference on a
    // known-shape AST.
    #[test]
    fn helpers_match_hand_decoded_reference() {
        let prog = parse(
            r#"
            fn main() {
                match 1 {
                    1 -> "one"
                    _ -> "other"
                }
            }
        "#,
        );

        // Hand-decoded reference: walk decls[0] → fn body → last stmt → expr.
        let hand_expr = {
            let f = match &prog.decls[0] {
                Decl::Fn(f) => f,
                _ => panic!("expected fn"),
            };
            let stmts = match &f.body.kind {
                ExprKind::Block(s) => s,
                _ => panic!("expected block"),
            };
            match stmts.last().unwrap() {
                Stmt::Expr(e) => e,
                _ => panic!("expected expr stmt"),
            }
        };

        let helper_expr = last_expr_of_main(&prog);
        // Both references must point at the exact same node.
        assert!(std::ptr::eq(hand_expr, helper_expr));

        // last_match_of_main must agree with hand-decoded match.
        let hand_arms = match &hand_expr.kind {
            ExprKind::Match { arms, .. } => arms,
            _ => panic!("expected match"),
        };
        let (_scrut, helper_arms) = last_match_of_main(&prog);
        assert_eq!(hand_arms.len(), helper_arms.len());
        assert!(std::ptr::eq(&hand_arms[0], &helper_arms[0]));

        // first_match_arm_of_main must alias arms[0].
        let first = first_match_arm_of_main(&prog);
        assert!(std::ptr::eq(&hand_arms[0], first));
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

    /// The brace shapes that the closure-parameter grammar must keep
    /// apart: a typed closure, a record literal with the same prefix, a
    /// block holding a tuple, a destructuring closure, a closure whose
    /// annotation is a function type, and a parameterless closure.
    #[test]
    fn test_closure_param_brace_disambiguation() {
        let kind_of = |body: &str| -> String {
            let prog = parse(&format!("fn main() {{\n  {body}\n}}"));
            match &last_expr_of_main(&prog).kind {
                ExprKind::Lambda { params, .. } => format!(
                    "lambda/{}/{}",
                    params.len(),
                    params.iter().filter(|p| p.ty.is_some()).count()
                ),
                ExprKind::AnonRecord { .. } => "record".to_string(),
                ExprKind::Block(_) => "block".to_string(),
                other => format!("{other:?}"),
            }
        };
        assert_eq!(kind_of("{ a: Int -> a }"), "lambda/1/1");
        assert_eq!(kind_of("{ a: 1 }"), "record");
        assert_eq!(kind_of("{ a: Int }"), "record");
        assert_eq!(kind_of("{ x -> x }"), "lambda/1/0");
        assert_eq!(kind_of("{ (a, b) }"), "block");
        assert_eq!(kind_of("{ (a, b): (Int, Int) -> a }"), "lambda/1/1");
        assert_eq!(kind_of("{ Point { x, y } -> x }"), "lambda/1/0");
        assert_eq!(kind_of("{ { name, ...rest } -> name }"), "lambda/1/0");
        assert_eq!(kind_of("{ f: Fn(Int) -> Int, x -> f(x) }"), "lambda/2/1");
        assert_eq!(kind_of("{ -> 1 }"), "lambda/0/0");
        assert_eq!(kind_of("{ a: { v: Int -> v } }"), "record");
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
            let wc = &f.where_clauses[0];
            assert_eq!(wc.type_param, intern::intern("x"));
            assert_eq!(wc.trait_name, intern::intern("Display"));
            assert!(wc.trait_args.is_empty());
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
        let (scrutinee, arms) = last_match_of_main(&prog);
        let scrutinee = scrutinee.expect("expected match expression with scrutinee");
        assert!(
            matches!(scrutinee.kind, ExprKind::Pipe(_, _)),
            "expected Pipe scrutinee, got {:?}",
            scrutinee.kind
        );
        // Should have 2 arms
        assert_eq!(arms.len(), 2);
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

    fn parse_err(input: &str) -> Diagnostic {
        let tokens = Lexer::new(crate::source::FileId::default(), input)
            .tokenize()
            .unwrap();
        Parser::new(tokens, input).parse_program().unwrap_err()
    }

    fn parse_recovering(input: &str) -> (Program, Vec<Diagnostic>) {
        let tokens = Lexer::new(crate::source::FileId::default(), input)
            .tokenize()
            .unwrap();
        Parser::new(tokens, input).parse_program_recovering()
    }

    // ── 1. Error recovery ───────────────────────────────────────────

    #[test]
    fn test_recovery_reports_same_line_decl_and_keeps_both() {
        let (prog, errs) = parse_recovering("fn a() { 1 } fn b() { 2 }\n");
        assert_eq!(prog.decls.len(), 2, "both declarations are kept");
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(
            errs[0].message.contains("expected a newline before 'fn'"),
            "{}",
            errs[0].message
        );
    }

    #[test]
    fn test_recovery_reports_every_top_level_name_bound_twice() {
        // Two items of `import m.{ ... }` lines are the resolver's to
        // judge (one definition imported twice is one binding); an item
        // and a function are not.
        let (prog, errs) =
            parse_recovering("import a.{ x }\nimport b.{ x }\nfn x() { 1 }\nfn y() { 2 }\n");
        assert_eq!(prog.decls.len(), 4, "every declaration is kept");
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(
            errs[0]
                .message
                .contains("by the import and by the function here")
        );
        // The error points at the function's name on line 3; the label
        // points at the first binder, the item `x` of the first import.
        assert_eq!(errs[0].span.start, 33);
        assert_eq!(errs[0].labels[0].0.start, 11);
    }

    #[test]
    fn test_recovery_stub_does_not_bind_its_name() {
        // The stub stands in for the broken `f` the user is fixing; the
        // later real `f` is not a second binding.
        let (_, errs) = parse_recovering("fn f(\nfn f() { 1 }\n");
        assert!(
            errs.iter().all(|e| !e.message.contains("bound twice")),
            "{errs:?}"
        );
    }

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
        let arm = first_match_arm_of_main(&prog);
        assert!(matches!(&arm.pattern.kind, PatternKind::Or(pats) if pats.len() == 3));
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
        let arm = first_match_arm_of_main(&prog);
        assert!(matches!(&arm.pattern.kind, PatternKind::Range(1, 10)));
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
        let arm = first_match_arm_of_main(&prog);
        assert!(
            matches!(&arm.pattern.kind, PatternKind::Pin(name) if *name == intern::intern("x"))
        );
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
        let arm = first_match_arm_of_main(&prog);
        if let PatternKind::Map(ref entries) = arm.pattern.kind {
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].0, "key");
            assert!(
                matches!(entries[0].1.kind, PatternKind::Ident(ref v) if *v == intern::intern("v"))
            );
        } else {
            panic!("expected map pattern");
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
        let arm = first_match_arm_of_main(&prog);
        if let PatternKind::Constructor {
            ref name,
            args: ref inner,
            ..
        } = arm.pattern.kind
        {
            assert_eq!(*name, intern::intern("Some"));
            assert_eq!(inner.len(), 1);
            assert!(matches!(&inner[0].kind, PatternKind::Tuple(pats) if pats.len() == 2));
        } else {
            panic!("expected constructor pattern, got {:?}", arm.pattern);
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
        let (_, arms) = last_match_of_main(&prog);
        if let PatternKind::List(ref pats, ref rest) = arms[0].pattern.kind {
            assert_eq!(pats.len(), 1);
            assert!(matches!(&pats[0].kind, PatternKind::Ident(n) if *n == intern::intern("h")));
            assert!(rest.is_some());
            assert!(
                matches!(&rest.as_deref().unwrap().kind, PatternKind::Ident(n) if *n == intern::intern("t"))
            );
        } else {
            panic!("expected list pattern");
        }
        // Second arm: empty list
        assert!(matches!(&arms[1].pattern.kind, PatternKind::List(pats, None) if pats.is_empty()));
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
        let arm = first_match_arm_of_main(&prog);
        if let PatternKind::Record {
            ref name,
            ref fields,
            has_rest,
            ..
        } = arm.pattern.kind
        {
            assert_eq!(*name, Some(intern::intern("User")));
            assert_eq!(fields.len(), 2);
            assert_eq!(fields[0].0, intern::intern("name"));
            assert!(fields[0].2.is_none()); // shorthand
            assert_eq!(fields[1].0, intern::intern("age"));
            assert!(fields[1].2.is_none());
            assert!(!has_rest);
        } else {
            panic!("expected record pattern");
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
        let expr = last_expr_of_main(&prog);
        if let ExprKind::List(ref elems) = expr.kind {
            assert!(elems.is_empty());
        } else {
            panic!("expected empty list, got {:?}", expr.kind);
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
        let expr = last_expr_of_main(&prog);
        if let ExprKind::Map(ref entries) = expr.kind {
            assert_eq!(entries.len(), 2);
        } else {
            panic!("expected map literal, got {:?}", expr.kind);
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
        let expr = last_expr_of_main(&prog);
        if let ExprKind::SetLit(ref elems) = expr.kind {
            assert_eq!(elems.len(), 3);
        } else {
            panic!("expected set literal, got {:?}", expr.kind);
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
        let expr = last_expr_of_main(&prog);
        assert!(matches!(&expr.kind, ExprKind::Range(_, _)));
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
        let expr = last_expr_of_main(&prog);
        // Should be Pipe(Pipe(a, f), g) — left-associative
        if let ExprKind::Pipe(ref left, ref right) = expr.kind {
            assert!(matches!(&right.kind, ExprKind::Ident(n) if *n == intern::intern("g")));
            assert!(matches!(&left.kind, ExprKind::Pipe(_, _)));
        } else {
            panic!("expected pipe expression, got {:?}", expr.kind);
        }
    }

    #[test]
    fn test_question_mark_wraps_full_pipe() {
        // `x |> f(y)?` must parse as `(x |> f(y))?` — `?` applies to the
        // piped result, not to the inner call `f(y)`.
        let prog = parse(
            r#"
            fn main() {
                a |> f(b)?
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        let expr = last_expr_of_main(&prog);
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

    /// Parse `src` as the body of `main` and return its last expression.
    fn parse_main_expr(src: &str) -> Expr {
        let prog = parse(&format!("fn main() {{\n{src}\n}}\n"));
        last_expr_of_main(&prog).clone()
    }

    #[test]
    fn test_question_mark_is_tight_under_binary_operators() {
        // `?` binds like a call: under every binary operator it unwraps
        // just its own operand.
        for (src, want) in [
            ("a + b?", BinOp::Add),
            ("a * b?", BinOp::Mul),
            ("a == b?", BinOp::Eq),
            ("a && b?", BinOp::And),
            ("a < b?", BinOp::Lt),
        ] {
            let expr = parse_main_expr(src);
            let ExprKind::Binary(_, op, rhs) = &expr.kind else {
                panic!("{src}: expected Binary at top, got {:?}", expr.kind);
            };
            assert_eq!(*op, want, "{src}");
            assert!(
                matches!(&rhs.kind, ExprKind::QuestionMark(_)),
                "{src}: expected `?` on the right operand, got {:?}",
                rhs.kind
            );
        }
    }

    #[test]
    fn test_question_mark_on_both_operands() {
        // `int.parse(a)? + int.parse(b)?` adds two unwrapped values.
        let expr = parse_main_expr("int.parse(a)? + int.parse(b)?");
        let ExprKind::Binary(lhs, BinOp::Add, rhs) = &expr.kind else {
            panic!("expected Add at top, got {:?}", expr.kind);
        };
        assert!(matches!(&lhs.kind, ExprKind::QuestionMark(_)));
        assert!(matches!(&rhs.kind, ExprKind::QuestionMark(_)));
    }

    #[test]
    fn test_question_mark_is_tight_under_range_and_unary() {
        let expr = parse_main_expr("a..b?");
        let ExprKind::Range(_, end) = &expr.kind else {
            panic!("expected Range at top, got {:?}", expr.kind);
        };
        assert!(matches!(&end.kind, ExprKind::QuestionMark(_)));

        let expr = parse_main_expr("-x?");
        let ExprKind::Unary(UnaryOp::Neg, operand) = &expr.kind else {
            panic!("expected Neg at top, got {:?}", expr.kind);
        };
        assert!(matches!(&operand.kind, ExprKind::QuestionMark(_)));
    }

    #[test]
    fn test_question_mark_after_ascription_wraps_it() {
        // `as` takes a type, so a following `?` applies to the
        // ascription: `x as Int?` is `(x as Int)?`.
        let expr = parse_main_expr("x as Int?");
        let ExprKind::QuestionMark(inner) = &expr.kind else {
            panic!("expected `?` at top, got {:?}", expr.kind);
        };
        assert!(matches!(&inner.kind, ExprKind::Ascription(_, _)));
    }

    #[test]
    fn test_question_mark_ends_a_whole_pipeline() {
        // A trailing `?` on a pipeline applies to the whole pipeline.
        let expr = parse_main_expr("x |> f |> g?");
        let ExprKind::QuestionMark(inner) = &expr.kind else {
            panic!("expected `?` at top, got {:?}", expr.kind);
        };
        let ExprKind::Pipe(lhs, _) = &inner.kind else {
            panic!("expected Pipe inside `?`, got {:?}", inner.kind);
        };
        assert!(matches!(&lhs.kind, ExprKind::Pipe(_, _)));
    }

    #[test]
    fn test_question_mark_in_parenthesised_pipe_stage() {
        // Parentheses keep a `?` on the stage itself.
        let expr = parse_main_expr("a |> (f?)");
        let ExprKind::Pipe(_, rhs) = &expr.kind else {
            panic!("expected Pipe at top, got {:?}", expr.kind);
        };
        assert!(matches!(&rhs.kind, ExprKind::QuestionMark(_)));
    }

    #[test]
    fn test_question_mark_inside_a_stage_operand_is_tight() {
        // `?` inside a call argument of a stage is not a stage-ending `?`.
        let expr = parse_main_expr("a |> f(b?)");
        assert!(
            matches!(&expr.kind, ExprKind::Pipe(_, _)),
            "expected Pipe at top, got {:?}",
            expr.kind
        );
    }

    #[test]
    fn test_question_mark_still_binds_before_pipe_when_on_left() {
        // `f(a)? |> g` parses as `(f(a)?) |> g` — `?` binds to the
        // preceding call before the pipe takes its LHS.
        let prog = parse(
            r#"
            fn main() {
                f(a)? |> g
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        let expr = last_expr_of_main(&prog);
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
        let expr = last_expr_of_main(&prog);
        if let ExprKind::Return(ref val) = expr.kind {
            assert!(val.is_some());
        } else {
            panic!("expected return");
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
        let expr = last_expr_of_main(&prog);
        if let ExprKind::Return(ref val) = expr.kind {
            assert!(val.is_none());
        } else {
            panic!("expected return");
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
        let expr = last_expr_of_main(&prog);
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
            matches!(&prog.decls[2], Decl::Import(ImportTarget::Alias(m, a, _), _) if *m == intern::intern("http") && *a == intern::intern("h"))
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
            let wc = &f.where_clauses[0];
            assert_eq!(wc.type_param, intern::intern("x"));
            assert_eq!(wc.trait_name, intern::intern("Display"));
            assert!(wc.trait_args.is_empty());
        } else {
            panic!("expected fn decl");
        }
    }

    #[test]
    fn test_lambda_with_typed_params() {
        let prog = parse(
            r#"
            fn main() {
                { x: Int, y: Int -> x + y }
            }
        "#,
        );
        assert_eq!(prog.decls.len(), 1);
        let expr = last_expr_of_main(&prog);
        if let ExprKind::Lambda { ref params, .. } = expr.kind {
            assert_eq!(params.len(), 2);
            assert!(params[0].ty.is_some());
            assert!(params[1].ty.is_some());
        } else {
            panic!("expected lambda, got {:?}", expr.kind);
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
        let (_, arms) = last_match_of_main(&prog);
        assert_eq!(arms.len(), 5);
        assert!(arms[0].guard.is_some());
        assert!(arms[1].guard.is_none());
        assert!(arms[2].guard.is_some());
        assert!(arms[3].guard.is_some());
        assert!(arms[4].guard.is_none());
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
        let arm = first_match_arm_of_main(&prog);
        if let PatternKind::Record {
            ref name,
            ref fields,
            has_rest,
            ..
        } = arm.pattern.kind
        {
            assert_eq!(*name, Some(intern::intern("User")));
            assert_eq!(fields.len(), 1);
            assert!(has_rest);
        } else {
            panic!("expected record pattern with rest");
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
        let expr = last_expr_of_main(&prog);
        if let ExprKind::Loop { ref bindings, .. } = expr.kind {
            assert!(bindings.is_empty());
        } else {
            panic!("expected loop, got {:?}", expr.kind);
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
        let arm = first_match_arm_of_main(&prog);
        assert!(matches!(&arm.pattern.kind, PatternKind::Range(-10, 10)));
    }

    // ── Headers: an expression that is followed by a block ──────────

    #[test]
    fn test_loop_initialiser_may_end_in_a_constructor() {
        let prog = parse(
            r#"
            fn build(n) {
                loop i = n, acc = Nil {
                    acc
                }
            }
        "#,
        );
        match &last_expr_of_main(&prog).kind {
            ExprKind::Loop { bindings, .. } => {
                assert_eq!(bindings.len(), 2);
                assert!(
                    matches!(&bindings[1].2.kind, ExprKind::Ident(_)),
                    "expected the constructor name, got {:?}",
                    bindings[1].2.kind
                );
            }
            other => panic!("expected loop, got {:?}", other),
        }
    }

    #[test]
    fn test_loop_initialiser_may_be_a_record_literal() {
        let prog = parse(
            r#"
            fn build(n) {
                loop i = n, p = Pt { x: 1 } {
                    p
                }
            }
        "#,
        );
        match &last_expr_of_main(&prog).kind {
            ExprKind::Loop { bindings, .. } => {
                assert_eq!(bindings.len(), 2);
                assert!(
                    matches!(&bindings[1].2.kind, ExprKind::RecordCreate { .. }),
                    "expected a record literal, got {:?}",
                    bindings[1].2.kind
                );
            }
            other => panic!("expected loop, got {:?}", other),
        }
    }

    #[test]
    fn test_trailing_closure_inside_parentheses_in_a_scrutinee() {
        let prog = parse(
            r#"
            fn main() {
                match (list.filter(xs) { x -> x > 1 }) {
                    [] -> "none"
                    _ -> "some"
                }
            }
        "#,
        );
        let (scrutinee, arms) = last_match_of_main(&prog);
        let scrutinee = scrutinee.expect("expected match expression with scrutinee");
        assert!(
            matches!(scrutinee.kind, ExprKind::Call(_, _)),
            "expected Call scrutinee, got {:?}",
            scrutinee.kind
        );
        assert_eq!(arms.len(), 2);
    }

    #[test]
    fn test_match_body_directly_after_a_pipe_operand() {
        let prog = parse(
            r#"
            fn main() {
                match xs |> list.head {
                    Some(x) -> x
                    None -> 0
                }
            }
        "#,
        );
        let (scrutinee, arms) = last_match_of_main(&prog);
        let scrutinee = scrutinee.expect("expected match expression with scrutinee");
        assert!(
            matches!(scrutinee.kind, ExprKind::Pipe(_, _)),
            "expected Pipe scrutinee, got {:?}",
            scrutinee.kind
        );
        assert_eq!(arms.len(), 2);
    }

    #[test]
    fn test_header_context_does_not_outlive_a_parse_error() {
        // The first function fails inside a match scrutinee. The second
        // one must still be allowed its trailing closure.
        let (prog, errors) = parse_recovering(
            r#"
            fn broken() {
                match ) {
                    _ -> 1
                }
            }
            fn fine() {
                list.map(xs) { x -> x + 1 }
            }
        "#,
        );
        assert!(!errors.is_empty());
        let fine = prog
            .decls
            .iter()
            .find_map(|d| match d {
                Decl::Fn(f) if f.name == intern::intern("fine") && !f.is_recovery_stub => Some(f),
                _ => None,
            })
            .expect("`fine` must parse");
        assert!(
            matches!(&fine.body.kind, ExprKind::Block(stmts) if stmts.len() == 1),
            "expected one statement (a call with a trailing closure), got {:?}",
            fine.body.kind
        );
    }
}
