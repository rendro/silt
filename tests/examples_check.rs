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

/// Guard against reintroducing a drifting example-count claim in user-facing
/// docs. Prior fixes removed "40+ runnable sample programs" / "35+ runnable
/// sample programs" from README.md and docs/getting-started.md after the count
/// drifted away from the real number of files in `examples/`. The convergent
/// decision was to not state a count at all. This test locks that in: neither
/// file may contain a `<digits>+ runnable sample` pattern.
#[test]
fn docs_do_not_claim_drifting_example_count() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let targets = [
        manifest_dir.join("README.md"),
        manifest_dir.join("docs").join("getting-started.md"),
    ];

    // Hand-rolled matcher for `\d+\+ runnable sample` to avoid pulling in a
    // regex dependency for a single test.
    fn contains_drifting_count(haystack: &str) -> bool {
        let needle = " runnable sample";
        for (idx, _) in haystack.match_indices(needle) {
            // Walk backwards from `idx` over one or more digits followed by a '+'.
            let prefix = &haystack.as_bytes()[..idx];
            if prefix.last() != Some(&b'+') {
                continue;
            }
            let mut i = prefix.len() - 1; // position of '+'
            if i == 0 {
                continue;
            }
            let mut digit_count = 0;
            while i > 0 {
                let b = prefix[i - 1];
                if b.is_ascii_digit() {
                    digit_count += 1;
                    i -= 1;
                } else {
                    break;
                }
            }
            if digit_count > 0 {
                return true;
            }
        }
        false
    }

    let mut failures: Vec<String> = Vec::new();
    for path in &targets {
        let body = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e));
        if contains_drifting_count(&body) {
            failures.push(format!(
                "{} contains a `<digits>+ runnable sample` count; per prior audit \
                 fix the convergent decision is to omit the count entirely so it \
                 cannot drift from the real file count in examples/",
                path.display()
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
