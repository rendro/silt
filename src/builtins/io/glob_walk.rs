//! The walk `fs.glob` matches its pattern by: the glob crate's
//! (`glob::glob`, whose order and rules it keeps), except that an entry
//! found by listing a directory is a directory only when it is one
//! itself, not a symlink to one. So neither `**` nor `*` goes into a
//! linked directory, as `fs.walk` does not, and a link to `.` makes no
//! loop; a link is matched by its own name. A part of the pattern with
//! no wildcard (`link/x.txt`) names a path, which the system resolves
//! through a link as it does any path. (The crate follows every link
//! and has no option not to.)

use std::fs;
use std::path::{self, Component, Path, PathBuf};

use glob::{MatchOptions, Pattern, PatternError};

/// A path met on the walk, and whether to go into it.
struct Found {
    path: PathBuf,
    is_dir: bool,
}

impl Found {
    /// A path the pattern names (its root, or one without wildcards):
    /// the system follows a link in it.
    fn named(path: PathBuf) -> Found {
        let is_dir = fs::metadata(&path).is_ok_and(|md| md.is_dir());
        Found { path, is_dir }
    }

    /// An entry of a directory's listing: a symlink is no directory.
    fn listed(path: PathBuf, entry: &fs::DirEntry) -> Found {
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        Found { path, is_dir }
    }
}

/// One part of the pattern between separators.
struct Part {
    pattern: Pattern,
    /// `**`.
    recursive: bool,
    /// It has a wildcard (`*`, `?`, `[`), so the directory is listed.
    wild: bool,
}

/// The `index` of the part a found path is still to be matched by, or
/// [`MATCHED`] when it matched the last.
const MATCHED: usize = usize::MAX;

/// What is still to be looked at, the next one last.
type Todo = Vec<Result<(Found, usize), std::io::Error>>;

/// The paths `pattern` matches, in the glob crate's order, or why a
/// directory could not be listed.
pub(super) struct Matches {
    parts: Vec<Part>,
    /// The pattern ends in a separator: only directories match.
    require_dir: bool,
    todo: Todo,
    /// Where the walk begins, until it has.
    scope: Option<Found>,
}

/// The options `glob::glob` matches with.
const OPTIONS: MatchOptions = MatchOptions {
    case_sensitive: true,
    require_literal_separator: false,
    require_literal_leading_dot: false,
};

/// The walk for `pattern`: an error if it is no pattern.
pub(super) fn matches(pattern: &str) -> Result<Matches, PatternError> {
    Pattern::new(pattern)?;
    let mut components = Path::new(pattern).components().peekable();
    while let Some(Component::Prefix(..) | Component::RootDir) = components.peek() {
        components.next();
    }
    let rest = components.map(|c| c.as_os_str()).collect::<PathBuf>();
    let normalized = Path::new(pattern).iter().collect::<PathBuf>();
    let root_len = normalized.as_os_str().len() - rest.as_os_str().len();
    let root = (root_len > 0).then(|| Path::new(&pattern[..root_len]));
    if root.is_some_and(unsupported_verbatim) {
        return Ok(Matches {
            parts: Vec::new(),
            require_dir: false,
            todo: Vec::new(),
            scope: None,
        });
    }
    let scope = Found::named(root.map_or_else(|| PathBuf::from("."), Path::to_path_buf));
    let mut parts = Vec::new();
    for text in pattern[root_len.min(pattern.len())..].split_terminator(path::is_separator) {
        parts.push(Part {
            pattern: Pattern::new(text)?,
            recursive: text == "**",
            wild: text.contains(['*', '?', '[']),
        });
    }
    if root_len == pattern.len() {
        parts.push(Part {
            pattern: Pattern::new("")?,
            recursive: false,
            wild: false,
        });
    }
    Ok(Matches {
        parts,
        require_dir: pattern.ends_with(path::is_separator),
        todo: Vec::new(),
        scope: Some(scope),
    })
}

