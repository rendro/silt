//! What the VM runs: a compiled program, with what it was compiled for.
//!
//! The session makes it (`crate::session::Session::compile`) and the VM
//! runs it (`crate::vm::Vm::run_program`); it is defined here, below
//! both, so that the VM does not depend on the session.

use std::sync::Arc;

use crate::bytecode::{Function, Globals};
use crate::source::Span;

/// What the VM runs: the compiled functions, the first of which is the
/// script that installs the program's globals (and, for
/// [`crate::session::Entry::Main`], calls `main`).
pub struct Program {
    pub functions: Vec<Function>,
    /// The types its values are of, which the VM is given
    /// ([`crate::vm::Vm::run_program`]).
    pub types: crate::typeinfo::TypeTable,
    /// Its global slots, which the VM is given too.
    pub globals: Arc<Globals>,
    /// The part of `globals` that stands whatever becomes of the
    /// program's run. For a REPL entry: the slots, impl methods and
    /// `Display` impls of the entries before it that ran to their end,
    /// without the entry's own, which the session takes back if the
    /// entry fails. For any other program: all of `globals`. (The VM
    /// of a task keeps this table, see `Vm::look_again`.)
    pub settled: Arc<Globals>,
    pub entry: EntryPoint,
}

/// What a [`Program`] was compiled for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryPoint {
    /// `main`, bound at `span` (its name): where a `main` that returns
    /// `Err` is reported.
    Main {
        span: Span,
    },
    /// The selected tests, in source order.
    Tests(Vec<TestFn>),
    Cell,
}

/// How `silt test` treats a top-level function, going by its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestKind {
    /// `test_*`: the function is called; it fails when it raises an error
    /// or returns `Err(..)`.
    Run,
    /// `skip_test_*`: the function is reported as skipped, not called.
    Skip,
}

/// A test function selected to run or to be reported as skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestFn {
    pub name: String,
    pub kind: TestKind,
    /// The span of the function's name.
    pub span: Span,
    /// The function's global slot, which the compiler gives it.
    pub slot: u16,
}
