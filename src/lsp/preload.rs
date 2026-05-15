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
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};

use super::Server;

/// Characters that must be percent-encoded inside a `file://` path
/// component. Built from `CONTROLS` (all bytes below 0x20 plus 0x7F)
/// and then every non-unreserved printable ASCII byte except `/`
/// (preserved as the path separator) and `:` (preserved so the Windows
/// drive letter `C:` survives; it's in RFC 3986 `pchar`'s sub-delims
/// via `pchar = unreserved / pct-encoded / sub-delims / ":" / "@"`).
///
/// Unreserved set per RFC 3986: `ALPHA / DIGIT / "-" / "." / "_" / "~"`.
/// Everything else in ASCII gets encoded. Non-ASCII bytes are encoded
/// by `utf8_percent_encode` automatically.
const URI_PATH_RESERVED: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'!')
    .add(b'"')
    .add(b'#')
    .add(b'$')
    .add(b'%')
    .add(b'&')
    .add(b'\'')
    .add(b'(')
    .add(b')')
    .add(b'*')
    .add(b'+')
    .add(b',')
    .add(b';')
    .add(b'<')
    .add(b'=')
    .add(b'>')
    .add(b'?')
    .add(b'@')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}')
    .add(b'\x7f');

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

/// Skip-list policy: delegate to the crate-wide `file_discovery`
/// module so the LSP preloader and the CLI's recursive `.silt`
/// discovery (`src/cli/paths.rs::find_silt_files`) share a single
/// definition of "directories to skip when hunting for source files".
/// Audit round 87 LATENT: pre-fix, this rule lived only in preload.rs
/// and the CLI walkers happily descended into `target/`, `.git/`,
/// `node_modules/`, and `fuzz/corpus/` — so `silt fmt`/`silt test`
/// could rewrite or execute vendored / generated `.silt` files the
/// LSP correctly ignored. The helper is kept here as a thin shim so
/// the surrounding `walk` reads naturally; the policy itself now lives
/// in one place.
fn should_skip_dir(path: &Path) -> bool {
    crate::file_discovery::should_skip_dir(path)
}

fn load_file(server: &mut Server, path: &Path) {
    let contents = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(err) => {
            eprintln!("silt-lsp: preload: cannot read {}: {err}", path.display());
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
///
/// Percent-encodes every path byte that isn't in the RFC 3986
/// unreserved set (plus `/` as the separator and `:` so Windows drive
/// letters survive). Spaces become `%20`, non-ASCII bytes become
/// `%XX%XX…`. Without this, `Uri::from_str` rejects any workspace
/// containing a space or non-ASCII character, so preload silently
/// skipped those files.
pub fn path_to_file_uri(path: &Path) -> Option<Uri> {
    let abs = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let s = abs.to_str()?;

    // On Unix, absolute paths start with `/`; LSP wants `file:///path`.
    // On Windows, absolute paths look like `C:\foo` → `file:///C:/foo`.
    #[cfg(windows)]
    let raw = {
        let with_fwd = s.replace('\\', "/");
        if with_fwd.starts_with('/') {
            format!("file://{with_fwd}")
        } else {
            format!("file:///{with_fwd}")
        }
    };
    #[cfg(not(windows))]
    let raw = if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{s}")
    };

    // Split into `file://` prefix and the path remainder; only the
    // remainder needs percent-encoding (the scheme must stay literal).
    // Every reachable branch above writes `file://` then either the
    // already-leading `/` or an inserted `/`, so the full prefix is
    // always `file:///` on a valid absolute path — but we handle the
    // `file://` slice conservatively for robustness.
    let prefix_len = "file://".len();
    let (prefix, remainder) = raw.split_at(prefix_len);
    let encoded_remainder = utf8_percent_encode(remainder, URI_PATH_RESERVED).to_string();
    let encoded = format!("{prefix}{encoded_remainder}");

    Uri::from_str(&encoded).ok()
}
