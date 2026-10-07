//! The module graph: every module a session has read, what each of its
//! imports names, and the order in which they are checked.
//!
//! A module is read and parsed once. The graph is built breadth-first
//! from an entry module; an import names a builtin module or another
//! module of the graph. A module whose file cannot be read or parsed is
//! still a node of the graph: it is reported once, and its importers
//! treat it as poisoned instead of failing on every name it would have
//! supplied.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use crate::ast::{self, Decl};
use crate::diagnostic::{Code, Diagnostic};
use crate::intern::{Symbol, intern, resolve};
use crate::lexer::{Lexer, Token};
use crate::module;
use crate::parser::Parser;
use crate::source::{FileId, SourceMap, SourceName, Span};

use crate::package_graph::PackageId;

use super::packages::Packages;

/// A module of the graph: an index into [`ModuleGraph::modules`].
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct ModuleId(pub u32);

impl ModuleId {
    pub(crate) fn index(self) -> usize {
        self.0 as usize
    }
}

/// One module: a `.silt` file of a package.
pub struct Module {
    pub id: ModuleId,
    /// The package the module belongs to.
    pub package: PackageId,
    /// That package's `[package].name` (`__local__` for a script).
    pub package_name: Symbol,
    /// The module's name in its package: `"lib"` for a dependency's
    /// library, `"util"` for `src/util.silt`, the file's stem for an
    /// entry file.
    pub name: Symbol,
    /// The file, as it is named in diagnostics.
    pub path: PathBuf,
    /// The file's text in the session's source map; `None` when it could
    /// not be read.
    pub file: Option<FileId>,
    /// The declarations as parsed, before any check; `None` when the
    /// file could not be read or lexed. Checking works on a copy, so a
    /// module can be checked again after one of its imports changed.
    pub ast: Option<ast::Program>,
    /// Why the module could not be read, its lex error, or its parse
    /// errors.
    pub problems: Vec<Diagnostic>,
    /// Why the module's file could not be read (the kind and text of the
    /// I/O error). Each import of the module reports it at its own span
    /// ([`Import::problem`]).
    pub load_error: Option<(std::io::ErrorKind, String)>,
    /// The import that first brought the module into the graph: the
    /// importing module, the name written and the span of the `import`.
    pub first_import: Option<(ModuleId, Symbol, Span)>,
    /// The module's imports, in source order.
    pub imports: Vec<Import>,
    /// For a host module, its index in the session's `Config::host`.
    pub host: Option<usize>,
}

impl Module {
    /// Whether the module failed to load or parse. An importer treats
    /// such a module as poisoned; as an entry, a module with parse
    /// errors is still checked as far as it parsed.
    pub fn failed(&self) -> bool {
        self.ast.is_none() || !self.problems.is_empty()
    }

    /// The modules this module imports, in source order, each once.
    pub fn imported_modules(&self) -> Vec<ModuleId> {
        let mut seen = HashSet::new();
        self.imports
            .iter()
            .filter_map(|import| match import.resolution {
                ImportResolution::Module(id) if seen.insert(id) => Some(id),
                _ => None,
            })
            .collect()
    }
}

/// One `import` of a module.
pub struct Import {
    /// The module name written after `import`.
    pub name: Symbol,
    /// The span of the `import` declaration.
    pub span: Span,
    pub resolution: ImportResolution,
    /// Why the module it names could not be read, at this import: each
    /// importer gets its own error, made again when it is reparsed.
    pub problem: Option<Diagnostic>,
}

/// What an `import` names.
#[derive(Debug, Clone)]
pub enum ImportResolution {
    /// A builtin module (`list`, `io`, ...).
    Builtin,
    /// A module of the graph.
    Module(ModuleId),
    /// An earlier REPL cell (`<repl:k>`), which the session imports into
    /// each later cell: checked already and installed, it is not an edge
    /// of the graph, so a cell's graph does not grow with the session.
    Cell(ModuleId),
    /// Nothing: the name resolves to no file. The diagnostic says why.
    Unresolved(Diagnostic),
}

/// The declarations of the module file `file`, whose text is `text`,
/// with their doc comments, and its lex error or parse errors. A text
/// that does not lex has no declarations.
pub fn parse_text(file: FileId, text: &str) -> (Option<ast::Program>, Vec<Diagnostic>) {
    match Lexer::new(file, text).tokenize() {
        Ok(tokens) => {
            let (program, errors) = Parser::new(tokens, text)
                .with_docs()
                .parse_program_recovering();
            (Some(program), errors)
        }
        Err(e) => (None, vec![e]),
    }
}

