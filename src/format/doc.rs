//! The layout document and its renderer.
//!
//! The printer turns a program into a [`Doc`]: text, places where a line
//! may break, groups that break all of their places or none, and
//! comments. [`render`] lays a `Doc` out in a given width. It measures
//! each group once (`fits`) and keeps its state on explicit stacks, so
//! its depth does not grow with the document's.

/// Columns per level of [`Doc::Nest`].
pub const INDENT: usize = 2;

#[derive(Debug, Clone, PartialEq)]
pub enum Doc {
    Nil,
    /// Text as it is. A line break inside it (a multi-line string
    /// literal) is written as it is, without indentation.
    Text(String),
    /// A one-line `{- -}` comment between two tokens of a line. The
    /// renderer keeps it apart from its neighbours by a space, except
    /// after an opening bracket and before a closing one or a comma.
    Comment(String),
    /// A space in a group that fits its line, a line break in one that
    /// does not.
    Line,
    /// Nothing in a group that fits, a line break in one that does not.
    SoftLine,
    /// A line break. The groups around it break.
    HardLine,
    /// A line break and one empty line, wherever the output stands. The
    /// groups around it break.
    BlankLine,
    /// The unit of layout: on one line if its content fits the rest of
    /// the line, else every `Line` and `SoftLine` directly in it breaks.
    /// The flag says that the content holds a forced break; set by
    /// [`Doc::group`].
    Group(Box<Doc>, bool),
    /// The content, with every line break in it indented one level more.
    Nest(Box<Doc>),
    Concat(Vec<Doc>),
    /// The first if the enclosing group is broken, the second if not.
    IfBreak(Box<Doc>, Box<Doc>),
    /// A comment at the end of a line: held back and written before the
    /// next line break of the output, so it never comments out what
    /// follows it on its line. A group does not fit when one is waiting
    /// at one of its `Line`s. The flag says that nothing can follow the
    /// comment on its line (a `--` comment, or a `{- -}` that spans
    /// lines).
    LineSuffix(String, bool),
    /// A comment on a line of its own, wherever the output stands. The
    /// groups around it break.
    OwnLine(String),
}

impl Doc {
    pub fn text(text: impl Into<String>) -> Doc {
        Doc::Text(text.into())
    }

    pub fn concat(parts: Vec<Doc>) -> Doc {
        Doc::Concat(parts)
    }

    pub fn nest(doc: Doc) -> Doc {
        Doc::Nest(Box::new(doc))
    }

    pub fn group(doc: Doc) -> Doc {
        let forced = doc.forces_break();
        Doc::Group(Box::new(doc), forced)
    }

    pub fn if_break(broken: Doc, flat: Doc) -> Doc {
        Doc::IfBreak(Box::new(broken), Box::new(flat))
    }

    /// Whether the document holds a forced break outside the branches of
    /// an `IfBreak`: a group around it cannot be on one line. A nested
    /// group answers from its flag, so building a document looks at each
    /// node a bounded number of times.
    pub fn forces_break(&self) -> bool {
        let mut stack = vec![self];
        while let Some(doc) = stack.pop() {
            match doc {
                Doc::HardLine | Doc::BlankLine | Doc::OwnLine(_) => return true,
                Doc::Group(_, forced) => {
                    if *forced {
                        return true;
                    }
                }
                Doc::Nest(inner) => stack.push(inner),
                Doc::Concat(parts) => stack.extend(parts.iter()),
                Doc::Nil
                | Doc::Text(_)
                | Doc::Comment(_)
                | Doc::Line
                | Doc::SoftLine
                | Doc::IfBreak(..)
                | Doc::LineSuffix(..) => {}
            }
        }
        false
    }

