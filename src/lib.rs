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
//! `silt::Tag`, `silt::Value`, `silt::Vm`, `silt::VmError`,
//! `silt::session::Builtins`, `silt::session::Config`, `silt::session::Entry`,
//! `silt::session::EntryPoint`, `silt::session::TestFn`, `silt::session::TestKind`,
//! `silt::session::HostCall`, `silt::session::HostModule`,
//! `silt::session::LockPolicy`, `silt::session::Program`,
//! `silt::session::ProjectSetup`, `silt::session::Session`,
//! `silt::session::FileId`, `silt::session::Span`,
//! `silt::session::SourceMap`, `silt::session::Diagnostic` and
//! `silt::session::Severity`, with what they hand out (a `Value`'s
//! variants, an analysis). A diagnostic is promised by its methods
//! (its message, severity, place, notes and help), not by its code or
//! its quick fixes. (tests/meta/public_api_tests.rs keeps this list and the
//! guide one.)
//!
//! Every module but `session` is the crate's own. Three more roots are
//! public, and none of them is promised: `silt::cli::main`, what the
//! `silt` binary runs; `silt::playground`, what the playground
//! (playground/) takes beyond the embedding API, with the `playground`
//! feature; and `silt::testing`, the one door of the tests, benches and
//! fuzz targets to the inside, with the `test-hooks` feature. A `pub`
//! item of the crate is one that outside code reaches by one of these:
//! a build with the test API reports any other (`unreachable_pub`), and
//! tests/meta/public_items.txt lists them all.

#![allow(clippy::mutable_key_type)]
#![cfg_attr(any(test, feature = "test-hooks"), warn(unreachable_pub))]
// On wasm32 the library is the playground's: the command, and what only
// the command uses (ending the process, the terminal, the files of a
// package), is not built there. Dead code is judged by the native builds.
#![cfg_attr(target_arch = "wasm32", allow(dead_code))]

mod ast;
mod builtins;
mod bytecode;
// The `silt` command; src/main.rs calls `cli::main`. Not for wasm32,
// where the playground uses the library and there is no command.
#[cfg(not(target_arch = "wasm32"))]
#[doc(hidden)]
pub mod cli;
mod compiler;
mod defs;
mod diagnostic;
mod disassemble;
mod evidence;
// What the command prints for a subcommand whose feature is not built.
#[cfg(any(
    not(feature = "lsp"),
    not(feature = "repl"),
    test,
    feature = "test-hooks"
))]
mod feature_stub;
mod file_discovery;
mod format;
#[cfg(any(test, feature = "test-hooks"))]
mod fuzz_invariants;
mod git;
#[cfg(feature = "http")]
mod http_wire;
mod intern;
mod layers;
mod lexer;
mod lockfile;
#[cfg(feature = "lsp")]
mod lsp;
mod manifest;
mod module;
mod package_graph;
mod parser;
#[cfg(feature = "playground")]
pub mod playground;
mod program;
#[cfg(feature = "repl")]
mod repl;
mod runtime;
mod scheduler;
pub mod session;
mod source;
mod text;
mod typechecker;
mod typeinfo;
mod types;
// The self-updater shells out to curl/tar and replaces the running binary in
// place. Neither mechanism applies on wasm32 — the playground is embedded via
// the library path, not the CLI — so gate the module to native targets only.
#[cfg(not(target_arch = "wasm32"))]
mod update;
mod value;
mod vm;
#[cfg(feature = "watch")]
mod watch;

// The names an embedder uses (see "What the crate promises" above).
pub use typeinfo::Tag;
pub use value::{FromValue, IntoValue, Value};
pub use vm::{Buffer, Clock, Exit, HostIo, Output, Vm, VmError};

/// The version of silt: what `silt --version` prints after the name.
pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The test API: what the tests, the benches and the fuzz crate read of
/// the crate's inside. Built with the `test-hooks` feature only (and in
/// the crate's own tests); no part of what the crate promises.
#[cfg(any(test, feature = "test-hooks"))]
#[doc(hidden)]
pub mod testing {
    pub use crate::session::testing::*;
    pub use crate::vm::SystemClock;

    pub mod fuzz_invariants {
        pub use crate::fuzz_invariants::*;
    }
    pub mod ast {
        pub use crate::ast::*;
    }
    pub mod builtins {
        pub use crate::builtins::*;
    }
    pub mod bytecode {
        pub use crate::bytecode::*;
    }
    pub mod defs {
        pub use crate::defs::*;
    }
    pub mod diagnostic {
        pub use crate::diagnostic::*;
    }
    pub mod disassemble {
        pub use crate::disassemble::*;
    }
    pub mod feature_stub {
        pub use crate::feature_stub::*;
    }
    pub mod format {
        pub use crate::format::*;
    }
    pub mod git {
        pub use crate::git::*;
    }
    #[cfg(feature = "http")]
    pub mod http_wire {
        pub use crate::http_wire::*;
    }
    pub mod intern {
        pub use crate::intern::*;
    }
    pub mod layers {
        pub use crate::layers::*;
    }
    pub mod lexer {
        pub use crate::lexer::*;
    }
    pub mod lockfile {
        pub use crate::lockfile::*;
    }
    #[cfg(feature = "lsp")]
    pub mod lsp {
        pub use crate::lsp::*;
    }
    pub mod manifest {
        pub use crate::manifest::*;
    }
    pub mod module {
        pub use crate::module::*;
    }
    pub mod package_graph {
        pub use crate::package_graph::*;
    }
    pub mod parser {
        pub use crate::parser::*;
    }
    #[cfg(feature = "repl")]
    pub mod repl {
        pub use crate::repl::*;
    }
    pub mod runtime {
        pub use crate::runtime::*;
    }
    pub mod scheduler {
        pub use crate::scheduler::*;
    }
    pub mod source {
        pub use crate::source::*;
    }
    pub mod typechecker {
        pub use crate::typechecker::*;
    }
    pub mod typeinfo {
        pub use crate::typeinfo::*;
    }
    pub mod types {
        pub use crate::types::*;
    }
    pub mod value {
        pub use crate::value::*;
    }
    pub mod vm {
        pub use crate::vm::*;
    }
}