/// Every module a session has read.
#[derive(Default)]
pub struct ModuleGraph {
    modules: Vec<Module>,
    /// Modules by canonical file path, so a file is one module however
    /// it is reached.
    by_path: HashMap<PathBuf, ModuleId>,
    /// The host modules, by name.
    hosts: HashMap<Symbol, ModuleId>,
}

/// The modules reachable from an entry, in the order they are checked.
pub struct Ordering {
    /// Every module reachable from the entry, each after the modules it
    /// imports; the entry is last.
    pub modules: Vec<ModuleId>,
    /// An `ImportCycle` error for each import that closes a cycle.
    pub cycles: Vec<Diagnostic>,
    /// The imports that close a cycle, as (importing module, imported
    /// module): the importer is checked before the imported module, which
    /// it then treats as poisoned.
    pub back_edges: HashSet<(ModuleId, ModuleId)>,
}

impl ModuleGraph {
    /// The module `id`.
    pub fn module(&self, id: ModuleId) -> &Module {
        &self.modules[id.index()]
    }

    /// Every module of the graph, by id.
    pub fn modules(&self) -> &[Module] {
        &self.modules
    }

    /// The module of the file at `path`, if the graph has it.
    pub fn module_at(&self, path: &Path) -> Option<ModuleId> {
        self.by_path.get(&canonical_key(path)).copied()
    }

    /// Enter the file at `path`, with the text `text`, as an entry
    /// module of `package`, or give its text to the module the graph has
    /// for that file. The module is parsed; its imports are resolved by
    /// [`ModuleGraph::load`].
    pub(super) fn enter(
        &mut self,
        path: &Path,
        name: SourceName,
        text: &str,
        package: (PackageId, Symbol),
        sources: &mut SourceMap,
    ) -> ModuleId {
        let id = match self.module_at(path) {
            Some(id) => id,
            None => {
                let module_name = path
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.push(Module {
                    id: ModuleId(0),
                    package: package.0,
                    package_name: package.1,
                    name: intern(&module_name),
                    path: path.to_path_buf(),
                    file: None,
                    ast: None,
                    problems: Vec::new(),
                    load_error: None,
                    first_import: None,
                    imports: Vec::new(),
                    host: None,
                })
            }
        };
        self.parse(id, name, text, sources);
        id
    }

    /// The REPL entry a session imports into each later entry by its
    /// name, `<repl:n>`: a name no program can write.
    fn cell_module(&self, name: Symbol) -> Option<ModuleId> {
        let name = resolve(name);
        if !name.starts_with("<repl:") {
            return None;
        }
        self.module_at(Path::new(&name))
    }

    /// The declarations of module `id`, for the session to add to.
    pub(super) fn ast_mut(&mut self, id: ModuleId) -> Option<&mut ast::Program> {
        self.modules[id.index()].ast.as_mut()
    }

