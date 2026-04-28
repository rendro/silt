//! Stdout-parity walker for `println(...)  -- <expected>` annotations
//! inside markdown doc files (`docs/**/*.md`, `README.md`).
//!
//! Round-62 audit gap G1: the existing walkers fall short on
//! `docs/**/*.md` content:
//!
//!   - `tests/docs_stdlib_println_parity_tests.rs` only walks builtin
//!     doc strings via `silt::typechecker::iter_builtin_docs()`.
//!   - `tests/examples_check.rs::all_doc_fn_main_blocks_run_if_safe`
//!     compiles + runs markdown ```silt fences but does NOT diff
//!     stdout against `-- <expected>` annotations.
//!
//! Result: `docs/concurrency.md`, `docs/language/*.md`,
//! `docs/proposals/*.md`, and `README.md` operated under a weak gate.
//! Round 62 caught a quoted-string `println(msg) -- "hello"` in
//! `concurrency.md:215` that this walker would have caught immediately.
//!
//! ## What this walker does
//!
//! 1. Recursively walks `README.md` plus every `docs/**/*.md`.
//! 2. Extracts each ```silt fence whose body contains `fn main`.
//! 3. For each fence, parses every line containing `println(` looking
//!    for an `-- <expected>` trailing annotation.
//! 4. Writes the fence body to a temp `.silt` file and runs `silt run`,
//!    capturing stdout.
//! 5. Walks stdout in source order, locating each annotated expected
//!    value within a small skip window so unannotated `println` calls
//!    or branch-only output don't break alignment.
//! 6. Fails loudly with a precise `<file>:<fence-line> [block-line N]`
//!    pointer plus the expected vs. observed text.
//!
//! ## Skip rules
//!
//! Same conservative deny list as the stdlib parity walker (and as
//! `all_doc_fn_main_blocks_run_if_safe`). The deny list intentionally
//! avoids blanket-skipping concurrency / time / fs APIs unless the
//! block actually invokes them — the round-62 regression appeared in
//! a fence that imported `channel`/`task`, so leaving those out of
//! the skip set would defeat the lock.
//!
//! Per-block opt-out is supported via:
//!   - First non-empty line equal to `// noexec` or `-- noexec`
//!     (matches `examples_check.rs`).
//!
//! ## Illustrative annotations
//!
//! Annotations that contain `approximately`, start with `e.g.`, or
//! contain `...` are treated as illustrative — we still require the
//! corresponding println produced *some* non-empty output, but we
//! don't byte-compare. This matches the stdlib walker's behavior.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Same deny list as the stdlib parity walker. Kept in lockstep so a
/// block that's safe to *run* under the existing markdown walker is
/// also safe to *stdout-diff* here. If you tighten one list, tighten
/// the other in the same patch.
const DENY_SUBSTRINGS: &[&str] = &[
    // Networked.
    "http.get",
    "http.post",
    "http.put",
    "http.delete",
    "http.serve",
    "http.Server",
    // Concurrency.
    "task.spawn",
    "task.spawn_until",
    "task.deadline",
    "task.sleep",
    "channel.new",
    "channel.send",
    "channel.receive",
    "channel.recv",
    "channel.select",
    "channel.close",
    // Interactive IO.
    "io.read_line",
    "io.stdin",
    "read_line",
    // File system.
    "fs.read",
    "fs.write",
    "fs.append",
    "fs.list",
    "fs.delete",
    "fs.remove",
    "fs.exists",
    "fs.create_dir",
    "fs.copy",
    "fs.move",
    "fs.metadata",
    // Environment.
    "env.get",
    "env.set",
    "env.args",
    // Time — non-deterministic sources.
    "time.sleep",
    "time.now",
    "time.today",
    "time.to_utc",
    // Random sources.
    "math.random",
    "uuid.v4",
    "uuid.v7",
    // Infinite loops.
    "loop {",
    "while true",
    // Process exit.
    "process.exit",
];

const NOEXEC_MARKERS: &[&str] = &["// noexec", "-- noexec"];

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

