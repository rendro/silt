//! Package/manifest/lockfile helpers shared across the CLI subcommands.
//!
//! Covers project root discovery, lockfile auto-update/resolve logic,
//! and the "where should imports be resolved from" plumbing used by
//! the compile pipeline and by command-specific dispatch paths.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process;

use silt::intern::{self, Symbol};
use silt::lockfile::{Lockfile, LockfileError};
use silt::manifest::{Manifest, ManifestError};

/// Walk up from `start` looking for the nearest `silt.toml`. Returns the
/// project root directory and the loaded `Manifest` if found, or `None`
/// if no manifest is reachable before the filesystem root.
///
/// Replaces the heuristic `project_anchor()` which looked for `silt.toml`
/// OR `.git`. With first-class manifest support, only `silt.toml` matters
/// for project boundaries.
pub fn find_project_root(start: &Path) -> Result<Option<(PathBuf, Manifest)>, ManifestError> {
    match Manifest::find(start) {
        Some(dir) => {
            let manifest = Manifest::load(&dir.join("silt.toml"))?;
            Ok(Some((dir, manifest)))
        }
        None => Ok(None),
    }
}

/// Print a manifest error to stderr and exit. Used by callers that need
/// the manifest to proceed (e.g. `silt run` resolving the entry point).
pub(crate) fn die_on_manifest_error(err: ManifestError) -> ! {
    eprintln!("error: {err}");
    process::exit(1);
}

/// Synthetic package name used when compiling a `.silt` file outside any
/// silt package (REPL-style invocations, ad-hoc scripts, the
/// `silt run script.silt` legacy path). Matches what
/// `Compiler::with_project_root` used internally pre-PR-4 so any
/// downstream code keying on the local package name keeps working.
const ANONYMOUS_LOCAL_PACKAGE: &str = "__local__";

/// Derive the package_roots map and local-package symbol the compiler
/// needs to resolve `import` statements from `path`.
///
/// Two modes:
///   - `path` lives inside a silt package (manifest reachable above its
///     parent): we resolve the dep tree from `silt.lock`, optionally
///     auto-regenerating the lock if it's missing or stale (controlled
///     by `auto_update_lock`). The local package is registered under
///     its real name from `silt.toml`; deps are registered under the
///     names from their respective manifests.
///   - No manifest reachable: we synthesise a single-root setup under
///     [`ANONYMOUS_LOCAL_PACKAGE`] mapped to the file's parent
///     directory. This preserves the legacy "ad-hoc script" behavior
///     where `import foo` resolves to a sibling `foo.silt`.
///
/// `auto_update_lock = false` is what `silt fmt` and `silt disasm` use:
/// they should never mutate the lockfile (read-only operations); if
/// the lock is missing or stale they just resolve from the existing
/// (possibly empty) lockfile, which is fine because the local package
/// always loads regardless and missing deps surface naturally as
/// import errors.
///
/// Manifest or lockfile errors are fatal — they're rendered to stderr
/// and the process exits with code 1. Run/check/test paths can't
/// proceed without a coherent dep graph.
pub(crate) fn package_setup_for_file(
    path: &str,
    auto_update_lock: bool,
) -> (Symbol, HashMap<Symbol, PathBuf>) {
    let file_parent = Path::new(path)
        .canonicalize()
        .unwrap_or_else(|_| Path::new(path).to_path_buf())
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));

    match find_project_root(&file_parent) {
        Ok(Some((root, manifest))) => {
            let lockfile_path = root.join("silt.lock");
            let lockfile = if auto_update_lock {
                ensure_fresh_lockfile(&manifest, &lockfile_path)
            } else {
                load_or_resolve_lockfile(&manifest, &lockfile_path)
            };
            let package_roots = lockfile.package_roots(&manifest);
            (manifest.package.name, package_roots)
        }
        Ok(None) => fallback_package_setup(&file_parent),
        Err(e) => die_on_manifest_error(e),
    }
}

/// Construct the no-package fallback: synthetic local package name
/// mapped to `dir` so legacy ad-hoc scripts continue to resolve
/// `import foo` against sibling files.
fn fallback_package_setup(dir: &Path) -> (Symbol, HashMap<Symbol, PathBuf>) {
    let local = intern::intern(ANONYMOUS_LOCAL_PACKAGE);
    let mut roots = HashMap::new();
    roots.insert(local, dir.to_path_buf());
    (local, roots)
}

