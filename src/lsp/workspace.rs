//! Workspace-wide queries: references, rename targets and symbols.
#![allow(deprecated)] // SymbolInformation.deprecated field is LSP-required
//!
//! A name is found by what it means, not by how it is spelled. The
//! resolver leaves a [`Res`] on every node that names a definition, so a
//! reference to a function, a `let`, a type, a variant or a trait is a
//! node whose `Res` is that definition's [`DefId`]: `mk` of module `geo`
//! is found as `mk`, as `geo.mk` and as the item of `import geo.{ mk }`,
//! and a function `mk` of another module is not. A local binding (a
//! parameter, a `let` in a body, a pattern's binder) is found in its
//! document, by the binding a use resolves to.
//!
//! The modules searched are those the open documents' sessions have
//! checked, and the workspace files that import the module of the
//! definition, which are checked for the query. A definition is the same
//! in every session by where it is declared ([`DefKey`]).

use std::collections::HashSet;
use std::path::PathBuf;

use lsp_types::{Location, SymbolInformation, SymbolKind, Uri};

use crate::ast::{
    Decl, Expr, ExprKind, FnDecl, ImportTarget, Pattern, PatternKind, Program, Stmt, TraitRef,
    TypeBody, TypeDecl, TypeExpr, TypeExprKind, WhereClause,
};
use crate::defs::{DefId, DefKind, DefTable, Res};
use crate::intern::{Symbol, resolve as resolve_sym};
use crate::session::{ImportResolution, ModuleId, Session};
use crate::source::{SourceFile, Span};

use super::Server;
use super::ast_walk::visit_expr_children;
use super::conversions::span_to_range;
use super::local_bindings::{find_local_binding_at_offset, nearest_local_binding_for};
use super::project::{path_key, project_dir};
use super::state::{Document, LocalBinding};

/// A definition, named the same way in every session.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(super) enum DefKey {
    /// One of silt's own: the builtin definitions have the same ids in
    /// every session.
    Builtin(DefId),
    /// The definition whose name is declared at byte `start` of the file
    /// `path` (a path key).
    At {
        path: PathBuf,
        start: u32,
        name: Symbol,
    },
}

/// What the name under the cursor means.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum Target {
    /// Definitions: one, or the value and the type an import item names.
    Defs(Vec<DefKey>),
    /// The local binding of the document whose name starts at this byte
    /// (see `local_bindings.rs`).
    Local(usize),
}

impl Target {
    /// Whether a rename may edit it: a local, or a definition of a file.
    pub(super) fn is_renameable(&self) -> bool {
        match self {
            Target::Defs(keys) => keys.iter().all(|key| matches!(key, DefKey::At { .. })),
            Target::Local(_) => true,
        }
    }
}

/// Whether byte `cursor` is in `span` or directly behind it.
fn holds(span: Span, cursor: usize) -> bool {
    span.start as usize <= cursor && cursor <= span.end as usize
}

/// The session-independent name of the definition `id` of `session`.
fn def_key(session: &Session, id: DefId) -> DefKey {
    let def = session.defs().get(id);
    if def.module.is_builtin() || !def.span.is_in_source() {
        return DefKey::Builtin(id);
    }
    DefKey::At {
        path: path_key(&session.graph().module(def.module).path),
        start: def.span.start,
        name: def.name,
    }
}

/// The definition of `session` that `key` names, if the session has its
/// module.
fn def_of_key(session: &Session, key: &DefKey) -> Option<DefId> {
    match key {
        DefKey::Builtin(id) => Some(*id),
        DefKey::At { path, start, name } => {
            let module = session
                .graph()
                .modules()
                .iter()
                .find(|m| path_key(&m.path) == *path)?;
            let defs = session.defs();
            defs.of_module(module.id).iter().copied().find(|id| {
                let def = defs.get(*id);
                def.span.start == *start && def.name == *name
            })
        }
    }
}

