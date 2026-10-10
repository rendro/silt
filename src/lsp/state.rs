//! Document state types used by the LSP server.
//!
//! These are the in-memory representations of an open document plus the
//! definition/binding metadata the handlers consult on each request.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::ast::*;
use crate::intern::Symbol;
use crate::lexer::{Lexed, Lexer, Tok};
use crate::session::ModuleId;
use crate::source::{FileId, SourceFile};
use crate::types::Type;

// ── Document state ─────────────────────────────────────────────────

pub(super) struct DefInfo {
    pub(super) ty: Option<Type>,
    pub(super) params: Vec<String>,
    /// Markdown documentation from a doc comment preceding the decl,
    /// if any. Populated by `build_definitions` from `FnDecl.doc`,
    /// `TypeDecl.doc`, `TraitDecl.doc`, and `Decl::Let { doc, .. }`.
    /// Surfaced via hover / completion / signature-help as Markdown.
    pub(super) doc: Option<String>,
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
    /// For the binder of a later alternative of an or-pattern
    /// (`Left(n) | Right(n)`): the byte the first alternative's binder
    /// of the name starts at. The binders of all alternatives are one
    /// binding, named by the first.
    pub(super) same_as: Option<usize>,
}

impl LocalBinding {
    /// The binding this binder is a site of, by the byte its first
    /// binder starts at.
    pub(super) fn id(&self) -> usize {
        self.same_as.unwrap_or(self.binding_offset)
    }
}

pub(super) struct Document {
    /// The document's text and its lines. The spans of `program` are
    /// byte ranges in it.
    pub(super) source: SourceFile,
    /// The file the document's URI names.
    pub(super) path: PathBuf,
    /// `path` as a file key (canonical when it exists), computed once.
    pub(super) key: PathBuf,
    /// Whether the editor has the document open. A document that is not
    /// open is a workspace file indexed for the cross-file features: its
    /// declarations as parsed, without types and without diagnostics.
    pub(super) open: bool,
    /// For an open document, its module in its project's session.
    pub(super) module: Option<ModuleRef>,
    /// The declarations: for an open document, the session's checked
    /// module, with the types the checker filled in, as far as the text
    /// parses. `None` when the analysis failed.
    pub(super) program: Option<Arc<Program>>,
    /// Definition map: name → definition info (built from top-level declarations).
    pub(super) definitions: HashMap<Symbol, DefInfo>,
    /// Local bindings (let, params, match/when) with approximate source positions.
    pub(super) locals: Vec<LocalBinding>,
    /// The tokens of `source`, made when a request first asks for them
    /// (see `tokens`) and kept until the text changes (`set_text`).
    pub(super) lexed: std::cell::OnceCell<Lexed>,
}

impl Document {
    /// The tokens of the document's text: lexed once for each text, not
    /// for each request that reads them.
    pub(super) fn tokens(&self) -> &[Tok] {
        &self
            .lexed
            .get_or_init(|| Lexer::new(FileId::default(), &self.source.text).tokenize())
            .tokens
    }

    /// Give the document the text `source`.
    pub(super) fn set_text(&mut self, source: SourceFile) {
        self.source = source;
        self.lexed = std::cell::OnceCell::new();
    }
}

/// A module of a project's session.
#[derive(Clone)]
pub(super) struct ModuleRef {
    /// The project directory (the key of `Server::projects`).
    pub(super) project: PathBuf,
    pub(super) id: ModuleId,
}

/// A local variable binding visible at a given cursor position.
pub(super) struct LocalVar {
    pub(super) name: String,
    pub(super) ty: Option<Type>,
}
