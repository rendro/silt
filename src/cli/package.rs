//! Package/manifest/lockfile helpers shared across the CLI subcommands.
//!
//! Covers project root discovery, lockfile auto-update/resolve logic,
//! and the "where should imports be resolved from" plumbing used by
//! the compile pipeline and by command-specific dispatch paths.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};

use silt::diagnostic::Diagnostic;
use silt::intern::{self, Symbol};
use silt::manifest::Manifest;
use silt::package_graph::{LockChange, LockPolicy, resolve_packages};
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

/// Synthetic package name used when compiling a `.silt` file outside any
/// silt package (ad-hoc scripts): `import foo` resolves to a sibling
/// `foo.silt`.
const ANONYMOUS_LOCAL_PACKAGE: &str = "__local__";

/// Derive the package_roots map and local-package symbol the compiler
/// needs to resolve `import` statements from `path`.
///
/// Two modes:
///   - `path` lives inside a silt package (manifest reachable above its
///     parent): the package graph is resolved (`package_graph`), with
///     `silt.lock` rewritten when it no longer pins the graph if
///     `auto_update_lock`, or only read otherwise. The local package is
///     registered under its `[package].name`; every dependency under
///     the key that names it in `[dependencies]`. One flat map serves
///     every package, so a key that names two different packages in the
///     graph is an error.
///   - No manifest reachable: a single-root setup under
///     [`ANONYMOUS_LOCAL_PACKAGE`] mapped to the file's parent
///     directory, so `import foo` resolves to a sibling `foo.silt`.
///
/// `auto_update_lock = false` is what `silt fmt` and `silt disasm` use:
/// they never write the lockfile.
///
/// Package errors are fatal: they're rendered and the process exits
/// with code 1. Run/check/test can't proceed without a coherent graph.
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

    let Some(root) = Manifest::find(&file_parent) else {
        return fallback_package_setup(&file_parent);
    };
    let policy = if auto_update_lock {
        LockPolicy::Update
    } else {
        LockPolicy::ReadOnly
    };
    let mut sources = SourceMap::new();
    let graph = match resolve_packages(&root, policy, &mut sources) {
        Ok(graph) => graph,
        Err(diagnostics) => die_on_manifest_error(PackageFailure {
            sources,
            diagnostics,
        }),
    };
    if graph.lock == LockChange::Updated {
        eprintln!("Updating silt.lock for new dependencies in silt.toml");
    }
    // The compiler knows one flat map from import name to source
    // directory, for every package at once. So one key may name only one
    // package in the whole graph: a key that two packages use for two
    // different packages is an error, never a silent pick of one.
    let root_node = graph.package(graph.root);
    let mut roots = HashMap::new();
    roots.insert(root_node.name, root_node.src.clone());
    let mut named: HashMap<Symbol, (silt::package_graph::PackageId, silt::source::Span)> =
        HashMap::new();
    let mut diagnostics = Vec::new();
    for node in &graph.packages {
        for (key, dep, span) in &node.deps {
            match named.get(key) {
                Some((first, _)) if first == dep => {}
                Some((first, first_span)) => diagnostics.push(
                    Diagnostic::error(
                        silt::diagnostic::Code::DependencyKeyCollision,
                        *span,
                        format!(
                            "dependency key `{}` names two different packages in this graph: \
                             `{}` and `{}`",
                            intern::resolve(*key),
                            intern::resolve(graph.package(*first).name),
                            intern::resolve(graph.package(*dep).name),
                        ),
                    )
                    .with_label(
                        *first_span,
                        format!(
                            "`{}` is `{}` here",
                            intern::resolve(*key),
                            intern::resolve(graph.package(*first).name)
                        ),
                    )
                    .with_help("give one of the two dependencies another key"),
                ),
                None => {
                    named.insert(*key, (*dep, *span));
                    roots
                        .entry(*key)
                        .or_insert_with(|| graph.package(*dep).src.clone());
                }
            }
        }
    }
    if !diagnostics.is_empty() {
        die_on_manifest_error(PackageFailure {
            sources,
            diagnostics,
        });
    }
    (root_node.name, roots)
}

/// Construct the no-package fallback: synthetic local package name
/// mapped to `dir` so ad-hoc scripts resolve `import foo` against
/// sibling files.
fn fallback_package_setup(dir: &Path) -> (Symbol, HashMap<Symbol, PathBuf>) {
    let local = intern::intern(ANONYMOUS_LOCAL_PACKAGE);
    let mut roots = HashMap::new();
    roots.insert(local, dir.to_path_buf());
    (local, roots)
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
