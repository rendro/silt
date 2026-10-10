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

    /// Text that is no token: the lexer said what is wrong with it
    /// (`Lexed::errors`) and went on behind it.
    Error,

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
            Token::Error => write!(f, "invalid token"),
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

/// What the lexer makes of a file: its tokens, its comments in source
/// order, and what is wrong with the text. Each token names the comments
/// in front of it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Lexed {
    pub tokens: Vec<Tok>,
    pub comments: Vec<Comment>,
    /// The lex errors, in source order: the first `MAX_SYNTAX_ERRORS`
    /// of them. Text that is no token is a `Token::Error` among the
    /// tokens; a string with a wrong escape is still its string token.
    /// A mistake that is made again (a name with a letter outside ASCII
    /// that is used again, another semicolon) is one error, at its
    /// first place, with a note that counts the others: their tokens
    /// are `Token::Error`s without an error of their own. A text with
    /// an error is never run or formatted.
    pub errors: Vec<Diagnostic>,
    /// The errors behind those of `errors`, which are counted and not
    /// kept. With the first of them the tokens end: the rest of the
    /// text is one `Token::Error`, so what is kept of a text that is
    /// no program at all does not grow with it.
    pub more_errors: usize,
    /// Whether the text ends inside a string or a block comment that is
    /// not closed (see `is_cut_short`).
    pub cut_short: bool,
}

impl Lexed {
    /// The tokens of a text without a lex error, or its first error.
    pub fn checked(mut self) -> Result<Lexed, Diagnostic> {
        match self.errors.is_empty() {
            true => Ok(self),
            false => Err(self.errors.swap_remove(0)),
        }
    }

    /// Whether the text ends inside a string or a block comment that is
    /// not closed. Where it was meant to end is unknown, so what the
    /// tokens before it are is unknown too: only the lexer's errors are
    /// worth reporting for such a text.
    pub fn is_cut_short(&self) -> bool {
        self.cut_short
    }

    /// Whether the text is not finished: it ends inside a string or a
    /// block comment (`is_cut_short`), or a delimiter it opens (`(`,
    /// `[`, `{`, `#{`, `#[`, the `{` of a string interpolation) is not
    /// closed at its end. A closer with nothing open closes nothing.
    /// The REPL reads another line of such an input.
    pub fn ends_open(&self) -> bool {
        let open = self.tokens.iter().fold(0usize, |open, tok| match tok.kind {
            Token::LParen
            | Token::LBracket
            | Token::LBrace
            | Token::HashBrace
            | Token::HashBracket
            | Token::StringStart(_) => open + 1,
            Token::RParen | Token::RBracket | Token::RBrace | Token::StringEnd(_) => {
                open.saturating_sub(1)
            }
            _ => open,
        });
        open > 0 || self.is_cut_short()
    }

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

/// How many lex and parse errors of one file are kept and reported; the
/// rest are counted.
pub const MAX_SYNTAX_ERRORS: usize = 50;

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
    /// The first `MAX_SYNTAX_ERRORS` errors, in source order.
    errors: Vec<Diagnostic>,
    /// The errors behind them: counted, not kept.
    more_errors: usize,
    /// Whether the text ends inside an unclosed string or comment.
    cut_short: bool,
    /// The mistakes that are one error when they are made again: what
    /// was written (for the note), the index in `errors` of its first
    /// error, and the places it is written again.
    repeats: std::collections::HashMap<String, (usize, usize)>,
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
            errors: Vec::new(),
            more_errors: 0,
            cut_short: false,
            repeats: std::collections::HashMap::new(),
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

