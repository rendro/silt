//! Source-grep lock tests for rustdoc warnings introduced by intra-doc
//! links to private items, redundant explicit link targets, or HTML-like
//! sequences inside doc comments.
//!
//! Each test asserts on the file's textual contents — running
//! `cargo doc --no-deps --release` from a test would be too slow, so we
//! pin the fixed strings here. If you legitimately need to reintroduce
//! one of the patterns below, update the corresponding test and verify
//! `cargo doc --no-deps --release 2>&1 | grep warning:` is still clean.
//!
//! This file is shared between agents — append new tests at the bottom
//! using unique test names; do not replace existing tests.

use std::fs;

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"))
}

#[test]
fn lockfile_doc_no_private_link() {
    // src/lockfile.rs `matches_manifest` doc must not intra-doc-link to
    // the private `Lockfile::resolve_offline` (rustdoc emits a warning
    // for public docs linking private items).
    let src = read("src/lockfile.rs");
    assert!(
        !src.contains("[`Lockfile::resolve_offline`]"),
        "src/lockfile.rs must not contain intra-doc link `[`Lockfile::resolve_offline`]` \
         in public docs (rustdoc warns: public doc links to private item)"
    );
}

#[test]
fn scheduler_doc_no_private_link() {
    // src/scheduler.rs `submit` doc must not intra-doc-link to the
    // private `MAX_TASKS` constant.
    let src = read("src/scheduler.rs");
    // The warning fires on the doc-comment form `[`MAX_TASKS`]`. Plain
    // code references like `MAX_TASKS` (no leading `[`) inside the
    // implementation are fine.
    assert!(
        !src.contains("[`MAX_TASKS`]"),
        "src/scheduler.rs must not contain intra-doc link `[`MAX_TASKS`]` \
         in public docs (rustdoc warns: public doc links to private item)"
    );
}

#[test]
fn suggest_doc_no_unescaped_html_tag() {
    // src/typechecker/suggest.rs module doc must not contain an
    // unescaped `<candidate>` sequence — rustdoc parses it as an
    // unclosed HTML tag.
    let src = read("src/typechecker/suggest.rs");
    assert!(
        !src.contains("<candidate>"),
        "src/typechecker/suggest.rs must not contain unescaped `<candidate>` \
         in doc comments (rustdoc warns: unclosed HTML tag `candidate`)"
    );
    // Also guard against the obvious sibling forms that would re-trigger
    // the same warning class.
    assert!(
        !src.contains("<typo>"),
        "src/typechecker/suggest.rs must not contain unescaped `<typo>` in doc comments"
    );
    assert!(
        !src.contains("<mod>"),
        "src/typechecker/suggest.rs must not contain unescaped `<mod>` in doc comments"
    );
}

// ── Round 76 L5canon: src/types/canonical.rs rustdoc fixes ──────────

/// `AliasInfo` doc used to intra-doc-link `[`TypeExpr`]` (a bare
/// item name). The `TypeExpr` type lives at `crate::ast::TypeExpr`,
/// not in `types::canonical`'s scope, so rustdoc emitted "unresolved
/// link" warnings. Round 76 L5canon switches the link to the fully-
/// qualified path `[crate::ast::TypeExpr]`.
#[test]
fn canonical_intra_doc_link_uses_full_path() {
    let src = read("src/types/canonical.rs");
    // The bare form must not be present any more.
    assert!(
        !src.contains("[`TypeExpr`]"),
        "src/types/canonical.rs must not contain bare intra-doc link `[`TypeExpr`]` \
         (rustdoc warns: unresolved link); use the fully-qualified `[crate::ast::TypeExpr]` form."
    );
}

/// `canonical_name` doc used to write `[`Symbol`](crate::intern::Symbol)`
/// — a redundant explicit link target. Rustdoc resolves the bare
/// `[`Symbol`]` form from the in-scope `use crate::intern::{Symbol,
/// ...}` import and emits a "redundant explicit link target"
/// warning when both the link text and the target are spelled out.
/// Round 76 L5canon strips the redundant target.
#[test]
fn canonical_no_redundant_link_target() {
    let src = read("src/types/canonical.rs");
    assert!(
        !src.contains("[`Symbol`](crate::intern::Symbol)"),
        "src/types/canonical.rs must not contain redundant explicit link target \
         `[`Symbol`](crate::intern::Symbol)` (rustdoc warns: redundant explicit link target); \
         the bare `[`Symbol`]` form resolves through the in-scope `use`."
    );
}

