//! Filesystem path helpers used across several CLI subcommands:
//! recursive .silt discovery for `silt fmt`, and the lexical
//! path-normalization / path-relative helpers that `silt add` uses
//! when recording dependency paths in `silt.toml`.

use std::fs;
use std::path::{Path, PathBuf};

/// Recursively find all .silt files in a directory.
pub(crate) fn find_silt_files(dir: &Path) -> Vec<String> {
    let mut results = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return results;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            results.extend(find_silt_files(&path));
        } else {
            let name = path.to_string_lossy().to_string();
            if name.ends_with(".silt") {
                results.push(name);
            }
        }
    }
    results.sort();
    results
}

/// Lexically normalize a path: collapse `.` and `..` components without
/// touching the filesystem. Lockfile resolution does this internally
/// for dep paths; we apply the same normalization here so the manifest
/// records (and the success-line prints) the user-recognizable form.
pub(crate) fn normalize_path(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Express `target` as a path relative to `base`, using `..` segments
/// where necessary. Returns `None` only when the inputs differ in
/// rootedness (one absolute, one relative) — there's no sensible
/// relative form in that case and the caller falls back to absolute.
///
/// Rolling our own keeps us off the `pathdiff` crate; the logic is
/// 20 lines and the v0.7 manifest only needs ASCII-cleanly-named
/// paths anyway.
pub(crate) fn relative_from(base: &Path, target: &Path) -> Option<PathBuf> {
    if base.is_absolute() != target.is_absolute() {
        return None;
    }
    let base_components: Vec<_> = base.components().collect();
    let target_components: Vec<_> = target.components().collect();
    // Find the longest common prefix.
    let mut shared = 0;
    while shared < base_components.len()
        && shared < target_components.len()
        && base_components[shared] == target_components[shared]
    {
        shared += 1;
    }
    let mut result = PathBuf::new();
    for _ in shared..base_components.len() {
        result.push("..");
    }
    for comp in &target_components[shared..] {
        result.push(comp.as_os_str());
    }
    if result.as_os_str().is_empty() {
        // base == target — express that as `.` rather than the empty
        // string so toml_edit emits a syntactically valid path.
        result.push(".");
    }
    Some(result)
}
