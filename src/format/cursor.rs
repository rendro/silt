//! The token cursor: the printer's view of the source.
//!
//! The printer walks the syntax tree and the token list together. Every
//! piece of output that is a source token is written through
//! [`Cursor::token`], which checks that the source has that token next,
//! writes the comments in front of it, the token as it is spelled in the
//! source, and the comments behind it on its line, and moves on. So a
//! comment is written in one place, and no position of the grammar can
//! forget its comments: a comment belongs to a token, and every token is
//! visited.
//!
//! How a comment is written depends only on what stands around it in the
//! source:
//!
//! - a `--` comment, a `{- -}` comment that spans lines, and a one-line
//!   `{- -}` comment that ends its line are *end-of-line* comments.
//!   Behind a token of their line they become a [`Doc::LineSuffix`];
//!   first on their line, a [`Doc::OwnLine`];
//! - a one-line `{- -}` comment with a token behind it on its line is
//!   *inline*, a [`Doc::Comment`]. It is written with the token after
//!   it, or with the token before it when a comma or a closing bracket
//!   follows.
//!
//! A token the printer does not write (redundant parentheses, a comma it
//! decides for itself) is skipped; its comments go to the next token
//! that is written, as end-of-line comments.

use crate::lexer::{Comment, CommentKind, Lexed, Tok, Token};
use crate::source::Span;

use super::doc::Doc;

/// The printer asked for a token the source does not have next.
#[derive(Debug, Clone)]
pub struct Mismatch {
    pub message: String,
    pub span: Span,
}

pub struct Cursor<'a> {
    source: &'a str,
    /// The tokens without `Token::Newline`: a token's `newlines_before`
    /// says the same.
    tokens: Vec<&'a Tok>,
    comments: &'a [Comment],
    pos: usize,
    /// The comments before this index are written.
    next_comment: u32,
    /// Comments of skipped tokens, for the next token that is written.
    carried: Vec<Doc>,
    /// The first thing the printer asked for that the source does not
    /// hold. Once set, the printer's result is not used.
    pub error: Option<Mismatch>,
}

enum Class {
    Inline,
    /// With whether nothing can follow the comment on its line.
    EndOfLine(bool),
}

fn is_closer_or_comma(kind: &Token) -> bool {
    matches!(
        kind,
        Token::RParen
            | Token::RBracket
            | Token::RBrace
            | Token::Comma
            | Token::StringMiddle(_)
            | Token::StringEnd(_)
    )
}

impl<'a> Cursor<'a> {
    pub fn new(source: &'a str, lexed: &'a Lexed) -> Self {
        Cursor {
            source,
            tokens: lexed
                .tokens
                .iter()
                .filter(|tok| tok.kind != Token::Newline)
                .collect(),
            comments: &lexed.comments,
            pos: 0,
            next_comment: 0,
            carried: Vec::new(),
            error: None,
        }
    }

