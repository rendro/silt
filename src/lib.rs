pub mod ast;
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
#[cfg(feature = "watch")]
pub mod watch;
pub mod types;
pub mod typechecker;
pub mod value;
pub mod vm;

// Re-export FFI types for embedders.
pub use value::{FromValue, IntoValue, Value};
pub use vm::{Vm, VmError};
