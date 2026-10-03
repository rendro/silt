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

mod cells;
mod entry;
mod graph;
mod host;
mod packages;
pub mod testing;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::ast;
use crate::bytecode::{Function, Globals};
use crate::compiler::{Compiler, EarlierCells, ModuleUnit, ProgramUnits};
use crate::defs::DefTable;
use crate::diagnostic::{Code, Diagnostic};
use crate::intern::{Symbol, intern, resolve};
use crate::source::{FileId, SourceMap, SourceName};
use crate::typechecker::names::{self, Imported, ModuleKind, ModuleScope};
use crate::typechecker::{self, ModuleContext, Tables};
use crate::types::Type;
use crate::value::{HostFn, HostShape};

pub use entry::{
    ENTRY_POINT, TestFn, TestKind, looks_like_library_module, looks_like_test_file, selected_tests,
    test_functions, test_kind,
};
pub use graph::{Import, ImportResolution, Module, ModuleGraph, ModuleId, Ordering, parse_text};
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
    /// A REPL cell, added by [`Session::add_cell`]: its declarations are
    /// installed, and a cell of statements runs them; the script returns
    /// their value. Any other file: its declarations, with nothing
    /// called.
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
    /// The module's top-level names, and what it offers its importers.
    pub scope: ModuleScope,
    /// The inferred type of each top-level value the module binds.
    pub top_level: HashMap<crate::intern::Symbol, Type>,
    /// The module's type errors and warnings.
    pub diagnostics: Vec<Diagnostic>,
}