    /// Enter the host module `host`, the `index`th of the session's
    /// configuration. Its signatures are parsed as a module of `pub fn`
    /// headers; what is wrong with them is the module's problems.
    pub(super) fn enter_host(
        &mut self,
        index: usize,
        host: &super::HostModule,
        sources: &mut SourceMap,
    ) -> ModuleId {
        let name = intern(&host.name);
        let id = self.push(Module {
            id: ModuleId(0),
            package: PackageId(u32::MAX),
            package_name: intern("<host>"),
            name,
            path: PathBuf::from(format!("<host:{}>", host.name)),
            file: None,
            ast: None,
            problems: Vec::new(),
            load_error: None,
            first_import: None,
            imports: Vec::new(),
            host: Some(index),
        });
        let text = host.signatures();
        self.parse(id, SourceName::Host(host.name.clone()), &text, sources);
        let file = self.module(id).file.expect("the text was registered");
        let module = &mut self.modules[id.index()];
        // A signature that does not parse is reported as such, and
        // nothing more is said about the signatures.
        let parsed = module.problems.is_empty();
        let mut problems = Vec::new();
        let at_start = Span::point(file, 0);
        if !is_module_name(&host.name) {
            problems.push(Diagnostic::error(
                Code::HostModuleCollision,
                at_start,
                format!(
                    "the host module `{}` cannot be imported: its name is not an identifier",
                    host.name
                ),
            ));
        } else if module::is_builtin_module(&host.name) {
            problems.push(Diagnostic::error(
                Code::HostModuleCollision,
                at_start,
                format!(
                    "the host module `{0}` cannot be imported: `import {0}` names the builtin \
                     module `{0}`",
                    host.name
                ),
            ));
        }
        if self.hosts.insert(name, id).is_some() {
            problems.push(Diagnostic::error(
                Code::HostModuleCollision,
                at_start,
                format!("two host modules are named `{}`", host.name),
            ));
        }
        let help = "write it as `fn double(x: Int) -> Int`";
        if let Some(ast) = module.ast.as_mut().filter(|_| parsed) {
            let mut signature_problems = Vec::new();
            for decl in &mut ast.decls {
                let Decl::Fn(f) = decl else {
                    signature_problems.push(
                        Diagnostic::error(
                            Code::HostSignature,
                            decl_span(decl),
                            "a host function is declared by a `fn` header with no body",
                        )
                        .with_help(help),
                    );
                    continue;
                };
                f.is_pub = true;
                if !f.is_signature_only {
                    signature_problems.push(
                        Diagnostic::error(
                            Code::HostSignature,
                            f.span,
                            "a host function is declared by a `fn` header with no body",
                        )
                        .with_help(help),
                    );
                    continue;
                }
                let untyped = f
                    .params
                    .iter()
                    .any(|p| p.kind == ast::ParamKind::Data && p.ty.is_none())
                    || f.return_type.is_none();
                if untyped {
                    signature_problems.push(
                        Diagnostic::error(
                            Code::HostSignature,
                            f.span,
                            format!(
                                "the host function `{}` must declare the type of each \
                                 parameter and its return type",
                                f.name
                            ),
                        )
                        .with_help(help),
                    );
                }
                let types = f.params.iter().filter_map(|p| p.ty.as_ref());
                if types
                    .chain(&f.return_type)
                    .any(super::host::mentions_function)
                {
                    signature_problems.push(Diagnostic::error(
                        Code::HostSignature,
                        f.span,
                        format!(
                            "the host function `{}` cannot take or return a function: a host \
                             function has no way to call one",
                            f.name
                        ),
                    ));
                }
            }
            let declared = ast.decls.len();
            if declared != host.fns.len() {
                signature_problems.push(Diagnostic::error(
                    Code::HostSignature,
                    at_start,
                    format!(
                        "the host module `{}` has {} but its signatures declare {declared}",
                        host.name,
                        plural(host.fns.len(), "function")
                    ),
                ));
            } else {
                for (decl, function) in ast.decls.iter().zip(&host.fns) {
                    if let (Decl::Fn(f), Some(arity)) = (decl, function.arity)
                        && f.params.len() != arity
                    {
                        signature_problems.push(Diagnostic::error(
                            Code::HostSignature,
                            f.span,
                            format!(
                                "the host function `{}` is given a Rust function of {}, but \
                                 its signature declares {}",
                                f.name,
                                plural(arity, "argument"),
                                plural(f.params.len(), "parameter")
                            ),
                        ));
                    }
                }
            }
            problems.extend(signature_problems);
        }
        module.problems.extend(problems);
        id
    }

    /// Add `module`, giving it the next id.
    fn push(&mut self, mut module: Module) -> ModuleId {
        let id = ModuleId(self.modules.len() as u32);
        module.id = id;
        self.by_path.insert(canonical_key(&module.path), id);
        self.modules.push(module);
        id
    }

    /// Register `text` as the file of module `id` and parse it. Its
    /// imports are left unresolved until [`ModuleGraph::load`].
    fn parse(&mut self, id: ModuleId, name: SourceName, text: &str, sources: &mut SourceMap) {
        let cell = match name {
            SourceName::Repl(n) => Some(n),
            _ => None,
        };
        let file = sources.add(name, text.into());
        // A module that could not be read and has a text now (an editor
        // opened the file): what each importer recorded about the failed
        // load at its `import` no longer holds.
        if self.modules[id.index()].load_error.is_some() {
            for importer in &mut self.modules {
                for import in &mut importer.imports {
                    if matches!(import.resolution, ImportResolution::Module(target) if target == id)
                    {
                        import.problem = None;
                    }
                }
            }
        }
        let module = &mut self.modules[id.index()];
        module.file = Some(file);
        module.imports.clear();
        module.load_error = None;
        let (ast, problems) = match cell {
            Some(n) => match Lexer::new(file, text).tokenize() {
                Ok(tokens) => {
                    let (program, errors) =
                        Parser::new(tokens, text).parse_cell(intern(&cell_name(n)));
                    (Some(program), errors)
                }
                Err(e) => (None, vec![e]),
            },
            None => parse_text(file, text),
        };
        module.ast = ast;
        module.problems = problems;
    }

