//! Walk `examples/` and run `silt check` on every `*.silt` file.
//!
//! This pins the contract that every shipped example type-checks cleanly.
//! We intentionally only run `silt check` (not `silt run`) because some
//! examples are networked (http_server/http_client), interactive, or
//! long-running. Type-checking catches any API/syntax drift without
//! actually executing user code.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Pool size for parallel subprocess fan-out inside individual tests.
/// Cargo test already runs distinct tests in parallel, so we keep this
/// modest (≤8) to avoid oversubscribing CI runners and starving the silt
/// scheduler workers spawned by each child process. Subprocess spawn +
/// silt cold-start dominates these walkers, so even 4 workers gives a
/// large wall-clock reduction.
fn pool_size() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(2, 8)
}

/// Run `task` for each item in `items` across a small thread pool, in
/// parallel. The task is `Sync`-bound so it can borrow shared state by
/// reference; failures should be pushed into shared `Mutex`-protected
/// vecs from inside the closure.
fn par_for_each<T: Sync>(items: &[T], task: impl Fn(&T) + Sync) {
    let next = AtomicUsize::new(0);
    let task_ref = &task;
    let next_ref = &next;
    let n = items.len();
    std::thread::scope(|scope| {
        for _ in 0..pool_size() {
            scope.spawn(move || {
                loop {
                    let idx = next_ref.fetch_add(1, Ordering::SeqCst);
                    if idx >= n {
                        return;
                    }
                    task_ref(&items[idx]);
                }
            });
        }
    });
}

/// Files that are intentionally skipped. Keep this list empty unless a
/// concrete reason is documented inline.
const SKIP: &[&str] = &[
    // (none currently — add with a comment explaining why if needed)
];