/// Auto-update path: regenerate `silt.lock` if it's missing or stale
/// relative to `manifest`. Prints a single notice line to stderr when
/// a regeneration happens so the user knows the file changed.
///
/// Any lockfile error (resolve, parse, write) is fatal. We deliberately
/// don't fall back silently — a half-resolved lockfile is worse than
/// no lockfile because it would let imports succeed against stale
/// content checksums.
fn ensure_fresh_lockfile(manifest: &Manifest, lockfile_path: &Path) -> Lockfile {
    let existing = match Lockfile::load(lockfile_path) {
        Ok(lock) => Some(lock),
        Err(LockfileError::Io(err, _)) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => die_on_lockfile_error(e),
    };
    let needs_refresh = match &existing {
        None => true,
        Some(lock) => !lock.matches_manifest(manifest),
    };
    if !needs_refresh {
        return existing.expect("checked above");
    }
    if existing.is_some() {
        eprintln!("Updating silt.lock for new dependencies in silt.toml");
    }
    let fresh = match Lockfile::resolve(manifest) {
        Ok(l) => l,
        Err(e) => die_on_lockfile_error(e),
    };
    if let Err(e) = fresh.write(lockfile_path) {
        die_on_lockfile_error(e);
    }
    fresh
}

/// Read-only path: load `silt.lock` if it exists, otherwise resolve
/// from the manifest in-memory without writing. Used by `silt fmt`
/// and `silt disasm`, which shouldn't touch the lockfile.
fn load_or_resolve_lockfile(manifest: &Manifest, lockfile_path: &Path) -> Lockfile {
    match Lockfile::load(lockfile_path) {
        Ok(lock) => lock,
        Err(LockfileError::Io(err, _)) if err.kind() == std::io::ErrorKind::NotFound => {
            // No lockfile on disk, but we still need *some* dep map for
            // the compiler. Resolve in-memory; if that fails the user
            // gets a clear error (and can run `silt update` to write a
            // real lock and see the same diagnostic).
            match Lockfile::resolve(manifest) {
                Ok(l) => l,
                Err(e) => die_on_lockfile_error(e),
            }
        }
        Err(e) => die_on_lockfile_error(e),
    }
}

pub(crate) fn die_on_lockfile_error(err: LockfileError) -> ! {
    eprintln!("error: {err}");
    process::exit(1);
}

/// What the caller intends to do with the resolved entry point.
///
/// Round 93 (fix G2): lib-only packages — the REQUIRED shape for
/// dependencies (`src/lib.silt`, no `src/main.silt`) — must be
/// checkable with a bare `silt check`, while `silt run` (and
/// `silt disasm`, which disassembles the runnable artifact) still
/// need a `main.silt` to execute.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum EntryPointKind {
    /// Require `src/main.silt`. Used by `silt run` and `silt disasm` —
    /// both need an executable entry point.
    RequireMain,
    /// Prefer `src/main.silt`, fall back to `src/lib.silt` when main is
    /// absent. Used by `silt check`, which only needs something to
    /// typecheck/compile.
    AllowLib,
}

/// Resolve the package entry point (`<root>/src/main.silt`) for the current
/// directory, requiring a runnable `main.silt`. Thin wrapper over
/// [`resolve_package_entry_point_for`] kept so run/disasm call sites read
/// the same as before round 93.
pub(crate) fn resolve_package_entry_point() -> Result<Option<PathBuf>, ()> {
    resolve_package_entry_point_for(EntryPointKind::RequireMain)
}

/// Resolve the package entry point for the current directory.
///
/// Returns:
/// - `Ok(Some(path))` — we are inside a package and a suitable entry
///   point exists (`src/main.silt`, or `src/lib.silt` under
///   [`EntryPointKind::AllowLib`]).
/// - `Ok(None)` — there is no enclosing package (no `silt.toml` in any parent).
/// - `Err(())` — entry point check failed and we already wrote a diagnostic.
///   The caller should propagate the failure as a non-zero exit.
///
/// When the package is lib-only and the caller requires main, the
/// diagnostic names both ways out (`silt check` works on library-only
/// packages; add a `main.silt` to run). Lock:
/// tests/round93_lib_check_tests.rs.
pub(crate) fn resolve_package_entry_point_for(kind: EntryPointKind) -> Result<Option<PathBuf>, ()> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let (root, _manifest) = match find_project_root(&cwd) {
        Ok(Some(pair)) => pair,
        Ok(None) => return Ok(None),
        Err(e) => die_on_manifest_error(e),
    };
    let entry = root.join("src").join("main.silt");
    if entry.is_file() {
        return Ok(Some(entry));
    }
    let lib = root.join("src").join("lib.silt");
    if lib.is_file() {
        if kind == EntryPointKind::AllowLib {
            return Ok(Some(lib));
        }
        eprintln!(
            "package has no entry point — expected `src/main.silt` at {}",
            entry.display()
        );
        eprintln!("  = note: found `src/lib.silt` — this is a library-only package");
        eprintln!(
            "  = help: `silt check` works on library-only packages; add `src/main.silt` \
             with a `main()` function to make it runnable"
        );
        return Err(());
    }
    eprintln!(
        "package has no entry point — expected `src/main.silt` at {}",
        entry.display()
    );
    Err(())
}
