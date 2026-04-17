//! Top-level help text, usage banners, and the shared signature/description
//! alignment plumbing. Pulled out of `main.rs` so the banner helpers can
//! share one source-of-truth and the `tests/cli_round26_tests.rs` lockers
//! keep passing across refactors.

use crate::cli::features::enabled_features;

/// Render the usage text shown by `silt --help` and the no-args screen.
///
/// Subcommands gated by Cargo features are annotated inline with the
/// feature they require, and the bottom line lists which features were
/// compiled in. This lets users discover missing features BEFORE running
/// a subcommand that would otherwise fail with "The 'X' feature is not
/// enabled" only after invocation.
pub(crate) fn usage_text() -> String {
    // Mark feature-gated subcommands with a `[feature: X]` suffix. The
    // marker is present regardless of whether the feature is compiled in —
    // that way `silt --help` is identical across builds and the user can
    // see what a richer build would offer.
    //
    // Alignment is structural: each row is `  <signature (padded to SIG_WIDTH)>  <desc>`.
    // Widen SIG_WIDTH if a new signature exceeds it — the help-row
    // alignment tests in tests/cli_test_rendering_tests.rs and
    // tests/cli_round26_tests.rs will fail otherwise.
    //
    // Round-26 L9.2: SIG_WIDTH widened to fit the full
    // `silt add <name> --git <url> [--rev|--branch|--tag <ref>]`
    // signature (58 chars) so its description column aligns with the
    // other rows instead of being pushed right by 12 characters.
    const SIG_WIDTH: usize = 58;
    let line = |sig: &str, desc: &str| format!("  {sig:<SIG_WIDTH$}  {desc}\n");
    let run_desc: String = {
        let mut d = String::from("Run a program");
        if !cfg!(feature = "watch") {
            d.push_str("  [--watch requires feature: watch]");
        }
        d
    };
    let mut out = String::new();
    out.push_str("silt — a statically-typed, expression-based language\n");
    out.push('\n');
    out.push_str("Usage:\n");
    out.push_str(&line(
        "silt run [--watch] [--disassemble] <file.silt>",
        &run_desc,
    ));
    out.push_str(&line(
        "silt check [--format json] [--watch] <file.silt>",
        "Type-check without running",
    ));
    out.push_str(&line(
        "silt test [--filter <pattern>] [--watch] [path]",
        "Run test functions",
    ));
    out.push_str(&line("silt fmt [--check] [files...]", "Format source code"));
    out.push_str(&line("silt repl", "Interactive REPL  [feature: repl]"));
    out.push_str(&line(
        "silt init",
        "Create a new silt package in this directory",
    ));
    out.push_str(&line(
        "silt lsp",
        "Start the language server  [feature: lsp]",
    ));
    out.push_str(&line(
        "silt disasm [--watch] [<file.silt>]",
        "Show bytecode disassembly",
    ));
    out.push_str(&line(
        "silt self-update [--dry-run] [--force]",
        "Update the silt binary to the latest release",
    ));
    out.push_str(&line(
        "silt update [<dep-name>]",
        "Regenerate silt.lock for the current package's dependencies",
    ));
    out.push_str(&line(
        "silt add <name> --path <path>",
        "Add a path-based dependency to silt.toml",
    ));
    out.push_str(&line(
        "silt add <name> --git <url> [--rev|--branch|--tag <ref>]",
        "Add a git-based dependency to silt.toml",
    ));
    out.push('\n');
    out.push_str(&format!("Enabled features: {}\n", enabled_features()));
    out
}

/// Single source of truth for the `silt check` usage banner line.
/// Both the `--help` path and the "no arguments given" path render
/// from this so they can't drift apart. A regression test in
/// tests/cli.rs asserts the two banners are byte-identical.
pub(crate) fn check_usage_banner() -> &'static str {
    "silt check [--format json] [--watch] <file.silt>"
}

/// Single source of truth for the `silt run` usage banner line.
///
/// Four code paths print this — `--help`, no-args, the watch
/// dry-validation gate, and the missing-file-after-flags fallback.
/// Keeping them all rendering from this helper is locked by
/// `tests/run_banner_consistency_tests.rs::test_silt_run_banner_consistency_all_paths`.
///
/// We deliberately keep `<file.silt>` (without optional brackets) so the
/// banner stays byte-identical across paths even though `silt run` now
/// also accepts no file argument when invoked inside a package — the
/// optional-no-arg behavior is documented separately in the help text.
pub(crate) fn run_usage_banner() -> &'static str {
    "silt run [--watch] [--disassemble] <file.silt>"
}

/// Single source of truth for the `silt disasm` usage banner line.
///
/// Three code paths print this — `--help`, the watch dry-validation
/// gate when outside a package, and the bare-invocation fallback
/// outside a package. The bracketed form reflects that `<file.silt>`
/// is optional when invoked inside a silt package (the entry point is
/// resolved from the manifest). The round-26 `cli_round26_tests.rs`
/// suite locks this helper so the three banners can't drift apart.
pub(crate) fn disasm_usage_banner() -> &'static str {
    "silt disasm [<file.silt>]"
}

/// Full body of `silt run --help`, rendered to stdout. Shared by the
/// explicit `silt run --help` path and the legacy
/// `silt <file>.silt --help` convenience shim so the two can't drift
/// apart (round-26 G4).
pub(crate) fn run_help_text() -> String {
    let mut s = String::new();
    s.push_str(&format!("Usage: {}\n", run_usage_banner()));
    s.push('\n');
    s.push_str("Options:\n");
    s.push_str("  --watch, -w     Re-run on file changes\n");
    s.push_str("  --disassemble   Show bytecode disassembly instead of running\n");
    s.push('\n');
    s.push_str("Examples:\n");
    s.push_str("  silt run                      (inside a package, runs src/main.silt)\n");
    s.push_str("  silt run main.silt\n");
    s.push_str("  silt run --watch main.silt\n");
    s.push_str("  silt run --disassemble main.silt\n");
    s
}
