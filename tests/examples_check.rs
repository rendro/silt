//! Walk `examples/` and run `silt check` on every `*.silt` file.
//!
//! This pins the contract that every shipped example type-checks cleanly.
//! We intentionally only run `silt check` (not `silt run`) because some
//! examples are networked (http_server/http_client), interactive, or
//! long-running. Type-checking catches any API/syntax drift without
//! actually executing user code.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Files that are intentionally skipped. Keep this list empty unless a
/// concrete reason is documented inline.
const SKIP: &[&str] = &[
    // (none currently — add with a comment explaining why if needed)
];

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

/// Recursively collect every `.silt` file under `dir`.
fn collect_silt_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            collect_silt_files(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("silt") {
            out.push(path);
        }
    }
}

#[test]
fn every_example_type_checks() {
    let examples_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    assert!(
        examples_dir.is_dir(),
        "expected examples directory at {}",
        examples_dir.display()
    );

    let mut files = Vec::new();
    collect_silt_files(&examples_dir, &mut files);
    files.sort();
    assert!(
        !files.is_empty(),
        "expected at least one example .silt file under {}",
        examples_dir.display()
    );

    let mut failures: Vec<String> = Vec::new();

    for file in &files {
        let name = file
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        if SKIP.contains(&name) {
            continue;
        }

        let output = silt_cmd()
            .arg("check")
            .arg(file)
            .output()
            .expect("failed to spawn silt");

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            failures.push(format!(
                "{}: exit={:?}\nstdout:\n{}\nstderr:\n{}",
                file.display(),
                output.status.code(),
                stdout,
                stderr
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "silt check failed for {} example(s):\n\n{}",
        failures.len(),
        failures.join("\n---\n")
    );
}

/// Guard against reintroducing a drifting count claim in user-facing docs.
/// Prior fixes removed "40+ runnable sample programs" / "35+ runnable sample
/// programs" from README.md and docs/getting-started.md, and "160+ stdlib
/// functions" from docs/editor-setup.md, after the counts drifted away from
/// the real number of files in `examples/` or real stdlib function count.
/// The convergent decision was to not state a count at all.
///
/// This test locks that in across the whole doc surface: it scans README.md
/// and every `.md` file under `docs/` for any `<digits>+<ws>?<noun>` pattern
/// where `<noun>` is one of the drift-prone kinds (stdlib, runnable sample,
/// example, keyword, function, module). If a doc states such a count, it
/// must be removed — not corrected — per the standing audit preference.
#[test]
fn docs_do_not_claim_drifting_example_count() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));

    // Collect targets: README.md plus every .md file under docs/ (recursive).
    let mut targets: Vec<PathBuf> = Vec::new();
    targets.push(manifest_dir.join("README.md"));
    fn collect_md_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                collect_md_files(&path, out);
            } else if path.extension().and_then(|s| s.to_str()) == Some("md") {
                out.push(path);
            }
        }
    }
    collect_md_files(&manifest_dir.join("docs"), &mut targets);
    targets.sort();

    // Drift-prone nouns that, when prefixed by `<digits>+`, indicate a count
    // claim that can (and historically has) drifted away from reality.
    const DRIFT_NOUNS: &[&str] = &[
        "stdlib",
        "runnable sample",
        "example",
        "examples",
        "keyword",
        "keywords",
        "function",
        "functions",
        "module",
        "modules",
    ];

    // Hand-rolled matcher for `\d+\+\s*<noun>` to avoid pulling in a regex
    // dependency for a single test. Returns the first offending match as a
    // short snippet, or None if the haystack is clean.
    fn find_drifting_count(haystack: &str) -> Option<String> {
        for noun in DRIFT_NOUNS {
            // Try matching `<digits>+ <noun>` and `<digits>+<noun>` (no space).
            for (idx, _) in haystack.match_indices(noun) {
                // Require that the match is at a word boundary on the right
                // (next char is not alphanumeric/underscore), so we don't
                // treat "function" inside "functional" as a hit.
                let tail = &haystack.as_bytes()[idx + noun.len()..];
                if let Some(&b) = tail.first() {
                    if b.is_ascii_alphanumeric() || b == b'_' {
                        continue;
                    }
                }

                // Walk backwards over optional whitespace, then require '+',
                // then one or more ascii digits.
                let prefix = &haystack.as_bytes()[..idx];
                let mut i = prefix.len();
                // Skip whitespace immediately before the noun.
                while i > 0 && (prefix[i - 1] == b' ' || prefix[i - 1] == b'\t') {
                    i -= 1;
                }
                if i == 0 || prefix[i - 1] != b'+' {
                    continue;
                }
                i -= 1; // position of '+'
                let plus_pos = i;
                let mut digit_count = 0;
                while i > 0 && prefix[i - 1].is_ascii_digit() {
                    digit_count += 1;
                    i -= 1;
                }
                if digit_count == 0 {
                    continue;
                }
                // Build a short snippet for the error message.
                let snippet_start = i;
                let snippet_end = idx + noun.len();
                let snippet = &haystack[snippet_start..snippet_end];
                // Require that the char before the digits is not itself a
                // digit or word char (avoids matching "v1.0+ function" etc.).
                if snippet_start > 0 {
                    let b = prefix[snippet_start - 1];
                    if b.is_ascii_alphanumeric() || b == b'_' || b == b'.' {
                        continue;
                    }
                }
                let _ = plus_pos;
                return Some(snippet.to_string());
            }
        }
        None
    }

    let mut failures: Vec<String> = Vec::new();
    for path in &targets {
        let body = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e));
        if let Some(hit) = find_drifting_count(&body) {
            failures.push(format!(
                "{} contains a drift-prone `<digits>+ <noun>` count: `{}`. \
                 Per prior audit fix the convergent decision is to omit the \
                 count entirely so it cannot drift from reality.",
                path.display(),
                hit
            ));
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Extracts every ```silt fenced block from a markdown file. Returns a
/// vector of `(opener_line_number_1indexed, block_source)` tuples. The
/// opener line is the line of the ```silt fence (not the first content
/// line), so error messages point at something a user can search for.
fn extract_silt_blocks(body: &str) -> Vec<(usize, String)> {
    let mut blocks: Vec<(usize, String)> = Vec::new();
    let mut lines = body.lines().enumerate();
    while let Some((idx, line)) = lines.next() {
        if line.trim_start().starts_with("```silt") {
            let opener_line = idx + 1; // 1-indexed line of the ```silt fence
            let mut buf = String::new();
            for (_, content) in lines.by_ref() {
                if content.trim_start().starts_with("```") {
                    break;
                }
                buf.push_str(content);
                buf.push('\n');
            }
            blocks.push((opener_line, buf));
        }
    }
    blocks
}

/// Regression test for GAP audit findings D1+D2: every ```silt fenced
/// block in the documentation that contains `fn main` must type-check
/// cleanly via `silt check`. Supersedes the old http-only walker by
/// covering `README.md` and every `.md` file under `docs/` recursively.
///
/// This locks in the convergent decision that every runnable code block
/// shipped in user-facing docs must be copy-paste-able: a reader should
/// be able to select the block, save it to a `.silt` file, and have the
/// type checker accept it without edits.
///
/// Snippet blocks (type signatures, REPL-style one-liners, partial
/// programs without a `fn main`) are intentionally skipped — those are
/// fragments, not complete programs.
#[test]
fn all_doc_fn_main_blocks_type_check() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));

    // Collect targets: README.md plus every .md file under docs/ (recursive).
    let mut targets: Vec<PathBuf> = Vec::new();
    let readme = manifest_dir.join("README.md");
    if readme.is_file() {
        targets.push(readme);
    }
    fn collect_md_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                collect_md_files(&path, out);
            } else if path.extension().and_then(|s| s.to_str()) == Some("md") {
                out.push(path);
            }
        }
    }
    collect_md_files(&manifest_dir.join("docs"), &mut targets);
    targets.sort();
    assert!(
        !targets.is_empty(),
        "expected at least one markdown target (README.md or docs/**/*.md)"
    );

    let tmp_dir =
        std::env::temp_dir().join(format!("silt_all_doc_check_{}", std::process::id()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");

    let mut failures: Vec<String> = Vec::new();
    let mut runnable_block_count = 0usize;

    for doc_path in &targets {
        let body = std::fs::read_to_string(doc_path)
            .unwrap_or_else(|e| panic!("failed to read {}: {}", doc_path.display(), e));
        let blocks = extract_silt_blocks(&body);

        for (opener_line, src) in blocks {
            // Only full programs (containing `fn main`) are expected to
            // type-check standalone. Snippet blocks are skipped.
            if !src.contains("fn main") {
                continue;
            }
            runnable_block_count += 1;

            let file_stem = doc_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("doc");
            let file = tmp_dir.join(format!("{file_stem}_line{opener_line}.silt"));
            std::fs::write(&file, &src).expect("write temp silt file");

            let output = silt_cmd()
                .arg("check")
                .arg(&file)
                .output()
                .expect("failed to spawn silt");
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stdout = String::from_utf8_lossy(&output.stdout);
                failures.push(format!(
                    "{}:{} (```silt fence): exit={:?}\nstdout:\n{}\nstderr:\n{}",
                    doc_path.display(),
                    opener_line,
                    output.status.code(),
                    stdout,
                    stderr
                ));
            }
        }
    }

    assert!(
        runnable_block_count > 0,
        "expected at least one runnable ```silt block (containing `fn main`) across all docs"
    );

    // Best-effort cleanup; leave artifacts on failure for debugging.
    if failures.is_empty() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    assert!(
        failures.is_empty(),
        "silt check failed for {} ```silt block(s) across docs/ and README.md:\n\n{}",
        failures.len(),
        failures.join("\n---\n")
    );
}
