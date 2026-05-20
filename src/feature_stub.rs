//! Pure helper for `--help` handling in feature-gated subcommands when
//! the gating feature is disabled.
//!
//! Background — Round 88 GAP: `silt repl --help` and `silt lsp --help`
//! used to short-circuit to "feature not enabled, rebuild with: cargo
//! build --features X" and exit 1, even though the user had asked for
//! the usage banner. The top-level `silt --help` advertises both
//! subcommands (annotated `[feature: repl]` / `[feature: lsp]`), so
//! users who read it then type `silt <cmd> --help` to discover the
//! usage shape — and would get blocked on a build whose default
//! feature set happened to exclude that subcommand.
//!
//! The fix routes both the feature-on and feature-off paths through
//! one shared usage string per subcommand. When the gating feature is
//! disabled the dispatcher delegates to
//! [`disabled_feature_response`], which honors `--help`/`-h` BEFORE
//! emitting the "feature not enabled" error.
//!
//! Returning `(exit_code, message)` rather than calling `process::exit`
//! directly is what makes this branch testable from
//! `tests/round88_disabled_feature_help_tests.rs` without having to
//! build a side binary with a different feature set.

/// Compute the response a feature-gated subcommand should emit when
/// its gating feature is NOT compiled in.
///
/// `args` is the post-subcommand argument slice (i.e. `&full_args[2..]`)
/// — what the user typed AFTER `silt <cmd>`. `feature` is the Cargo
/// feature name (e.g. `"repl"`, `"lsp"`). `usage_text` is the full
/// usage banner string that should be returned when the user asked
/// for help.
///
/// Returns `(exit_code, output_string)`:
/// - If `args` contains `--help` or `-h`: `(0, usage_text.to_string())`.
/// - Otherwise: `(1, "The '<feature>' feature is not enabled. Rebuild
///   with: cargo build --features <feature>")`.
///
/// The "feature is not enabled" wording is kept stable so that
/// existing user muscle memory and downstream tooling that greps for
/// it (and the regression test at
/// `tests/round88_disabled_feature_help_tests.rs`) continue to work.
pub fn disabled_feature_response(args: &[String], feature: &str, usage_text: &str) -> (i32, String) {
    for arg in args {
        if arg == "--help" || arg == "-h" {
            return (0, usage_text.to_string());
        }
    }
    (
        1,
        format!(
            "The '{feature}' feature is not enabled. Rebuild with: cargo build --features {feature}"
        ),
    )
}
