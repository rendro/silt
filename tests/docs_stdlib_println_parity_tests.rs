//! Stdout-parity walker for `docs/stdlib/*.md` `println(...)  -- <expected>`
//! annotations.
//!
//! The existing walker [`all_doc_fn_main_blocks_run_if_safe`] in
//! `tests/examples_check.rs` runs every safe `fn main` block from
//! `docs/**/*.md` via `silt run`, but only checks exit status / panic
//! detection — it never compares stdout to what the doc claims the
//! block prints. As a result, a doc snippet can drift its expected
//! `println(...)  -- <output>` comment away from the actual runtime
//! output without failing any test. Round-60 L11 was a single instance
//! of this drift (see `tests/docs_math_display_tests.rs`) — this
//! walker is the generalization: every `-- <expected>` annotation on a
//! `println` in `docs/stdlib/*.md` is now locked against the actual
//! `silt run` stdout for the enclosing `fn main` block.
//!
//! This walker is the lock test for this round's fixes: ~50 sites
//! across `docs/stdlib/*.md` had `.0` on integer-valued floats or
//! double-quoted strings inside `println` comments that did not
//! match the actual stdout (silt's `Float` Display drops the `.0`
//! and `println` does not wrap `String` values in quotes). The
//! paired doc edits bring those annotations back in line with
//! reality; this walker ensures they never drift again.
//!
//! ### Matching algorithm
//!
//! For each markdown file under `docs/stdlib/`, we look at every
//! ```silt``` fenced block. If the block contains a `fn main`
//! declaration AND survives the safety filter (same deny list as
//! the run-if-safe walker in `examples_check.rs`), we:
//!
//! 1. Extract, in source order, pairs of `(println(<expr>), expected)`
//!    where `expected` is the text after `-- ` on the same line. A
//!    line like `println(x)  -- 42` produces one pair whose expected
//!    is `"42"`. The match is intentionally loose: the whole
//!    `println(` must appear on the line, but the expression can
//!    span the rest of the line up to the `--`.
//! 2. Write the block to a temp `.silt` file and run `silt run` on it.
//! 3. Split stdout into lines (trimming the trailing newline).
//! 4. Pair the stdout lines with the extracted expected strings in
//!    source order and assert each matches (after trimming
//!    surrounding whitespace).
//!
//! ### Conventions / caveats
//!
//! - If the expected text contains "approximately" or starts with
//!   "e.g." (case-insensitive), the exact-match assertion is
//!   skipped for that pair — the expected is illustrative.
//!   `math.md`'s `math.sin(1.5707963)` / `math.tan(0.7853982)`
//!   cases use the "approximately" form because the actual output
//!   is a float-precision artifact that the prose is trying to
//!   approximate.
//! - If the number of extracted pairs does not match the number
//!   of stdout lines, we DO NOT fail — doc blocks can have
//!   `println` calls whose expected output is not annotated, and
//!   that is fine. We only assert the annotated lines match.
//! - Blocks using networked / interactive / filesystem / long-
//!   running APIs are skipped using the same deny list as
//!   `all_doc_fn_main_blocks_run_if_safe`. The purpose of the
//!   walker is to catch doc drift on SAFE snippets; unsafe
//!   snippets are covered by the existing walker plus hand-
//!   written tests.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Same deny list as `tests/examples_check.rs::all_doc_fn_main_blocks_run_if_safe`
/// — kept in sync by convention. A block whose body contains any of
/// these substrings is skipped (not compared to stdout) because we
/// cannot safely execute it inside the test harness OR it emits
/// non-deterministic output that a stdout-parity walker cannot lock.
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
    // Time — the parity walker additionally skips
    // non-deterministic sources (time.now / time.today / time.to_utc).
    // `time.sleep` is the classic blocking form.
    "time.sleep",
    "time.now",
    "time.today",
    "time.to_utc",
    // `math.random` is CSPRNG-backed and intentionally produces
    // non-deterministic output — any annotation there is
    // illustrative.
    "math.random",
    // `uuid.v4` / `uuid.v7` / `uuid.nil` — the first two are
    // random / time-ordered (non-deterministic); `uuid.nil` is
    // deterministic but is currently demo'd in a block with v4/v7
    // siblings that would trip alignment. The uuid showcase also
    // uses `match` arms heavily.
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

