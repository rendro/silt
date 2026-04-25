//! Round-35 DOC agent locks.
//!
//! These tests pin doc state round-35 fixed so it doesn't drift again:
//!
//! - F17a: `docs/language/traits.md` — the supertrait-bounds example must
//!   not use the non-existent `if`/`else` construct; it must use `match`.
//! - F17b: `docs/stdlib/tcp.md` — the `peer_addr` / `set_nodelay` rows
//!   must not carry the internal "PR-2 stub" label.
//! - F17c: `docs/stdlib/bytes.md` — must not version-pin `tcp` with
//!   "from v0.9" (tcp has shipped).
//! - F17d: `docs/language/modules.md` — the stale Built-in modules
//!   "Key Functions" table must be removed in favor of a pointer to the
//!   authoritative per-module index in `docs/stdlib/index.md`.

use std::path::{Path, PathBuf};

fn manifest_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e))
}

fn doc_path(rel: &[&str]) -> PathBuf {
    let mut p = manifest_dir().to_path_buf();
    for seg in rel {
        p.push(seg);
    }
    p
}

// ─── F17a: traits.md must not use `if`/`else` ─────────────────────────

#[test]
fn traits_md_supertrait_example_uses_match_not_if_else() {
    let path = doc_path(&["docs", "language", "traits.md"]);
    let body = read(&path);

    // Specific buggy snippet: `if a.equal(b)` must be gone.
    assert!(
        !body.contains("if a.equal(b)"),
        "docs/language/traits.md still contains `if a.equal(b)` — Silt has \
         no `if` keyword. Rewrite the snippet to use `match`."
    );

    // And the replacement must use `match a.equal(b)`.
    assert!(
        body.contains("match a.equal(b)"),
        "docs/language/traits.md is missing the `match a.equal(b)` \
         replacement snippet (the supertrait bounds example must exercise \
         the inherited `Equal::equal` through `match`)."
    );
}

// ─── F17b: tcp module doc must not carry the internal PR-2 label ─────

#[test]
fn tcp_md_has_no_pr_2_stub_label() {
    // Round 62 phase-2: the former `docs/stdlib/tcp.md` is now
    // inlined into `super::docs::TCP_MD` (in
    // `src/typechecker/builtins/docs.rs`) and surfaced via LSP
    // hover on every `tcp.*` builtin via `attach_module_overview`.
    // We pull any registered tcp.* doc as a representative sample
    // — the body is identical for every name.
    let docs = silt::typechecker::builtin_docs();
    let body = docs
        .keys()
        .filter(|k| k.starts_with("tcp."))
        .find_map(|k| docs.get(k))
        .cloned()
        .expect("expected at least one tcp.* builtin doc registered");
    assert!(
        !body.contains("PR-2"),
        "the tcp module doc (now inlined as `super::docs::TCP_MD` in \
         src/typechecker/builtins/docs.rs) still contains the internal \
         project label \"PR-2\" (used on the peer_addr / set_nodelay \
         rows). Rephrase to user-facing wording that matches the \
         runtime error."
    );
}

// ─── F17c: bytes module doc must not version-pin tcp ─────────────────

#[test]
fn bytes_md_has_no_v0_9_version_pin_on_tcp() {
    // Round 62 phase-2: bytes module markdown is `super::docs::BYTES_MD`.
    let docs = silt::typechecker::builtin_docs();
    let body = docs
        .keys()
        .filter(|k| k.starts_with("bytes."))
        .find_map(|k| docs.get(k))
        .cloned()
        .expect("expected at least one bytes.* builtin doc registered");
    assert!(
        !body.contains("from v0.9"),
        "the bytes module doc (now inlined as `super::docs::BYTES_MD`) \
         still contains the stale \"from v0.9\" version pin on the tcp \
         reference. TCP has shipped — drop the version tail."
    );
    assert!(
        !body.contains("v0.9"),
        "the bytes module doc still references `v0.9`; generalize the \
         wording so it does not pin to a shipped release."
    );
}

// ─── F17d: modules.md must not carry the stale Key Functions table ────

#[test]
fn modules_md_does_not_carry_stale_key_functions_table() {
    let path = doc_path(&["docs", "language", "modules.md"]);
    let body = read(&path);

    // The old table's header is "| Module    | Key Functions ..." — the
    // signature "Key Functions" column should be gone in favor of the
    // per-module LSP-delivered docs.
    assert!(
        !body.contains("Key Functions"),
        "docs/language/modules.md still contains the stale \"Key \
         Functions\" table. Delete it and point to the LSP-delivered \
         per-name docs (round 62 phase-2)."
    );

    // The replacement text should point users at the LSP for stdlib
    // discovery.
    assert!(
        body.contains("LSP") || body.contains("hover"),
        "docs/language/modules.md must point users at the LSP / editor \
         hover for built-in module docs (round 62 phase-2)."
    );
}
