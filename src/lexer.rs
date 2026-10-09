use std::fmt;
use std::ops::Range;

use crate::diagnostic::{Code, Diagnostic};
use crate::intern::{self, Symbol};
use crate::source::{FileId, Span};

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    // Keywords
    Let,
    Fn,
    Type,
    Trait,
    Match,
    When,
    Return,
    Pub,
    Mod,
    Import,
    As,
    Else,
    Where,
    Loop,

    // Literals
    /// An integer literal's magnitude. `Int(i64::MIN)` is the magnitude
    /// 2^63, which no Int has: the parser accepts it only directly after a
    /// minus sign, as the smallest Int.
    Int(i64),
    Float(f64),
    Bool(bool),
    /// A complete string with no interpolation.
    /// The bool is `true` when the source used triple-quote (`"""`) syntax.
    StringLit(String, bool),
    /// Start of an interpolated string (text before first `{`)
    StringStart(String),
    /// Middle segment between `}` and next `{`
    StringMiddle(String),
    /// End segment after last `}` to closing `"`
    StringEnd(String),

    // Identifiers
    Ident(Symbol),

    // Operators
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    EqEq,
    NotEq,
    Lt,
    Gt,
    LtEq,
    GtEq,
    AndAnd,
    OrOr,
    Not,
    Pipe,     // |>
    Bar,      // |
    Question, // ?
    Caret,    // ^
    DotDot,   // ..
    /// `...` — three-dot spread/rest operator used for row-polymorphic
    /// records. Distinct from `..` (DotDot) which is used for ranges and
    /// list spread; the three-dot form is reserved for record/row syntax
    /// (`{...other, age: 30}`, `{name: String, ...r}`, `{name: n, ...rest}`).
    DotDotDot, // ...
    Arrow,    // ->

    // Delimiters
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    HashBrace,   // #{
    HashBracket, // #[

    // Punctuation
    Comma,
    Colon,
    ColonColon, // :: — used for associated-type projection (`Self::Item`,
    // `<a as Trait>::Item`). Lexed as a single 2-char token so the parser
    // can distinguish it from a stray double-colon (`x: : T`) and so the
    // formatter can round-trip the token directly.
    Dot,
    Eq, // =

    Eof,
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Token::Let => write!(f, "let"),
            Token::Fn => write!(f, "fn"),
            Token::Type => write!(f, "type"),
            Token::Trait => write!(f, "trait"),
            Token::Match => write!(f, "match"),
            Token::When => write!(f, "when"),
            Token::Return => write!(f, "return"),
            Token::Pub => write!(f, "pub"),
            Token::Mod => write!(f, "mod"),
            Token::Import => write!(f, "import"),
            Token::As => write!(f, "as"),
            Token::Else => write!(f, "else"),
            Token::Where => write!(f, "where"),
            Token::Loop => write!(f, "loop"),
            Token::Int(n) => write!(f, "{}", n.unsigned_abs()),
            Token::Float(n) => write!(f, "{n}"),
            Token::Bool(b) => write!(f, "{b}"),
            Token::StringLit(s, _) => write!(f, "\"{}\"", escape_control_chars(s)),
            Token::StringStart(s) => write!(f, "\"{}{{", escape_control_chars(s)),
            Token::StringMiddle(s) => write!(f, "}}{}{{", escape_control_chars(s)),
            Token::StringEnd(s) => write!(f, "}}{}\"", escape_control_chars(s)),
            Token::Ident(s) => write!(f, "{s}"),
            Token::Plus => write!(f, "+"),
            Token::Minus => write!(f, "-"),
            Token::Star => write!(f, "*"),
            Token::Slash => write!(f, "/"),
            Token::Percent => write!(f, "%"),
            Token::EqEq => write!(f, "=="),
            Token::NotEq => write!(f, "!="),
            Token::Lt => write!(f, "<"),
            Token::Gt => write!(f, ">"),
            Token::LtEq => write!(f, "<="),
            Token::GtEq => write!(f, ">="),
            Token::AndAnd => write!(f, "&&"),
            Token::OrOr => write!(f, "||"),
            Token::Not => write!(f, "!"),
            Token::Pipe => write!(f, "|>"),
            Token::Bar => write!(f, "|"),
            Token::Question => write!(f, "?"),
            Token::Caret => write!(f, "^"),
            Token::DotDot => write!(f, ".."),
            Token::DotDotDot => write!(f, "..."),
            Token::Arrow => write!(f, "->"),
            Token::LParen => write!(f, "("),
            Token::RParen => write!(f, ")"),
            Token::LBrace => write!(f, "{{"),
            Token::RBrace => write!(f, "}}"),
            Token::LBracket => write!(f, "["),
            Token::RBracket => write!(f, "]"),
            Token::HashBrace => write!(f, "#{{"),
            Token::HashBracket => write!(f, "#["),
            Token::Comma => write!(f, ","),
            Token::Colon => write!(f, ":"),
            Token::ColonColon => write!(f, "::"),
            Token::Dot => write!(f, "."),
            Token::Eq => write!(f, "="),
            Token::Eof => write!(f, "EOF"),
        }
    }
}

/// A token with its place in the source and the trivia in front of it.
#[derive(Debug, Clone, PartialEq)]
pub struct Tok {
    pub kind: Token,
    pub span: Span,
    /// Line breaks between the previous token or comment and this token,
    /// saturating at 2 (0 = same line, 1 = next line, 2 = a blank line).
    pub newlines_before: u8,
    /// Comments between the previous token and this one: a range of
    /// `Lexed::comments`. The end-of-file token carries the last comments
    /// of the file.
    pub comments: Range<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentKind {
    /// `-- ...` up to (not including) the line break.
    Line,
    /// `{- ... -}` with any nesting.
    Block,
}

/// A comment. Its text is `&source[span]`, delimiters included.
#[derive(Debug, Clone, PartialEq)]
pub struct Comment {
    pub kind: CommentKind,
    pub span: Span,
    /// Line breaks between the previous token or comment and this
    /// comment, saturating at 2. A comment is trailing when this is 0 and
    /// a token precedes it; otherwise it leads the next token.
    pub newlines_before: u8,
}

impl Comment {
    /// The comment as written in `source`, the text it was lexed from.
    pub fn text<'a>(&self, source: &'a str) -> &'a str {
        &source[self.span.start as usize..self.span.end as usize]
    }
}