/// The definitions the item `item` of `import module.{ item }` in module
/// `importer` names: a value, a type, or both.
fn import_item_defs(
    session: &Session,
    importer: ModuleId,
    module: Symbol,
    item: Symbol,
) -> Vec<DefId> {
    use crate::typechecker::names::Binding;
    let target = session
        .graph()
        .module(importer)
        .imports
        .iter()
        .find_map(|import| match import.resolution {
            ImportResolution::Module(id) if import.name == module => Some(id),
            _ => None,
        });
    let Some(exports) = target
        .and_then(|id| session.module_analysis(id))
        .map(|analysis| &analysis.scope.exports)
    else {
        return Vec::new();
    };
    [exports.values.get(&item), exports.types.get(&item)]
        .into_iter()
        .filter_map(|binding| match binding {
            Some(Binding::Def(id)) => Some(*id),
            _ => None,
        })
        .collect()
}

impl Server {
    /// What the name at byte `cursor` of the open document `uri` means,
    /// with the span of the name as written there.
    pub(super) fn target_at(&self, uri: &Uri, cursor: usize) -> Option<(Span, Target)> {
        let doc = self.documents.get(uri)?;
        let program = doc.program.as_ref()?;
        let module = doc.module.as_ref()?;
        let session = &self.projects.get(&module.project)?.session;
        let uses = Uses::of(program, session, module.id);
        let here: Vec<&(Span, DefId)> = uses
            .defs
            .iter()
            .filter(|(span, _)| holds(*span, cursor))
            .collect();
        if let Some((span, _)) = here.first() {
            let keys = here.iter().map(|(_, id)| def_key(session, *id)).collect();
            return Some((*span, Target::Defs(keys)));
        }
        // The name of a declaration of this module.
        let defs = session.defs();
        let declared = defs.of_module(module.id).iter().copied().find_map(|id| {
            let def = defs.get(id);
            (def.span.is_in_source() && holds(def.span, cursor))
                .then(|| (def.span, Target::Defs(vec![def_key(session, id)])))
        });
        if declared.is_some() {
            return declared;
        }
        let local = uses.locals.iter().find(|l| holds(l.span, cursor))?;
        Some((local.span, Target::Local(local.binding(&doc.locals)?)))
    }

    /// Every place that names `target`, which was found in the open
    /// document `uri`; with the declaration if `include_definition`.
    /// Sorted by file and position.
    pub(super) fn references_to(
        &mut self,
        uri: &Uri,
        target: &Target,
        include_definition: bool,
    ) -> Vec<Location> {
        let mut locations = match target {
            Target::Local(binding) => self.local_references(uri, *binding, include_definition),
            Target::Defs(keys) => {
                self.check_importers(keys);
                self.def_references(keys, include_definition)
            }
        };
        locations.sort_by(|a, b| {
            (a.uri.as_str(), a.range.start.line, a.range.start.character).cmp(&(
                b.uri.as_str(),
                b.range.start.line,
                b.range.start.character,
            ))
        });
        locations.dedup();
        locations
    }

    /// The binder of the local binding at byte `binding` of the document
    /// `uri`, and the uses that resolve to it.
    fn local_references(&self, uri: &Uri, binding: usize, include_binder: bool) -> Vec<Location> {
        let found = || {
            let doc = self.documents.get(uri)?;
            let program = doc.program.as_ref()?;
            let module = doc.module.as_ref()?;
            let session = &self.projects.get(&module.project)?.session;
            let uses = Uses::of(program, session, module.id);
            let ranges: Vec<_> = uses
                .locals
                .iter()
                .filter(|l| include_binder || !l.binder)
                .filter(|l| l.binding(&doc.locals) == Some(binding))
                .map(|l| span_to_range(&l.span, &doc.source))
                .collect();
            Some(ranges)
        };
        found()
            .unwrap_or_default()
            .into_iter()
            .map(|range| Location::new(uri.clone(), range))
            .collect()
    }

