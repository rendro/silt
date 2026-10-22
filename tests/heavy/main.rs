//! Test suite: the two largest legacy suites (integration, integration_concurrency),
//! the formatter's property sweep, and the language server's work over
//! many files.
//!
//! One test binary; each module was a separate test crate before.

mod checker_scaling;
#[path = "../frontend/fmt_property/mod.rs"]
mod fmt_property;
mod fmt_property_sweep_tests;
mod init_order_sweep;
mod integration;
mod lexer_scaling;
#[cfg(feature = "lsp")]
mod lsp_workspace_perf;
#[cfg(feature = "http")]
#[path = "../support/port_file.rs"]
mod port_file;
#[path = "../support/quiet.rs"]
mod quiet;
mod quiet_tests;
#[cfg(feature = "lsp")]
#[path = "../lsp/rename_sweep.rs"]
mod rename_sweep;
#[cfg(feature = "lsp")]
mod rename_sweep_examples;
mod repl_paste;
mod repl_session;
mod runtime_scaling;
#[cfg(feature = "lsp")]
#[path = "../lsp/support.rs"]
mod support;
