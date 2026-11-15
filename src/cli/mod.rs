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

/// The commands of `silt`, each with what runs it: the one list of them.
/// `run_main` dispatches by it, the "did you mean" hint of an unknown
/// command searches it, and the usage text has a row for each of them
/// and for no other (a test of `help` holds that). The order is the
/// hint's: of two commands as close to a typo, the first is named.
pub(crate) const COMMANDS: &[(&str, fn(&[String]))] = &[
    ("run", run::dispatch),
    ("disasm", disasm::dispatch),
    ("test", test::dispatch),
    ("check", check::dispatch),
    ("lsp", lsp::dispatch),
    ("repl", repl::dispatch),
    ("fmt", fmt::dispatch),
    ("init", init::dispatch),
    ("self-update", self_update::dispatch),
    // Regenerates the lock of the package's dependencies.
    ("update", update::dispatch),
    // `silt add <name> --path <path>` or `silt add <name> --git <url>
    // (--rev|--branch|--tag) <ref>`: a dependency into `silt.toml`, and
    // `silt.lock` regenerated.
    ("add", add::dispatch),
    // A dependency out of `silt.toml`, and `silt.lock` regenerated.
    ("remove", remove::dispatch),
];

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

/// `silt <command>` refuses to start where the environment sets the
/// runtime's settings (`SILT_IO_TIMEOUT`, `SILT_TIME_SLICE`) to a value
/// silt cannot read: exit 1, as for a bad flag.
pub(crate) fn refuse_bad_settings(command: &str) {
    if let Some(error) = silt::scheduler::settings_error() {
        eprintln!("error: silt {command}: {error}");
        std::process::exit(1);
    }
}
