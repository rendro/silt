//! Package/manifest/lockfile helpers shared across the CLI subcommands.
//!
//! Covers project root discovery, the entry point of a package, and the
//! rendering of package diagnostics. The packages of a program are
//! resolved by the session (`silt::session`).

use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};

use silt::diagnostic::Diagnostic;
use silt::manifest::Manifest;
use silt::source::SourceMap;

/// Package diagnostics and the source map holding the manifests and
/// lockfile they point into.
pub(crate) struct PackageFailure {
    pub(crate) sources: SourceMap,
    pub(crate) diagnostics: Vec<Diagnostic>,
}

/// Walk up from `start` looking for the nearest `silt.toml`. Returns the
/// project root directory and the loaded `Manifest` if found, or `None`
/// if no manifest is reachable before the filesystem root.
///
/// Only `silt.toml` marks a project boundary.
pub(crate) fn find_project_root(
    start: &Path,
) -> Result<Option<(PathBuf, Manifest)>, PackageFailure> {
    match Manifest::find(start) {
        Some(dir) => {
            let mut sources = SourceMap::new();
            match Manifest::load(&dir.join("silt.toml"), &mut sources) {
                Ok(manifest) => Ok(Some((dir, manifest))),
                Err(d) => Err(PackageFailure {
                    sources,
                    diagnostics: vec![d],
                }),
            }
        }
        None => Ok(None),
    }
}

/// Print package diagnostics to stderr (or as JSON on stdout, see
/// [`print_manifest_errors_as_json`]) and exit 1. Used by callers that
/// need the manifest or the package graph to proceed.
pub(crate) fn die_on_manifest_error(failure: PackageFailure) -> ! {
    let PackageFailure {
        sources,
        diagnostics,
    } = failure;
    if MANIFEST_ERRORS_AS_JSON.load(Ordering::Relaxed) {
        let json = serde_json::Value::Array(
            diagnostics
                .iter()
                .map(|d| silt::diagnostic::render_json(&sources, d))
                .collect(),
        );
        println!("{json}");
    } else {
        silt::diagnostic::eprint_all(&sources, &diagnostics);
    }
    process::exit(1);
}

/// Whether package errors are printed as `silt check --format json`
/// prints its diagnostics, on stdout. Set by `silt check --format json`.
static MANIFEST_ERRORS_AS_JSON: AtomicBool = AtomicBool::new(false);

/// Print package errors as JSON on stdout from now on.
pub(crate) fn print_manifest_errors_as_json() {
    MANIFEST_ERRORS_AS_JSON.store(true, Ordering::Relaxed);
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
/// tests/lang/round93_lib_check_tests.rs.
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
            silt::git::escape_for_display(&entry.display().to_string())
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
        silt::git::escape_for_display(&entry.display().to_string())
    );
    Err(())
}
