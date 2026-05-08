//! Source-grep regression locks for round-79 doc-frontmatter fixes.
//!
//! Finding DOC-L1 (LATENT): two pages in `docs/language/` shared
//! `order: 6` (operators.md and loops-and-pipes.md), which made the
//! generated guide-sidebar ordering non-deterministic between renders.
//!
//! Finding DOC-L2 (LATENT): `docs/strict-effects-migration.md` used
//! `section: "Guides"` (plural) while every other top-level guide used
//! `section: "Guide"` (singular), splitting the doc into a phantom
//! section in the generated TOC.
//!
//! Both checks are source-grep over the `docs/` tree at test time.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Returns the value of the first `key:` line in the leading YAML
/// frontmatter of `path`, or `None` if no such line exists in the
/// first ~20 lines. Strips surrounding whitespace and a single pair of
/// matching double quotes.
fn frontmatter_value(path: &Path, key: &str) -> Option<String> {
    let body = fs::read_to_string(path).ok()?;
    let prefix = format!("{}:", key);
    for (i, line) in body.lines().enumerate() {
        if i > 20 {
            break;
        }
        if let Some(rest) = line.strip_prefix(&prefix) {
            let trimmed = rest.trim();
            // Strip a single pair of surrounding double quotes if present.
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
fn doc_section_uniformity() {
    // Round-79 DOC-L2 lock: every doc with a `section:` frontmatter
    // line must use the singular `"Guide"` (or `"Language"`), never
    // the stray plural `"Guides"`. The plural would silently spawn a
    // phantom TOC section in the rendered docs.
    let docs_dir = repo_root().join("docs");
    let files = md_files_in(&docs_dir);
    assert!(
        !files.is_empty(),
        "expected at least one *.md file in {}",
        docs_dir.display()
    );

    let mut offenders: Vec<String> = Vec::new();
    for path in &files {
        if let Some(value) = frontmatter_value(path, "section") {
            if value == "Guides" {
                offenders.push(format!(
                    "{}: section: \"Guides\" — should be \"Guide\" (singular)",
                    path.strip_prefix(repo_root()).unwrap_or(path).display()
                ));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "doc frontmatter `section:` values must be uniform — every \
         top-level guide uses \"Guide\" (singular), but the following \
         file(s) use \"Guides\" (plural):\n  - {}",
        offenders.join("\n  - ")
    );
}

#[test]
fn doc_language_order_uniqueness() {
    // Round-79 DOC-L1 lock: every file in docs/language/ must have a
    // unique `order:` value in its frontmatter. Two files sharing an
    // order makes sidebar rendering non-deterministic.
    let lang_dir = repo_root().join("docs").join("language");
    let files = md_files_in(&lang_dir);
    assert!(
        !files.is_empty(),
        "expected at least one *.md file in {}",
        lang_dir.display()
    );

    // Map order -> list of files that claim it.
    let mut by_order: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for path in &files {
        if let Some(order) = frontmatter_value(path, "order") {
            by_order.entry(order).or_default().push(path.clone());
        }
    }

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
        "every file in docs/language/ must have a unique `order:` \
         value, but the following collisions were found:\n  - {}",
        duplicates.join("\n  - ")
    );
}