    /// Resolve the imports of `entry` and of every module they reach,
    /// breadth-first, reading and parsing each module file once. A
    /// module whose imports are resolved already is not looked at again.
    pub(super) fn load(
        &mut self,
        entry: ModuleId,
        packages: &Packages,
        overlays: &HashMap<PathBuf, String>,
        sources: &mut SourceMap,
    ) {
        let mut queue = VecDeque::from([entry]);
        let mut visited = HashSet::from([entry]);
        while let Some(id) = queue.pop_front() {
            // A failed module is not checked, so its imports are not
            // followed; an entry is checked as far as it parsed.
            if (id == entry || !self.module(id).failed()) && !self.imports_resolved(id) {
                self.resolve_imports(id, packages, overlays, sources);
            }
            if id != entry && self.module(id).failed() {
                continue;
            }
            for next in self.module(id).imported_modules() {
                if visited.insert(next) {
                    queue.push_back(next);
                }
            }
        }
    }

    /// Whether the imports of `id` have been resolved: every `import`
    /// declaration of its parsed text has an entry in `imports`.
    fn imports_resolved(&self, id: ModuleId) -> bool {
        let module = self.module(id);
        match &module.ast {
            None => true,
            Some(ast) => module.imports.len() == import_decls(ast).count(),
        }
    }

    /// Resolve each import of module `id`, adding the modules it names
    /// that the graph does not have yet.
    fn resolve_imports(
        &mut self,
        id: ModuleId,
        packages: &Packages,
        overlays: &HashMap<PathBuf, String>,
        sources: &mut SourceMap,
    ) {
        let decls: Vec<(Symbol, Span)> = match &self.module(id).ast {
            Some(ast) => import_decls(ast).collect(),
            None => return,
        };
        let package = self.module(id).package;
        let mut imports = Vec::with_capacity(decls.len());
        for (name, span) in decls {
            let mut problem = None;
            let resolution = if let Some(feature) = module::missing_feature(&resolve(name)) {
                // The module is known, so no file or dependency is looked
                // for, and what the program takes from it is not reported
                // again name by name.
                ImportResolution::Unresolved(
                    Diagnostic::error(
                        Code::ModuleNotFound,
                        span,
                        module::needs_feature(&format!("the builtin module '{name}'"), feature),
                    )
                    .with_help(format!("rebuild silt with `--features {feature}`")),
                )
            } else if module::is_builtin_module(&resolve(name)) {
                ImportResolution::Builtin
            } else if let Some(cell) = self.cell_module(name) {
                ImportResolution::Cell(cell)
            } else if let Some(&host) = self.hosts.get(&name) {
                // A dependency or module file of the same name would be
                // hidden by the host module.
                match resolve_import(packages, package, name, span) {
                    Ok(target)
                        if overlays.contains_key(&canonical_key(&target.path))
                            || target.path.exists() =>
                    {
                        let dependency = packages.dependency(package, name).is_some();
                        ImportResolution::Unresolved(host_collision(
                            name,
                            &target.path,
                            dependency,
                            span,
                        ))
                    }
                    _ => ImportResolution::Module(host),
                }
            } else {
                match resolve_import(packages, package, name, span) {
                    Ok(target) => {
                        let target_id =
                            self.module_for(target, (id, name, span), overlays, sources);
                        let module = self.module(target_id);
                        if module.load_error.is_some() && is_no_file(&module.path) {
                            let name = resolve(name);
                            problem = Some(Diagnostic::error(
                                Code::ModuleNotFound,
                                span,
                                format!(
                                    "no open document named '{name}' beside this one \
                                     (an unsaved document reads no files)"
                                ),
                            ));
                        } else if let Some((kind, text)) = &module.load_error {
                            let mut d = module::module_load_error(
                                &resolve(name),
                                &module.path,
                                &crate::git::escape_for_display(&module_path_for_display(
                                    &module.path,
                                )),
                                &std::io::Error::new(*kind, text.clone()),
                                span,
                            );
                            if let Some(help) = undeclared_dependency_help(packages, package, name)
                            {
                                d.help.insert(0, help);
                            }
                            problem = Some(d);
                        }
                        ImportResolution::Module(target_id)
                    }
                    Err(d) => ImportResolution::Unresolved(d),
                }
            };
            imports.push(Import {
                name,
                span,
                resolution,
                problem,
            });
        }
        self.modules[id.index()].imports = imports;
    }