    pub fn is_nil(&self) -> bool {
        match self {
            Doc::Nil => true,
            Doc::Concat(parts) => parts.iter().all(Doc::is_nil),
            _ => false,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Flat,
    Break,
}

/// A document waiting to be written, with the indentation and the mode
/// of the group it stands in.
type Cmd<'a> = (usize, Mode, &'a Doc);

/// A comment held back for the end of the line.
struct Suffix {
    text: String,
    /// Where in the output it was met.
    at: usize,
    /// Nothing can follow it on its line.
    ends_line: bool,
}

struct Renderer {
    width: usize,
    out: String,
    /// Characters on the current line, its indentation included.
    col: usize,
    /// Nothing is written on the current line yet; `indent` is the
    /// indentation it gets when something is.
    at_line_start: bool,
    indent: usize,
    suffixes: Vec<Suffix>,
    /// An own-line comment ends the current line: the next text starts a
    /// new one.
    line_is_closed: bool,
    /// An inline comment was the last thing written.
    after_comment: bool,
}

/// Lay `doc` out in `width` columns. The result ends with one line break
/// unless it is empty.
pub fn render(doc: &Doc, width: usize) -> String {
    let mut r = Renderer {
        width,
        out: String::new(),
        col: 0,
        at_line_start: true,
        indent: 0,
        suffixes: Vec::new(),
        line_is_closed: false,
        after_comment: false,
    };
    let mut stack: Vec<Cmd> = vec![(0, Mode::Break, doc)];
    while let Some((indent, mode, doc)) = stack.pop() {
        match doc {
            Doc::Nil => {}
            Doc::Text(text) => r.text(text, indent),
            Doc::Comment(text) => r.comment(text, indent),
            Doc::Line | Doc::SoftLine => {
                if mode == Mode::Break {
                    r.newline(indent);
                } else if matches!(doc, Doc::Line) {
                    r.text(" ", indent);
                }
            }
            Doc::HardLine => r.newline(indent),
            Doc::BlankLine => r.blank_line(indent),
            Doc::Group(inner, forced) => {
                let mode = if *forced || !r.fits((indent, Mode::Flat, inner), &stack) {
                    Mode::Break
                } else {
                    Mode::Flat
                };
                stack.push((indent, mode, inner));
            }
            Doc::Nest(inner) => stack.push((indent + INDENT, mode, inner)),
            Doc::Concat(parts) => {
                stack.extend(parts.iter().rev().map(|part| (indent, mode, part)));
            }
            Doc::IfBreak(broken, flat) => {
                let chosen = if mode == Mode::Break { broken } else { flat };
                stack.push((indent, mode, chosen));
            }
            Doc::LineSuffix(text, ends_line) => r.suffix(text, *ends_line, indent),
            Doc::OwnLine(text) => r.own_line(text, indent),
        }
    }
    r.finish()
}

impl Renderer {
    /// Whether `next`, followed by the rest of the document up to its
    /// first line break, fits what is left of the current line.
    fn fits(&self, next: Cmd, rest: &[Cmd]) -> bool {
        let start = if self.at_line_start {
            self.indent
        } else {
            self.col
        };
        let mut left = self.width as isize - start as isize;
        let mut suffix_waits = !self.suffixes.is_empty();
        let mut rest_index = rest.len();
        let mut stack: Vec<Cmd> = vec![next];
        loop {
            let (indent, mode, doc) = match stack.pop() {
                Some(cmd) => cmd,
                None if rest_index == 0 => return true,
                None => {
                    rest_index -= 1;
                    rest[rest_index]
                }
            };
            match doc {
                Doc::Nil => {}
                Doc::Text(text) => match text.split_once('\n') {
                    // The line ends inside the text.
                    Some((first, _)) => return left >= first.chars().count() as isize,
                    None => left -= text.chars().count() as isize,
                },
                Doc::Comment(text) => left -= text.chars().count() as isize + 1,
                Doc::Line | Doc::SoftLine => {
                    if mode == Mode::Break {
                        return true;
                    }
                    if suffix_waits {
                        return false;
                    }
                    if matches!(doc, Doc::Line) {
                        left -= 1;
                    }
                }
                Doc::HardLine | Doc::BlankLine | Doc::OwnLine(_) => return true,
                Doc::Group(inner, forced) => {
                    let mode = if *forced { Mode::Break } else { mode };
                    stack.push((indent, mode, inner));
                }
                Doc::Nest(inner) => stack.push((indent, mode, inner)),
                Doc::Concat(parts) => {
                    stack.extend(parts.iter().rev().map(|part| (indent, mode, part)));
                }
                Doc::IfBreak(broken, flat) => {
                    let chosen = if mode == Mode::Break { broken } else { flat };
                    stack.push((indent, mode, chosen));
                }
                Doc::LineSuffix(..) => suffix_waits = true,
            }
            if left < 0 {
                return false;
            }
        }
    }

