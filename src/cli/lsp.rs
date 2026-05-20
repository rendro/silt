//! `silt lsp` — thin wrapper around `silt::lsp::run`.

use std::process;

/// Single source of truth for the `silt lsp --help` usage banner.
///
/// Both the feature-on path (which actually starts the LSP server)
/// and the feature-off stub render from this so they can't drift
/// apart. Round 88 GAP fix: `silt lsp --help` MUST print this banner
/// and exit 0 even when the `lsp` feature was compiled out, instead
/// of the pre-fix "feature not enabled" rebuild hint that blocked
/// users discovering the usage shape via the top-level `silt --help`
/// advertisement (`silt lsp  [feature: lsp]`).
pub(crate) fn usage_text() -> String {
    let mut s = String::new();
    s.push_str("Usage: silt lsp\n");
    s.push('\n');
    s.push_str("Start the silt language server. Communicates over stdio using the\n");
    s.push_str("Language Server Protocol — invoked automatically by editor extensions\n");
    s.push_str("(VS Code, Vim/Neovim, etc.). Not typically run directly from a terminal.\n");
    s
}

#[cfg(feature = "lsp")]
pub(crate) fn dispatch(args: &[String]) {
    for arg in &args[2..] {
        if arg == "--help" || arg == "-h" {
            print!("{}", usage_text());
            process::exit(0);
        }
    }
    // Reject unknown flags before starting the server.
    for arg in &args[2..] {
        if arg.starts_with('-') && arg != "--help" && arg != "-h" {
            eprintln!("silt lsp: unknown flag '{arg}'");
            eprintln!("Run 'silt lsp --help' for usage.");
            process::exit(1);
        }
    }
    // Reject positional arguments — the LSP server reads/writes
    // entirely over stdio and takes no positionals. Pre-fix the
    // dispatcher silently accepted them, which made `silt lsp foo`
    // look like it had worked. Mirror the rejection pattern used by
    // `silt fmt`, `silt run`, `silt update`, and `silt add`.
    for arg in &args[2..] {
        if !arg.starts_with('-') {
            eprintln!("silt lsp: unexpected argument '{arg}'");
            eprintln!("Run 'silt lsp --help' for usage.");
            process::exit(1);
        }
    }
    silt::lsp::run();
}

#[cfg(not(feature = "lsp"))]
pub(crate) fn dispatch(args: &[String]) {
    // Route through the shared pure helper so the `--help`/`-h` path
    // prints the usage banner (and exits 0) BEFORE the rebuild hint.
    // The disabled-feature regression suite at
    // tests/round88_disabled_feature_help_tests.rs exercises the
    // helper directly so this branch is covered regardless of which
    // features the test binary was compiled with.
    let tail: &[String] = if args.len() > 2 { &args[2..] } else { &[] };
    let (code, msg) = silt::feature_stub::disabled_feature_response(tail, "lsp", &usage_text());
    if code == 0 {
        print!("{msg}");
    } else {
        eprintln!("{msg}");
    }
    process::exit(code);
}
