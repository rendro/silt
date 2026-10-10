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

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use lsp_types::{Location, SymbolInformation, SymbolKind, Uri};

use crate::ast::{Decl, FnDecl, ImportTarget, PatternKind, Program, Selection, TypeBody, TypeDecl};
use crate::defs::{DefId, DefKind, Res};
use crate::intern::{Symbol, resolve as resolve_sym};
use crate::session::{ModuleId, Session};
use crate::source::{SourceFile, Span};

use super::Server;
use super::conversions::span_to_range;
use super::local_bindings::nearest_local_binding_for;
use super::names::{DefUse, Names};
use super::project::{path_key, project_dir};
use super::state::{DefInfo, Document};

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

/// What a name under the cursor is, for hover.
pub(super) enum Named {
    /// A definition of a file, with what its module knows of it.
    Definition(DefInfo),
    /// A local binding.
    Local,
    /// Anything else: one of silt's own names, a field, no name at all.
    Other,
}

/// Every use of a definition in the checked modules of every session,
/// and the punned declarations, by project directory and module: what a
/// references or rename query walks the workspace for. It holds until
/// the next analysis.
pub(super) struct DefUses {
    projects: Vec<(PathBuf, Vec<ModuleUses>)>,
}

struct ModuleUses {
    module: ModuleId,
    /// The URI of the module's file.
    uri: Option<Uri>,
    uses: Vec<(Span, DefId)>,
    punned_declarations: Vec<Span>,
}

/// A place that names a target.
pub(super) struct Place {
    pub(super) location: Location,
    /// The name is a record field written without its value or pattern
    /// (`P { x }`): a rename writes the field out, `P { x: new }`.
    pub(super) punned: bool,
}

/// The 1-based line of byte `at` of `source`.
fn line_of(source: &SourceFile, at: u32) -> u32 {
    span_to_range(
        &Span {
            file: crate::source::FileId::default(),
            start: at,
            end: at,
        },
        source,
    )
    .start
    .line
        + 1
}