    /// Start the line that `text` is about to be written on.
    fn open_line(&mut self, indent: usize) {
        if self.line_is_closed {
            self.newline(indent);
        }
        if self.at_line_start {
            self.out.extend(std::iter::repeat_n(' ', self.indent));
            self.col = self.indent;
            self.at_line_start = false;
        }
    }

    fn push(&mut self, text: &str) {
        self.out.push_str(text);
        self.col = match text.rsplit_once('\n') {
            Some((_, last)) => last.chars().count(),
            None => self.col + text.chars().count(),
        };
    }

    fn text(&mut self, text: &str, indent: usize) {
        if text.is_empty() {
            return;
        }
        // A space between the parts of a line is not worth a line of
        // its own.
        if text == " " && (self.at_line_start || self.line_is_closed) {
            return;
        }
        self.open_line(indent);
        if self.after_comment && !text.starts_with([' ', ')', ']', '}', ',']) {
            self.push(" ");
        }
        self.after_comment = false;
        self.push(text);
    }

    fn comment(&mut self, text: &str, indent: usize) {
        // A comment that waits for the end of the line stands before
        // this one in the source: it has to be written first.
        if !self.suffixes.is_empty() {
            self.settle_suffixes();
        }
        let fresh = self.at_line_start || self.line_is_closed;
        self.open_line(indent);
        if !fresh && !self.out.ends_with([' ', '(', '[', '{']) {
            self.push(" ");
        }
        self.push(text);
        self.after_comment = true;
    }

    fn suffix(&mut self, text: &str, ends_line: bool, indent: usize) {
        if self.line_is_closed {
            self.newline(indent);
        }
        // Nothing can follow a comment that ends its line, so the line
        // is broken where that comment was met.
        if self.suffixes.last().is_some_and(|last| last.ends_line) {
            self.settle_suffixes();
        }
        self.suffixes.push(Suffix {
            text: text.to_string(),
            at: self.out.len(),
            ends_line,
        });
    }

    /// Write the waiting comments where they were met, and break the
    /// line after one that ends its line: what was written behind it
    /// moves to a new line, indented one level more than this one.
    fn settle_suffixes(&mut self) {
        let line_start = self.out.rfind('\n').map_or(0, |i| i + 1);
        let line_indent = if self.at_line_start {
            self.indent
        } else {
            self.out[line_start..]
                .chars()
                .take_while(|c| *c == ' ')
                .count()
        };
        for suffix in std::mem::take(&mut self.suffixes).into_iter().rev() {
            let at = suffix.at.max(line_start);
            let behind = self.out.split_off(at);
            let behind = behind.trim_start_matches(' ');
            let kept = self.out.trim_end_matches(' ').len();
            self.out.truncate(kept);
            if self.out.len() > line_start {
                self.out.push(' ');
            } else {
                self.out.extend(std::iter::repeat_n(' ', line_indent));
            }
            self.out.push_str(&suffix.text);
            if suffix.ends_line {
                self.out.push('\n');
                if behind.is_empty() {
                    // Only possible for the comment met last: the line
                    // after it is still to be written.
                    self.at_line_start = true;
                    self.indent = line_indent + INDENT;
                } else {
                    self.out
                        .extend(std::iter::repeat_n(' ', line_indent + INDENT));
                    self.out.push_str(behind);
                    self.at_line_start = false;
                }
            } else {
                if !behind.is_empty() {
                    self.out.push(' ');
                    self.out.push_str(behind);
                }
                self.at_line_start = false;
            }
        }
        let last_line = self.out.rfind('\n').map_or(0, |i| i + 1);
        self.col = self.out[last_line..].chars().count();
        self.after_comment = false;
    }

