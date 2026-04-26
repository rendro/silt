use std::fmt;

use crate::intern::{self, Symbol};

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

    // Whitespace
    Newline,

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
            Token::Int(n) => write!(f, "{n}"),
            Token::Float(n) => write!(f, "{n}"),
            Token::Bool(b) => write!(f, "{b}"),
            Token::StringLit(s, _) => write!(f, "\"{s}\""),
            Token::StringStart(s) => write!(f, "\"{s}{{"),
            Token::StringMiddle(s) => write!(f, "}}{s}{{"),
            Token::StringEnd(s) => write!(f, "}}{s}\""),
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
            Token::Newline => write!(f, "\\n"),
            Token::Eof => write!(f, "EOF"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    pub line: usize,
    pub col: usize,
    pub offset: usize,
}

impl Span {
    pub fn new(line: usize, col: usize) -> Self {
        Self {
            line,
            col,
            offset: 0,
        }
    }

    pub fn with_offset(line: usize, col: usize, offset: usize) -> Self {
        Self { line, col, offset }
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.col)
    }
}

pub type SpannedToken = (Token, Span);

#[derive(Debug)]
pub struct LexError {
    pub message: String,
    pub span: Span,
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.span, self.message)
    }
}

pub struct Lexer {
    source: Vec<char>,
    pos: usize,
    line: usize,
    col: usize,
    byte_offset: usize,
    /// Stack of brace depths at which string interpolations began.
    /// When we encounter `}` and brace_depth matches the top of this stack,
    /// we resume scanning a string instead of emitting RBrace.
    interp_stack: Vec<usize>,
    brace_depth: usize,
}

impl Lexer {
    pub fn new(source: &str) -> Self {
        Self {
            source: source.chars().collect(),
            pos: 0,
            line: 1,
            col: 1,
            byte_offset: 0,
            interp_stack: Vec::new(),
            brace_depth: 0,
        }
    }

    pub fn tokenize(&mut self) -> Result<Vec<SpannedToken>, LexError> {
        let mut tokens = Vec::new();
        loop {
            let tok = self.next_token()?;
            let is_eof = tok.0 == Token::Eof;
            tokens.push(tok);
            if is_eof {
                break;
            }
        }
        Ok(tokens)
    }

    fn span(&self) -> Span {
        Span::with_offset(self.line, self.col, self.byte_offset)
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
        if ch == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(ch)
    }

    fn skip_whitespace(&mut self) -> bool {
        let mut found_newline = false;
        while let Some(ch) = self.peek() {
            match ch {
                ' ' | '\t' | '\r' => {
                    self.advance_char();
                }
                '\n' => {
                    found_newline = true;
                    self.advance_char();
                }
                _ => break,
            }
        }
        found_newline
    }

    fn skip_line_comment(&mut self) {
        while let Some(ch) = self.peek() {
            if ch == '\n' {
                break;
            }
            self.advance_char();
        }
    }