/// What the lexer makes of a file: its tokens, and its comments in
/// source order. Each token names the comments in front of it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Lexed {
    pub tokens: Vec<Tok>,
    pub comments: Vec<Comment>,
}

impl Lexed {
    /// The comments between the token before `tok` and `tok`.
    pub fn comments_before(&self, tok: &Tok) -> &[Comment] {
        &self.comments[tok.comments.start as usize..tok.comments.end as usize]
    }

    /// Whether `tok` starts a line: a line break stands between the
    /// token before it and `tok`, in front of or behind the comments
    /// between them. (A `{- -}` comment that spans lines is no line
    /// break: what follows it stands on the line it ends on.)
    pub fn line_break_before(&self, tok: &Tok) -> bool {
        tok.newlines_before > 0
            || self
                .comments_before(tok)
                .iter()
                .any(|comment| comment.newlines_before > 0)
    }
}

/// A token and where it starts, as the scanners hand it to `tokenize`.
type Scanned = (Token, Span);

/// Authoritative keyword list. Reserved words a user cannot bind as an
/// identifier; each name corresponds to a non-`Bool` match arm in
/// `scan_ident_or_keyword` below.
///
/// Boolean literals (`true`, `false`) are intentionally split out into
/// `KEYWORD_LITERALS` because the lexer emits them as `Token::Bool(_)`
/// rather than a keyword-shaped token — semantically they are literals,
/// even though they are reserved-word-shaped.
///
/// Consumers (LSP completion, LSP rename, etc.) MUST source their
/// keyword lists from these constants instead of hand-rolling parallel
/// arrays. A parity-lock test in
/// `tests/meta/lexer_keyword_parity_tests.rs` asserts this set matches the
/// `match name.as_str()` arms in `scan_ident_or_keyword` and that no
/// LSP module re-introduces a hand-rolled keyword list.
pub const KEYWORDS: &[&str] = &[
    "as", "else", "fn", "import", "let", "loop", "match", "mod", "pub", "return", "trait", "type",
    "when", "where",
];

/// Reserved-word-shaped boolean literals. Lexed as `Token::Bool(_)`,
/// not as keyword tokens, but still un-bindable as identifiers — so
/// surfaces that gate "is this a keyword" must consult both
/// `KEYWORDS` and `KEYWORD_LITERALS` (see `src/lsp/rename.rs`).
pub const KEYWORD_LITERALS: &[&str] = &["true", "false"];

pub struct Lexer {
    file: FileId,
    source: Vec<char>,
    pos: usize,
    byte_offset: usize,
    /// Stack of brace depths at which string interpolations began.
    /// When we encounter `}` and brace_depth matches the top of this stack,
    /// we resume scanning a string instead of emitting RBrace.
    interp_stack: Vec<usize>,
    brace_depth: usize,
    /// Every comment read so far, in source order.
    comments: Vec<Comment>,
    /// Line breaks since the previous token or comment, saturating at 2.
    gap_newlines: u8,
    /// The first comment in `comments` that no token has taken yet.
    gap_comments: u32,
}

impl Lexer {
    /// A lexer for `source`, the text of the file `file`.
    pub fn new(file: FileId, source: &str) -> Self {
        let mut lexer = Self {
            file,
            source: source.chars().collect(),
            pos: 0,
            byte_offset: 0,
            interp_stack: Vec::new(),
            brace_depth: 0,
            comments: Vec::new(),
            gap_newlines: 0,
            gap_comments: 0,
        };
        // Skip a single leading UTF-8 BOM (U+FEFF) — Windows tools
        // (Notepad, PowerShell `>` redirects) prepend one by default,
        // and rustc likewise accepts it. We *skip* rather than strip so
        // byte offsets stay relative to the original source string; the
        // BOM counts as the first column of line 1, just like any other
        // skipped character. A BOM anywhere else
        // in the file is still an error (reported by name, since the
        // character itself is zero-width and invisible).
        if lexer.peek() == Some('\u{FEFF}') {
            lexer.advance_char();
        }
        lexer
    }

    pub fn tokenize(&mut self) -> Result<Lexed, Diagnostic> {
        let mut tokens = Vec::new();
        loop {
            let (kind, start) = self.next_token()?;
            // Every scan stops right after its token, so the token ends
            // where the lexer stands now.
            let span = Span {
                end: self.byte_offset as u32,
                ..start
            };
            let is_eof = kind == Token::Eof;
            let end = self.comments.len() as u32;
            let comments = self.gap_comments..end;
            self.gap_comments = end;
            tokens.push(Tok {
                kind,
                span,
                newlines_before: std::mem::take(&mut self.gap_newlines),
                comments,
            });
            if is_eof {
                break;
            }
        }
        Ok(Lexed {
            tokens,
            comments: std::mem::take(&mut self.comments),
        })
    }

    /// Record the comment that starts at `start` and ends at the current
    /// position.
    fn record_comment(&mut self, kind: CommentKind, start: Span) {
        self.comments.push(Comment {
            kind,
            span: self.since(start),
            newlines_before: std::mem::take(&mut self.gap_newlines),
        });
    }

    /// The span from the start of `start` to the current position: a
    /// token the lexer has just read.
    fn since(&self, start: Span) -> Span {
        Span {
            end: self.byte_offset as u32,
            ..start
        }
    }

    /// The empty span at the current position.
    fn span(&self) -> Span {
        Span::point(self.file, self.byte_offset as u32)
    }

    fn peek(&self) -> Option<char> {
        self.source.get(self.pos).copied()
    }

    fn peek_ahead(&self, offset: usize) -> Option<char> {
        self.source.get(self.pos + offset).copied()
    }