    /// End the current line; the next one is indented by `indent`.
    fn newline(&mut self, indent: usize) {
        for suffix in std::mem::take(&mut self.suffixes) {
            if self.at_line_start {
                self.out.extend(std::iter::repeat_n(' ', self.indent));
                self.at_line_start = false;
            } else {
                let kept = self.out.trim_end_matches(' ').len();
                self.out.truncate(kept);
                self.out.push(' ');
            }
            self.out.push_str(&suffix.text);
        }
        let kept = self.out.trim_end_matches(' ').len();
        self.out.truncate(kept);
        self.out.push('\n');
        self.at_line_start = true;
        self.indent = indent;
        self.col = 0;
        self.line_is_closed = false;
        self.after_comment = false;
    }

    fn blank_line(&mut self, indent: usize) {
        if !self.at_line_start || self.line_is_closed || !self.suffixes.is_empty() {
            self.newline(indent);
        }
        self.indent = indent;
        if !self.out.is_empty() && !self.out.ends_with("\n\n") {
            self.out.push('\n');
        }
    }

    fn own_line(&mut self, text: &str, indent: usize) {
        if !self.at_line_start || self.line_is_closed || !self.suffixes.is_empty() {
            self.newline(indent);
        }
        self.indent = indent;
        self.open_line(indent);
        self.push(text);
        self.line_is_closed = true;
    }

