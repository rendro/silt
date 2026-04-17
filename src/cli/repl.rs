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
    silt::repl::run_repl();
}

#[cfg(not(feature = "repl"))]
pub(crate) fn dispatch(_args: &[String]) {
    eprintln!("The 'repl' feature is not enabled. Rebuild with: cargo build --features repl");
    process::exit(1);
}