    fn advance_char(&mut self) -> Option<char> {
        let ch = self.source.get(self.pos).copied()?;
        self.pos += 1;
        self.byte_offset += ch.len_utf8();
        Some(ch)
    }

    fn skip_whitespace(&mut self) {
        while let Some(ch) = self.peek() {
            match ch {
                ' ' | '\t' | '\r' => {
                    self.advance_char();
                }
                '\n' => {
                    self.gap_newlines = (self.gap_newlines + 1).min(2);
                    self.advance_char();
                }
                _ => break,
            }
        }
    }

    fn skip_line_comment(&mut self) {
        while let Some(ch) = self.peek() {
            if ch == '\n' {
                break;
            }
            self.advance_char();
        }
    }

    fn skip_block_comment(&mut self) -> Result<(), Diagnostic> {
        // We've already consumed `{-`
        let mut depth = 1;
        let start = self.span();
        while depth > 0 {
            match self.advance_char() {
                Some('{') if self.peek() == Some('-') => {
                    self.advance_char();
                    depth += 1;
                }
                Some('-') if self.peek() == Some('}') => {
                    self.advance_char();
                    depth -= 1;
                }
                Some(_) => {}
                None => {
                    return Err(Diagnostic::error(
                        Code::UnterminatedComment,
                        start,
                        "unterminated block comment",
                    ));
                }
            }
        }
        Ok(())
    }

    fn scan_string(&mut self, is_continuation: bool, start: Span) -> Result<Scanned, Diagnostic> {
        let mut text = String::new();

        loop {
            match self.peek() {
                None => {
                    let message = if !self.interp_stack.is_empty() {
                        "unterminated string interpolation; use \\{ for a literal brace".to_string()
                    } else {
                        "unterminated string".to_string()
                    };
                    return Err(Diagnostic::error(Code::UnterminatedString, start, message));
                }
                Some('\\') => {
                    // Capture the backslash's position BEFORE consuming
                    // it: the unknown-escape diagnostic below must anchor
                    // its caret on `\`, not two columns past the sequence
                    // (rustc anchors bad escapes the same way).
                    let esc_span = self.span();
                    self.advance_char();
                    match self.advance_char() {
                        Some('n') => text.push('\n'),
                        Some('t') => text.push('\t'),
                        Some('\\') => text.push('\\'),
                        Some('"') => text.push('"'),
                        Some('{') => text.push('{'),
                        Some('}') => text.push('}'),
                        // Control chars (CR, TAB, U+0001, …) garble or
                        // vanish when echoed raw — CR returns the terminal
                        // cursor to column 0 mid-line, U+0001 is invisible
                        // — so escape them via `escape_default` (`\r`,
                        // `\u{1}`, …), mirroring the round-100 fix to the
                        // `unexpected character` catch-all below. Printable
                        // unknown escapes (`\q`, …) keep their plain form.
                        Some(c) if c.is_control() => {
                            return Err(Diagnostic::error(
                                Code::InvalidEscape,
                                self.span(),
                                format!("unknown escape sequence: \\{}", c.escape_default()),
                            ));
                        }
                        Some(c) => {
                            return Err(Diagnostic::error(
                                Code::InvalidEscape,
                                esc_span,
                                format!("unknown escape sequence: \\{c}"),
                            ));
                        }
                        None => {
                            return Err(Diagnostic::error(
                                Code::UnterminatedString,
                                start,
                                "unterminated escape sequence",
                            ));
                        }
                    }
                }
                Some('{') => {
                    self.advance_char(); // consume `{`
                    self.interp_stack.push(self.brace_depth);
                    self.brace_depth += 1; // track interpolation brace
                    let tok = if is_continuation {
                        Token::StringMiddle(text)
                    } else {
                        Token::StringStart(text)
                    };
                    return Ok((tok, start));
                }
                Some('"') => {
                    self.advance_char(); // consume closing `"`
                    let tok = if is_continuation {
                        Token::StringEnd(text)
                    } else {
                        Token::StringLit(text, false)
                    };
                    return Ok((tok, start));
                }
                Some(ch) => {
                    self.advance_char();
                    text.push(ch);
                }
            }
        }
    }

    fn scan_triple_string(&mut self, start: Span) -> Result<Scanned, Diagnostic> {
        // We've already consumed the opening `"""`.
        // Read raw content until closing `"""`.
        // No escape processing, no interpolation.
        let mut raw = String::new();

        loop {
            match self.peek() {
                None => {
                    return Err(Diagnostic::error(
                        Code::UnterminatedString,
                        start,
                        "unterminated triple-quoted string",
                    ));
                }
                Some('"') if self.peek_ahead(1) == Some('"') && self.peek_ahead(2) == Some('"') => {
                    // Consume closing """
                    self.advance_char();
                    self.advance_char();
                    self.advance_char();
                    break;
                }
                Some(ch) => {
                    self.advance_char();
                    raw.push(ch);
                }
            }
        }

        // Apply indentation stripping.
        let result = Self::strip_triple_string_indentation(&raw);
        Ok((Token::StringLit(result, true), start))
    }

