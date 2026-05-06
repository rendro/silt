//! Round-74 GAP fix #5: `docs/language/modules.md` enumerates every
//! built-in module exposed via `silt::module::BUILTIN_MODULES`.
//!
//! Before round 74, `fs` (with `fs.list_dir`, `fs.stat`, `fs.read_link`,
//! `fs.walk`, `fs.glob`, etc.) and `env` (with `env.get/set/remove/vars`)
//! were absent from user-facing docs. Round 74 adds a "Built-in modules"
//! listing to `docs/language/modules.md` and this parity lock asserts
//! that every entry from `BUILTIN_MODULES` (`src/module.rs:4`) appears
//! in that listing — so adding a new builtin module without documenting
//! it fails the build.

use silt::module::BUILTIN_MODULES;

const MODULES_DOC_SRC: &str = include_str!("../docs/language/modules.md");

#[test]
fn modules_doc_lists_every_builtin_module() {
    let mut missing: Vec<&str> = Vec::new();
    for &name in BUILTIN_MODULES {
        // Each module must appear as a backticked code span at the
        // start of a row in the listing — `| \`<name>\` |` — so we
        // anchor to that form to avoid matching incidental prose.
        let row_token = format!("| `{name}` |");
        if !MODULES_DOC_SRC.contains(&row_token) {
            missing.push(name);
        }
    }
    assert!(
        missing.is_empty(),
        "docs/language/modules.md is missing built-in module rows for: {missing:?}\n\
         Every entry in `silt::module::BUILTIN_MODULES` must appear as `| \\`<name>\\` |` \
         in the Built-in modules listing."
    );
}

#[test]
fn modules_doc_calls_out_fs_helpers() {
    // Round-74 finding #5 specifically called out `fs.list_dir`,
    // `fs.stat`, `fs.read_link` as silently undocumented entry points.
    // Pin them so the listing can't be trimmed without notice.
    for needle in &[
        "fs.list_dir",
        "fs.stat",
        "fs.read_link",
    ] {
        assert!(
            MODULES_DOC_SRC.contains(needle),
            "docs/language/modules.md should mention `{needle}` in the fs row \
             (round-74 GAP-5 follow-up)."
        );
    }
}

#[test]
fn modules_doc_calls_out_env_helpers() {
    for needle in &[
        "env.get",
        "env.set",
        "env.remove",
        "env.vars",
    ] {
        assert!(
            MODULES_DOC_SRC.contains(needle),
            "docs/language/modules.md should mention `{needle}` in the env row \
             (round-74 GAP-5 follow-up)."
        );
    }
}
