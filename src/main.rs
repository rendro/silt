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

use crate::cli::help::{GLOBAL_FLAGS, usage_text};
use crate::cli::watch::maybe_handle_watch;

// Windows defaults to a 1 MiB main-thread stack which is too tight for
// silt's recursive walks (type canonicalization, auto-derive synthesis,
// row-polymorphic field unification, supertrait obligation chasing).
// Linux/macOS default to 8 MiB and are unaffected. Spawn the entire
// dispatcher on a worker thread with an explicit 8 MiB reserve so the
// build behaves identically across platforms — same trick rustc uses.
const SILT_STACK_SIZE: usize = 8 * 1024 * 1024;

fn main() {
    let args: Vec<String> = env::args().collect();
    let join = std::thread::Builder::new()
        .name("silt-main".into())
        .stack_size(SILT_STACK_SIZE)
        .spawn(move || run_main(args))
        .expect("spawning silt main worker thread");
    if let Err(payload) = join.join() {
        std::panic::resume_unwind(payload);
    }
}

fn run_main(args: Vec<String>) {
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
        "--help" | "-h" => {
            print!("{}", usage_text());
            process::exit(0);
        }
        // `silt help` with no extra args mirrors `silt --help`. With a
        // trailing token (`silt help fmt`) it drills into the
        // per-subcommand help by re-dispatching as `silt <cmd> --help`.
        // This matches the convention from cargo / git / rustup so users
        // who type `silt help <X>` reflexively get what they expect
        // instead of the global banner.
        "help" => {
            if args.len() >= 3 {
                let sub = args[2].clone();
                let mut forwarded: Vec<String> = Vec::with_capacity(3 + args.len() - 3);
                forwarded.push(args[0].clone());
                forwarded.push(sub);
                forwarded.push("--help".to_string());
                // Pass through any extra trailing tokens — most
                // subcommands ignore them after `--help`, but keeping
                // them lets future per-subcommand `help <topic>`
                // routing work without another touch here.
                for extra in &args[3..] {
                    forwarded.push(extra.clone());
                }
                run_main(forwarded);
                return;
            }
            print!("{}", usage_text());
            process::exit(0);
        }
        "run" => cli::run::dispatch(&args),
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
            eprintln!("Unknown command: '{other}'");
            if let Some(suggestion) = suggest_subcommand(other) {
                eprintln!("  Did you mean '{suggestion}'?");
            }
            eprintln!("Run 'silt' with no arguments to see available commands.");
            process::exit(1);
        }
    }
}

/// Levenshtein edit distance between two strings, computed with the
/// classic two-row dynamic-programming table. Used only by
/// [`suggest_subcommand`] for the "did you mean" hint on unknown
/// `silt <subcommand>` invocations, so we keep the implementation
/// local rather than re-using `crate::typechecker::suggest::levenshtein`
/// (which lives behind `pub(super)` visibility).
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let n = a.len();
    let m = b.len();
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut curr: Vec<usize> = vec![0; m + 1];
    for i in 1..=n {
        curr[0] = i;
        for j in 1..=m {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[m]
}

/// List of valid `silt` subcommands, used to power the "did you mean"
/// hint for unknown commands. Must stay in sync with the match arms in
/// [`run_main`] — a regression test in
/// `tests/cli_help_and_unknown_subcommand_tests.rs` exercises a typo of
/// each of these to lock them together.
const SUBCOMMANDS: &[&str] = &[
    "run",
    "disasm",
    "test",
    "check",
    "lsp",
    "repl",
    "fmt",
    "init",
    "self-update",
    "update",
    "add",
];

/// Find the closest valid token to `typo` within edit distance 2.
/// Returns `None` if no candidate is within that distance — in which
/// case the caller should suppress the suggestion line entirely rather
/// than offer a wildly unrelated command.
///
/// If `typo` starts with `-` we search the recognized global flag
/// list (`--version`, `--help`, …); otherwise we search the
/// subcommand list. Mixing the two pools would let `silt run` get
/// suggested for `silt -ru` (edit distance 2 to `--run`-shaped flags),
/// which is more confusing than helpful.
fn suggest_subcommand(typo: &str) -> Option<&'static str> {
    let pool: &[&'static str] = if typo.starts_with('-') {
        GLOBAL_FLAGS
    } else {
        SUBCOMMANDS
    };
    let mut best: Option<(&'static str, usize)> = None;
    for &candidate in pool {
        let d = edit_distance(typo, candidate);
        if d <= 2 {
            match best {
                Some((_, bd)) if d >= bd => {}
                _ => best = Some((candidate, d)),
            }
        }
    }
    best.map(|(name, _)| name)
}