    /// Strip indentation from a triple-quoted string based on the closing `"""`
    /// position. The algorithm:
    /// 1. Split the raw content into lines
    /// 2. The last line (before closing `"""`) determines the indentation prefix
    /// 3. Strip that prefix from each content line
    /// 4. Remove the first line if it is blank (after opening `"""`)
    /// 5. Remove the last line if it is blank (before closing `"""`)
    fn strip_triple_string_indentation(raw: &str) -> String {
        let lines: Vec<&str> = raw.split('\n').collect();

        if lines.is_empty() {
            return String::new();
        }

        // Determine indentation from the last line (before closing """)
        let last_line = lines[lines.len() - 1];
        let indent = if last_line.chars().all(|c| c == ' ' || c == '\t') {
            last_line.len()
        } else {
            0
        };

        let mut result_lines: Vec<&str> = Vec::new();
        for line in &lines {
            let bytes = line.as_bytes();
            // `indent` is a byte count derived from a last_line that is
            // entirely ASCII whitespace, so any matching whitespace
            // prefix on another line is also ASCII and byte-equals
            // char-indexed length. Check the prefix at the byte level —
            // safe even when subsequent content contains multi-byte
            // characters, because we never split inside one. Falling
            // through to `push(line)` preserves a line whose leading
            // bytes aren't all ASCII whitespace (e.g. a line starting
            // with a box-drawing character), which previously panicked
            // on a mid-char `split_at(indent)`.
            if bytes.len() >= indent && bytes[..indent].iter().all(|&b| b == b' ' || b == b'\t') {
                result_lines.push(&line[indent..]);
            } else if bytes.iter().all(|&b| b == b' ' || b == b'\t') {
                // Line is shorter than indent (or equal) and contains
                // only ASCII whitespace — treat as blank.
                result_lines.push("");
            } else {
                result_lines.push(line);
            }
        }

        // Remove first line if blank (right after opening """)
        if !result_lines.is_empty() && result_lines[0].is_empty() {
            result_lines.remove(0);
        }

        // Remove last line if blank (right before closing """)
        if !result_lines.is_empty() && result_lines[result_lines.len() - 1].is_empty() {
            result_lines.pop();
        }

        result_lines.join("\n")
    }

    fn scan_number(&mut self, first: char, start: Span) -> Result<Scanned, Diagnostic> {
        // Handle hex (0x) and binary (0b) prefixes
        if first == '0'
            && let Some(prefix) = self.peek()
        {
            if prefix == 'x' || prefix == 'X' {
                self.advance_char(); // consume 'x'
                return self.scan_hex_int(start);
            }
            if prefix == 'b' || prefix == 'B' {
                self.advance_char(); // consume 'b'
                return self.scan_binary_int(start);
            }
        }

        let mut num = String::new();
        num.push(first);

        while let Some(ch) = self.peek() {
            if ch.is_ascii_digit() || ch == '_' {
                self.advance_char();
                if ch != '_' {
                    num.push(ch);
                }
            } else {
                break;
            }
        }

        let mut is_float = false;

        // Check for float: `.` followed by a digit (not `..` for range)
        if self.peek() == Some('.') && self.peek_ahead(1).is_some_and(|c| c.is_ascii_digit()) {
            is_float = true;
            self.advance_char(); // consume `.`
            num.push('.');
            while let Some(ch) = self.peek() {
                if ch.is_ascii_digit() || ch == '_' {
                    self.advance_char();
                    if ch != '_' {
                        num.push(ch);
                    }
                } else {
                    break;
                }
            }
        }

        // Check for scientific notation: e/E followed by optional +/- and digits
        // Scientific notation always produces a Float
        if let Some(e) = self.peek()
            && (e == 'e' || e == 'E')
        {
            is_float = true;
            self.advance_char(); // consume 'e'
            num.push('e');
            // Optional sign
            if let Some(sign) = self.peek()
                && (sign == '+' || sign == '-')
            {
                self.advance_char();
                num.push(sign);
            }
            // Must have at least one digit after e
            if !self.peek().is_some_and(|c| c.is_ascii_digit()) {
                return Err(Diagnostic::error(
                    Code::InvalidNumber,
                    self.since(start),
                    "expected digit after exponent",
                ));
            }
            while let Some(ch) = self.peek() {
                if ch.is_ascii_digit() || ch == '_' {
                    self.advance_char();
                    if ch != '_' {
                        num.push(ch);
                    }
                } else {
                    break;
                }
            }
        }

        if is_float {
            let val: f64 = num.parse().map_err(|_| {
                Diagnostic::error(
                    Code::InvalidNumber,
                    self.since(start),
                    "number literal too large",
                )
            })?;
            if !val.is_finite() {
                return Err(Diagnostic::error(
                    Code::InvalidNumber,
                    self.since(start),
                    "number literal out of range (not finite)",
                ));
            }
            Ok((Token::Float(val), start))
        } else {
            let val = int_magnitude(&num, 10).ok_or_else(|| {
                Diagnostic::error(
                    Code::InvalidNumber,
                    self.since(start),
                    "number literal too large",
                )
            })?;
            Ok((Token::Int(val), start))
        }
    }

    fn scan_hex_int(&mut self, start: Span) -> Result<Scanned, Diagnostic> {
        let mut digits = String::new();
        while let Some(ch) = self.peek() {
            if ch.is_ascii_hexdigit() || ch == '_' {
                self.advance_char();
                if ch != '_' {
                    digits.push(ch);
                }
            } else {
                break;
            }
        }
        if digits.is_empty() {
            return Err(Diagnostic::error(
                Code::InvalidNumber,
                self.since(start),
                "expected hex digit after 0x",
            ));
        }
        let val = int_magnitude(&digits, 16).ok_or_else(|| {
            Diagnostic::error(
                Code::InvalidNumber,
                self.since(start),
                "hex literal too large",
            )
        })?;
        Ok((Token::Int(val), start))
    }

    fn scan_binary_int(&mut self, start: Span) -> Result<Scanned, Diagnostic> {
        let mut digits = String::new();
        while let Some(ch) = self.peek() {
            if ch == '0' || ch == '1' || ch == '_' {
                self.advance_char();
                if ch != '_' {
                    digits.push(ch);
                }
            } else {
                break;
            }
        }
        if digits.is_empty() {
            return Err(Diagnostic::error(
                Code::InvalidNumber,
                self.since(start),
                "expected binary digit after 0b",
            ));
        }
        let val = int_magnitude(&digits, 2).ok_or_else(|| {
            Diagnostic::error(
                Code::InvalidNumber,
                self.since(start),
                "binary literal too large",
            )
        })?;
        Ok((Token::Int(val), start))
    }

    fn scan_ident_or_keyword(&mut self, first: char, start: Span) -> Scanned {
        let mut name = String::new();
        name.push(first);

        while let Some(ch) = self.peek() {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                self.advance_char();
                name.push(ch);
            } else {
                break;
            }
        }

