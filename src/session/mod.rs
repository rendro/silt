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

mod analysis;
mod cells;
mod entry;
mod graph;
mod host;
mod packages;
#[cfg(any(test, feature = "test-hooks"))]
pub(crate) mod testing;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::ast;
use crate::bytecode::Globals;
use crate::compiler::{Compiler, EarlierCells, ModuleUnit, ProgramUnits};
use crate::defs::{DefTable, ModuleId};
use crate::diagnostic::Code;
use crate::intern::{Symbol, intern, resolve};
use crate::program::ENTRY_POINT;
use crate::source::SourceName;
use crate::typechecker::names::{self, Imported, ModuleKind};
use crate::typechecker::{self, ModuleContext, Tables};
#[cfg(feature = "repl")]
use crate::types::Type;
use crate::value::HostFn;

// What a session hands out: a file's id, a place, the files read, and
// what an analysis finds.
pub use crate::diagnostic::{Diagnostic, Severity};
pub use crate::program::{EntryPoint, Program, TestFn, TestKind};
pub use crate::source::{FileId, SourceMap, Span};
pub use crate::value::HostCall;
pub(crate) use analysis::ModuleAnalysis;
// For `silt test` (crate::cli, which a WebAssembly build has not).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use entry::selected_tests;
#[cfg(feature = "lsp")]
pub(crate) use graph::parse_text;
pub(crate) use graph::{ImportResolution, Module, ModuleGraph, Ordering};
pub use host::HostModule;
pub(crate) use packages::Packages;
pub use packages::{LockPolicy, ProjectSetup};

/// How a session finds and treats the files of a program.
#[derive(Debug, Clone)]
pub struct Config {
    pub project: ProjectSetup,
    pub lock: LockPolicy,
    /// The modules of Rust functions an embedder offers the program.
    pub host: Vec<HostModule>,
    /// The builtin modules the program may import.
    pub builtins: Builtins,
}

/// The builtin modules a program may import: an `import` of any other
/// is an error when the program is checked. The prelude (`println`,
/// `Option`, ...) is every program's.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Builtins {
    /// Every builtin module of the build.
    #[default]
    All,
    /// Only these (`["list", "string", "math"]`): a host that runs
    /// programs it did not write keeps `fs`, `io`, `env`, `process`,
    /// `tcp`, `http` and `postgres` from them so.
    Only(Vec<String>),
}

impl Builtins {
    /// Whether a program may import the builtin module `name`.
    pub(crate) fn allows(&self, name: &str) -> bool {
        match self {
            Builtins::All => true,
            Builtins::Only(names) => names.iter().any(|allowed| allowed == name),
        }
    }

    /// The error for an `import` (at `span`) of the builtin module
    /// `name`, which the program may not import.
    fn refusal(&self, name: Symbol, span: crate::source::Span) -> Diagnostic {
        let allowed = match self {
            Builtins::All => Vec::new(),
            Builtins::Only(names) => {
                let mut names: Vec<&str> = (names.iter())
                    .map(String::as_str)
                    .filter(|name| crate::module::is_builtin_module(name))
                    .collect();
                names.sort_unstable();
                names.dedup();
                names
            }
        };
        let offered = match allowed.as_slice() {
            [] => "no builtin module".to_string(),
            names => format!("only {}", names.join(", ")),
        };
        Diagnostic::error(
            Code::ModuleNotFound,
            span,
            format!(
                "the builtin module '{name}' is not offered to this program: the application \
                 that runs it lets it import {offered}"
            ),
        )
    }
}

/// What a program is compiled for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// A program that starts at `main`, which must have the type
    /// `() -> ()` or `() -> Result((), e)` (a `main` that never returns
    /// is one too).
    Main,
    /// The test functions (`test_*`, `skip_test_*`) whose names contain
    /// `filter`, each of the type `() -> ()` or `() -> Result((), e)`.
    /// The program calls nothing; the test runner calls each test.
    Tests { filter: Option<String> },
    /// A module checked on its own (`silt check`, the language server):
    /// its declarations, with nothing called. A `main` it binds must be
    /// able to start. Its functions named `test_*` and `skip_test_*` are
    /// tests, held to the signature of one, only in a test file, whose
    /// name ends with `_test.silt`; in any other file such a function is
    /// an ordinary one, as `silt run` takes it.
    Module,
    /// A REPL cell, added by `Session::add_cell`: its declarations are
    /// installed, and a cell of statements runs them; the script returns
    /// their value. Any other file: its declarations, with nothing
    /// called. Such a program is run by the REPL
    /// (`repl::Repl`, in a build with `repl`), not by
    /// [`crate::vm::Vm::run_program`].
    Cell,
}

