//! Round 93 GAP lock: every ```silt fence inside the builtin docs
//! (LSP hover markdown served from `src/typechecker/builtins/docs.rs`)
//! must pass `silt check`.
//!
//! ### Why this walker exists
//!
//! The builtin docs carry ~268 unique ```silt fenced examples that are
//! shown verbatim to users via LSP hover. Before this lock the only
//! walker over that fence set was
//! `tests/docs_stdlib_println_parity_tests.rs`, which *runs* only the
//! fences that (a) contain `fn main`, (b) survive the run-safety deny
//! list, and (c) carry `println(...) -- expected` annotations — about
//! 166 of the 268. The remainder (deny-listed fs/http/channel/task/env/
//! time examples, safe-but-unannotated blocks, and a handful of
//! fragments) were never validated by anything: a typo or API drift in
//! one of them would ship silently in hover docs.
//!
//! ### What this lock asserts
//!
//! Every unique ```silt fence must pass `silt check` — which drives the
//! full lex → parse → typecheck → compile pipeline (see
//! `src/cli/check.rs`) without executing any user code:
//!
//! - Fences containing `fn main` are checked as-is.
//! - Fragment fences (no `fn main`) are completed without editing the
//!   snippet itself, via two attempts:
//!   1. `fn main(){}` appended — covers declaration-shaped fragments
//!      (type/fn definitions shown without an entry point).
//!   2. If (1) fails: the fragment's statements are wrapped in a
//!      closure inside `fn main` with a trailing `Ok(())` (so `?`
//!      lines type-check), any `import` lines are hoisted to the top,
//!      and `import <m>` is prepended for every builtin module the
//!      fragment references (`silt::module::BUILTIN_MODULES`). This is
//!      exactly what a reader pasting an expression-shaped hover
//!      example into a program would write around it.
//!   A fragment fails the gate only if BOTH attempts fail.
//!
//! The run-safety deny list used by the println-parity walker and
//! `examples_check.rs::all_doc_fn_main_blocks_run_if_safe` applies to
//! *running* snippets only. Checking never executes code, so NOTHING
//! is exempt here — deny-listed and `noexec` fences are checked too.
//!
//! ### Coverage lock
//!
//! The test also asserts it checked at least `MIN_EXPECTED_FENCES`
//! unique fences. If a refactor breaks fence extraction (or the doc
//! registry stops returning entries) the walker fails loudly instead
//! of silently iterating an empty set.

use std::collections::HashSet;
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Floor for the unique-fence coverage assertion. The registry holds
/// ~268 unique fences today (261 with `fn main` + 7 fragments); the
/// floor leaves a little headroom for doc edits that merge or remove
/// an example, while still firing if the walker ever starts skipping
/// whole classes of fences.
const MIN_EXPECTED_FENCES: usize = 260;

/// Pool size for the `silt check` subprocess fan-out. Same rationale
/// as `tests/examples_check.rs::pool_size`: cargo already parallelizes
/// across tests, so keep this modest to avoid oversubscribing CI.
fn pool_size() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(2, 8)
}

/// Run `task` for each item across a small thread pool (mirrors
/// `tests/examples_check.rs::par_for_each`).
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

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

/// Extract every ```silt fenced block from a doc body. Returns
/// `(opener_line_number_1indexed, block_source)` tuples. Same
/// extraction as `tests/docs_stdlib_println_parity_tests.rs` so the
/// two walkers always agree on what a fence is.
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