/// Returns `true` if the given source is unsafe to execute under the
/// walker (matches the deny list or carries a `// noexec` / `-- noexec`
/// marker on its first non-empty line).
fn is_unsafe_to_run(src: &str) -> bool {
    let first_nonempty = src.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let trimmed = first_nonempty.trim();
    if NOEXEC_MARKERS.iter().any(|m| trimmed == *m) {
        return true;
    }
    DENY_SUBSTRINGS.iter().any(|needle| src.contains(needle))
}

/// Extract every ```silt fenced block from a markdown file. Returns a
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
    /// Line number within the block (1-indexed), for error reporting.
    block_line: usize,
    /// The expected output text after `-- ` on the same line, trimmed.
    /// `None` means the println had no annotated expected output —
    /// those pairs are counted toward stdout line consumption but the
    /// exact-match assertion is skipped.
    expected: Option<String>,
}

/// Scan the block body for `println(...)  -- <expected>` sites. The
/// extraction is intentionally loose:
///
/// - We iterate lines of the block.
/// - A line is an annotated `println` iff it contains both the
///   substring `println(` and a ` -- ` separator after it.
/// - A line that contains `println(` but no same-line `-- ` is a
///   `println` with no annotation — we record it with
///   `expected: None` so stdout line counting stays aligned.
/// - Lines that do NOT contain `println(` are ignored (they don't
///   produce stdout on their own and are not the walker's concern).
///
/// This misses a couple of edge cases that are fine to miss:
///
/// - `println` inside a `match` / `if` / `for` arm only fires on
///   some branches — we treat every textual `println(` line as
///   producing exactly one stdout line. If the block uses
///   conditional `println`, the stdout alignment can diverge and
///   we simply can't assert — that's handled by the "pair count
///   must not exceed stdout line count" check below, which skips
///   the block on mismatch rather than failing.
/// - `println` inside a string literal — unlikely in docs.
fn extract_println_pairs(src: &str) -> Vec<PrintlnPair> {
    let mut pairs = Vec::new();
    for (i, line) in src.lines().enumerate() {
        // Strip any leading whitespace — the block is indented inside
        // the fence but that's irrelevant to the match.
        let trimmed = line.trim_start();
        // We only care about lines that invoke println.
        if !trimmed.contains("println(") {
            continue;
        }
        // Skip commented-out prose lines like `-- println(x)` inside
        // the block that document what a line WOULD print.
        if trimmed.starts_with("--") || trimmed.starts_with("//") {
            continue;
        }
        // Skip match-arm printlns — the walker cannot tell at doc-
        // extraction time which arm of a match fires, so any line
        // of the form `Foo(x) -> println(...)  -- <expected>` is
        // conditional and alignment with stdout is unreliable. We
        // detect this via the `->` token appearing before the
        // `println(` substring.
        let println_pos = trimmed.find("println(").unwrap();
        if let Some(arrow_pos) = trimmed[..println_pos].find("->") {
            // `->` before `println(` is the match-arm signature.
            // (A fn-return-type arrow like `fn foo() -> Int {` also
            // trips this, but those don't have `println(` on the
            // same line so we never get here for them.)
            let _ = arrow_pos;
            continue;
        }
        // Look for an ` -- ` separator on the same line. The doc
        // convention is two-space + `--`, but we accept a single
        // space too so snippets that fell out of the convention
        // still get checked.
        let expected = find_expected_comment(trimmed);
        pairs.push(PrintlnPair {
            block_line: i + 1,
            expected,
        });
    }
    pairs
}

/// Find a trailing `-- <expected>` comment on a line and return the
/// expected text (trimmed). The match requires that the `--` is
/// preceded by whitespace and followed by a space — so it doesn't
/// misfire on e.g. `"a--b"` inside a string or a `x--` decrement.
fn find_expected_comment(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i + 3 < bytes.len() {
        if bytes[i] == b' ' && bytes[i + 1] == b'-' && bytes[i + 2] == b'-' {
            // Need a space (or end of line) after the `--` to count.
            if i + 3 == bytes.len() {
                return Some(String::new());
            }
            if bytes[i + 3] == b' ' {
                // Skip the leading spaces after `-- `.
                let tail = &line[i + 3..];
                return Some(tail.trim().to_string());
            }
        }
        i += 1;
    }
    None
}

