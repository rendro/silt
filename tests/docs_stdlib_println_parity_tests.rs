//! Stdout-parity walker for `println(...)  -- <expected>` annotations
//! inside builtin doc strings.
//!
//! Round 62 phase-2 moved every `docs/stdlib/*.md` file's prose into
//! per-name doc strings registered alongside the type signatures in
//! `src/typechecker/builtins/`. The walker no longer scans markdown
//! files on disk; it pulls the registered builtin docs via
//! `silt::typechecker::iter_builtin_docs()` and applies the same
//! `\`\`\`silt` fence + `println(...) -- expected` extraction
//! against `silt run` stdout.
//!
//! The lock semantics are unchanged: every `-- <expected>` annotation
//! on a `println` inside a runnable (safe) doc snippet is locked
//! against actual stdout. A drift — `.0` on an integer-valued float,
//! `"..."` wrapping a bare String, etc. — causes this test to fail
//! with a precise `name:block-line` pointer.
//!
//! ### Matching algorithm
//!
//! For each `(name, doc)` pair from `iter_builtin_docs()` we look at
//! every ```silt``` fenced block. If the block contains a `fn main`
//! declaration AND survives the safety filter (same deny list as
//! the run-if-safe walker in `examples_check.rs`), we:
//!
//! 1. Extract, in source order, pairs of `(println(<expr>), expected)`
//!    where `expected` is the text after `-- ` on the same line.
//! 2. Write the block to a temp `.silt` file and run `silt run` on it.
//! 3. Walk through `silt run` stdout looking for each annotated
//!    expected value in order, allowing arbitrary intervening lines
//!    (within a 32-line skip window) so conditional/match-arm
//!    `println` calls don't break alignment.
//!
//! ### De-duplication
//!
//! Many doc strings share the same fenced block (e.g. every
//! `bytes.*` name gets the module overview, which has one big
//! example). Without deduplication the walker would run the same
//! snippet many times. We hash each block source and skip
//! already-seen sources.
//!
//! ### Conventions / caveats
//!
//! - If the expected text contains "approximately" or starts with
//!   "e.g." (case-insensitive), the exact-match assertion is
//!   skipped for that pair — the expected is illustrative.
//! - If the number of extracted pairs does not match the number
//!   of stdout lines, we DO NOT fail — doc blocks can have
//!   `println` calls whose expected output is not annotated, and
//!   that is fine. We only assert the annotated lines match.
//! - Blocks using networked / interactive / filesystem / long-
//!   running APIs are skipped using the same deny list as
//!   `all_doc_fn_main_blocks_run_if_safe`.

use std::collections::HashSet;
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Same deny list as `tests/examples_check.rs::all_doc_fn_main_blocks_run_if_safe`.
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

fn is_unsafe_to_run(src: &str) -> bool {
    let first_nonempty = src.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let trimmed = first_nonempty.trim();
    if NOEXEC_MARKERS.iter().any(|m| trimmed == *m) {
        return true;
    }
    DENY_SUBSTRINGS.iter().any(|needle| src.contains(needle))
}