    fn finish(mut self) -> String {
        if !self.at_line_start || !self.suffixes.is_empty() {
            self.newline(0);
        }
        let kept = self.out.trim_end_matches('\n').len();
        self.out.truncate(kept);
        if !self.out.is_empty() {
            self.out.push('\n');
        }
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(text: &str) -> Doc {
        Doc::text(text)
    }

    /// `open item, item close`: on one line, or one item per line with a
    /// trailing comma.
    fn list(open: &str, items: Vec<Doc>, close: &str) -> Doc {
        let mut inner = vec![Doc::SoftLine];
        let count = items.len();
        for (i, item) in items.into_iter().enumerate() {
            inner.push(item);
            if i + 1 < count {
                inner.push(t(","));
                inner.push(Doc::Line);
            }
        }
        inner.push(Doc::if_break(t(","), Doc::Nil));
        Doc::group(Doc::concat(vec![
            t(open),
            Doc::nest(Doc::concat(inner)),
            Doc::SoftLine,
            t(close),
        ]))
    }

    #[test]
    fn a_group_that_fits_stays_on_one_line() {
        let doc = list("[", vec![t("a"), t("b"), t("c")], "]");
        assert_eq!(render(&doc, 9), "[a, b, c]\n");
    }

    #[test]
    fn a_group_that_does_not_fit_breaks_every_line_of_its_own() {
        let doc = list("[", vec![t("a"), t("b"), t("c")], "]");
        assert_eq!(render(&doc, 8), "[\n  a,\n  b,\n  c,\n]\n");
    }

    #[test]
    fn an_inner_group_breaks_only_when_it_has_to() {
        let inner = list("(", vec![t("x"), t("y")], ")");
        let doc = list("[", vec![t("first"), inner, t("last")], "]");
        assert_eq!(render(&doc, 12), "[\n  first,\n  (x, y),\n  last,\n]\n");
        assert_eq!(
            render(&doc, 7),
            "[\n  first,\n  (\n    x,\n    y,\n  ),\n  last,\n]\n"
        );
    }

    #[test]
    fn what_follows_a_group_on_its_line_counts() {
        // `[a, b]` fits five columns, `[a, b] +` does not.
        let doc = Doc::concat(vec![list("[", vec![t("a"), t("b")], "]"), t(" +")]);
        assert_eq!(render(&doc, 8), "[a, b] +\n");
        assert_eq!(render(&doc, 7), "[\n  a,\n  b,\n] +\n");
    }

    #[test]
    fn nesting_indents_the_lines_after_a_break() {
        let doc = Doc::group(Doc::concat(vec![
            t("first"),
            Doc::nest(Doc::concat(vec![Doc::Line, t("|> second")])),
        ]));
        assert_eq!(render(&doc, 80), "first |> second\n");
        assert_eq!(render(&doc, 10), "first\n  |> second\n");
    }

    #[test]
    fn a_hard_line_breaks_the_groups_around_it() {
        let block = Doc::concat(vec![
            t("{"),
            Doc::nest(Doc::concat(vec![Doc::HardLine, t("x")])),
            Doc::HardLine,
            t("}"),
        ]);
        let doc = list("(", vec![t("a"), block], ")");
        assert_eq!(render(&doc, 80), "(\n  a,\n  {\n    x\n  },\n)\n");
    }

    #[test]
    fn a_blank_line_is_one_empty_line_without_spaces() {
        let doc = Doc::nest(Doc::concat(vec![
            t("a"),
            Doc::HardLine,
            Doc::BlankLine,
            Doc::BlankLine,
            t("b"),
        ]));
        assert_eq!(render(&doc, 80), "a\n\n  b\n");
    }

    #[test]
    fn a_line_suffix_is_written_before_the_next_line_break() {
        // The comment was met before the comma and lands behind it.
        let doc = Doc::concat(vec![
            t("a"),
            Doc::LineSuffix("-- c".into(), true),
            t(","),
            Doc::HardLine,
            t("b"),
        ]);
        assert_eq!(render(&doc, 80), "a, -- c\nb\n");
    }

    #[test]
    fn a_group_with_a_waiting_line_suffix_breaks() {
        let doc = list(
            "f(",
            vec![
                Doc::concat(vec![t("a"), Doc::LineSuffix("-- c".into(), true)]),
                t("b"),
            ],
            ")",
        );
        assert_eq!(render(&doc, 80), "f(\n  a, -- c\n  b,\n)\n");
    }

    #[test]
    fn the_group_that_breaks_is_the_one_with_the_next_break_point() {
        // The inner group has no line after the comment; the outer one
        // has, so the outer one breaks and the inner one stays whole.
        let inner = Doc::group(Doc::concat(vec![
            t("g("),
            Doc::SoftLine,
            t("x)"),
            Doc::LineSuffix("-- c".into(), true),
        ]));
        let doc = list("f(", vec![inner, t("b")], ")");
        assert_eq!(render(&doc, 80), "f(\n  g(x), -- c\n  b,\n)\n");
    }

    #[test]
    fn a_line_suffix_does_not_count_for_the_width() {
        let doc = Doc::concat(vec![
            list("[", vec![t("a"), t("b")], "]"),
            Doc::LineSuffix("-- a long comment".into(), true),
        ]);
        assert_eq!(render(&doc, 6), "[a, b] -- a long comment\n");
    }

    #[test]
    fn a_second_line_suffix_breaks_the_line_where_the_first_was_met() {
        let doc = Doc::concat(vec![
            t("let x ="),
            Doc::LineSuffix("-- one".into(), true),
            t(" 5"),
            Doc::LineSuffix("-- two".into(), true),
        ]);
        assert_eq!(render(&doc, 80), "let x = -- one\n  5 -- two\n");
    }

    #[test]
    fn a_block_comment_and_a_line_comment_share_the_end_of_a_line() {
        let doc = Doc::concat(vec![
            t("a"),
            Doc::LineSuffix("{- one -}".into(), false),
            t(" + b"),
            Doc::LineSuffix("-- two".into(), true),
        ]);
        assert_eq!(render(&doc, 80), "a + b {- one -} -- two\n");
    }

    #[test]
    fn an_inline_comment_keeps_its_place_behind_a_waiting_one() {
        let doc = Doc::concat(vec![
            t("x ="),
            Doc::LineSuffix("-- one".into(), true),
            t(" "),
            Doc::Comment("{- two -}".into()),
            t("5"),
        ]);
        assert_eq!(render(&doc, 80), "x = -- one\n  {- two -} 5\n");
    }

    #[test]
    fn an_own_line_comment_is_alone_on_its_line() {
        // At the start of a line it is written there; in the middle of
        // one it starts a new line. What follows starts a new line too.
        let doc = Doc::nest(Doc::concat(vec![
            t("x = "),
            Doc::OwnLine("-- c".into()),
            t("5"),
            Doc::HardLine,
            Doc::OwnLine("-- d".into()),
            t("y"),
        ]));
        assert_eq!(render(&doc, 80), "x =\n  -- c\n  5\n  -- d\n  y\n");
    }

    #[test]
    fn an_own_line_comment_breaks_the_groups_around_it() {
        let doc = list(
            "[",
            vec![
                t("a"),
                Doc::concat(vec![Doc::OwnLine("-- c".into()), t("b")]),
            ],
            "]",
        );
        assert_eq!(render(&doc, 80), "[\n  a,\n  -- c\n  b,\n]\n");
    }

    #[test]
    fn a_waiting_comment_is_written_before_an_own_line_comment() {
        let doc = Doc::concat(vec![
            t("a"),
            Doc::LineSuffix("-- one".into(), true),
            Doc::OwnLine("-- two".into()),
            Doc::BlankLine,
            t("b"),
        ]);
        assert_eq!(render(&doc, 80), "a -- one\n-- two\n\nb\n");
    }

    #[test]
    fn an_inline_comment_is_set_apart_by_spaces() {
        let c = || Doc::Comment("{- c -}".into());
        let doc = Doc::concat(vec![
            t("f("),
            c(),
            t("a"),
            c(),
            t(","),
            t(" b"),
            c(),
            t(")"),
        ]);
        assert_eq!(render(&doc, 80), "f({- c -} a {- c -}, b {- c -})\n");
        let doc = Doc::concat(vec![c(), t("a"), c(), t("+ b")]);
        assert_eq!(render(&doc, 80), "{- c -} a {- c -} + b\n");
    }

    #[test]
    fn an_inline_comment_counts_for_the_width() {
        let doc = list(
            "[",
            vec![Doc::concat(vec![Doc::Comment("{- c -}".into()), t("a")])],
            "]",
        );
        assert_eq!(render(&doc, 11), "[{- c -} a]\n");
        assert_eq!(render(&doc, 10), "[\n  {- c -} a,\n]\n");
    }

    #[test]
    fn text_with_a_line_break_is_measured_to_its_first_line() {
        let string = t("\"\"\"\n    raw\n  \"\"\"");
        let doc = list("f(", vec![string], ")");
        assert_eq!(render(&doc, 10), "f(\"\"\"\n    raw\n  \"\"\")\n");
        // The column after the text is that of its last line.
        let doc = Doc::concat(vec![
            t("\"\"\"\n\"\"\""),
            list("(", vec![t("aaaa"), t("bbbb")], ")"),
        ]);
        assert_eq!(render(&doc, 12), "\"\"\"\n\"\"\"(\n  aaaa,\n  bbbb,\n)\n");
    }

    #[test]
    fn if_break_follows_its_group() {
        let doc = |width| {
            render(
                &Doc::group(Doc::concat(vec![
                    t("a"),
                    Doc::Line,
                    Doc::if_break(t("broken"), t("flat")),
                ])),
                width,
            )
        };
        assert_eq!(doc(80), "a flat\n");
        assert_eq!(doc(3), "a\nbroken\n");
    }

    #[test]
    fn no_line_ends_in_a_space_and_the_result_ends_in_one_line_break() {
        let doc = Doc::concat(vec![
            t("a = "),
            Doc::HardLine,
            t("b "),
            Doc::LineSuffix("-- c".into(), true),
            Doc::HardLine,
            Doc::BlankLine,
        ]);
        assert_eq!(render(&doc, 80), "a =\nb -- c\n");
        assert_eq!(render(&Doc::Nil, 80), "");
    }

    #[test]
    fn a_deep_document_does_not_overflow_the_stack_of_the_renderer() {
        let mut doc = t("x");
        for _ in 0..20_000 {
            doc = Doc::group(Doc::nest(Doc::concat(vec![t("("), doc, t(")")])));
        }
        let out = render(&doc, 100);
        assert_eq!(out.len(), 40_002);
        // Dropping a deep tree recurses; take it apart by hand.
        let mut stack = vec![doc];
        while let Some(doc) = stack.pop() {
            match doc {
                Doc::Group(inner, _) | Doc::Nest(inner) => stack.push(*inner),
                Doc::Concat(parts) => stack.extend(parts),
                _ => {}
            }
        }
    }
}
