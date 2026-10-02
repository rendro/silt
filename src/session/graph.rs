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
use crate::lexer::Lexer;
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
    /// The import that first brought the module into the graph: the
    /// importing module, the name written and the span of the `import`.
    pub first_import: Option<(ModuleId, Symbol, Span)>,
    /// The module's imports, in source order.
    pub imports: Vec<Import>,
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
}

/// What an `import` names.
#[derive(Debug, Clone)]
pub enum ImportResolution {
    /// A builtin module (`list`, `io`, ...).
    Builtin,
    /// A module of the graph.
    Module(ModuleId),
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
                    first_import: None,
                    imports: Vec::new(),
                })
            }
        };
        self.parse(id, name, text, sources);
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
        let file = sources.add(name, text.into());
        let module = &mut self.modules[id.index()];
        module.file = Some(file);
        module.imports.clear();
        let (ast, problems) = parse_text(file, text);
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
            let resolution = if module::is_builtin_module(&resolve(name)) {
                ImportResolution::Builtin
            } else {
                match resolve_import(packages, package, name, span) {
                    Ok(target) => {
                        let help = undeclared_dependency_help(packages, package, name);
                        ImportResolution::Module(self.module_for(
                            target,
                            (id, name, span),
                            help,
                            overlays,
                            sources,
                        ))
                    }
                    Err(d) => ImportResolution::Unresolved(d),
                }
            };
            imports.push(Import {
                name,
                span,
                resolution,
            });
        }
        self.modules[id.index()].imports = imports;
    }

    /// The module of the file `target` names, read and parsed if the
    /// graph does not have it yet. `import` is the import that reaches
    /// it: a file that cannot be read is reported there.
    fn module_for(
        &mut self,
        target: ImportedFile,
        import: (ModuleId, Symbol, Span),
        help: Option<String>,
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
            first_import: Some(import),
            imports: Vec::new(),
        });
        let (_, name, span) = import;
        let text = match overlays.get(&canonical_key(&target.path)) {
            Some(text) => Ok(text.clone()),
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
                let mut d = module::module_load_error(
                    &resolve(name),
                    &target.path,
                    &crate::git::escape_for_display(&module_path_for_display(&target.path)),
                    &e,
                    span,
                );
                if let Some(help) = help {
                    d.help.insert(0, help);
                }
                self.modules[id.index()].problems.push(d);
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
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// `p` canonicalized, also when `p` itself does not exist (a module that
/// was looked for and not found): the nearest existing ancestor is
/// canonicalized and the rest of the path appended. On Windows this also
/// resolves short (8.3) directory names, so a path under a short-named
/// working directory still compares with its long form.
fn canonicalize_existing_prefix(p: &Path) -> Option<PathBuf> {
    let mut rest = Vec::new();
    let mut current = p;
    loop {
        if let Ok(canon) = std::fs::canonicalize(current) {
            let mut out = canon;
            for part in rest.iter().rev() {
                out.push(part);
            }
            return Some(out);
        }
        rest.push(current.file_name()?.to_os_string());
        current = current.parent()?;
    }
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
        // On Windows, `p` may be in extended-length form (`\\?\C:\...`)
        // while `cwd` is not; canonicalizing both makes them comparable.
        if let (Some(p_canon), Ok(cwd_canon)) =
            (canonicalize_existing_prefix(p), std::fs::canonicalize(&cwd))
            && let Ok(rel) = p_canon.strip_prefix(&cwd_canon)
        {
            return rel.display().to_string();
        }
    }
    let s = p.display().to_string();
    #[cfg(windows)]
    {
        if let Some(stripped) = s.strip_prefix(r"\\?\") {
            return stripped.to_string();
        }
    }
    s
}