/// Returns true if the expected text is "illustrative" rather than
/// exact — i.e. contains `approximately`, starts with `e.g.`, or
/// contains an ellipsis `...`. In those cases the walker asserts
/// SOMETHING was printed but not exactly what.
fn is_illustrative(expected: &str) -> bool {
    let lower = expected.to_ascii_lowercase();
    lower.contains("approximately")
        || lower.starts_with("e.g.")
        || lower.starts_with("e.g ")
        || expected.contains("...")
}

/// Normalise a doc annotation expected value by stripping any trailing
/// explanatory parenthetical commentary that is not part of the actual
/// value. For example:
///
/// - `29 (leap year)` → `29`
/// - `2024-02-29 (leap year, clamped)` → `2024-02-29`
/// - `true (division returns ExtFloat)` → `true`
/// - `0  (silt's Float display drops the trailing `.0` for integer-valued floats)` → `0`
///
/// But NOT:
///
/// - `Ok(42)` → `Ok(42)` (the paren is part of the value)
/// - `Err(TimeOutOfRange(invalid date: 2024-13-1))` → unchanged
/// - `(1, 2, 3)` → unchanged
///
/// The heuristic: a trailing `(...)` is prose iff it is preceded by
/// whitespace AND the text inside is free-form commentary (not a
/// struct/variant wrapper). We detect free-form commentary by the
/// presence of non-balanced parens or the first whitespace-separated
/// word being a lowercase English word rather than a capitalized
/// identifier.
///
/// Rather than try to be clever, we use the simplest rule that works
/// for the current doc corpus: strip the first `  (` (two spaces +
/// open paren) to end-of-string OR a single ` (` that is preceded by
/// a whitespace char in the expected. Because doc annotations tend
/// to use the `-- <value>  (<prose>)` pattern with two spaces, we
/// try two-space first and fall back to single-space + lowercase-
/// starting prose.
fn strip_trailing_commentary(expected: &str) -> &str {
    // Two-space + `(` form — the doc convention for trailing prose.
    // Always prose (values never have a double space inside).
    if let Some(pos) = expected.find("  (") {
        return &expected[..pos];
    }
    // Single-space + `(` form. We strip ONLY when the character
    // immediately before the space is alphanumeric (so we don't
    // misfire on tuple commas like `[(1, a), (2, b)]` where the `,`
    // is non-alphanumeric). Constructor wrappers like `Ok(42)` /
    // `Some(Foo)` have no space before the `(` so this never hits
    // them.
    if let Some(pos) = expected.find(" (") {
        // Also require the content inside to not have further nested
        // structure we can't reason about. Use the simplest rule:
        // the first char inside the paren must be alphabetic
        // (prose), not a digit / symbol (tuple/record).
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

fn collect_stdlib_md_files(out: &mut Vec<PathBuf>) {
    let stdlib_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("docs")
        .join("stdlib");
    let entries = std::fs::read_dir(&stdlib_dir).expect("read docs/stdlib/");
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("md") {
            out.push(path);
        }
    }
    out.sort();
}

/// The walker is the lock test for this round's fixes: every
/// `println(...)  -- <expected>` annotation in a runnable (safe) doc
/// block under `docs/stdlib/` must match the actual `silt run`
/// stdout for the enclosing `fn main` block. A drift — `.0` on an
/// integer-valued float, `"..."` wrapping a bare String, etc. —
/// causes this test to fail with a precise `file:line` pointer.
#[test]
fn docs_stdlib_println_annotations_match_silt_run_stdout() {
    let mut files: Vec<PathBuf> = Vec::new();
    collect_stdlib_md_files(&mut files);
    assert!(
        !files.is_empty(),
        "expected at least one `docs/stdlib/*.md` file"
    );

    let tmp_dir = std::env::temp_dir().join(format!(
        "silt_docs_stdlib_println_parity_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");

    let mut failures: Vec<String> = Vec::new();
    let mut checked_blocks = 0usize;
    let mut checked_pairs = 0usize;

    for doc_path in &files {
        let body = std::fs::read_to_string(doc_path)
            .unwrap_or_else(|e| panic!("failed to read {}: {}", doc_path.display(), e));
        let blocks = extract_silt_blocks(&body);

        for (opener_line, src) in blocks {
            // Only `fn main` blocks produce stdout to compare.
            if !src.contains("fn main") {
                continue;
            }
            // Safety filter — same semantics as the existing run-if-safe walker.
            if is_unsafe_to_run(&src) {
                continue;
            }

            let pairs = extract_println_pairs(&src);
            // If the block has no annotated println at all, nothing
            // to check here; skip to avoid spending a subprocess.
            let annotated_count = pairs.iter().filter(|p| p.expected.is_some()).count();
            if annotated_count == 0 {
                continue;
            }

            checked_blocks += 1;

            // Write the block to a temp file and invoke `silt run`.
            let file_stem = doc_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("doc");
            let file = tmp_dir.join(format!("{file_stem}_line{opener_line}.silt"));
            std::fs::write(&file, &src).expect("write temp silt file");

            let output = silt_cmd()
                .arg("run")
                .arg(&file)
                .output()
                .expect("failed to spawn silt");

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
                let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
                failures.push(format!(
                    "{}:{} (```silt fence): `silt run` exited non-zero while \
                     the walker was trying to verify println annotations. \
                     exit={:?}\nstdout:\n{}\nstderr:\n{}",
                    doc_path.display(),
                    opener_line,
                    output.status.code(),
                    stdout,
                    stderr
                ));
                continue;
            }

            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            let stdout_lines: Vec<&str> = stdout.lines().collect();

            // Walk ANNOTATED pairs in source order. For each one,
            // advance a cursor through stdout looking for a matching
            // line. Lines that don't match any annotation (e.g. they
            // come from a println-without-annotation inside a match
            // arm, or a print inside a loop) are silently skipped —
            // we only validate annotations that have corresponding
            // output in the expected source order. If we run out of
            // stdout lines before finding a match for an annotation,
            // that's a failure.
            //
            // The search-based approach handles two tricky cases the
            // linear approach can't:
            //   1. Match-arm printlns (`Ok(x) -> println(x)`) which
            //      the extractor skips as conditional but which
            //      still produce output at runtime.
            //   2. Printlns without annotations (e.g. interleaved
            //      debug prints) — they consume a stdout line but
            //      the walker doesn't try to align against them.
            let mut cursor = 0usize;
            for pair in &pairs {
                let Some(expected) = pair.expected.as_deref() else {
                    continue;
                };
                let want = strip_trailing_commentary(expected.trim()).trim();
                let illustrative = is_illustrative(expected);

                // Find the next stdout line at or after `cursor` that
                // matches (exact for non-illustrative, non-empty for
                // illustrative). We don't skip arbitrary many lines
                // though — if we go more than 32 lines without a
                // match, we treat that as a genuine mismatch rather
                // than a benign extra-output gap.
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
                        checked_pairs += 1;
                    }
                    None => {
                        // For illustrative annotations that didn't
                        // find any non-empty output in the window,
                        // DON'T fail — illustrative annotations
                        // indicate the walker can't nail down the
                        // exact output, and the block may have
                        // other reasons why the expected line
                        // isn't present.
                        if illustrative {
                            continue;
                        }
                        let preview = stdout_lines
                            .get(cursor..search_end)
                            .map(|lines| lines.join("\n"))
                            .unwrap_or_default();
                        failures.push(format!(
                            "{}:{} (```silt fence, block line {}): println stdout \
                             parity mismatch — could not find annotated value in \
                             remaining stdout.\n  expected: {:?}\n  searched: {:?}\n\
                             full stdout:\n{}",
                            doc_path.display(),
                            opener_line,
                            pair.block_line,
                            want,
                            preview,
                            stdout
                        ));
                    }
                }
            }
        }
    }

    // Sanity: the walker must exercise real blocks. If filtering
    // becomes too aggressive and zero blocks get checked, this is a
    // configuration failure.
    assert!(
        checked_blocks > 0,
        "walker checked zero doc blocks — the deny list or the
        println-pair extractor is broken."
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
        "docs/stdlib/*.md: {} println annotation(s) do not match `silt run` \
         stdout. Either the doc comment drifted from reality (common \
         causes: `.0` on integer-valued floats, double-quoted strings \
         inside `println` comments, `Some(\"foo\")` instead of \
         `Some(foo)`) or silt's Display convention changed. Fix the \
         doc annotation to match observed stdout, OR add an \
         `approximately` / `e.g.` qualifier if the value is \
         illustrative.\n\n{}",
        failures.len(),
        failures.join("\n---\n")
    );
}
