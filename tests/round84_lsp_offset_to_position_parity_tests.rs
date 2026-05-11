//! Round 84 — BLOAT dedup lock.
//!
//! Background: `offset_to_position` lived as a private fn in
//! `src/lsp/document_symbols.rs` and `src/lsp/code_action.rs`, and as
//! inline computations inside `src/lsp/semantic_tokens.rs` and
//! `src/lsp/conversions.rs::binding_range`. Four near-identical
//! implementations of the same byte-offset → UTF-16 LSP `Position`
//! computation. They were collapsed to one canonical helper in
//! `src/lsp/conversions.rs::offset_to_position`.
//!
//! The behavioural parity tests live inline in
//! `src/lsp/conversions.rs`'s `#[cfg(test)] mod tests` (alongside the
//! sibling `span_to_position` / `position_to_offset` tests) because
//! the helper is `pub(crate)` and not reachable from integration
//! tests. This integration test enforces the **single-definition
//! invariant**: a source-grep over `src/lsp/*.rs` must find exactly
//! one `fn offset_to_position` definition, and it must live in
//! `conversions.rs`. Any future copy-paste regression trips this test.

use std::fs;
use std::path::Path;

/// There must be exactly ONE `fn offset_to_position` definition across
/// `src/lsp/*.rs` — the canonical one in `src/lsp/conversions.rs`.
///
/// We match function-definition lines (`fn offset_to_position(` with
/// optional `pub`/`pub(crate)`/`pub(super)` prefix), so call sites,
/// imports, doc comments, and string-literal mentions are NOT counted.
#[test]
fn offset_to_position_is_defined_exactly_once_in_lsp_tree() {
    let lsp_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("lsp");
    let mut definers: Vec<String> = Vec::new();

    for entry in fs::read_dir(&lsp_dir).expect("read src/lsp") {
        let entry = entry.expect("read_dir entry");
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        let src = fs::read_to_string(&path).expect("read .rs file");
        for line in src.lines() {
            let trimmed = line.trim_start();
            // Strip any leading visibility modifier so we can check
            // for the bare `fn offset_to_position(` opener uniformly.
            let after_vis = trimmed
                .strip_prefix("pub(crate) ")
                .or_else(|| trimmed.strip_prefix("pub(super) "))
                .or_else(|| trimmed.strip_prefix("pub "))
                .unwrap_or(trimmed);
            if after_vis.starts_with("fn offset_to_position(") {
                definers.push(format!(
                    "{}: {}",
                    path.file_name().unwrap().to_string_lossy(),
                    trimmed
                ));
            }
        }
    }

    assert_eq!(
        definers.len(),
        1,
        "expected exactly one `fn offset_to_position` definition in src/lsp/, found {}: {:#?}",
        definers.len(),
        definers
    );
    assert!(
        definers[0].starts_with("conversions.rs:"),
        "the canonical definition must live in conversions.rs, got: {}",
        definers[0]
    );
}

/// Companion lock: the four sites that previously held duplicate
/// definitions must still REFERENCE `offset_to_position` (as a call
/// site or import), proving they were rewired to the canonical helper
/// rather than silently dropped. If a future refactor removes the
/// reference from any of these files, an audit reviewer should
/// re-verify the call path still goes through the canonical helper.
#[test]
fn original_duplicate_sites_now_reference_canonical_helper() {
    let lsp_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("lsp");
    // Files that previously held a duplicate implementation. Each must
    // now mention `offset_to_position` (either by importing or by
    // calling it) — otherwise the dedup was incomplete.
    let consumers = [
        "document_symbols.rs",
        "code_action.rs",
        "semantic_tokens.rs",
        // conversions.rs is the DEFINER, so it always mentions the
        // name; including it would be tautological. binding_range's
        // delegation is exercised by inline tests.
    ];
    for f in &consumers {
        let path = lsp_dir.join(f);
        let src = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));
        assert!(
            src.contains("offset_to_position"),
            "{} no longer references `offset_to_position`; either the \
             dedup is incomplete or this consumer was removed — re-check \
             the audit-round-84 rewiring",
            f
        );
    }
}
