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
/// `import <name>` statements, resolve them relative to the main file's
/// project root, and record each top-level `fn <name>` / `pub fn <name>`
/// we find in the resulting module file.
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
    // repeat for transitive imports.
    let mut queue: Vec<(String, String)> = vec![(main_path.to_string(), main_source.to_string())];
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(main_path.to_string());

    while let Some((_cur_path, cur_source)) = queue.pop() {
        for import_name in extract_imports(&cur_source) {
            // Skip builtin modules — they're not file-backed.
            if silt::module::is_builtin_module(&import_name) {
                continue;
            }
            let file_path = project_root.join(format!("{import_name}.silt"));
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

            queue.push((file_key, mod_source));
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