    /// Where `target` is declared: a local's binder in the document
    /// `uri`, a definition's name in its file. Nothing for one of silt's
    /// own.
    pub(super) fn declarations_of(&self, uri: &Uri, target: &Target) -> Vec<Location> {
        match target {
            Target::Local(binding) => {
                let binder = self.documents.get(uri).and_then(|doc| {
                    let local = doc.locals.iter().find(|b| b.binding_offset == *binding)?;
                    Some(super::conversions::offsets_to_range(
                        &doc.source,
                        local.binding_offset,
                        local.binding_offset + local.binding_len,
                    ))
                });
                binder
                    .map(|range| Location::new(uri.clone(), range))
                    .into_iter()
                    .collect()
            }
            Target::Defs(keys) => {
                let mut locations: Vec<Location> = Vec::new();
                for key in keys {
                    let found = self.projects.values().find_map(|project| {
                        let session = &project.session;
                        let def = session.defs().get(def_of_key(session, key)?);
                        if def.module.is_builtin() || !def.span.is_in_source() {
                            return None;
                        }
                        let module = session.graph().module(def.module);
                        let source = session.sources().get(module.file?)?;
                        let uri = self.uri_for_path(&module.path)?;
                        Some(Location::new(uri, span_to_range(&def.span, source)))
                    });
                    if let Some(location) = found
                        && !locations.contains(&location)
                    {
                        locations.push(location);
                    }
                }
                locations
            }
        }
    }

    /// The definition of the type `id` of the session of the open
    /// document `doc`, as a target.
    pub(super) fn type_target(&self, doc: &Document, id: crate::defs::TypeId) -> Option<Target> {
        let module = doc.module.as_ref()?;
        let session = &self.projects.get(&module.project)?.session;
        Some(Target::Defs(vec![def_key(session, id.0)]))
    }

    /// Check the workspace files that import the module of one of
    /// `keys` and that no session has checked: a file that is not open
    /// and that no open document imports is only parsed until a query
    /// needs what its names mean.
    fn check_importers(&mut self, keys: &[DefKey]) {
        let stems: HashSet<Symbol> = keys
            .iter()
            .filter_map(|key| match key {
                DefKey::At { path, .. } => path.file_stem(),
                DefKey::Builtin(_) => None,
            })
            .map(|stem| crate::intern::intern(&stem.to_string_lossy()))
            .collect();
        let imports_one = |program: &Program| {
            program.decls.iter().any(|decl| match decl {
                Decl::Import(
                    ImportTarget::Module(m)
                    | ImportTarget::Items(m, _)
                    | ImportTarget::Alias(m, ..),
                    _,
                ) => stems.contains(m),
                _ => false,
            })
        };
        let importers: Vec<PathBuf> = self
            .documents
            .values()
            .filter(|doc| !doc.open && doc.program.as_deref().is_some_and(imports_one))
            .map(|doc| doc.path.clone())
            .collect();
        for path in importers {
            if let Some(project) = self.projects.get_mut(&project_dir(&path)) {
                project.check_file(&path);
            }
        }
    }

    /// Every place in the checked modules of every session that names
    /// one of `keys`.
    fn def_references(&self, keys: &[DefKey], include_definition: bool) -> Vec<Location> {
        let mut locations = Vec::new();
        for project in self.projects.values() {
            let session = &project.session;
            let ids: HashSet<DefId> = keys
                .iter()
                .filter_map(|key| def_of_key(session, key))
                .collect();
            if ids.is_empty() {
                continue;
            }
            let location = |module: ModuleId, span: Span| {
                let module = session.graph().module(module);
                let source = session.sources().get(module.file?)?;
                let uri = self.uri_for_path(&module.path)?;
                Some(Location::new(uri, span_to_range(&span, source)))
            };
            for module in session.graph().modules() {
                let Some(analysis) = session.module_analysis(module.id) else {
                    continue;
                };
                for (span, id) in Uses::of(&analysis.ast, session, module.id).defs {
                    if ids.contains(&id) {
                        locations.extend(location(module.id, span));
                    }
                }
            }
            if include_definition {
                for id in &ids {
                    let def = session.defs().get(*id);
                    if def.span.is_in_source() && !def.module.is_builtin() {
                        locations.extend(location(def.module, def.span));
                    }
                }
            }
        }
        locations
    }

