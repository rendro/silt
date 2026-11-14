//! # Silt Compiler and Runtime
//!
//! Silt source code flows through a five-stage pipeline:
//!
//! 1. **Lexer** (`lexer`) -- tokenizes source text into a stream of tokens.
//! 2. **Parser** (`parser`) -- builds an AST (`ast`) from the token stream.
//! 3. **Type checker** (`typechecker`) -- infers and validates types (`types`)
//!    across the AST, reporting diagnostics (`diagnostic`).
//! 4. **Compiler** (`compiler`) -- lowers the typed AST to bytecode (`bytecode`).
//! 5. **VM** (`vm`) -- executes bytecode, using the `scheduler` for
//!    concurrent tasks and `builtins` for the standard library.
//!
//! Supporting modules: `format` (source formatting), `module` (module
//! resolution), `intern` (string interning), `disassemble` (bytecode
//! inspection). Optional features: `lsp`, `repl`, `watch`.
//!
//! ## What the crate promises
//!
//! An embedder (docs/ffi.md) gets these names, and only these are
//! promised: `silt::Buffer`, `silt::Clock`, `silt::Exit`,
//! `silt::FromValue`, `silt::HostIo`, `silt::IntoValue`, `silt::Output`,
//! `silt::Value`, `silt::Vm`, `silt::VmError`, `silt::session::Builtins`,
//! `silt::session::Config`, `silt::session::Entry`,
//! `silt::session::HostCall`, `silt::session::HostModule`,
//! `silt::session::LockPolicy`, `silt::session::Program`,
//! `silt::session::ProjectSetup` and `silt::session::Session`, with
//! what they hand out (a `Value`'s variants, an analysis, a
//! diagnostic). Every other `pub` item is the crate's own: the
//! `silt` command, the language server, the playground and the tests
//! use it, and it changes without notice. The test API
//! (`silt::session::testing`, `silt::fuzz_invariants`,
//! `silt::scheduler::test_support`) is built with the `test-hooks`
//! feature only. (tests/meta/public_api_tests.rs keeps this list and
//! the guide one.)

#![allow(clippy::mutable_key_type)]

pub mod ast;
pub mod builtins;
pub mod bytecode;
pub(crate) mod compiler;
pub mod defs;
pub mod diagnostic;
pub mod disassemble;
pub(crate) mod evidence;
pub mod feature_stub;
pub mod file_discovery;
pub mod format;
#[cfg(any(test, feature = "test-hooks"))]
pub mod fuzz_invariants;
pub mod git;
#[cfg(feature = "http")]
pub mod http_wire;
pub mod intern;
pub mod layers;
pub mod lexer;
pub mod lockfile;
#[cfg(feature = "lsp")]
pub mod lsp;
pub mod manifest;
pub mod module;
pub mod package_graph;
pub mod parser;
pub(crate) mod program;
#[cfg(feature = "repl")]
pub mod repl;
pub mod scheduler;
pub mod session;
pub mod source;
pub mod typechecker;
pub mod typeinfo;
pub mod types;
// The self-updater shells out to curl/tar and replaces the running binary in
// place. Neither mechanism applies on wasm32 — the playground is embedded via
// the library path, not the CLI — so gate the module to native targets only.
pub mod runtime;
#[cfg(not(target_arch = "wasm32"))]
pub mod update;
pub mod value;
pub mod vm;
#[cfg(feature = "watch")]
pub mod watch;

// Re-export the value and conversion types embedders use.
pub use value::{FromValue, IntoValue, Value};
pub use vm::{Buffer, Clock, Exit, HostIo, Output, SystemClock, Vm, VmError};

/// The version of silt: what `silt --version` prints after the name.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
