//! Round 88 GAP — `silt repl --help` and `silt lsp --help` must print
//! the usage banner and exit 0 even when their gating Cargo feature is
//! NOT compiled in.
//!
//! Pre-fix, the `#[cfg(not(feature = "repl"))]` and
//! `#[cfg(not(feature = "lsp"))]` stubs at `src/cli/repl.rs:39-42`
//! and `src/cli/lsp.rs:41-44` printed "The 'X' feature is not
//! enabled. Rebuild with: cargo build --features X" and exit 1
//! UNCONDITIONALLY — including when the user passed `--help` or
//! `-h`. That blocks the discovery flow advertised by the top-level
//! `silt --help` output, which lists `silt repl  [feature: repl]`
//! and `silt lsp  [feature: lsp]` regardless of compiled features
//! and invites users to type `silt <cmd> --help` for the usage shape.
//!
//! The fix factored the disabled-feature branch into the pure
//! `silt::feature_stub::disabled_feature_response(args, feature,
//! usage_text)` helper that returns `(exit_code, output_string)`.
//! When `--help`/`-h` appears in `args`, it returns
//! `(0, usage_text)`; otherwise it returns `(1, "<rebuild hint>")`.
//! The CLI stubs at `src/cli/repl.rs` and `src/cli/lsp.rs` route
//! through this helper.
//!
//! Why this test calls the helper directly instead of spawning a
//! `--no-default-features --features lsp` binary: building a side
//! binary with a different feature set inside a `#[test]` would
//! require either `escargot`, a build.rs trick, or a separate
//! Cargo invocation — all painful and slow. The task spec
//! explicitly allows the "factor into a testable function" route,
//! and the helper is the actual branch that the CLI stubs call, so
//! exercising it gives the same coverage as a side-binary test
//! would. The matrix sweep (`cargo build --no-default-features
//! --features lsp` / `--features repl`) in the verification step
//! confirms the stubs still compile and call the helper correctly.

use std::process;

/// Sanity that `process::id()` returns a non-zero value — the harness
/// uses it as the disambiguator for any temp paths the test might
/// create. The actual regression doesn't touch the filesystem, but
/// the task spec calls out the convention so we exercise it once.
fn test_pid() -> u32 {
    let pid = process::id();
    assert!(pid > 0, "process::id() must be non-zero");
    pid
}

/// `--help` MUST return exit 0 and the usage banner, even when the
/// gating feature is disabled. Locks the GAP fix for both the `repl`
/// and `lsp` stubs.
#[test]
fn help_flag_returns_usage_and_zero_exit_for_repl() {
    let _pid = test_pid();
    let args = vec!["--help".to_string()];
    let usage = "Usage: silt repl\n\nStart an interactive REPL session.\n";
    let (code, out) = silt::feature_stub::disabled_feature_response(&args, "repl", usage);
    assert_eq!(code, 0, "--help must exit 0, got {code}");
    assert!(
        out.contains("Usage:"),
        "--help output must contain 'Usage:', got: {out:?}"
    );
    assert!(
        out.contains("silt repl"),
        "--help output must contain the command name 'silt repl', got: {out:?}"
    );
    // Must NOT leak the rebuild hint into the help path.
    assert!(
        !out.contains("feature is not enabled"),
        "--help output must not include the rebuild hint, got: {out:?}"
    );
}

#[test]
fn help_flag_returns_usage_and_zero_exit_for_lsp() {
    let _pid = test_pid();
    let args = vec!["--help".to_string()];
    let usage = "Usage: silt lsp\n\nStart the silt language server. Communicates over stdio...\n";
    let (code, out) = silt::feature_stub::disabled_feature_response(&args, "lsp", usage);
    assert_eq!(code, 0, "--help must exit 0, got {code}");
    assert!(
        out.contains("Usage:"),
        "--help output must contain 'Usage:', got: {out:?}"
    );
    assert!(
        out.contains("silt lsp"),
        "--help output must contain the command name 'silt lsp', got: {out:?}"
    );
}

/// Short form `-h` MUST be honored just like `--help`.
#[test]
fn short_help_flag_returns_usage_and_zero_exit() {
    let _pid = test_pid();
    let args = vec!["-h".to_string()];
    let usage = "Usage: silt repl\n";
    let (code, out) = silt::feature_stub::disabled_feature_response(&args, "repl", usage);
    assert_eq!(code, 0, "-h must exit 0, got {code}");
    assert!(
        out.contains("Usage:"),
        "-h output must contain 'Usage:', got: {out:?}"
    );
}

