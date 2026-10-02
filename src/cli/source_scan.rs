//! Questions about a source file that decide how the CLI treats it:
//! "which functions are tests?", "where is this function's name?".
//!
//! Every answer is read from the parsed declarations, the same ones the
//! typechecker and the compiler work on. Nothing here looks at source
//! text: a text scan cannot tell a declaration from the same words inside
//! a comment or a string, and it breaks on spacing the parser accepts.

use silt::ast::{Decl, Program};
use silt::intern::resolve;
use silt::source::Span;

pub(crate) use silt::session::{TestKind, test_functions};

/// The span of the name of the top-level function `name` of `program`.
pub(crate) fn fn_name_span(program: &Program, name: &str) -> Option<Span> {
    program.decls.iter().find_map(|decl| match decl {
        Decl::Fn(f) if resolve(f.name) == name => Some(f.name_span),
        _ => None,
    })
}
