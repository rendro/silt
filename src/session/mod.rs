//! The compilation session: one place that reads a program's files,
//! builds its module graph, checks each module once and compiles it.
//!
//! Every front door (`run`, `check`, `test`, `disasm`, the LSP, ...)
//! drives a [`Session`]: it opens an entry file, asks for the analysis
//! of it (the static diagnostics of every module the entry reaches), and
//! compiles it for an [`Entry`]. The session never prints and never
//! exits; the door renders what it returns.
//!
//! ```text
//! open(path) -> FileId                       read once, registered once
//! analyze(file) -> &Analysis                  graph, then each module checked once
//! compile(file, Entry::Main) -> Program       entry point checked by its type
//! ```

mod entry;
mod graph;
mod host;
mod packages;
pub mod testing;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::ast;
use crate::bytecode::Function;
use crate::compiler::{Compiler, ModuleUnit, ProgramUnits};
use crate::diagnostic::Diagnostic;
use crate::intern::{Symbol, intern, resolve};
use crate::source::{FileId, SourceMap, SourceName};
use crate::typechecker::{self, ModuleExports};
use crate::types::Type;
use crate::types::canonical::Resolver;
use crate::value::HostFn;

pub use entry::{
    ENTRY_POINT, TestFn, TestKind, looks_like_library_module, looks_like_test_file, test_functions,
    test_kind,
};
pub use graph::{Import, ImportResolution, Module, ModuleGraph, ModuleId, Ordering};
pub use host::{HostFunction, HostModule};
pub use packages::{LockPolicy, Package, Packages, ProjectSetup};

/// How a session finds and treats the files of a program.
#[derive(Debug, Clone)]
pub struct Config {
    pub project: ProjectSetup,
    pub lock: LockPolicy,
    /// The modules of Rust functions an embedder offers the program.
    pub host: Vec<HostModule>,
}

/// What a program is compiled for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// A program that starts at `main`, which must have the type `() -> a`.
    Main,
    /// The test functions (`test_*`, `skip_test_*`) whose names contain
    /// `filter`, each of the type `() -> a`. The program calls nothing;
    /// the test runner calls each test.
    Tests { filter: Option<String> },
    /// A REPL entry: the declarations, with nothing called.
    Cell,
}

/// The static diagnostics of an entry file and every module it reaches.
#[derive(Debug, Clone)]
pub struct Analysis {
    /// The diagnostics of every module of the entry's graph: package
    /// problems, the entry's parse errors, why an import failed (each
    /// other module's load or parse problems, the imports that resolve to
    /// nothing, the import cycles), the entry's type diagnostics, then
    /// each other module's type errors. Each diagnostic once.
    pub diagnostics: Vec<Diagnostic>,
    /// The modules of the graph, each after the modules it imports; the
    /// entry is last.
    pub modules: Vec<ModuleId>,
    pub entry: ModuleId,
}

impl Analysis {
    /// Whether any diagnostic is an error.
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(Diagnostic::is_error)
    }
}

/// One module after its check.
pub struct ModuleAnalysis {
    /// The declarations, with what the checker filled in (expression
    /// types, synthesized impls).
    pub ast: Arc<ast::Program>,
    /// What the module offers its importers.
    pub exports: ModuleExports,
    /// The inferred type of each top-level value the module binds.
    pub top_level: HashMap<crate::intern::Symbol, Type>,
    /// The module's type errors and warnings.
    pub diagnostics: Vec<Diagnostic>,
    /// The type aliases and associated-type bindings the module sees:
    /// those of the modules it imports, then its own. The compiler
    /// canonicalizes the module's impl targets with them.
    pub resolver: Arc<Resolver>,
}

/// What the VM runs: the compiled functions, the first of which is the
/// script that installs the program's globals (and, for
/// [`Entry::Main`], calls `main`).
pub struct Program {
    pub functions: Vec<Function>,
    pub entry: EntryPoint,
    /// The compiler's warnings.
    pub warnings: Vec<Diagnostic>,
}

/// What a [`Program`] was compiled for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryPoint {
    Main,
    /// The selected tests, in source order.
    Tests(Vec<TestFn>),
    Cell,
}