    /// The tokens of the whole text. An error does not end it: the text
    /// that is no token becomes a `Token::Error`, the error is recorded
    /// (`Lexed::errors`), and the lexer goes on behind it. With the
    /// first error that is not kept (`Lexed::more_errors`) the tokens
    /// end: the lexer goes on only to count, and the rest of the text is
    /// one `Token::Error`.
    pub fn tokenize(&mut self) -> Lexed {
        let mut tokens = Vec::new();
        // Where the token starts with which the tokens end.
        let mut cut: Option<Span> = None;
        loop {
            let (kind, start) = self.next_token();
            let is_eof = kind == Token::Eof;
            if cut.is_none() && self.more_errors > 0 && !is_eof {
                cut = Some(start);
            }
            match cut {
                // Counting: nothing is kept but the comments in front of
                // the cut, which the token of the rest carries.
                Some(_) if !is_eof => {
                    self.comments.truncate(self.gap_comments as usize);
                    continue;
                }
                Some(cut) => {
                    self.comments.truncate(self.gap_comments as usize);
                    self.push_token(&mut tokens, Token::Error, cut);
                    self.gap_newlines = 0;
                }
                None => {}
            }
            self.push_token(&mut tokens, kind, start);
            if is_eof {
                break;
            }
        }
        for (written, (index, more)) in std::mem::take(&mut self.repeats) {
            if more > 0 {
                let places = if more == 1 { "place" } else { "places" };
                self.errors[index]
                    .notes
                    .push(format!("{written} is written in {more} more {places}"));
            }
        }
        Lexed {
            tokens,
            comments: std::mem::take(&mut self.comments),
            errors: std::mem::take(&mut self.errors),
            more_errors: self.more_errors,
            cut_short: self.cut_short,
        }
    }

    /// Add the token `kind`, which starts at `start` and ends where the
    /// lexer stands (every scan stops right after its token), with the
    /// trivia in front of it.
    fn push_token(&mut self, tokens: &mut Vec<Tok>, kind: Token, start: Span) {
        let end = self.comments.len() as u32;
        let comments = self.gap_comments..end;
        self.gap_comments = end;
        tokens.push(Tok {
            kind,
            span: self.since(start),
            newlines_before: std::mem::take(&mut self.gap_newlines),
            comments,
        });
    }

    /// Record the error that `error` makes, if it is among the first
    /// `MAX_SYNTAX_ERRORS`; count it otherwise.
    fn error(&mut self, error: impl FnOnce() -> Diagnostic) {
        if self.errors.len() < MAX_SYNTAX_ERRORS {
            self.errors.push(error());
        } else {
            self.more_errors += 1;
        }
    }

