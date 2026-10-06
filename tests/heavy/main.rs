//! Test suite: the two largest legacy suites (integration, integration_concurrency)
//! and the formatter's property sweep.
//!
//! One test binary; each module was a separate test crate before.

mod checker_scaling;
#[path = "../frontend/fmt_property/mod.rs"]
mod fmt_property;
mod fmt_property_sweep_tests;
mod integration;