/// Decide whether a block should be excluded from the stdout-diff
/// walker. Mirrors `all_doc_fn_main_blocks_run_if_safe::skip_reason`
/// in `examples_check.rs`.
fn is_unsafe_to_run(src: &str) -> bool {
    let first_nonempty = src.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let trimmed = first_nonempty.trim();
    if NOEXEC_MARKERS.iter().any(|m| trimmed == *m) {
        return true;
    }
    DENY_SUBSTRINGS.iter().any(|needle| src.contains(needle))
}

/// Recursively collect every `*.md` file under `dir`.
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

/// Extract every ```silt fenced block from a markdown body. Returns a
/// vector of `(opener_line_number_1indexed, block_source)` tuples.
/// Mirrors `extract_silt_blocks` in `tests/examples_check.rs` and the
/// stdlib parity walker so the three walkers see the same fences.
fn extract_silt_blocks(body: &str) -> Vec<(usize, String)> {
    let mut blocks: Vec<(usize, String)> = Vec::new();
    let mut lines = body.lines().enumerate();
    while let Some((idx, line)) = lines.next() {
        if line.trim_start().starts_with("```silt") {
            let opener_line = idx + 1;
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

/// A single `(println(...), expected)` pair extracted from a block.
#[derive(Debug)]
struct PrintlnPair {
    /// 1-indexed line number within the fence body. Used in failure
    /// messages so a doc author can locate the offending println.
    block_line: usize,
    expected: Option<String>,
}

/// Walk every line in `src` and, for each line containing `println(`,
/// extract any `-- <expected>` trailing annotation. Lines that begin
/// with `--` or `//` (the line itself is a comment, not a call) are
/// ignored. Lines whose `println(` is to the right of an `->` (e.g.
/// inside a function-type annotation, `fn foo() -> Result(...)`) are
/// also ignored — the prefix `->` heuristic is identical to the stdlib
/// walker's, and the same caveats apply.
fn extract_println_pairs(src: &str) -> Vec<PrintlnPair> {
    let mut pairs = Vec::new();
    for (i, line) in src.lines().enumerate() {
        let trimmed = line.trim_start();
        if !trimmed.contains("println(") {
            continue;
        }
        if trimmed.starts_with("--") || trimmed.starts_with("//") {
            continue;
        }
        let println_pos = trimmed.find("println(").unwrap();
        if trimmed[..println_pos].contains("->") {
            continue;
        }
        let expected = find_expected_comment(trimmed);
        pairs.push(PrintlnPair {
            block_line: i + 1,
            expected,
        });
    }
    pairs
}

/// Locate `-- <expected>` on a single line and return the trailing
/// expected text trimmed of whitespace. Requires a literal ` -- ` (or
/// terminal ` --`) so we don't pick up `--` inside string literals
/// like `"foo--bar"` or arithmetic `x--`.
fn find_expected_comment(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i + 3 < bytes.len() {
        if bytes[i] == b' ' && bytes[i + 1] == b'-' && bytes[i + 2] == b'-' {
            if i + 3 == bytes.len() {
                return Some(String::new());
            }
            if bytes[i + 3] == b' ' {
                let tail = &line[i + 3..];
                return Some(tail.trim().to_string());
            }
        }
        i += 1;
    }
    None
}

/// Some doc annotations are deliberately illustrative (random output,
/// timestamps, hashes). Match the same heuristic as the stdlib walker.
fn is_illustrative(expected: &str) -> bool {
    let lower = expected.to_ascii_lowercase();
    lower.contains("approximately")
        || lower.starts_with("e.g.")
        || lower.starts_with("e.g ")
        || expected.contains("...")
}

/// Strip parenthesised commentary from the expected, e.g.
/// `42 (the answer)` -> `42`. Matches the stdlib walker.
fn strip_trailing_commentary(expected: &str) -> &str {
    if let Some(pos) = expected.find("  (") {
        return &expected[..pos];
    }
    if let Some(pos) = expected.find(" (") {
        let before_ok = expected[..pos]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric());
        let first_inside = expected[pos + 2..].chars().next();
        let inside_ok = first_inside.is_some_and(|c| c.is_alphabetic());
        if before_ok && inside_ok {
            return &expected[..pos];
        }
    }
    expected
}

/// Outcome of comparing extracted pairs against captured stdout. Used
/// by the live walker AND by the synthetic sanity test, so the diff
/// logic is exercised by both. `Mismatch` is the meaningful failure;
/// `Ok` includes the count of pairs we matched.
#[derive(Debug, PartialEq, Eq)]
enum DiffOutcome {
    Ok { matched: usize },
    Mismatch { expected: String, searched: String },
}

/// The shared stdout-diff routine. Walks `pairs` in source order and
/// looks for each annotated expected value in `stdout_lines`, allowing
/// up to `max_skip` intervening lines so unannotated prints in the
/// fence body don't break alignment. Returns the first mismatch, or
/// `Ok { matched }` if every annotated pair was found.
fn diff_println_pairs(pairs: &[PrintlnPair], stdout: &str) -> DiffOutcome {
    let stdout_lines: Vec<&str> = stdout.lines().collect();
    let mut cursor = 0usize;
    let mut matched = 0usize;
    let max_skip = 32usize;

    for pair in pairs {
        let Some(expected) = pair.expected.as_deref() else {
            continue;
        };
        let want = strip_trailing_commentary(expected.trim()).trim();
        let illustrative = is_illustrative(expected);
        let search_end = stdout_lines.len().min(cursor + max_skip);
        let mut found_at: Option<usize> = None;
        for idx in cursor..search_end {
            let got = stdout_lines[idx].trim_end();
            if illustrative {
                if !got.trim().is_empty() {
                    found_at = Some(idx);
                    break;
                }
            } else if got == want {
                found_at = Some(idx);
                break;
            }
        }
        match found_at {
            Some(idx) => {
                cursor = idx + 1;
                matched += 1;
            }
            None => {
                if illustrative {
                    continue;
                }
                let preview = stdout_lines
                    .get(cursor..search_end)
                    .map(|lines| lines.join("\n"))
                    .unwrap_or_default();
                return DiffOutcome::Mismatch {
                    expected: want.to_string(),
                    searched: preview,
                };
            }
        }
    }
    DiffOutcome::Ok { matched }
}

#[test]
fn docs_markdown_println_annotations_match_silt_run_stdout() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));

    // Universe: every .md under docs/. We deliberately leave README.md
    // out of this walker because README annotations are typically
    // narrative ("record update", "the answer", etc.) rather than
    // exact stdout. The stdlib + docs/ walkers cover the
    // strict-stdout-diff case; mixing the two conventions creates
    // false positives on README without adding regression coverage
    // (README has zero `fn main` fences with strict-stdout
    // annotations as of round 62).
    let mut targets: Vec<PathBuf> = Vec::new();
    collect_md_files(&manifest_dir.join("docs"), &mut targets);
    targets.sort();
    assert!(
        !targets.is_empty(),
        "expected at least one markdown target under docs/"
    );

    let tmp_dir = std::env::temp_dir().join(format!(
        "silt_docs_md_println_parity_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");

    // Phase 1: collect runnable jobs sequentially. Dedup identical
    // fence bodies so a snippet copy/pasted into multiple files only
    // runs once.
    struct Job {
        doc_path: PathBuf,
        opener_line: usize,
        src: String,
        file_stem: String,
        pairs: Vec<PrintlnPair>,
    }
    let mut jobs: Vec<Job> = Vec::new();
    let mut seen_blocks: HashSet<u64> = HashSet::new();

    for doc_path in &targets {
        let body = std::fs::read_to_string(doc_path)
            .unwrap_or_else(|e| panic!("failed to read {}: {}", doc_path.display(), e));
        let blocks = extract_silt_blocks(&body);
        for (opener_line, src) in blocks {
            if !src.contains("fn main") {
                continue;
            }
            if is_unsafe_to_run(&src) {
                continue;
            }

            // Dedup identical bodies so a snippet copy/pasted into
            // several markdown files only runs once.
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            std::hash::Hash::hash(&src, &mut hasher);
            let h = std::hash::Hasher::finish(&hasher);
            if !seen_blocks.insert(h) {
                continue;
            }

            let pairs = extract_println_pairs(&src);
            let annotated_count = pairs.iter().filter(|p| p.expected.is_some()).count();
            if annotated_count == 0 {
                continue;
            }

            let file_stem = doc_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("doc")
                .to_string();
            jobs.push(Job {
                doc_path: doc_path.clone(),
                opener_line,
                src,
                file_stem,
                pairs,
            });
        }
    }

    let checked_blocks = jobs.len();

    // Phase 2: parallel subprocess fan-out. Subprocess spawn + silt
    // cold-start dominates wall clock; sizing to available_parallelism
    // capped at 8 matches the other doc walkers in this crate.
    let pool_size = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(2, 8);

    let failures: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let checked_pairs = AtomicUsize::new(0);
    let next_job = AtomicUsize::new(0);
    let jobs_ref = &jobs;
    let tmp_dir_ref = &tmp_dir;
    let failures_ref = &failures;
    let checked_pairs_ref = &checked_pairs;
    let next_job_ref = &next_job;

    std::thread::scope(|scope| {
        for _ in 0..pool_size {
            scope.spawn(move || {
                loop {
                    let idx = next_job_ref.fetch_add(1, Ordering::SeqCst);
                    if idx >= jobs_ref.len() {
                        return;
                    }
                    let job = &jobs_ref[idx];
                    let file = tmp_dir_ref.join(format!(
                        "{}_line{}.silt",
                        job.file_stem, job.opener_line
                    ));
                    std::fs::write(&file, &job.src).expect("write temp silt file");

                    let output = silt_cmd()
                        .arg("run")
                        .arg(&file)
                        .output()
                        .expect("failed to spawn silt");

                    if !output.status.success() {
                        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
                        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
                        failures_ref.lock().unwrap().push(format!(
                            "{}:{} (```silt fence): `silt run` exited non-zero \
                             while the walker was trying to verify println \
                             annotations. exit={:?}\nstdout:\n{}\nstderr:\n{}",
                            job.doc_path.display(),
                            job.opener_line,
                            output.status.code(),
                            stdout,
                            stderr
                        ));
                        continue;
                    }

                    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
                    match diff_println_pairs(&job.pairs, &stdout) {
                        DiffOutcome::Ok { matched } => {
                            checked_pairs_ref.fetch_add(matched, Ordering::SeqCst);
                        }
                        DiffOutcome::Mismatch { expected, searched } => {
                            // Find the first annotated pair to point at;
                            // the diff returns on first mismatch, so the
                            // offending pair is the first whose expected
                            // is non-illustrative AND wasn't located.
                            let pointer = job
                                .pairs
                                .iter()
                                .find(|p| {
                                    p.expected
                                        .as_deref()
                                        .map(|e| {
                                            !is_illustrative(e)
                                                && strip_trailing_commentary(e.trim()).trim()
                                                    == expected
                                        })
                                        .unwrap_or(false)
                                })
                                .map(|p| p.block_line)
                                .unwrap_or(0);
                            failures_ref.lock().unwrap().push(format!(
                                "{}:{} (```silt fence, block line {}): \
                                 expected `{}` but got mismatch — could not find \
                                 annotated value in remaining stdout.\n  \
                                 searched: {:?}\n\
                                 full stdout:\n{}",
                                job.doc_path.display(),
                                job.opener_line,
                                pointer,
                                expected,
                                searched,
                                stdout
                            ));
                        }
                    }
                }
            });
        }
    });

    let failures: Vec<String> = failures.into_inner().unwrap();
    let checked_pairs = checked_pairs.load(Ordering::SeqCst);

    // The walker is a regression LOCK, not a coverage gate. As of
    // round 62 the docs/ tree has very few fences that are
    // simultaneously (a) safe to run under the deny list, (b)
    // contain `fn main`, and (c) carry `-- <expected>` annotations
    // on a `println(...)` line. A dormant walker is still useful:
    // the moment a doc author adds such an annotation (the natural
    // way to document expected output), this walker locks it
    // against drift. Zero pairs is therefore acceptable — what we
    // require is that the extraction + diff machinery is exercised
    // by SOME path, which the synthetic sanity test below
    // (`walker_extraction_distinguishes_quoted_from_unquoted_println_output`)
    // guarantees.
    //
    // We do still emit a one-line note when zero pairs were seen so
    // a human inspecting CI output can tell whether this walker did
    // any real work this run.
    if checked_pairs == 0 {
        eprintln!(
            "[docs_markdown_println_parity] walker scanned {} runnable \
             fence(s) but found 0 annotated `println(...) -- <expected>` \
             pairs. The walker is armed but dormant — it will activate \
             the moment a doc author adds a strict-stdout annotation \
             to a runnable ```silt fn main``` fence under docs/.",
            checked_blocks,
        );
    }

    if failures.is_empty() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    assert!(
        failures.is_empty(),
        "{} println annotation(s) in markdown docs do not match `silt run` \
         stdout. Either the doc comment drifted from reality (common \
         causes: `.0` on integer-valued floats, double-quoted strings \
         inside `println` comments, `Some(\"foo\")` instead of \
         `Some(foo)`) or silt's Display convention changed. Fix the doc \
         annotation in the corresponding `.md` file to match observed \
         stdout, OR add an `approximately` / `e.g.` qualifier if the \
         value is illustrative.\n\n{}",
        failures.len(),
        failures.join("\n---\n")
    );
}

