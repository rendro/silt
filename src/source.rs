//! Source files and positions in them.
//!
//! A [`Span`] is a byte range in one file. A line and a column exist only
//! through the [`SourceMap`], which holds the text of every file a
//! compilation reads: the lexer, the parser and everything after them
//! carry byte offsets, and whoever prints a position asks the map for it.

use std::path::PathBuf;
use std::sync::Arc;

/// The identity of a file in a [`SourceMap`]. The first file added to a
/// map gets `FileId::default()`, so a tool that works on one text alone
/// can lex it without building a map first.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub struct FileId(u32);

impl FileId {
    /// The declarations silt makes itself rather than reads from a file:
    /// its builtin types, their derived impls and the methods they get.
    /// No `SourceMap` holds text for it, so no position in it is ever
    /// printed.
    pub const BUILTIN: FileId = FileId(u32::MAX);
}

/// A byte range `start..end` (end exclusive) in the file `file`.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct Span {
    pub file: FileId,
    pub start: u32,
    pub end: u32,
}

impl Span {
    /// The span of what silt declares itself (see [`FileId::BUILTIN`]).
    pub const BUILTIN: Span = Span {
        file: FileId::BUILTIN,
        start: 0,
        end: 0,
    };

    /// Whether this span is in a file that has text: everything but
    /// [`Span::BUILTIN`].
    pub fn is_in_source(self) -> bool {
        self.file != FileId::BUILTIN
    }

    /// The empty span at byte `at` of `file`.
    pub fn point(file: FileId, at: u32) -> Self {
        Span {
            file,
            start: at,
            end: at,
        }
    }

    /// The span from the start of `self` to the end of `last`: the extent
    /// of a construct whose first part is `self` and whose last part is
    /// `last`. Never shorter than `self`.
    pub fn to(self, last: Span) -> Self {
        Span {
            file: self.file,
            start: self.start,
            end: last.end.max(self.end),
        }
    }

    /// The start as a `usize` byte offset, for slicing the file's text.
    pub fn start_offset(self) -> usize {
        self.start as usize
    }

    /// The end as a `usize` byte offset, for slicing the file's text.
    pub fn end_offset(self) -> usize {
        self.end as usize
    }
}

/// Where the text of a [`SourceFile`] came from.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SourceName {
    /// A file on disk, named as it is shown to the user.
    Path(PathBuf),
    /// The `n`th entry typed into the REPL.
    Repl(usize),
    /// An editor's unsaved text of the file at this path.
    Overlay(PathBuf),
    /// A package manifest (`silt.toml`).
    Manifest(PathBuf),
    /// Text that comes with silt itself.
    Builtin,
}

/// The text of one file and the start of each of its lines.
pub struct SourceFile {
    pub path: SourceName,
    pub text: Arc<str>,
    /// Byte offset of the first character of each line. Lines end at
    /// `\n`, as in the lexer; a `\r` before it belongs to the line.
    line_starts: Vec<u32>,
}

impl SourceFile {
    pub fn new(path: SourceName, text: Arc<str>) -> Self {
        let mut line_starts = vec![0];
        line_starts.extend(
            text.bytes()
                .enumerate()
                .filter(|&(_, b)| b == b'\n')
                .map(|(i, _)| i as u32 + 1),
        );
        SourceFile {
            path,
            text,
            line_starts,
        }
    }

    /// `at` moved back onto the text: no further than its end, and onto
    /// the start of the character it points into.
    fn clamp(&self, at: u32) -> usize {
        let mut at = (at as usize).min(self.text.len());
        while !self.text.is_char_boundary(at) {
            at -= 1;
        }
        at
    }

    /// Index (0-based) of the line holding byte `at`.
    fn line_index(&self, at: usize) -> usize {
        self.line_starts
            .partition_point(|&start| start as usize <= at)
            .saturating_sub(1)
    }

    /// The number of lines: one more than the number of `\n`.
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// The 1-based line and the 1-based column, counted in characters, of
    /// byte `at`. An offset past the end counts as the end; an offset
    /// inside a character counts as that character.
    pub fn line_col(&self, at: u32) -> (u32, u32) {
        let at = self.clamp(at);
        let line = self.line_index(at);
        let start = self.line_starts[line] as usize;
        let col = self.text[start..at].chars().count();
        (line as u32 + 1, col as u32 + 1)
    }

    /// The text of 1-based line `line`, without its line break. `None`
    /// for a line the file does not have.
    pub fn line_text(&self, line: u32) -> Option<&str> {
        let index = (line as usize).checked_sub(1)?;
        let start = *self.line_starts.get(index)? as usize;
        let end = self
            .line_starts
            .get(index + 1)
            .map_or(self.text.len(), |&next| next as usize - 1);
        Some(&self.text[start..end])
    }

    /// Byte offset of the start of 1-based line `line`, or the end of the
    /// text for a line past the last one.
    pub fn line_start(&self, line: u32) -> u32 {
        let index = (line as usize).saturating_sub(1);
        self.line_starts
            .get(index)
            .copied()
            .unwrap_or(self.text.len() as u32)
    }

    /// The LSP position of byte `at`: a 0-based line and a 0-based column
    /// counted in UTF-16 code units, as the protocol counts them.
    #[cfg(feature = "lsp")]
    pub fn lsp_position(&self, at: u32) -> lsp_types::Position {
        let at = self.clamp(at);
        let line = self.line_index(at);
        let start = self.line_starts[line] as usize;
        let character: usize = self.text[start..at].chars().map(char::len_utf16).sum();
        lsp_types::Position::new(line as u32, character as u32)
    }