// ── Round 83 regression locks ───────────────────────────────────────
//
// Round 80 (commit 3cfc406) demoted `fn checksum_path_source` to
// `pub(crate)`, but the doc-comment on the public field
// `LockedPackage::checksum` still intra-doc-linked to it. Rustdoc
// emitted: "public documentation for `checksum` links to private item
// `checksum_path_source`". Round 83 reworded the doc to plain
// backticks (no square brackets). This test pins the fix.
#[test]
fn lockfile_checksum_doc_no_private_link() {
    let src = read("src/lockfile.rs");
    assert!(
        !src.contains("[`checksum_path_source`]"),
        "src/lockfile.rs must not contain intra-doc link `[`checksum_path_source`]` \
         on `LockedPackage::checksum` doc — `checksum_path_source` is `pub(crate)`, \
         so rustdoc warns: public doc links to private item. Use plain backticks instead."
    );
}

// Round 81 (commit f429a92) introduced a private
// `fn is_symbol_user_renameable_at_cursor`, but the doc-comment on the
// public `fn is_user_renameable` intra-doc-linked to it. Rustdoc warned
// twice (the linker doc and a reference line). Round 83 reworded the
// doc to plain backticks. This test pins the fix.
#[test]
fn rename_is_user_renameable_doc_no_private_link() {
    let src = read("src/lsp/rename.rs");
    assert!(
        !src.contains("[`is_symbol_user_renameable_at_cursor`]"),
        "src/lsp/rename.rs must not contain intra-doc link \
         `[`is_symbol_user_renameable_at_cursor`]` on `is_user_renameable` doc — \
         the linked helper is private, so rustdoc warns: public doc links to \
         private item. Use plain backticks instead."
    );
}

// ── Round 83 subprocess lock ────────────────────────────────────────
//
// The source-grep locks above catch reverts at the *known* sites. But
// rounds 80 and 81 introduced *new* rustdoc warnings at fresh sites
// that no grep could anticipate. Round 83's defence-in-depth: actually
// run `cargo doc --no-deps --release` and assert zero `^warning:`
// lines. This is slower (a few seconds incremental, longer cold) but
// catches arbitrary new drift the grep locks miss by design.
//
// Run with: `cargo test --test rustdoc_warnings_lock_tests
//            rustdoc_zero_warnings_via_subprocess -- --include-ignored`
// or normally — it is not `#[ignore]`d. CI executes the full suite.
#[test]
fn rustdoc_zero_warnings_via_subprocess() {
    use std::process::Command;

    // Honour the `CARGO` env var if cargo set it for us (it does when
    // running `cargo test`). Fall back to the bare `cargo` binary in
    // PATH otherwise.
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());

    let output = match Command::new(&cargo)
        .args([
            "doc",
            "--no-deps",
            "--release",
            "--quiet",
            "--manifest-path",
            "Cargo.toml",
        ])
        // Force colour off so any tty-detection in cargo can't sneak
        // ANSI bytes into the output we grep.
        .env("CARGO_TERM_COLOR", "never")
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            // We do NOT pass silently — surface the failure so a broken
            // toolchain can't hide a regression. The only case this
            // legitimately fires is if `cargo` isn't on PATH, which
            // shouldn't happen in any sane test environment.
            panic!(
                "failed to spawn `{} doc --no-deps --release --quiet`: {e}. \
                 The rustdoc-zero-warnings lock requires cargo to be available.",
                cargo.to_string_lossy()
            );
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        panic!(
            "`cargo doc --no-deps --release --quiet` failed (exit {:?}). \
             This lock requires `cargo doc` to succeed so we can count its warnings. \
             ── stdout ──\n{stdout}\n── stderr ──\n{stderr}",
            output.status.code()
        );
    }

    // Rustdoc writes warnings to stderr. Combine both streams to be
    // robust against future changes in cargo's plumbing.
    let combined = format!("{stdout}\n{stderr}");
    let warning_lines: Vec<&str> = combined
        .lines()
        .filter(|l| l.starts_with("warning:"))
        .collect();

    assert!(
        warning_lines.is_empty(),
        "`cargo doc --no-deps --release` produced {} `^warning:` line(s); \
         expected 0. This lock asserts the zero-rustdoc-warning invariant \
         introduced in round 75 L5. Fix the underlying warnings — do not \
         weaken this test.\n── warning lines ──\n{}\n── full stderr ──\n{stderr}",
        warning_lines.len(),
        warning_lines.join("\n")
    );
}