impl Server {
    /// The names of the open document `doc`, with its session and module.
    fn names_of<'a>(&'a self, doc: &'a Document) -> Option<(Names<'a>, &'a Session, ModuleId)> {
        let program = doc.program.as_ref()?;
        let module = doc.module.as_ref()?;
        let session = &self.projects.get(&module.project)?.session;
        Some((Names::of(program, session, module.id), session, module.id))
    }

    /// What the name at byte `cursor` of the open document `uri` means,
    /// with the span of the name as written there.
    pub(super) fn target_at(&self, uri: &Uri, cursor: usize) -> Option<(Span, Target)> {
        let doc = self.documents.get(uri)?;
        let (names, session, module) = self.names_of(doc)?;
        let here: Vec<&DefUse> = names
            .defs
            .iter()
            .filter(|used| holds(used.span, cursor))
            .collect();
        if let Some(first) = here.first() {
            let keys = here.iter().map(|used| def_key(session, used.id)).collect();
            return Some((first.span, Target::Defs(keys)));
        }
        // The name of a declaration of this module.
        let defs = session.defs();
        let declared = defs.of_module(module).iter().copied().find_map(|id| {
            let def = defs.get(id);
            (def.span.is_in_source() && holds(def.span, cursor))
                .then(|| (def.span, Target::Defs(vec![def_key(session, id)])))
        });
        if declared.is_some() {
            return declared;
        }
        let local = names.locals.iter().find(|l| holds(l.span, cursor))?;
        Some((local.span, Target::Local(local.binding(&doc.locals)?)))
    }

    /// What the name at byte `cursor` of the open document `uri` is: for
    /// a definition, its type and documentation as the module that
    /// declares it has them.
    pub(super) fn named_at(&self, uri: &Uri, cursor: usize) -> Named {
        let Some((_, target)) = self.target_at(uri, cursor) else {
            return Named::Other;
        };
        let keys = match target {
            Target::Local(_) => return Named::Local,
            Target::Defs(keys) => keys,
        };
        let info = keys.iter().find_map(|key| {
            let DefKey::At { name, .. } = key else {
                return None;
            };
            self.projects.values().find_map(|project| {
                let session = &project.session;
                let def = session.defs().get(def_of_key(session, key)?);
                let checked = session.module_analysis(def.module)?;
                // A variant is known under its own name, as its type is.
                super::definitions::build_definitions(
                    &checked.ast,
                    Some(&checked.top_level),
                    &session.tables().record_fields(),
                )
                .remove(name)
            })
        });
        match info {
            Some(info) => Named::Definition(info),
            None => Named::Other,
        }
    }

    /// Why `target`, found in the document `uri`, cannot be renamed, if
    /// it cannot: it is one of silt's own names, or `self`.
    pub(super) fn not_renameable(&self, uri: &Uri, target: &Target) -> Option<String> {
        match target {
            Target::Defs(keys) => keys
                .iter()
                .any(|key| matches!(key, DefKey::Builtin(_)))
                .then(|| "is a builtin and cannot be renamed".to_string()),
            Target::Local(binding) => {
                let doc = self.documents.get(uri)?;
                let local = doc.locals.iter().find(|b| b.binding_offset == *binding)?;
                (resolve_sym(local.name) == "self").then(|| {
                    "is the receiver of a method and cannot be renamed: a method's first \
                     parameter is `self`"
                        .to_string()
                })
            }
        }
    }

    /// Every place that names `target`, which was found in the open
    /// document `uri`; with the declaration if `include_definition`.
    /// Sorted by file and position.
    pub(super) fn places_of(
        &mut self,
        uri: &Uri,
        target: &Target,
        include_definition: bool,
    ) -> Vec<Place> {
        let mut places = match target {
            Target::Local(binding) => self.local_places(uri, *binding, include_definition),
            Target::Defs(keys) => {
                if self.def_uses.is_none() {
                    self.check_importers();
                    self.def_uses = Some(self.walk_def_uses());
                }
                self.def_places(keys, include_definition)
            }
        };
        let key = |p: &Place| {
            (
                p.location.uri.as_str().to_string(),
                p.location.range.start.line,
                p.location.range.start.character,
            )
        };
        places.sort_by_key(key);
        places.dedup_by(|a, b| a.location == b.location);
        places
    }

    /// The locations of [`Server::places_of`].
    pub(super) fn references_to(
        &mut self,
        uri: &Uri,
        target: &Target,
        include_definition: bool,
    ) -> Vec<Location> {
        self.places_of(uri, target, include_definition)
            .into_iter()
            .map(|place| place.location)
            .collect()
    }

    /// The binders of the local binding `binding` of the document `uri`
    /// (one per alternative of an or-pattern), and the uses that resolve
    /// to it.
    fn local_places(&self, uri: &Uri, binding: usize, include_binders: bool) -> Vec<Place> {
        let Some(doc) = self.documents.get(uri) else {
            return Vec::new();
        };
        let Some((names, ..)) = self.names_of(doc) else {
            return Vec::new();
        };
        names
            .locals
            .iter()
            .filter(|l| include_binders || !l.binder)
            .filter(|l| l.binding(&doc.locals) == Some(binding))
            .map(|l| Place {
                location: Location::new(uri.clone(), span_to_range(&l.span, &doc.source)),
                punned: l.punned,
            })
            .collect()
    }

    /// Where `target` is declared: a local's binder in the document
    /// `uri` (the first alternative's, of an or-pattern), a definition's
    /// name in its file. Nothing for one of silt's own.
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
                        self.location_in(session, def.module, def.span)
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

    /// `span` of the module `module` of `session` as a location.
    fn location_in(&self, session: &Session, module: ModuleId, span: Span) -> Option<Location> {
        let module = session.graph().module(module);
        let source = session.sources().get(module.file?)?;
        let uri = self.uri_for_path(&module.path)?;
        Some(Location::new(uri, span_to_range(&span, source)))
    }

    /// The declaration of the method whose name at a call holds byte
    /// `cursor` of the open document `uri`: the method of the impl the
    /// checker selected, when that impl writes it, else the trait's.
    pub(super) fn method_declaration_at(&self, uri: &Uri, cursor: usize) -> Option<Location> {
        let doc = self.documents.get(uri)?;
        let (names, session, _) = self.names_of(doc)?;
        let method = names.methods.iter().find(|m| holds(m.span, cursor))?;
        let (tr, ty) = match method.selection {
            Selection::Impl { tr, ty } => (tr, Some(ty)),
            Selection::Native { tr } | Selection::Dynamic { tr } => (tr, None),
            Selection::Field | Selection::FieldCall => return None,
        };
        let named = |methods: &[FnDecl]| {
            methods
                .iter()
                .find(|m| m.name == method.name)
                .map(|m| m.name_span)
        };
        let mut of_trait = None;
        for module in session.graph().modules() {
            let Some(analysis) = session.module_analysis(module.id) else {
                continue;
            };
            for decl in &analysis.ast.decls {
                match decl {
                    Decl::TraitImpl(ti)
                        if ti.trait_res == Some(Res::Def(tr.0))
                            && ty.is_some_and(|ty| ti.target_res == Some(Res::Def(ty.0))) =>
                    {
                        if let Some(span) = named(&ti.methods) {
                            return self.location_in(session, module.id, span);
                        }
                    }
                    Decl::Trait(t)
                        if session.defs().get(tr.0).module == module.id
                            && t.name_span == session.defs().get(tr.0).span =>
                    {
                        of_trait = named(&t.methods).map(|span| (module.id, span));
                    }
                    _ => {}
                }
            }
        }
        let (module, span) = of_trait?;
        self.location_in(session, module, span)
    }

    /// The definition of the type `id` of the session of the open
    /// document `doc`, as a target.
    pub(super) fn type_target(&self, doc: &Document, id: crate::defs::TypeId) -> Option<Target> {
        let module = doc.module.as_ref()?;
        let session = &self.projects.get(&module.project)?.session;
        Some(Target::Defs(vec![def_key(session, id.0)]))
    }

    /// Check the workspace files that no session has checked and that
    /// import a module of their project or of a dependency: a file that is not open and that no open
    /// document imports is only parsed until a query needs what its
    /// names mean. Which module an `import` names is the session's to
    /// say (a file of the package, a dependency's library), so every
    /// such file is checked; the reference walk then finds the importers
    /// by what their names resolve to.
    fn check_importers(&mut self) {
        let imports_a_file = |program: &Program| {
            program.decls.iter().any(|decl| match decl {
                Decl::Import(
                    ImportTarget::Module(m)
                    | ImportTarget::Items(m, _)
                    | ImportTarget::Alias(m, ..),
                    _,
                ) => !crate::module::is_builtin_module(&resolve_sym(*m)),
                _ => false,
            })
        };
        let importers: Vec<PathBuf> = self
            .documents
            .values()
            .filter(|doc| !doc.open && doc.program.as_deref().is_some_and(imports_a_file))
            .map(|doc| doc.path.clone())
            .collect();
        // A project of the workspace that has no open document gets a
        // session for the query; the next analysis drops it again.
        for path in importers {
            let dir = project_dir(&path);
            self.projects
                .entry(dir.clone())
                .or_insert_with(|| super::project::Project::new(&dir))
                .check_file(&path);
        }
    }

    /// The uses of definitions in every checked module of every session.
    fn walk_def_uses(&self) -> DefUses {
        // The URI of each file, an open document's first: looked up once
        // per module, not once per place.
        let mut uris: HashMap<&PathBuf, &Uri> = HashMap::new();
        for (uri, doc) in self.documents.iter().filter(|(_, doc)| !doc.open) {
            uris.insert(&doc.key, uri);
        }
        for (uri, doc) in self.documents.iter().filter(|(_, doc)| doc.open) {
            uris.insert(&doc.key, uri);
        }
        let mut projects = Vec::new();
        for (dir, project) in &self.projects {
            let session = &project.session;
            let mut modules = Vec::new();
            for module in session.graph().modules() {
                let Some(analysis) = session.module_analysis(module.id) else {
                    continue;
                };
                let names = Names::of(&analysis.ast, session, module.id);
                modules.push(ModuleUses {
                    module: module.id,
                    uri: uris
                        .get(&path_key(&module.path))
                        .map(|uri| (*uri).clone())
                        .or_else(|| super::path_to_file_uri(&module.path)),
                    uses: names.defs.iter().map(|used| (used.span, used.id)).collect(),
                    punned_declarations: names.punned_declarations,
                });
            }
            projects.push((dir.clone(), modules));
        }
        DefUses { projects }
    }

    /// Every place in the checked modules of every session that names
    /// one of `keys`, from what the last walk found.
    fn def_places(&self, keys: &[DefKey], include_definition: bool) -> Vec<Place> {
        let mut places = Vec::new();
        let Some(def_uses) = &self.def_uses else {
            return places;
        };
        for (dir, modules) in &def_uses.projects {
            let Some(project) = self.projects.get(dir) else {
                continue;
            };
            let session = &project.session;
            let ids: HashSet<DefId> = keys
                .iter()
                .filter_map(|key| def_of_key(session, key))
                .collect();
            if ids.is_empty() {
                continue;
            }
            for module in modules {
                let source = session
                    .graph()
                    .module(module.module)
                    .file
                    .and_then(|file| session.sources().get(file));
                let (Some(uri), Some(source)) = (&module.uri, source) else {
                    continue;
                };
                let place = |span: Span, punned: bool| Place {
                    location: Location::new(uri.clone(), span_to_range(&span, source)),
                    punned,
                };
                for (span, _) in module.uses.iter().filter(|(_, id)| ids.contains(id)) {
                    places.push(place(*span, false));
                }
                if !include_definition {
                    continue;
                }
                for id in &ids {
                    let def = session.defs().get(*id);
                    if def.module == module.module && def.span.is_in_source() {
                        places.push(place(
                            def.span,
                            module.punned_declarations.contains(&def.span),
                        ));
                    }
                }
            }
        }
        places
    }

    /// Why a rename of `target`, found in the open document `uri`,
    /// cannot be the same program under another name: a file that it
    /// touches, or that could name the target, has a declaration that
    /// failed to parse (a recovery stub stands in for it, or nothing),
    /// so its names are unknown. The file and the place of its first
    /// error; `None` when every such file is whole.
    ///
    /// The files: the document's own, and for a definition its module
    /// and every checked module that imports it (a module that imports
    /// a broken one sees its names as unknown, too).
    pub(super) fn incomplete_for_rename(&mut self, uri: &Uri, target: &Target) -> Option<String> {
        // (The importers that are not open are checked for the query.)
        self.places_of(uri, target, true);
        let broken = |session: &Session, id: ModuleId| -> Option<String> {
            let module = session.graph().module(id);
            if !module.incomplete {
                return None;
            }
            let file = module
                .path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let error = module.problems.first()?;
            let (line, col) = session
                .sources()
                .line_col((error.span.file, error.span.start));
            Some(format!(
                "{file} has a syntax error at {line}:{col} ({})",
                error.message
            ))
        };
        let doc = self.documents.get(uri)?;
        if let Some((session, module)) = self.checked(doc)
            && let Some(broken) = broken(session, module)
        {
            return Some(broken);
        }
        let Target::Defs(keys) = target else {
            return None;
        };
        for project in self.projects.values() {
            let session = &project.session;
            let homes: HashSet<ModuleId> = keys
                .iter()
                .filter_map(|key| def_of_key(session, key))
                .map(|id| session.defs().get(id).module)
                .collect();
            if homes.is_empty() {
                continue;
            }
            for module in session.graph().modules() {
                let names_it = homes.contains(&module.id)
                    || !session.graph().reach(module.id).is_disjoint(&homes);
                if names_it && let Some(broken) = broken(session, module.id) {
                    return Some(broken);
                }
            }
        }
        // (A file with an edit is one of those: it names the target.)
        None
    }

    /// The places of the open document `uri` that name `target`, which
    /// was found in it, the declaration included: what a highlight
    /// shows. Nothing but this document is read.
    pub(super) fn places_in_document(&self, uri: &Uri, target: &Target) -> Vec<Location> {
        let keys = match target {
            Target::Local(binding) => {
                return self
                    .local_places(uri, *binding, true)
                    .into_iter()
                    .map(|place| place.location)
                    .collect();
            }
            Target::Defs(keys) => keys,
        };
        let Some(doc) = self.documents.get(uri) else {
            return Vec::new();
        };
        let Some((names, session, module)) = self.names_of(doc) else {
            return Vec::new();
        };
        let ids: HashSet<DefId> = keys
            .iter()
            .filter_map(|key| def_of_key(session, key))
            .collect();
        let declared = ids.iter().filter_map(|id| {
            let def = session.defs().get(*id);
            (def.module == module && def.span.is_in_source()).then_some(def.span)
        });
        let mut spans: Vec<Span> = names
            .defs
            .iter()
            .filter(|used| ids.contains(&used.id))
            .map(|used| used.span)
            .chain(declared)
            .collect();
        spans.sort_by_key(|span| span.start);
        spans.dedup();
        spans
            .into_iter()
            .map(|span| Location::new(uri.clone(), span_to_range(&span, &doc.source)))
            .collect()
    }

    /// Why renaming `target`, found in the open document `uri`, to `new`
    /// would change what a name means, if it would: at a place that
    /// names the target, `new` already names something else (a local, a
    /// top-level name of that module, one of silt's own names), so the
    /// renamed name would be taken for that, or that for it.
    pub(super) fn rename_clash(&mut self, uri: &Uri, target: &Target, new: &str) -> Option<String> {
        let new_sym = crate::intern::intern(new);
        let places = self.places_of(uri, target, true);
        // One of silt's own names, visible in every module: a builtin
        // function, type, trait or constructor.
        let own = crate::module::builtin_free_function_names().contains(&new)
            || crate::types::builtins::iter_all().any(|ty| ty.name == new)
            || crate::module::builtin_module_types().contains(&new)
            || crate::module::all_builtin_constructor_names().any(|name| name == new)
            || crate::defs::BUILTIN_TRAITS.contains(&new)
            || self.projects.values().any(|project| {
                let defs = project.session.defs();
                defs.of_module(ModuleId::PRELUDE)
                    .iter()
                    .any(|id| defs.get(*id).name == new_sym)
            });
        if own {
            return Some(format!("`{new}` is one of silt's own names"));
        }
        // The module of each file, in the session that checked it.
        let mut modules: HashMap<PathBuf, (&Session, ModuleId)> = HashMap::new();
        for project in self.projects.values() {
            let session = &project.session;
            for module in session.graph().modules() {
                if session.module_analysis(module.id).is_some() {
                    modules.insert(path_key(&module.path), (session, module.id));
                }
            }
        }
        // The places are sorted by file: each file is looked at once.
        let mut top_level_of: Option<(&Uri, bool)> = None;
        for place in &places {
            let Some(doc) = self.documents.get(&place.location.uri) else {
                continue;
            };
            let file = || {
                doc.path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default()
            };
            let at =
                super::conversions::position_to_offset(&doc.source, &place.location.range.start);
            if let Some(local) = nearest_local_binding_for(&doc.locals, new_sym, at) {
                return Some(format!(
                    "`{new}` is already a local name at {}:{} (bound at line {})",
                    file(),
                    place.location.range.start.line + 1,
                    line_of(&doc.source, local.binding_offset as u32)
                ));
            }
            // A top-level name of the module: a declaration, an import.
            let taken = match top_level_of {
                Some((uri, taken)) if *uri == place.location.uri => taken,
                _ => {
                    let taken = modules.get(&doc.key).is_some_and(|(session, module)| {
                        session.module_analysis(*module).is_some_and(|analysis| {
                            analysis.scope.values.contains_key(&new_sym)
                                || analysis.scope.types.contains_key(&new_sym)
                        })
                    });
                    top_level_of = Some((&place.location.uri, taken));
                    taken
                }
            };
            if taken {
                return Some(format!("`{new}` is already a top-level name of {}", file()));
            }
        }
        None
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