    /// The module of the file `target` names, read and parsed if the
    /// graph does not have it yet. `import` is the import that reaches
    /// it first. A file that cannot be read leaves the module's
    /// `load_error`, which each import reports.
    fn module_for(
        &mut self,
        target: ImportedFile,
        import: (ModuleId, Symbol, Span),
        overlays: &HashMap<PathBuf, String>,
        sources: &mut SourceMap,
    ) -> ModuleId {
        if let Some(id) = self.module_at(&target.path) {
            return id;
        }
        let id = self.push(Module {
            id: ModuleId(0),
            package: target.package,
            package_name: target.package_name,
            name: target.module,
            path: target.path.clone(),
            file: None,
            ast: None,
            problems: Vec::new(),
            load_error: None,
            first_import: Some(import),
            imports: Vec::new(),
            host: None,
        });
        let (_, name, span) = import;
        let text = match overlays.get(&canonical_key(&target.path)) {
            Some(text) => Ok(text.clone()),
            None if is_no_file(&target.path) => {
                Err(std::io::Error::from(std::io::ErrorKind::NotFound))
            }
            None => std::fs::read_to_string(&target.path),
        };
        match text {
            Ok(text) => {
                let source_name = if overlays.contains_key(&canonical_key(&target.path)) {
                    SourceName::Overlay(target.path.clone())
                } else {
                    SourceName::Path(target.path.clone())
                };
                self.parse(id, source_name, &text, sources);
                let module_name = resolve(name);
                let module = &mut self.modules[id.index()];
                module.problems = std::mem::take(&mut module.problems)
                    .into_iter()
                    .map(|d| d.with_label(span, format!("module '{module_name}' is imported here")))
                    .collect();
            }
            Err(e) => {
                self.modules[id.index()].load_error = Some((e.kind(), e.to_string()));
            }
        }
        id
    }

    /// Reparse module `id` from `text` (an editor's buffer). Its imports
    /// are resolved again by the next [`ModuleGraph::load`].
    pub(super) fn reparse(
        &mut self,
        id: ModuleId,
        name: SourceName,
        text: &str,
        sources: &mut SourceMap,
    ) {
        self.parse(id, name, text, sources);
        if self.module(id).first_import.is_some() && self.module(id).failed() {
            // An imported module's problems are labelled with the import
            // that reached it, as when it was first read.
            if let Some((_, import_name, span)) = self.module(id).first_import {
                let module_name = resolve(import_name);
                let module = &mut self.modules[id.index()];
                module.problems = std::mem::take(&mut module.problems)
                    .into_iter()
                    .map(|d| d.with_label(span, format!("module '{module_name}' is imported here")))
                    .collect();
            }
        }
    }

    /// The modules reachable from `entry`, each after the modules it
    /// imports (a depth-first postorder), and the import cycles among
    /// them. Failed modules are in the order too; their imports are not
    /// followed.
    pub fn order(&self, entry: ModuleId) -> Ordering {
        #[derive(Clone, Copy, PartialEq)]
        enum Mark {
            Unvisited,
            OnStack,
            Done,
        }
        let mut marks = vec![Mark::Unvisited; self.modules.len()];
        let mut ordering = Ordering {
            modules: Vec::new(),
            cycles: Vec::new(),
            back_edges: HashSet::new(),
        };
        // An explicit stack of (module, index of the next import to
        // follow), so a long import chain cannot overflow the Rust stack.
        let mut stack: Vec<(ModuleId, usize)> = vec![(entry, 0)];
        marks[entry.index()] = Mark::OnStack;
        while let Some(&mut (id, ref mut next)) = stack.last_mut() {
            let module = self.module(id);
            let edges: &[Import] = if module.failed() && id != entry {
                &[]
            } else {
                &module.imports
            };
            if *next >= edges.len() {
                marks[id.index()] = Mark::Done;
                ordering.modules.push(id);
                stack.pop();
                continue;
            }
            let import = &edges[*next];
            *next += 1;
            let ImportResolution::Module(target) = import.resolution else {
                continue;
            };
            match marks[target.index()] {
                Mark::Unvisited => {
                    marks[target.index()] = Mark::OnStack;
                    stack.push((target, 0));
                }
                Mark::OnStack => {
                    if ordering.back_edges.insert((id, target)) {
                        let start = stack.iter().position(|(m, _)| *m == target).unwrap_or(0);
                        let chain: Vec<ModuleId> = stack[start..].iter().map(|(m, _)| *m).collect();
                        ordering.cycles.push(self.cycle_error(&chain, import));
                    }
                }
                Mark::Done => {}
            }
        }
        ordering
    }

