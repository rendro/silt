//! Stdout-parity walker for `println(...)  -- <expected>` annotations
//! inside the markdown doc files (`docs/**/*.md`).
//!
//! `tests/lang/examples_check.rs::all_doc_fn_main_blocks_run_if_safe`
//! compiles and runs the markdown ```silt fences but does not compare
//! stdout with the `-- <expected>` annotations; this walker does, for
//! the stdlib reference pages (`docs/stdlib/*.md`, which are also what
//! an editor shows for a builtin name) as for every other page.
//!
//! ## What this walker does
//!
//! 1. Recursively walks every `docs/**/*.md`.
//! 2. Extracts each ```silt fence. A fence with `fn main` is a whole
//!    program. A fence without one that has an annotated `println` is a
//!    fragment: it is wrapped in `fn main() { ... }`, with an `import`
//!    of each builtin module it names, and taken when that program
//!    checks (a fragment that uses the names of an earlier fence does
//!    not, and cannot be run on its own).
//! 3. For each program, parses every line containing `println(` looking
//!    for an `-- <expected>` trailing annotation.
//! 4. Writes the program to a directory of its own and runs `silt run`
//!    there (files it writes and reads stay in that directory; an
//!    environment variable it sets is its process's), capturing stdout,
//!    with a time limit.
//! 5. Walks stdout in source order, locating each annotated expected
//!    value within a small skip window so unannotated `println` calls
//!    or branch-only output don't break alignment.
//! 6. Fails loudly with a precise `<file>:<fence-line> [block-line N]`
//!    pointer plus the expected vs. observed text.
//!
//! ## Skip rules
//!
//! A fence is not run when it reaches the network or a database, reads
//! stdin, ends the process, loops for ever, or prints random values (the
//! deny list below). Files, the environment, tasks, channels, streams
//! and the clock are run: a value that depends on the clock or on timing
//! is written as an example (`-- e.g. 100.304408ms`), see below.
//!
//! Per-block opt-out is supported via:
//!   - First non-empty line equal to `// noexec` or `-- noexec`
//!     (matches `examples_check.rs`).
//!
//! ## Illustrative annotations
//!
//! Annotations that contain `approximately`, start with `e.g.`, or
//! contain `...` are treated as illustrative: the corresponding println
//! must still have printed a line that is not empty, which is not
//! compared.

use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// What a fence that is run must not do: reach the network or a
/// database, read stdin, end the process, loop for ever, or print
/// random values.
const DENY_SUBSTRINGS: &[&str] = &[
    // Networked.
    "http.get",
    "http.post",
    "http.put",
    "http.delete",
    "http.request",
    "http.serve",
    "http.Server",
    "tcp.connect",
    "tcp.listen",
    "tcp.accept",
    "stream.tcp_",
    "stream.write_to_tcp",
    "postgres.",
    // Interactive IO.
    "io.read_line",
    "io.read_stdin",
    "io.stdin",
    "stream.stdin_lines",
    "read_line",
    // Random sources.
    "math.random",
    "crypto.random_bytes",
    "uuid.v4",
    "uuid.v7",
    // Infinite loops.
    "loop {",
    "while true",
    // Process exit.
    "process.exit",
];

const NOEXEC_MARKERS: &[&str] = &["// noexec", "-- noexec"];

/// How long one run of a fence may take.
const RUN_LIMIT: Duration = Duration::from_secs(60);

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

/// Decide whether a block should be excluded from the stdout-diff
/// walker.
fn is_unsafe_to_run(src: &str) -> bool {
    let first_nonempty = src.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let trimmed = first_nonempty.trim();
    if NOEXEC_MARKERS.contains(&trimmed) {
        return true;
    }
    DENY_SUBSTRINGS.iter().any(|needle| src.contains(needle))
}

