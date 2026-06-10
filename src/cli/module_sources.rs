//! Utilities for mapping bare function names to the imported-module
//! source files they came from. Used by `silt run` and `silt test` to
//! render runtime errors against the correct source when the error
//! bubbles out of an imported module.

use std::fs;
use std::path::{Path, PathBuf};

/// Build a map from bare top-level function name → (file_path, source text)
/// for every module file that `main_path` transitively imports.
///
/// We scan `main_source` (and each imported module's source) for
/// `import <name>` statements, resolve them the same way the compiler
/// does (see `Compiler::resolve_import` in src/compiler/mod.rs), and
/// record each top-level `fn <name>` / `pub fn <name>` we find in the
/// resulting module file. Resolution rules, mirrored from the compiler:
///
///   - If the import segment names a package from the manifest/lockfile
///     dependency graph (path dep or git dep), and it is not the
///     current package importing a module that shadows its own name,
///     the import is cross-package: the source is the dep's
///     `<dep_src>/lib.silt`, and the dep's OWN imports resolve relative
///     to the dep's source root (so nested dep-sibling modules map to
///     the dep's files, not the consumer's).
///   - Otherwise the import is local: `<pkg_src>/<name>.silt` relative
///     to whichever package the importing file belongs to.
///
/// Round 93: before the dep channel existed here, runtime errors inside
/// package-dependency code fell back to the entry file's source and
/// rendered "division by zero --> main.silt:<dep line>:<dep col>" with
/// a caret on an unrelated line. Lock:
/// tests/round93_dep_error_attribution_tests.rs.
///
/// This is a *best-effort* mapping used solely to improve runtime-error
/// rendering when an error propagates out of an imported module. Name
/// collisions are handled by *exclusion*, not by winner-takes-all:
///
///   1. If a function name is ALSO defined at the top level of the main
///      source file, it is excluded from the map. The renderer then falls
///      back to the main source — which is correct, because the VM's
///      innermost frame name cannot distinguish `main::foo` from
///      `mod::foo`, and the main file is the safer guess.
///   2. If a function name appears in MORE THAN ONE imported module, it
///      is likewise excluded — we have no way to pick the right module.
///
/// In both cases a map miss causes the renderer to fall back to the main
/// source, which is the safe default: at worst the rendered snippet
/// points at main's line N, which is typically close to the call site
/// that invoked the module function.
///
/// See E1 in the audit for the original gap (runtime errors from module
/// code rendered against the main file), and the follow-up collision
/// case (`test_module_runtime_error_with_name_collision_renders_correct_file`)
/// which motivated the exclusion strategy here.
pub(crate) fn collect_module_function_sources(
    main_path: &str,
    main_source: &str,
) -> std::collections::HashMap<String, (PathBuf, String)> {
    use std::collections::{HashMap, HashSet};

    let mut out: HashMap<String, (PathBuf, String)> = HashMap::new();
    let project_root: PathBuf = Path::new(main_path)
        .canonicalize()
        .unwrap_or_else(|_| Path::new(main_path).to_path_buf())
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));

    // Manifest/lockfile dependency channel (round 93): map package
    // names from the dep graph to their source roots so imports of
    // path/git deps resolve to the dep's `lib.silt` instead of falling
    // through to a (nonexistent) sibling file. Best-effort: any
    // manifest/lockfile problem degrades to the sibling-only behavior.
    let (local_pkg_name, package_roots) = collect_package_roots(&project_root);

    // Names defined at the top level of the main source file. Any module
    // function sharing one of these names is ambiguous w.r.t. the VM's
    // bare-name call frame, so we exclude it from the map and let the
    // renderer fall back to the main source.
    let main_fn_names: HashSet<String> = extract_top_level_fn_names(main_source)
        .into_iter()
        .collect();

    // First pass: walk the import graph, recording every (fn_name,
    // module_file, module_source) tuple we encounter. We can't decide
    // inclusion until we've seen the full graph — a name that appears in
    // one module might also appear in another, in which case it must be
    // excluded from the final map.
    let mut candidates: Vec<(String, PathBuf, String)> = Vec::new();
    let mut name_module_count: HashMap<String, usize> = HashMap::new();

    // BFS from main source: scan import statements, load each module file,
    // repeat for transitive imports. Each queue item carries the package
    // context (source root + package name) the file belongs to, so a
    // dep's own imports resolve against the DEP's source root — mirroring
    // the compiler's `compiling_package_stack`. The entry file's package
    // context is `None`, matching `Compiler::current_package()` returning
    // `None` while the stack is empty (so even an import that shadows the
    // local package's own name resolves cross-package at entry level,
    // exactly like `Compiler::resolve_import`).
    struct Pending {
        source: String,
        pkg_root: PathBuf,
        pkg_name: Option<String>,
    }
    let mut queue: Vec<Pending> = vec![Pending {
        source: main_source.to_string(),
        pkg_root: project_root.clone(),
        pkg_name: None,
    }];
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(main_path.to_string());

    while let Some(cur) = queue.pop() {
        for import_name in extract_imports(&cur.source) {
            // Skip builtin modules — they're not file-backed.
            if silt::module::is_builtin_module(&import_name) {
                continue;
            }
            // Cross-package import? Mirrors `Compiler::resolve_import`:
            // a segment matching a registered package resolves to the
            // dep's `lib.silt`, UNLESS the current package is importing
            // a module that happens to share its own package name (a
            // local self-import).
            let dep_root = match package_roots.get(&import_name) {
                Some(root) if cur.pkg_name.as_deref() != Some(import_name.as_str()) => {
                    Some(root.clone())
                }
                _ => None,
            };
            let (file_path, child_root, child_pkg) = match dep_root {
                Some(dep_src) => (
                    dep_src.join("lib.silt"),
                    dep_src.clone(),
                    Some(import_name.clone()),
                ),
                None => (
                    cur.pkg_root.join(format!("{import_name}.silt")),
                    cur.pkg_root.clone(),
                    // Local import: the module belongs to whichever
                    // package the importer belongs to (entry-level
                    // imports belong to the local package).
                    cur.pkg_name.clone().or_else(|| local_pkg_name.clone()),
                ),
            };
            let file_key = file_path.display().to_string();
            if !seen.insert(file_key.clone()) {
                continue;
            }
            let Ok(mod_source) = fs::read_to_string(&file_path) else {
                continue;
            };
            // Per-module dedupe: a function name appearing twice in the
            // SAME file still counts as a single module for collision
            // purposes.
            let mut local_names: HashSet<String> = HashSet::new();
            for fn_name in extract_top_level_fn_names(&mod_source) {
                if local_names.insert(fn_name.clone()) {
                    *name_module_count.entry(fn_name.clone()).or_insert(0) += 1;
                    candidates.push((fn_name, file_path.clone(), mod_source.clone()));
                }
            }
            // Register the synthetic module-init frame name so that
            // top-level errors (e.g. `pub let x = 1 / 0`) can be
            // resolved to the module's source file.
            let init_key = format!("<module:{import_name}>");
            out.insert(init_key, (file_path.clone(), mod_source.clone()));

            queue.push(Pending {
                source: mod_source,
                pkg_root: child_root,
                pkg_name: child_pkg,
            });
        }
    }

    // Second pass: build the final map, excluding any name that either
    // collides with main or is defined in more than one module.
    for (fn_name, file_path, mod_source) in candidates {
        if main_fn_names.contains(&fn_name) {
            continue;
        }
        if name_module_count.get(&fn_name).copied().unwrap_or(0) > 1 {
            continue;
        }
        // At this point the name is unique to a single module and not
        // shadowed by the main file, so recording it is unambiguous.
        out.entry(fn_name).or_insert((file_path, mod_source));
    }
    out
}

