//! Round-87 GAP-DOC-HELP lock.
//!
//! Five sites under `docs/language/` claimed that `silt --help`
//! enumerates either modules, error variants, or assertion entry
//! points. It does not — `silt --help` (`src/cli/help.rs`) prints
//! only the subcommand list and global flags. Pointing readers at
//! `silt --help` for any of those follow-ups is a documentation bug:
//! the user runs the command, sees nothing they were promised, and
//! loses trust in the docs.
//!
//! This source-grep lock walks every `*.md` under `docs/` and rejects
//! any occurrence of the literal substring `` `silt --help` `` that
//! sits within a small window of words suggesting enumeration of
//! module / error / assertion content. The window is intentionally
//! tight so that incidental, correct references to `silt --help`
//! (e.g. as the canonical way to discover the subcommand list) stay
//! legal.

use std::path::{Path, PathBuf};

const HELP_TOKEN: &str = "`silt --help`";
const FORBIDDEN_NEIGHBOURS: &[&str] = &[
    "modules",
    "module",
    "error",
    "assertion",
    "assertions",
    "enumerate",
    "enumerates",
    "list",
    "lists",
    // "full reference" is the cue collections.md uses to claim
    // `silt --help` is the canonical reference for `list.*`,
    // `map.*`, and `set.*`. Catch the noun on its own so the
    // map/set sites (which don't otherwise mention `list`) are
    // flagged too.
    "reference",
];

/// Window radius (in bytes) around each `` `silt --help` `` occurrence
/// we scan for the neighbour words. The audit finding suggested ~50
/// chars; the actual pre-fix sites in `collections.md` open the
/// sentence with "For the full reference, hover over any `<coll>.*`
/// call in your editor (the LSP renders the inlined builtin docs) or
/// run `silt --help`." — putting the trigger noun ~120 chars before
/// the help reference. 150 covers that comfortably while keeping the
/// window narrow enough to ignore mentions of the same nouns
/// elsewhere on the page.
const WINDOW: usize = 150;

fn collect_markdown_files(root: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_markdown_files(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("md") {
            out.push(path);
        }
    }
}

#[test]
fn docs_do_not_overclaim_silt_help() {
    let mut docs = Vec::new();
    collect_markdown_files(Path::new("docs"), &mut docs);
    assert!(
        !docs.is_empty(),
        "expected to find at least one .md under docs/ — wrong cwd?"
    );

    let mut offenders: Vec<String> = Vec::new();
    for path in &docs {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => continue,
        };
        // Find every occurrence of `silt --help` (with backticks).
        let mut start = 0usize;
        while let Some(rel) = text[start..].find(HELP_TOKEN) {
            let abs = start + rel;
            let window_start = abs.saturating_sub(WINDOW);
            let window_end = (abs + HELP_TOKEN.len() + WINDOW).min(text.len());
            // `text` is utf-8; clamp to char boundaries.
            let window_start = floor_char_boundary(&text, window_start);
            let window_end = ceil_char_boundary(&text, window_end);
            let window = &text[window_start..window_end].to_lowercase();
            for word in FORBIDDEN_NEIGHBOURS {
                // Crude whole-word check: surround with non-alnum
                // boundaries by checking the haystack against the
                // pattern with simple delimiters.
                if contains_word(window, word) {
                    // Locate line number for the error report.
                    let line_no = text[..abs].bytes().filter(|b| *b == b'\n').count() + 1;
                    offenders.push(format!(
                        "{}:{}: `silt --help` appears near `{}` in window: {:?}",
                        path.display(),
                        line_no,
                        word,
                        &text[window_start..window_end]
                    ));
                    break;
                }
            }
            start = abs + HELP_TOKEN.len();
        }
    }

    assert!(
        offenders.is_empty(),
        "docs/ overclaims what `silt --help` enumerates ({} site(s)):\n{}",
        offenders.len(),
        offenders.join("\n")
    );
}

fn contains_word(haystack: &str, word: &str) -> bool {
    let mut start = 0;
    while let Some(rel) = haystack[start..].find(word) {
        let abs = start + rel;
        let before_ok = abs == 0
            || !haystack.as_bytes()[abs - 1].is_ascii_alphanumeric()
                && haystack.as_bytes()[abs - 1] != b'_';
        let after_idx = abs + word.len();
        let after_ok = after_idx >= haystack.len()
            || (!haystack.as_bytes()[after_idx].is_ascii_alphanumeric()
                && haystack.as_bytes()[after_idx] != b'_');
        if before_ok && after_ok {
            return true;
        }
        start = abs + word.len();
    }
    false
}

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char_boundary(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}
