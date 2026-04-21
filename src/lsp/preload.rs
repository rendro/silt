//! Workspace preloader: on initialize, scan the root for `.silt` files
//! and feed them through `update_document` so cross-file features
//! (goto-def, references, rename, workspace/symbol) work for files the
//! user hasn't explicitly opened.
//!
//! The scan is best-effort: unreadable files, parse failures, and
//! symlink loops never abort startup — they're logged to stderr and
//! the walk continues.

use std::fs;
use std::path::Path;
use std::str::FromStr;

use lsp_types::Uri;

use super::Server;

/// Maximum recursion depth for the workspace scan. Keeps a rogue
/// symlink cycle or an inhumanly-deep tree from pegging the thread.
const MAX_DEPTH: usize = 16;

/// Recursively walk `root`, load every `.silt` file, and feed it
/// through the normal `update_document` pipeline.
///
/// Skips `target/`, `.git/`, and any directory under `fuzz/corpus/`
/// (the last of which would otherwise ingest tens of thousands of
/// 1-byte entries and swamp memory).
pub(super) fn preload_workspace(server: &mut Server, root: &Path) {
    walk(server, root, 0);
}

fn walk(server: &mut Server, dir: &Path, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }

    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(err) => {
            eprintln!(
                "silt-lsp: preload: cannot read dir {}: {err}",
                dir.display()
            );
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };

        if file_type.is_dir() {
            if should_skip_dir(&path) {
                continue;
            }
            walk(server, &path, depth + 1);
        } else if file_type.is_file() {
            if path.extension().and_then(|s| s.to_str()) != Some("silt") {
                continue;
            }
            load_file(server, &path);
        }
        // Symlinks: ignored. `is_file`/`is_dir` on a DirEntry's file_type
        // do not follow them, so symlink loops can't recurse here.
    }
}

fn should_skip_dir(path: &Path) -> bool {
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

fn load_file(server: &mut Server, path: &Path) {
    let contents = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(err) => {
            eprintln!(
                "silt-lsp: preload: cannot read {}: {err}",
                path.display()
            );
            return;
        }
    };

    let uri = match path_to_file_uri(path) {
        Some(u) => u,
        None => {
            eprintln!(
                "silt-lsp: preload: cannot build file:// URI for {}",
                path.display()
            );
            return;
        }
    };

    // update_document handles parse/type errors internally and still
    // stores a (possibly-degraded) Document entry.
    server.update_document(uri, contents);
}

/// Build a `file://`-scheme `Uri` from an absolute filesystem path.
/// Uses `Uri::from_str` per the LSP 3.17+ Uri type.
fn path_to_file_uri(path: &Path) -> Option<Uri> {
    let abs = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let s = abs.to_str()?;

    // On Unix, absolute paths start with `/`; LSP wants `file:///path`.
    // On Windows, absolute paths look like `C:\foo` → `file:///C:/foo`.
    #[cfg(windows)]
    let encoded = {
        let with_fwd = s.replace('\\', "/");
        if with_fwd.starts_with('/') {
            format!("file://{with_fwd}")
        } else {
            format!("file:///{with_fwd}")
        }
    };
    #[cfg(not(windows))]
    let encoded = if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{s}")
    };

    Uri::from_str(&encoded).ok()
}
