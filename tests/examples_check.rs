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
            let mut compiler = Compiler::with_project_root(project_root.clone());
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
        "print", "println", "panic", "Ok", "Err", "Some", "None", "Int", "Float", "String",
        "Bool",
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

/// Regression lock for the LATENT audit: `docs/stdlib/io-fs.md`'s frontmatter
/// `title:` used to say "io / fs" even though the file also documents
/// `env.*` and both `docs/stdlib/index.md` and `docs/stdlib-reference.md`
/// already refer to it as "io / fs / env". This test guarantees the three
/// labels stay in sync.
#[test]
fn io_fs_frontmatter_title_matches_stdlib_index() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let io_fs_path = manifest_dir.join("docs").join("stdlib").join("io-fs.md");
    let body = std::fs::read_to_string(&io_fs_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", io_fs_path.display(), e));

    // The frontmatter `title:` must be "io / fs / env".
    assert!(
        body.contains("title: \"io / fs / env\""),
        "{}: frontmatter title must be `io / fs / env` to match the stdlib \
         index entries and the file's actual env coverage, but got:\n{}",
        io_fs_path.display(),
        body.lines().take(6).collect::<Vec<_>>().join("\n")
    );

    // And the stdlib index/reference must still label it consistently.
    let index_path = manifest_dir.join("docs").join("stdlib").join("index.md");
    let index_body = std::fs::read_to_string(&index_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", index_path.display(), e));
    assert!(
        index_body.contains("io / fs / env"),
        "{}: expected a reference to `io / fs / env` so the frontmatter \
         title and index label stay in sync",
        index_path.display()
    );

    let reference_path = manifest_dir.join("docs").join("stdlib-reference.md");
    if reference_path.is_file() {
        let reference_body = std::fs::read_to_string(&reference_path)
            .unwrap_or_else(|e| panic!("failed to read {}: {}", reference_path.display(), e));
        assert!(
            reference_body.contains("io / fs / env"),
            "{}: expected a reference to `io / fs / env` so the frontmatter \
             title and stdlib-reference label stay in sync",
            reference_path.display()
        );
    }
}
