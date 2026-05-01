//! `silt repl` — thin wrapper around `silt::repl::run_repl`.

use std::process;

#[cfg(feature = "repl")]
pub(crate) fn dispatch(args: &[String]) {
    for arg in &args[2..] {
        if arg == "--help" || arg == "-h" {
            println!("Usage: silt repl");
            println!();
            println!("Start an interactive REPL session. Type :help inside for commands.");
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
pub(crate) fn dispatch(_args: &[String]) {
    eprintln!("The 'repl' feature is not enabled. Rebuild with: cargo build --features repl");
    process::exit(1);
}
