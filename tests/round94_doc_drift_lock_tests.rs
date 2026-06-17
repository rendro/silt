//! Round 94 doc-drift locks.
//!
//! These are SOURCE-GREP locks over the documentation files: they fail the
//! moment a known-bad string returns to a doc, independent of any runtime
//! behavior. Each corresponds to a confirmed documentation-drift finding.

const DESIGN_DECISIONS: &str = include_str!("../docs/language/design-decisions.md");
const LOOPS_AND_PIPES: &str = include_str!("../docs/language/loops-and-pipes.md");
const ERROR_FROM_TRAIT: &str = include_str!("../docs/proposals/error-from-trait.md");
const MODULES: &str = include_str!("../docs/language/modules.md");

// Finding 1: `string.concat` does not exist; only `string.join` does.
#[test]
fn design_decisions_has_no_string_concat() {
    assert!(
        !DESIGN_DECISIONS.contains("string.concat"),
        "design-decisions.md references nonexistent `string.concat`; use `string.join`"
    );
    assert!(
        DESIGN_DECISIONS.contains("string.join"),
        "design-decisions.md should keep the `string.join` reference"
    );
}

#[test]
fn loops_and_pipes_has_no_string_concat() {
    assert!(
        !LOOPS_AND_PIPES.contains("string.concat"),
        "loops-and-pipes.md references nonexistent `string.concat`; use `string.join`"
    );
    assert!(
        LOOPS_AND_PIPES.contains("string.join"),
        "loops-and-pipes.md should keep the `string.join` reference"
    );
}

// Finding 2: silt has no `impl` keyword; trait impls are `trait <Name> for <Type>`.
// The "works in silt today" block must not contain Rust-style `impl IntoAppError for`.
#[test]
fn error_from_trait_works_today_block_uses_trait_not_impl() {
    assert!(
        !ERROR_FROM_TRAIT.contains("impl IntoAppError for"),
        "error-from-trait.md uses Rust-style `impl IntoAppError for`; \
         the works-today block must use `trait IntoAppError for`"
    );
    assert!(
        ERROR_FROM_TRAIT.contains("trait IntoAppError for"),
        "error-from-trait.md should use the corrected `trait IntoAppError for` form"
    );
}

// Finding 3: circular-import error string drift.
#[test]
fn modules_circular_import_message_matches_runtime() {
    assert!(
        !MODULES.contains("error: circular import:"),
        "modules.md shows the stale `error: circular import:` prefix"
    );
    assert!(
        MODULES.contains("circular import detected"),
        "modules.md should show the real `circular import detected` wording"
    );
}