/// Whether `src` names the module `module` (`list.map`), not as a part of
/// a longer name or of a string.
fn names_module(src: &str, module: &str) -> bool {
    let needle = format!("{module}.");
    let mut from = 0;
    while let Some(at) = src[from..].find(&needle) {
        let at = from + at;
        let before = src[..at].chars().next_back();
        let after = src[at + needle.len()..].chars().next();
        let starts_a_name = !before.is_some_and(|c| c.is_alphanumeric() || "_.\"".contains(c));
        if starts_a_name && after.is_some_and(|c| c.is_ascii_alphabetic()) {
            return true;
        }
        from = at + needle.len();
    }
    false
}

/// A fragment as a program: its `import` lines, an `import` of each
/// builtin module it names and does not import, and the rest of it as
/// the body of `main`.
fn wrap_fragment(src: &str) -> String {
    let (imports, body): (Vec<&str>, Vec<&str>) =
        src.lines().partition(|l| l.starts_with("import "));
    let mut out = String::new();
    for line in &imports {
        out.push_str(line);
        out.push('\n');
    }
    for module in &silt::builtins::registry::registry().modules {
        let imported = imports.iter().any(|l| {
            l.strip_prefix("import ")
                .and_then(|rest| rest.strip_prefix(module.name))
                .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '.']))
        });
        if !imported && names_module(src, module.name) {
            out.push_str(&format!("import {}\n", module.name));
        }
    }
    out.push_str("\nfn main() {\n");
    for line in body {
        out.push_str("  ");
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("}\n");
    out
}

