//! What the session keeps of one module after its check. Its own module,
//! so that the type is no name of `silt::session`: the tests reach it
//! through `silt::testing::SessionInside`.

use std::collections::HashMap;
use std::sync::Arc;

use crate::ast;
use crate::diagnostic::Diagnostic;
use crate::typechecker::names::ModuleScope;
use crate::types::Type;

/// One module after its check.
pub struct ModuleAnalysis {
    /// The declarations, with what the checker filled in (expression
    /// types, what each field access means).
    pub ast: Arc<ast::Program>,
    /// The module's top-level names, and what it offers its importers.
    pub scope: ModuleScope,
    /// The inferred type of each top-level value the module binds.
    pub top_level: HashMap<crate::intern::Symbol, Type>,
    /// The signature an editor writes for each generic function (`fn
    /// twice(xs: m(Int)) -> m(Int) where m: Functor`).
    #[cfg_attr(not(any(feature = "lsp", feature = "test-hooks")), allow(dead_code))]
    pub signatures: HashMap<crate::intern::Symbol, crate::typechecker::Signature>,
    /// The module's top-level `let`s, by the span of each, in the order
    /// they are initialised in.
    pub let_order: Vec<crate::source::Span>,
    /// The module's type errors and warnings.
    pub diagnostics: Vec<Diagnostic>,
    /// For each top-level `main` and test function whose error type
    /// has no `Display`, the error reported where it is an entry point:
    /// `main` of a file run or checked, a test of a file tested.
    pub entry_display: HashMap<crate::intern::Symbol, Diagnostic>,
    /// How the module writes each other module of the program where it
    /// names one of its types, as the checker's messages do. Kept only in
    /// a session `for_editor` (empty otherwise).
    #[cfg_attr(not(any(feature = "lsp", feature = "repl")), allow(dead_code))]
    pub(crate) modules_written: HashMap<crate::defs::ModuleId, crate::typechecker::ModuleWritten>,
}
