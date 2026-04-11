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

/// Regression test for a GAP audit finding: the `http.serve` example in
/// `docs/stdlib/http.md` was not self-contained (referenced `User`,
/// `json.parse`, `string.split`, and `list.filter` without importing them
/// or defining `User`). A reader could not copy the example into a file
/// and have it type-check.
///
/// This test extracts every ```silt code block from `docs/stdlib/http.md`
/// that looks like a complete program (contains `fn main`), writes it to
/// a temp file, and runs `silt check` on it. If any block fails to
/// type-check, the test fails — which locks in the convergent decision
/// that every runnable block in the HTTP stdlib doc must stand alone.
#[test]
fn http_stdlib_doc_examples_type_check() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let doc_path = manifest_dir.join("docs").join("stdlib").join("http.md");
    let body = std::fs::read_to_string(&doc_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", doc_path.display(), e));

    // Hand-roll a tiny fenced-code-block extractor: open on ```silt, close on
    // the next line that starts with ```. We avoid pulling in a markdown dep.
    let mut blocks: Vec<(usize, String)> = Vec::new();
    let mut lines = body.lines().enumerate();
    while let Some((idx, line)) = lines.next() {
        if line.trim_start().starts_with("```silt") {
            let start_line = idx + 2; // 1-indexed, first content line
            let mut buf = String::new();
            for (_, content) in lines.by_ref() {
                if content.trim_start().starts_with("```") {
                    break;
                }
                buf.push_str(content);
                buf.push('\n');
            }
            blocks.push((start_line, buf));
        }
    }
    assert!(
        !blocks.is_empty(),
        "expected at least one ```silt code block in {}",
        doc_path.display()
    );

    // Only full programs (containing `fn main`) are expected to type-check
    // standalone. Snippet blocks (e.g. type signatures, REPL-style one-liners)
    // are intentionally skipped.
    let runnable: Vec<&(usize, String)> = blocks
        .iter()
        .filter(|(_, src)| src.contains("fn main"))
        .collect();
    assert!(
        !runnable.is_empty(),
        "expected at least one runnable ```silt block (containing `fn main`) in {}",
        doc_path.display()
    );

    let tmp_dir =
        std::env::temp_dir().join(format!("silt_http_doc_check_{}", std::process::id()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");

    let mut failures: Vec<String> = Vec::new();
    for (block_idx, (start_line, src)) in runnable.iter().enumerate() {
        let file = tmp_dir.join(format!("block_{block_idx}.silt"));
        std::fs::write(&file, src).expect("write temp silt file");
        let output = silt_cmd()
            .arg("check")
            .arg(&file)
            .output()
            .expect("failed to spawn silt");
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            failures.push(format!(
                "{}:{} (block #{block_idx}): exit={:?}\nstdout:\n{}\nstderr:\n{}",
                doc_path.display(),
                start_line,
                output.status.code(),
                stdout,
                stderr
            ));
        }
    }

    // Best-effort cleanup; leave artifacts on failure for debugging.
    if failures.is_empty() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    assert!(
        failures.is_empty(),
        "silt check failed for {} ```silt block(s) in docs/stdlib/http.md:\n\n{}",
        failures.len(),
        failures.join("\n---\n")
    );
}
