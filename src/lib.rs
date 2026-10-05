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
//! Supporting modules: `formatter` (source formatting), `module` (module
//! resolution), `intern` (string interning), `disassemble` (bytecode
//! inspection). Optional features: `lsp`, `repl`, `watch`.

#![allow(clippy::mutable_key_type)]

pub mod ast;
pub mod builtins;
pub mod bytecode;
pub mod compiler;
pub mod defs;
pub mod diagnostic;
pub mod disassemble;
pub mod feature_stub;
pub mod file_discovery;
pub mod formatter;
pub mod fuzz_invariants;
pub mod git;
pub mod intern;
pub mod lexer;
pub mod lockfile;
#[cfg(feature = "lsp")]
pub mod lsp;
pub mod manifest;
pub mod module;
pub mod package_graph;
pub mod parser;
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
pub use vm::{Buffer, Clock, HostIo, Output, SystemClock, Vm, VmError};