/// What the imports of `module` name, as the compiler looks them up:
/// by the module name written after `import`, and by each name an
/// import binds (`m` for `import m`, `n` for `import m as n`), each
/// mapped to the unit of the imported module.
fn unit_imports(
    module: &Module,
    index: &HashMap<ModuleId, usize>,
) -> (HashMap<Symbol, usize>, HashMap<Symbol, usize>) {
    let imports: HashMap<Symbol, usize> = module
        .imports
        .iter()
        .filter_map(|import| match import.resolution {
            ImportResolution::Module(target) => Some((import.name, index[&target])),
            _ => None,
        })
        .collect();
    let mut bindings = HashMap::new();
    if let Some(ast) = &module.ast {
        for decl in &ast.decls {
            let (name, bound) = match decl {
                ast::Decl::Import(ast::ImportTarget::Module(name), _) => (name, name),
                ast::Decl::Import(ast::ImportTarget::Alias(name, alias, _), _) => (name, alias),
                _ => continue,
            };
            if let Some(&unit) = imports.get(name) {
                bindings.insert(*bound, unit);
            }
        }
    }
    (imports, bindings)
}

/// A compilation session. See the module documentation.
pub struct Session {
    config: Config,
    sources: SourceMap,
    graph: ModuleGraph,
    /// The packages, resolved on first use. Until then `None`.
    packages: Option<Result<Packages, Vec<Diagnostic>>>,
    /// The packages the graph is built on: those resolved, or none when
    /// resolving them failed.
    fallback_packages: Packages,
    /// Editor buffers, by canonical path: read instead of the disk.
    overlays: HashMap<PathBuf, String>,
    /// The module of each file the session was given.
    file_modules: HashMap<FileId, ModuleId>,
    /// The paths the entry files were given by, as the user named them.
    entry_paths: HashMap<ModuleId, String>,
    /// Each module checked, until something it depends on changes.
    analyses: HashMap<ModuleId, ModuleAnalysis>,
    /// The analysis of each entry asked for, until anything changes.
    results: HashMap<ModuleId, Analysis>,
    /// The number of REPL cells added.
    cells: usize,
    /// The module files of the project's packages named like builtin
    /// modules, found when the packages are resolved.
    module_name_problems: Vec<Diagnostic>,
}

impl Session {
    pub fn new(config: Config) -> Session {
        let mut sources = SourceMap::new();
        let mut graph = ModuleGraph::default();
        for (index, host) in config.host.iter().enumerate() {
            graph.enter_host(index, host, &mut sources);
        }
        Session {
            config,
            sources,
            graph,
            packages: None,
            fallback_packages: Packages::unnamed(None),
            overlays: HashMap::new(),
            file_modules: HashMap::new(),
            entry_paths: HashMap::new(),
            analyses: HashMap::new(),
            results: HashMap::new(),
            cells: 0,
            module_name_problems: Vec::new(),
        }
    }

    /// The text of every file the session has read.
    pub fn sources(&self) -> &SourceMap {
        &self.sources
    }

    /// The module graph.
    pub fn graph(&self) -> &ModuleGraph {
        &self.graph
    }

    /// The packages of the project: its manifests and `silt.lock` are
    /// read on the first call (and registered in the source map, where
    /// their diagnostics point), and the lock is rewritten when the
    /// policy allows it and it no longer pins the graph. When that
    /// fails, the session goes on as if there were no project, and every
    /// analysis reports the package diagnostics.
    pub fn packages(&mut self) -> Result<&Packages, &[Diagnostic]> {
        if self.packages.is_none() {
            let resolved = packages::project_packages(
                &self.config.project,
                self.config.lock,
                &mut self.sources,
            );
            if let Ok(packages) = &resolved {
                self.module_name_problems =
                    packages::modules_named_like_builtins(packages, &mut self.sources);
            }
            self.packages = Some(resolved);
        }
        match self.packages.as_ref().expect("resolved above") {
            Ok(packages) => Ok(packages),
            Err(diagnostics) => Err(diagnostics),
        }
    }

    /// The text of every file the session has read, once it is done.
    pub fn into_sources(self) -> SourceMap {
        self.sources
    }

    /// The packages the graph is built on.
    fn graph_packages(&mut self) -> Packages {
        match self.packages() {
            Ok(packages) => packages.clone(),
            Err(_) => self.fallback_packages.clone(),
        }
    }

    /// Read the file at `path` (an editor buffer set for it, if any) and
    /// enter it as an entry. `path` is kept as given: diagnostics name the
    /// file so.
    pub fn open(&mut self, path: &Path) -> Result<FileId, std::io::Error> {
        let key = graph::canonical_key(path);
        let (name, text) = match self.overlays.get(&key) {
            Some(text) => (SourceName::Overlay(path.to_path_buf()), text.clone()),
            None => (
                SourceName::Path(path.to_path_buf()),
                std::fs::read_to_string(path)?,
            ),
        };
        Ok(self.enter_text(path, name, &text))
    }

