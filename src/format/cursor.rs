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
//! - a `--` comment behind a token of its line is a
//!   [`Doc::LineSuffix`];
//! - a `{- -}` comment with a token in front of it or behind it on its
//!   line is a [`Doc::Comment`]: it stays between its tokens. It is
//!   written with the token behind it, or with the token in front of it
//!   when a comma, a closing bracket or the end of the line follows;
//! - a comment on a line without a token, or with nothing but a closing
//!   bracket behind it, is a [`Doc::OwnLine`].
//!
//! A token the printer does not write (redundant parentheses, a comma it
//! decides for itself) is skipped. The comments in front of a skipped
//! opening parenthesis stay as they are, in front of what it opened;
//! those of a skipped comma or closing parenthesis go to the next token
//! that is written and end the line there, because the line break they
//! stood at may have been allowed by the skipped token alone.

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
    /// For each `(` that is open at the cursor, whether it was skipped.
    parens: Vec<bool>,
    /// For each token that is a `(`, the index of its `)`.
    closers: Vec<usize>,
    /// The opening parentheses skipped since the last written token:
    /// for each, the index of the first comment behind it and the line
    /// breaks in front of it. Those line breaks stand in front of what
    /// follows the parenthesis now.
    skipped_open: Vec<(u32, u8)>,
    /// A comment that ends its line was written or carried since the
    /// last token: a second one cannot follow it on that line.
    line_ended: bool,
    /// The first thing the printer asked for that the source does not
    /// hold. Once set, the printer's result is not used.
    pub error: Option<Mismatch>,
}