    /// The byte offset of an LSP position. A line past the last one means
    /// the end of the text; a column past the end of its line (a `\r`
    /// before the line break is not part of the line) means the end of
    /// the line; a column inside a character means that character.
    #[cfg(feature = "lsp")]
    pub fn offset_of_lsp_position(&self, pos: lsp_types::Position) -> u32 {
        let Some(line) = self.line_text(pos.line + 1) else {
            return self.text.len() as u32;
        };
        let line = line.strip_suffix('\r').unwrap_or(line);
        let start = self.line_starts[pos.line as usize] as usize;
        let mut units = 0u32;
        for (i, ch) in line.char_indices() {
            if units >= pos.character {
                return (start + i) as u32;
            }
            units += ch.len_utf16() as u32;
            if units > pos.character {
                return (start + i) as u32;
            }
        }
        (start + line.len()) as u32
    }
}

/// The lines and columns a span covers, with the text of its first line:
/// what a diagnostic prints.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Snippet {
    /// 1-based line and character column of the start.
    pub line: u32,
    pub col: u32,
    /// 1-based line and character column of the end.
    pub end_line: u32,
    pub end_col: u32,
    /// The text of the start's line, without its line break.
    pub line_text: String,
}

/// Every file of a compilation, by [`FileId`].
#[derive(Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
}

impl SourceMap {
    pub fn new() -> Self {
        SourceMap::default()
    }

    /// The number of files in the map.
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// Add a file and return its id.
    pub fn add(&mut self, path: SourceName, text: Arc<str>) -> FileId {
        let id = FileId(self.files.len() as u32);
        self.files.push(SourceFile::new(path, text));
        id
    }

    /// The file `id`. Panics for an id from another map.
    pub fn file(&self, id: FileId) -> &SourceFile {
        &self.files[id.0 as usize]
    }

    /// The file `id`, when this map has it.
    pub fn get(&self, id: FileId) -> Option<&SourceFile> {
        self.files.get(id.0 as usize)
    }

    /// The 1-based line and character column of byte `at.1` of file `at.0`.
    pub fn line_col(&self, at: (FileId, u32)) -> (u32, u32) {
        self.file(at.0).line_col(at.1)
    }

    /// The LSP position of byte `at.1` of file `at.0`.
    #[cfg(feature = "lsp")]
    pub fn lsp_position(&self, at: (FileId, u32)) -> lsp_types::Position {
        self.file(at.0).lsp_position(at.1)
    }

    /// Where `span` starts and ends, and the text of its first line.
    pub fn snippet(&self, span: Span) -> Snippet {
        let file = self.file(span.file);
        let (line, col) = file.line_col(span.start);
        let (end_line, end_col) = file.line_col(span.end);
        Snippet {
            line,
            col,
            end_line,
            end_col,
            line_text: file.line_text(line).unwrap_or("").to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(text: &str) -> SourceFile {
        SourceFile::new(SourceName::Builtin, text.into())
    }

    #[test]
    fn line_col_counts_characters_from_one() {
        let f = file("ab\nλx = 1\n");
        assert_eq!(f.line_col(0), (1, 1));
        assert_eq!(f.line_col(2), (1, 3));
        assert_eq!(f.line_col(3), (2, 1));
        // `λ` is two bytes: the byte after it is column 2.
        assert_eq!(f.line_col(5), (2, 2));
        // Inside `λ`: the character itself.
        assert_eq!(f.line_col(4), (2, 1));
        // The end of a text that ends with a line break is a line of its own.
        assert_eq!(f.line_col(11), (3, 1));
        assert_eq!(f.line_col(999), (3, 1));
    }

    #[test]
    fn line_text_drops_the_line_break() {
        let f = file("one\r\ntwo\n");
        assert_eq!(f.line_text(1), Some("one\r"));
        assert_eq!(f.line_text(2), Some("two"));
        assert_eq!(f.line_text(3), Some(""));
        assert_eq!(f.line_text(4), None);
        assert_eq!(f.line_text(0), None);
        assert_eq!(f.line_count(), 3);
    }

    #[cfg(feature = "lsp")]
    #[test]
    fn lsp_positions_count_utf16_units_both_ways() {
        let f = file("a😀b\nc");
        // `😀` is four bytes and two UTF-16 units.
        assert_eq!(f.lsp_position(5), lsp_types::Position::new(0, 3));
        assert_eq!(f.offset_of_lsp_position(lsp_types::Position::new(0, 3)), 5);
        // A column inside the surrogate pair means the character.
        assert_eq!(f.offset_of_lsp_position(lsp_types::Position::new(0, 2)), 1);
        assert_eq!(f.offset_of_lsp_position(lsp_types::Position::new(0, 99)), 6);
        assert_eq!(f.offset_of_lsp_position(lsp_types::Position::new(1, 0)), 7);
        assert_eq!(f.offset_of_lsp_position(lsp_types::Position::new(9, 0)), 8);
    }

    #[test]
    fn spans_are_twelve_bytes() {
        assert_eq!(std::mem::size_of::<Span>(), 12);
    }

    #[test]
    fn the_map_numbers_files_in_order() {
        let mut map = SourceMap::new();
        let a = map.add(SourceName::Builtin, "x".into());
        let b = map.add(SourceName::Repl(1), "let y = 2".into());
        assert_eq!(a, FileId::default());
        assert_ne!(a, b);
        let snippet = map.snippet(Span {
            file: b,
            start: 4,
            end: 5,
        });
        assert_eq!((snippet.line, snippet.col, snippet.end_col), (1, 5, 6));
        assert_eq!(snippet.line_text, "let y = 2");
    }
}
