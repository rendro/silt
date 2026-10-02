//! The projects of the open documents, each with its compilation session.
//!
//! A document belongs to the project of the nearest `silt.toml` above it,
//! or, without one, to its directory. Each project has one
//! [`Session`] (read-only: the LSP never writes `silt.lock`) that lives as
//! long as a document of the project is open. Every open document is an
//! overlay of the session, so each module is read once and checked again
//! only when it or a module it imports changed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use crate::diagnostic::Diagnostic;
use crate::manifest::Manifest;
use crate::session::{Config, LockPolicy, ModuleId, ProjectSetup, Session};

/// The project directory of the file at `path`: the directory of the
/// nearest `silt.toml` above it, else the file's own directory.
pub(super) fn project_dir(path: &Path) -> PathBuf {
    let dir = path.parent().unwrap_or(Path::new("."));
    Manifest::find(dir).unwrap_or_else(|| dir.to_path_buf())
}

/// `path` as the key of a file: canonical when it exists.
pub(super) fn path_key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// One project and its session.
pub(super) struct Project {
    pub(super) config: Config,
    pub(super) session: Session,
    /// The modification time and size of the project's `silt.toml` and
    /// `silt.lock` when the session was made. The session resolves the
    /// packages once, so a change to either makes a new session.
    stamps: Vec<Stamp>,
    /// The text given to the session for each file, by path key.
    overlays: HashMap<PathBuf, Arc<str>>,
    /// What compiling each analysed entry module found, with the modules
    /// of its graph. Dropped when one of them changes.
    compiled: HashMap<ModuleId, (Vec<ModuleId>, Vec<Diagnostic>)>,
}

impl Project {
    pub(super) fn new(dir: &Path) -> Project {
        let project = if dir.is_dir() {
            ProjectSetup::Discover(dir.to_path_buf())
        } else {
            ProjectSetup::None
        };
        let config = Config {
            project,
            lock: LockPolicy::ReadOnly,
            host: Vec::new(),
        };
        Project {
            session: Session::new(config.clone()),
            config,
            stamps: stamps(dir),
            overlays: HashMap::new(),
            compiled: HashMap::new(),
        }
    }

    /// Whether the project's manifest or lockfile changed since the
    /// session was made.
    pub(super) fn is_stale(&self, dir: &Path) -> bool {
        stamps(dir) != self.stamps
    }

    /// Give the session `text` for the file at `path`, unless it has that
    /// text already. Returns the file's module when the text was new.
    pub(super) fn set_text(&mut self, path: &Path, text: &Arc<str>) -> Option<ModuleId> {
        let key = path_key(path);
        if self.overlays.get(&key) == Some(text) {
            return None;
        }
        self.overlays.insert(key, text.clone());
        let file = self.session.set_overlay(path, text.to_string());
        Some(self.session.module_of(file))
    }

    /// Whether the session has a module for the file at `path`.
    pub(super) fn has_module(&self, path: &Path) -> bool {
        self.session.graph().module_at(path).is_some()
    }

    /// The module of the file at `path`, which the session has.
    pub(super) fn module_at(&self, path: &Path) -> ModuleId {
        self.session
            .graph()
            .module_at(path)
            .expect("a file given to the session has a module")
    }

    /// The file of module `id`, which the session has read.
    pub(super) fn file(&self, id: ModuleId) -> crate::source::FileId {
        self.session
            .graph()
            .module(id)
            .file
            .expect("an entered module has a file")
    }

    /// Forget what compiling found for every entry whose graph holds one
    /// of `changed`.
    pub(super) fn forget_compiled(&mut self, changed: &[ModuleId]) {
        self.compiled
            .retain(|_, (modules, _)| !modules.iter().any(|m| changed.contains(m)));
    }

    /// What compiling the entry module `id` finds, once its analysis has
    /// no error: the compile errors (`main` is not required, as for a
    /// test file). Computed once until a module of its graph changes.
    pub(super) fn compile_errors(&mut self, id: ModuleId, modules: &[ModuleId]) -> &[Diagnostic] {
        if !self.compiled.contains_key(&id) {
            let file = self.file(id);
            let errors = match self
                .session
                .compile(file, crate::session::Entry::Tests { filter: None })
            {
                Ok(_) => Vec::new(),
                Err(errors) => errors,
            };
            self.compiled.insert(id, (modules.to_vec(), errors));
        }
        &self.compiled[&id].1
    }
}

/// What tells one version of a file from another: its modification time
/// and size, or nothing when it does not exist.
type Stamp = Option<(SystemTime, u64)>;

/// The stamps of the manifest and lockfile in `dir`.
fn stamps(dir: &Path) -> Vec<Stamp> {
    ["silt.toml", "silt.lock"]
        .iter()
        .map(|name| {
            let meta = std::fs::metadata(dir.join(name)).ok()?;
            Some((meta.modified().ok()?, meta.len()))
        })
        .collect()
}
