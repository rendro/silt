//! CLI entry-point modules for the `silt` binary.
//!
//! Each subcommand owns its parsing, help text, and implementation in a
//! dedicated submodule under `crate::cli`. `main.rs` stays a thin
//! dispatcher — it decodes the top-level subcommand and delegates to the
//! matching `cli::<subcmd>::dispatch` function.
//!
//! Shared plumbing lives in the non-subcommand modules:
//!   - `package` — manifest/lockfile discovery.
//!   - `paths` — filesystem path helpers, and the session of an entry
//!     file (`silt run|check|disasm|test` each drive it themselves).
//!   - `help` — usage banners and the top-level `--help` text.
//!   - `features` — `cfg!(feature = ...)` list for the help footer.
//!   - `watch` — the `--watch` interceptor.

pub(crate) mod add;
pub(crate) mod check;
pub(crate) mod disasm;
pub(crate) mod features;
pub(crate) mod fmt;
pub(crate) mod help;
pub(crate) mod init;
pub(crate) mod lsp;
pub(crate) mod package;
pub(crate) mod paths;
pub(crate) mod remove;
pub(crate) mod repl;
pub(crate) mod run;
pub(crate) mod self_update;
pub(crate) mod test;
pub(crate) mod update;
pub(crate) mod watch;

/// The usage error of a flag given twice to `silt <command>`: exit 1.
pub(crate) fn once_only(command: &str, flag: &str) -> ! {
    let command = match command {
        "" => "silt".to_string(),
        command => format!("silt {command}"),
    };
    eprintln!("error: {command}: {flag} was specified more than once");
    std::process::exit(1);
}

/// The first line of the usage error of `silt <command>` given no file
/// outside a package, where there is no `src/main.silt` to take.
pub(crate) fn no_file(command: &str) {
    eprintln!(
        "error: silt {command}: no file given, and no silt.toml found in this directory or any parent"
    );
}