/// What the VM runs: the compiled functions, the first of which is the
/// script that installs the program's globals (and, for
/// [`Entry::Main`], calls `main`).
pub struct Program {
    pub functions: Vec<Function>,
    /// The types its values are of, which the VM is given
    /// ([`crate::vm::Vm::run_program`]).
    pub types: crate::typeinfo::TypeTable,
    /// Its global slots, which the VM is given too.
    pub globals: Arc<Globals>,
    pub entry: EntryPoint,
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
/// by the module name written after `import`, each mapped to the unit
/// of the imported module.
fn unit_imports(module: &Module, index: &HashMap<ModuleId, usize>) -> HashMap<Symbol, usize> {
    module
        .imports
        .iter()
        .filter_map(|import| match import.resolution {
            ImportResolution::Module(target) => Some((import.name, index[&target])),
            _ => None,
        })
        .collect()
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
    /// The definitions of every module the session has checked, and of
    /// the builtins.
    defs: Arc<DefTable>,
    /// What the checks of the modules share: the type variables, the
    /// types, traits and impls, and the scheme of every definition of a
    /// checked module.
    tables: Tables,
    /// The analysis of each entry asked for, until anything changes.
    results: HashMap<ModuleId, Analysis>,
    /// The REPL cells added.
    cells: cells::Cells,
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
            defs: Arc::new(names::new_def_table()),
            tables: Tables::for_session(),
            results: HashMap::new(),
            cells: cells::Cells::default(),
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

    /// Give the file at `path` an editor's text. The file's module and
    /// every module that imports it, directly or not, are checked again
    /// on the next [`Session::analyze`]; nothing else is.
    pub fn set_overlay(&mut self, path: &Path, text: String) -> FileId {
        let key = graph::canonical_key(path);
        self.overlays.insert(key, text.clone());
        self.enter_text(path, SourceName::Overlay(path.to_path_buf()), &text)
    }

    /// Add a REPL entry as a file of its own, `<repl:n>`: declarations,
    /// or statements whose value [`Entry::Cell`] returns. It sees what
    /// the committed cells bind (see [`Session::commit_cell`]).
    pub fn add_cell(&mut self, text: String) -> FileId {
        // What an earlier cell that was not committed declared is gone.
        let dropped: Vec<ModuleId> = self
            .cells
            .info
            .keys()
            .filter(|cell| !self.cells.is_committed(**cell))
            .copied()
            .collect();
        for cell in dropped {
            self.tables.forget(cell);
        }
        self.cells.count += 1;
        let n = self.cells.count;
        let file = self.enter_text(
            &PathBuf::from(graph::cell_name(n)),
            SourceName::Repl(n),
            &text,
        );
        let id = self.module_of(file);
        if let Some(program) = self.graph.ast_mut(id) {
            self.cells.prepare(id, n, file, program);
        }
        file
    }

    /// Commit the REPL cell `file`, compiled and run: every cell added
    /// after it sees what it binds, and the modules it imported are
    /// installed. A cell that is not committed leaves the session as it
    /// was.
    pub fn commit_cell(&mut self, file: FileId) {
        let id = self.module_of(file);
        let (Some(analysis), Some(result)) = (self.analyses.get(&id), self.results.get(&id)) else {
            return;
        };
        self.cells.commit(id, &analysis.ast, &result.modules);
    }

    /// The problems of the project the session is in: its manifests and
    /// lockfile, and its module files named like builtin modules. Every
    /// analysis reports them, but a REPL cell's: the REPL shows them
    /// once, when it starts.
    pub fn project_problems(&mut self) -> Vec<Diagnostic> {
        let mut problems = match self.packages() {
            Ok(_) => Vec::new(),
            Err(diagnostics) => diagnostics.to_vec(),
        };
        problems.extend(self.module_name_problems.iter().cloned());
        problems
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
        self.tables.forget(id);
        for dependent in self.graph.reverse_dependents(id) {
            self.analyses.remove(&dependent);
            self.tables.forget(dependent);
        }
        self.results.clear();
    }

    /// The module of the file `file`.
    pub fn module_of(&self, file: FileId) -> ModuleId {
        self.file_modules[&file]
    }

    /// The definitions the session's modules and the builtins declare.
    pub fn defs(&self) -> &DefTable {
        &self.defs
    }

    /// What the checks of the session's modules found: every type,
    /// trait, impl and method, and the scheme of each definition.
    pub fn tables(&self) -> &Tables {
        &self.tables
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
        let mut imported: HashMap<Symbol, Imported<'_>> = HashMap::new();
        let mut bugs = Vec::new();
        for import in &module.imports {
            let resolution = match &import.resolution {
                ImportResolution::Builtin => ModuleId::builtin(&resolve(import.name))
                    .map_or(Imported::Poisoned, Imported::Builtin),
                ImportResolution::Cell(cell) => match self.analyses.get(cell) {
                    Some(analysis) => Imported::Cell(*cell, &analysis.scope),
                    // A committed cell is checked; this is a bug of the
                    // session, reported rather than a panic of the REPL.
                    None => {
                        bugs.push(Diagnostic::error(
                            Code::CompilerBug,
                            import.span,
                            format!("silt bug: the REPL entry {} is not checked", import.name),
                        ));
                        Imported::Poisoned
                    }
                },
                ImportResolution::Module(target) => match self.analyses.get(target) {
                    Some(analysis)
                        if !self.graph.module(*target).failed()
                            && !ordering.back_edges.contains(&(id, *target)) =>
                    {
                        Imported::Module(*target, &analysis.scope)
                    }
                    _ => Imported::Poisoned,
                },
                ImportResolution::Unresolved(_) => Imported::Poisoned,
            };
            imported.insert(import.name, resolution);
        }
        let kind = if module.host.is_some() {
            ModuleKind::Host
        } else if self.cells.info.contains_key(&id) {
            ModuleKind::Cell
        } else {
            ModuleKind::File
        };
        // What a cell imports from the earlier cells, by name.
        let mut earlier = Vec::new();
        for decl in &ast.decls {
            if let ast::Decl::Import(ast::ImportTarget::Items(m, items), _) = decl
                && let Some(Imported::Cell(_, scope)) = imported.get(m)
            {
                for (item, _) in items {
                    if let Some(names::Binding::Def(def)) = scope.exports.values.get(item) {
                        earlier.push((*item, *def));
                    }
                }
            }
        }
        let resolution =
            names::resolve_module(&mut ast, id, kind, &imported, Arc::make_mut(&mut self.defs));
        let check = typechecker::check_module(
            &mut ast,
            ModuleContext {
                module: id,
                module_name: module.name,
                kind,
                package: (kind != ModuleKind::Host).then_some(module.package_name),
                scope: &resolution.scope,
                earlier: &earlier,
                defs: self.defs.clone(),
                tables: &mut self.tables,
            },
        );
        ModuleAnalysis {
            ast: Arc::new(ast),
            scope: resolution.scope,
            top_level: check.top_level,
            diagnostics: bugs
                .into_iter()
                .chain(resolution.diagnostics)
                .chain(check.diagnostics)
                .collect(),
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
        if !self.cells.info.contains_key(&entry) {
            if let Some(Err(diagnostics)) = &self.packages {
                for d in diagnostics {
                    push(d, &mut out);
                }
            }
            for d in &self.module_name_problems {
                push(d, &mut out);
            }
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
            // A module whose file could not be read: each import of it.
            for &importer in &ordering.modules {
                for import in &self.graph.module(importer).imports {
                    if matches!(import.resolution, ImportResolution::Module(t) if t == id)
                        && let Some(d) = &import.problem
                    {
                        push(d, &mut out);
                    }
                }
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
    /// error is not compiled: the `Err` holds the analysis's errors. (A
    /// door that shows the analysis compiles only when it has none.)
    /// Otherwise the entry point is checked by its inferred type, then the
    /// modules are compiled; the `Err` holds what that found.
    pub fn compile(&mut self, entry: FileId, target: Entry) -> Result<Program, Vec<Diagnostic>> {
        let analysis = self.analyze(entry);
        if analysis.has_errors() {
            return Err(analysis
                .diagnostics
                .iter()
                .filter(|d| d.is_error())
                .cloned()
                .collect());
        }
        let id = self.module_of(entry);
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

        let units = self.program_units(id, matches!(target, Entry::Cell));
        let program = self.analyses[&id].ast.clone();
        let mut compiler = match Compiler::for_program(units) {
            Ok(compiler) => compiler,
            Err(e) => {
                let mut errors = vec![e];
                errors.extend(entry_errors);
                return Err(errors);
            }
        };
        let compiled = match target {
            Entry::Main => compiler.compile_program(&program, self.top_level_def(id, ENTRY_POINT)),
            Entry::Tests { .. } => compiler.compile_declarations(&program),
            Entry::Cell if !self.cells.info.contains_key(&id) => {
                compiler.compile_declarations(&program)
            }
            Entry::Cell => {
                let cell = &self.cells.info[&id];
                let program = cell.compiled(&program);
                let wrapper = cell.wrapper();
                let statements = program
                    .decls
                    .iter()
                    .any(|decl| matches!(decl, ast::Decl::Fn(f) if resolve(f.name) == wrapper));
                if statements {
                    compiler.compile_program(&program, self.top_level_def(id, &wrapper))
                } else {
                    compiler.compile_declarations(&program)
                }
            }
        };
        match compiled {
            Ok(_) if !entry_errors.is_empty() => Err(entry_errors),
            Ok(functions) => {
                let globals = compiler.globals().clone();
                if matches!(target, Entry::Cell) && self.cells.info.contains_key(&id) {
                    self.cells.globals = globals.clone();
                }
                let entry = match entry_point {
                    EntryPoint::Tests(mut tests) => {
                        for test in &mut tests {
                            test.slot = self.global_slot(id, &test.name, &globals);
                        }
                        EntryPoint::Tests(tests)
                    }
                    other => other,
                };
                Ok(Program {
                    functions,
                    types: compiler.types(),
                    globals: Arc::new(globals),
                    entry,
                })
            }
            Err(e) => {
                let mut errors = vec![e];
                errors.extend(entry_errors);
                Err(errors)
            }
        }
    }

    /// The modules of the analysed program of module `id`, as the
    /// compiler takes them: numbered in graph order, with each import
    /// mapped to the unit of the module it names. For a REPL cell
    /// (`cell`), with what the earlier cells installed.
    pub(crate) fn program_units(&self, id: ModuleId, cell: bool) -> ProgramUnits {
        let modules = &self.results[&id].modules;
        let index: HashMap<ModuleId, usize> =
            modules.iter().enumerate().map(|(i, id)| (*id, i)).collect();
        let entry_package = self.graph.module(id).package;
        let earlier = match cell && self.cells.info.contains_key(&id) {
            true => self.cells.earlier(&index),
            false => EarlierCells::default(),
        };
        ProgramUnits {
            defs: self.defs.clone(),
            resolver: Arc::new(self.tables.resolver().clone()),
            earlier,
            modules: modules
                .iter()
                .map(|m| {
                    let module = self.graph.module(*m);
                    ModuleUnit {
                        id: *m,
                        program: self.analyses[m].ast.clone(),
                        name: resolve(module.name),
                        qualifier: match (module.package == entry_package, resolve(module.name)) {
                            (true, name) => name,
                            (false, name) if name == "lib" => resolve(module.package_name),
                            (false, name) => format!("{}.{name}", module.package_name),
                        },
                        imports: unit_imports(module, &index),
                        host: module
                            .host
                            .map_or_else(HashMap::new, |host| self.host_functions(module, host)),
                    }
                })
                .collect(),
            entry: index[&id],
        }
    }

    /// The definition the top-level name `name` of module `id` binds,
    /// its own or an imported one.
    fn top_level_def(&self, id: ModuleId, name: &str) -> Option<crate::defs::DefId> {
        match self.analyses.get(&id)?.scope.values.get(&intern(name))? {
            names::Binding::Def(def) => Some(*def),
            _ => None,
        }
    }

    /// The global slot of the top-level function `name` of module `id`.
    fn global_slot(&self, id: ModuleId, name: &str, globals: &Globals) -> u16 {
        let name = intern(name);
        self.defs
            .of_module(id)
            .iter()
            .find(|def| self.defs.get(**def).name == name)
            .and_then(|def| globals.def(*def))
            .expect("a selected test is a top-level function of the entry")
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
                        returns: decl
                            .return_type
                            .as_ref()
                            .map_or(HostShape::Any, host::shape),
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
