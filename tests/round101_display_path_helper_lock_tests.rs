//! Round-101 LATENT lock: the error-rendering path-normalization policy
//! ("render paths in the style the user typed on the command line",
//! audit rounds 17/21 F13 + G1/G2) used to exist as a byte-identical
//! 21-line closure duplicated between `silt run` (src/cli/run.rs) and
//! `silt test` (src/cli/test.rs) — an acknowledged-mirror drift hazard:
//! a future path-rendering fix applied to only one file would silently
//! leave the other subcommand rendering the old style.
//!
//! Fix: the body is hoisted into the single shared helper
//! `crate::cli::paths::display_path_for`; both subcommands build their
//! local `normalize_path` closure from it.
//!
//! Lock approach (source grep — the helper is bin-crate-private, so its
//! direct behavioral locks live as unit tests in src/cli/paths.rs
//! `display_path_for_*`, and the end-to-end rendering behavior stays
//! pinned by tests/cli_test_rendering_tests.rs
//! `test_run_module_error_paths_consistently_normalized` /
//! `test_test_setup_error_paths_normalized` /
//! `test_cross_module_call_stack_uses_consistent_path_style`, which are
//! unchanged by this refactor):
//!   1. neither run.rs nor test.rs contains the duplicated closure body
//!      anymore (its unambiguous fingerprint: `candidate.strip_prefix`);
//!   2. both delegate to `display_path_for`;
//!   3. the helper is defined exactly once, in src/cli/paths.rs.
//!
//! Mutation reasoning: re-inlining the logic into either file (the
//! pre-round-101 shape) reintroduces `candidate.strip_prefix` there
//! and/or drops the `display_path_for` call, tripping the asserts
//! below.

const CLI_RUN_RS: &str = include_str!("../src/cli/run.rs");
const CLI_TEST_RS: &str = include_str!("../src/cli/test.rs");
const CLI_PATHS_RS: &str = include_str!("../src/cli/paths.rs");

/// The duplicated closure body must not live in run.rs or test.rs.
/// `candidate.strip_prefix` is the fingerprint of the old inline body:
/// test.rs has other (str-typed) `strip_prefix` calls for flag parsing,
/// so the receiver-qualified form is the unambiguous marker.
#[test]
fn run_and_test_no_longer_inline_the_normalization_body() {
    for (name, src) in [
        ("src/cli/run.rs", CLI_RUN_RS),
        ("src/cli/test.rs", CLI_TEST_RS),
    ] {
        assert!(
            !src.contains("candidate.strip_prefix"),
            "{name} contains `candidate.strip_prefix` — the path-normalization \
             body has been re-inlined (pre-round-101 duplicated-closure shape). \
             Path-display policy must live only in \
             `crate::cli::paths::display_path_for` so `silt run` and \
             `silt test` error rendering cannot drift."
        );
        assert!(
            !src.contains("candidate.is_absolute()"),
            "{name} contains `candidate.is_absolute()` — the absolute-branch \
             half of the old duplicated normalization closure is back inline. \
             Route it through `crate::cli::paths::display_path_for` instead."
        );
    }
}

/// Both subcommands must build their `normalize_path` closure from the
/// shared helper.
#[test]
fn run_and_test_both_delegate_to_the_shared_helper() {
    for (name, src) in [
        ("src/cli/run.rs", CLI_RUN_RS),
        ("src/cli/test.rs", CLI_TEST_RS),
    ] {
        assert!(
            src.contains("display_path_for("),
            "{name} no longer calls `display_path_for` — its error-path \
             rendering has been unwired from the shared policy helper. If the \
             call was legitimately refactored, it must still render error and \
             call-stack frame paths in the style the user typed (see the \
             behavioral locks in tests/cli_test_rendering_tests.rs); update \
             this lock to the new spelling only after confirming that."
        );
    }
}

/// The policy must have exactly one definition, and it must live in
/// src/cli/paths.rs — a second `fn display_path_for` anywhere in the
/// CLI is the mirror-drift hazard reborn under the helper's own name.
#[test]
fn helper_is_defined_exactly_once_in_paths_rs() {
    // The paren-qualified needle skips the helper's own unit tests
    // (`fn display_path_for_relative_...` etc.) in src/cli/paths.rs.
    assert_eq!(
        CLI_PATHS_RS.matches("fn display_path_for(").count(),
        1,
        "src/cli/paths.rs must define `fn display_path_for` exactly once"
    );
    for (name, src) in [
        ("src/cli/run.rs", CLI_RUN_RS),
        ("src/cli/test.rs", CLI_TEST_RS),
    ] {
        assert_eq!(
            src.matches("fn display_path_for(").count(),
            0,
            "{name} defines its own `fn display_path_for` — the path-display \
             policy must have a single definition in src/cli/paths.rs"
        );
    }
}