    fn tok(&self) -> &'a Tok {
        // The token list ends with the end-of-file token, and the
        // cursor never moves past it.
        self.tokens[self.pos.min(self.tokens.len() - 1)]
    }

    /// The next source token.
    pub fn peek(&self) -> &'a Token {
        &self.tok().kind
    }

    /// The source token `n` places after the next one.
    pub fn peek_at(&self, n: usize) -> &'a Token {
        &self.tokens[(self.pos + n).min(self.tokens.len() - 1)].kind
    }

    /// Whether the next source token is of the kind of `kind` (its text
    /// or value is not compared).
    pub fn at(&self, kind: &Token) -> bool {
        std::mem::discriminant(self.peek()) == std::mem::discriminant(kind)
    }

    /// Whether the next source token is the identifier `name`.
    pub fn at_word(&self, name: &str) -> bool {
        matches!(self.peek(), Token::Ident(_)) && self.text_of(self.tok()) == name
    }

    /// Where the next source token starts.
    pub fn offset(&self) -> u32 {
        self.tok().span.start
    }

    /// The index of the next source token, to compare two positions.
    pub fn position(&self) -> usize {
        self.pos
    }

    /// How many comments are written so far, to tell whether a stretch
    /// of tokens held one.
    pub fn comments_written(&self) -> u32 {
        self.next_comment
    }

    /// The source from byte `start` to the end of the token written
    /// last.
    pub fn text_since(&self, start: u32) -> &'a str {
        let end = match self.pos.checked_sub(1) {
            Some(prev) => self.tokens[prev].span.end,
            None => start,
        };
        &self.source[start as usize..end.max(start) as usize]
    }

    fn text_of(&self, tok: &Tok) -> &'a str {
        &self.source[tok.span.start as usize..tok.span.end as usize]
    }

    /// Record that the source is not what the printer expects.
    pub fn fail(&mut self, message: impl Into<String>) {
        if self.error.is_none() {
            self.error = Some(Mismatch {
                message: message.into(),
                span: self.tok().span,
            });
        }
    }

    fn class(&self, index: u32, next: &Tok, end: u32) -> Class {
        let comment = &self.comments[index as usize];
        if comment.kind == CommentKind::Line {
            return Class::EndOfLine(true);
        }
        if comment.text(self.source).contains('\n') {
            return Class::EndOfLine(true);
        }
        let token_follows = index + 1 == end;
        let next_newlines = if token_follows {
            next.newlines_before
        } else {
            self.comments[index as usize + 1].newlines_before
        };
        if next_newlines == 0 && !(token_follows && next.kind == Token::Eof) {
            Class::Inline
        } else {
            Class::EndOfLine(false)
        }
    }

    fn comment_text(&self, index: u32) -> String {
        self.comments[index as usize]
            .text(self.source)
            .trim_end()
            .to_string()
    }

    /// The comments in front of the next token that are not written
    /// yet.
    fn gap(&self) -> std::ops::Range<u32> {
        let range = &self.tok().comments;
        range.start.max(self.next_comment)..range.end.max(self.next_comment)
    }

    /// Whether the next token, or the first comment on a line of its own
    /// in front of it, stands after an empty line.
    pub fn blank_before(&self) -> bool {
        for index in self.gap() {
            let newlines = self.comments[index as usize].newlines_before;
            if newlines > 0 {
                return newlines >= 2;
            }
        }
        self.tok().newlines_before >= 2
    }

    /// Whether a line break stands between the token written last (and
    /// the comments on its line) and the next token.
    pub fn line_break_before(&self) -> bool {
        self.gap()
            .any(|index| self.comments[index as usize].newlines_before > 0)
            || self.tok().newlines_before > 0
    }

    /// Whether a comment in front of the next token will stand on a line
    /// of its own.
    pub fn own_line_comment_ahead(&self) -> bool {
        let tok = self.tok();
        let gap = self.gap();
        gap.clone().any(|index| {
            let newlines = self.comments[index as usize].newlines_before;
            matches!(self.class(index, tok, gap.end), Class::EndOfLine(_))
                && (newlines > 0 || self.pos == 0)
        })
    }

    /// Whether a comment stands in front of the next token.
    pub fn comment_ahead(&self) -> bool {
        !self.gap().is_empty() || !self.carried.is_empty()
    }

    /// The comments in front of the next token, and those carried over
    /// from skipped tokens. `token` calls this; the printer calls it
    /// where the comments belong inside an indentation that the token
    /// itself is outside of (the comments before a closing brace).
    pub fn leading(&mut self) -> Doc {
        let mut docs = std::mem::take(&mut self.carried);
        let tok = self.tok();
        let gap = self.gap();
        let mut first = true;
        let mut own_line = false;
        for index in gap.clone() {
            let newlines = self.comments[index as usize].newlines_before;
            let text = self.comment_text(index);
            if !first && newlines >= 2 {
                docs.push(Doc::BlankLine);
            }
            match self.class(index, tok, gap.end) {
                Class::Inline => docs.push(Doc::Comment(text)),
                Class::EndOfLine(ends_line) if newlines == 0 && self.pos > 0 => {
                    docs.push(Doc::LineSuffix(text, ends_line));
                }
                Class::EndOfLine(_) => {
                    docs.push(Doc::OwnLine(text));
                    own_line = true;
                }
            }
            first = false;
        }
        self.next_comment = gap.end;
        // An empty line between a comment and what it stands above is
        // kept: it says the comment is not about that.
        if own_line
            && tok.newlines_before >= 2
            && !is_closer_or_comma(&tok.kind)
            && tok.kind != Token::Eof
        {
            docs.push(Doc::BlankLine);
        }
        Doc::concat(docs)
    }

    /// The comments behind the token written last, on its line.
    pub fn trailing(&mut self) -> Doc {
        let tok = self.tok();
        let gap = self.gap();
        let mut docs = Vec::new();
        for index in gap.clone() {
            if self.comments[index as usize].newlines_before > 0 || self.pos == 0 {
                break;
            }
            let text = self.comment_text(index);
            match self.class(index, tok, gap.end) {
                Class::EndOfLine(ends_line) => docs.push(Doc::LineSuffix(text, ends_line)),
                Class::Inline if is_closer_or_comma(&tok.kind) => docs.push(Doc::Comment(text)),
                Class::Inline => break,
            }
            self.next_comment = index + 1;
        }
        Doc::concat(docs)
    }

    /// The comments of the file's first lines that are not about the
    /// first declaration: those above the last empty line in front of
    /// it. They stay at the top when the declarations are put in order.
    pub fn header(&mut self) -> Doc {
        if self.pos != 0 {
            return Doc::Nil;
        }
        let tok = self.tok();
        let gap = self.gap();
        let mut end = gap.start;
        for index in gap.clone().skip(1) {
            if self.comments[index as usize].newlines_before >= 2 {
                end = index;
            }
        }
        if tok.newlines_before >= 2 {
            end = gap.end;
        }
        let mut docs = Vec::new();
        for index in gap.start..end {
            if index > gap.start && self.comments[index as usize].newlines_before >= 2 {
                docs.push(Doc::BlankLine);
            }
            let text = self.comment_text(index);
            // A header comment has a comment or an empty line behind it.
            match self.class(index, tok, gap.end) {
                Class::Inline => docs.push(Doc::Comment(text)),
                Class::EndOfLine(_) => docs.push(Doc::OwnLine(text)),
            }
        }
        self.next_comment = end;
        Doc::concat(docs)
    }

    /// The next source token as text, without its comments, and move on.
    fn bare(&mut self) -> Doc {
        let tok = self.tok();
        let text = self.text_of(tok);
        if tok.kind != Token::Eof {
            self.pos += 1;
        }
        match tok.kind {
            // The span of the rest of a string starts behind the brace
            // that closes the interpolation.
            Token::StringMiddle(_) | Token::StringEnd(_) => Doc::text(format!("}}{text}")),
            _ => Doc::text(text),
        }
    }

    /// Skip the next source token; its comments are carried to the next
    /// token that is written.
    fn skip_one(&mut self) {
        let tok = self.tok();
        let gap = self.gap();
        for index in gap.clone() {
            let text = self.comment_text(index);
            self.carried.push(match self.class(index, tok, gap.end) {
                Class::Inline => Doc::Comment(text),
                Class::EndOfLine(ends_line) => Doc::LineSuffix(text, ends_line),
            });
        }
        self.next_comment = gap.end;
        self.pos += 1;
        let trailing = self.trailing();
        if !trailing.is_nil() {
            self.carried.push(trailing);
        }
    }

    /// Move to the next source token of the kind of `kind`, over
    /// parentheses and commas the printer does not write. `false`, with
    /// the error recorded, if another token is in the way.
    fn seek(&mut self, kind: &Token) -> bool {
        loop {
            if self.at(kind) {
                return true;
            }
            match self.peek() {
                Token::LParen | Token::RParen | Token::Comma => self.skip_one(),
                found => {
                    self.fail(format!(
                        "the printer expected {kind} where the source has {found}"
                    ));
                    return false;
                }
            }
        }
    }

    /// Write the next source token, which is of the kind of `kind`, with
    /// its comments.
    pub fn token(&mut self, kind: &Token) -> Doc {
        if !self.seek(kind) {
            return Doc::Nil;
        }
        let leading = self.leading();
        let text = self.bare();
        let trailing = self.trailing();
        Doc::concat(vec![leading, text, trailing])
    }

    /// As `token`, in two steps: the token with the comments in front of
    /// it, and (from `trailing`) the comments behind it.
    pub fn token_without_trailing(&mut self, kind: &Token) -> Doc {
        if !self.seek(kind) {
            return Doc::Nil;
        }
        let leading = self.leading();
        let text = self.bare();
        Doc::concat(vec![leading, text])
    }

    /// If the next source token is of the kind of `kind`, skip it and
    /// give its comments for this place: the printer writes this token
    /// by its own rule (a trailing comma).
    pub fn skip(&mut self, kind: &Token) -> Doc {
        if !self.at(kind) {
            return Doc::Nil;
        }
        self.skip_one();
        Doc::concat(std::mem::take(&mut self.carried))
    }

    /// Whether every token is written.
    pub fn at_end(&self) -> bool {
        self.tok().kind == Token::Eof
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::doc::render;
    use crate::lexer::Lexer;
    use crate::source::FileId;

    /// Write every token of `input` with one space between two, and a
    /// line break where the source has one before a token; skip the
    /// tokens of the kinds in `skip`.
    fn respace(input: &str, skip: &[Token]) -> String {
        let lexed = Lexer::new(FileId::default(), input).tokenize().unwrap();
        let mut cursor = Cursor::new(input, &lexed);
        let mut docs = vec![cursor.header()];
        let mut first = true;
        while !cursor.at_end() {
            let kind = cursor.peek().clone();
            if skip.contains(&kind) {
                docs.push(cursor.skip(&kind));
                continue;
            }
            if !first {
                let newline = cursor.line_break_before();
                if cursor.blank_before() {
                    docs.push(Doc::BlankLine);
                } else if newline {
                    docs.push(Doc::HardLine);
                } else {
                    docs.push(Doc::text(" "));
                }
            }
            first = false;
            docs.push(cursor.token(&kind));
        }
        docs.push(cursor.leading());
        assert!(cursor.error.is_none());
        render(&Doc::concat(docs), 100)
    }

    #[test]
    fn comments_keep_their_place_and_their_kind() {
        let cases = [
            // A trailing comment stays behind its token.
            ("a -- c\nb\n", "a -- c\nb\n"),
            ("a {- c -}\nb\n", "a {- c -}\nb\n"),
            // An own-line comment stays on its line.
            ("a\n-- c\nb\n", "a\n-- c\nb\n"),
            ("a\n{- c -}\nb\n", "a\n{- c -}\nb\n"),
            ("a\n{- c\n   d -}\nb\n", "a\n{- c\n   d -}\nb\n"),
            // An inline comment stays between its tokens.
            ("a {- c -} b\n", "a {- c -} b\n"),
            ("a\n{- c -} b\n", "a\n{- c -} b\n"),
            ("({- c -} a {- d -})\n", "( {- c -} a {- d -} )\n"),
            // Empty lines: one is kept, between comments too.
            ("a\n\n\n-- c\n\n-- d\nb\n", "a\n\n-- c\n\n-- d\nb\n"),
            ("a\n-- c\n\nb\n", "a\n-- c\n\nb\n"),
            // The end of the file.
            ("a -- c", "a -- c\n"),
            ("a {- c -}", "a {- c -}\n"),
            ("a\n\n-- c\n", "a\n-- c\n"),
            ("-- only\n", "-- only\n"),
            // A comment that spans lines ends its line.
            ("a {- c\n d -} b\n", "a b {- c\n d -}\n"),
            // The space behind a comment is not part of it.
            ("a -- c   \nb\n", "a -- c\nb\n"),
        ];
        for (input, expected) in cases {
            assert_eq!(respace(input, &[]), expected, "input: {input:?}");
            // The result is a fixed point.
            assert_eq!(respace(expected, &[]), expected, "again: {expected:?}");
        }
    }

    #[test]
    fn the_header_ends_at_the_last_empty_line_before_the_first_token() {
        let header = |input: &str| {
            let lexed = Lexer::new(FileId::default(), input).tokenize().unwrap();
            let mut cursor = Cursor::new(input, &lexed);
            let header = cursor.header();
            let rest = cursor.leading();
            (render(&header, 100), render(&rest, 100))
        };
        assert_eq!(
            header("-- h\n\n-- i\n\n-- about a\na"),
            ("-- h\n\n-- i\n".to_string(), "-- about a\n".to_string())
        );
        assert_eq!(
            header("-- about a\na"),
            (String::new(), "-- about a\n".to_string())
        );
        assert_eq!(header("-- h\n\na"), ("-- h\n".to_string(), String::new()));
    }

    #[test]
    fn the_comments_of_a_skipped_token_go_to_the_next_written_one() {
        // A line comment behind a skipped comma lands at the end of the
        // line; an own-line comment in front of one does too, because
        // the line break it stood at is gone.
        assert_eq!(respace("a, -- c\nb\n", &[Token::Comma]), "a -- c\nb\n");
        assert_eq!(respace("a\n-- c\n, b\n", &[Token::Comma]), "a b -- c\n");
        assert_eq!(respace("a {- c -} , b\n", &[Token::Comma]), "a {- c -} b\n");
    }

    #[test]
    fn a_token_the_source_does_not_have_is_an_error_but_brackets_are_skipped() {
        let input = "((a)) -- c\n+ b";
        let lexed = Lexer::new(FileId::default(), input).tokenize().unwrap();
        let mut cursor = Cursor::new(input, &lexed);
        let ident = Token::Ident(crate::intern::intern("x"));
        let a = cursor.token(&ident);
        let plus = cursor.token(&Token::Plus);
        assert!(cursor.error.is_none());
        assert_eq!(
            render(&Doc::concat(vec![a, Doc::text(" "), plus]), 100),
            "a + -- c\n"
        );
        cursor.token(&Token::Star);
        let error = cursor.error.expect("`*` is not next");
        assert!(error.message.contains("expected *"), "{}", error.message);
    }
}