    /// Enter the file at `path` with the text `text`, already read by the
    /// caller.
    pub fn open_text(&mut self, path: &Path, text: &str) -> FileId {
        self.enter_text(path, SourceName::Path(path.to_path_buf()), text)
    }

    /// Give the file at `path` an editor's text. The file's module and
    /// every module that imports it, directly or not, are checked again
    /// on the next [`Session::analyze`]; nothing else is.
    pub fn set_overlay(&mut self, path: &Path, text: String) -> FileId {
        let key = graph::canonical_key(path);
        self.overlays.insert(key, text.clone());
        self.enter_text(path, SourceName::Overlay(path.to_path_buf()), &text)
    }

    /// Add a REPL entry as a file of its own, `<repl:n>`.
    pub fn add_cell(&mut self, text: String) -> FileId {
        self.cells += 1;
        let n = self.cells;
        self.enter_text(
            &PathBuf::from(format!("<repl:{n}>")),
            SourceName::Repl(n),
            &text,
        )
    }

    /// Register `text` as the file at `path` and parse it, as an entry
    /// module or as the new text of the module the graph has for it.
    fn enter_text(&mut self, path: &Path, name: SourceName, text: &str) -> FileId {
        let existing = self.graph.module_at(path);
        let id = match existing {
            Some(id) => {
                self.graph.reparse(id, name, text, &mut self.sources);
                self.invalidate(id);
                id
            }
            None => {
                let packages = self.graph_packages();
                let root = packages.package(packages.root);
                let package = (root.id, root.name);
                self.graph
                    .enter(path, name, text, package, &mut self.sources)
            }
        };
        self.entry_paths
            .entry(id)
            .or_insert_with(|| path.display().to_string());
        let file = self.graph.module(id).file.expect("the text was registered");
        self.file_modules.insert(file, id);
        file
    }

    /// Forget the check of module `id` and of every module that imports
    /// it, and every analysis.
    fn invalidate(&mut self, id: ModuleId) {
        self.analyses.remove(&id);
        for dependent in self.graph.reverse_dependents(id) {
            self.analyses.remove(&dependent);
        }
        self.results.clear();
    }

    /// The module of the file `file`.
    pub fn module_of(&self, file: FileId) -> ModuleId {
        self.file_modules[&file]
    }

    /// The check of module `id`, once [`Session::analyze`] has made it.
    pub fn module_analysis(&self, id: ModuleId) -> Option<&ModuleAnalysis> {
        self.analyses.get(&id)
    }

    /// The analysis of the entry file `entry`: its module graph is
    /// completed, and each module that is not checked yet is checked,
    /// in the order of the graph. Asked again with nothing changed, it is
    /// the same analysis.
    pub fn analyze(&mut self, entry: FileId) -> &Analysis {
        let id = self.module_of(entry);
        if !self.results.contains_key(&id) {
            let analysis = self.analyze_module(id);
            self.results.insert(id, analysis);
        }
        &self.results[&id]
    }

    fn analyze_module(&mut self, entry: ModuleId) -> Analysis {
        let packages = self.graph_packages();
        self.graph
            .load(entry, &packages, &self.overlays, &mut self.sources);
        let ordering = self.graph.order(entry);
        // The host modules are checked whether or not the program imports
        // them: what is wrong with one is the embedder's to hear.
        let hosts: Vec<ModuleId> = self
            .graph
            .modules()
            .iter()
            .filter(|m| m.host.is_some() && !m.failed())
            .map(|m| m.id)
            .collect();
        for id in hosts {
            if !self.analyses.contains_key(&id) {
                let analysis = self.check(id, &ordering);
                self.analyses.insert(id, analysis);
            }
        }
        for &id in &ordering.modules {
            let module = self.graph.module(id);
            if self.analyses.contains_key(&id) || (id != entry && module.failed()) {
                continue;
            }
            let analysis = self.check(id, &ordering);
            self.analyses.insert(id, analysis);
        }
        Analysis {
            diagnostics: self.collect_diagnostics(entry, &ordering),
            modules: ordering.modules,
            entry,
        }
    }

