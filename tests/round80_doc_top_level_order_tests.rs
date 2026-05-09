//! Source-grep regression locks for round-80 doc-frontmatter fixes.
//!
//! Finding L5 (round 80): `docs/strict-effects-migration.md` was the
//! only top-level `docs/*.md` page without an `order:` frontmatter
//! field. Every other top-level guide pinned a unique numeric order
//! (why-silt=0, getting-started=1, language-guide=2, concurrency=3,
//! ffi=4, editor-setup=5), but the strict-effects migration page had
//! no entry, leaving its sidebar position non-deterministic.
//!
//! Round 79 already locked the docs/language/ subtree
//! (round79_doc_frontmatter_parity_tests.rs::doc_language_order_uniqueness).
//! This file extends the parity invariant to the top-level docs/ tree:
//!   (a) every top-level docs/*.md must declare `order:`
//!   (b) the declared `order:` values must be pairwise unique

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Returns the value of the first `key:` line in the leading YAML
/// frontmatter of `path`, or `None` if no such line exists in the
/// first ~20 lines. Mirrors the helper in
/// round79_doc_frontmatter_parity_tests.rs.
fn frontmatter_value(path: &Path, key: &str) -> Option<String> {
    let body = fs::read_to_string(path).ok()?;
    let prefix = format!("{}:", key);
    for (i, line) in body.lines().enumerate() {
        if i > 20 {
            break;
        }
        if let Some(rest) = line.strip_prefix(&prefix) {
            let trimmed = rest.trim();
            let unquoted = trimmed
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .unwrap_or(trimmed);
            return Some(unquoted.to_string());
        }
    }
    None
}

/// Iterates every `*.md` file in the given directory (non-recursive).
fn md_files_in(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) == Some("md") {
            out.push(p);
        }
    }
    out.sort();
    out
}

#[test]
fn doc_top_level_order_present_and_unique() {
    // Round-80 L5 lock: every top-level docs/*.md must declare an
    // `order:` frontmatter field, and those values must be pairwise
    // unique. Missing or duplicated order values yield non-deterministic
    // sidebar rendering.
    let docs_dir = repo_root().join("docs");
    let files = md_files_in(&docs_dir);
    assert!(
        !files.is_empty(),
        "expected at least one *.md file in {}",
        docs_dir.display()
    );

    // (a) Presence: every file must have `order:`.
    let mut missing: Vec<String> = Vec::new();
    let mut by_order: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for path in &files {
        match frontmatter_value(path, "order") {
            Some(order) => {
                by_order.entry(order).or_default().push(path.clone());
            }
            None => {
                missing.push(
                    path.strip_prefix(repo_root())
                        .unwrap_or(path.as_path())
                        .display()
                        .to_string(),
                );
            }
        }
    }
    assert!(
        missing.is_empty(),
        "every top-level docs/*.md must declare an `order:` \
         frontmatter field, but the following file(s) are missing \
         it:\n  - {}",
        missing.join("\n  - ")
    );

    // (b) Uniqueness: no two files may share the same `order:`.
    let mut duplicates: Vec<String> = Vec::new();
    for (order, paths) in &by_order {
        if paths.len() > 1 {
            let names: Vec<String> = paths
                .iter()
                .map(|p| {
                    p.strip_prefix(repo_root())
                        .unwrap_or(p.as_path())
                        .display()
                        .to_string()
                })
                .collect();
            duplicates.push(format!("order: {} -> {}", order, names.join(", ")));
        }
    }
    assert!(
        duplicates.is_empty(),
        "every top-level docs/*.md must have a unique `order:` value, \
         but the following collisions were found:\n  - {}",
        duplicates.join("\n  - ")
    );
}