/// `--help` anywhere in the arg list still triggers the help path —
/// no positional / flag-order coupling.
#[test]
fn help_flag_works_after_other_args() {
    let _pid = test_pid();
    let args = vec!["--unknown".to_string(), "--help".to_string()];
    let usage = "Usage: silt lsp\n";
    let (code, out) = silt::feature_stub::disabled_feature_response(&args, "lsp", usage);
    assert_eq!(code, 0, "--help mixed with other args must still exit 0");
    assert!(out.contains("Usage:"));
}

/// Empty arg list (`silt repl` with no flags) MUST emit the rebuild
/// hint and exit 1 — the disabled-feature error path must still fire
/// when the user did NOT ask for help.
#[test]
fn empty_args_returns_rebuild_hint_and_exit_one_for_repl() {
    let _pid = test_pid();
    let args: Vec<String> = Vec::new();
    let usage = "Usage: silt repl\n";
    let (code, out) = silt::feature_stub::disabled_feature_response(&args, "repl", usage);
    assert_eq!(code, 1, "no args must exit 1, got {code}");
    assert!(
        out.contains("feature is not enabled"),
        "rebuild hint must mention 'feature is not enabled', got: {out:?}"
    );
    assert!(
        out.contains("cargo build --features repl"),
        "rebuild hint must include the cargo command, got: {out:?}"
    );
    assert!(
        out.contains("'repl'"),
        "rebuild hint must name the 'repl' feature, got: {out:?}"
    );
}

#[test]
fn empty_args_returns_rebuild_hint_and_exit_one_for_lsp() {
    let _pid = test_pid();
    let args: Vec<String> = Vec::new();
    let usage = "Usage: silt lsp\n";
    let (code, out) = silt::feature_stub::disabled_feature_response(&args, "lsp", usage);
    assert_eq!(code, 1, "no args must exit 1, got {code}");
    assert!(
        out.contains("feature is not enabled"),
        "rebuild hint must mention 'feature is not enabled', got: {out:?}"
    );
    assert!(
        out.contains("cargo build --features lsp"),
        "rebuild hint must include the cargo command, got: {out:?}"
    );
}

/// Non-help flags (e.g. `--strict`) MUST take the rebuild-hint path,
/// not the help path. Otherwise any unknown flag could accidentally
/// suppress the feature-disabled error.
#[test]
fn non_help_flags_take_the_rebuild_hint_path() {
    let _pid = test_pid();
    let args = vec!["--strict".to_string(), "foo".to_string()];
    let usage = "Usage: silt repl\n";
    let (code, out) = silt::feature_stub::disabled_feature_response(&args, "repl", usage);
    assert_eq!(code, 1, "non-help args must exit 1");
    assert!(out.contains("feature is not enabled"));
}

/// The `repl` and `lsp` cli modules each expose a `usage_text()`
/// helper that the feature-on path prints on `--help`. The
/// disabled-feature stubs route through the same helper so the
/// banner cannot drift between the two paths. We can't call the
/// `cli::*::usage_text` functions from this integration test (the
/// `cli` module is `mod`, not `pub mod`), so we lock the banner
/// shape via the production-side asserts above: both stub paths
/// contain "Usage:" and the command name. Round 26 / 36 / 67
/// already lock the structural banner-alignment policy in
/// tests/cli_round*_tests.rs.
#[test]
fn usage_text_contract_is_usage_colon_prefix() {
    let _pid = test_pid();
    // Exercise the helper with a banner that resembles what the
    // production usage_text() emits: starts with "Usage: " and ends
    // with a newline.
    let banner = "Usage: silt repl\n\nbody\n";
    let args = vec!["--help".to_string()];
    let (code, out) = silt::feature_stub::disabled_feature_response(&args, "repl", banner);
    assert_eq!(code, 0);
    assert!(
        out.starts_with("Usage:"),
        "usage banner must start with 'Usage:', got: {out:?}"
    );
    assert!(
        out.ends_with('\n'),
        "usage banner must end with a newline, got: {out:?}"
    );
}