    /// Record the error of a mistake that is one error however often it
    /// is made: `written` names what was written. The first time it is
    /// the error that `error` makes; again, it is counted for that
    /// error's note. (A mistake whose first error was not kept counts
    /// as an error each time.)
    fn repeatable_error(&mut self, written: String, error: impl FnOnce() -> Diagnostic) {
        if let Some((_, more)) = self.repeats.get_mut(&written) {
            *more += 1;
        } else if self.errors.len() < MAX_SYNTAX_ERRORS {
            self.repeats.insert(written, (self.errors.len(), 0));
            self.errors.push(error());
        } else {
            self.more_errors += 1;
        }
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

    /// Skip to the end of the block comment whose `{-` was just read. One
    /// that is not closed is an error and runs to the end of the text.
    fn skip_block_comment(&mut self) {
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
                    self.cut_short = true;
                    self.error(|| {
                        Diagnostic::error(
                            Code::UnterminatedComment,
                            start,
                            "unterminated block comment",
                        )
                    });
                    return;
                }
            }
        }
    }

    fn scan_string(&mut self, is_continuation: bool, start: Span) -> Scanned {
        let mut text = String::new();

        loop {
            match self.peek() {
                None => {
                    let message = if !self.interp_stack.is_empty() {
                        "unterminated string interpolation; use \\{ for a literal brace".to_string()
                    } else {
                        "unterminated string".to_string()
                    };
                    return self.unterminated(start, message);
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
                        // An unknown escape is an error of the string,
                        // which goes on behind it.
                        Some(c) if c.is_control() => {
                            let at = self.span();
                            self.error(|| {
                                Diagnostic::error(
                                    Code::InvalidEscape,
                                    at,
                                    format!("unknown escape sequence: \\{}", c.escape_default()),
                                )
                            });
                        }
                        Some(c) => self.error(|| {
                            Diagnostic::error(
                                Code::InvalidEscape,
                                esc_span,
                                format!("unknown escape sequence: \\{c}"),
                            )
                        }),
                        None => return self.unterminated(start, "unterminated escape sequence"),
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
                    return (tok, start);
                }
                Some('"') => {
                    self.advance_char(); // consume closing `"`
                    let tok = if is_continuation {
                        Token::StringEnd(text)
                    } else {
                        Token::StringLit(text, false)
                    };
                    return (tok, start);
                }
                Some(ch) => {
                    self.advance_char();
                    text.push(ch);
                }
            }
        }
    }

    fn scan_triple_string(&mut self, start: Span) -> Scanned {
        // We've already consumed the opening `"""`.
        // Read raw content until closing `"""`.
        // No escape processing, no interpolation.
        let mut raw = String::new();

        loop {
            match self.peek() {
                None => return self.unterminated(start, "unterminated triple-quoted string"),
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
        (Token::StringLit(result, true), start)
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

    /// The error `message` for the number read from `start` to here.
    fn invalid_number(&mut self, start: Span, message: &str) -> Scanned {
        let span = self.since(start);
        self.error(|| Diagnostic::error(Code::InvalidNumber, span, message));
        (Token::Error, start)
    }

    /// The error `message` for the string that starts at `start` and is
    /// not closed where the text ends.
    fn unterminated(&mut self, start: Span, message: impl Into<String>) -> Scanned {
        self.cut_short = true;
        self.error(|| Diagnostic::error(Code::UnterminatedString, start, message));
        (Token::Error, start)
    }

    fn scan_number(&mut self, first: char, start: Span) -> Scanned {
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
                return self.invalid_number(start, "expected digit after exponent");
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
            let Ok(val) = num.parse::<f64>() else {
                return self.invalid_number(start, "number literal too large");
            };
            if !val.is_finite() {
                return self.invalid_number(start, "number literal out of range (not finite)");
            }
            (Token::Float(val), start)
        } else {
            match int_magnitude(&num, 10) {
                Some(val) => (Token::Int(val), start),
                None => self.invalid_number(start, "number literal too large"),
            }
        }
    }

    fn scan_hex_int(&mut self, start: Span) -> Scanned {
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
            return self.invalid_number(start, "expected hex digit after 0x");
        }
        match int_magnitude(&digits, 16) {
            Some(val) => (Token::Int(val), start),
            None => self.invalid_number(start, "hex literal too large"),
        }
    }

    fn scan_binary_int(&mut self, start: Span) -> Scanned {
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
            return self.invalid_number(start, "expected binary digit after 0b");
        }
        match int_magnitude(&digits, 2) {
            Some(val) => (Token::Int(val), start),
            None => self.invalid_number(start, "binary literal too large"),
        }
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

    fn next_token(&mut self) -> Scanned {
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
                    self.skip_block_comment();
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
            return (Token::Eof, start);
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
            'a'..='z' | 'A'..='Z' | '_' => self.scan_ident_or_keyword(ch, start),

            // Operators and punctuation
            '+' => (Token::Plus, start),
            '*' => (Token::Star, start),
            '%' => (Token::Percent, start),
            '?' => (Token::Question, start),
            '^' => (Token::Caret, start),
            ',' => (Token::Comma, start),
            ':' => {
                // Associated-type projection: `Self::Item` and
                // `<a as Trait>::Item` use `::` as a 2-char token. A
                // single `:` continues to mean record/field/where-clause
                // separator. Lookahead disambiguates without disturbing
                // any existing single-`:` site.
                if self.peek() == Some(':') {
                    self.advance_char();
                    (Token::ColonColon, start)
                } else {
                    (Token::Colon, start)
                }
            }
            '(' => (Token::LParen, start),
            ')' => (Token::RParen, start),
            '[' => (Token::LBracket, start),
            ']' => (Token::RBracket, start),

            '#' if self.peek() == Some('{') => {
                self.advance_char();
                self.brace_depth += 1;
                (Token::HashBrace, start)
            }

            '#' if self.peek() == Some('[') => {
                self.advance_char();
                (Token::HashBracket, start)
            }

            '{' => {
                self.brace_depth += 1;
                (Token::LBrace, start)
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
                (Token::RBrace, start)
            }

            '-' => {
                if self.peek() == Some('>') {
                    self.advance_char();
                    (Token::Arrow, start)
                } else if self.peek() == Some('-') {
                    // Line comment — shouldn't happen here since we skip comments above,
                    // but handle it just in case
                    self.skip_line_comment();
                    self.record_comment(CommentKind::Line, start);
                    self.next_token()
                } else {
                    (Token::Minus, start)
                }
            }

            '/' => (Token::Slash, start),

            '.' => {
                if self.peek() == Some('.') {
                    self.advance_char();
                    if self.peek() == Some('.') {
                        self.advance_char();
                        (Token::DotDotDot, start)
                    } else {
                        (Token::DotDot, start)
                    }
                } else {
                    (Token::Dot, start)
                }
            }

            '=' => {
                if self.peek() == Some('=') {
                    self.advance_char();
                    (Token::EqEq, start)
                } else {
                    (Token::Eq, start)
                }
            }

            '!' => {
                if self.peek() == Some('=') {
                    self.advance_char();
                    (Token::NotEq, start)
                } else {
                    (Token::Not, start)
                }
            }

            '<' => {
                if self.peek() == Some('=') {
                    self.advance_char();
                    (Token::LtEq, start)
                } else {
                    (Token::Lt, start)
                }
            }

            '>' => {
                if self.peek() == Some('=') {
                    self.advance_char();
                    (Token::GtEq, start)
                } else {
                    (Token::Gt, start)
                }
            }

            '|' => {
                if self.peek() == Some('>') {
                    self.advance_char();
                    (Token::Pipe, start)
                } else if self.peek() == Some('|') {
                    self.advance_char();
                    (Token::OrOr, start)
                } else {
                    (Token::Bar, start)
                }
            }

            '&' => {
                if self.peek() == Some('&') {
                    self.advance_char();
                    (Token::AndAnd, start)
                } else {
                    self.unexpected(start, || {
                        "unexpected character '&', did you mean '&&'?".to_string()
                    })
                }
            }

            // A semicolon, and those directly behind it. One that is
            // written again is the same mistake.
            ';' => {
                let span = self.since(start);
                self.repeatable_error("a semicolon".to_string(), || {
                    Diagnostic::error(
                        Code::UnexpectedChar,
                        span,
                        "semicolons are not used in silt — use a newline to separate statements",
                    )
                });
                while self.peek() == Some(';') {
                    self.advance_char();
                }
                (Token::Error, start)
            }
            // A letter or digit outside ASCII, at the start of a name or
            // inside one (`café` ends at the `f`). The letters and digits
            // that follow it are of the same word: one error, and the
            // same word written again is the same mistake.
            _ if ch.is_alphanumeric() => {
                let span = self.since(start);
                let word = |c: char| c.is_alphanumeric() || c == '_';
                // The ASCII part of the word, which is the name token in
                // front: read back once, for this one error of the word.
                let mut begin = self.pos - 1;
                while begin > 0 && word(self.source[begin - 1]) {
                    begin -= 1;
                }
                while self.peek().is_some_and(word) {
                    self.advance_char();
                }
                let written: String = self.source[begin..self.pos].iter().collect();
                self.repeatable_error(format!("'{written}'"), || {
                    Diagnostic::error(
                        Code::UnexpectedChar,
                        span,
                        format!(
                            "unexpected character: '{ch}'; a name is made of ASCII letters, digits and '_'"
                        ),
                    )
                });
                (Token::Error, start)
            }
            // A quotation mark that is not silt's: the text up to its
            // partner on the line is the string that was meant (`'a'`,
            // `` `x` ``, one pasted from a word processor), and one
            // error. The partner is looked for from here, once: a mark
            // without one reads to the end of its line, and then no
            // mark of its kind stands behind it on that line to read it
            // again.
            '\'' | '`' | '“' | '”' | '‘' | '’' => {
                let error = self.unexpected(start, || format!("unexpected character: '{ch}'"));
                let is_partner = |c: char| match ch {
                    '\'' | '`' => c == ch,
                    _ => matches!(c, '“' | '”' | '‘' | '’'),
                };
                let mut ahead = 0;
                let partner = loop {
                    match self.peek_ahead(ahead) {
                        None | Some('\n') => break None,
                        Some(c) if is_partner(c) => break Some(ahead),
                        Some(_) => ahead += 1,
                    }
                };
                if let Some(partner) = partner {
                    for _ in 0..=partner {
                        self.advance_char();
                    }
                }
                error
            }
            // Any other character is no part of silt: it and the
            // characters like it that stand directly behind it (`@@@`,
            // `@$~`, stray control bytes) are one error, named by the
            // first.
            _ => {
                let error = self.unexpected(start, || match ch {
                    // A BOM after the start of the file (the leading one
                    // is skipped in `Lexer::new`) is invisible and
                    // zero-width, so quoting the raw char would render
                    // an empty-looking error. Name it instead.
                    '\u{FEFF}' => "unexpected character: byte-order mark (U+FEFF); \
                                   a BOM is only permitted at the very start of the file"
                        .to_string(),
                    // Control characters (U+0001–U+001F, U+007F, …) are
                    // invisible, so quoting the raw byte renders an
                    // empty-looking error in any non-`cat -v` sink (log
                    // files, LSP JSON diagnostics, captured test
                    // output): `escape_default` yields `\u{1}`, `\t`,
                    // etc.
                    _ if ch.is_control() => {
                        format!("unexpected character: '{}'", ch.escape_default())
                    }
                    // A space that is not ASCII's, a zero-width
                    // character: quoted, it would show as nothing or
                    // as a space. Its code point names it.
                    _ if is_invisible(ch) => {
                        format!("unexpected character: U+{:04X} (invisible)", ch as u32)
                    }
                    _ => format!("unexpected character: '{ch}'"),
                });
                while self.peek().is_some_and(is_stray) {
                    self.advance_char();
                }
                error
            }
        }
    }

    /// The error that `message` makes for the character read from
    /// `start` to here, which starts text that is no token.
    fn unexpected(&mut self, start: Span, message: impl FnOnce() -> String) -> Scanned {
        let span = self.since(start);
        self.error(|| Diagnostic::error(Code::UnexpectedChar, span, message()));
        (Token::Error, start)
    }
}

