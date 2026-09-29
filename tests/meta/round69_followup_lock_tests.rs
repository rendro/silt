//! Round-69 follow-up locks.
//!
//! Two findings that the main round-69 commit deferred and the user then
//! asked for in follow-up:
//!
//! 1. **VS Code extension `package.json` was missing publication metadata**
//!    (`repository`, `homepage`, `license`, `author`, `bugs`). When the
//!    extension is published to the marketplace these fields populate the
//!    sidebar (GitHub link, license badge, issue tracker). They are
//!    cheap to add and easy to forget on subsequent edits, so we lock
//!    each field so a future maintainer cannot drop them silently.
//!
//! 2. **Dead `iter_primitives` / `iter_containers` helpers** in
//!    `src/types/builtins.rs` had no callers outside their own self-test.
//!    The deletion is a no-op only if `iter_all().filter(|b| b.kind == X)`
//!    produces the same partition. The test below pins both counts
//!    against the authoritative `BUILTIN_TYPES` slice so a future
//!    refactor that drops a `Primitive` or `Container` entry can't
//!    silently change the partition.

use std::path::PathBuf;

fn read_repo_file(rel: &str) -> String {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e))
}

#[test]
fn vscode_package_json_carries_publication_metadata() {
    let body = read_repo_file("editors/vscode/package.json");
    let parsed: serde_json::Value =
        serde_json::from_str(&body).expect("editors/vscode/package.json must parse as JSON");

    // Required top-level string fields. Each populates a marketplace
    // sidebar element.
    for key in ["license", "author", "homepage"] {
        let v = parsed.get(key).unwrap_or_else(|| {
            panic!(
                "editors/vscode/package.json must declare `{key}` (round-69 \
                 follow-up: marketplace sidebar metadata)"
            )
        });
        assert!(
            v.as_str().is_some_and(|s| !s.is_empty()),
            "editors/vscode/package.json `{key}` must be a non-empty string"
        );
    }

    // Repository must be an object pointing at the silt monorepo with
    // `directory` set so the marketplace links into the extension
    // subtree, not the crate root.
    let repo = parsed.get("repository").expect(
        "editors/vscode/package.json must declare `repository` (object with type/url/directory)",
    );
    let repo_obj = repo.as_object().expect("`repository` must be an object");
    assert_eq!(
        repo_obj.get("type").and_then(|v| v.as_str()),
        Some("git"),
        "`repository.type` should be `git`"
    );
    let url = repo_obj
        .get("url")
        .and_then(|v| v.as_str())
        .expect("`repository.url` must be a string");
    assert!(
        url.contains("github.com") && url.contains("silt"),
        "`repository.url` should point at the silt repo on GitHub, got `{url}`"
    );
    assert!(
        repo_obj.get("directory").is_some(),
        "`repository.directory` should be set so the marketplace links into editors/vscode"
    );

    // Bugs URL keeps users one click from the issue tracker.
    let bugs = parsed
        .get("bugs")
        .expect("editors/vscode/package.json must declare `bugs`");
    let bugs_url = bugs
        .get("url")
        .and_then(|v| v.as_str())
        .expect("`bugs.url` must be a string");
    assert!(
        bugs_url.contains("github.com"),
        "`bugs.url` should point at the GitHub issue tracker"
    );

    // License must match the workspace.
    assert_eq!(
        parsed.get("license").and_then(|v| v.as_str()),
        Some("MIT"),
        "`license` should match the workspace MIT license"
    );
}

#[test]
fn iter_all_partitions_into_primitives_and_containers() {
    // Round-69 follow-up: deleted dead `iter_primitives` /
    // `iter_containers` helpers. The single existing self-test moved to
    // an explicit `iter_all().filter(...)` form. This sibling test
    // pins the same partition from outside the module so a future
    // edit that introduces a new `BuiltinKind` variant can't silently
    // bypass either branch.
    use silt::types::builtins::{BuiltinKind, iter_all};

    let prim_count = iter_all()
        .filter(|b| b.kind == BuiltinKind::Primitive)
        .count();
    let cont_count = iter_all()
        .filter(|b| b.kind == BuiltinKind::Container)
        .count();
    let all_count = iter_all().count();

    assert!(prim_count > 0, "must register at least one primitive");
    assert!(cont_count > 0, "must register at least one container");
    assert_eq!(
        prim_count + cont_count,
        all_count,
        "BuiltinKind must remain a two-way partition (Primitive | Container) — \
         a new variant requires updating this lock and every consumer of \
         BUILTIN_TYPES."
    );
}