        let tok = match name.as_str() {
            "let" => Token::Let,
            "fn" => Token::Fn,
            "type" => Token::Type,
            "trait" => Token::Trait,
            "match" => Token::Match,
            "when" => Token::When,
            "return" => Token::Return,
            // "select" is no longer a keyword; it's now channel.select
            "pub" => Token::Pub,
            "mod" => Token::Mod,
            "import" => Token::Import,
            "as" => Token::As,
            "else" => Token::Else,
            "where" => Token::Where,
            "loop" => Token::Loop,
            "true" => Token::Bool(true),
            "false" => Token::Bool(false),
            _ => Token::Ident(intern::intern(&name)),
        };
        (tok, start)
    }

    fn next_token(&mut self) -> Result<Scanned, Diagnostic> {
        // Skip whitespace, counting the line breaks.
        self.skip_whitespace();

        // Skip comments (may require multiple passes if comment is followed by whitespace/comment)
        loop {
            match (self.peek(), self.peek_ahead(1)) {
                (Some('-'), Some('-')) => {
                    let comment_span = self.span();
                    self.skip_line_comment();
                    self.record_comment(CommentKind::Line, comment_span);
                    self.skip_whitespace();
                    continue;
                }
                (Some('{'), Some('-')) => {
                    let comment_span = self.span();
                    self.advance_char();
                    self.advance_char();
                    self.skip_block_comment()?;
                    self.record_comment(CommentKind::Block, comment_span);
                    self.skip_whitespace();
                    continue;
                }
                _ => break,
            }
        }

        let start = self.span();

        // Check if we're at EOF
        let Some(ch) = self.advance_char() else {
            return Ok((Token::Eof, start));
        };

        match ch {
            // String (triple-quoted or regular)
            '"' => {
                if self.peek() == Some('"') && self.peek_ahead(1) == Some('"') {
                    self.advance_char(); // consume second "
                    self.advance_char(); // consume third "
                    self.scan_triple_string(start)
                } else {
                    self.scan_string(false, start)
                }
            }

            // Numbers
            '0'..='9' => self.scan_number(ch, start),

            // Identifiers and keywords
            'a'..='z' | 'A'..='Z' | '_' => Ok(self.scan_ident_or_keyword(ch, start)),

            // Operators and punctuation
            '+' => Ok((Token::Plus, start)),
            '*' => Ok((Token::Star, start)),
            '%' => Ok((Token::Percent, start)),
            '?' => Ok((Token::Question, start)),
            '^' => Ok((Token::Caret, start)),
            ',' => Ok((Token::Comma, start)),
            ':' => {
                // Associated-type projection: `Self::Item` and
                // `<a as Trait>::Item` use `::` as a 2-char token. A
                // single `:` continues to mean record/field/where-clause
                // separator. Lookahead disambiguates without disturbing
                // any existing single-`:` site.
                if self.peek() == Some(':') {
                    self.advance_char();
                    Ok((Token::ColonColon, start))
                } else {
                    Ok((Token::Colon, start))
                }
            }
            '(' => Ok((Token::LParen, start)),
            ')' => Ok((Token::RParen, start)),
            '[' => Ok((Token::LBracket, start)),
            ']' => Ok((Token::RBracket, start)),

            '#' if self.peek() == Some('{') => {
                self.advance_char();
                self.brace_depth += 1;
                Ok((Token::HashBrace, start))
            }

            '#' if self.peek() == Some('[') => {
                self.advance_char();
                Ok((Token::HashBracket, start))
            }

            '{' => {
                self.brace_depth += 1;
                Ok((Token::LBrace, start))
            }

            '}' => {
                // Check if this closes a string interpolation
                if let Some(&interp_depth) = self.interp_stack.last()
                    && self.brace_depth == interp_depth + 1
                {
                    self.interp_stack.pop();
                    self.brace_depth -= 1;
                    // Resume scanning the string
                    let cont_start = self.span();
                    return self.scan_string(true, cont_start);
                }
                self.brace_depth = self.brace_depth.saturating_sub(1);
                Ok((Token::RBrace, start))
            }

            '-' => {
                if self.peek() == Some('>') {
                    self.advance_char();
                    Ok((Token::Arrow, start))
                } else if self.peek() == Some('-') {
                    // Line comment — shouldn't happen here since we skip comments above,
                    // but handle it just in case
                    self.skip_line_comment();
                    self.record_comment(CommentKind::Line, start);
                    self.next_token()
                } else {
                    Ok((Token::Minus, start))
                }
            }

            '/' => Ok((Token::Slash, start)),

            '.' => {
                if self.peek() == Some('.') {
                    self.advance_char();
                    if self.peek() == Some('.') {
                        self.advance_char();
                        Ok((Token::DotDotDot, start))
                    } else {
                        Ok((Token::DotDot, start))
                    }
                } else {
                    Ok((Token::Dot, start))
                }
            }

            '=' => {
                if self.peek() == Some('=') {
                    self.advance_char();
                    Ok((Token::EqEq, start))
                } else {
                    Ok((Token::Eq, start))
                }
            }

            '!' => {
                if self.peek() == Some('=') {
                    self.advance_char();
                    Ok((Token::NotEq, start))
                } else {
                    Ok((Token::Not, start))
                }
            }

            '<' => {
                if self.peek() == Some('=') {
                    self.advance_char();
                    Ok((Token::LtEq, start))
                } else {
                    Ok((Token::Lt, start))
                }
            }

            '>' => {
                if self.peek() == Some('=') {
                    self.advance_char();
                    Ok((Token::GtEq, start))
                } else {
                    Ok((Token::Gt, start))
                }
            }

            '|' => {
                if self.peek() == Some('>') {
                    self.advance_char();
                    Ok((Token::Pipe, start))
                } else if self.peek() == Some('|') {
                    self.advance_char();
                    Ok((Token::OrOr, start))
                } else {
                    Ok((Token::Bar, start))
                }
            }

            '&' => {
                if self.peek() == Some('&') {
                    self.advance_char();
                    Ok((Token::AndAnd, start))
                } else {
                    Err(Diagnostic::error(
                        Code::UnexpectedChar,
                        self.since(start),
                        "unexpected character '&', did you mean '&&'?",
                    ))
                }
            }

            ';' => Err(Diagnostic::error(
                Code::UnexpectedChar,
                self.since(start),
                "semicolons are not used in silt — use a newline to separate statements",
            )),
            // A BOM after the start of the file (the leading one is
            // skipped in `Lexer::new`) is invisible and zero-width, so
            // quoting the raw char would render an empty-looking error.
            // Name it instead.
            '\u{FEFF}' => Err(Diagnostic::error(
                Code::UnexpectedChar,
                self.since(start),
                "unexpected character: byte-order mark (U+FEFF); \
                          a BOM is only permitted at the very start of the file",
            )),
            // ASCII/Unicode control characters (U+0001–U+001F, U+007F,
            // …) are invisible, so quoting the raw byte renders an
            // empty-looking error in any non-`cat -v` sink (log files,
            // LSP JSON diagnostics, captured test output). Escape them —
            // the same reasoning the BOM arm above applies, generalised
            // to every invisible control char. `escape_default` yields
            // `\u{1}`, `\t`, etc.; printable chars (`@`, …) are not
            // control and keep their plain quoted form.
            _ if ch.is_control() => Err(Diagnostic::error(
                Code::UnexpectedChar,
                self.since(start),
                format!("unexpected character: '{}'", ch.escape_default()),
            )),
            // A letter or digit outside ASCII, at the start of a name or
            // inside one (`café` ends at the `f`).
            _ if ch.is_alphanumeric() => Err(Diagnostic::error(
                Code::UnexpectedChar,
                self.since(start),
                format!(
                    "unexpected character: '{ch}'; a name is made of ASCII letters, digits and '_'"
                ),
            )),
            _ => Err(Diagnostic::error(
                Code::UnexpectedChar,
                self.since(start),
                format!("unexpected character: '{ch}'"),
            )),
        }
    }
}