    /// The workspace symbols whose name holds `query`, whatever the
    /// case; every symbol for an empty query. A document a session has
    /// checked gives its definitions; one that is only parsed, its
    /// declarations. Each is at its name.
    pub(super) fn workspace_symbols_matching(&self, query: &str) -> Vec<SymbolInformation> {
        let query = query.to_lowercase();
        let mut results = Vec::new();
        let mut uris: Vec<&Uri> = self.documents.keys().collect();
        uris.sort_by_key(|uri| uri.as_str());
        for uri in uris {
            let doc = &self.documents[uri];
            let mut symbols = match self.checked(doc) {
                Some((session, module)) => checked_symbols(session, module, uri, &doc.source),
                None => match &doc.program {
                    Some(program) => parsed_symbols(program, uri, &doc.source),
                    None => Vec::new(),
                },
            };
            symbols
                .retain(|symbol| query.is_empty() || symbol.name.to_lowercase().contains(&query));
            results.extend(symbols);
        }
        results
    }

    /// The session that has checked the document `doc`, and its module
    /// there.
    fn checked(&self, doc: &Document) -> Option<(&Session, ModuleId)> {
        self.projects.values().find_map(|project| {
            let session = &project.session;
            let module = session
                .graph()
                .modules()
                .iter()
                .find(|m| path_key(&m.path) == doc.key)?;
            // The text the session checked is the document's.
            let same_text = module
                .file
                .and_then(|file| session.sources().get(file))
                .is_some_and(|source| source.text == doc.source.text);
            (same_text && session.module_analysis(module.id).is_some())
                .then_some((session, module.id))
        })
    }
}

fn symbol(
    name: Symbol,
    kind: SymbolKind,
    span: Span,
    container: Option<Symbol>,
    uri: &Uri,
    source: &SourceFile,
) -> SymbolInformation {
    SymbolInformation {
        name: resolve_sym(name),
        kind,
        tags: None,
        deprecated: None,
        location: Location::new(uri.clone(), span_to_range(&span, source)),
        container_name: container.map(resolve_sym),
    }
}

/// The definitions of the checked module `module`, in the order they
/// were declared.
fn checked_symbols(
    session: &Session,
    module: ModuleId,
    uri: &Uri,
    source: &SourceFile,
) -> Vec<SymbolInformation> {
    let defs = session.defs();
    let mut ids: Vec<DefId> = defs.of_module(module).to_vec();
    ids.sort_by_key(|id| defs.get(*id).span.start);
    ids.into_iter()
        .filter_map(|id| {
            let def = defs.get(id);
            let (kind, container) = match def.kind {
                DefKind::Fn => (SymbolKind::FUNCTION, None),
                DefKind::Let => (SymbolKind::CONSTANT, None),
                // An alias is no type of its own.
                DefKind::TypeAlias => (SymbolKind::TYPE_PARAMETER, None),
                DefKind::Type(_) if defs.variants(id).is_empty() => (SymbolKind::STRUCT, None),
                DefKind::Type(_) => (SymbolKind::ENUM, None),
                DefKind::Variant { ty, .. } => (SymbolKind::ENUM_MEMBER, Some(defs.get(ty.0).name)),
                DefKind::Trait(_) => (SymbolKind::INTERFACE, None),
                DefKind::Host => return None,
            };
            def.span
                .is_in_source()
                .then(|| symbol(def.name, kind, def.span, container, uri, source))
        })
        .collect()
}