/// Examples that are allowed to emit compile/type warnings under
/// `silt check`. Each entry MUST have a documented reason. This list is
/// consulted only by the warning-free half of `every_example_type_checks`;
/// every file in this list still has to type-check cleanly (exit 0).
///
/// The goal of the warning walker (round-16 GAP G6 lock) is to catch
/// *new* warnings introduced by future edits — not to force fixes on
/// pre-existing warnings that represent known limitations. Each entry
/// below is a deliberate exception; anything outside this list must be
/// warning-free or the test fails.
const WARN_ALLOWLIST: &[&str] = &[
    // The `match expr` blocks in `fn eval`, `fn simplify`, `fn depth`,
    // `fn node_count`, `fn to_rpn` and the trait `Display` for `Expr`
    // cover every `Expr` variant, but the type checker's pattern
    // exhaustiveness analysis hits a recursion-depth limit on the
    // recursive `Expr` type and emits a `warning[type]` regardless.
    // Adding `_ -> ...` arms does not silence the warning (verified
    // against line 230 which already has `_ -> "other"`). This is a
    // known type-checker limitation tracked in src/ — the example
    // itself is correct.
    "expr_eval.silt",
    // `let result = mymath.add(3, 4)` shadows the builtin `result`
    // module. The `result` binding is the pedagogically natural
    // variable name for a computation result, and this example is the
    // first thing a reader sees under examples/modules/. Renaming
    // would harm the teaching value; the warning is harmless.
    "main.silt",
    // `let result = matches |> list.fold(...)` shadows the builtin
    // `result` module. Same rationale as above — `result` is the
    // natural name for the fold's accumulator.
    "link_checker.silt",
    // `(_, Message(result)) -> { let (worker_id, outcome) = result }`
    // destructures the channel message payload into a binding named
    // `result`, which shadows the builtin `result` module. Renaming
    // inside a deep match arm would make the example harder to read.
    "concurrent_processor.silt",
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
fn every_example_type_checks_and_has_no_warnings() {
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

    let failures: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let warn_failures: Mutex<Vec<String>> = Mutex::new(Vec::new());

    par_for_each(&files, |file| {
        let name = file
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        if SKIP.contains(&name) {
            return;
        }

        let output = silt_cmd()
            .arg("check")
            .arg(file)
            .output()
            .expect("failed to spawn silt");

        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);

        if !output.status.success() {
            failures.lock().unwrap().push(format!(
                "{}: exit={:?}\nstdout:\n{}\nstderr:\n{}",
                file.display(),
                output.status.code(),
                stdout,
                stderr
            ));
            return;
        }

        // Round-16 GAP G6 lock: every example must also be
        // warning-free. This mirrors the companion doc-walker
        // `test_doc_fn_main_blocks_emit_no_compile_warnings` so a new
        // `warning[...]` on any example cannot ship silently.
        if WARN_ALLOWLIST.contains(&name) {
            return;
        }
        let warn_lines: Vec<&str> = stderr.lines().filter(|l| l.contains("warning[")).collect();
        if !warn_lines.is_empty() {
            warn_failures.lock().unwrap().push(format!(
                "{}: emitted {} warning line(s):\n{}",
                file.display(),
                warn_lines.len(),
                warn_lines.join("\n")
            ));
        }
    });

    let failures = failures.into_inner().unwrap();
    let warn_failures = warn_failures.into_inner().unwrap();

    assert!(
        failures.is_empty(),
        "silt check failed for {} example(s):\n\n{}",
        failures.len(),
        failures.join("\n---\n")
    );

    assert!(
        warn_failures.is_empty(),
        "silt check emitted warnings for {} example(s). A new warning \
         on any example would ship silently without this lock — fix the \
         underlying cause in the example, or (if it's a known type-checker \
         limitation) add the filename to `WARN_ALLOWLIST` in \
         tests/examples_check.rs with a documented reason.\n\n{}",
        warn_failures.len(),
        warn_failures.join("\n---\n")
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
                if let Some(&b) = tail.first()
                    && (b.is_ascii_alphanumeric() || b == b'_')
                {
                    continue;
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

    // Secondary matcher for bare `<digits> keyword(s)` patterns (no `+`
    // suffix). This closes the round-15 L4 gap: the getting-started and
    // bindings-and-functions docs used to say "14 keywords" which is
    // currently accurate but drifts whenever a keyword is added or
    // removed. The standing user direction is to remove drift-prone counts
    // entirely rather than re-correct them. We check the plural and
    // singular forms for both `keyword` and — just in case — `stdlib
    // function`.
    fn find_bare_keyword_count(haystack: &str) -> Option<String> {
        const BARE_NOUNS: &[&str] = &["keyword", "keywords"];
        for noun in BARE_NOUNS {
            for (idx, _) in haystack.match_indices(noun) {
                // Right-side word boundary check.
                let tail = &haystack.as_bytes()[idx + noun.len()..];
                if let Some(&b) = tail.first()
                    && (b.is_ascii_alphanumeric() || b == b'_')
                {
                    continue;
                }
                // Walk backwards over one or more spaces, then digits.
                let prefix = &haystack.as_bytes()[..idx];
                let mut i = prefix.len();
                let mut saw_space = false;
                while i > 0 && (prefix[i - 1] == b' ' || prefix[i - 1] == b'\t') {
                    saw_space = true;
                    i -= 1;
                }
                if !saw_space {
                    continue;
                }
                let mut digit_count = 0;
                while i > 0 && prefix[i - 1].is_ascii_digit() {
                    digit_count += 1;
                    i -= 1;
                }
                if digit_count == 0 {
                    continue;
                }
                // The char before the digits must not be alphanumeric
                // (avoids matching `v14 keywords` or `1.14 keywords`).
                if i > 0 {
                    let b = prefix[i - 1];
                    if b.is_ascii_alphanumeric() || b == b'_' || b == b'.' {
                        continue;
                    }
                }
                let snippet_end = idx + noun.len();
                return Some(haystack[i..snippet_end].to_string());
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
        if let Some(hit) = find_bare_keyword_count(&body) {
            failures.push(format!(
                "{} contains a drift-prone bare `<digits> keyword(s)` count: \
                 `{}`. Per the round-15 L4 audit fix, the convergent \
                 decision is to drop the count and enumerate the keywords \
                 (or use descriptive phrasing like 'a small, fixed keyword \
                 set') so the doc cannot drift when a keyword is added or \
                 removed.",
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

    let tmp_dir = std::env::temp_dir().join(format!("silt_all_doc_check_{}", std::process::id()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");

    // Phase 1: collect every (doc, opener_line, src, file_stem) job
    // sequentially so the parallel pool only handles per-block work.
    struct Job {
        doc_path: PathBuf,
        opener_line: usize,
        src: String,
        file_stem: String,
    }
    let mut jobs: Vec<Job> = Vec::new();
    for doc_path in &targets {
        let body = std::fs::read_to_string(doc_path)
            .unwrap_or_else(|e| panic!("failed to read {}: {}", doc_path.display(), e));
        let blocks = extract_silt_blocks(&body);
        for (opener_line, src) in blocks {
            if !src.contains("fn main") {
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
            });
        }
    }
    let runnable_block_count = jobs.len();

    let failures: Mutex<Vec<String>> = Mutex::new(Vec::new());

    par_for_each(&jobs, |job| {
        let file = tmp_dir.join(format!("{}_line{}.silt", job.file_stem, job.opener_line));
        std::fs::write(&file, &job.src).expect("write temp silt file");

        let output = silt_cmd()
            .arg("check")
            .arg(&file)
            .output()
            .expect("failed to spawn silt");
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            failures.lock().unwrap().push(format!(
                "{}:{} (```silt fence): exit={:?}\nstdout:\n{}\nstderr:\n{}",
                job.doc_path.display(),
                job.opener_line,
                output.status.code(),
                stdout,
                stderr
            ));
        }
    });

    let failures = failures.into_inner().unwrap();

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

/// Stronger sibling of `all_doc_fn_main_blocks_type_check` that also drives
/// the compile phase (lex → parse → typecheck → compile) on every
/// `fn main` block in README + docs/**/*.md.
///
/// Why: `silt check` only runs the type checker, which globally
/// preregisters every stdlib function, so doc examples pass the type check
/// even when they forget the matching `import <module>` line at the top.
/// Real users run `silt run`, which fails with
/// `module 'X' is not imported`. The compile phase (which `silt run`
/// always runs) catches that missing-import error deterministically
/// without having to execute the VM, which is important because some
/// doc examples are interactive (io.stdin.read_line), networked
/// (http.get/serve), or long-running (time.sleep, channel.recv).
///
/// This test drives the compile phase directly via the silt library API
/// so it never runs user code. Removing an `import` line from any doc
/// block that uses that module must make this test fail — that's the
/// lock we need so README/docs never silently drift away from runnable.
#[test]
fn all_doc_fn_main_blocks_compile() {
    use silt::compiler::Compiler;
    use silt::lexer::Lexer;
    use silt::parser::Parser;
    use silt::typechecker;

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

    // Give the compiler a stable project root for any `import "./foo"`
    // relative imports that might appear in doc blocks. None of the
    // current doc blocks use file-based imports, but we set a real
    // directory so the compiler never trips on a missing root.
    let project_root = manifest_dir.to_path_buf();

    let mut failures: Vec<String> = Vec::new();
    let mut runnable_block_count = 0usize;

    for doc_path in &targets {
        let body = std::fs::read_to_string(doc_path)
            .unwrap_or_else(|e| panic!("failed to read {}: {}", doc_path.display(), e));
        let blocks = extract_silt_blocks(&body);

        for (opener_line, src) in blocks {
            // Only full programs (containing `fn main`) are expected to
            // compile standalone. Snippet blocks are skipped.
            if !src.contains("fn main") {
                continue;
            }
            runnable_block_count += 1;

            // Reset the interner per block so one block's interned
            // strings can't leak into another's diagnostics.
            silt::intern::reset();

            // Drive the pipeline: lex → parse → typecheck → compile.
            // We deliberately stop after compile (no VM run) so
            // interactive / networked / long-running examples are safe.
            let tokens = match Lexer::new(&src).tokenize() {
                Ok(t) => t,
                Err(e) => {
                    failures.push(format!(
                        "{}:{} (```silt fence): lex error: {:?}",
                        doc_path.display(),
                        opener_line,
                        e
                    ));
                    continue;
                }
            };

            let (mut program, parse_errors) = Parser::new(tokens).parse_program_recovering();
            if !parse_errors.is_empty() {
                failures.push(format!(
                    "{}:{} (```silt fence): parse errors: {:?}",
                    doc_path.display(),
                    opener_line,
                    parse_errors
                ));
                continue;
            }

            // Typecheck (informational — hard type errors get reported
            // below alongside any compile error so the user sees
            // everything at once).
            let type_errors = typechecker::check(&mut program);
            let hard_type_errors: Vec<_> = type_errors
                .iter()
                .filter(|e| e.severity == typechecker::Severity::Error)
                .collect();

            // Compile. This is where the missing-import error surfaces.
            // Use the lower-level `with_package_roots` API directly with
            // a synthetic single-package map; doc blocks don't import
            // sibling .silt files, so the root is just here to satisfy
            // the compiler's invariant.
            let local_pkg = silt::intern::intern("__doc_block__");
            let mut roots = std::collections::HashMap::new();
            roots.insert(local_pkg, project_root.clone());
            let mut compiler = Compiler::with_package_roots(local_pkg, roots);
            match compiler.compile_program(&program) {
                Ok(_) => {
                    if !hard_type_errors.is_empty() {
                        failures.push(format!(
                            "{}:{} (```silt fence): type errors: {:?}",
                            doc_path.display(),
                            opener_line,
                            hard_type_errors
                        ));
                    }
                }
                Err(e) => {
                    failures.push(format!(
                        "{}:{} (```silt fence): compile error: {}",
                        doc_path.display(),
                        opener_line,
                        e.message
                    ));
                }
            }
        }
    }

    assert!(
        runnable_block_count > 0,
        "expected at least one runnable ```silt block (containing `fn main`) across all docs"
    );

    assert!(
        failures.is_empty(),
        "compile phase failed for {} ```silt block(s) across docs/ and README.md. \
         This almost always means a doc example forgot to `import <module>` \
         at the top of its `fn main` block. Fix by adding the missing imports \
         in the markdown source:\n\n{}",
        failures.len(),
        failures.join("\n---\n")
    );
}

/// Regression lock for the GAP audit: `docs/language/bindings-and-functions.md`
/// used to claim "only 7 names" always available in the global namespace,
/// but the type-checker actually preregisters 11 (the 7 original prelude
/// names plus the 4 primitive type descriptors `Int`/`Float`/`String`/`Bool`
/// used by `json.parse_map` and other type-directed APIs). The convergent
/// fix from prior audit rounds is to drop the numeric count entirely and
/// just list every name — so this test asserts (a) the outdated "only 7"
/// phrasing is gone and (b) every preregistered always-available name is
/// mentioned by the doc. If a future commit adds another always-available
/// name to `src/typechecker/builtins.rs` without updating this doc, the
/// test will fail with a clear pointer.
#[test]
fn bindings_and_functions_globals_list_matches_reality() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let doc_path = manifest_dir
        .join("docs")
        .join("language")
        .join("bindings-and-functions.md");
    let body = std::fs::read_to_string(&doc_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", doc_path.display(), e));

    // (a) The old "only 7 names" claim (or any "N names that are always
    //     available" phrasing with a count) must not come back. Any
    //     numeric count here is drift-prone; use a stable list instead.
    assert!(
        !body.contains("only 7 names"),
        "{}: the outdated 'only 7 names' phrasing came back. The global \
         namespace actually has 11 always-available names (7 prelude + 4 \
         type descriptors). Replace the count with a stable list of names.",
        doc_path.display()
    );
    // Lightweight guard against a resurrected "<digits> names that are
    // always available" pattern. Hand-rolled to avoid pulling in regex.
    let lower = body.to_ascii_lowercase();
    if let Some(pos) = lower.find(" names that are always available") {
        // Walk backwards over digits immediately before the match.
        let prefix = &lower.as_bytes()[..pos];
        let mut i = prefix.len();
        while i > 0 && prefix[i - 1] == b' ' {
            i -= 1;
        }
        let mut had_digit = false;
        while i > 0 && prefix[i - 1].is_ascii_digit() {
            had_digit = true;
            i -= 1;
        }
        assert!(
            !had_digit,
            "{}: contains a drift-prone '<digits> names that are always \
             available' count. Drop the count; list the names instead.",
            doc_path.display()
        );
    }

    // (b) Every always-available name must be mentioned somewhere in the
    //     file. This matches the set registered by
    //     `src/typechecker/builtins.rs` (the prelude + `Int`/`Float`/
    //     `String`/`Bool` primitive type descriptors).
    let required = [
        "print", "println", "panic", "Ok", "Err", "Some", "None", "Int", "Float", "String", "Bool",
    ];
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|name| !body.contains(name))
        .collect();
    assert!(
        missing.is_empty(),
        "{}: missing always-available name(s): {:?}. These are registered \
         in src/typechecker/builtins.rs and must appear in the bindings \
         doc's 'always available' list.",
        doc_path.display(),
        missing
    );
}

/// Round 62 phase-2 obsoleted this test: the `docs/stdlib/io-fs.md`
/// file (and its frontmatter title), `docs/stdlib/index.md`, and
/// `docs/stdlib-reference.md` were all deleted as part of inlining
/// stdlib prose into `super::docs::IO_FS_MD` for LSP delivery. The
/// io / fs / env coverage is now verified by the
/// `every_authoritative_builtin_has_a_non_empty_doc` lock in
/// `tests/docs_stdlib_println_parity_tests.rs`. Keeping a stub here
/// so the migration is visible in `git log -p`.
#[test]
fn io_fs_frontmatter_title_matches_stdlib_index() {
    // Sanity-poke the new contract: the io / fs / env modules each
    // have at least one binding with a non-empty registered doc.
    let docs = silt::typechecker::builtin_docs();
    for prefix in &["io.", "fs.", "env."] {
        let any = docs
            .iter()
            .any(|(k, v)| k.starts_with(prefix) && !v.trim().is_empty());
        assert!(
            any,
            "no `{prefix}*` binding has a non-empty registered doc — \
             round 62 phase-2 inlined the io-fs prose into \
             `super::docs::IO_FS_MD` and each of io / fs / env attaches \
             its own filtered subset via `attach_module_docs_filtered`. \
             Restore the section."
        );
    }
}

/// Regression lock for the LATENT audit: the `silt disasm` command description
/// used to drift across three sources. `src/main.rs` is the authoritative
/// `--help` text and says "Show bytecode disassembly"; README.md used
/// "Inspect compiled bytecode" and docs/getting-started.md used
/// "inspect compiled bytecode". This test keeps all three in sync.
///
/// README.md uses sentence-case entries with no leading `--`, so it must
/// contain the exact sentence-case phrase. docs/getting-started.md uses
/// lowercase entries prefixed with `-- `, so it must contain the lowercase
/// phrase. Reverting either doc to the old "inspect compiled bytecode"
/// wording — or changing the authoritative phrasing in src/main.rs without
/// updating the docs — will make this test fail.
#[test]
fn disasm_wording_consistent_across_main_readme_getting_started() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let main_rs = std::fs::read_to_string(manifest_dir.join("src").join("main.rs"))
        .expect("read src/main.rs");
    let readme = std::fs::read_to_string(manifest_dir.join("README.md")).expect("read README.md");
    let getting_started =
        std::fs::read_to_string(manifest_dir.join("docs").join("getting-started.md"))
            .expect("read docs/getting-started.md");

    // Authoritative wording comes from src/main.rs --help text.
    assert!(
        main_rs.contains("Show bytecode disassembly"),
        "src/main.rs no longer contains the authoritative disasm description \
         'Show bytecode disassembly'; update this test and the docs to match"
    );

    // README.md uses sentence-case help entries (e.g. "Run a program"),
    // so it must carry the exact sentence-case phrase.
    assert!(
        readme.contains("Show bytecode disassembly"),
        "README.md disasm description drifted from src/main.rs \
         ('Show bytecode disassembly')"
    );

    // docs/getting-started.md uses lowercase `-- <verb phrase>` entries
    // (e.g. "-- run a program"), so it must carry the lowercase phrase.
    assert!(
        getting_started.contains("show bytecode disassembly"),
        "docs/getting-started.md disasm description drifted from \
         src/main.rs (expected lowercase 'show bytecode disassembly' \
         to match the surrounding '-- <verb phrase>' format)"
    );
}

/// Regression test for round 15 BROKEN finding: `docs/language/testing.md`
/// shipped a ```silt block that declared `fn test_string_length` calling
/// `string.length("hello")` but only had `import test` at the top. The
/// block compiled under `all_doc_fn_main_blocks_compile` only because that
/// walker skips blocks with no `fn main`, so the missing `import string`
/// slipped through. A user copy-pasting the block and running `silt test`
/// on it would see `module 'string' is not imported`.
///
/// This walker closes the gap: every ```silt fence in README + docs/**/*.md
/// that declares a `fn test_*` or `fn skip_test_*` is treated as a testable
/// file, written to a `*.test.silt` temp file, and driven through
/// `silt test`. The whole point of a test doc block is to be copy-paste
/// runnable under `silt test`; if it isn't, the doc is broken. This is the
/// regression lock for the fixed `testing.md` block and will catch any new
/// doc test block that forgets an import (or otherwise fails to run).
#[test]
fn all_doc_fn_test_blocks_compile() {
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

    // Hand-rolled scanner for `fn test_<ident>` / `fn skip_test_<ident>`
    // declarations, so we don't need a regex dependency. Returns true iff
    // the block contains at least one declaration that `silt test` would
    // pick up as a runnable (or intentionally skipped) test.
    fn block_has_test_fn(src: &str) -> bool {
        for line in src.lines() {
            let trimmed = line.trim_start();
            let rest = match trimmed.strip_prefix("fn ") {
                Some(r) => r,
                None => continue,
            };
            let rest = rest
                .strip_prefix("skip_test_")
                .or_else(|| rest.strip_prefix("test_"));
            if let Some(after) = rest {
                // The char right after must look like an identifier continuation
                // so we don't match `fn test` as a bare-word false positive.
                if let Some(c) = after.chars().next()
                    && (c.is_ascii_alphanumeric() || c == '_')
                {
                    return true;
                }
            }
        }
        false
    }

    let tmp_dir =
        std::env::temp_dir().join(format!("silt_all_doc_test_blocks_{}", std::process::id()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");

    // Phase 1: collect every (doc, opener_line, src, file_stem) job
    // sequentially so the parallel pool only handles per-block work.
    struct Job {
        doc_path: PathBuf,
        opener_line: usize,
        src: String,
        file_stem: String,
    }
    let mut jobs: Vec<Job> = Vec::new();
    for doc_path in &targets {
        let body = std::fs::read_to_string(doc_path)
            .unwrap_or_else(|e| panic!("failed to read {}: {}", doc_path.display(), e));
        let blocks = extract_silt_blocks(&body);
        for (opener_line, src) in blocks {
            if !block_has_test_fn(&src) {
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
            });
        }
    }
    let testable_block_count = jobs.len();

    let failures: Mutex<Vec<String>> = Mutex::new(Vec::new());

    par_for_each(&jobs, |job| {
        // `silt test`'s auto-discovery path only picks up files that end
        // in `_test.silt` or `.test.silt`. Writing the temp file with
        // the `.test.silt` suffix keeps things consistent even when we
        // pass the filename explicitly, and makes the temp artifact
        // self-describing for any debugging on failure.
        let file = tmp_dir.join(format!("{}_line{}.test.silt", job.file_stem, job.opener_line));
        std::fs::write(&file, &job.src).expect("write temp silt test file");

        let output = silt_cmd()
            .arg("test")
            .arg(&file)
            .output()
            .expect("failed to spawn silt");
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            failures.lock().unwrap().push(format!(
                "{}:{} (```silt fence): exit={:?}\nstdout:\n{}\nstderr:\n{}",
                job.doc_path.display(),
                job.opener_line,
                output.status.code(),
                stdout,
                stderr
            ));
        }
    });

    let failures = failures.into_inner().unwrap();

    assert!(
        testable_block_count > 0,
        "expected at least one testable ```silt block (declaring `fn test_*` \
         or `fn skip_test_*`) across all docs — did the testing doc move?"
    );

    // Best-effort cleanup; leave artifacts on failure for debugging.
    if failures.is_empty() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    assert!(
        failures.is_empty(),
        "silt test failed for {} ```silt block(s) declaring `fn test_*` or \
         `fn skip_test_*` across docs/ and README.md. These blocks must be \
         copy-paste runnable under `silt test` — almost always this means a \
         block forgot an `import <module>` line at the top:\n\n{}",
        failures.len(),
        failures.join("\n---\n")
    );
}

/// Targeted regression lock for round 15 BROKEN finding:
/// `float.to_int`'s doc used to claim that it "Accepts both `Float`
/// and `ExtFloat`", but the typechecker signature in
/// `src/typechecker/builtins.rs` is `(Float) -> Int` only. Round 62
/// phase-2 inlined the int-float doc into
/// `super::docs::INT_FLOAT_MD`; we look up `float.to_int`'s
/// registered builtin doc.
///
/// If someone re-adds the claim, this test fails loudly.
#[test]
fn int_float_doc_does_not_claim_float_to_int_accepts_extfloat() {
    let docs = silt::typechecker::builtin_docs();
    let body = docs
        .get("float.to_int")
        .cloned()
        .expect("float.to_int builtin doc must be registered");

    assert!(
        !body.contains("Accepts both Float and ExtFloat"),
        "float.to_int's inlined builtin doc reintroduced the false claim \
         that it accepts `ExtFloat`. The typechecker signature is \
         `(Float) -> Int` only. Strike the clause from the corresponding \
         section in `super::docs::INT_FLOAT_MD` (in \
         src/typechecker/builtins/docs.rs)."
    );
    assert!(
        !body.contains("Accepts both `Float` and `ExtFloat`"),
        "float.to_int's inlined builtin doc reintroduced the false claim \
         in backticked form."
    );
}

/// Regression lock for round 15 GAP finding G7: `float.to_string`'s
/// doc used to document only the 2-arg form, contradicting the
/// runtime which accepts both 1-arg (shortest round-trippable) and
/// 2-arg (fixed decimal places) forms. Round 62 phase-2 inlined the
/// int-float prose into `super::docs::INT_FLOAT_MD`.
///
/// This test pins:
///   (a) the inlined float.to_string doc lists BOTH signatures
///   (b) modules.md no longer claims "no overloading" for float.to_string
#[test]
fn test_float_to_string_doc_documents_both_overloads() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let modules = manifest_dir
        .join("docs")
        .join("language")
        .join("modules.md");

    let docs = silt::typechecker::builtin_docs();
    let body = docs
        .get("float.to_string")
        .cloned()
        .expect("float.to_string builtin doc must be registered");
    let modules_body = std::fs::read_to_string(&modules)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", modules.display(), e));

    // (a) the inlined float.to_string doc must include the 1-arg form.
    assert!(
        body.contains("float.to_string(f: Float) -> String"),
        "float.to_string builtin doc is missing the 1-arg signature. \
         Both overloads must appear in the corresponding section of \
         `super::docs::INT_FLOAT_MD`."
    );
    // (a) and the 2-arg form.
    assert!(
        body.contains("float.to_string(f: Float, decimals: Int) -> String"),
        "float.to_string builtin doc is missing the 2-arg signature."
    );

    // (b) modules.md must no longer claim "no overloading".
    assert!(
        !modules_body.contains("no overloading"),
        "{}: reintroduced the false 'no overloading' claim for \
         float.to_string.",
        modules.display()
    );
    assert!(
        !modules_body.contains("takes **two arguments**"),
        "{}: reintroduced the outdated 'takes two arguments' phrasing \
         for float.to_string — the 1-arg form is supported at runtime.",
        modules.display()
    );
}

/// Regression lock for round 15 GAP finding G8: 27 ```silt fenced blocks
/// across `docs/stdlib/*.md` declared `let result = ...` in a `fn main`
/// block, which shadows the builtin `result` module and causes the
/// compiler to emit `warning[compile]: variable 'result' shadows the
/// builtin 'result' module`. A user copy-pasting any of those blocks
/// would see the warning even though nothing in the example was broken.
///
/// `all_doc_fn_main_blocks_compile` only checked for hard errors; this
/// walker extends that contract by failing if ANY `warning[` line
/// appears in the `silt check` stderr for a doc block's `fn main`.
/// Reverting a single block to `let result = ...` makes this walker
/// fail with a precise file path + opener line + the warning text.
#[test]
fn test_doc_fn_main_blocks_emit_no_compile_warnings() {
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
        std::env::temp_dir().join(format!("silt_doc_warnings_walker_{}", std::process::id()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");

    // Phase 1: collect every (doc, opener_line, src, file_stem) job
    // sequentially so the parallel pool only handles per-block work.
    struct Job {
        doc_path: PathBuf,
        opener_line: usize,
        src: String,
        file_stem: String,
    }
    let mut jobs: Vec<Job> = Vec::new();
    for doc_path in &targets {
        let body = std::fs::read_to_string(doc_path)
            .unwrap_or_else(|e| panic!("failed to read {}: {}", doc_path.display(), e));
        let blocks = extract_silt_blocks(&body);
        for (opener_line, src) in blocks {
            if !src.contains("fn main") {
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
            });
        }
    }
    let checked = jobs.len();

    let failures: Mutex<Vec<String>> = Mutex::new(Vec::new());

    par_for_each(&jobs, |job| {
        let file = tmp_dir.join(format!("{}_line{}.silt", job.file_stem, job.opener_line));
        std::fs::write(&file, &job.src).expect("write temp silt file");

        let output = silt_cmd()
            .arg("check")
            .arg(&file)
            .output()
            .expect("failed to spawn silt");
        let stderr = String::from_utf8_lossy(&output.stderr);
        let warn_lines: Vec<&str> = stderr.lines().filter(|l| l.contains("warning[")).collect();
        if !warn_lines.is_empty() {
            failures.lock().unwrap().push(format!(
                "{}:{} (```silt fence): emitted compile warning(s):\n{}",
                job.doc_path.display(),
                job.opener_line,
                warn_lines.join("\n")
            ));
        }
    });

    let failures = failures.into_inner().unwrap();

    assert!(
        checked > 0,
        "expected at least one runnable ```silt block (containing `fn main`) \
         across all docs"
    );

    if failures.is_empty() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    assert!(
        failures.is_empty(),
        "doc ```silt blocks must emit no compile warnings under `silt check`. \
         {} block(s) failed. A copy-paste runnable example should never \
         surface a warning to the user. The common cause is `let result = \
         ...` shadowing the builtin `result` module — rename the binding \
         to something else.\n\n{}",
        failures.len(),
        failures.join("\n---\n")
    );
}

/// Regression lock for round 15 GAP finding G9: the Tooling block in
/// `docs/getting-started.md` used to list 10 subcommands (run, run -w,
/// check, check --format, test, fmt, fmt --check, repl, lsp, disasm) but
/// omitted `silt init`, even though `silt init` is the very first
/// command the same file tells the user to run in the "Your first
/// program" section and is listed both in `src/main.rs` help output and
/// README.md.
///
/// This test extracts the set of subcommands from `silt --help` output
/// (the authoritative list in src/main.rs) and asserts that every one of
/// them appears in the Tooling block of getting-started.md. If a new
/// subcommand is added to src/main.rs without updating this doc, the
/// walker fails with a precise pointer at the missing command.
#[test]
fn test_getting_started_tooling_block_matches_main_help() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let gs_path = manifest_dir.join("docs").join("getting-started.md");
    let gs_body = std::fs::read_to_string(&gs_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", gs_path.display(), e));

    // Extract the Tooling block. It's a ```sh fenced block immediately
    // after a `## Tooling` heading. We look for the heading, then the
    // next ```sh fence, then collect until the closing fence.
    let tooling_idx = gs_body
        .find("## Tooling")
        .expect("docs/getting-started.md is missing a '## Tooling' heading");
    let rest = &gs_body[tooling_idx..];
    let sh_open = rest
        .find("```sh")
        .expect("docs/getting-started.md Tooling section lacks a ```sh block");
    let body_after_open = &rest[sh_open + "```sh".len()..];
    let sh_close = body_after_open
        .find("```")
        .expect("docs/getting-started.md Tooling ```sh block is unterminated");
    let tooling_block = &body_after_open[..sh_close];

    // Run the silt binary with `--help` to get the authoritative list.
    let help_output = silt_cmd()
        .arg("--help")
        .output()
        .expect("failed to spawn silt --help");
    assert!(
        help_output.status.success(),
        "silt --help exited non-zero: {:?}",
        help_output.status.code()
    );
    let help_text = String::from_utf8_lossy(&help_output.stdout).to_string()
        + &String::from_utf8_lossy(&help_output.stderr);

    // Pull out every `silt <subcommand>` entry from the help output.
    // The help lines look like `  silt run [--watch] <file.silt>    Run a program`.
    // We only keep the first identifier-ish token after `silt ` (so
    // `silt run`, `silt check`, ..., `silt init`, ...).
    let mut required_subcommands: Vec<String> = Vec::new();
    for line in help_text.lines() {
        let trimmed = line.trim_start();
        let rest = match trimmed.strip_prefix("silt ") {
            Some(r) => r,
            None => continue,
        };
        let sub: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphabetic() || *c == '_')
            .collect();
        if sub.is_empty() {
            continue;
        }
        // Skip the literal `help` subcommand (per convention in other
        // tests that compare against --help output).
        if sub == "help" {
            continue;
        }
        if !required_subcommands.contains(&sub) {
            required_subcommands.push(sub);
        }
    }

    assert!(
        !required_subcommands.is_empty(),
        "could not extract any `silt <subcommand>` entries from `silt \
         --help` output — has the help format changed?\n\n{}",
        help_text
    );

    // Assert every required subcommand appears in the Tooling block.
    let mut missing: Vec<String> = Vec::new();
    for sub in &required_subcommands {
        let needle = format!("silt {}", sub);
        if !tooling_block.contains(&needle) {
            missing.push(sub.clone());
        }
    }
    assert!(
        missing.is_empty(),
        "{}: Tooling block is missing subcommand(s) {:?} that appear in \
         `silt --help` (authoritative list from src/main.rs). Add a line \
         for each missing subcommand so the doc stays in sync.\n\nTooling \
         block:\n{}\n\nHelp output:\n{}",
        gs_path.display(),
        missing,
        tooling_block,
        help_text
    );
}

/// Round-16 GAP G8 lock: doc `fn main` blocks are compiled by
/// `all_doc_fn_main_blocks_compile` but never actually *run* by the
/// walker. That means a block that compiles but panics at runtime
/// (wrong-shaped record, impossible match arm, hidden division by
/// zero, missing option unwrap) ships to users silently.
///
/// This walker extracts every ```silt fence block in `docs/**/*.md`
/// (plus README.md) that contains `fn main(`, applies an aggressive
/// skip heuristic to exclude blocks that need interactive,
/// networked, filesystem, or long-running features the walker cannot
/// safely execute, and invokes `silt run` on each surviving block.
/// A block that exits non-zero, panics, or emits `error[` on stderr
/// is a regression.
///
/// # Skip list convention
///
/// A block is skipped if:
///
/// 1. The first non-empty line inside the ``` ```silt ``` fence is a
///    comment of exactly `// noexec` or `-- noexec`. This is the
///    per-block opt-out — use it sparingly for blocks that compile
///    cleanly but deliberately demonstrate runtime behavior the
///    walker cannot validate.
///
/// 2. The block body contains any substring from `DENY_SUBSTRINGS`
///    below. This is the heuristic opt-out. Each entry documents why.
///    The deny list is intentionally conservative — a false positive
///    (block skipped when it could have run) is far cheaper than a
///    false negative (walker hangs forever on an interactive block
///    or tries to hit a real network).
///
/// The current skip rate is high (roughly 70-80% of doc blocks) and
/// that is OK — running even 20-30% of blocks is a large improvement
/// over running 0%. As the runtime grows sandboxed mocks for fs/env
/// and channel operations, the deny list can tighten.
///
/// # Timeout
///
/// Each subprocess has a 10-second wall clock cap enforced via
/// `Command::output()`'s implicit wait plus a post-hoc duration check
/// — if a block *does* slip through the deny list and runs long, the
/// test still terminates reasonably because none of the non-deny
/// blocks should take more than a handful of milliseconds.
#[test]
fn all_doc_fn_main_blocks_run_if_safe() {
    // Each entry in this list suppresses execution of any ```silt
    // block whose body contains it. The match is a raw substring
    // check — we deliberately skip blocks that *mention* these APIs
    // even in comments, because many doc blocks comment out a line
    // to document expected output. False positives on the walker
    // are acceptable.
    //
    // Kept as a `const` so the skip list is discoverable from a
    // single grep of the file and changes show up in review.
    const DENY_SUBSTRINGS: &[&str] = &[
        // Networked — would hit the real internet in CI.
        "http.get",
        "http.post",
        "http.put",
        "http.delete",
        "http.serve",
        "http.Server",
        // Concurrency — can block forever on unbounded
        // send/receive, and task scheduling is non-deterministic
        // under subprocess timing. Blocks using these are doc-only.
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
        // Interactive IO — blocks waiting for stdin in CI.
        "io.read_line",
        "io.stdin",
        "read_line",
        // File system — platform-dependent behavior and may
        // require fixtures that don't exist in the test harness.
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
        // Environment — CI environment may not have the
        // variables the doc block assumes.
        "env.get",
        "env.set",
        "env.args",
        // Time — `time.sleep` blocks; `time.now` is
        // non-deterministic but usually safe, so we only deny
        // the blocking form.
        "time.sleep",
        // Infinite loops — `loop { ... }` without a bound is
        // common in server/daemon demos and would hang the
        // walker. This is a fuzzy match; blocks using bounded
        // `loop acc = 0 { ... }` accumulators are safe because
        // they don't contain the bare `loop {` pattern.
        "loop {",
        "while true",
        // `process.exit` would terminate the subprocess but
        // is usually paired with demonstration of signal
        // handling — skip to avoid spurious non-zero exits.
        "process.exit",
    ];

    // Blocks whose first non-empty line is one of these markers
    // are opted out of execution explicitly by the doc author.
    const NOEXEC_MARKERS: &[&str] = &["// noexec", "-- noexec"];

    /// Decide whether a block should be executed. Returns
    /// `Some(reason)` to skip with a human-readable reason, or
    /// `None` to run it.
    fn skip_reason(src: &str) -> Option<String> {
        // 1. Explicit opt-out via `// noexec` / `-- noexec` marker
        //    on the first non-empty line.
        let first_nonempty = src.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        let trimmed = first_nonempty.trim();
        for marker in NOEXEC_MARKERS {
            if trimmed == *marker {
                return Some(format!("explicit {marker}"));
            }
        }

        // 2. Heuristic deny list.
        for needle in DENY_SUBSTRINGS {
            if src.contains(needle) {
                return Some(format!("contains `{needle}`"));
            }
        }

        None
    }

    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));

    // Collect targets: README.md plus every .md file under docs/
    // (recursive). Matches the set used by
    // `all_doc_fn_main_blocks_compile` so the two walkers operate
    // on the same universe of blocks.
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

    let tmp_dir = std::env::temp_dir().join(format!("silt_doc_run_walker_{}", std::process::id()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");

    // Phase 1: collect every job sequentially. Skipped blocks count
    // toward `skipped` but produce no parallel work.
    struct Job {
        doc_path: PathBuf,
        opener_line: usize,
        src: String,
        file_stem: String,
    }
    let mut jobs: Vec<Job> = Vec::new();
    let mut total_fn_main_blocks = 0usize;
    let mut skipped = 0usize;

    for doc_path in &targets {
        let body = std::fs::read_to_string(doc_path)
            .unwrap_or_else(|e| panic!("failed to read {}: {}", doc_path.display(), e));
        let blocks = extract_silt_blocks(&body);

        for (opener_line, src) in blocks {
            if !src.contains("fn main") {
                continue;
            }
            total_fn_main_blocks += 1;

            if skip_reason(&src).is_some() {
                skipped += 1;
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
            });
        }
    }
    let ran = jobs.len();
    let failures: Mutex<Vec<String>> = Mutex::new(Vec::new());

    par_for_each(&jobs, |job| {
        let file = tmp_dir.join(format!("{}_line{}.silt", job.file_stem, job.opener_line));
        std::fs::write(&file, &job.src).expect("write temp silt file");

        let output = silt_cmd()
            .arg("run")
            .arg(&file)
            .output()
            .expect("failed to spawn silt");

        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);

        if stderr.contains("panicked at") {
            failures.lock().unwrap().push(format!(
                "{}:{} (```silt fence): silt panicked while running \
                 the block.\nstderr:\n{}\nstdout:\n{}",
                job.doc_path.display(),
                job.opener_line,
                stderr,
                stdout
            ));
            return;
        }

        let error_lines: Vec<&str> = stderr.lines().filter(|l| l.contains("error[")).collect();
        if !error_lines.is_empty() {
            failures.lock().unwrap().push(format!(
                "{}:{} (```silt fence): runtime error(s):\n{}\n\
                 stderr:\n{}\nstdout:\n{}",
                job.doc_path.display(),
                job.opener_line,
                error_lines.join("\n"),
                stderr,
                stdout
            ));
            return;
        }

        if !output.status.success() {
            failures.lock().unwrap().push(format!(
                "{}:{} (```silt fence): exit={:?}\nstdout:\n{}\n\
                 stderr:\n{}",
                job.doc_path.display(),
                job.opener_line,
                output.status.code(),
                stdout,
                stderr
            ));
        }
    });

    let failures = failures.into_inner().unwrap();

    // Sanity check: the universe should have at least some `fn main`
    // blocks (the compile walker already asserts this), and the
    // runnable subset should be non-empty so the walker is actually
    // exercising something. If the deny list grows so broad that
    // zero blocks run, treat that as a configuration failure.
    assert!(
        total_fn_main_blocks > 0,
        "expected at least one ```silt block with `fn main` across docs/"
    );
    assert!(
        ran > 0,
        "the DENY_SUBSTRINGS list skipped every doc block — the walker \
         would never catch a runtime regression. Tighten the list."
    );

    if failures.is_empty() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    assert!(
        failures.is_empty(),
        "`silt run` failed for {} doc ```silt block(s) out of {} runnable \
         (of {} fn-main total, {} skipped by deny list). A doc block that \
         compiles cleanly but fails at runtime is a user-visible bug — \
         fix the doc or tag the block with a `// noexec` first-line \
         marker and file an issue for the underlying cause.\n\n{}",
        failures.len(),
        ran,
        total_fn_main_blocks,
        skipped,
        failures.join("\n---\n")
    );
}

// ─── Round-23 audit (agent G): doc presence lock-in tests ────────────────
//
// The walker tests above exercise any fn-main block inside a silt code
// fence, but they don't guarantee the surrounding prose still exists.
// These tests pin the *documentation* itself — if someone deletes the
// section, these fail. Keep them narrow (substring matches on stable
// anchors) so they aren't tripped by legitimate prose edits.

#[test]
fn docs_mention_task_deadline_builtin() {
    let docs = silt::typechecker::builtin_docs();
    let body = docs
        .get("task.deadline")
        .cloned()
        .expect("task.deadline builtin doc must be registered");
    assert!(
        body.contains("task.deadline(dur: Duration"),
        "task.deadline builtin doc should show the (Duration, () -> a) -> a signature"
    );
    assert!(
        body.contains("I/O timeout (task.deadline exceeded)"),
        "task.deadline builtin doc should quote the exact error message silt emits"
    );
}

#[test]
fn docs_mention_task_spawn_until_builtin() {
    let docs = silt::typechecker::builtin_docs();
    let body = docs
        .get("task.spawn_until")
        .cloned()
        .expect("task.spawn_until builtin doc must be registered");
    assert!(
        body.contains("task.spawn_until(dur: Duration"),
        "task.spawn_until builtin doc should show the (Duration, () -> a) -> Handle(a) signature"
    );
}

#[test]
fn docs_mention_silt_io_timeout_env_var() {
    let doc = std::fs::read_to_string("docs/concurrency.md")
        .expect("docs/concurrency.md must be readable");
    assert!(
        doc.contains("SILT_IO_TIMEOUT"),
        "docs/concurrency.md must document the SILT_IO_TIMEOUT env var"
    );
    assert!(
        doc.contains("I/O timeout (SILT_IO_TIMEOUT exceeded)"),
        "concurrency docs should quote the exact error silt emits when the global timeout fires"
    );
}

#[test]
fn docs_mention_watchdog_zombie_limitation() {
    let doc = std::fs::read_to_string("docs/concurrency.md").unwrap();
    assert!(
        doc.contains("SILT_IO_TIMEOUT")
            && (doc.contains("zombie")
                || doc.contains("not cancelled")
                || doc.contains("continues to completion")),
        "docs/concurrency.md must document watchdog-zombie limitation near SILT_IO_TIMEOUT"
    );
}

#[test]
fn docs_mention_silt_run_disassemble_flag() {
    // The `silt run --disassemble` flag was previously only visible via
    // `silt run --help`. Pin a mention in user-facing docs so it stays
    // discoverable.
    let getting_started = std::fs::read_to_string("docs/getting-started.md")
        .expect("docs/getting-started.md must be readable");
    let readme = std::fs::read_to_string("README.md").expect("README.md must be readable");
    assert!(
        getting_started.contains("--disassemble") || readme.contains("--disassemble"),
        "either docs/getting-started.md or README.md must mention the `silt run --disassemble` flag"
    );
}