enum Class {
    Inline,
    /// With whether it is a `--` comment: nothing can follow that on
    /// its line.
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
        let tokens: Vec<&Tok> = lexed
            .tokens
            .iter()
            .filter(|tok| tok.kind != Token::Newline)
            .collect();
        let mut closers = vec![0; tokens.len()];
        let mut open = Vec::new();
        for (i, tok) in tokens.iter().enumerate() {
            match tok.kind {
                Token::LParen => open.push(i),
                Token::RParen => {
                    if let Some(opener) = open.pop() {
                        closers[opener] = i;
                    }
                }
                _ => {}
            }
        }
        Cursor {
            source,
            tokens,
            closers,
            skipped_open: Vec::new(),
            line_ended: false,
            comments: &lexed.comments,
            pos: 0,
            next_comment: 0,
            carried: Vec::new(),
            parens: Vec::new(),
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

    /// Where the next source token starts.
    pub fn offset(&self) -> u32 {
        self.tok().span.start
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
        // Inline: the next token stands on the comment's line, with
        // nothing but comments between them.
        if !self.token_on_line(index, end) {
            return Class::EndOfLine(false);
        }
        // If that token is a closing bracket or a comma, and no token
        // stands in front of the comment on its line, the comment is
        // taken as one on a line of its own: a line that holds only
        // comments and a closing bracket is not kept as such.
        let start = self.next_comment.min(next.comments.start).min(index);
        let fresh_line =
            self.pos == 0 || (start..=index).any(|i| self.newlines_before_comment(i) > 0);
        if fresh_line && is_closer_or_comma(&next.kind) {
            return Class::EndOfLine(false);
        }
        Class::Inline
    }

    /// Whether the next token stands on the line of the comment `index`,
    /// with nothing but comments between them.
    fn token_on_line(&self, index: u32, end: u32) -> bool {
        self.newlines_before_token() == 0
            && self.tok().kind != Token::Eof
            && (index + 1..end).all(|later| self.newlines_before_comment(later) == 0)
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
        // The comments of a skipped opening parenthesis are still to
        // be written: they stand in front of this token now.
        self.next_comment..self.tok().comments.end.max(self.next_comment)
    }

    /// Whether the next token, or the first comment on a line of its own
    /// in front of it, stands after an empty line.
    pub fn blank_before(&self) -> bool {
        for index in self.gap() {
            let newlines = self.newlines_before_comment(index);
            if newlines > 0 {
                return newlines >= 2;
            }
        }
        self.newlines_before_token() >= 2
    }

    /// The line breaks in front of the next token, those in front of
    /// skipped opening parentheses included.
    fn newlines_before_token(&self) -> u8 {
        let tok = self.tok();
        self.skipped_open
            .iter()
            .filter(|(first_comment, _)| *first_comment == tok.comments.end)
            .map(|(_, newlines)| *newlines)
            .fold(tok.newlines_before, u8::max)
    }

    /// The line breaks in front of the comment `index`, those in front
    /// of skipped opening parentheses included.
    fn newlines_before_comment(&self, index: u32) -> u8 {
        self.skipped_open
            .iter()
            .filter(|(first_comment, _)| *first_comment == index)
            .map(|(_, newlines)| *newlines)
            .fold(self.comments[index as usize].newlines_before, u8::max)
    }

    /// Whether a line break stands between the token written last (and
    /// the comments on its line) and the next token.
    pub fn line_break_before(&self) -> bool {
        self.gap()
            .any(|index| self.newlines_before_comment(index) > 0)
            || self.newlines_before_token() > 0
    }

    /// Whether a comment in front of the next token will stand on a line
    /// of its own.
    pub fn own_line_comment_ahead(&self) -> bool {
        let tok = self.tok();
        let gap = self.gap();
        gap.clone().any(|index| {
            let newlines = self.newlines_before_comment(index);
            matches!(self.class(index, tok, gap.end), Class::EndOfLine(_))
                && (newlines > 0 || self.pos == 0)
        })
    }

    /// Whether a comment that ends its line stands behind the token
    /// written last.
    pub fn line_ended(&self) -> bool {
        self.line_ended
    }

    /// Whether a comment stands in front of the next token.
    pub fn comment_ahead(&self) -> bool {
        !self.gap().is_empty() || !self.carried.is_empty()
    }

    /// As `leading`, without the empty lines between the comments and
    /// behind them: for a declaration that is moved (an import).
    pub fn leading_without_blank_lines(&mut self) -> Doc {
        fn strip(doc: Doc) -> Doc {
            match doc {
                Doc::BlankLine => Doc::Nil,
                Doc::Concat(parts) => Doc::Concat(parts.into_iter().map(strip).collect()),
                other => other,
            }
        }
        strip(self.leading())
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
            let newlines = self.newlines_before_comment(index);
            let text = self.comment_text(index);
            if !first && newlines >= 2 {
                docs.push(Doc::BlankLine);
            }
            match self.class(index, tok, gap.end) {
                Class::Inline => docs.push(Doc::Comment(text)),
                // Behind a comment on a line of its own, a comment is
                // on a line of its own too.
                Class::EndOfLine(true) if newlines == 0 && self.pos > 0 && !own_line => {
                    docs.push(Doc::LineSuffix(text));
                }
                Class::EndOfLine(false) if newlines == 0 && self.pos > 0 && !own_line => {
                    docs.push(Doc::Comment(text));
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
            && self.newlines_before_token() >= 2
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
            if self.newlines_before_comment(index) > 0 || self.pos == 0 {
                break;
            }
            let text = self.comment_text(index);
            match self.class(index, tok, gap.end) {
                Class::EndOfLine(true) => {
                    docs.push(Doc::LineSuffix(text));
                    self.line_ended = true;
                }
                // A `{- -}` comment at the end of the line stays behind
                // its token, wherever the line ends then.
                Class::EndOfLine(false) => docs.push(Doc::Comment(text)),
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
            if self.newlines_before_comment(index) >= 2 {
                end = index;
            }
        }
        if tok.newlines_before >= 2 {
            end = gap.end;
        }
        let mut docs = Vec::new();
        for index in gap.start..end {
            if index > gap.start && self.newlines_before_comment(index) >= 2 {
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
        self.skipped_open.clear();
        self.line_ended = false;
        self.step(false);
        match tok.kind {
            // The span of the rest of a string starts behind the brace
            // that closes the interpolation.
            Token::StringMiddle(_) | Token::StringEnd(_) => Doc::text(format!("}}{text}")),
            _ => Doc::text(text),
        }
    }

    /// Move past the next source token, which is written or skipped.
    fn step(&mut self, skipped: bool) {
        match self.tok().kind {
            Token::Eof => return,
            Token::LParen => self.parens.push(skipped),
            Token::RParen => {
                self.parens.pop();
            }
            _ => {}
        }
        self.pos += 1;
    }

    /// Skip the closing parentheses whose opening ones were skipped: the
    /// printer has written what stood between them. Called after an
    /// expression or a pattern, so that the printer can look at what
    /// follows it.
    pub fn close_skipped_parens(&mut self) {
        while self.at(&Token::RParen) && self.parens.last() == Some(&true) {
            self.skip_one();
        }
    }

    /// How many of the `(` at the cursor are parentheses around the
    /// expression that starts here and ends at byte `end`: those whose
    /// `)` stand directly behind the expression's last token. A `(`
    /// that closes earlier is around a part of the expression.
    pub fn wrappers(&self, end: u32) -> usize {
        let last = self.tokens.partition_point(|tok| tok.span.end < end);
        if self.tokens.get(last).is_none_or(|tok| tok.span.end != end) {
            return 0;
        }
        let run = self.tokens[last + 1..]
            .iter()
            .take_while(|tok| tok.kind == Token::RParen)
            .count();
        self.tokens[self.pos.min(self.tokens.len())..]
            .iter()
            .enumerate()
            .take_while(|(i, tok)| {
                let closer = self.closers[self.pos + i];
                tok.kind == Token::LParen && closer > last && closer <= last + run
            })
            .count()
    }

    /// Skip `count` tokens of the kind of `kind`, as far as they stand
    /// at the cursor.
    pub fn skip_n(&mut self, kind: &Token, count: usize) {
        for _ in 0..count {
            if !self.at(kind) {
                break;
            }
            self.skip_one();
        }
    }

    /// The comments carried over from skipped tokens, for this place:
    /// behind what the skipped tokens closed.
    pub fn carried(&mut self) -> Doc {
        Doc::concat(std::mem::take(&mut self.carried))
    }

    /// Skip the opening parentheses at the cursor: the printer knows
    /// that what it is about to write does not start with one.
    pub fn skip_open_parens(&mut self) {
        while self.at(&Token::LParen) {
            self.skip_one();
        }
    }

    /// Skip `()` at the cursor: an empty list the tree does not record.
    pub fn skip_empty_parens(&mut self) {
        if self.at(&Token::LParen) && matches!(self.peek_at(1), Token::RParen) {
            self.skip_one();
            self.skip_one();
        }
    }

    /// Skip the next source token; its comments are carried to the next
    /// token that is written.
    fn skip_one(&mut self) {
        let tok = self.tok();
        if tok.kind == Token::LParen {
            // The comments in front of an opening parenthesis stay
            // where they are, in front of what it opened.
            let newlines = self.newlines_before_token();
            self.skipped_open.push((tok.comments.end, newlines));
        } else {
            // The line break at a comment in front of a closing
            // parenthesis or a comma was inside brackets that may be
            // gone: the comment ends the line instead.
            let gap = self.gap();
            for index in gap.clone() {
                let text = self.comment_text(index);
                let class = self.class(index, tok, gap.end);
                self.carry(text, class);
            }
            self.next_comment = gap.end;
        }
        self.step(true);
        // The comments behind the skipped token, on its line.
        let tok = self.tok();
        let gap = self.gap();
        for index in gap.clone() {
            if self.newlines_before_comment(index) > 0 {
                break;
            }
            let class = self.class(index, tok, gap.end);
            if matches!(class, Class::Inline) && !is_closer_or_comma(&tok.kind) {
                break;
            }
            let text = self.comment_text(index);
            self.carry(text, class);
            self.next_comment = index + 1;
        }
    }

    /// Carry the comment of a skipped token to the next written token.
    /// It ends the line there; behind another comment that ends the
    /// line, it gets a line of its own.
    fn carry(&mut self, text: String, class: Class) {
        self.carried.push(match class {
            Class::Inline | Class::EndOfLine(false) => Doc::Comment(text),
            Class::EndOfLine(true) if self.line_ended => Doc::OwnLine(text),
            Class::EndOfLine(true) => {
                self.line_ended = true;
                Doc::LineSuffix(text)
            }
        });
    }

    /// Whether a comment stands in the source between the bytes `start`
    /// and `end`.
    pub fn comments_between(&self, start: u32, end: u32) -> bool {
        let first = self.comments.partition_point(|c| c.span.start < start);
        self.comments.get(first).is_some_and(|c| c.span.start < end)
    }

    /// Whether a comment stands directly inside one of the `count`
    /// pairs of parentheses at the cursor, which are around an
    /// expression that ends at byte `end`: behind an opening one or in
    /// front of a closing one. There a line break is allowed that is not
    /// allowed without the parentheses.
    pub fn comments_inside_wrappers(&self, count: usize, end: u32) -> bool {
        let Some(inner) = self.tokens.get(self.pos + count) else {
            return false;
        };
        let last = self.tokens.partition_point(|tok| tok.span.end < end);
        let Some(closer) = self.tokens.get(last + count) else {
            return false;
        };
        self.comments_between(self.tok().span.end, inner.span.start)
            || self.comments_between(end, closer.span.start)
    }

    /// Whether the next source token is a `(` with a comment directly
    /// behind it or in front of its `)`.
    pub fn comments_inside_paren(&self) -> bool {
        if !self.at(&Token::LParen) {
            return false;
        }
        let closer = self.tokens[self.closers[self.pos]];
        let behind = self.tokens[(self.pos + 1).min(self.tokens.len() - 1)];
        !behind.comments.is_empty() || !closer.comments.is_empty()
    }

    /// Whether the `(` at the cursor holds a token of the kind of
    /// `kind` that is in no other bracket.
    pub fn paren_holds(&self, kind: &Token) -> bool {
        if !self.at(&Token::LParen) {
            return false;
        }
        let mut depth = 0;
        for tok in &self.tokens[self.pos + 1..self.closers[self.pos].max(self.pos + 1)] {
            match tok.kind {
                Token::LParen
                | Token::LBracket
                | Token::LBrace
                | Token::HashBrace
                | Token::HashBracket
                | Token::StringStart(_) => depth += 1,
                Token::RParen | Token::RBracket | Token::RBrace | Token::StringEnd(_) => depth -= 1,
                _ if depth == 0
                    && std::mem::discriminant(&tok.kind) == std::mem::discriminant(kind) =>
                {
                    return true;
                }
                _ => {}
            }
        }
        false
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
            // A comment that spans lines stays between its tokens too.
            ("a {- c\n d -} b\n", "a {- c\n d -} b\n"),
            // A line of comments and a closing bracket: the comments
            // get lines of their own.
            ("( a\n{- c -} {- d -} )\n", "( a\n{- c -}\n{- d -}\n)\n"),
            ("( a {- c -} )\n", "( a {- c -} )\n"),
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
        // The comments in front of a skipped opening parenthesis stay in
        // front of what it opened.
        let parens = [Token::LParen, Token::RParen];
        assert_eq!(respace("a\n\n-- c\n(b)\n", &parens), "a\n\n-- c\nb\n");
        assert_eq!(
            respace("a\n-- c\n\n({- d -} b)\n", &parens),
            "a\n-- c\n\n{- d -} b\n"
        );
        assert_eq!(
            respace("a ( -- c\nb -- d\n)\n", &parens),
            "a -- c\nb -- d\n"
        );
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