    /// The error for the import `closing`, made by the last module of
    /// `chain`, that imports the first one again. The message names the
    /// chain; each import along it is a label.
    fn cycle_error(&self, chain: &[ModuleId], closing: &Import) -> Diagnostic {
        let first = chain[0];
        let single_package = chain
            .iter()
            .all(|m| self.module(*m).package == self.module(first).package);
        let name_of = |id: ModuleId| {
            let module = self.module(id);
            if single_package {
                resolve(module.name)
            } else {
                format!("{}::{}", module.package_name, module.name)
            }
        };
        let mut names: Vec<String> = chain.iter().map(|m| name_of(*m)).collect();
        names.push(name_of(first));
        let mut d = Diagnostic::error(
            Code::ImportCycle,
            closing.span,
            format!(
                "circular import detected: {} (module '{}' imports itself directly or indirectly)",
                names.join(" -> "),
                closing.name
            ),
        );
        for pair in chain.windows(2) {
            let (from, to) = (pair[0], pair[1]);
            if let Some(import) = self
                .module(from)
                .imports
                .iter()
                .find(|i| matches!(i.resolution, ImportResolution::Module(t) if t == to))
            {
                d = d.with_label(
                    import.span,
                    format!("'{}' imports '{}' here", name_of(from), name_of(to)),
                );
            }
        }
        d
    }

    /// Every module that imports `id`, directly or not.
    pub fn reverse_dependents(&self, id: ModuleId) -> Vec<ModuleId> {
        let mut found: Vec<ModuleId> = Vec::new();
        let mut queue = VecDeque::from([id]);
        let mut seen = HashSet::from([id]);
        while let Some(target) = queue.pop_front() {
            for module in &self.modules {
                let imports_target = module
                    .imports
                    .iter()
                    .any(|i| matches!(i.resolution, ImportResolution::Module(t) if t == target));
                if imports_target && seen.insert(module.id) {
                    found.push(module.id);
                    queue.push_back(module.id);
                }
            }
        }
        found
    }
}

/// The name of the `n`th REPL entry: its module, its file, and the
/// function that holds its statements.
pub(super) fn cell_name(n: usize) -> String {
    format!("<repl:{n}>")
}

/// The `import` declarations of `program`: the module name written and
/// the declaration's span.
fn import_decls(program: &ast::Program) -> impl Iterator<Item = (Symbol, Span)> + '_ {
    program.decls.iter().filter_map(|decl| match decl {
        Decl::Import(
            ast::ImportTarget::Module(m)
            | ast::ImportTarget::Items(m, _)
            | ast::ImportTarget::Alias(m, ..),
            span,
        ) => Some((*m, *span)),
        _ => None,
    })
}

/// The file an `import` of a user module names.
struct ImportedFile {
    path: PathBuf,
    /// The package of the imported module.
    package: PackageId,
    package_name: Symbol,
    /// The module's name in that package.
    module: Symbol,
}

/// Resolve `import name`, written in a module of `package`, in this
/// order: a builtin module (handled by the caller); a key of the
/// package's own `[dependencies]`, which names that package's
/// `src/lib.silt`; a module `src/<name>.silt` of the package. A
/// dependency of another package cannot be imported without declaring
/// it, and `import lib` in a dependency is its own library.
fn resolve_import(
    packages: &Packages,
    package: PackageId,
    name: Symbol,
    span: Span,
) -> Result<ImportedFile, Diagnostic> {
    let module_name = resolve(name);
    if let Some(dep) = packages.dependency(package, name) {
        let dep = packages.package(dep);
        let src = dep.src.clone().unwrap_or_default();
        let lib = src.join("lib.silt");
        if !lib.exists() {
            return Err(Diagnostic::error(
                Code::ModuleNotFound,
                span,
                format!(
                    "package '{module_name}' has no library entry point — \
                     expected `src/lib.silt` in the dep at {}",
                    crate::git::escape_for_display(&src.display().to_string())
                ),
            ));
        }
        return Ok(ImportedFile {
            path: lib,
            package: dep.id,
            package_name: dep.name,
            module: intern("lib"),
        });
    }
    let own = packages.package(package);
    let Some(src) = &own.src else {
        return Err(Diagnostic::error(
            Code::ModuleNotFound,
            span,
            format!("cannot import module '{module_name}': no project root set"),
        ));
    };
    Ok(ImportedFile {
        path: src.join(format!("{module_name}.silt")),
        package,
        package_name: own.name,
        module: name,
    })
}