    /// Check module `id`, whose imports are checked already, except those
    /// that failed or that close a cycle: those are poisoned.
    fn check(&mut self, id: ModuleId, ordering: &Ordering) -> ModuleAnalysis {
        let module = self.graph.module(id);
        let mut ast = module
            .ast
            .clone()
            .unwrap_or(ast::Program { decls: Vec::new() });
        if module.host.is_some() {
            let mut resolver = Resolver::new();
            let check = typechecker::check_host_module(&mut ast, &mut resolver);
            return ModuleAnalysis {
                ast: Arc::new(ast),
                exports: check.exports,
                top_level: check.top_level,
                diagnostics: check.diagnostics,
                resolver: Arc::new(resolver),
            };
        }
        let mut imports = HashMap::new();
        let mut poisoned = HashSet::new();
        let mut resolver = Resolver::new();
        for import in &module.imports {
            match &import.resolution {
                ImportResolution::Builtin => {}
                ImportResolution::Module(target) => match self.analyses.get(target) {
                    Some(analysis)
                        if !self.graph.module(*target).failed()
                            && !ordering.back_edges.contains(&(id, *target)) =>
                    {
                        imports.insert(import.name, analysis.exports.clone());
                        resolver.absorb(&analysis.resolver);
                    }
                    _ => {
                        poisoned.insert(import.name);
                    }
                },
                ImportResolution::Unresolved(_) => {
                    poisoned.insert(import.name);
                }
            }
        }
        let check = typechecker::check_module(
            &mut ast,
            Some(module.package_name),
            imports,
            poisoned,
            &mut resolver,
        );
        ModuleAnalysis {
            ast: Arc::new(ast),
            exports: check.exports,
            top_level: check.top_level,
            diagnostics: check.diagnostics,
            resolver: Arc::new(resolver),
        }
    }

    /// The diagnostics of the analysis of `entry`, in the order of
    /// [`Analysis::diagnostics`]. Another module's warnings are its own:
    /// they are shown when it is checked as an entry.
    fn collect_diagnostics(&self, entry: ModuleId, ordering: &Ordering) -> Vec<Diagnostic> {
        let mut out: Vec<Diagnostic> = Vec::new();
        let push = |d: &Diagnostic, out: &mut Vec<Diagnostic>| {
            if !out.contains(d) {
                out.push(d.clone());
            }
        };
        if let Some(Err(diagnostics)) = &self.packages {
            for d in diagnostics {
                push(d, &mut out);
            }
        }
        for d in &self.module_name_problems {
            push(d, &mut out);
        }
        // What is wrong with a host module is reported whether or not
        // the program imports it.
        for module in self.graph.modules().iter().filter(|m| m.host.is_some()) {
            for d in &module.problems {
                push(d, &mut out);
            }
            if let Some(analysis) = self.analyses.get(&module.id) {
                for d in analysis.diagnostics.iter().filter(|d| d.is_error()) {
                    push(d, &mut out);
                }
            }
        }
        // The entry's own parse errors, then why an import failed (a
        // module that cannot be read or parsed, a name that resolves to
        // nothing, a cycle), then the type diagnostics.
        for d in &self.graph.module(entry).problems {
            push(d, &mut out);
        }
        for &id in ordering.modules.iter().filter(|&&id| id != entry) {
            for d in &self.graph.module(id).problems {
                push(d, &mut out);
            }
        }
        for &id in &ordering.modules {
            for import in &self.graph.module(id).imports {
                if let ImportResolution::Unresolved(d) = &import.resolution {
                    push(d, &mut out);
                }
            }
        }
        for d in &ordering.cycles {
            push(d, &mut out);
        }
        if let Some(analysis) = self.analyses.get(&entry) {
            for d in &analysis.diagnostics {
                push(d, &mut out);
            }
        }
        for &id in ordering.modules.iter().filter(|&&id| id != entry) {
            if let Some(analysis) = self.analyses.get(&id) {
                for d in analysis.diagnostics.iter().filter(|d| d.is_error()) {
                    push(d, &mut out);
                }
            }
        }
        out
    }