/// Escape control characters in a string-token payload for `Token`'s
/// `Display` impl (`StringLit`/`StringStart`/`StringMiddle`/`StringEnd`).
///
/// `scan_string`/`scan_triple_string` accept raw control bytes and raw
/// newlines inside string bodies, so without this gate a parser
/// diagnostic like `expected declaration, found "a\u{1}b"` would embed
/// the raw byte (an invisible offender in logs/LSP JSON) and a raw
/// newline would split the quoted found-token across the rendered
/// header and its `= note:` continuation line. Only control characters
/// (`char::is_control`: C0 incl. `\n`/`\t`/`\r`, DEL, C1) are escaped
/// via `escape_default`; printable non-ASCII passes through unchanged.
/// This mirrors the `unexpected character` control-char escape in
/// `scan_token` above. Safe to apply here: `Token`'s `Display` is
/// consumed only by parser diagnostics — the formatter renders from
/// the AST and pattern-matches token variants, and fuzz invariants use
/// `Debug`.
/// The value of an integer literal's digits (underscores removed). The
/// magnitude 2^63 is `i64::MIN` (see `Token::Int`); anything larger is too
/// large.
fn int_magnitude(digits: &str, radix: u32) -> Option<i64> {
    const MIN_MAGNITUDE: u64 = i64::MIN.unsigned_abs();
    match u64::from_str_radix(digits, radix) {
        Ok(MIN_MAGNITUDE) => Some(i64::MIN),
        Ok(n) => i64::try_from(n).ok(),
        Err(_) => None,
    }
}

