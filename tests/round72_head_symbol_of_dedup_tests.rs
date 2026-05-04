//! Round 72 LATENT L2: lock test for the `head_symbol_of` /
//! `head_symbol_of_canon` deduplication.
//!
//! Pre-fix state. Two byte-identical copies of the same `Type ->
//! Option<Symbol>` mapping lived in:
//!
//!   - `src/typechecker/mod.rs:6164-6185` (`fn head_symbol_of`)
//!   - `src/types/canonical.rs:511-532`   (`fn head_symbol_of_canon`)
//!
//! The doc comment on the canonical-module copy admitted the
//! duplication: *"Mirror of the typechecker's `head_symbol_of`
//! (kept here so this module owns the alias-routing logic without
//! crossing crate-internal boundaries)."* Same drift class as the
//! round 71 `Fn` regression, where one copy returned `intern("Fun")`
//! while the others returned `intern("Fn")` and the user's trait
//! impl silently failed to dispatch.
//!
//! Post-fix state. `head_symbol_of_canon` is now `pub(crate)` in
//! `src/types/canonical.rs` and re-exported from
//! `src/typechecker/mod.rs` via
//! `pub(crate) use crate::types::canonical::head_symbol_of_canon as head_symbol_of;`
//! There is exactly ONE definition of the function. These tests
//! grep the source tree to lock that invariant — adding a second
//! definition (whether under either name) is a regression even if
//! the bodies happen to agree at the moment of authorship.

/// The canonical definition lives in `src/types/canonical.rs` and
/// nowhere else. Search for `fn head_symbol_of_canon` (the function
/// definition syntax, including any visibility modifier) and assert
/// exactly ONE occurrence across all `.rs` files under `src/`.
#[test]
fn only_one_definition_of_head_symbol_of_canon() {
    let files: Vec<std::path::PathBuf> = walk_rs("src");
    let mut hits: Vec<(std::path::PathBuf, usize)> = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).expect("read source");
        for (lineno, line) in text.lines().enumerate() {
            // Match the function-definition syntax. We look for the
            // exact "fn head_symbol_of_canon(" head — with optional
            // visibility before it — so that comments mentioning the
            // name (`/// Mirror of head_symbol_of_canon ...`) don't
            // count.
            let trimmed = line.trim_start();
            if trimmed.starts_with("fn head_symbol_of_canon(")
                || trimmed.starts_with("pub fn head_symbol_of_canon(")
                || trimmed.starts_with("pub(crate) fn head_symbol_of_canon(")
                || trimmed.starts_with("pub(super) fn head_symbol_of_canon(")
            {
                hits.push((f.clone(), lineno + 1));
            }
        }
    }
    assert_eq!(
        hits.len(),
        1,
        "expected exactly ONE definition of `fn head_symbol_of_canon` in src/; found {}: {:?}",
        hits.len(),
        hits
    );
    let (path, _line) = &hits[0];
    let path_str = path.to_string_lossy();
    assert!(
        path_str.ends_with("types/canonical.rs"),
        "the canonical definition must live in src/types/canonical.rs; \
         found at {path_str}"
    );
}

/// Conjugate lock: a freestanding `fn head_symbol_of(` definition
/// (no `_canon` suffix) must NOT exist under `src/`. The
/// typechecker references the canonical-module copy via a
/// `pub(crate) use ... as head_symbol_of;` re-export, which doesn't
/// match the `fn head_symbol_of(` head we grep for — so the lock
/// fires the moment someone reintroduces a local body.
#[test]
fn no_freestanding_head_symbol_of_definition() {
    let files: Vec<std::path::PathBuf> = walk_rs("src");
    let mut hits: Vec<(std::path::PathBuf, usize, String)> = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).expect("read source");
        for (lineno, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("fn head_symbol_of(")
                || trimmed.starts_with("pub fn head_symbol_of(")
                || trimmed.starts_with("pub(crate) fn head_symbol_of(")
                || trimmed.starts_with("pub(super) fn head_symbol_of(")
            {
                hits.push((f.clone(), lineno + 1, line.to_string()));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "expected ZERO standalone `fn head_symbol_of(` definitions in src/; \
         found {} — the round-72 LATENT L2 dedup lock is broken: {:?}",
        hits.len(),
        hits
    );
}

/// The typechecker side must reach the canonical-module copy via a
/// `use` re-export, not a fresh body. This pins the bridge edge
/// (the `pub(crate) use crate::types::canonical::head_symbol_of_canon`
/// declaration) so a partial revert that removes the re-export
/// without re-adding a local body — leaving callers broken — is
/// caught at test time rather than build time on a downstream commit.
#[test]
fn typechecker_re_exports_canonical_head_symbol_of() {
    let src = include_str!("../src/typechecker/mod.rs");
    assert!(
        src.contains(
            "pub(crate) use crate::types::canonical::head_symbol_of_canon as head_symbol_of;"
        ),
        "expected `src/typechecker/mod.rs` to re-export `head_symbol_of_canon` \
         as `head_symbol_of`; the round-72 LATENT L2 dedup bridge is broken"
    );
}

// ── helpers ──────────────────────────────────────────────────────────

fn walk_rs(root: &str) -> Vec<std::path::PathBuf> {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let start = manifest.join(root);
    let mut out = Vec::new();
    walk_into(&start, &mut out);
    out
}

fn walk_into(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_into(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}