    /// Compile the entry file `entry` for `target`. An analysis with an
    /// error is not compiled: the error is the analysis's, and the result
    /// is `Err` with nothing more. Otherwise the entry point is checked
    /// by its inferred type, then the modules are compiled; the `Err`
    /// holds what that found.
    pub fn compile(&mut self, entry: FileId, target: Entry) -> Result<Program, Vec<Diagnostic>> {
        if self.analyze(entry).has_errors() {
            return Err(Vec::new());
        }
        let id = self.module_of(entry);
        let modules = self.results[&id].modules.clone();
        let analysis = &self.analyses[&id];
        // The entry point is checked here and reported after the compile
        // errors: a program the compiler rejects may lack a `main` only
        // because of that (`let (main, y) = ...`).
        let main = intern(ENTRY_POINT);
        let mut entry_errors = Vec::new();
        let entry_point = match &target {
            Entry::Main => {
                entry_errors.extend(match analysis.top_level.get(&main) {
                    None => Some(entry::missing_main(
                        &analysis.ast,
                        self.graph
                            .module(id)
                            .file
                            .expect("an analysed module has a file"),
                        self.entry_paths.get(&id).map_or("", String::as_str),
                    )),
                    Some(ty) => entry::check_main(&analysis.ast, ty),
                });
                EntryPoint::Main
            }
            Entry::Tests { filter } => {
                // A file run for its tests that is a program too (it binds
                // `main` and is neither a library module nor a test file)
                // must have a `main` that can start.
                if let Some(ty) = analysis.top_level.get(&main)
                    && !looks_like_library_module(&analysis.ast)
                    && !looks_like_test_file(&analysis.ast)
                {
                    entry_errors.extend(entry::check_main(&analysis.ast, ty));
                }
                let (tests, errors) =
                    entry::select_tests(&analysis.ast, &analysis.top_level, filter.as_deref());
                entry_errors.extend(errors);
                EntryPoint::Tests(tests)
            }
            Entry::Cell => EntryPoint::Cell,
        };

        // The units, numbered in graph order, with each import mapped to
        // the unit of the module it names.
        let index: HashMap<ModuleId, usize> =
            modules.iter().enumerate().map(|(i, id)| (*id, i)).collect();
        // Each module's globals take the first name it is imported by; a
        // module imported by a name another module took already (an app's
        // `util` and a dependency's own `util`) is told apart by its
        // package.
        let mut globals: HashMap<ModuleId, String> = HashMap::new();
        let mut taken: HashSet<String> = HashSet::new();
        for m in &modules {
            for import in &self.graph.module(*m).imports {
                let ImportResolution::Module(target) = import.resolution else {
                    continue;
                };
                if globals.contains_key(&target) {
                    continue;
                }
                let mut global = resolve(import.name);
                if !taken.insert(global.clone()) {
                    let module = self.graph.module(target);
                    global = format!("{}::{}", module.package_name, resolve(module.name));
                    taken.insert(global.clone());
                }
                globals.insert(target, global);
            }
        }
        let units = ProgramUnits {
            modules: modules
                .iter()
                .map(|m| {
                    let module = self.graph.module(*m);
                    let (imports, bindings) = unit_imports(module, &index);
                    ModuleUnit {
                        program: self.analyses[m].ast.clone(),
                        resolver: self.analyses[m].resolver.clone(),
                        name: resolve(module.name),
                        global: globals
                            .get(m)
                            .cloned()
                            .unwrap_or_else(|| resolve(module.name)),
                        imports,
                        bindings,
                        host: module
                            .host
                            .map_or_else(HashMap::new, |host| self.host_functions(module, host)),
                    }
                })
                .collect(),
            entry: index[&id],
        };
        let program = self.analyses[&id].ast.clone();
        let mut compiler = Compiler::for_program(units);
        let compiled = match target {
            Entry::Main => compiler.compile_program(&program),
            Entry::Tests { .. } | Entry::Cell => compiler.compile_declarations(&program),
        };
        match compiled {
            Ok(_) if !entry_errors.is_empty() => Err(entry_errors),
            Ok(functions) => Ok(Program {
                functions,
                entry: entry_point,
                warnings: compiler.warnings().to_vec(),
            }),
            Err(e) => {
                let mut errors = vec![e];
                errors.extend(entry_errors);
                Err(errors)
            }
        }
    }

    /// The functions of the host module `module`, the `host`th of the
    /// configuration, by name, as the program installs them.
    fn host_functions(&self, module: &Module, host: usize) -> HashMap<Symbol, Arc<HostFn>> {
        let decls = module.ast.as_ref().map_or(&[][..], |ast| &ast.decls[..]);
        decls
            .iter()
            .zip(&self.config.host[host].fns)
            .filter_map(|(decl, f)| match decl {
                ast::Decl::Fn(decl) => Some((
                    decl.name,
                    Arc::new(HostFn {
                        name: format!("{}.{}", module.name, decl.name),
                        call: f.call.clone(),
                    }),
                )),
                _ => None,
            })
            .collect()
    }

    /// The files of the graph of `entry`, for a watcher: each module's
    /// file, as named in diagnostics. Host modules have none.
    pub fn files(&self, entry: FileId) -> Vec<PathBuf> {
        let id = self.module_of(entry);
        match self.results.get(&id) {
            Some(analysis) => analysis
                .modules
                .iter()
                .map(|m| self.graph.module(*m))
                .filter(|m| m.host.is_none())
                .map(|m| m.path.clone())
                .collect(),
            None => vec![self.graph.module(id).path.clone()],
        }
    }
}
