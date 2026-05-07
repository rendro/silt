//! Round-75 doc-currency lock tests.
//!
//! Each test pins a docs-side fix from round 75:
//!
//! - DOC-1 (BROKEN): `docs/strict-effects-migration.md` and
//!   `docs/proposals/effect-rows.md` previously claimed Phases A→D
//!   shipped in v0.12. They actually shipped in v0.13 — git tag
//!   `4bb2e9d` (v0.12 bump) predates the phase A landing
//!   (`768c74d`); v0.13 (`9e533cf`) is the first release that
//!   includes all four phases.
//! - DOC-2 (BROKEN): `docs/language/modules.md` listed `task.scope`
//!   as a real builtin. The actual task-module functions are
//!   `task.spawn`, `task.join`, `task.cancel`, `task.deadline`,
//!   `task.spawn_until` (see `src/typechecker/builtins.rs:455+`).
//! - DOC-4 (GAP): `docs/language/generics.md` "supertrait expansion"
//!   used `Ordered` as if it were built-in. The auto-derived set is
//!   `Equal`, `Hash`, `Compare`, `Display` (see
//!   `src/typechecker/auto_derive.rs`). The fix rewrites the example
//!   to either explicitly define `Ordered` on-page or to mention one
//!   of the four real auto-derived traits.
//! - DOC-5 (GAP): `editors/vscode/README.md` "What works" listed only
//!   7 features. The LSP server advertises 18 (see
//!   `src/lsp/mod.rs:430-475`); the canonical user-facing list lives
//!   in `docs/editor-setup.md`.
//! - VM-DOC (GAP): `src/typechecker/builtins/docs.rs` task.cancel doc
//!   body documented the running and parked cases but was silent on
//!   the queued-but-not-yet-running case.

const STRICT_EFFECTS_MIGRATION: &str =
    include_str!("../docs/strict-effects-migration.md");
const EFFECT_ROWS_PROPOSAL: &str = include_str!("../docs/proposals/effect-rows.md");
const MODULES_DOC: &str = include_str!("../docs/language/modules.md");
const GENERICS_DOC: &str = include_str!("../docs/language/generics.md");
const VSCODE_README: &str = include_str!("../editors/vscode/README.md");
const BUILTINS_DOCS_RS: &str = include_str!("../src/typechecker/builtins/docs.rs");

#[test]
fn effect_rows_phases_say_v0_13() {
    // The migration guide must say v0.13, not v0.12, for the Phase D
    // shipping claim.
    assert!(
        STRICT_EFFECTS_MIGRATION.contains("v0.13"),
        "docs/strict-effects-migration.md should mention v0.13 \
         (the actual release in which Phases A→D shipped)."
    );
    assert!(
        !STRICT_EFFECTS_MIGRATION.contains("Phase D of the effect-row tracking proposal in v0.12"),
        "docs/strict-effects-migration.md still claims Phase D \
         shipped in v0.12. The phases shipped in v0.13 (git tag \
         `9e533cf`); v0.12 (`4bb2e9d`) was tagged before phase A \
         (`768c74d`) landed."
    );
}

#[test]
fn effect_rows_proposal_says_v0_13() {
    assert!(
        EFFECT_ROWS_PROPOSAL.contains("v0.13"),
        "docs/proposals/effect-rows.md should mention v0.13 (the \
         actual release in which Phases A→D shipped)."
    );
    assert!(
        !EFFECT_ROWS_PROPOSAL.contains("Phases A→D shipped in v0.12"),
        "docs/proposals/effect-rows.md still claims Phases A→D \
         shipped in v0.12. The phases shipped in v0.13 (git tag \
         `9e533cf`); v0.12 (`4bb2e9d`) was tagged before phase A \
         (`768c74d`) landed."
    );
}

#[test]
fn modules_doc_lists_real_task_builtins() {
    // Find the table row for the `task` module and assert it does
    // NOT list `task.scope` (which doesn't exist) and DOES list the
    // real builtins.
    let task_row = MODULES_DOC
        .lines()
        .find(|line| line.starts_with("| `task` |"))
        .expect(
            "docs/language/modules.md should contain a table row \
             for the `task` module starting `| `task` |`",
        );
    assert!(
        !task_row.contains("task.scope"),
        "docs/language/modules.md task row still lists `task.scope`, \
         which is not a real builtin. Real items: task.spawn, \
         task.join, task.cancel, task.deadline, task.spawn_until \
         (see src/typechecker/builtins.rs:455+). Row was: {task_row:?}"
    );
    assert!(
        task_row.contains("task.cancel"),
        "docs/language/modules.md task row should mention \
         `task.cancel`. Row was: {task_row:?}"
    );
    assert!(
        task_row.contains("task.deadline"),
        "docs/language/modules.md task row should mention \
         `task.deadline`. Row was: {task_row:?}"
    );
}