/// The declarations of a module that is only parsed.
fn parsed_symbols(program: &Program, uri: &Uri, source: &SourceFile) -> Vec<SymbolInformation> {
    let mut out = Vec::new();
    for decl in &program.decls {
        match decl {
            Decl::Fn(f) => out.push(symbol(
                f.name,
                SymbolKind::FUNCTION,
                f.name_span,
                None,
                uri,
                source,
            )),
            Decl::Let { pattern, .. } => {
                if let PatternKind::Ident(name) = &pattern.kind {
                    out.push(symbol(
                        *name,
                        SymbolKind::CONSTANT,
                        pattern.span,
                        None,
                        uri,
                        source,
                    ));
                }
            }
            Decl::Type(t) => type_symbols(t, uri, source, &mut out),
            Decl::Trait(t) => out.push(symbol(
                t.name,
                SymbolKind::INTERFACE,
                t.name_span,
                None,
                uri,
                source,
            )),
            Decl::TraitImpl(_) | Decl::Import(..) => {}
        }
    }
    out
}

fn type_symbols(t: &TypeDecl, uri: &Uri, source: &SourceFile, out: &mut Vec<SymbolInformation>) {
    let kind = match &t.body {
        TypeBody::Enum(_) => SymbolKind::ENUM,
        TypeBody::Record(_) => SymbolKind::STRUCT,
        TypeBody::Alias(_) => SymbolKind::TYPE_PARAMETER,
    };
    out.push(symbol(t.name, kind, t.name_span, None, uri, source));
    if let TypeBody::Enum(variants) = &t.body {
        for v in variants {
            out.push(symbol(
                v.name,
                SymbolKind::ENUM_MEMBER,
                v.name_span,
                Some(t.name),
                uri,
                source,
            ));
        }
    }
}

// ── The names a module writes ──────────────────────────────────────

/// A local name as a module writes it: where a pattern, a parameter or
/// a `loop` binds it, or a use of it.
struct LocalName {
    span: Span,
    name: Symbol,
    binder: bool,
}

impl LocalName {
    /// The binding it is or resolves to, by the byte its name starts
    /// at, when the document's local bindings have it.
    fn binding(&self, locals: &[LocalBinding]) -> Option<usize> {
        let at = self.span.start as usize;
        let binding = if self.binder {
            find_local_binding_at_offset(locals, at)
        } else {
            nearest_local_binding_for(locals, self.name, at)
        };
        binding.map(|b| b.binding_offset)
    }
}

/// Every name a module writes that the resolver gave a meaning, apart
/// from the names its declarations declare (those are the definitions'
/// own spans).
struct Uses<'a> {
    session: &'a Session,
    module: ModuleId,
    /// The span of a name as written, and the definition it names.
    defs: Vec<(Span, DefId)>,
    /// Each local name: its binders and its uses.
    locals: Vec<LocalName>,
    /// The pattern being walked is a top-level `let`'s: its names are
    /// definitions.
    declares: bool,
}

impl<'a> Uses<'a> {
    fn of(program: &Program, session: &'a Session, module: ModuleId) -> Uses<'a> {
        let mut uses = Uses {
            session,
            module,
            defs: Vec::new(),
            locals: Vec::new(),
            declares: false,
        };
        for decl in &program.decls {
            uses.decl(decl);
        }
        uses
    }

    fn table(&self) -> &'a DefTable {
        self.session.defs()
    }

    /// A name at `span` that means `res`.
    fn name(&mut self, span: Span, res: Option<Res>) {
        if let Some(Res::Def(id)) = res
            && span.is_in_source()
        {
            self.defs.push((span, id));
        }
    }

    /// A local name at `span`.
    fn local(&mut self, span: Span, name: Symbol, binder: bool) {
        if self.declares && binder {
            return;
        }
        self.locals.push(LocalName { span, name, binder });
    }