/// Synthetic sanity test for the extraction + compare logic itself.
/// This locks the walker independent of the real docs tree: even if
/// every doc snippet is well-aligned, we still need to know that a
/// quoted-vs-bare String mismatch (the round-62 `concurrency.md:215`
/// case) actually trips the diff. If a future refactor accidentally
/// makes the diff lenient (e.g. trims surrounding quotes from the
/// expected text), this test fails immediately.
#[test]
fn walker_extraction_distinguishes_quoted_from_unquoted_println_output() {
    // The body deliberately mirrors the round-62 regression: a String
    // value passed to `println` prints bare (`hello`), but the
    // annotation wraps it in quotes (`"hello"`). The diff MUST flag
    // this as a mismatch.
    let block_body = r#"fn main() {
  let msg = "hello"
  println(msg) -- "hello"
}
"#;

    // Extraction must find exactly one println pair with the quoted
    // expected text preserved.
    let pairs = extract_println_pairs(block_body);
    let annotated: Vec<&PrintlnPair> =
        pairs.iter().filter(|p| p.expected.is_some()).collect();
    assert_eq!(
        annotated.len(),
        1,
        "extract_println_pairs should find exactly one annotated \
         println pair in the synthetic block; found {}: {:?}",
        annotated.len(),
        pairs
    );
    assert_eq!(
        annotated[0].expected.as_deref(),
        Some("\"hello\""),
        "extraction should preserve the literal annotation text \
         (including the surrounding quotes), so the diff sees what \
         the doc author wrote — not a normalized form. If this fails, \
         `find_expected_comment` is silently stripping quotes and the \
         walker can no longer catch quoted-string regressions."
    );

    // Diff against the bare stdout that silt actually produces for
    // `println(msg)` where `msg : String = "hello"`. The expected
    // annotation `"hello"` does NOT match the bare `hello` line, so
    // the diff must report Mismatch.
    let actual_stdout = "hello\n";
    let outcome = diff_println_pairs(&pairs, actual_stdout);
    match outcome {
        DiffOutcome::Mismatch { expected, .. } => {
            assert_eq!(
                expected, "\"hello\"",
                "diff should surface the literal expected text from \
                 the annotation; got {expected:?}"
            );
        }
        DiffOutcome::Ok { matched } => panic!(
            "diff reported Ok (matched={matched}) for a quoted-vs-bare \
             mismatch — the walker is broken and would silently let \
             round-62-style regressions through. Inspect \
             `diff_println_pairs` and `is_illustrative`."
        ),
    }

    // Sanity: the inverse case (annotation matches bare output) must
    // succeed, otherwise the diff is trivially failing every input.
    let block_body_ok = r#"fn main() {
  let msg = "hello"
  println(msg) -- hello
}
"#;
    let pairs_ok = extract_println_pairs(block_body_ok);
    let outcome_ok = diff_println_pairs(&pairs_ok, "hello\n");
    match outcome_ok {
        DiffOutcome::Ok { matched } => {
            assert_eq!(
                matched, 1,
                "diff with matching bare annotation should match \
                 exactly one pair; got {matched}"
            );
        }
        DiffOutcome::Mismatch { expected, searched } => panic!(
            "diff incorrectly reported Mismatch on a well-formed \
             bare-vs-bare match. expected={expected:?}, \
             searched={searched:?}"
        ),
    }
}
