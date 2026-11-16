//! Source files and positions in them.
//!
//! A [`Span`] is a byte range in one file. A line and a column exist only
//! through the [`SourceMap`], which holds the text of every file a
//! compilation reads: the lexer, the parser and everything after them
//! carry byte offsets, and whoever prints a position asks the map for it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The identity of a file in a [`SourceMap`]. The first file added to a
/// map gets `FileId::default()`, so a tool that works on one text alone
/// can lex it without building a map first.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub struct FileId(u32);

impl FileId {
    /// The declarations silt makes itself rather than reads from a file:
    /// its builtin types and traits.
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
    #[cfg_attr(not(feature = "repl"), allow(dead_code))]
    Repl(usize),
    /// An editor's unsaved text of the file at this path.
    Overlay(PathBuf),
    /// A package manifest (`silt.toml`).
    Manifest(PathBuf),
    /// The signatures of a host module an embedder declared, by the
    /// module's name.
    Host(String),
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
    /// The same as the language server protocol counts lines, where it
    /// differs: a lone `\r` ends a line too. None for a text without
    /// one, whose lines are `line_starts` (see `lsp_lines`).
    #[cfg(feature = "lsp")]
    lsp_line_starts: Option<Vec<u32>>,
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
        #[cfg(feature = "lsp")]
        let lsp_line_starts = lsp_line_starts(&text);
        SourceFile {
            path,
            text,
            line_starts,
            #[cfg(feature = "lsp")]
            lsp_line_starts,
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
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// The number of lines that have text or stand in front of a line
    /// break, as `str::lines` counts them: a line break at the end of
    /// the text starts no line. From the line table, not by reading the
    /// text.
    pub fn text_lines(&self) -> usize {
        self.line_starts.len() - usize::from(self.text.is_empty() || self.text.ends_with('\n'))
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

    /// The start of each line as the language server protocol counts
    /// lines: a line ends at `\n`, `\r\n` or a lone `\r`. Every position
    /// the server hands out or reads goes by these lines.
    #[cfg(feature = "lsp")]
    fn lsp_lines(&self) -> &[u32] {
        self.lsp_line_starts.as_deref().unwrap_or(&self.line_starts)
    }

    /// The protocol's line `line` (0-based, see `lsp_lines`): the byte
    /// offset of its start and its text without its line end. None for a
    /// line the text does not have.
    #[cfg(feature = "lsp")]
    pub(crate) fn lsp_line(&self, line: u32) -> Option<(usize, &str)> {
        let lines = self.lsp_lines();
        let start = *lines.get(line as usize)? as usize;
        let end = lines
            .get(line as usize + 1)
            .map_or(self.text.len(), |&n| n as usize);
        let text = &self.text[start..end];
        let text = (text
            .strip_suffix('\n')
            .map(|t| t.strip_suffix('\r').unwrap_or(t)))
        .or_else(|| text.strip_suffix('\r'))
        .unwrap_or(text);
        Some((start, text))
    }

    /// The number of the protocol's lines that have text or stand in
    /// front of a line end (as `text_lines`, by `lsp_lines`).
    #[cfg(feature = "lsp")]
    pub(crate) fn lsp_text_lines(&self) -> usize {
        let ends_a_line = self.text.ends_with(['\n', '\r']);
        self.lsp_lines().len() - usize::from(self.text.is_empty() || ends_a_line)
    }

    /// The LSP position of byte `at`: a 0-based line (see `lsp_lines`) and
    /// a 0-based column counted in UTF-16 code units, as the protocol
    /// counts them. The end of the text is the end of the document.
    #[cfg(feature = "lsp")]
    pub fn lsp_position(&self, at: u32) -> lsp_types::Position {
        let at = self.clamp(at);
        let lines = self.lsp_lines();
        let line = lines
            .partition_point(|&start| start as usize <= at)
            .saturating_sub(1);
        let start = lines[line] as usize;
        let character: usize = self.text[start..at].chars().map(char::len_utf16).sum();
        lsp_types::Position::new(line as u32, character as u32)
    }

    /// The byte offset of an LSP position (lines as `lsp_lines` counts
    /// them). A line past the last one means the end of the text; a
    /// column past the end of its line (its line end is not part of it)
    /// means the end of the line; a column inside a character means that
    /// character.
    #[cfg(feature = "lsp")]
    pub fn offset_of_lsp_position(&self, pos: lsp_types::Position) -> u32 {
        let Some((start, line)) = self.lsp_line(pos.line) else {
            return self.text.len() as u32;
        };
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
#[cfg(any(test, feature = "test-hooks"))]
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

/// Where the lines of `text` start as the language server protocol
/// counts them (a line ends at `\n`, `\r\n` or a lone `\r`), if that is
/// not where they start for the lexer: only a text with a lone `\r`.
#[cfg(feature = "lsp")]
fn lsp_line_starts(text: &str) -> Option<Vec<u32>> {
    let bytes = text.as_bytes();
    let lone_cr = |i: usize| bytes[i] == b'\r' && bytes.get(i + 1) != Some(&b'\n');
    if !(0..bytes.len()).any(lone_cr) {
        return None;
    }
    let mut starts = vec![0];
    starts.extend(
        (0..bytes.len())
            .filter(|&i| bytes[i] == b'\n' || lone_cr(i))
            .map(|i| i as u32 + 1),
    );
    Some(starts)
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
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// Every file of the map, in the order they were added.
    pub fn files(&self) -> impl Iterator<Item = &SourceFile> {
        self.files.iter()
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
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn lsp_position(&self, at: (FileId, u32)) -> lsp_types::Position {
        self.file(at.0).lsp_position(at.1)
    }

    /// Where `span` starts and ends, and the text of its first line.
    #[cfg(any(test, feature = "test-hooks"))]
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

/// `path` made canonical, written as a user writes it: without the
/// `\\?\` prefix Windows gives a canonical path (`\\?\UNC\server\share`
/// is `\\server\share`). Files, URIs and keys made from it are the ones
/// an editor and a user name. A path that cannot be made canonical (it
/// does not exist) is returned as it is.
pub fn canonical_path(path: &Path) -> PathBuf {
    match std::fs::canonicalize(path) {
        Ok(canonical) => without_verbatim_prefix(&canonical),
        Err(_) => path.to_path_buf(),
    }
}

/// [`canonical_path`], also for a path that does not exist: its nearest
/// existing ancestor made canonical, with the rest of the path appended
/// as given. Two spellings of one place (on Windows a short `RUNNER~1`
/// name and its long form, a verbatim prefix or none) come out the same.
/// A path or a file name as every line silt prints writes it (a status
/// line, `error reading ...`, the place line ` --> ` of a diagnostic): by
/// the display rule of untrusted text (`git::escape_for_display`), so a
/// control character, an unusual space or an invisible or direction
/// character is shown as an escape (`a\u{1b}cb.silt`). A name from the
/// file system or a manifest cannot drive the terminal or forge a line.
/// A JSON document keeps the real string.
pub(crate) fn shown_path(path: impl AsRef<Path>) -> String {
    crate::git::escape_for_display(&path.as_ref().to_string_lossy())
}

/// Whether `word`, a name or a path, can stand in a command line silt
/// suggests (`mkdir NAME && cd NAME`, `silt test PATH`) and be pasted as
/// it stands: only ASCII letters, digits, `_`, `-`, `.` and the path
/// separator `/` (on Windows `\\` and `:` too), not empty, and not
/// starting with `-` (it would read as a flag). A shell does nothing
/// else with such a word. A suggestion with any other word is said in
/// words, not as a command: one with a space, a `;` or a control
/// character would do something else, and its escaped form would name
/// another file.
pub(crate) fn pasteable(word: &str) -> bool {
    let allowed = |c: char| {
        c.is_ascii_alphanumeric()
            || matches!(c, '_' | '-' | '.' | '/')
            || (cfg!(windows) && matches!(c, '\\' | ':'))
    };
    !word.is_empty() && !word.starts_with('-') && word.chars().all(allowed)
}

pub(crate) fn canonical_path_lenient(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(path),
            Err(_) => return path.to_path_buf(),
        }
    };
    let mut rest = Vec::new();
    let mut current = absolute.as_path();
    loop {
        if let Ok(canonical) = std::fs::canonicalize(current) {
            let mut out = without_verbatim_prefix(&canonical);
            for part in rest.iter().rev() {
                out.push(part);
            }
            return out;
        }
        match (current.file_name(), current.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name.to_os_string());
                current = parent;
            }
            _ => return absolute,
        }
    }
}

/// `path` without the `\\?\` (verbatim) prefix of a Windows path, which
/// no user writes: `\\?\C:\x` is `C:\x`, `\\?\UNC\server\share` is
/// `\\server\share`. Any other path is returned as it is.
pub(crate) fn without_verbatim_prefix(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{unc}"))
    } else if let Some(disk) = text.strip_prefix(r"\\?\") {
        PathBuf::from(disk)
    } else {
        path.to_path_buf()
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

    #[cfg(feature = "lsp")]
    #[test]
    fn lsp_lines_end_at_a_line_feed_a_crlf_and_a_lone_carriage_return() {
        use lsp_types::Position;
        // Lines: "a", "b", "c", "", "d" (a lone CR, a CRLF, a LF, a lone
        // CR, then the end).
        let f = file("a\rb\r\nc\n\rd");
        let lines: Vec<_> = (0..6).map(|n| f.lsp_line(n)).collect();
        assert_eq!(
            lines,
            [
                Some((0, "a")),
                Some((2, "b")),
                Some((5, "c")),
                Some((7, "")),
                Some((8, "d")),
                None
            ]
        );
        assert_eq!(f.lsp_text_lines(), 5);
        assert_eq!(f.lsp_position(2), Position::new(1, 0));
        assert_eq!(f.lsp_position(4), Position::new(1, 2));
        assert_eq!(f.lsp_position(9), Position::new(4, 1));
        assert_eq!(f.offset_of_lsp_position(Position::new(1, 9)), 3);
        assert_eq!(f.offset_of_lsp_position(Position::new(4, 0)), 8);
        assert_eq!(f.offset_of_lsp_position(Position::new(5, 0)), 9);
        // The way back gives every offset again, but the line feed of a
        // CRLF, which is inside a line end: it gives the end of the line.
        for at in (0..=9u32).filter(|&at| at != 4) {
            let back = f.offset_of_lsp_position(f.lsp_position(at));
            assert_eq!(back, at, "{at}");
        }
        // A text without a lone CR keeps the lexer's lines.
        let g = file("x\r\ny\n");
        assert_eq!(g.lsp_line(1), Some((3, "y")));
        assert_eq!(g.lsp_text_lines(), 2);
        assert_eq!(g.lsp_position(5), Position::new(2, 0));
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