    fn decl(&mut self, decl: &Decl) {
        match decl {
            Decl::Fn(f) => self.fn_decl(f),
            Decl::TraitImpl(ti) => {
                if ti.is_auto_derived {
                    return;
                }
                self.name(ti.trait_name_span, ti.trait_res);
                self.name(ti.target_type_span, ti.target_res);
                self.where_clauses(&ti.where_clauses);
                for arg in ti.trait_args.iter().chain(&ti.target_type_args) {
                    self.type_expr(arg);
                }
                for binding in &ti.assoc_type_bindings {
                    self.type_expr(&binding.ty);
                }
                for method in &ti.methods {
                    self.fn_decl(method);
                }
            }
            Decl::Trait(t) => {
                for supertrait in &t.supertraits {
                    self.trait_ref(supertrait);
                }
                self.where_clauses(&t.param_where_clauses);
                for assoc in &t.assoc_types {
                    for bound in &assoc.bounds {
                        self.trait_ref(bound);
                    }
                }
                for method in &t.methods {
                    self.fn_decl(method);
                }
            }
            Decl::Let {
                value, pattern, ty, ..
            } => {
                self.declares = true;
                self.pattern(pattern);
                self.declares = false;
                if let Some(ty) = ty {
                    self.type_expr(ty);
                }
                self.expr(value);
            }
            Decl::Type(t) => match &t.body {
                TypeBody::Record(fields) => {
                    for field in fields {
                        self.type_expr(&field.ty);
                    }
                }
                TypeBody::Enum(variants) => {
                    for variant in variants {
                        for field in &variant.fields {
                            self.type_expr(field);
                        }
                    }
                }
                TypeBody::Alias(ty) => self.type_expr(ty),
            },
            // An item of `import m.{ item }` names what `m` exports.
            Decl::Import(ImportTarget::Items(module, items), _) => {
                for (item, span) in items {
                    for id in import_item_defs(self.session, self.module, *module, *item) {
                        self.name(*span, Some(Res::Def(id)));
                    }
                }
            }
            Decl::Import(..) => {}
        }
    }

    fn fn_decl(&mut self, f: &FnDecl) {
        for param in &f.params {
            self.pattern(&param.pattern);
            if let Some(ty) = &param.ty {
                self.type_expr(ty);
            }
        }
        if let Some(ty) = &f.return_type {
            self.type_expr(ty);
        }
        self.where_clauses(&f.where_clauses);
        self.expr(&f.body);
    }

    fn where_clauses(&mut self, clauses: &[WhereClause]) {
        for clause in clauses {
            self.name(clause.trait_name_span, clause.trait_res);
            for arg in &clause.trait_args {
                self.type_expr(arg);
            }
        }
    }

    fn trait_ref(&mut self, r: &TraitRef) {
        self.name(r.span, r.res);
        for arg in &r.args {
            self.type_expr(arg);
        }
    }

    fn type_expr(&mut self, te: &TypeExpr) {
        match &te.kind {
            TypeExprKind::Named { name_span, .. } => self.name(*name_span, te.res),
            TypeExprKind::Generic {
                name_span, args, ..
            } => {
                self.name(*name_span, te.res);
                for arg in args {
                    self.type_expr(arg);
                }
            }
            TypeExprKind::Tuple(elems) => {
                for elem in elems {
                    self.type_expr(elem);
                }
            }
            TypeExprKind::Function(params, ret) => {
                for param in params {
                    self.type_expr(param);
                }
                self.type_expr(ret);
            }
            TypeExprKind::SelfType => {}
            TypeExprKind::AssocProj { receiver, .. } => self.type_expr(receiver),
            TypeExprKind::AnonRecord { fields, .. } => {
                for (_, ty) in fields {
                    self.type_expr(ty);
                }
            }
        }
    }