#[test]
fn generics_doc_no_phantom_ordered_as_builtin() {
    // The four auto-derived traits are Equal, Hash, Compare, Display
    // (see src/typechecker/auto_derive.rs). The supertrait-expansion
    // example must either define `Ordered` on-page (so the reader
    // sees the supertrait edge being established) or explicitly
    // reference one of the four real auto-derived names.
    //
    // Find the section header.
    let idx = GENERICS_DOC
        .find("### Supertrait expansion")
        .expect("docs/language/generics.md should have a `### Supertrait expansion` section");
    // Take a window covering the section body (up to the next `###`).
    let after = &GENERICS_DOC[idx..];
    let end = after[3..]
        .find("\n### ")
        .map(|p| p + 3)
        .unwrap_or(after.len());
    let section = &after[..end];

    let defines_ordered_on_page = section.contains("trait Ordered");
    let mentions_real_auto_derived = section.contains("Equal")
        && (section.contains("Hash")
            || section.contains("Compare")
            || section.contains("Display"));

    assert!(
        defines_ordered_on_page || mentions_real_auto_derived,
        "docs/language/generics.md `Supertrait expansion` section must \
         either explicitly define `Ordered` on-page (with `trait \
         Ordered: ...`) or reference one of the four real \
         auto-derived traits (Equal/Hash/Compare/Display). \
         Section body was:\n{section}"
    );

    // Also check that the four-auto-derived list nearby still names
    // the four real traits.
    let four_section_idx = GENERICS_DOC
        .find("### The four auto-derived traits")
        .expect(
            "docs/language/generics.md should have a `### The four \
             auto-derived traits` section",
        );
    let four_section = &GENERICS_DOC[four_section_idx..];
    assert!(
        four_section.contains("Equal")
            && four_section.contains("Hash")
            && four_section.contains("Compare")
            && four_section.contains("Display"),
        "docs/language/generics.md `The four auto-derived traits` \
         section should name all four: Equal, Hash, Compare, Display."
    );
}

#[test]
fn vscode_readme_lists_lsp_features_in_parity_with_editor_setup_docs() {
    // Tokens that prove the README's "What works" list grew from the
    // round-74 7-item list to mirror the canonical 18-feature
    // `docs/editor-setup.md` table.
    let tokens = [
        "rename",
        "references",
        "workspace symbol",
        "inlay hint",
        "code action",
    ];
    let lower = VSCODE_README.to_lowercase();
    for tok in tokens {
        assert!(
            lower.contains(tok),
            "editors/vscode/README.md should mention `{tok}` in its \
             feature list (parity with docs/editor-setup.md)."
        );
    }
}

#[test]
fn task_cancel_doc_explains_queued_case() {
    // The markdown blob for task.cancel in src/typechecker/builtins/docs.rs
    // must mention the queued / not-yet-running case in addition to the
    // running and parked cases.
    let lower = BUILTINS_DOCS_RS.to_lowercase();
    let queued_words =
        lower.contains("queued") || lower.contains("not yet running") || lower.contains("not-yet-running");
    assert!(
        queued_words,
        "src/typechecker/builtins/docs.rs task.cancel doc body should \
         mention the queued / not-yet-running case (the task may run a \
         slice before its result is discarded; first-writer-wins)."
    );
    // Specifically locate the task.cancel section and check that the
    // queued-case prose lives within it.
    let idx = BUILTINS_DOCS_RS
        .find("## `task.cancel`")
        .expect("src/typechecker/builtins/docs.rs should contain a `## `task.cancel`` section");
    let after = &BUILTINS_DOCS_RS[idx..];
    let end = after[3..]
        .find("\n## ")
        .map(|p| p + 3)
        .unwrap_or(after.len());
    let section = &after[..end];
    let section_lower = section.to_lowercase();
    assert!(
        section_lower.contains("queued")
            || section_lower.contains("not yet running")
            || section_lower.contains("not-yet-running"),
        "task.cancel doc section in src/typechecker/builtins/docs.rs \
         should explain the queued / not-yet-running case. Section \
         body:\n{section}"
    );
}
