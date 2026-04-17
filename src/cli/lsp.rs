//! `silt lsp` — thin wrapper around `silt::lsp::run`.

use std::process;

#[cfg(feature = "lsp")]
pub(crate) fn dispatch(args: &[String]) {
    for arg in &args[2..] {
        if arg == "--help" || arg == "-h" {
            println!("Usage: silt lsp");
            println!();
            println!("Start the silt language server. Communicates over stdio using the");
            println!("Language Server Protocol — invoked automatically by editor extensions");
            println!("(VS Code, Vim/Neovim, etc.). Not typically run directly from a terminal.");
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
    silt::lsp::run();
}

#[cfg(not(feature = "lsp"))]
pub(crate) fn dispatch(_args: &[String]) {
    eprintln!("The 'lsp' feature is not enabled. Rebuild with: cargo build --features lsp");
    process::exit(1);
}
