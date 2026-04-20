//! Builtin function modules for the Silt VM.
//!
//! Each submodule implements a family of builtin functions (e.g. `string.*`,
//! `list.*`) and exposes a single `call` entry point that the main VM dispatch
//! delegates to.

pub mod bytes;
pub mod collections;
pub mod concurrency;
pub mod core;
pub mod crypto;
pub mod data;
pub mod encoding;
pub mod io;
pub mod numeric;
#[cfg(feature = "postgres")]
pub mod postgres;
pub mod stream;
pub mod string;
#[cfg(feature = "tcp")]
pub mod tcp;