fn escape_control_chars(s: &str) -> std::borrow::Cow<'_, str> {
    if !s.chars().any(char::is_control) {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 8);
    for ch in s.chars() {
        if ch.is_control() {
            out.extend(ch.escape_default());
        } else {
            out.push(ch);
        }
    }
    std::borrow::Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex(input: &str) -> Vec<Token> {
        Lexer::new(crate::source::FileId::default(), input)
            .tokenize()
            .unwrap()
            .tokens
            .into_iter()
            .map(|tok| tok.kind)
            .filter(|tok| !matches!(tok, Token::Eof))
            .collect()
    }

    /// The trivia of `input`, one item per token:
    /// its comments as `<text>^n`, then the token as `text^n`, where `n`
    /// is `newlines_before`.
    fn trivia(input: &str) -> String {
        let lexed = Lexer::new(crate::source::FileId::default(), input)
            .tokenize()
            .unwrap();
        let mut seen = 0;
        let mut out = Vec::new();
        for tok in &lexed.tokens {
            // The ranges follow each other and leave no comment out.
            assert_eq!(tok.comments.start, seen);
            seen = tok.comments.end;
            for comment in lexed.comments_before(tok) {
                let text = comment.text(input);
                match comment.kind {
                    CommentKind::Line => assert!(text.starts_with("--") && !text.contains('\n')),
                    CommentKind::Block => assert!(text.starts_with("{-") && text.ends_with("-}")),
                }
                out.push(format!("<{text}>^{}", comment.newlines_before));
            }
            let text = match tok.kind {
                Token::Eof => "EOF",
                _ => &input[tok.span.start as usize..tok.span.end as usize],
            };
            out.push(format!("{text}^{}", tok.newlines_before));
        }
        assert_eq!(seen as usize, lexed.comments.len());
        out.join(" ")
    }

    #[test]
    fn test_trivia() {
        let cases: &[(&str, &str)] = &[
            ("", "EOF^0"),
            ("a", "a^0 EOF^0"),
            ("a b", "a^0 b^0 EOF^0"),
            ("a\nb", "a^0 b^1 EOF^0"),
            ("a\n\nb", "a^0 b^2 EOF^0"),
            // The count saturates at 2.
            ("a\n\n\n\nb", "a^0 b^2 EOF^0"),
            ("a\r\n\r\nb", "a^0 b^2 EOF^0"),
            ("\n\na", "a^2 EOF^0"),
            ("a\n", "a^0 EOF^1"),
            ("a\n\n", "a^0 EOF^2"),
            // The end-of-file token carries the last comments.
            ("-- c", "<-- c>^0 EOF^0"),
            ("-- c\na", "<-- c>^0 a^1 EOF^0"),
            ("a -- c", "a^0 <-- c>^0 EOF^0"),
            ("a -- c\n", "a^0 <-- c>^0 EOF^1"),
            ("a\n\n-- c\n{- d -}", "a^0 <-- c>^2 <{- d -}>^1 EOF^0"),
            // Trailing (0 after a token) and leading comments; a token
            // counts its line breaks from the comment before it.
            ("a -- c\nb", "a^0 <-- c>^0 b^1 EOF^0"),
            ("a\n-- c\nb", "a^0 <-- c>^1 b^1 EOF^0"),
            ("a\n\n-- c\n\nb", "a^0 <-- c>^2 b^2 EOF^0"),
            (
                "a -- c\n  -- d\n\n  -- e\nb",
                "a^0 <-- c>^0 <-- d>^1 <-- e>^2 b^1 EOF^0",
            ),
            ("a {- c -} b", "a^0 <{- c -}>^0 b^0 EOF^0"),
            // A line break inside a block comment is not a line break
            // between tokens.
            ("a {- c\n d -} b", "a^0 <{- c\n d -}>^0 b^0 EOF^0"),
            ("a\n{- c {- d -} -}\nb", "a^0 <{- c {- d -} -}>^1 b^1 EOF^0"),
            (
                "f({- c -}x, -- d\n  y)",
                "f^0 (^0 <{- c -}>^0 x^0 ,^0 <-- d>^0 y^1 )^0 EOF^0",
            ),
            // Comment markers in a string are text; in an interpolation
            // hole they are comments.
            (
                "\"s -- no \\{- no -\\}\" -- c",
                "\"s -- no \\{- no -\\}\"^0 <-- c>^0 EOF^0",
            ),
            ("\"a{ x -- c\n }b\"", "\"a{^0 x^0 <-- c>^0 b\"^1 EOF^0"),
            ("\u{FEFF}-- c\na", "<-- c>^0 a^1 EOF^0"),
        ];
        for (input, expected) in cases {
            assert_eq!(trivia(input), *expected, "input: {input:?}");
        }
    }

    #[test]
    fn test_basic_tokens() {
        assert_eq!(
            lex("let x = 42"),
            vec![
                Token::Let,
                Token::Ident(intern::intern("x")),
                Token::Eq,
                Token::Int(42),
            ]
        );
    }

    #[test]
    fn test_operators() {
        assert_eq!(
            lex("|> -> .. == != <= >="),
            vec![
                Token::Pipe,
                Token::Arrow,
                Token::DotDot,
                Token::EqEq,
                Token::NotEq,
                Token::LtEq,
                Token::GtEq,
            ]
        );
    }

    #[test]
    fn test_string_simple() {
        assert_eq!(
            lex(r#""hello""#),
            vec![Token::StringLit("hello".into(), false)]
        );
    }

    #[test]
    fn test_string_interpolation() {
        let tokens = lex(r#""hello {name}""#);
        assert_eq!(
            tokens,
            vec![
                Token::StringStart("hello ".into()),
                Token::Ident(intern::intern("name")),
                Token::StringEnd(String::new()),
            ]
        );
    }

    #[test]
    fn test_string_multi_interp() {
        let tokens = lex(r#""a {x} b {y} c""#);
        assert_eq!(
            tokens,
            vec![
                Token::StringStart("a ".into()),
                Token::Ident(intern::intern("x")),
                Token::StringMiddle(" b ".into()),
                Token::Ident(intern::intern("y")),
                Token::StringEnd(" c".into()),
            ]
        );
    }

    #[test]
    fn test_number_and_range() {
        assert_eq!(
            lex("1..101"),
            vec![Token::Int(1), Token::DotDot, Token::Int(101)]
        );
    }

    #[test]
    #[allow(clippy::approx_constant)]
    fn test_float() {
        assert_eq!(lex("3.14"), vec![Token::Float(3.14)]);
    }

    #[test]
    fn test_line_comment() {
        assert_eq!(lex("42 -- comment"), vec![Token::Int(42)]);
    }

    #[test]
    fn test_block_comment() {
        assert_eq!(lex("{- comment -} 42"), vec![Token::Int(42)]);
    }

    #[test]
    fn test_nested_block_comment() {
        assert_eq!(lex("{- outer {- inner -} -} 42"), vec![Token::Int(42)]);
    }

    #[test]
    fn test_keywords() {
        assert_eq!(
            lex("fn let match when return"),
            vec![
                Token::Fn,
                Token::Let,
                Token::Match,
                Token::When,
                Token::Return,
            ]
        );
    }

    #[test]
    fn test_hash_brace() {
        assert_eq!(lex("#{ }"), vec![Token::HashBrace, Token::RBrace]);
    }

    #[test]
    fn test_escaped_brace_in_string() {
        assert_eq!(
            lex(r#""\{not interp\}""#),
            vec![Token::StringLit("{not interp}".into(), false),]
        );
    }

    #[test]
    fn test_where_keyword() {
        assert_eq!(lex("where"), vec![Token::Where]);
    }

    #[test]
    fn test_triple_quoted_basic() {
        assert_eq!(
            lex(r#""""hello""""#),
            vec![Token::StringLit("hello".into(), true)]
        );
    }

    #[test]
    fn test_triple_quoted_multiline_with_indent_stripping() {
        let input = "    let x = \"\"\"\n      hello\n      world\n      \"\"\"";
        let tokens = lex(input);
        assert_eq!(
            tokens,
            vec![
                Token::Let,
                Token::Ident(intern::intern("x")),
                Token::Eq,
                Token::StringLit("hello\nworld".into(), true),
            ]
        );
    }

    #[test]
    fn test_triple_quoted_embedded_quotes() {
        let input = "\"\"\"she said \"hi\" to me\"\"\"";
        let tokens = lex(input);
        assert_eq!(
            tokens,
            vec![Token::StringLit("she said \"hi\" to me".into(), true),]
        );
    }

    #[test]
    fn test_triple_quoted_no_interpolation() {
        let input = "\"\"\"{name} and {age}\"\"\"";
        let tokens = lex(input);
        assert_eq!(
            tokens,
            vec![Token::StringLit("{name} and {age}".into(), true),]
        );
    }

    #[test]
    fn test_triple_quoted_no_escape_processing() {
        let input = r#""""\n\t\\""" "#;
        let tokens = lex(input);
        assert_eq!(tokens, vec![Token::StringLit(r"\n\t\\".into(), true),]);
    }

    #[test]
    fn test_triple_quoted_empty() {
        assert_eq!(lex(r#""""""""#), vec![Token::StringLit("".into(), true)]);
    }

    #[test]
    fn test_triple_quoted_json_example() {
        // Simulates the motivating use case
        let input = "let json = \"\"\"\n  {\n    \"name\": \"Alice\"\n  }\n  \"\"\"";
        let tokens = lex(input);
        assert_eq!(
            tokens,
            vec![
                Token::Let,
                Token::Ident(intern::intern("json")),
                Token::Eq,
                Token::StringLit("{\n  \"name\": \"Alice\"\n}".into(), true),
            ]
        );
    }

    #[test]
    fn test_triple_quoted_preserves_internal_indentation() {
        // Closing """ has 4 spaces of indent; content lines have 4+ spaces
        let input = "    \"\"\"\n    line1\n      indented\n    line3\n    \"\"\"";
        let tokens = lex(input);
        assert_eq!(
            tokens,
            vec![Token::StringLit("line1\n  indented\nline3".into(), true),]
        );
    }

    #[test]
    fn test_triple_quoted_single_line_content() {
        // Opening and content on separate lines but single content line
        let input = "\"\"\"\nhello\n\"\"\"";
        let tokens = lex(input);
        assert_eq!(tokens, vec![Token::StringLit("hello".into(), true),]);
    }

    #[test]
    fn test_triple_quoted_line_starting_with_multibyte_char_does_not_panic() {
        // Regression lock for a fuzz-discovered lexer panic: when a
        // content line starts with a multi-byte character (e.g. `─`,
        // U+2500, 3 UTF-8 bytes) and the closing `"""` indent would
        // fall mid-character, `strip_triple_string_indentation` used
        // to call `split_at(indent)` and crash with "byte index N is
        // not a char boundary". The line should just be kept as-is
        // since its leading bytes aren't ASCII whitespace.
        let input = "\"\"\"\n─x\n  \"\"\"";
        let tokens = lex(input);
        assert_eq!(tokens, vec![Token::StringLit("─x".into(), true)]);
    }

    #[test]
    fn test_hex_literal() {
        assert_eq!(lex("0xFF"), vec![Token::Int(255)]);
        assert_eq!(lex("0x1A"), vec![Token::Int(26)]);
        assert_eq!(lex("0X10"), vec![Token::Int(16)]);
        assert_eq!(lex("0x00"), vec![Token::Int(0)]);
    }

    #[test]
    fn test_smallest_int_magnitude() {
        // 2^63 in every spelling is the one token the parser accepts only
        // behind a minus sign; one more is too large.
        for src in [
            "9223372036854775808",
            "9_223_372_036_854_775_808",
            "0x8000000000000000",
            "0b1000000000000000000000000000000000000000000000000000000000000000",
        ] {
            assert_eq!(lex(src), vec![Token::Int(i64::MIN)], "{src}");
        }
        assert_eq!(Token::Int(i64::MIN).to_string(), "9223372036854775808");
        for src in ["9223372036854775809", "0x8000000000000001"] {
            assert!(
                Lexer::new(crate::source::FileId::default(), src)
                    .tokenize()
                    .is_err(),
                "{src}"
            );
        }
    }

    #[test]
    fn test_hex_with_underscores() {
        assert_eq!(lex("0xFF_FF"), vec![Token::Int(0xFFFF)]);
    }

    #[test]
    fn test_binary_literal() {
        assert_eq!(lex("0b1010"), vec![Token::Int(10)]);
        assert_eq!(lex("0B110"), vec![Token::Int(6)]);
        assert_eq!(lex("0b0"), vec![Token::Int(0)]);
    }

    #[test]
    fn test_binary_with_underscores() {
        assert_eq!(lex("0b1111_0000"), vec![Token::Int(0xF0)]);
    }

    #[test]
    fn test_scientific_notation_always_float() {
        assert_eq!(lex("1e5"), vec![Token::Float(1e5)]);
        assert_eq!(lex("1E5"), vec![Token::Float(1e5)]);
        assert_eq!(lex("2e10"), vec![Token::Float(2e10)]);
        // Even whole-number results are Float
        assert_eq!(lex("1e2"), vec![Token::Float(100.0)]);
    }

    #[test]
    fn test_scientific_with_sign() {
        assert_eq!(lex("1e+5"), vec![Token::Float(1e5)]);
        assert_eq!(lex("1e-3"), vec![Token::Float(1e-3)]);
    }

    #[test]
    fn test_scientific_with_decimal() {
        assert_eq!(lex("1.5e3"), vec![Token::Float(1500.0)]);
        assert_eq!(lex("4.25e0"), vec![Token::Float(4.25)]);
        assert_eq!(lex("2.5e-1"), vec![Token::Float(0.25)]);
    }

    #[test]
    fn test_scientific_rejects_overflow() {
        // 1e999 is not finite — must be rejected
        let result = Lexer::new(crate::source::FileId::default(), "1e999").tokenize();
        assert!(result.is_err());
    }

    #[test]
    fn test_hex_empty_digits_error() {
        let result = Lexer::new(crate::source::FileId::default(), "0x").tokenize();
        assert!(result.is_err());
    }

    #[test]
    fn test_binary_empty_digits_error() {
        let result = Lexer::new(crate::source::FileId::default(), "0b").tokenize();
        assert!(result.is_err());
    }

    #[test]
    fn test_scientific_no_digit_after_e_error() {
        let result = Lexer::new(crate::source::FileId::default(), "1e").tokenize();
        assert!(result.is_err());
    }
}