/// Best-effort mirror of the package-roots derivation the compile
/// pipeline performs via `cli::package::package_setup_for_file` +
/// `Lockfile::package_roots`, restricted to read-only operations so an
/// error-rendering helper can never mutate the lockfile, hit the
/// network, or exit the process.
///
/// Returns `(local_package_name, package_name → src_dir)`. Both path
/// deps and git deps are covered: `Lockfile::package_roots` resolves a
/// path dep to `<dep>/src` and a git dep to its content-addressed cache
/// checkout's `src/` (the checkout already exists by the time a runtime
/// error can occur — the compile pipeline populated it).
///
/// Degrades gracefully: no manifest above `entry_dir` (ad-hoc script),
/// unreadable manifest, or missing/corrupt lockfile all yield an empty
/// map, which reproduces the legacy sibling-only resolution. By the
/// time this runs, `silt run` / `silt test` have already auto-updated
/// the lockfile, so the load path is the one that executes in practice.
fn collect_package_roots(
    entry_dir: &Path,
) -> (Option<String>, std::collections::HashMap<String, PathBuf>) {
    use std::collections::HashMap;

    let mut roots: HashMap<String, PathBuf> = HashMap::new();
    let Some(manifest_dir) = silt::manifest::Manifest::find(entry_dir) else {
        return (None, roots);
    };
    let Ok(manifest) = silt::manifest::Manifest::load(&manifest_dir.join("silt.toml")) else {
        return (None, roots);
    };
    let local_name = silt::intern::resolve(manifest.package.name);
    let Ok(lockfile) = silt::lockfile::Lockfile::load(&manifest_dir.join("silt.lock")) else {
        // No (or unreadable) lockfile: still register the local
        // package's own root so self-name imports keep working, but
        // skip dep mapping.
        roots.insert(local_name.clone(), entry_dir.to_path_buf());
        return (Some(local_name), roots);
    };
    for (sym, path) in lockfile.package_roots(&manifest) {
        roots.insert(silt::intern::resolve(sym), path);
    }
    (Some(local_name), roots)
}

/// Extract the bare module names referenced by `import <name>` statements
/// in `source`. Supports both `import foo` and `import foo.{ Bar, baz }`
/// forms — we just need the module name, not the item list.
fn extract_imports(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw_line in source.lines() {
        let line = raw_line.trim_start();
        let Some(rest) = line.strip_prefix("import ") else {
            continue;
        };
        // Module name runs to the first `.`, whitespace, `{`, or `as`.
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() {
            out.push(name);
        }
    }
    out
}

/// Extract the names of top-level `fn <name>` (optionally `pub fn`)
/// declarations in `source`. This is a purely textual scan — we only
/// need it to correlate a runtime frame's function name with a module
/// file, so missing an edge case (e.g. an `fn` inside a multi-line
/// comment) just means falling back to the main file for rendering.
fn extract_top_level_fn_names(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw_line in source.lines() {
        let line = raw_line.trim_start();
        let rest = match line.strip_prefix("pub fn ") {
            Some(r) => r,
            None => match line.strip_prefix("fn ") {
                Some(r) => r,
                None => continue,
            },
        };
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() {
            out.push(name);
        }
    }
    out
}