    fn skip_block_comment(&mut self) -> Result<(), LexError> {
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
                    return Err(LexError {
                        message: "unterminated block comment".to_string(),
                        span: start,
                    });
                }
            }
        }
        Ok(())
    }

    fn scan_string(
        &mut self,
        is_continuation: bool,
        start: Span,
    ) -> Result<SpannedToken, LexError> {
        let mut text = String::new();

        loop {
            match self.peek() {
                None => {
                    let message = if !self.interp_stack.is_empty() {
                        "unterminated string interpolation; use \\{ for a literal brace".to_string()
                    } else {
                        "unterminated string".to_string()
                    };
                    return Err(LexError {
                        message,
                        span: start,
                    });
                }
                Some('\\') => {
                    self.advance_char();
                    match self.advance_char() {
                        Some('n') => text.push('\n'),
                        Some('t') => text.push('\t'),
                        Some('\\') => text.push('\\'),
                        Some('"') => text.push('"'),
                        Some('{') => text.push('{'),
                        Some('}') => text.push('}'),
                        Some(c) => {
                            return Err(LexError {
                                message: format!("unknown escape sequence: \\{c}"),
                                span: self.span(),
                            });
                        }
                        None => {
                            return Err(LexError {
                                message: "unterminated escape sequence".to_string(),
                                span: start,
                            });
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

    fn scan_triple_string(&mut self, start: Span) -> Result<SpannedToken, LexError> {
        // We've already consumed the opening `"""`.
        // Read raw content until closing `"""`.
        // No escape processing, no interpolation.
        let mut raw = String::new();

        loop {
            match self.peek() {
                None => {
                    return Err(LexError {
                        message: "unterminated triple-quoted string".to_string(),
                        span: start,
                    });
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

    fn scan_number(&mut self, first: char, start: Span) -> Result<SpannedToken, LexError> {
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
                return Err(LexError {
                    message: "expected digit after exponent".into(),
                    span: start,
                });
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
            let val: f64 = num.parse().map_err(|_| LexError {
                message: "number literal too large".into(),
                span: start,
            })?;
            if !val.is_finite() {
                return Err(LexError {
                    message: "number literal out of range (not finite)".into(),
                    span: start,
                });
            }
            Ok((Token::Float(val), start))
        } else {
            let val: i64 = num.parse().map_err(|_| LexError {
                message: "number literal too large".into(),
                span: start,
            })?;
            Ok((Token::Int(val), start))
        }
    }

    fn scan_hex_int(&mut self, start: Span) -> Result<SpannedToken, LexError> {
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
            return Err(LexError {
                message: "expected hex digit after 0x".into(),
                span: start,
            });
        }
        let val = i64::from_str_radix(&digits, 16).map_err(|_| LexError {
            message: "hex literal too large".into(),
            span: start,
        })?;
        Ok((Token::Int(val), start))
    }

    fn scan_binary_int(&mut self, start: Span) -> Result<SpannedToken, LexError> {
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
            return Err(LexError {
                message: "expected binary digit after 0b".into(),
                span: start,
            });
        }
        let val = i64::from_str_radix(&digits, 2).map_err(|_| LexError {
            message: "binary literal too large".into(),
            span: start,
        })?;
        Ok((Token::Int(val), start))
    }

    fn scan_ident_or_keyword(&mut self, first: char, start: Span) -> SpannedToken {
        let mut name = String::new();
        name.push(first);

        while let Some(ch) = self.peek() {
            if ch.is_alphanumeric() || ch == '_' {
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

    fn next_token(&mut self) -> Result<SpannedToken, LexError> {
        // Skip whitespace, tracking newlines
        let mut had_newline = self.skip_whitespace();

        // Skip comments (may require multiple passes if comment is followed by whitespace/comment)
        loop {
            match (self.peek(), self.peek_ahead(1)) {
                (Some('-'), Some('-')) => {
                    self.skip_line_comment();
                    had_newline |= self.skip_whitespace();
                    continue;
                }
                (Some('{'), Some('-')) => {
                    let _start = self.span();
                    self.advance_char();
                    self.advance_char();
                    self.skip_block_comment()?;
                    had_newline |= self.skip_whitespace();
                    continue;
                }
                _ => break,
            }
        }

        // Emit a newline token if we crossed a line boundary
        // (but collapse multiple newlines into one)
        if had_newline && self.peek().is_some() {
            return Ok((Token::Newline, self.span()));
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
                    self.next_token()
                } else {
                    Ok((Token::Minus, start))
                }
            }

            '/' => Ok((Token::Slash, start)),

            '.' => {
                if self.peek() == Some('.') {
                    self.advance_char();
                    Ok((Token::DotDot, start))
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
                    Err(LexError {
                        message: "unexpected character '&', did you mean '&&'?".to_string(),
                        span: start,
                    })
                }
            }

            ';' => Err(LexError {
                message: "semicolons are not used in silt — use a newline to separate statements"
                    .to_string(),
                span: start,
            }),
            _ => Err(LexError {
                message: format!("unexpected character: '{ch}'"),
                span: start,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex(input: &str) -> Vec<Token> {
        Lexer::new(input)
            .tokenize()
            .unwrap()
            .into_iter()
            .map(|(tok, _)| tok)
            .filter(|tok| !matches!(tok, Token::Newline | Token::Eof))
            .collect()
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
        let result = Lexer::new("1e999").tokenize();
        assert!(result.is_err());
    }

    #[test]
    fn test_hex_empty_digits_error() {
        let result = Lexer::new("0x").tokenize();
        assert!(result.is_err());
    }

    #[test]
    fn test_binary_empty_digits_error() {
        let result = Lexer::new("0b").tokenize();
        assert!(result.is_err());
    }

    #[test]
    fn test_scientific_no_digit_after_e_error() {
        let result = Lexer::new("1e").tokenize();
        assert!(result.is_err());
    }
}
