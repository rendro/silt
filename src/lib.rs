#![allow(clippy::mutable_key_type)]

pub mod ast;
pub mod builtins;
pub mod bytecode;
pub mod compiler;
pub mod disassemble;
pub mod errors;
pub mod formatter;
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
pub mod value;
pub mod vm;
#[cfg(feature = "watch")]
pub mod watch;

// Re-export FFI types for embedders.
pub use value::{FromValue, IntoValue, Value};
pub use vm::{Vm, VmError};