/// Builtin names like `fs.read` or `string.to_upper` are not valid
/// file stems everywhere; keep alphanumerics, map the rest to `_`.
fn sanitize_stem(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// Fragment completion, attempt 1: append an empty `fn main(){}`.
/// Works for declaration-shaped fragments (type / fn definitions).
fn fragment_with_main_appended(src: &str) -> String {
    format!("{src}\nfn main(){{}}\n")
}

/// Fragment completion, attempt 2: wrap the fragment's statements in
/// a closure inside `fn main`, hoisting any `import` lines to the top
/// and auto-prepending `import <m>` for every builtin module the
/// fragment references. The trailing `Ok(())` lets `?`-bearing lines
/// type-check (the closure's return type is inferred as
/// `Result(_, _)`); for fragments without `?` it is a harmless final
/// expression.
fn fragment_wrapped_in_closure(src: &str) -> String {
    let mut imports: Vec<String> = Vec::new();
    let mut body_lines: Vec<&str> = Vec::new();
    for line in src.lines() {
        if line.trim_start().starts_with("import ") {
            imports.push(line.trim().to_string());
        } else {
            body_lines.push(line);
        }
    }
    // Auto-import every builtin module the fragment mentions as
    // `<module>.` and does not already import.
    for m in silt::module::BUILTIN_MODULES {
        let stmt = format!("import {m}");
        if src.contains(&format!("{m}.")) && !imports.iter().any(|i| i == &stmt) {
            imports.push(stmt);
        }
    }
    format!(
        "{}\n\nfn main() {{\n  let _doc_fragment = fn() {{\n{}\n    Ok(())\n  }}\n}}\n",
        imports.join("\n"),
        body_lines.join("\n")
    )
}

/// Every unique ```silt fence in the builtin (LSP hover) docs must
/// pass `silt check` — full lex → parse → typecheck → compile, no
/// execution. Fragments get `fn main(){}` appended; nothing is
/// deny-list-exempt because nothing runs.
#[test]
fn all_builtin_doc_silt_fences_pass_silt_check() {
    let entries: Vec<(String, String)> = silt::typechecker::iter_builtin_docs();
    assert!(
        !entries.is_empty(),
        "expected at least one builtin doc entry — \
         silt::typechecker::iter_builtin_docs() returned nothing"
    );

    // Phase 1: collect every unique fence. Dedup by block source hash
    // (many bindings share one module-overview block — same scheme as
    // the println-parity walker) so each unique snippet is checked
    // exactly once.
    struct Job {
        /// First builtin name whose doc carries this fence (for error
        /// pointers; duplicates under other names are deduped away).
        name: String,
        opener_line: usize,
        /// Raw fence source as served in hover docs.
        src: String,
        is_fragment: bool,
        stem: String,
    }
    let mut jobs: Vec<Job> = Vec::new();
    let mut seen_blocks: HashSet<u64> = HashSet::new();

    for (name, body) in &entries {
        for (opener_line, src) in extract_silt_blocks(body) {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            std::hash::Hash::hash(&src, &mut hasher);
            let h = std::hash::Hasher::finish(&hasher);
            if !seen_blocks.insert(h) {
                continue;
            }

            let is_fragment = !src.contains("fn main");
            jobs.push(Job {
                name: name.clone(),
                opener_line,
                src,
                is_fragment,
                stem: sanitize_stem(name),
            });
        }
    }

    let total_fences = jobs.len();
    let fragment_fences = jobs.iter().filter(|j| j.is_fragment).count();
    eprintln!(
        "round93 builtin-docs walker: checking {total_fences} unique \
         ```silt fences ({} fn-main, {fragment_fences} fragments)",
        total_fences - fragment_fences
    );

    // Coverage lock: silent fence-skipping must fail loudly.
    assert!(
        total_fences >= MIN_EXPECTED_FENCES,
        "builtin-docs walker only found {total_fences} unique ```silt \
         fences but expected at least {MIN_EXPECTED_FENCES}. Either a \
         large number of hover examples were deleted (lower the floor \
         deliberately, with a comment) or fence extraction / the doc \
         registry broke and the walker is silently skipping fences."
    );

    let tmp_dir = std::env::temp_dir().join(format!(
        "silt_round93_builtin_docs_walker_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");

    // Phase 2: `silt check` every fence in parallel. `silt check`
    // drives the full compile pipeline (typecheck + compile phase —
    // see src/cli/check.rs) without executing the program, so the
    // run-safety deny list is irrelevant here.
    let failures: Mutex<Vec<String>> = Mutex::new(Vec::new());
    par_for_each(&jobs, |job| {
        // Each attempt is `(label, completed_source)`; a fence passes
        // if ANY attempt passes. Full programs get exactly one
        // attempt (as-is); fragments get the two completion attempts.
        let attempts: Vec<(&str, String)> = if job.is_fragment {
            vec![
                (
                    "fragment, `fn main(){}` appended",
                    fragment_with_main_appended(&job.src),
                ),
                (
                    "fragment, wrapped in closure with auto-imports",
                    fragment_wrapped_in_closure(&job.src),
                ),
            ]
        } else {
            vec![("full program, checked as-is", job.src.clone())]
        };

        let mut attempt_errors: Vec<String> = Vec::new();
        for (i, (label, check_src)) in attempts.iter().enumerate() {
            let file = tmp_dir.join(format!("{}_line{}_a{}.silt", job.stem, job.opener_line, i));
            std::fs::write(&file, check_src).expect("write temp silt file");

            let output = silt_cmd()
                .arg("check")
                .arg(&file)
                .output()
                .expect("failed to spawn silt");
            if output.status.success() {
                return; // fence passes — no failure recorded
            }
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            attempt_errors.push(format!(
                "  attempt {} ({label}): exit={:?}\n  stdout:\n{}\n  stderr:\n{}",
                i + 1,
                output.status.code(),
                stdout,
                stderr
            ));
        }

        failures.lock().unwrap().push(format!(
            "builtin doc `{}` fence at doc-line {}: every completion attempt failed\n{}",
            job.name,
            job.opener_line,
            attempt_errors.join("\n")
        ));
    });

    let failures = failures.into_inner().unwrap();

    // Best-effort cleanup; leave artifacts on failure for debugging.
    if failures.is_empty() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    assert!(
        failures.is_empty(),
        "silt check failed for {} of {} unique ```silt fence(s) in the \
         builtin (LSP hover) docs. Hover examples are user-facing and \
         must stay copy-paste-able; fix the doc string in \
         src/typechecker/builtins/docs.rs:\n\n{}",
        failures.len(),
        total_fences,
        failures.join("\n---\n")
    );
}
