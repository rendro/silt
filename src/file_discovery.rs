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
/// `enter` lets the walk into, in the order of names. Links are
/// followed, and a directory is entered once, by the first path that
/// reaches it: a link back to a directory the walk has entered (a loop)
/// or a second way into one is not followed, so each file is named at
/// its own depth. A file reached by a second path (a link to it) is
/// named once, by the first.
pub fn files_under(
    root: &Path,
    enter: &dyn Fn(&Path) -> bool,
    wanted: &dyn Fn(&Path) -> bool,
) -> Vec<PathBuf> {
    struct Walk<'a> {
        enter: &'a dyn Fn(&Path) -> bool,
        wanted: &'a dyn Fn(&Path) -> bool,
        entered: HashSet<PathBuf>,
        named: HashSet<PathBuf>,
        out: Vec<PathBuf>,
    }
    fn walk(dir: &Path, w: &mut Walk<'_>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            if !path.is_dir() {
                if (w.wanted)(&path) && w.named.insert(canonical(&path)) {
                    w.out.push(path);
                }
            } else if (w.enter)(&path) && w.entered.insert(canonical(&path)) {
                walk(&path, w);
            }
        }
    }
    fn canonical(path: &Path) -> PathBuf {
        fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    }
    let mut w = Walk {
        enter,
        wanted,
        entered: HashSet::from([canonical(root)]),
        named: HashSet::new(),
        out: Vec::new(),
    };
    walk(root, &mut w);
    w.out
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
