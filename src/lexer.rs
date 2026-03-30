use std::fmt;

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
    Select,
    Pub,
    Mod,
    Import,
    As,
    Else,
    Where,

    // Literals
    Int(i64),
    Float(f64),
    Bool(bool),
    /// A complete string with no interpolation
    StringLit(String),
    /// Start of an interpolated string (text before first `{`)
    StringStart(String),
    /// Middle segment between `}` and next `{`
    StringMiddle(String),
    /// End segment after last `}` to closing `"`
    StringEnd(String),

    // Identifiers
    Ident(String),

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
    DotDot,   // ..
    Arrow,    // ->

    // Delimiters
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    HashBrace, // #{

    // Punctuation
    Comma,
    Colon,
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
            // spawn is no longer a keyword; it's the task.spawn module function
            Token::Select => write!(f, "select"),
            Token::Pub => write!(f, "pub"),
            Token::Mod => write!(f, "mod"),
            Token::Import => write!(f, "import"),
            Token::As => write!(f, "as"),
            Token::Else => write!(f, "else"),
            Token::Where => write!(f, "where"),
            Token::Int(n) => write!(f, "{n}"),
            Token::Float(n) => write!(f, "{n}"),
            Token::Bool(b) => write!(f, "{b}"),
            Token::StringLit(s) => write!(f, "\"{s}\""),
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
            Token::DotDot => write!(f, ".."),
            Token::Arrow => write!(f, "->"),
            Token::LParen => write!(f, "("),
            Token::RParen => write!(f, ")"),
            Token::LBrace => write!(f, "{{"),
            Token::RBrace => write!(f, "}}"),
            Token::LBracket => write!(f, "["),
            Token::RBracket => write!(f, "]"),
            Token::HashBrace => write!(f, "#{{"),
            Token::Comma => write!(f, ","),
            Token::Colon => write!(f, ":"),
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
        Self { line, col, offset: 0 }
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

    fn scan_string(&mut self, is_continuation: bool) -> Result<SpannedToken, LexError> {
        // For a new string, we've already consumed the opening `"`.
        // For a continuation (after `}`), we start scanning immediately.
        let start = self.span();
        let mut text = String::new();

        loop {
            match self.peek() {
                None => {
                    return Err(LexError {
                        message: "unterminated string".to_string(),
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
                        Token::StringLit(text)
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

    fn scan_number(&mut self, first: char) -> SpannedToken {
        let start = self.span();
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

        // Check for float: `.` followed by a digit (not `..` for range)
        if self.peek() == Some('.') && self.peek_ahead(1).is_some_and(|c| c.is_ascii_digit()) {
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
            let val: f64 = num.parse().unwrap();
            (Token::Float(val), start)
        } else {
            let val: i64 = num.parse().unwrap();
            (Token::Int(val), start)
        }
    }

    fn scan_ident_or_keyword(&mut self, first: char) -> SpannedToken {
        let start = self.span();
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
            // "spawn" is no longer a keyword; it's now task.spawn
            "select" => Token::Select,
            "pub" => Token::Pub,
            "mod" => Token::Mod,
            "import" => Token::Import,
            "as" => Token::As,
            "else" => Token::Else,
            "where" => Token::Where,
            "true" => Token::Bool(true),
            "false" => Token::Bool(false),
            _ => Token::Ident(name),
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
            // String
            '"' => self.scan_string(false),

            // Numbers
            '0'..='9' => Ok(self.scan_number(ch)),

            // Identifiers and keywords
            'a'..='z' | 'A'..='Z' | '_' => Ok(self.scan_ident_or_keyword(ch)),

            // Operators and punctuation
            '+' => Ok((Token::Plus, start)),
            '*' => Ok((Token::Star, start)),
            '%' => Ok((Token::Percent, start)),
            '?' => Ok((Token::Question, start)),
            ',' => Ok((Token::Comma, start)),
            ':' => Ok((Token::Colon, start)),
            '(' => Ok((Token::LParen, start)),
            ')' => Ok((Token::RParen, start)),
            '[' => Ok((Token::LBracket, start)),
            ']' => Ok((Token::RBracket, start)),

            '#' if self.peek() == Some('{') => {
                self.advance_char();
                self.brace_depth += 1;
                Ok((Token::HashBrace, start))
            }

            '{' => {
                self.brace_depth += 1;
                Ok((Token::LBrace, start))
            }

            '}' => {
                // Check if this closes a string interpolation
                if let Some(&interp_depth) = self.interp_stack.last() {
                    if self.brace_depth == interp_depth + 1 {
                        self.interp_stack.pop();
                        self.brace_depth -= 1;
                        // Resume scanning the string
                        return self.scan_string(true);
                    }
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
        assert_eq!(lex("let x = 42"), vec![
            Token::Let, Token::Ident("x".into()), Token::Eq, Token::Int(42),
        ]);
    }

    #[test]
    fn test_operators() {
        assert_eq!(lex("|> -> .. == != <= >="), vec![
            Token::Pipe, Token::Arrow, Token::DotDot,
            Token::EqEq, Token::NotEq, Token::LtEq, Token::GtEq,
        ]);
    }

    #[test]
    fn test_string_simple() {
        assert_eq!(lex(r#""hello""#), vec![Token::StringLit("hello".into())]);
    }

    #[test]
    fn test_string_interpolation() {
        let tokens = lex(r#""hello {name}""#);
        assert_eq!(tokens, vec![
            Token::StringStart("hello ".into()),
            Token::Ident("name".into()),
            Token::StringEnd(String::new()),
        ]);
    }

    #[test]
    fn test_string_multi_interp() {
        let tokens = lex(r#""a {x} b {y} c""#);
        assert_eq!(tokens, vec![
            Token::StringStart("a ".into()),
            Token::Ident("x".into()),
            Token::StringMiddle(" b ".into()),
            Token::Ident("y".into()),
            Token::StringEnd(" c".into()),
        ]);
    }

    #[test]
    fn test_number_and_range() {
        assert_eq!(lex("1..101"), vec![Token::Int(1), Token::DotDot, Token::Int(101)]);
    }

    #[test]
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
        assert_eq!(lex("fn let match when return"), vec![
            Token::Fn, Token::Let, Token::Match, Token::When, Token::Return,
        ]);
    }

    #[test]
    fn test_hash_brace() {
        assert_eq!(lex("#{ }"), vec![Token::HashBrace, Token::RBrace]);
    }

    #[test]
    fn test_escaped_brace_in_string() {
        assert_eq!(lex(r#""\{not interp\}""#), vec![
            Token::StringLit("{not interp}".into()),
        ]);
    }

    #[test]
    fn test_where_keyword() {
        assert_eq!(lex("where"), vec![Token::Where]);
    }
}
