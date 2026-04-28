//! Round 62 audit lock: doc-link sanity, version freshness, and
//! builtin-count parity for the effect-rows materials.
//!
//! These tests exist because the round-62 audit caught five concrete
//! drifts:
//!
//! - B6: five dangling markdown links pointed at the deleted
//!   `docs/stdlib/` directory (the stdlib reference was inlined into
//!   `src/typechecker/builtins/docs.rs`). Each link must be replaced
//!   with prose that points the user at LSP hover or `silt --help`.
//! - B7: a `println(msg)  -- "hello"` annotation in
//!   `docs/concurrency.md` — `println` on a `Value::String` calls
//!   `display_value` which prints the bare string (no quotes), so the
//!   expectation must be `-- hello` (no quotes).
//! - G2: stale `v0.12 ships` / `for v0.12` prose lingering in v0.13.
//!   The phases shipped in v0.12; v0.13 keeps the permissive default.
//! - G3: builtin count drift between the proposal (~400 / 401) and
//!   the actual count (388 per commit `0c72f41`).
//! - L9: a non-deterministic concurrency example that printed a
//!   specific receive order — the receive order is a permutation of
//!   the spawn order, not a guarantee.
//!
//! If any of these regress, the test below points at the offending
//! file with a precise message.

use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_doc(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e))
}

/// B6 lock: none of the five files that previously linked into
/// `docs/stdlib/` may contain `../stdlib/` anywhere. The directory is
/// gone; any link there is broken.
#[test]
fn docs_language_does_not_link_to_deleted_stdlib_dir() {
    let affected = [
        "docs/language/error-handling.md",
        "docs/language/testing.md",
        "docs/language/collections.md",
    ];
    for rel in affected {
        let body = read_doc(rel);
        assert!(
            !body.contains("../stdlib/"),
            "{rel} still contains a link to ../stdlib/ (deleted directory). \
             Replace each occurrence with prose pointing at LSP hover or \
             `silt --help`."
        );
    }
}

/// B7 lock: the `println(msg)  -- "hello"` line in concurrency.md
/// must not include the literal `"hello"` (with quotes) on a `--`
/// comment line — `println(Value::String("hello"))` prints the bare
/// `hello` via `display_value`.
#[test]
fn concurrency_md_println_does_not_quote_string_output() {
    let body = read_doc("docs/concurrency.md");
    let mut hits: Vec<(usize, &str)> = Vec::new();
    for (idx, line) in body.lines().enumerate() {
        let trimmed = line.trim_start();
        // We're looking for `-- "hello"` style comments inside the
        // rendezvous example. Any `--` comment line that pairs a bare
        // double-quoted string after the `--` is a quoted-output
        // annotation; that's the bug.
        if trimmed.starts_with("--") && trimmed.contains("\"hello\"") {
            hits.push((idx + 1, line));
        }
    }
    assert!(
        hits.is_empty(),
        "docs/concurrency.md still has quoted-string println annotations: \
         {hits:?}. `println(value::String)` prints the bare string."
    );
}

/// G2 lock: the effect-rows materials must not present-tense ship a
/// v0.12-only world. v0.13 is the current crate version; the phases
/// shipped in v0.12 and the default is still permissive in v0.13.
#[test]
fn effect_rows_docs_use_v0_12_or_later_for_phase_a_to_d() {
    let migration = read_doc("docs/strict-effects-migration.md");
    let proposal = read_doc("docs/proposals/effect-rows.md");

    // The exact stale phrases the audit flagged. Each must be gone.
    let banned_pairs = [
        ("docs/strict-effects-migration.md", &migration, "v0.12 ships"),
        (
            "docs/strict-effects-migration.md",
            &migration,
            "across all of v0.12 without the flag",
        ),
        (
            "docs/strict-effects-migration.md",
            &migration,
            "for\nv0.12 the choice stays with you",
        ),
        (
            "docs/proposals/effect-rows.md",
            &proposal,
            "implemented in v0.12 (Phases A",
        ),
        (
            "docs/proposals/effect-rows.md",
            &proposal,
            "Default flips in v0.13 or v1.0",
        ),
    ];
    for (path, body, banned) in banned_pairs {
        assert!(
            !body.contains(banned),
            "{path} still contains stale prose `{banned}`. Reword to \
             reflect that v0.13 keeps the permissive default."
        );
    }
}

/// G3 lock: the proposal's three builtin-count callouts must agree
/// with the committed classification (388 per commit `0c72f41`).
#[test]
fn effect_rows_builtin_count_matches_committed_classification() {
    let body = read_doc("docs/proposals/effect-rows.md");
    // Stale numbers seen in the round-62 audit.
    for stale in ["~400 builtins", "401 builtins"] {
        assert!(
            !body.contains(stale),
            "docs/proposals/effect-rows.md still mentions `{stale}`. \
             Reconcile to 388 (per commit 0c72f41 — \"Effect rows phase \
             C: stdlib sweep — 388 builtins classified\")."
        );
    }
    // Positive assertion: the committed count appears at least three
    // times (Friction line, sweep-ordering line, Phase C line).
    let occurrences = body.matches("388 builtins").count();
    assert!(
        occurrences >= 3,
        "docs/proposals/effect-rows.md should mention `388 builtins` \
         in at least three places (Friction / Stdlib sweep ordering / \
         Phase C). Saw {occurrences}."
    );
}

/// L9 lock: the spawn-work-join example in concurrency.md must
/// acknowledge that receive order is a permutation of spawn order,
/// not a fixed `10, 20, 30` sequence. The fix-author's choice was
/// prose-only ("in some order" + "permutation"); we lock both
/// keywords so a future cleanup can swap one phrasing for the other
/// as long as the disclaimer survives.
#[test]
fn concurrency_md_spawn_work_join_disclaims_receive_order() {
    let body = read_doc("docs/concurrency.md");
    let needle = "results: 10, 20, 30";
    let pos = body
        .find(needle)
        .unwrap_or_else(|| panic!("docs/concurrency.md no longer contains `{needle}`"));
    // Take a 600-byte window starting at the match — enough to cover
    // the surrounding `--` comment lines.
    let end = (pos + 600).min(body.len());
    let window = &body[pos..end];
    let has_disclaimer =
        window.contains("in some order") || window.contains("permutation");
    assert!(
        has_disclaimer,
        "docs/concurrency.md spawn-work-join example near `{needle}` \
         no longer notes that the receive order is non-deterministic. \
         Add `in some order` or `permutation` to the surrounding prose."
    );
}

/// Belt-and-braces check: a fast scan over every `.md` under `docs/`
/// catching any markdown link of the form `](../stdlib/...)`. The
/// round-62 audit listed five exact files but a future drift might
/// reintroduce the link elsewhere.
#[test]
fn no_doc_anywhere_links_to_deleted_stdlib_dir() {
    let mut offenders: Vec<String> = Vec::new();
    walk_md(&repo_root().join("docs"), &mut |path, body| {
        for (idx, line) in body.lines().enumerate() {
            if line.contains("](../stdlib/") || line.contains("](./stdlib/") {
                offenders.push(format!("{}:{}: {}", path.display(), idx + 1, line));
            }
        }
    });
    assert!(
        offenders.is_empty(),
        "found markdown links to the deleted docs/stdlib/ directory:\n  {}",
        offenders.join("\n  ")
    );
}

fn walk_md(dir: &Path, visit: &mut dyn FnMut(&Path, &str)) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_md(&path, visit);
        } else if path.extension().and_then(|s| s.to_str()) == Some("md") {
            if let Ok(body) = fs::read_to_string(&path) {
                visit(&path, &body);
            }
        }
    }
}