/// Whether `c` shows as nothing or as white space: the spaces outside
/// ASCII, the zero-width and directional marks, the variation
/// selectors.
fn is_invisible(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{200B}'..='\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2060}'..='\u{206F}'
                | '\u{FE00}'..='\u{FE0F}'
        )
}

/// Whether `c` is no part of silt in any place outside a string or a
/// comment: not white space, not a letter or digit (those have errors of
/// their own), and none of the characters a token starts with or the
/// quotation marks that are read in pairs.
fn is_stray(c: char) -> bool {
    match c {
        ' ' | '\t' | '\r' | '\n' => false,
        '@' | '$' | '~' | '\\' => true,
        _ if c.is_ascii() => c.is_ascii_control(),
        '“' | '”' | '‘' | '’' => false,
        _ => !c.is_alphanumeric(),
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
            .checked()
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
            .checked()
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
                    .checked()
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
        let result = Lexer::new(crate::source::FileId::default(), "1e999")
            .tokenize()
            .checked();
        assert!(result.is_err());
    }

    #[test]
    fn test_hex_empty_digits_error() {
        let result = Lexer::new(crate::source::FileId::default(), "0x")
            .tokenize()
            .checked();
        assert!(result.is_err());
    }

    #[test]
    fn test_binary_empty_digits_error() {
        let result = Lexer::new(crate::source::FileId::default(), "0b")
            .tokenize()
            .checked();
        assert!(result.is_err());
    }

    #[test]
    fn test_scientific_no_digit_after_e_error() {
        let result = Lexer::new(crate::source::FileId::default(), "1e")
            .tokenize()
            .checked();
        assert!(result.is_err());
    }

    /// The tokens of `input` (without the Eof) and its errors' messages.
    fn lex_all(input: &str) -> (Vec<Token>, Vec<String>) {
        let lexed = Lexer::new(crate::source::FileId::default(), input).tokenize();
        (
            lexed
                .tokens
                .into_iter()
                .map(|tok| tok.kind)
                .filter(|tok| !matches!(tok, Token::Eof))
                .collect(),
            lexed.errors.into_iter().map(|e| e.message).collect(),
        )
    }

    #[test]
    fn test_the_lexer_goes_on_behind_an_error() {
        let name = |s: &str| Token::Ident(intern::intern(s));
        // Text that is no token is an Error token, and what follows it
        // is read.
        let (tokens, errors) = lex_all("1 @ 2 ; 3");
        assert_eq!(
            tokens,
            vec![
                Token::Int(1),
                Token::Error,
                Token::Int(2),
                Token::Error,
                Token::Int(3)
            ]
        );
        assert_eq!(errors.len(), 2, "{errors:?}");
        // Numbers that are none.
        let (tokens, errors) = lex_all("0x 1e 5 99999999999999999999");
        assert_eq!(
            tokens,
            vec![Token::Error, Token::Error, Token::Int(5), Token::Error]
        );
        assert_eq!(errors.len(), 3, "{errors:?}");
        // One error for a character and its repetitions, for the rest of
        // a word behind a letter outside ASCII, and for a pair of
        // typographic quotation marks with what they hold.
        let (tokens, errors) = lex_all("@@@ x");
        assert_eq!(tokens, vec![Token::Error, name("x")]);
        assert_eq!(errors, vec!["unexpected character: '@'"]);
        let (tokens, errors) = lex_all("größe = 1");
        assert_eq!(
            tokens,
            vec![name("gr"), Token::Error, Token::Eq, Token::Int(1)]
        );
        assert_eq!(errors.len(), 1, "{errors:?}");
        let (tokens, errors) = lex_all("f(“a b”, 1)");
        assert_eq!(
            tokens,
            vec![
                name("f"),
                Token::LParen,
                Token::Error,
                Token::Comma,
                Token::Int(1),
                Token::RParen
            ]
        );
        assert_eq!(errors, vec!["unexpected character: '“'"]);
        let (tokens, errors) = lex_all("c = 'a' + `b c` + \u{1}\u{2}1");
        assert_eq!(
            tokens,
            vec![
                name("c"),
                Token::Eq,
                Token::Error,
                Token::Plus,
                Token::Error,
                Token::Plus,
                Token::Error,
                Token::Int(1)
            ]
        );
        assert_eq!(errors.len(), 3, "{errors:?}");
    }

    #[test]
    fn test_a_mistake_made_again_is_one_error_that_counts_the_places() {
        let lexed = Lexer::new(
            crate::source::FileId::default(),
            "let café = 1; let naïve = café + café; naïve",
        )
        .tokenize();
        let errors: Vec<(u32, &str, &[String])> = lexed
            .errors
            .iter()
            .map(|e| (e.span.start, e.message.as_str(), e.notes.as_slice()))
            .collect();
        let name = "; a name is made of ASCII letters, digits and '_'";
        assert_eq!(
            errors,
            vec![
                (
                    7,
                    format!("unexpected character: 'é'{name}").as_str(),
                    &["'café' is written in 2 more places".to_string()][..]
                ),
                (
                    13,
                    "semicolons are not used in silt — use a newline to separate statements",
                    &["a semicolon is written in 1 more place".to_string()][..]
                ),
                (
                    21,
                    format!("unexpected character: 'ï'{name}").as_str(),
                    &["'naïve' is written in 1 more place".to_string()][..]
                ),
            ]
        );
        // Each place is an Error token all the same.
        let invalid = lexed
            .tokens
            .iter()
            .filter(|tok| tok.kind == Token::Error)
            .count();
        assert_eq!(invalid, 7);
        // A name written once has no note.
        let lexed = Lexer::new(crate::source::FileId::default(), "größe").tokenize();
        assert!(lexed.errors[0].notes.is_empty());
    }

    #[test]
    fn test_errors_behind_the_fiftieth_are_counted_and_the_tokens_end() {
        let text: String = (0..60).map(|i| format!("x{i} @ ")).collect();
        let lexed = Lexer::new(crate::source::FileId::default(), &text).tokenize();
        assert_eq!(lexed.errors.len(), MAX_SYNTAX_ERRORS);
        assert_eq!(lexed.more_errors, 10);
        // Fifty names with their errors, the fifty-first name, and one
        // token for the rest of the text, from the first error that is
        // not kept.
        assert_eq!(lexed.tokens.len(), 2 * MAX_SYNTAX_ERRORS + 1 + 1 + 1);
        let rest = &lexed.tokens[lexed.tokens.len() - 2];
        assert_eq!(rest.kind, Token::Error);
        assert_eq!(
            &text[rest.span.start as usize..rest.span.end as usize],
            &text[text.find("x50 @").unwrap() + 4..]
        );
        // Text without an error is not cut, however long.
        let lexed = Lexer::new(crate::source::FileId::default(), &"x ".repeat(1000)).tokenize();
        assert_eq!((lexed.tokens.len(), lexed.more_errors), (1001, 0));
    }

    #[test]
    fn test_a_run_of_characters_that_are_no_part_of_silt_is_one_error() {
        let (tokens, errors) = lex_all("1 @$~\\\u{1}\u{feff}€ 2 ;;; 3 ; 4");
        assert_eq!(
            tokens,
            vec![
                Token::Int(1),
                Token::Error,
                Token::Int(2),
                Token::Error,
                Token::Int(3),
                Token::Error,
                Token::Int(4)
            ]
        );
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert_eq!(errors[0], "unexpected character: '@'");
    }

    #[test]
    fn test_a_wrong_escape_is_an_error_of_a_string_that_goes_on() {
        let (tokens, errors) = lex_all(r#"x = "a\qb\zc" + 1"#);
        assert_eq!(
            tokens,
            vec![
                Token::Ident(intern::intern("x")),
                Token::Eq,
                Token::StringLit("abc".into(), false),
                Token::Plus,
                Token::Int(1)
            ]
        );
        assert_eq!(
            errors,
            vec![
                "unknown escape sequence: \\q",
                "unknown escape sequence: \\z"
            ]
        );
    }

    #[test]
    fn test_a_text_that_ends_in_a_string_or_comment_is_cut_short() {
        let lex = |input: &str| Lexer::new(crate::source::FileId::default(), input).tokenize();
        for (input, tokens, message) in [
            (
                "1 \"abc",
                vec![Token::Int(1), Token::Error],
                "unterminated string",
            ),
            (
                "1 \"\"\"abc\"",
                vec![Token::Int(1), Token::Error],
                "unterminated triple-quoted string",
            ),
            (
                "1 {- abc",
                vec![Token::Int(1)],
                "unterminated block comment",
            ),
        ] {
            let lexed = lex(input);
            assert!(lexed.is_cut_short(), "{input}");
            assert_eq!(lexed.errors.len(), 1, "{input}");
            assert_eq!(lexed.errors[0].message, message, "{input}");
            let kinds: Vec<Token> = lexed
                .tokens
                .into_iter()
                .map(|tok| tok.kind)
                .filter(|tok| !matches!(tok, Token::Eof))
                .collect();
            assert_eq!(kinds, tokens, "{input}");
        }
        assert!(!lex("1 @ 2 \"a\\q\"").is_cut_short());
    }
}
