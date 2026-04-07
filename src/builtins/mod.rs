//! Builtin function modules for the Silt VM.
//!
//! Each submodule implements a family of builtin functions (e.g. `string.*`,
//! `list.*`) and exposes a single `call` entry point that the main VM dispatch
//! delegates to.

pub mod collections;
pub mod concurrency;
pub mod core;
pub mod data;
pub mod io;
pub mod numeric;
pub mod string;