/// A verbatim root other than a disk (`\\?\UNC\...`), which the crate
/// matches nothing under.
#[cfg(windows)]
fn unsupported_verbatim(root: &Path) -> bool {
    match root.components().next() {
        Some(Component::Prefix(prefix)) => {
            prefix.kind().is_verbatim()
                && !matches!(prefix.kind(), std::path::Prefix::VerbatimDisk(_))
        }
        _ => false,
    }
}

#[cfg(not(windows))]
fn unsupported_verbatim(_: &Path) -> bool {
    false
}

impl Iterator for Matches {
    type Item = Result<PathBuf, std::io::Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(scope) = self.scope.take()
            && !self.parts.is_empty()
        {
            fill_todo(&mut self.todo, &self.parts, 0, &scope);
        }
        loop {
            let (found, mut index) = match self.todo.pop()? {
                Ok(next) => next,
                Err(e) => return Some(Err(e)),
            };
            if index == MATCHED {
                if self.require_dir && !found.is_dir {
                    continue;
                }
                return Some(Ok(found.path));
            }
            if self.parts[index].recursive {
                let mut next = index;
                while next + 1 < self.parts.len() && self.parts[next + 1].recursive {
                    next += 1;
                }
                let last = next == self.parts.len() - 1;
                if found.is_dir {
                    // `**` matches the directory itself and what is in it.
                    fill_todo(&mut self.todo, &self.parts, next, &found);
                    if last {
                        return Some(Ok(found.path));
                    }
                    index = next + 1;
                } else if last {
                    continue;
                } else {
                    index = next + 1;
                }
            }
            // (A name that is no UTF-8 is not matched, as by the crate.)
            let Some(name) = found.path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if self.parts[index].pattern.matches_with(name, OPTIONS) {
                if index == self.parts.len() - 1 {
                    if !self.require_dir || found.is_dir {
                        return Some(Ok(found.path));
                    }
                } else {
                    fill_todo(&mut self.todo, &self.parts, index + 1, &found);
                }
            }
        }
    }
}

/// Puts what is under `found` that `parts[index]` is to match onto
/// `todo`: the one path a part without wildcards names (`.` and `..`
/// among them), or the listing of a directory, sorted.
fn fill_todo(todo: &mut Todo, parts: &[Part], index: usize, found: &Found) {
    let add = |todo: &mut Todo, next: Found| match index + 1 == parts.len() {
        // (It matched; `.` and `..` would not match again by name.)
        true => todo.push(Ok((next, MATCHED))),
        false => fill_todo(todo, parts, index + 1, &next),
    };
    let part = &parts[index];
    let here = found.path == Path::new(".");
    match (part.wild, found.is_dir) {
        (false, _) => {
            let name = part.pattern.as_str();
            let special = name == "." || name == "..";
            let next = Found::named(match here {
                true => PathBuf::from(name),
                false => found.path.join(name),
            });
            let exists =
                || fs::metadata(&next.path).is_ok() || fs::symlink_metadata(&next.path).is_ok();
            if (special && found.is_dir) || (!special && exists()) {
                add(todo, next);
            }
        }
        (true, true) => {
            let listing = fs::read_dir(&found.path).and_then(|entries| {
                entries
                    .map(|entry| {
                        entry.map(|entry| {
                            let path = match here {
                                true => PathBuf::from(entry.file_name()),
                                false => entry.path(),
                            };
                            Found::listed(path, &entry)
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
            });
            match listing {
                Ok(mut children) => {
                    children.sort_by(|a, b| b.path.file_name().cmp(&a.path.file_name()));
                    todo.extend(children.into_iter().map(|child| Ok((child, index))));
                    // `.` and `..` are matched only by a part that begins
                    // with a dot.
                    if part.pattern.as_str().starts_with('.') {
                        for special in [".", ".."] {
                            if part.pattern.matches_with(special, OPTIONS) {
                                add(todo, Found::named(found.path.join(special)));
                            }
                        }
                    }
                }
                Err(e) => todo.push(Err(e)),
            }
        }
        (true, false) => {}
    }
}
