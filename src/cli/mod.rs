//! CLI entry-point modules for the `silt` binary.
//!
//! Each subcommand owns its parsing, help text, and implementation in a
//! dedicated submodule under `crate::cli`. `main.rs` stays a thin
//! dispatcher — it decodes the top-level subcommand and delegates to the
//! matching `cli::<subcmd>::dispatch` function.
//!
//! Shared plumbing lives in the non-subcommand modules:
//!   - `pipeline` — the compile pipeline (`silt run|check|fmt|disasm|test`).
//!   - `package` — manifest/lockfile discovery.
//!   - `module_sources` — imported-module source lookup for error rendering.
//!   - `source_scan` — light text scans (`program_has_main`, etc.).
//!   - `paths` — filesystem path helpers.
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
pub(crate) mod module_sources;
pub(crate) mod package;
pub(crate) mod paths;
pub(crate) mod pipeline;
pub(crate) mod repl;
pub(crate) mod run;
pub(crate) mod self_update;
pub(crate) mod source_scan;
pub(crate) mod test;
pub(crate) mod update;
pub(crate) mod watch;
