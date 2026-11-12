//! The packages of a program, as the session sees them: the package
//! graph of the project (`package_graph::resolve_packages`), or one
//! unnamed package for a script outside any project.

use std::path::PathBuf;

use crate::diagnostic::{Code, Diagnostic};
use crate::intern::{Symbol, intern};
use crate::manifest::Manifest;
use crate::package_graph::{LockChange, PackageId, resolve_packages};
use crate::source::{FileId, SourceMap, SourceName, Span};

pub use crate::package_graph::LockPolicy;

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

/// One package of a program.
#[derive(Debug, Clone)]
pub struct Package {
    pub id: PackageId,
    /// `[package].name`, unique in a graph; `__local__` for a script.
    pub name: Symbol,
    /// The directory of its modules; `None` when there is no project.
    pub src: Option<PathBuf>,
    /// Its `silt.toml`; `None` for the unnamed package of a script.
    pub manifest: Option<FileId>,
    /// Each key of its `[dependencies]`, the package it names, and the
    /// span of the key in its `silt.toml`.
    pub deps: Vec<(Symbol, PackageId, Span)>,
}

/// The packages of a program.
#[derive(Debug, Clone)]
pub struct Packages {
    /// Indexed by `PackageId`.
    pub packages: Vec<Package>,
    /// The package the entry files belong to.
    pub root: PackageId,
    /// What resolving did to `silt.lock`.
    pub lock: LockChange,
}

impl Packages {
    pub fn package(&self, id: PackageId) -> &Package {
        &self.packages[id.0 as usize]
    }

    /// The package `key` names in `from`'s `[dependencies]`.
    pub fn dependency(&self, from: PackageId, key: Symbol) -> Option<PackageId> {
        self.package(from)
            .deps
            .iter()
            .find(|(k, _, _)| *k == key)
            .map(|(_, id, _)| *id)
    }

    /// One unnamed package whose modules are in `src`.
    pub fn unnamed(src: Option<PathBuf>) -> Packages {
        Packages {
            packages: vec![Package {
                id: PackageId(0),
                name: intern(UNNAMED_PACKAGE),
                src,
                manifest: None,
                deps: Vec::new(),
            }],
            root: PackageId(0),
            lock: LockChange::Unchanged,
        }
    }
}

/// The name of the package of a file outside any project. Package names
/// are validated against `[a-z][a-z0-9_-]*`, so it cannot collide.
const UNNAMED_PACKAGE: &str = "__local__";

/// The packages of `project`: its manifests and lockfile are read once
/// (manifests and lockfile are registered in `sources`, where the
/// diagnostics point), and the lockfile is rewritten when `lock` allows
/// it and it no longer pins the graph.
pub(super) fn project_packages(
    project: &ProjectSetup,
    lock: LockPolicy,
    sources: &mut SourceMap,
) -> Result<Packages, Vec<Diagnostic>> {
    let dir = match project {
        ProjectSetup::Discover(dir) => dir,
        ProjectSetup::Script(dir) => return Ok(Packages::unnamed(Some(dir.clone()))),
        ProjectSetup::None => return Ok(Packages::unnamed(None)),
    };
    let Some(root) = Manifest::find(dir) else {
        return Ok(Packages::unnamed(Some(dir.clone())));
    };
    let graph = resolve_packages(&root, lock, sources)?;
    Ok(Packages {
        packages: graph
            .packages
            .iter()
            .map(|node| Package {
                id: node.id,
                name: node.name,
                src: Some(node.src.clone()),
                manifest: Some(node.manifest),
                deps: node.deps.clone(),
            })
            .collect(),
        root: graph.root,
        lock: graph.lock,
    })
}

/// An error for each module file of a project package (one with a
/// manifest) whose name is no module name: one named like a builtin
/// module (`import list` always means the builtin `list`, so
/// `src/list.silt` could never be imported), and one that is not
/// lowercase letters, digits and `_` ([`crate::module::is_module_name`]).
/// The file is registered in `sources`, and the error is at its start.
pub(super) fn modules_named_like_builtins(
    packages: &Packages,
    sources: &mut SourceMap,
) -> Vec<Diagnostic> {
    let mut errors = Vec::new();
    for package in packages.packages.iter().filter(|p| p.manifest.is_some()) {
        let Some(src) = &package.src else {
            continue;
        };
        let Ok(entries) = std::fs::read_dir(src) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.extension().is_some_and(|ext| ext == "silt")
                    && p.file_stem()
                        .and_then(|stem| stem.to_str())
                        .is_some_and(|stem| {
                            crate::module::is_builtin_module(stem)
                                || !crate::module::is_module_name(stem)
                        })
            })
            .collect();
        paths.sort();
        for path in paths {
            let name = path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default();
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let file = sources.add(SourceName::Path(path.clone()), text.into());
            errors.push(file_name_problem(&name, file));
        }
    }
    errors
}

/// The error at the start of the module file `file`, named `name`, which
/// is no module name. A file named like a builtin module could never be
/// imported (`import list` always means the builtin `list`), nor one
/// named like a keyword (`import match` cannot parse); any other name
/// must be lowercase letters, digits and `_`, not starting with a digit.
/// The help names a module name to use, when there is one
/// ([`crate::module::suggested_module_name`]). A file named `.silt` has
/// the empty name.
pub(super) fn file_name_problem(name: &str, file: FileId) -> Diagnostic {
    if name.is_empty() {
        return Diagnostic::error(
            Code::ModuleNamedLikeBuiltin,
            Span::point(file, 0),
            "the file `.silt` has no module name: use lowercase letters, digits and `_`, \
             not starting with a digit",
        );
    }
    if crate::manifest::is_reserved_keyword(name) {
        return Diagnostic::error(
            Code::ModuleNamedLikeBuiltin,
            Span::point(file, 0),
            format!("the module `{name}` cannot be imported: `{name}` is a keyword"),
        )
        .with_help(format!("rename `{name}.silt`"));
    }
    if crate::module::is_builtin_module(name) {
        return Diagnostic::error(
            Code::ModuleNamedLikeBuiltin,
            Span::point(file, 0),
            format!(
                "the module `{name}` cannot be imported: `import {name}` names the \
                 builtin module `{name}`"
            ),
        )
        .with_help(format!("rename `{name}.silt`"));
    }
    let d = Diagnostic::error(
        Code::ModuleNamedLikeBuiltin,
        Span::point(file, 0),
        crate::manifest::invalid_module_name(name),
    );
    match crate::module::suggested_module_name(name) {
        None => d,
        Some(suggestion) => d.with_help(format!(
            "rename `{}.silt` to `{suggestion}.silt`",
            crate::git::escape_for_display(name)
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The help names a module name to rename the file to when there is
    /// one: lowercase, with `-`, ` ` and `.` as `_`. A name with a
    /// control character, a leading digit, a letter outside a-z, or whose
    /// lowercase form is a builtin module's has no such help, and the
    /// message shows the name by the display rule.
    #[test]
    fn the_rename_help_of_a_module_file_names_a_module_name() {
        let help = |name: &str| file_name_problem(name, FileId::default()).help.join("\n");
        assert_eq!(help("My-File"), "rename `My-File.silt` to `my_file.silt`");
        assert_eq!(help("a.test"), "rename `a.test.silt` to `a_test.silt`");
        for name in ["t15_\u{1b}[2J\rName", "2nd", "Мир", "String", "Match"] {
            assert_eq!(help(name), "", "{name:?}");
        }
        let d = file_name_problem("t15_\u{1b}[2J\rName", FileId::default());
        assert!(!d.message.contains(['\u{1b}', '\r']), "{:?}", d.message);
    }
}