/// Extract every ```silt fenced block from a doc body. Returns a
/// vector of `(opener_line_number_1indexed, block_source)` tuples.
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
    block_line: usize,
    expected: Option<String>,
}

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
        if let Some(arrow_pos) = trimmed[..println_pos].find("->") {
            let _ = arrow_pos;
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

fn is_illustrative(expected: &str) -> bool {
    let lower = expected.to_ascii_lowercase();
    lower.contains("approximately")
        || lower.starts_with("e.g.")
        || lower.starts_with("e.g ")
        || expected.contains("...")
}

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

#[test]
fn docs_stdlib_println_annotations_match_silt_run_stdout() {
    let entries: Vec<(String, String)> = silt::typechecker::iter_builtin_docs();
    assert!(
        !entries.is_empty(),
        "expected at least one builtin doc entry — \
         silt::typechecker::iter_builtin_docs() returned nothing"
    );

    let tmp_dir = std::env::temp_dir().join(format!(
        "silt_docs_stdlib_println_parity_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");

    // Phase 1: walk every doc entry and collect the unique runnable
    // blocks with annotated println pairs. Deduplication and unsafe
    // filtering happen here so the parallel worker pool only sees real
    // work. Sequential because the dedup HashSet is cheap and the
    // per-block work is in phase 2.
    struct Job {
        name: String,
        opener_line: usize,
        src: String,
        stem: String,
        pairs: Vec<PrintlnPair>,
    }
    let mut jobs: Vec<Job> = Vec::new();
    let mut seen_blocks: HashSet<u64> = HashSet::new();

    for (name, body) in &entries {
        let blocks = extract_silt_blocks(body);
        for (opener_line, src) in blocks {
            if !src.contains("fn main") {
                continue;
            }
            if is_unsafe_to_run(&src) {
                continue;
            }

            // Dedup: many bindings share the same body (module-level
            // overview; multi-name headings). Hash the block source
            // and skip duplicates so the walker runs each unique
            // snippet exactly once.
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

            // Sanitise binding name for use as a temp filename.
            let stem: String = name
                .chars()
                .map(|c| if c.is_alphanumeric() { c } else { '_' })
                .collect();
            jobs.push(Job {
                name: name.clone(),
                opener_line,
                src,
                stem,
                pairs,
            });
        }
    }

    let checked_blocks = jobs.len();

    // Phase 2: parallel subprocess fan-out. Each subprocess spawn +
    // silt cold-start is ~50–200ms and dominates the test runtime
    // (~115s sequential). A worker pool drops the wall-clock to
    // (jobs / num_workers) × per-job-cost. We size the pool to
    // available CPUs but cap at 8 so we don't oversubscribe small
    // CI runners and don't starve the silt scheduler workers spawned
    // by each child process.
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
                    let file =
                        tmp_dir_ref.join(format!("{}_line{}.silt", job.stem, job.opener_line));
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
                            "doc[{}]:{} (```silt fence): `silt run` exited \
                             non-zero while the walker was trying to verify println \
                             annotations. exit={:?}\nstdout:\n{}\nstderr:\n{}",
                            job.name,
                            job.opener_line,
                            output.status.code(),
                            stdout,
                            stderr
                        ));
                        continue;
                    }

                    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
                    let stdout_lines: Vec<&str> = stdout.lines().collect();

                    let mut cursor = 0usize;
                    for pair in &job.pairs {
                        let Some(expected) = pair.expected.as_deref() else {
                            continue;
                        };
                        let want = strip_trailing_commentary(expected.trim()).trim();
                        let illustrative = is_illustrative(expected);

                        let max_skip = 32usize;
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
                                checked_pairs_ref.fetch_add(1, Ordering::SeqCst);
                            }
                            None => {
                                if illustrative {
                                    continue;
                                }
                                let preview = stdout_lines
                                    .get(cursor..search_end)
                                    .map(|lines| lines.join("\n"))
                                    .unwrap_or_default();
                                failures_ref.lock().unwrap().push(format!(
                                    "doc[{}]:{} (```silt fence, block line {}): \
                                     println stdout parity mismatch — could not find \
                                     annotated value in remaining stdout.\n  \
                                     expected: {:?}\n  searched: {:?}\n\
                                     full stdout:\n{}",
                                    job.name,
                                    job.opener_line,
                                    pair.block_line,
                                    want,
                                    preview,
                                    stdout
                                ));
                            }
                        }
                    }
                }
            });
        }
    });

    let failures: Vec<String> = failures.into_inner().unwrap();
    let checked_pairs = checked_pairs.load(Ordering::SeqCst);

    assert!(
        checked_blocks > 0,
        "walker checked zero builtin doc blocks — the deny list, the
        println-pair extractor, or the iter_builtin_docs() registry
        is broken."
    );
    assert!(
        checked_pairs > 0,
        "walker checked zero println/expected pairs — annotation
        extraction is broken."
    );

    if failures.is_empty() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    assert!(
        failures.is_empty(),
        "{} println annotation(s) in builtin docs do not match `silt run` \
         stdout. Either the doc comment drifted from reality (common \
         causes: `.0` on integer-valued floats, double-quoted strings \
         inside `println` comments, `Some(\"foo\")` instead of \
         `Some(foo)`) or silt's Display convention changed. Fix the \
         doc annotation in the corresponding `super::docs::*_MD` raw \
         string in `src/typechecker/builtins/docs.rs` to match observed \
         stdout, OR add an `approximately` / `e.g.` qualifier if the \
         value is illustrative.\n\n{}",
        failures.len(),
        failures.join("\n---\n")
    );
}

/// Coverage smoke test: every authoritative qualified builtin name
/// should have a registered, non-empty doc string. Round-62-style
/// "drift can't sneak in" lock — adding a new builtin without
/// inlining its section into the corresponding `*_MD` blob fails
/// this test.
///
/// "Authoritative" here is every qualified name (`module.func`)
/// returned by `silt::typechecker::builtin_type_signatures()`.
///
/// We skip:
///   - The bare type-descriptor binding names (`Int`, `Float`, ...) —
///     those don't have a `.` so they're not in the qualified-name
///     set this test walks; this comment is just for orientation.
#[test]
fn every_authoritative_builtin_has_a_non_empty_doc() {
    let docs = silt::typechecker::builtin_docs();
    let sigs = silt::typechecker::builtin_type_signatures();

    let mut missing: Vec<String> = Vec::new();
    let mut empty: Vec<String> = Vec::new();

    for name in sigs.keys() {
        match docs.get(name) {
            None => missing.push(name.clone()),
            Some(d) if d.trim().is_empty() => empty.push(name.clone()),
            Some(_) => {}
        }
    }

    if !missing.is_empty() || !empty.is_empty() {
        missing.sort();
        empty.sort();
        panic!(
            "builtin doc coverage drift detected.\n  \
             missing docs ({} names): {:?}\n  \
             empty docs ({} names): {:?}\n\n\
             Every authoritative qualified builtin name (e.g. `list.map`, \
             `math.cos`) must have a non-empty doc string registered. \
             To fix: add a matching `## \\`<name>\\`` section to the \
             corresponding `super::docs::*_MD` blob in \
             `src/typechecker/builtins/docs.rs`, OR call \
             `attach_module_overview` for module-level prose only.",
            missing.len(),
            missing,
            empty.len(),
            empty,
        );
    }
}