/// The output of `silt <args> <file>` run in the directory of `file`, or
/// `None` when it took longer than [`RUN_LIMIT`] (and was ended).
fn run_in_dir(subcommand: &str, file: &Path) -> Option<std::process::Output> {
    let mut child = silt_cmd()
        .arg(subcommand)
        .arg(file.file_name().expect("a file name"))
        .current_dir(file.parent().expect("a directory"))
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn silt");
    let mut stdout = child.stdout.take().expect("stdout");
    let mut stderr = child.stderr.take().expect("stderr");
    let out_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let err_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait for silt") {
            break Some(status);
        }
        if start.elapsed() > RUN_LIMIT {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let stdout = out_reader.join().expect("stdout reader");
    let stderr = err_reader.join().expect("stderr reader");
    status.map(|status| std::process::Output {
        status,
        stdout,
        stderr,
    })
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
/// Mirrors `extract_silt_blocks` in `tests/lang/examples_check.rs` so the
/// two walkers see the same fences.
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
        // A `println` in a match arm or a closure (`Ok(v) -> println(v)
        // -- 1`) is compared like any other; one in a one-line function's
        // header and body is not (its function may never be called).
        if trimmed.starts_with("fn ") || trimmed.starts_with("pub fn ") {
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
/// timestamps, hashes).
fn is_illustrative(expected: &str) -> bool {
    let lower = expected.to_ascii_lowercase();
    lower.contains("approximately")
        || lower.starts_with("e.g.")
        || lower.starts_with("e.g ")
        || expected.contains("...")
}

/// The message of an annotation that says the line ends the program
/// with an error: `error: float overflow`, `runtime error: ...`.
fn error_annotation(expected: &str) -> Option<&str> {
    (expected.strip_prefix("runtime error: "))
        .or_else(|| expected.strip_prefix("error: "))
        .map(str::trim)
}

/// Strip parenthesised commentary from the expected, e.g.
/// `42 (the answer)` -> `42`.
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
        for (idx, got) in stdout_lines
            .iter()
            .enumerate()
            .take(search_end)
            .skip(cursor)
        {
            let got = got.trim_end();
            if illustrative {
                if !got.trim().is_empty() {
                    found_at = Some(idx);
                    break;
                }
            } else if got == want || got == expected.trim() {
                // The comment as written, or without a remark in
                // parentheses after it (`42 (the answer)`).
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

    // Universe: every .md under docs/. README.md is left out: its
    // annotations are narrative ("record update", "the answer").
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
        /// Which of the runs of a fence with several `-- error:` lines.
        variant: usize,
        /// The program run: the fence, or a fragment wrapped in `main`.
        src: String,
        /// Whether `src` is a wrapped fragment, which is run only when it
        /// checks.
        fragment: bool,
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

            let fragment = !src.contains("fn main");
            let program = if fragment { wrap_fragment(&src) } else { src };
            let pairs = extract_println_pairs(&program);
            let annotated_count = pairs.iter().filter(|p| p.expected.is_some()).count();
            if annotated_count == 0 {
                continue;
            }

            let file_stem = doc_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("doc")
                .to_string();
            // A line annotated `-- error: ...` ends the program, so a fence
            // with several is run once for each: without the error lines
            // before it, which are statements of their own.
            let errors: Vec<usize> = (pairs.iter().enumerate())
                .filter(|(_, p)| p.expected.as_deref().and_then(error_annotation).is_some())
                .map(|(i, _)| i)
                .collect();
            for variant in 0..errors.len().max(1) {
                let dropped: Vec<usize> = errors[..variant].to_vec();
                let src: String = (program.lines().enumerate())
                    .map(|(i, line)| {
                        let gone = dropped.iter().any(|&d| pairs[d].block_line == i + 1);
                        if gone {
                            String::new()
                        } else {
                            line.to_string()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
                    + "\n";
                let kept: Vec<PrintlnPair> = (pairs.iter().enumerate())
                    .filter(|(i, _)| !dropped.contains(i))
                    .map(|(_, p)| PrintlnPair {
                        block_line: p.block_line,
                        expected: p.expected.clone(),
                    })
                    .collect();
                jobs.push(Job {
                    doc_path: doc_path.clone(),
                    opener_line,
                    variant,
                    src,
                    fragment,
                    file_stem: file_stem.clone(),
                    pairs: kept,
                });
            }
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
    let not_alone = AtomicUsize::new(0);
    let next_job = AtomicUsize::new(0);
    let jobs_ref = &jobs;
    let tmp_dir_ref = &tmp_dir;
    let failures_ref = &failures;
    let checked_pairs_ref = &checked_pairs;
    let not_alone_ref = &not_alone;
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
                    // Each fence runs in a directory of its own, so the
                    // files it writes meet no other fence's. (A doc's
                    // name such as `channel-task` is no module name: `-`
                    // becomes `_`.)
                    let name = format!(
                        "{}_line{}_{}",
                        job.file_stem.to_lowercase().replace('-', "_"),
                        job.opener_line,
                        job.variant
                    );
                    let dir = tmp_dir_ref.join(&name);
                    std::fs::create_dir_all(&dir).expect("create the fence's directory");
                    let file = dir.join(format!("{name}.silt"));
                    std::fs::write(&file, &job.src).expect("write temp silt file");
                    let place = format!("{}:{}", job.doc_path.display(), job.opener_line);

                    // A fragment that does not stand alone (it uses the
                    // names of an earlier fence) cannot be run.
                    if job.fragment {
                        match run_in_dir("check", &file) {
                            Some(out) if out.status.success() => {}
                            _ => {
                                not_alone_ref.fetch_add(1, Ordering::SeqCst);
                                continue;
                            }
                        }
                    }

                    let Some(output) = run_in_dir("run", &file) else {
                        failures_ref.lock().unwrap().push(format!(
                            "{place} (```silt fence): `silt run` took longer than \
                             {RUN_LIMIT:?} and was ended"
                        ));
                        continue;
                    };

                    // A line annotated `-- error: <message>` (or `--
                    // runtime error: ...`) ends the program: the lines
                    // before it are compared with stdout, and the message
                    // must be that of the error the run ends with. The
                    // lines after it are run without it (a run of their own).
                    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
                    let first_error = job
                        .pairs
                        .iter()
                        .position(|p| p.expected.as_deref().and_then(error_annotation).is_some());
                    let pairs = match first_error {
                        Some(k) => {
                            let message = (job.pairs[k].expected.as_deref())
                                .and_then(error_annotation)
                                .unwrap_or_default();
                            // The message is that of the diagnostic's
                            // header, `error[runtime]: <message>`, not of
                            // the source line it echoes with its comment.
                            let reported = stderr.lines().any(|l| {
                                l.trim_start().starts_with("error") && l.contains(message)
                            });
                            if output.status.success() || !reported {
                                failures_ref.lock().unwrap().push(format!(
                                    "{place} (```silt fence, line {} of the program run): \
                                     expected the run to end with the error `{message}`; \
                                     exit={:?}\nstderr:\n{stderr}",
                                    job.pairs[k].block_line,
                                    output.status.code(),
                                ));
                                continue;
                            }
                            &job.pairs[..k]
                        }
                        None => &job.pairs[..],
                    };

                    if first_error.is_none() && !output.status.success() {
                        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
                        failures_ref.lock().unwrap().push(format!(
                            "{place} (```silt fence{}): `silt run` exited non-zero \
                             while the walker was trying to verify println \
                             annotations. exit={:?}\nstdout:\n{}\nstderr:\n{}",
                            if job.fragment {
                                ", wrapped in main"
                            } else {
                                ""
                            },
                            output.status.code(),
                            stdout,
                            stderr
                        ));
                        continue;
                    }

                    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
                    match diff_println_pairs(pairs, &stdout) {
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
                                "{place} (```silt fence{}, line {} of the program run): \
                                 expected `{}` but got mismatch — could not find \
                                 annotated value in remaining stdout.\n  \
                                 searched: {:?}\n\
                                 full stdout:\n{}",
                                if job.fragment { " wrapped in main" } else { "" },
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
    let not_alone = not_alone.load(Ordering::SeqCst);

    eprintln!(
        "[docs_markdown_println_parity] compared {checked_pairs} annotated println \
         line(s) in {checked_blocks} fence(s); {not_alone} fragment(s) that do not \
         check on their own were not run"
    );
    // The fences of docs/ hold hundreds of annotated println lines; a
    // run that compared none has lost its way (an extraction or a deny
    // rule that matches everything).
    assert!(
        checked_pairs >= 300,
        "the walker compared only {checked_pairs} annotated println line(s) \
         in {checked_blocks} fence(s); expected hundreds"
    );

    if failures.is_empty() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    assert!(
        failures.is_empty(),
        "{} println annotation(s) in markdown docs do not match `silt run` \
         stdout. Either the doc comment drifted from reality (common \
         causes: a whole Float written without its `.0`, a string inside a \
         structure written bare, `Some(foo)` instead of `Some(\"foo\")`, or a \
         string printed by itself written in quotes) or silt's Display \
         convention changed. Fix the doc \
         annotation in the corresponding `.md` file to match observed \
         stdout, OR add an `approximately` / `e.g.` qualifier if the \
         value is illustrative.\n\n{}",
        failures.len(),
        failures.join("\n---\n")
    );
}

/// A fragment is wrapped in `main` with the imports it needs: the
/// modules it names and does not import, and none it names only inside
/// a longer name or a string.
#[test]
fn a_fragment_is_wrapped_with_the_imports_it_needs() {
    let wrapped = wrap_fragment(
        "import list.{ map }\nprintln(http.segments(\"/a/b\")) -- [\"a\", \"b\"]\nlet s = \"io.x\"\n",
    );
    assert_eq!(
        wrapped,
        "import list.{ map }\nimport http\n\nfn main() {\n  \
         println(http.segments(\"/a/b\")) -- [\"a\", \"b\"]\n  let s = \"io.x\"\n}\n"
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
    let annotated: Vec<&PrintlnPair> = pairs.iter().filter(|p| p.expected.is_some()).collect();
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