/// The static diagnostics of an entry file and every module it reaches.
#[derive(Debug, Clone)]
pub struct Analysis {
    /// The diagnostics of every module of the entry's graph: package
    /// problems, the entry's parse errors, why an import failed (each
    /// other module's load or parse problems, the imports that resolve to
    /// nothing, the import cycles), the entry's type diagnostics, then
    /// each other module's type errors, with its warnings when it is a
    /// module of the entry's package. Each diagnostic once.
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
    /// Whether the checks keep what only an editor reads
    /// (`ModuleAnalysis::module_names`): set by the language server
    /// (`Session::for_editor`); a command pays nothing for it.
    for_editor: bool,
}

impl Session {
    pub fn new(config: Config) -> Session {
        let mut sources = SourceMap::new();
        let mut graph = ModuleGraph::new(config.builtins.clone());
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
            for_editor: false,
        }
    }

    /// Have the checks keep what only an editor reads: how each module
    /// writes the other modules' types (`ModuleAnalysis::module_names`).
    /// For the language server and the REPL (`:type`, `:doc`); the
    /// commands leave it unset and pay nothing for it.
    #[cfg(any(feature = "lsp", feature = "repl"))]
    pub(crate) fn set_for_editor(&mut self, on: bool) {
        self.for_editor = on;
    }

    /// How the module `here` writes the type `r` of another module in the
    /// editor (hover, signature help, completion, inlay hints) and in the
    /// REPL's `:type`, as the checker's messages write it: with its module,
    /// by the name the file gives the module (`errs.AppError` under
    /// `import da as errs`, `da.AppError` under `import da`, `j.JsonError`
    /// under `import json as j`), described where no import here can reach
    /// it (`InErr of package inner`); the module part is the checker's own
    /// (`ModuleWritten`). None for a type written by its bare name (a
    /// prelude type, one of the module's own, one the file imports by
    /// item: `Sq` after `import da.{ Sq }`), and where the module's
    /// check kept no names (a session not `set_for_editor`).
    #[cfg(any(feature = "lsp", feature = "repl"))]
    pub(crate) fn written_type_name(
        &self,
        here: ModuleId,
        r: crate::types::TypeRef,
    ) -> Option<String> {
        let analysis = self.analyses.get(&here)?;
        // A type the file imports by its name is written bare, as the
        // messages write it (`import da.{ Sq }`: `Sq`).
        if matches!(analysis.scope.types.get(&r.name), Some(names::Binding::Def(id)) if *id == r.id.0)
        {
            return None;
        }
        let written = |module: ModuleId| {
            (analysis.module_names.as_ref())?.written(module, &self.tables, &self.defs)
        };
        match crate::defs::builtin_types().get(r.id.0.0 as usize) {
            Some((_, None)) => None,
            Some((_, Some(module))) => Some(match ModuleId::builtin(module).and_then(written) {
                Some(written) => written.type_name(r.name),
                None => format!("{module}.{}", r.name),
            }),
            None => written(self.defs.get(r.id.0).module).map(|w| w.type_name(r.name)),
        }
    }

    /// How the module `here` writes the trait `t` in the editor (a bound of
    /// a signature): as the checker's messages write it, bare where the
    /// file imports it by item or declares it, by the file's name for its
    /// module (`d.Area` under `import da as d`, `j.Encode`), described where
    /// no import here can reach it; a prelude trait bare.
    #[cfg(any(feature = "lsp", feature = "repl"))]
    pub(crate) fn written_trait_name(&self, here: ModuleId, t: crate::types::TraitKey) -> String {
        let Some(analysis) = self.analyses.get(&here) else {
            return t.name.to_string();
        };
        if matches!(analysis.scope.types.get(&t.name), Some(names::Binding::Def(id)) if *id == t.id.0)
        {
            return t.name.to_string();
        }
        if let Some((name, module)) = crate::defs::builtin_trait_of_id(t.id.0) {
            return match module {
                Some(module) => self.written_builtin(here, &format!("{module}.{name}")),
                None => name.to_string(),
            };
        }
        let module = self.defs.get(t.id.0).module;
        match (analysis.module_names.as_ref())
            .and_then(|names| names.written(module, &self.tables, &self.defs))
        {
            Some(written) => written
                .type_name(t.name)
                .replace(crate::types::AFTER_ARGUMENTS, ""),
            None => t.name.to_string(),
        }
    }

    /// The builtin `qualified` (`option.to_result`) as the module `here`
    /// writes it, for a help the session writes: the checker's rule
    /// (`typechecker::written_builtin_member`), from the module's scope
    /// (an item it imports) and the names it binds builtin modules to.
    pub(crate) fn written_builtin(&self, here: ModuleId, qualified: &str) -> String {
        let Some((module, name)) = qualified.split_once('.') else {
            return qualified.to_string();
        };
        let (Some(id), Some(analysis)) = (ModuleId::builtin(module), self.analyses.get(&here))
        else {
            return qualified.to_string();
        };
        let by_item = |binding: Option<&names::Binding>| {
            matches!(binding, Some(names::Binding::Def(def))
                if self.defs.get(*def).module == id && resolve(self.defs.get(*def).name) == name)
        };
        let symbol = intern(name);
        let item = by_item(analysis.scope.values.get(&symbol))
            || by_item(analysis.scope.types.get(&symbol));
        crate::typechecker::written_builtin_member(
            module,
            name,
            item,
            analysis.builtin_bindings.get(&id).copied(),
        )
    }

    /// `diagnostic` as `silt check` prints it, its places read from the
    /// session's files: the header, the place with its source line and a
    /// caret, the other places, the notes and the help.
    pub fn render(&self, diagnostic: &Diagnostic) -> String {
        crate::diagnostic::render_human_in(
            &self.sources,
            diagnostic,
            &crate::diagnostic::COLORS_OFF,
        )
    }

    /// The text of every file the session has read.
    pub fn sources(&self) -> &SourceMap {
        &self.sources
    }

    /// The module graph.
    pub(crate) fn graph(&self) -> &ModuleGraph {
        &self.graph
    }

    /// Whether the project's packages were read and could not be
    /// resolved (see `packages`).
    #[cfg(feature = "lsp")]
    pub(crate) fn packages_failed(&self) -> bool {
        matches!(self.packages, Some(Err(_)))
    }

    /// The packages of the project: its manifests and `silt.lock` are
    /// read on the first call (and registered in the source map, where
    /// their diagnostics point), and the lock is rewritten when the
    /// policy allows it and it no longer pins the graph. When that
    /// fails, the session goes on as if there were no project, and every
    /// analysis reports the package diagnostics.
    pub(crate) fn packages(&mut self) -> Result<&Packages, &[Diagnostic]> {
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
    pub(crate) fn into_sources(self) -> SourceMap {
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
    pub(crate) fn open(&mut self, path: &Path) -> Result<FileId, std::io::Error> {
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
    #[cfg(feature = "repl")]
    pub(crate) fn add_cell(&mut self, text: String) -> FileId {
        // What an earlier cell that was not committed declared is gone.
        for cell in self.cells.take_dropped() {
            self.tables.forget(cell);
            self.release_cell(cell);
        }
        self.cells.count += 1;
        let n = self.cells.count;
        let file = self.enter_text(
            &PathBuf::from(crate::program::cell_name(n)),
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
    #[cfg(feature = "repl")]
    pub(crate) fn commit_cell(&mut self, file: FileId) {
        let id = self.module_of(file);
        let (Some(analysis), Some(result)) = (self.analyses.get(&id), self.results.get(&id)) else {
            return;
        };
        if let Some(before) = self.cells.commit(id, &analysis.ast, &result.modules) {
            self.release_cell(before);
        }
    }

    /// Give up what the session keeps of the REPL cell `cell` only to
    /// check a cell that imports it: its [`ModuleAnalysis`] (its scope
    /// and the types of its names, which are every name it saw) and
    /// the names its parsed text imports from the cell before it. No
    /// later cell imports it: a cell imports the last committed one,
    /// and this one is committed no longer, or never was. Its text, its
    /// definitions, what its check entered in the tables and its
    /// [`Analysis`] stay: a diagnostic that points into it reads as
    /// before, and it is never checked again.
    ///
    /// So a session keeps the analysis of one cell, not of every cell:
    /// its memory grows with what the entries declare, not with the
    /// entries times the names each of them sees.
    #[cfg(feature = "repl")]
    fn release_cell(&mut self, cell: ModuleId) {
        self.analyses.remove(&cell);
        if let Some(program) = self.graph.ast_mut(cell) {
            self.cells.release(cell, program);
        }
    }

    /// The problems of the project the session is in: its manifests and
    /// lockfile, and its module files named like builtin modules. Every
    /// analysis reports them, but a REPL cell's: the REPL shows them
    /// once, when it starts.
    #[cfg(feature = "repl")]
    pub(crate) fn project_problems(&mut self) -> Vec<Diagnostic> {
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
    pub(crate) fn module_of(&self, file: FileId) -> ModuleId {
        self.file_modules[&file]
    }

    /// The definitions the session's modules and the builtins declare.
    #[cfg(any(feature = "lsp", feature = "repl", test, feature = "test-hooks"))]
    pub(crate) fn defs(&self) -> &DefTable {
        &self.defs
    }

    /// What the checks of the session's modules found: every type,
    /// trait, impl and method, and the scheme of each definition.
    #[cfg(any(feature = "lsp", all(test, feature = "repl")))]
    pub(crate) fn tables(&self) -> &Tables {
        &self.tables
    }

    /// The check of module `id`, once [`Session::analyze`] has made it.
    /// Of the REPL cells, the last committed one has it, and the one
    /// being evaluated: an earlier cell's is given up.
    pub(crate) fn module_analysis(&self, id: ModuleId) -> Option<&ModuleAnalysis> {
        self.analyses.get(&id)
    }

    /// The type of the value of the REPL cell `file`, once
    /// [`Session::analyze`] has checked it: what the function that holds
    /// its statements returns, general in what the statements leave
    /// open, with what its scheme asks of those variables. `None` for a
    /// cell of declarations.
    #[cfg(feature = "repl")]
    pub(crate) fn cell_value_scheme(&self, file: FileId) -> Option<crate::types::Scheme> {
        let id = self.module_of(file);
        let wrapper = intern(&self.cells.info.get(&id)?.wrapper());
        let names::Binding::Def(def) = self.analyses.get(&id)?.scope.values.get(&wrapper)? else {
            return None;
        };
        let scheme = self.tables.scheme_of(*def)?;
        let Type::Fun(_, ret) = scheme.ty else {
            return None;
        };
        Some(crate::types::Scheme { ty: *ret, ..scheme })
    }

    /// The scheme of the definition `def`, as its module's check left
    /// it.
    #[cfg(feature = "repl")]
    pub(crate) fn def_scheme(&self, def: crate::defs::DefId) -> Option<crate::types::Scheme> {
        self.tables.scheme_of(def)
    }

    /// The file of the REPL cell that is module `id`.
    #[cfg(feature = "repl")]
    pub(crate) fn cell_file(&self, id: ModuleId) -> Option<FileId> {
        self.cells.info.get(&id).map(|cell| cell.file)
    }

    /// How a message about the REPL cell `file` writes the type or
    /// trait `def`, named `name`, when it is one of an earlier cell that
    /// a later cell declared again: with its cell (`<repl:1>.T`), as the
    /// cell's diagnostics write it; or a type of another module (a builtin
    /// module's, a module's of the program) as the editor and the
    /// messages write it (`written_type_name`: `regex.Regex`, `geo.Pt`).
    /// `None` for any other, which keeps its name.
    #[cfg(feature = "repl")]
    pub(crate) fn shadowed_cell_name(
        &self,
        file: FileId,
        def: crate::defs::DefId,
        name: Symbol,
    ) -> Option<String> {
        let owner = self.defs.get(def).module;
        // A type of a builtin module or of a module of the program
        // (`regex.Regex`, `geo.Pt`): as the editor writes it, with the
        // module part of the messages (`written_type_name`). (A trait
        // keeps its name.)
        if !self.cells.info.contains_key(&owner) {
            let def_ref = self.defs.get(def);
            if !def_ref.is_type() {
                return None;
            }
            let r = crate::types::TypeRef {
                id: crate::defs::TypeId(def),
                name,
            };
            return self.written_type_name(self.module_of(file), r);
        }
        let cell = self.cells.info.get(&owner)?;
        let scope = &self.analyses.get(&self.module_of(file))?.scope;
        match scope.types.get(&name) {
            Some(names::Binding::Def(id)) if *id == def => None,
            _ => Some(format!("{}.{name}", cell.wrapper())),
        }
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
        // The imports of every module checked here, in one walk: each
        // check finds its module's reach from them only if it asks
        // (`typechecker::Reach`), not by a walk of its own.
        let edges = std::sync::Arc::new(
            (self.graph).import_edges(std::iter::once(entry).chain(hosts.iter().copied())),
        );
        for id in hosts {
            if !self.analyses.contains_key(&id) {
                let analysis = self.check(id, &ordering, &edges);
                self.analyses.insert(id, analysis);
            }
        }
        for &id in &ordering.modules {
            let module = self.graph.module(id);
            if self.analyses.contains_key(&id) || (id != entry && module.failed()) {
                continue;
            }
            let analysis = self.check(id, &ordering, &edges);
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
    fn check(
        &mut self,
        id: ModuleId,
        ordering: &Ordering,
        edges: &std::sync::Arc<HashMap<ModuleId, Vec<ModuleId>>>,
    ) -> ModuleAnalysis {
        let module = self.graph.module(id);
        let mut ast = module.ast.clone().unwrap_or_default();
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
        let text = module.file.and_then(|f| self.sources.get(f));
        let hidden_imports = match kind {
            ModuleKind::Cell => self.cells.hidden_imports(),
            _ => HashMap::new(),
        };
        let resolution = names::resolve_module(
            &mut ast,
            id,
            kind,
            &imported,
            Arc::make_mut(&mut self.defs),
            names::Source {
                text: text.map_or("", |file| &file.text),
                complete: !module.incomplete,
                hidden_imports: &hidden_imports,
            },
        );
        let cell = kind == ModuleKind::Cell;
        let reach = (!cell).then(|| typechecker::Reach::new(id, edges.clone()));
        let cells = match cell {
            true => self.cells.ids(),
            false => Default::default(),
        };
        // (Each key of the package's `[dependencies]` whose library the
        // session has: a help offers its import by the key.)
        let packages = match &self.packages {
            Some(Ok(packages)) => packages,
            _ => &self.fallback_packages,
        };
        let dependencies = (packages.packages.get(module.package.0 as usize))
            .map_or(&[][..], |package| package.deps.as_slice())
            .iter()
            .filter_map(|(key, dep, _)| {
                let src = packages.packages.get(dep.0 as usize)?.src.as_ref()?;
                Some((*key, self.graph.module_at(&src.join("lib.silt"))?))
            })
            .collect();
        let package_name = (packages.packages.get(module.package.0 as usize))
            .map_or_else(|| intern(""), |package| package.name);
        let mut check = typechecker::check_module(
            &mut ast,
            ModuleContext {
                module: id,
                package: module.package,
                dependencies,
                package_name,
                module_name: module.name,
                kind,
                reach,
                cells,
                scope: &resolution.scope,
                earlier: &earlier,
                defs: self.defs.clone(),
                tables: &mut self.tables,
                for_editor: self.for_editor,
            },
        );
        say_hidden_modules(
            &self.sources,
            &resolution.hidden_modules,
            &mut check.diagnostics,
        );
        // Of a text that is cut short only the lexer's error is
        // reported: the declarations behind the cut are missing, and
        // every use of one would be an error of its own. The warnings
        // come after the errors, in the order of the text.
        let (mut warnings, errors): (Vec<Diagnostic>, Vec<Diagnostic>) = (resolution.diagnostics)
            .into_iter()
            .partition(Diagnostic::is_warning);
        warnings.sort_by_key(|d| d.span.start);
        // While the text has syntax errors, nothing is said to be never
        // used: the use may be in what did not parse. (A name bound twice
        // at the top level is no syntax error: everything parsed.)
        let module = self.graph.module(id);
        let syntax_error = |p: &Diagnostic| {
            matches!(
                p.phase(),
                crate::diagnostic::Phase::Lex | crate::diagnostic::Phase::Parse
            ) && p.code != Code::DuplicateTopLevel
                && !p.is_warning()
        };
        if module.incomplete || module.problems.iter().any(syntax_error) {
            warnings.retain(|d| !matches!(d.code, Code::UnusedName | Code::UnusedImport));
        }
        let diagnostics = match self.graph.module(id).cut_short {
            true => bugs,
            false => bugs
                .into_iter()
                .chain(errors)
                .chain(check.diagnostics)
                .chain(warnings)
                .collect(),
        };
        let diagnostics =
            without_unused_doubled_imports(&self.graph.module(id).problems, diagnostics);
        ModuleAnalysis {
            ast: Arc::new(ast),
            scope: resolution.scope,
            top_level: check.top_level,
            signatures: check.signatures,
            let_order: check.let_order,
            diagnostics,
            entry_display: check.entry_display,
            module_names: check.module_names,
            builtin_bindings: check.builtin_bindings,
        }
    }

    /// The diagnostics of the analysis of `entry`, in the order of
    /// [`Analysis::diagnostics`]. The warnings are those of the modules
    /// of the entry's package: a dependency's are not its user's to fix.
    /// Those of an earlier REPL entry were shown with it.
    fn collect_diagnostics(&self, entry: ModuleId, ordering: &Ordering) -> Vec<Diagnostic> {
        // (Each once, in the order found: a set beside the list, as a
        // program can have many thousands of warnings.)
        let mut out = Unique::default();
        let push = |d: &Diagnostic, out: &mut Unique| out.push(d);
        if !self.cells.info.contains_key(&entry) {
            if let Some(Err(diagnostics)) = &self.packages {
                for d in diagnostics {
                    push(d, &mut out);
                }
            }
            for d in &self.module_name_problems {
                push(d, &mut out);
            }
            let path = self.graph.module(entry).path.clone();
            if let Some(d) = self.entry_name_problem(entry, &path) {
                push(&d, &mut out);
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
        // Modules come in the order they are checked in, a module's
        // imports before it and the entry last: first why a module failed
        // (its parse errors, a file that cannot be read, a name that
        // resolves to nothing, a cycle), then the type diagnostics.
        // The problems of the imports of each module (a module whose file
        // could not be read: each import of it), in the order of the
        // importers and of their imports. (Found once: looking through
        // every import of the program for each module took time n squared
        // in a chain of n modules.)
        let mut import_problems: HashMap<ModuleId, Vec<&Diagnostic>> = HashMap::new();
        for &importer in &ordering.modules {
            for import in &self.graph.module(importer).imports {
                if let ImportResolution::Module(target) = import.resolution
                    && let Some(d) = &import.problem
                {
                    import_problems.entry(target).or_default().push(d);
                }
            }
        }
        for &id in &ordering.modules {
            for d in &self.graph.module(id).problems {
                push(d, &mut out);
            }
            for d in import_problems.get(&id).into_iter().flatten() {
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
        let package = self.graph.module(entry).package;
        for &id in &ordering.modules {
            let warnings = id == entry
                || (self.graph.module(id).package == package && !self.cells.info.contains_key(&id));
            if let Some(analysis) = self.analyses.get(&id) {
                for d in analysis.diagnostics.iter() {
                    if d.is_error() || warnings {
                        push(d, &mut out);
                    }
                }
            }
        }
        out.list
    }

    /// The error for the entry file `entry`, named `path`, when that
    /// name is no module name: a file named on the command line or
    /// opened in an editor is a module too, and a module's name is
    /// lowercase letters, digits and `_`, not starting with a digit, and
    /// not a builtin module's. A module file of a package's `src/` is
    /// reported already, by [`Session::packages`].
    fn entry_name_problem(&self, entry: ModuleId, path: &Path) -> Option<Diagnostic> {
        let module = self.graph.module(entry);
        if module.host.is_some() {
            return None;
        }
        // (By the file's name, not `extension`: `.silt` has none, and its
        // module name is the empty one.)
        let name = path.file_name()?.to_str()?.strip_suffix(".silt")?;
        if crate::module::is_module_name(name) && !crate::module::is_builtin_module(name) {
            return None;
        }
        let key = graph::canonical_key(&module.path);
        let reported = self.module_name_problems.iter().any(|d| {
            self.sources.get(d.span.file).is_some_and(
                |f| matches!(&f.path, SourceName::Path(p) if graph::canonical_key(p) == key),
            )
        });
        match reported {
            true => None,
            false => Some(packages::file_name_problem(name, module.file?)),
        }
    }

    /// The file-name error of the entry module `entry` as its analysis
    /// reports it (by the name the file was given last), and as it is for
    /// the name `opened_as`. An editor with one file open by two names (a
    /// link and the file it leads to) shares one analysis between them,
    /// and judges each document by its own name.
    #[cfg(feature = "lsp")]
    pub(crate) fn entry_name_problems(
        &self,
        entry: ModuleId,
        opened_as: &Path,
    ) -> (Option<Diagnostic>, Option<Diagnostic>) {
        let path = &self.graph.module(entry).path;
        (
            self.entry_name_problem(entry, path),
            self.entry_name_problem(entry, opened_as),
        )
    }

    /// Compile the entry file `entry` for `target`. An analysis with an
    /// error is not compiled: the `Err` holds the analysis's errors. (A
    /// door that shows the analysis compiles only when it has none.)
    /// Otherwise the entry point is checked by its inferred type, then the
    /// modules are compiled; the `Err` holds what that found.
    pub fn compile(&mut self, entry: FileId, target: Entry) -> Result<Program, Vec<Diagnostic>> {
        let entry_file = entry;
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
                        self.graph
                            .module(id)
                            .file
                            .expect("an analysed module has a file"),
                        self.entry_paths.get(&id).map_or("", String::as_str),
                    )),
                    Some(ty) => {
                        entry::check_main(&analysis.ast, ty, &|n| self.written_builtin(id, n))
                    }
                });
                entry_errors.extend(analysis.entry_display.get(&main).cloned());
                EntryPoint::Main {
                    span: entry::main_span(&analysis.ast),
                }
            }
            Entry::Tests { .. } | Entry::Module => {
                // A file checked or run for its tests that binds `main`
                // must have a `main` that can start.
                if let Some(ty) = analysis.top_level.get(&main) {
                    entry_errors.extend(entry::check_main(&analysis.ast, ty, &|n| {
                        self.written_builtin(id, n)
                    }));
                    entry_errors.extend(analysis.entry_display.get(&main).cloned());
                }
                // (A module checked on its own has tests only when it is
                // a test file.)
                let filter = match &target {
                    Entry::Tests { filter } => Some(filter.as_deref()),
                    _ => entry::is_test_file(&self.graph.module(id).path).then_some(None),
                };
                let mut tests = Vec::new();
                if let Some(filter) = filter {
                    let (selected, errors) =
                        entry::select_tests(&analysis.ast, &analysis.top_level, filter, &|n| {
                            self.written_builtin(id, n)
                        });
                    tests = selected;
                    entry_errors.extend(errors);
                    // (A test whose error type has no Display: also one
                    // whose signature is refused above.)
                    for (f, _, _) in entry::selected_tests(&analysis.ast, filter) {
                        entry_errors.extend(analysis.entry_display.get(&f.name).cloned());
                    }
                }
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
            Entry::Tests { .. } | Entry::Module => compiler.compile_declarations(&program),
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
                let globals = Arc::new(compiler.globals().clone());
                let types = compiler.types();
                // A cell's definitions are the session's when the cell
                // has run to its end; until then the session's globals
                // are what stands of the program.
                let settled =
                    match matches!(target, Entry::Cell) && self.cells.info.contains_key(&id) {
                        true => {
                            let settled = Arc::new(self.cells.globals.clone());
                            self.cells.compiled((*globals).clone());
                            settled
                        }
                        false => globals.clone(),
                    };
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
                    types,
                    globals,
                    settled,
                    entry,
                    files: Arc::new(self.sources.clone()),
                    entry_file,
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
    pub(crate) fn program_units(&self, id: ModuleId, cell: bool) -> ProgramUnits<'_> {
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
            tables: &self.tables,
            earlier,
            modules: modules
                .iter()
                .map(|m| {
                    let module = self.graph.module(*m);
                    ModuleUnit {
                        id: *m,
                        program: self.analyses[m].ast.clone(),
                        let_order: self.analyses[m].let_order.clone(),
                        // (A host module is imported by its name alone.)
                        qualifier: match (module.package == entry_package, resolve(module.name)) {
                            (_, name) if module.host.is_some() => name,
                            (true, name) => name,
                            (false, name) if name == "lib" => resolve(module.package_name),
                            (false, name) => format!("{}.{name}", module.package_name),
                        },
                        imports: unit_imports(module, &index),
                        host: (module.host).map(|host| self.host_functions(module, host)),
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
        let scope = self
            .analyses
            .get(&module.id)
            .map(|analysis| &analysis.scope);
        // The types the signatures name: the module's own, and the
        // builtin ones (`Int`, `time.Date`).
        let lookup = |qualifier: Option<Symbol>, name: Symbol| {
            if qualifier.is_none()
                && let Some(names::Binding::Def(def)) = scope.and_then(|s| s.types.get(&name))
                && self.defs.get(*def).module == module.id
            {
                let decl = decls.iter().find_map(|decl| match decl {
                    ast::Decl::Type(t) if t.name == name => Some(t),
                    _ => None,
                })?;
                return Some(host::Named {
                    id: crate::defs::TypeId(*def),
                    decl: Some(decl),
                });
            }
            let id = crate::defs::builtin_type_id(&resolve(name))?;
            let home = crate::defs::builtin_types()[id.0.0 as usize].1;
            if home != qualifier.map(resolve).as_deref() {
                return None;
            }
            Some(host::Named { id, decl: None })
        };
        let mut templates = crate::value::HostTemplates::new();
        let mut shapes = host::Shapes {
            lookup: &lookup,
            tables: &self.tables,
            templates: &mut templates,
        };
        let signed: Vec<_> = (decls.iter())
            .filter_map(|decl| match decl {
                ast::Decl::Fn(decl) => Some(decl),
                _ => None,
            })
            .zip(&self.config.host[host].fns)
            .map(|(decl, f)| (decl, f, host::returns(decl, &mut shapes)))
            .collect();
        // (One table of the types' fields for the module's functions.)
        let templates = Arc::new(templates);
        (signed.into_iter())
            .map(|(decl, f, (returns, vars))| {
                (
                    decl.name,
                    Arc::new(HostFn {
                        name: format!("{}.{}", module.name, decl.name),
                        call: f.call.clone(),
                        returns,
                        vars,
                        arity: decl.params.len(),
                        types: Default::default(),
                        templates: templates.clone(),
                    }),
                )
            })
            .collect()
    }

    /// The files of the graph of `entry`, for a watcher: each module's
    /// file, as named in diagnostics. Host modules have none.
    #[cfg(feature = "watch")]
    pub(crate) fn files(&self, entry: FileId) -> Vec<PathBuf> {
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

/// `diagnostics` without the unused-import warnings of an import that
/// is in a bound-twice error of `problems` (a top-level `let` or `fn`
/// named like it): one message per problem.
fn without_unused_doubled_imports(
    problems: &[Diagnostic],
    mut diagnostics: Vec<Diagnostic>,
) -> Vec<Diagnostic> {
    let doubled: Vec<crate::source::Span> = (problems.iter())
        .filter(|p| p.code == Code::DuplicateTopLevel)
        .flat_map(|p| p.labels.iter().map(|(span, _)| *span).chain([p.span]))
        .collect();
    let overlaps = |a: crate::source::Span, b: crate::source::Span| {
        a.file == b.file && a.start < b.end.max(b.start + 1) && b.start < a.end.max(a.start + 1)
    };
    diagnostics.retain(|d| {
        d.code != Code::UnusedImport || !doubled.iter().any(|span| overlaps(*span, d.span))
    });
    diagnostics
}

/// The help of an access `m.x` that names nothing because the local `m`
/// hides the imported module `m`, which has a member `x`
/// ([`names::HiddenModule`]): it replaces the help of the error that
/// the local has no field or method `x`.
fn say_hidden_modules(
    sources: &SourceMap,
    hidden: &[names::HiddenModule],
    diagnostics: &mut [Diagnostic],
) {
    for h in hidden {
        for d in diagnostics.iter_mut().filter(|d| {
            d.span == h.access
                && matches!(
                    d.code,
                    Code::UnknownMethod | Code::UnknownField | Code::NoSuchField
                )
        }) {
            let (name, module) = (resolve(h.name), resolve(h.module));
            let hidden = match name == module {
                true => format!("`{module}`"),
                false => format!("`{module}` (imported as `{name}`)"),
            };
            d.help = vec![match h.by {
                names::HiddenBy::Local(local) => {
                    let (line, _) = sources.line_col((local.file, local.start));
                    format!(
                        "`{name}` here is the local from line {line}, which hides the module \
                         {hidden}; rename the local"
                    )
                }
                names::HiddenBy::EarlierEntry => format!(
                    "`{name}` here is the `let` of an earlier entry, which hides the module \
                     {hidden}; rename it, or import the module under another name"
                ),
            }];
        }
    }
}

/// Diagnostics, each once, in the order they were first given.
#[derive(Default)]
pub(crate) struct Unique {
    pub(crate) list: Vec<Diagnostic>,
    seen: std::collections::HashSet<Diagnostic>,
}

impl Unique {
    /// Add `d` unless it is in already.
    pub(crate) fn push(&mut self, d: &Diagnostic) {
        if self.seen.insert(d.clone()) {
            self.list.push(d.clone());
        }
    }
}
