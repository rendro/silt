//! Document state types used by the LSP server.
//!
//! These are the in-memory representations of an open document plus the
//! definition/binding metadata the handlers consult on each request.

use std::collections::HashMap;

use crate::ast::*;
use crate::intern::Symbol;
use crate::lexer::Span;
use crate::types::Type;
use crate::types::effects::EffectSet;

// ── Document state ─────────────────────────────────────────────────

pub(super) struct DefInfo {
    pub(super) span: Span,
    pub(super) ty: Option<Type>,
    pub(super) params: Vec<String>,
    /// Markdown documentation from a doc comment preceding the decl,
    /// if any. Populated by `build_definitions` from `FnDecl.doc`,
    /// `TypeDecl.doc`, `TraitDecl.doc`, and `Decl::Let { doc, .. }`.
    /// Surfaced via hover / completion / signature-help as Markdown.
    pub(super) doc: Option<String>,
    /// Declared effect annotation from the fn signature, if this
    /// definition came from a `Decl::Fn`. `None` for non-fn defs
    /// (types, traits, top-level lets) — those don't carry effects.
    /// `Some(EffectSet::TOP)` for a fn without a user-written `!{...}`
    /// (the gradual-rollout permissive default). Surfaced on hover by
    /// the Phase B effect-rendering path.
    pub(super) declared_effects: Option<EffectSet>,
    /// Body-inferred effect set, populated by the typechecker after
    /// `check_fn_body` runs. `None` until inference completes; will
    /// stay `None` for recovery stubs and for non-fn defs. Hover
    /// renders both declared and inferred when they differ — useful
    /// for spotting over-broad annotations.
    pub(super) inferred_effects: Option<EffectSet>,
}

/// A local binding (let-bound identifier, function parameter, match binding, …)
/// with its approximate source position, for hover / goto-def on locals.
pub(super) struct LocalBinding {
    /// The identifier name (interned).
    pub(super) name: Symbol,
    /// Byte offset in the source where the binding identifier starts.
    pub(super) binding_offset: usize,
    /// Byte length of the binding identifier.
    pub(super) binding_len: usize,
    /// Start offset of the scope in which this binding is visible.
    pub(super) scope_start: usize,
    /// End offset of the scope (exclusive).
    pub(super) scope_end: usize,
    /// Inferred type, if known.
    pub(super) ty: Option<Type>,
}

pub(super) struct Document {
    pub(super) source: String,
    pub(super) program: Option<Program>,
    /// Definition map: name → definition info (built from top-level declarations).
    pub(super) definitions: HashMap<Symbol, DefInfo>,
    /// Local bindings (let, params, match/when) with approximate source positions.
    pub(super) locals: Vec<LocalBinding>,
}

/// A local variable binding visible at a given cursor position.
pub(super) struct LocalVar {
    pub(super) name: String,
    pub(super) ty: Option<Type>,
}
