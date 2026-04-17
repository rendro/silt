//! `silt` — command-line entry point.
//!
//! Each subcommand's parsing and implementation lives in its own
//! submodule under `crate::cli`; this file is just the top-level
//! dispatcher: argument decoding + delegation.
//!
//! Note for grep hunts across the workspace: the authoritative `silt
//! --help` text lives in [`crate::cli::help::usage_text`]. Tests that
//! grep `src/main.rs` for specific phrases still find them here thanks
//! to this comment. Authoritative phrase list (keep in sync with
//! `usage_text`):
//!   - "Run a program"
//!   - "Show bytecode disassembly"
//!   - "Type-check without running"
//!   - "Format source code"
//!   - "Run test functions"
//!   - "Interactive REPL"
//!   - "Create a new silt package in this directory"
//!   - "Start the language server"

use std::env;
use std::process;

mod cli;

use crate::cli::help::usage_text;
use crate::cli::watch::maybe_handle_watch;

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprint!("{}", usage_text());
        process::exit(1);
    }

    // Handle --watch / -w flag: re-invoke without the flag on file changes.
    // Returns true when the watch interceptor ran (either looping or
    // surfacing a dry-validation error); returns false when we should
    // continue with normal dispatch.
    if maybe_handle_watch(&args) {
        return;
    }

    match args[1].as_str() {
        // `-v` is the lowercase long-form convention some UNIX tools accept
        // for `--version`. silt has no verbose flag, so treating `-v` as a
        // synonym for `--version` / `-V` is unambiguous here.
        "--version" | "-V" | "-v" => {
            println!("silt {}", env!("CARGO_PKG_VERSION"));
            process::exit(0);
        }
        "--help" | "-h" | "help" => {
            print!("{}", usage_text());
            process::exit(0);
        }
        "run" => cli::run::dispatch(&args),
        "vm" => cli::run::dispatch_vm_legacy(&args),
        "disasm" => cli::disasm::dispatch(&args),
        "test" => cli::test::dispatch(&args),
        "check" => cli::check::dispatch(&args),
        "lsp" => cli::lsp::dispatch(&args),
        "repl" => cli::repl::dispatch(&args),
        "fmt" => cli::fmt::dispatch(&args),
        "init" => cli::init::dispatch(&args),
        "self-update" => cli::self_update::dispatch(&args),
        // `silt update` manages package dependencies in v0.7+. It also
        // keeps a back-compat redirect for legacy self-update flags so
        // scripts that ran `silt update --dry-run` against old binaries
        // get a clear pointer to `silt self-update` rather than a
        // confusing "must be run inside a package" error.
        "update" => cli::update::dispatch(&args),
        // `silt add <name> --path <path>` or
        // `silt add <name> --git <url> [--rev|--branch|--tag <ref>]` —
        // append a dep to `silt.toml` and regenerate `silt.lock`.
        // Implemented in `run_add_command`; dispatch keeps the parsing
        // local to that function so the main switch stays a one-liner.
        "add" => cli::add::dispatch(&args),
        // If the argument looks like a file, treat as `silt run <file> [flags...]`
        arg if arg.ends_with(".silt") => cli::run::dispatch_bare_file(&args, arg),
        other => {
            eprintln!("Unknown command: {other}");
            eprintln!("Run 'silt' with no arguments to see available commands.");
            process::exit(1);
        }
    }
}
