//! `silt repl` — thin wrapper around `silt::repl::run_repl`.

use std::process;

/// Single source of truth for the `silt repl --help` usage banner.
///
/// Both the feature-on path (which actually starts a REPL) and the
/// feature-off stub render from this so they can't drift apart. The
/// stub still needs the banner because `silt --help` advertises
/// `silt repl  [feature: repl]` to all users regardless of compiled
/// features (round 87 G — discoverability), and Round 88 GAP fixed
/// the follow-up: `silt repl --help` MUST print this banner and exit 0
/// even when the `repl` feature was compiled out, instead of the
/// pre-fix "feature not enabled" rebuild hint that blocked discovery.
pub(crate) fn usage_text() -> String {
    let mut s = String::new();
    s.push_str("Usage: silt repl\n");
    s.push('\n');
    s.push_str("Start an interactive REPL session. Type :help inside for commands.\n");
    s
}

#[cfg(feature = "repl")]
pub(crate) fn dispatch(args: &[String]) {
    for arg in &args[2..] {
        if arg == "--help" || arg == "-h" {
            // Trailing newline already in usage_text; use print! to avoid double blank line.
            print!("{}", usage_text());
            process::exit(0);
        }
    }
    // Reject unknown flags before starting the REPL.
    for arg in &args[2..] {
        if arg.starts_with('-') && arg != "--help" && arg != "-h" {
            eprintln!("silt repl: unknown flag '{arg}'");
            eprintln!("Run 'silt repl --help' for usage.");
            process::exit(1);
        }
    }
    // Reject positional arguments — the REPL takes none. Pre-fix the
    // dispatcher silently accepted (and ignored) any positional, which
    // made typos like `silt repl my_project` look like they had
    // worked. Mirror the rejection pattern used by `silt fmt`,
    // `silt run`, `silt update`, and `silt add`.
    for arg in &args[2..] {
        if !arg.starts_with('-') {
            eprintln!("silt repl: unexpected argument '{arg}'");
            eprintln!("Run 'silt repl --help' for usage.");
            process::exit(1);
        }
    }
    silt::repl::run_repl();
}

#[cfg(not(feature = "repl"))]
pub(crate) fn dispatch(args: &[String]) {
    // Route through the shared pure helper so the `--help`/`-h` path
    // prints the usage banner (and exits 0) BEFORE the rebuild hint.
    // The disabled-feature regression suite at
    // tests/round88_disabled_feature_help_tests.rs exercises the
    // helper directly so this branch is covered regardless of which
    // features the test binary was compiled with.
    let tail: &[String] = if args.len() > 2 { &args[2..] } else { &[] };
    let (code, msg) = silt::feature_stub::disabled_feature_response(tail, "repl", &usage_text());
    if code == 0 {
        print!("{msg}");
    } else {
        eprintln!("{msg}");
    }
    process::exit(code);
}
