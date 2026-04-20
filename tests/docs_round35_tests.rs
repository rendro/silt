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

// ─── F17b: tcp.md must not carry the internal PR-2 label ──────────────

#[test]
fn tcp_md_has_no_pr_2_stub_label() {
    let path = doc_path(&["docs", "stdlib", "tcp.md"]);
    let body = read(&path);
    assert!(
        !body.contains("PR-2"),
        "docs/stdlib/tcp.md still contains the internal project label \
         \"PR-2\" (used on the peer_addr / set_nodelay rows). Rephrase to \
         user-facing wording that matches the runtime error."
    );
}

// ─── F17c: bytes.md must not version-pin tcp ──────────────────────────

#[test]
fn bytes_md_has_no_v0_9_version_pin_on_tcp() {
    let path = doc_path(&["docs", "stdlib", "bytes.md"]);
    let body = read(&path);
    assert!(
        !body.contains("from v0.9"),
        "docs/stdlib/bytes.md still contains the stale \"from v0.9\" \
         version pin on the tcp reference. TCP has shipped — drop the \
         version tail."
    );
    // Broader check: no v0.9 reference at all in bytes.md.
    assert!(
        !body.contains("v0.9"),
        "docs/stdlib/bytes.md still references `v0.9`; generalize the \
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
    // per-module stdlib index.
    assert!(
        !body.contains("Key Functions"),
        "docs/language/modules.md still contains the stale \"Key \
         Functions\" table. Delete it and point to docs/stdlib/index.md \
         (the authoritative source)."
    );

    // And the replacement text must point at docs/stdlib/index.md.
    assert!(
        body.contains("stdlib/index.md"),
        "docs/language/modules.md must point to `docs/stdlib/index.md` as \
         the authoritative source for built-in modules."
    );
}