/// The help for an import of `name` that names no file in `package`,
/// when another package of the program declares a dependency by that
/// name: a dependency's dependencies are its own.
/// `n` and `word`, `word` made plural unless `n` is one.
fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

/// Whether `name` can be written after `import`: one identifier.
fn is_module_name(name: &str) -> bool {
    let Ok(tokens) = Lexer::new(FileId::default(), name).tokenize() else {
        return false;
    };
    let mut tokens = tokens
        .tokens
        .iter()
        .map(|tok| &tok.kind)
        .filter(|t| !matches!(t, Token::Eof));
    matches!(
        (tokens.next(), tokens.next()),
        (Some(Token::Ident(s)), None) if resolve(*s) == name
    )
}

/// The span of the declaration `decl`.
fn decl_span(decl: &Decl) -> Span {
    match decl {
        Decl::Fn(f) => f.span,
        Decl::Type(t) => t.span,
        Decl::Trait(t) => t.span,
        Decl::TraitImpl(i) => i.span,
        Decl::Import(_, span) | Decl::Let { span, .. } => *span,
    }
}

/// The error for an import of the host module `name` that the module
/// file at `path`, or a dependency of that key whose library it is,
/// would also answer.
fn host_collision(name: Symbol, path: &Path, dependency: bool, span: Span) -> Diagnostic {
    let shown = crate::git::escape_for_display(&module_path_for_display(path));
    let (what, help) = if dependency {
        (
            format!("the dependency `{name}` ({shown})"),
            format!("rename the dependency key `{name}` in silt.toml, or the host module"),
        )
    } else {
        (
            shown.clone(),
            format!("rename `{shown}` or the host module"),
        )
    };
    Diagnostic::error(
        Code::HostModuleCollision,
        span,
        format!("`import {name}` names both the host module `{name}` and {what}"),
    )
    .with_help(help)
}

pub(super) fn undeclared_dependency_help(
    packages: &Packages,
    package: PackageId,
    name: Symbol,
) -> Option<String> {
    let owner = packages
        .packages
        .iter()
        .find(|p| p.id != package && p.deps.iter().any(|(key, _, _)| *key == name))?;
    Some(format!(
        "`{}` is a dependency of the package `{}`, not of this one; declare it \
         under `[dependencies]` in this package's silt.toml to import it",
        resolve(name),
        resolve(owner.name)
    ))
}

/// The key a file is known by in the graph: its canonical path, or the
/// path as given when it does not exist.
pub(super) fn canonical_key(path: &Path) -> PathBuf {
    crate::source::canonical_path(path)
}

/// How a module file is named in a "cannot load module" diagnostic: its
/// path relative to the working directory when it lies under it,
/// unescaped. A module outside the working directory (a dependency under
/// `~/.silt/deps`) keeps its path: stripping some other prefix would lie
/// about where the file is.
fn module_path_for_display(p: &Path) -> String {
    if let Ok(cwd) = std::env::current_dir() {
        if let Ok(rel) = p.strip_prefix(&cwd) {
            return rel.display().to_string();
        }
        // On Windows the two may name one place differently (a short
        // 8.3 name, a verbatim prefix); their canonical forms agree.
        let p_canon = crate::source::canonical_path_lenient(p);
        let cwd_canon = crate::source::canonical_path_lenient(&cwd);
        if let Ok(rel) = p_canon.strip_prefix(&cwd_canon) {
            return rel.display().to_string();
        }
    }
    crate::source::without_verbatim_prefix(p)
        .display()
        .to_string()
}

/// Whether `path` is one no file can have: that of an editor's document
/// that is not a file (see the language server's `uri_to_path`). It is
/// not looked for on disk, and is shown to nobody.
fn is_no_file(path: &Path) -> bool {
    path.to_string_lossy().contains('\0')
}