    fn expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Ident(name) => match expr.res {
                Some(Res::Local) => self.local(expr.span, *name, false),
                res => self.name(expr.span, res),
            },
            // `m.f`, `m.Circle`, `Shape.Circle`: the member's own name.
            // (The span of a field access is its receiver's.)
            ExprKind::FieldAccess(_, _, field_span) => self.name(*field_span, expr.res),
            ExprKind::RecordCreate { name_span, .. } => self.name(*name_span, expr.res),
            ExprKind::Ascription(_, ty) => self.type_expr(ty),
            ExprKind::Match { arms, .. } => {
                for arm in arms {
                    self.pattern(&arm.pattern);
                }
            }
            ExprKind::Loop { bindings, .. } => {
                for (name, span, _) in bindings {
                    self.local(*span, *name, true);
                }
            }
            ExprKind::Lambda { params, .. } => {
                for param in params {
                    self.pattern(&param.pattern);
                    if let Some(ty) = &param.ty {
                        self.type_expr(ty);
                    }
                }
            }
            ExprKind::Block(stmts) => {
                for stmt in stmts {
                    self.stmt(stmt);
                }
                return;
            }
            _ => {}
        }
        visit_expr_children(expr, |child| self.expr(child));
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Let { value, pattern, ty } => {
                self.pattern(pattern);
                if let Some(ty) = ty {
                    self.type_expr(ty);
                }
                self.expr(value);
            }
            Stmt::When {
                expr,
                else_body,
                pattern,
                ..
            } => {
                self.pattern(pattern);
                self.expr(expr);
                self.expr(else_body);
            }
            Stmt::WhenBool {
                condition,
                else_body,
                ..
            } => {
                self.expr(condition);
                self.expr(else_body);
            }
            Stmt::Expr(e) => self.expr(e),
        }
    }

    fn pattern(&mut self, pattern: &Pattern) {
        match &pattern.kind {
            PatternKind::Ident(name) => match pattern.res {
                Some(res @ Res::Def(_)) => self.name(pattern.span, Some(res)),
                _ => self.local(pattern.span, *name, true),
            },
            PatternKind::Constructor {
                qualifier,
                name_span,
                args,
                ..
            } => {
                self.name(*name_span, pattern.res);
                // `m.Shape.Circle(r)`, `Shape.Circle(r)`: the segment
                // that names the variant's enum.
                if let Some(Res::Def(variant)) = pattern.res
                    && let DefKind::Variant { ty, .. } = self.table().get(variant).kind
                {
                    let enum_name = self.table().get(ty.0).name;
                    for segment in qualifier.iter().filter(|q| q.name == enum_name) {
                        self.name(segment.span, Some(Res::Def(ty.0)));
                    }
                }
                for arg in args {
                    self.pattern(arg);
                }
            }
            PatternKind::Record {
                name_span, fields, ..
            } => {
                self.name(*name_span, pattern.res);
                self.field_patterns(fields);
            }
            PatternKind::AnonRecord { fields, rest } => {
                self.field_patterns(fields);
                if let Some((name, span)) = rest {
                    self.local(*span, *name, true);
                }
            }
            PatternKind::Tuple(pats) | PatternKind::Or(pats) => {
                for p in pats {
                    self.pattern(p);
                }
            }
            PatternKind::List(pats, rest) => {
                for p in pats {
                    self.pattern(p);
                }
                if let Some(rest) = rest {
                    self.pattern(rest);
                }
            }
            PatternKind::Map(entries) => {
                for (_, p) in entries {
                    self.pattern(p);
                }
            }
            // `^name` compares with the local `name`: a use of it,
            // behind the `^`.
            PatternKind::Pin(name) => {
                let len = resolve_sym(*name).len() as u32;
                if pattern.span.end >= pattern.span.start + len {
                    let span = Span {
                        start: pattern.span.end - len,
                        ..pattern.span
                    };
                    self.local(span, *name, false);
                }
            }
            _ => {}
        }
    }

    /// The fields of a record pattern: `{ x: p }` matches `p`, the
    /// shorthand `{ x }` binds `x`.
    fn field_patterns(&mut self, fields: &[(Symbol, Span, Option<Pattern>)]) {
        for (name, span, sub) in fields {
            match sub {
                Some(sub) => self.pattern(sub),
                None => self.local(*span, *name, true),
            }
        }
    }
}
