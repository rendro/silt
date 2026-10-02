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
pub(crate) mod repl;
pub(crate) mod run;
pub(crate) mod self_update;
pub(crate) mod test;
pub(crate) mod update;
pub(crate) mod watch;
