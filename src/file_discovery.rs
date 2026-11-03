//! Filesystem-walk policy shared between callers that recursively scan
//! a workspace for `.silt` source files, and the walk itself
//! ([`files_under`]). Right now there are two callers:
//!
//! * The LSP preloader (`src/lsp/preload.rs`) — feeds every found file
//!   through `update_document` on initialize so cross-file features
//!   work for unopened files.
//! * The CLI's `silt fmt`/`silt test` file discovery
//!   (`src/cli/paths.rs::find_silt_files`) — drives `silt fmt <dir>`
//!   and `silt test <dir>`.
//!
//! Both callers MUST skip the same set of directories. The CLI walker
//! once descended into `target/`,
//! `.git/`, `node_modules/`, and `fuzz/corpus/` while the LSP correctly
//! ignored them — so `silt fmt` would rewrite vendored or generated
//! `.silt` files an unsuspecting user never meant to touch, and
//! `silt test` would try to execute them. Consolidating the policy
//! here means a future addition to the skip list (e.g. `vendor/`)
//! lands in one place and applies everywhere consistently.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

/// The files under `root` that `wanted` takes, in the directories
/// `enter` lets the walk into, in the order of names (see
/// [`walk_files`]).
pub fn files_under(
    root: &Path,
    enter: &dyn Fn(&Path) -> bool,
    wanted: &dyn Fn(&Path) -> bool,
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_files(root, enter, wanted, &mut |found| {
        if let Found::File(path) = found {
            out.push(path);
        }
        true
    });
    out
}

/// What [`walk_files`] finds.
pub enum Found {
    /// A file `wanted` takes.
    File(PathBuf),
    /// A directory that could not be read: it is skipped.
    Unreadable(PathBuf, std::io::Error),
}

/// Hand `found` each file under `root` that `wanted` takes, in the
/// directories `enter` lets the walk into, in the order of names, as
/// the walk reaches it; the walk stops when `found` says `false`.
/// Links are followed, and a directory is entered once, by the first
/// path that reaches it: a link back to a directory the walk has
/// entered (a loop) or a second way into one is not followed, so each
/// file is named at its own depth. A file reached by a second path (a
/// link to it) is named once, by the first. Only a link costs a look
/// at where it leads: the place of anything else is that of its
/// directory and its name.
pub fn walk_files(
    root: &Path,
    enter: &dyn Fn(&Path) -> bool,
    wanted: &dyn Fn(&Path) -> bool,
    found: &mut dyn FnMut(Found) -> bool,
) {
    struct Walk<'a> {
        enter: &'a dyn Fn(&Path) -> bool,
        wanted: &'a dyn Fn(&Path) -> bool,
        found: &'a mut dyn FnMut(Found) -> bool,
        entered: HashSet<PathBuf>,
        named: HashSet<PathBuf>,
    }
    /// Walk `dir`, whose place without links is `place`; `false` once
    /// `found` has said stop.
    fn walk(dir: &Path, place: &Path, w: &mut Walk<'_>) -> bool {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) => return (w.found)(Found::Unreadable(dir.to_path_buf(), e)),
        };
        let mut entries: Vec<(PathBuf, Option<fs::FileType>)> = entries
            .filter_map(|e| e.ok())
            .map(|e| (e.path(), e.file_type().ok()))
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        for (path, kind) in entries {
            // Where the entry is without links, and whether it is a
            // directory (where a link leads, for a link).
            let (is_dir, real) = match kind {
                Some(kind) if !kind.is_symlink() => (
                    kind.is_dir(),
                    place.join(path.file_name().unwrap_or_default()),
                ),
                _ => (path.is_dir(), canonical(&path)),
            };
            let go_on = if !is_dir {
                !((w.wanted)(&path) && w.named.insert(real)) || (w.found)(Found::File(path))
            } else {
                !((w.enter)(&path) && w.entered.insert(real.clone())) || walk(&path, &real, w)
            };
            if !go_on {
                return false;
            }
        }
        true
    }
    fn canonical(path: &Path) -> PathBuf {
        fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    }
    let place = canonical(root);
    let mut w = Walk {
        enter,
        wanted,
        found,
        entered: HashSet::from([place.clone()]),
        named: HashSet::new(),
    };
    walk(root, &place, &mut w);
}

/// Returns `true` if the recursive `.silt` walker should *not* descend
/// into `path`. Decision is based on `path`'s filename (and, for the
/// `fuzz/corpus/…` case, an ancestor-chain check) — never on the
/// filesystem state of the children.
///
/// Current skip set:
///   * `target/` — Cargo build output (and conventionally any other
///     project build dir reusing the name).
///   * `.git/` — VCS metadata.
///   * `node_modules/` — npm/yarn dep dirs. Vendored JS projects
///     occasionally carry `.silt` examples we don't want to ingest.
///   * any directory under `fuzz/corpus/…` — each fuzz-target's corpus
///     dir carries thousands of 1-byte files we don't want to parse.
pub fn should_skip_dir(path: &Path) -> bool {
    let name = match path.file_name().and_then(|s| s.to_str()) {
        Some(n) => n,
        None => return false,
    };
    if name == "target" || name == ".git" || name == "node_modules" {
        return true;
    }
    // Skip any directory under `fuzz/corpus/…`. The corpus dir itself
    // (`fuzz/corpus`) can have named children per fuzz target
    // (`fuzz_formatter`, …) and each of *those* carries thousands of
    // 1-byte files we don't want to parse. We detect the ancestor chain
    // by looking for a `corpus` segment whose parent is `fuzz`.
    let mut comps = path.components().rev();
    while let Some(c) = comps.next() {
        if c.as_os_str() == "corpus"
            && let Some(parent) = comps.next()
            && parent.as_os_str() == "fuzz"
        {
            return true;
        }
    }
    false
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    /// The walk names each file once, by the first path in the order of
    /// names (a link to a file before the file, a link to a folder, a
    /// link back into the walk), and reports a folder it cannot read.
    #[test]
    fn the_walk_names_each_file_once_and_reports_an_unreadable_folder() {
        let dir = std::env::temp_dir().join(format!("silt_walk_files_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("real/inner")).unwrap();
        fs::create_dir_all(dir.join("shut")).unwrap();
        fs::write(dir.join("real/inner/x.silt"), "").unwrap();
        fs::write(dir.join("z.silt"), "").unwrap();
        symlink(dir.join("z.silt"), dir.join("a_link.silt")).unwrap();
        symlink(dir.join("real"), dir.join("b_folder")).unwrap();
        symlink(&dir, dir.join("real/back")).unwrap();
        fs::set_permissions(dir.join("shut"), fs::Permissions::from_mode(0o000)).unwrap();

        let mut files = Vec::new();
        let mut unreadable = Vec::new();
        let any = |_: &Path| true;
        walk_files(&dir, &any, &any, &mut |found| {
            match found {
                Found::File(path) => files.push(path.strip_prefix(&dir).unwrap().to_owned()),
                Found::Unreadable(path, _) => unreadable.push(path),
            }
            true
        });
        fs::set_permissions(dir.join("shut"), fs::Permissions::from_mode(0o755)).unwrap();
        let _ = fs::remove_dir_all(&dir);

        let files: Vec<&str> = files.iter().filter_map(|p| p.to_str()).collect();
        assert_eq!(files, ["a_link.silt", "b_folder/inner/x.silt"]);
        // (Root reads every folder.)
        if unreadable.is_empty() {
            return;
        }
        assert_eq!(unreadable, [dir.join("shut")]);
    }
}
