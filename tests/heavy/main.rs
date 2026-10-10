//! Test suite: the two largest legacy suites (integration, integration_concurrency),
//! the formatter's property sweep, and the language server's work over
//! many files.
//!
//! One test binary; each module was a separate test crate before.

mod checker_scaling;
#[path = "../frontend/fmt_property/mod.rs"]
mod fmt_property;
mod fmt_property_sweep_tests;
mod integration;
mod lexer_scaling;
mod lsp_workspace_perf;
#[path = "../lsp/rename_sweep.rs"]
mod rename_sweep;
mod rename_sweep_examples;
#[path = "../lsp/support.rs"]
mod support;
