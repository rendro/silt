//! # Silt Compiler and Runtime
//!
//! Silt source code flows through a five-stage pipeline:
//!
//! 1. **Lexer** (`lexer`) -- tokenizes source text into a stream of tokens.
//! 2. **Parser** (`parser`) -- builds an AST (`ast`) from the token stream.
//! 3. **Type checker** (`typechecker`) -- infers and validates types (`types`)
//!    across the AST, reporting diagnostics via `errors`.
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
pub mod disassemble;
pub mod errors;
pub mod formatter;
pub mod intern;
pub mod lexer;
#[cfg(feature = "lsp")]
pub mod lsp;
pub mod module;
pub mod parser;
#[cfg(feature = "repl")]
pub mod repl;
pub mod scheduler;
pub mod typechecker;
pub mod types;
// The self-updater shells out to curl/tar and replaces the running binary in
// place. Neither mechanism applies on wasm32 — the playground is embedded via
// the library path, not the CLI — so gate the module to native targets only.
#[cfg(not(target_arch = "wasm32"))]
pub mod update;
pub mod value;
pub mod vm;
#[cfg(feature = "watch")]
pub mod watch;

// Re-export FFI types for embedders.
pub use value::{FromValue, IntoValue, Value};
pub use vm::{Vm, VmError};
