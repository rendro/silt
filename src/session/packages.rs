//! Where the packages of a program are: the one place the session reads
//! `silt.toml` and `silt.lock`.
//!
//! This is the package layer as it stands before the package graph
//! (`resolve_packages` in `package_graph.rs`) exists: packages are known
//! by name, and every package's source directory is found from the
//! lockfile. The session calls [`resolve_packages`] once and builds the
//! module graph on what it returns.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::intern::{Symbol, intern};
use crate::lockfile::{Lockfile, LockfileError};
use crate::manifest::{Manifest, ManifestError};

/// Where a session finds the project of the files it is given.
#[derive(Debug, Clone)]
pub enum ProjectSetup {
    /// Look for `silt.toml` in this directory and its parents. Without
    /// one, the directory is the source directory of an unnamed package,
    /// so a script can import its sibling files.
    Discover(PathBuf),
    /// No manifest: this directory is the source directory of an
    /// unnamed package.
    Script(PathBuf),
    /// No project at all: only builtin modules can be imported (tests,
    /// the playground).
    None,
}

/// What a session may do with `silt.lock`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockPolicy {
    /// Rewrite it when it is missing or does not match `silt.toml`
    /// (`run`, `check`, `test`).
    Update,
    /// Never write it: a missing lock is resolved in memory (`disasm`,
    /// `fmt`, the LSP).
    ReadOnly,
}

/// The packages of a program.
#[derive(Debug, Clone)]
pub struct Packages {
    /// The package the entry file belongs to.
    pub local: Symbol,
    /// The source directory (`<root>/src`, or the script's directory) of
    /// every package, by name. Dependencies are known by their name.
    pub roots: HashMap<Symbol, PathBuf>,
    /// Whether `silt.lock` was rewritten because it did not match the
    /// manifest.
    pub lock_rewritten: bool,
}

/// Why the packages could not be resolved.
#[derive(Debug)]
pub enum PackageError {
    Manifest(ManifestError),
    Lockfile(LockfileError),
}

/// The name of the package of a file outside any package. Package names
/// are validated against `[a-z][a-z0-9_-]*`, so it cannot collide.
const UNNAMED_PACKAGE: &str = "__local__";

/// Resolve the packages of `project`, reading `silt.toml` and
/// `silt.lock` once, and rewriting the lock when `lock` allows it and it
/// is stale.
pub fn resolve_packages(
    project: &ProjectSetup,
    lock: LockPolicy,
) -> Result<Packages, PackageError> {
    let unnamed = |dir: Option<&Path>| {
        let local = intern(UNNAMED_PACKAGE);
        let mut roots = HashMap::new();
        if let Some(dir) = dir {
            roots.insert(local, dir.to_path_buf());
        }
        Packages {
            local,
            roots,
            lock_rewritten: false,
        }
    };
    let dir = match project {
        ProjectSetup::Discover(dir) => dir,
        ProjectSetup::Script(dir) => return Ok(unnamed(Some(dir))),
        ProjectSetup::None => return Ok(unnamed(None)),
    };
    let Some(root) = Manifest::find(dir) else {
        return Ok(unnamed(Some(dir)));
    };
    let manifest = Manifest::load(&root.join("silt.toml")).map_err(PackageError::Manifest)?;
    let lockfile_path = root.join("silt.lock");
    let (lockfile, lock_rewritten) = match lock {
        LockPolicy::Update => fresh_lockfile(&manifest, &lockfile_path)?,
        LockPolicy::ReadOnly => (existing_lockfile(&manifest, &lockfile_path)?, false),
    };
    Ok(Packages {
        local: manifest.package.name,
        roots: lockfile.package_roots(&manifest),
        lock_rewritten,
    })
}

/// The lockfile, rewritten first when it is missing or does not match
/// `manifest`. The flag says whether an existing lock was replaced.
fn fresh_lockfile(manifest: &Manifest, path: &Path) -> Result<(Lockfile, bool), PackageError> {
    let existing = match Lockfile::load(path) {
        Ok(lock) => Some(lock),
        Err(LockfileError::Io(err, _)) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(PackageError::Lockfile(e)),
    };
    if let Some(lock) = existing.as_ref()
        && lock.matches_manifest(manifest)
    {
        return Ok((existing.expect("checked above"), false));
    }
    let fresh = Lockfile::resolve(manifest).map_err(PackageError::Lockfile)?;
    fresh.write(path).map_err(PackageError::Lockfile)?;
    Ok((fresh, existing.is_some()))
}

/// The lockfile as it is, or resolved in memory when there is none.
fn existing_lockfile(manifest: &Manifest, path: &Path) -> Result<Lockfile, PackageError> {
    match Lockfile::load(path) {
        Ok(lock) => Ok(lock),
        Err(LockfileError::Io(err, _)) if err.kind() == std::io::ErrorKind::NotFound => {
            Lockfile::resolve(manifest).map_err(PackageError::Lockfile)
        }
        Err(e) => Err(PackageError::Lockfile(e)),
    }
}
