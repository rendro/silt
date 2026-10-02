//! The package graph: every package a project depends on, resolved from
//! the manifests.
//!
//! # API
//!
//! ```text
//! pub struct PackageId(pub u32);
//! pub struct PackageNode {
//!     pub id: PackageId,
//!     pub name: Symbol,                         // [package].name, display only
//!     pub root: PathBuf,                        // package root (dir of its silt.toml), canonical
//!     pub src: PathBuf,                         // root/src
//!     pub manifest: FileId,                     // its silt.toml (SourceName::Manifest)
//!     pub deps: Vec<(Symbol, PackageId, Span)>, // key in THIS package's [dependencies] -> package;
//!                                               // span of the key in silt.toml
//! }
//! pub struct PackageGraph { pub root: PackageId, pub packages: Vec<PackageNode>, pub lock: LockChange }
//! pub enum LockPolicy { Update, ReadOnly, Refresh }
//! pub enum LockChange { Unchanged, Created, Updated }
//! pub fn resolve_packages(project_root: &Path, policy: LockPolicy, sources: &mut SourceMap)
//!     -> Result<PackageGraph, Vec<Diagnostic>>;
//! impl PackageGraph {
//!     pub fn package(&self, id: PackageId) -> &PackageNode;
//!     pub fn dependency(&self, from: PackageId, key: Symbol) -> Option<PackageId>;
//! }
//! ```
//!
//! `packages[id.0]` is the node of `id`; the root is `packages[0]`, and
//! the others follow in breadth-first order from it.
//!
//! # Rules
//!
//! - A package is identified by its source: the canonical path of its
//!   root for a path dependency, the URL and the full commit id for a git
//!   dependency. Two dependency keys that point at one source give one
//!   `PackageId`.
//! - The `[dependencies]` key is the name the importing package knows the
//!   dependency by (`import <key>` loads that package's `src/lib.silt`).
//!   A dependency's own `[package].name` is for display and the lockfile.
//! - Two different sources with the same `[package].name` in one graph
//!   are `DuplicatePackage`, with both dependency chains as labels.
//! - A dependency key equal to a module of the same package
//!   (`src/<key>.silt`) is `DependencyKeyCollision`.
//! - A relative path, and a git URL that is a relative local path (`./x`,
//!   `../x`), resolve against the directory of the manifest that names
//!   them, never against the working directory.
//! - The graph is built from the manifests. The lockfile only supplies
//!   the commit a git dependency was pinned to, and only when its URL and
//!   ref are the manifest's; any other entry is stale. Under
//!   [`LockPolicy::Update`] a stale lock is rewritten; under
//!   [`LockPolicy::ReadOnly`] a stale entry is `LockfileStale`. The lock
//!   lives next to the project's `silt.toml`.
//! - A symbolic link in a dependency's `silt.toml` or `src/` is
//!   `SymlinkInDependency`; a git checkout that holds one is not cached.
//!
//! Every problem is a [`Diagnostic`] in the `silt.toml` (or `silt.lock`)
//! it is about, a dependency's own manifest included. Nothing here
//! prints.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use crate::diagnostic::{Code, Diagnostic};
use crate::git::{self, GitError, GitRef, escape_for_display};
use crate::intern::{self, Symbol};
use crate::lockfile::{ChecksumError, LockedPackage, LockedSource, Lockfile, normalize_path};
use crate::manifest::{Dependency, DependencyEntry, Manifest, display_path};
use crate::source::{FileId, SourceMap, Span};

/// One resolved package source.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct PackageId(pub u32);

#[derive(Debug, Clone)]
pub struct PackageNode {
    pub id: PackageId,
    /// `[package].name`: for display and the lockfile only.
    pub name: Symbol,
    /// The package root, the directory of its `silt.toml`; canonical.
    pub root: PathBuf,
    /// `root/src`, where its modules are.
    pub src: PathBuf,
    /// Its `silt.toml` in the source map.
    pub manifest: FileId,
    /// Each key of this package's `[dependencies]`, the package it
    /// resolves to, and the span of the key in `silt.toml`.
    pub deps: Vec<(Symbol, PackageId, Span)>,
}

#[derive(Debug, Clone)]
pub struct PackageGraph {
    pub root: PackageId,
    /// Indexed by `PackageId`; the root first.
    pub packages: Vec<PackageNode>,
    /// What happened to `silt.lock`.
    pub lock: LockChange,
}

impl PackageGraph {
    pub fn package(&self, id: PackageId) -> &PackageNode {
        &self.packages[id.0 as usize]
    }

    /// The package that `key` in `from`'s `[dependencies]` resolves to.
    pub fn dependency(&self, from: PackageId, key: Symbol) -> Option<PackageId> {
        self.package(from)
            .deps
            .iter()
            .find(|(k, _, _)| *k == key)
            .map(|(_, id, _)| *id)
    }
}

/// How `silt.lock` is used.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum LockPolicy {
    /// `run`, `check`, `test`, `silt add`: reuse the lock's pins that are
    /// still the manifest's, resolve the rest, and rewrite the lock when
    /// it changed.
    Update,
    /// `fmt`, `disasm`, the LSP: never write. A lock entry that is not
    /// what the manifest resolves to is an error; with no lock at all
    /// the graph is resolved in memory.
    ReadOnly,
    /// `silt update`: ignore the lock's pins, resolve every branch and
    /// tag again, and write the lock.
    Refresh,
}

/// What resolving did to `silt.lock`.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum LockChange {
    /// Not written: it already pinned the graph, or the policy is
    /// [`LockPolicy::ReadOnly`].
    Unchanged,
    /// Written where there was none.
    Created,
    /// Rewritten.
    Updated,
}

/// Resolve the package graph of the project whose `silt.toml` is in
/// `project_root`, adding every manifest (and the lockfile) to
/// `sources`.
pub fn resolve_packages(
    project_root: &Path,
    policy: LockPolicy,
    sources: &mut SourceMap,
) -> Result<PackageGraph, Vec<Diagnostic>> {
    let root_dir = canonical(project_root);
    let manifest = Manifest::load(&root_dir.join("silt.toml"), sources).map_err(|d| vec![d])?;
    let lock_path = root_dir.join("silt.lock");
    let existing = match policy {
        LockPolicy::Refresh => None,
        LockPolicy::Update | LockPolicy::ReadOnly => {
            Lockfile::load(&lock_path, sources).map_err(|d| vec![d])?
        }
    };

    let mut resolver = Resolver {
        sources,
        policy,
        existing,
        nodes: Vec::new(),
        manifests: Vec::new(),
        locked: Vec::new(),
        via: Vec::new(),
        by_source: HashMap::new(),
        failed: HashSet::new(),
        diagnostics: Vec::new(),
        queue: VecDeque::new(),
    };
    let root = resolver.add(
        SourceKey::Path(root_dir.clone()),
        root_dir,
        manifest,
        LockedSource::Local,
        String::new(),
        None,
    );
    while let Some(id) = resolver.queue.pop_front() {
        let entries: Vec<(Symbol, DependencyEntry)> = resolver.manifests[id.0 as usize]
            .dependencies
            .iter()
            .map(|(k, e)| (*k, e.clone()))
            .collect();
        for (key, entry) in entries {
            if let Some(dep) = resolver.resolve_dependency(id, key, &entry) {
                resolver.nodes[id.0 as usize]
                    .deps
                    .push((key, dep, entry.key));
            }
        }
    }
    resolver.check_duplicate_names();
    resolver.check_key_collisions();
    if !resolver.diagnostics.is_empty() {
        return Err(resolver.diagnostics);
    }

    let fresh = resolver.lockfile();
    let lock = match (policy, &resolver.existing) {
        (LockPolicy::ReadOnly, _) => LockChange::Unchanged,
        (LockPolicy::Update, Some(old)) if old.same_pins(&fresh) => LockChange::Unchanged,
        (_, existing) => {
            let created = existing.is_none() && !lock_path.exists();
            if let Err(e) = fresh.write(&lock_path) {
                let root_manifest = resolver.nodes[root.0 as usize].manifest;
                return Err(vec![Diagnostic::error(
                    Code::PackageIo,
                    Span::point(root_manifest, 0),
                    format!(
                        "cannot write {}: {}",
                        escape_for_display(&display_path(&lock_path).display().to_string()),
                        escape_for_display(&e.to_string())
                    ),
                )]);
            }
            if created {
                LockChange::Created
            } else {
                LockChange::Updated
            }
        }
    };
    Ok(PackageGraph {
        root,
        packages: resolver.nodes,
        lock,
    })
}

/// What identifies a package.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
enum SourceKey {
    /// The canonical path of the package root.
    Path(PathBuf),
    /// The URL (a local one made absolute) and the full commit id.
    Git(String, String),
}

struct Resolver<'a> {
    sources: &'a mut SourceMap,
    policy: LockPolicy,
    existing: Option<Lockfile>,
    /// Indexed by `PackageId`, as are the three below.
    nodes: Vec<PackageNode>,
    manifests: Vec<Manifest>,
    /// The lock entry of each package, without its name and version.
    locked: Vec<(LockedSource, String)>,
    /// The dependency that first reached each package: the package that
    /// declares it and the span of its key. `None` for the root.
    via: Vec<Option<(PackageId, Span)>>,
    by_source: HashMap<SourceKey, PackageId>,
    /// Sources whose manifest or sources failed already: reported once.
    failed: HashSet<SourceKey>,
    diagnostics: Vec<Diagnostic>,
    queue: VecDeque<PackageId>,
}

impl Resolver<'_> {
    fn add(
        &mut self,
        key: SourceKey,
        root: PathBuf,
        manifest: Manifest,
        locked: LockedSource,
        checksum: String,
        via: Option<(PackageId, Span)>,
    ) -> PackageId {
        let id = PackageId(self.nodes.len() as u32);
        self.nodes.push(PackageNode {
            id,
            name: manifest.package.name,
            src: root.join("src"),
            root,
            manifest: manifest.file,
            deps: Vec::new(),
        });
        self.manifests.push(manifest);
        self.locked.push((locked, checksum));
        self.via.push(via);
        self.by_source.insert(key, id);
        self.queue.push_back(id);
        id
    }

    /// The package `entry` (the dependency `key` of `from`) resolves to,
    /// or `None` after reporting why there is none.
    fn resolve_dependency(
        &mut self,
        from: PackageId,
        key: Symbol,
        entry: &DependencyEntry,
    ) -> Option<PackageId> {
        let base = self.nodes[from.0 as usize].root.clone();
        let key_name = intern::resolve(key);
        match &entry.source {
            Dependency::Path { path } => {
                let dir = normalize_path(&base.join(path));
                let shown = escape_for_display(&path.display().to_string());
                if !dir.exists() {
                    self.diagnostics.push(
                        Diagnostic::error(
                            Code::DependencyNotFound,
                            entry.value,
                            format!("dependency `{key_name}`: path `{shown}` does not exist"),
                        )
                        .with_note(format!(
                            "the path is relative to the directory of this silt.toml and \
                             resolves to {}",
                            escape_for_display(&display_path(&dir).display().to_string())
                        )),
                    );
                    return None;
                }
                // Followed: a link here is rejected with the package's
                // other files, by the checksum walk.
                if !dir.join("silt.toml").is_file() {
                    self.diagnostics.push(Diagnostic::error(
                        Code::DependencyNotPackage,
                        entry.value,
                        format!(
                            "dependency `{key_name}`: `{shown}` is not a silt package \
                             (no silt.toml found)"
                        ),
                    ));
                    return None;
                }
                let root = canonical(&dir);
                let locked = LockedSource::Path { path: root.clone() };
                self.enter(
                    SourceKey::Path(root.clone()),
                    root,
                    locked,
                    from,
                    &key_name,
                    entry,
                )
            }
            Dependency::Git { url, ref_spec } => {
                let url = resolve_git_url(url, &base);
                let commit = match self.pinned_commit(&url, ref_spec) {
                    Some(commit) => commit,
                    None if self.policy == LockPolicy::ReadOnly && self.existing.is_some() => {
                        self.diagnostics.push(stale_diagnostic(
                            entry.value,
                            &key_name,
                            "it has no pin for this source",
                        ));
                        return None;
                    }
                    None => match git::resolve_ref(&url, ref_spec) {
                        Ok(commit) => commit,
                        Err(e) => {
                            self.diagnostics.push(git_diagnostic(
                                entry.value,
                                &key_name,
                                &url,
                                ref_spec,
                                &e,
                            ));
                            return None;
                        }
                    },
                };
                let root = match git::fetch_to_cache(&url, &commit) {
                    Ok(root) => canonical(&root),
                    Err(e) => {
                        let d = match &e {
                            GitError::Symlink { path } => {
                                symlink_diagnostic(entry.value, &key_name, path)
                            }
                            _ => git_diagnostic(entry.value, &key_name, &url, ref_spec, &e),
                        };
                        self.diagnostics.push(d);
                        return None;
                    }
                };
                // A rev is pinned as the full commit it resolved to; the
                // lock writes it as `rev` alone.
                let ref_spec = match ref_spec {
                    GitRef::Rev(_) => GitRef::Rev(commit.clone()),
                    other => other.clone(),
                };
                let locked = LockedSource::Git {
                    url: url.clone(),
                    ref_spec,
                    resolved_sha: commit.clone(),
                };
                self.enter(
                    SourceKey::Git(url, commit),
                    root,
                    locked,
                    from,
                    &key_name,
                    entry,
                )
            }
        }
    }

    /// The package at `root`, entering it into the graph the first time
    /// its source is met.
    fn enter(
        &mut self,
        source: SourceKey,
        root: PathBuf,
        locked: LockedSource,
        from: PackageId,
        key_name: &str,
        entry: &DependencyEntry,
    ) -> Option<PackageId> {
        if let Some(id) = self.by_source.get(&source) {
            return Some(*id);
        }
        if self.failed.contains(&source) {
            return None;
        }
        let checksum = match crate::lockfile::checksum_path_source(&root) {
            Ok(checksum) => checksum,
            Err(ChecksumError::Symlink(path)) => {
                self.diagnostics
                    .push(symlink_diagnostic(entry.value, key_name, &path));
                self.failed.insert(source);
                return None;
            }
            Err(ChecksumError::Io(e)) => {
                self.diagnostics.push(Diagnostic::error(
                    Code::PackageIo,
                    entry.value,
                    format!(
                        "dependency `{key_name}`: cannot read its sources: {}",
                        escape_for_display(&e.to_string())
                    ),
                ));
                self.failed.insert(source);
                return None;
            }
        };
        let manifest = match Manifest::load(&root.join("silt.toml"), self.sources) {
            Ok(manifest) => manifest,
            Err(d) => {
                self.diagnostics.push(d);
                self.failed.insert(source);
                return None;
            }
        };
        if self.policy == LockPolicy::ReadOnly
            && let Some(lock) = &self.existing
        {
            let name = intern::resolve(manifest.package.name);
            let entries: Vec<&LockedPackage> =
                lock.packages.iter().filter(|p| p.name == name).collect();
            if !entries.iter().any(|p| p.source == locked) {
                let reason = if entries.is_empty() {
                    "it has no pin for this source"
                } else {
                    "it pins another source than this one"
                };
                self.diagnostics
                    .push(stale_diagnostic(entry.value, key_name, reason));
                self.failed.insert(source);
                return None;
            }
        }
        Some(self.add(
            source,
            root,
            manifest,
            locked,
            checksum,
            Some((from, entry.key)),
        ))
    }

    /// The commit the existing lock pins `url` at `ref_spec` to, when the
    /// lock has an entry with that URL and a matching ref.
    fn pinned_commit(&self, url: &str, ref_spec: &GitRef) -> Option<String> {
        let lock = self.existing.as_ref()?;
        lock.packages.iter().find_map(|p| match &p.source {
            LockedSource::Git {
                url: locked_url,
                ref_spec: locked_ref,
                resolved_sha,
            } if locked_url == url && same_ref(ref_spec, locked_ref) => Some(resolved_sha.clone()),
            _ => None,
        })
    }

    /// `DuplicatePackage` for every name that two sources have.
    fn check_duplicate_names(&mut self) {
        let mut by_name: HashMap<Symbol, Vec<PackageId>> = HashMap::new();
        for node in &self.nodes {
            by_name.entry(node.name).or_default().push(node.id);
        }
        let mut names: Vec<_> = by_name
            .into_iter()
            .filter(|(_, ids)| ids.len() > 1)
            .collect();
        names.sort_by_key(|(_, ids)| ids[1]);
        for (name, ids) in names {
            let name = intern::resolve(name);
            // The last is never the root, which is the first package.
            let last = *ids.last().expect("two or more");
            let (_, at) = self.via[last.0 as usize].expect("only the root has no parent");
            let mut d = Diagnostic::error(
                Code::DuplicatePackage,
                at,
                format!(
                    "{} different packages are named `{name}`",
                    count_word(ids.len())
                ),
            );
            for &id in &ids {
                // The last key of the last chain is the primary span.
                d.labels
                    .extend(self.chain(id).into_iter().filter(|(span, _)| *span != at));
                d.notes.push(format!("`{name}` from {}", self.describe(id)));
            }
            d.help.push(format!(
                "a graph holds one package per name: make every dependency on `{name}` \
                 point at the same source"
            ));
            self.diagnostics.push(d);
        }
    }

    /// The dependency chain from the root to `id`, as labels on the keys
    /// that make it up; for the root, its name.
    fn chain(&self, id: PackageId) -> Vec<(Span, String)> {
        let mut labels = Vec::new();
        let mut at = id;
        while let Some((parent, span)) = self.via[at.0 as usize] {
            labels.push((
                span,
                format!(
                    "`{}` depends on `{}` here",
                    intern::resolve(self.nodes[parent.0 as usize].name),
                    intern::resolve(self.nodes[at.0 as usize].name),
                ),
            ));
            at = parent;
        }
        if labels.is_empty() {
            let manifest = &self.manifests[id.0 as usize];
            labels.push((manifest.package.name_span, "the package itself".to_string()));
        }
        labels.reverse();
        labels
    }

    /// Where the package `id` comes from, for a note.
    fn describe(&self, id: PackageId) -> String {
        match &self.locked[id.0 as usize].0 {
            LockedSource::Local => format!(
                "{} (this package)",
                escape_for_display(
                    &display_path(&self.nodes[id.0 as usize].root)
                        .display()
                        .to_string()
                )
            ),
            LockedSource::Path { path } => {
                escape_for_display(&display_path(path).display().to_string())
            }
            LockedSource::Git {
                url, resolved_sha, ..
            } => format!("{} at {resolved_sha}", escape_for_display(url)),
        }
    }

    /// `DependencyKeyCollision` for every key that is also a module of
    /// the package that declares it.
    fn check_key_collisions(&mut self) {
        for node in &self.nodes {
            for (key, _, span) in &node.deps {
                let key = intern::resolve(*key);
                if node.src.join(format!("{key}.silt")).is_file() {
                    self.diagnostics.push(
                        Diagnostic::error(
                            Code::DependencyKeyCollision,
                            *span,
                            format!(
                                "dependency key `{key}` is also the name of the module \
                                 `src/{key}.silt`"
                            ),
                        )
                        .with_help(format!(
                            "`import {key}` must mean one thing: rename the dependency key \
                             or the module"
                        )),
                    );
                }
            }
        }
    }

    /// The lockfile that pins this graph.
    fn lockfile(&self) -> Lockfile {
        let mut packages: Vec<LockedPackage> = self
            .nodes
            .iter()
            .zip(&self.manifests)
            .zip(&self.locked)
            .map(|((node, manifest), (source, checksum))| LockedPackage {
                name: intern::resolve(node.name),
                version: manifest.package.version.clone(),
                source: source.clone(),
                checksum: checksum.clone(),
            })
            .collect();
        packages.sort_by(|a, b| a.name.cmp(&b.name));
        Lockfile {
            version: 1,
            packages,
        }
    }
}

/// `n` as a word for a message: "two", "three", ... .
fn count_word(n: usize) -> String {
    match n {
        2 => "two".into(),
        3 => "three".into(),
        4 => "four".into(),
        n => n.to_string(),
    }
}

/// `path` made canonical, without the `\\?\` prefix Windows gives a
/// canonical path: that form is not what a user writes, and a git URL
/// made from it would not pass the URL rule. A path that cannot be made
/// canonical is returned as it is.
fn canonical(path: &Path) -> PathBuf {
    let Ok(canonical) = path.canonicalize() else {
        return path.to_path_buf();
    };
    let text = canonical.to_string_lossy();
    if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{unc}"))
    } else if let Some(disk) = text.strip_prefix(r"\\?\") {
        PathBuf::from(disk)
    } else {
        canonical
    }
}

/// Whether the lock's `locked` ref pins the manifest's `wanted`: the same
/// branch or tag, or, for a rev, a full commit id that the manifest's
/// rev is (a prefix of).
fn same_ref(wanted: &GitRef, locked: &GitRef) -> bool {
    match (wanted, locked) {
        (GitRef::Rev(rev), GitRef::Rev(commit)) => {
            git::is_valid_sha_shape(rev) && commit.starts_with(&rev.to_lowercase())
        }
        (GitRef::Branch(a), GitRef::Branch(b)) | (GitRef::Tag(a), GitRef::Tag(b)) => a == b,
        _ => false,
    }
}

/// A git URL as written in the manifest at `base`: a relative local path
/// (`./x`, `../x`) made absolute against `base`, any other URL as it is.
fn resolve_git_url(url: &str, base: &Path) -> String {
    if url.starts_with("./") || url.starts_with("../") {
        normalize_path(&base.join(url)).display().to_string()
    } else {
        url.to_string()
    }
}

/// A git failure at the dependency `key` on `url` at `ref_spec`: git's
/// message, then its own output as notes, each line marked as git's.
fn git_diagnostic(at: Span, key: &str, url: &str, ref_spec: &GitRef, err: &GitError) -> Diagnostic {
    // `GitError`'s rendering escapes every value and puts git's output
    // on lines of its own.
    let rendered = err.to_string();
    let mut lines = rendered.lines();
    let head = lines.next().unwrap_or_default();
    let mut d = Diagnostic::error(
        Code::GitDependency,
        at,
        format!(
            "git dependency `{key}` (`{}`, {} = `{}`): {head}",
            escape_for_display(url),
            ref_spec.kind(),
            escape_for_display(ref_spec.as_ref_string())
        ),
    );
    d.notes.extend(lines.map(str::to_string));
    d
}

fn symlink_diagnostic(at: Span, key: &str, path: &Path) -> Diagnostic {
    Diagnostic::error(
        Code::SymlinkInDependency,
        at,
        format!(
            "dependency `{key}` contains a symbolic link: `{}`",
            escape_for_display(&path.display().to_string())
        ),
    )
    .with_help("a dependency's files must be its own; replace the link with the file")
}

fn stale_diagnostic(at: Span, key: &str, reason: &str) -> Diagnostic {
    Diagnostic::error(
        Code::LockfileStale,
        at,
        format!("silt.lock is out of date for dependency `{key}`: {reason}"),
    )
    .with_help("run `silt update` to write silt.lock again")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rev_prefix_is_pinned_by_the_commit_it_starts() {
        let commit = "abcdef1".to_string() + &"0".repeat(33);
        assert!(same_ref(
            &GitRef::Rev("ABCDEF1".into()),
            &GitRef::Rev(commit.clone())
        ));
        assert!(same_ref(
            &GitRef::Rev(commit.clone()),
            &GitRef::Rev(commit.clone())
        ));
        assert!(!same_ref(
            &GitRef::Rev("abcdef2".into()),
            &GitRef::Rev(commit.clone())
        ));
        // A tag is never a rev, whatever its name.
        assert!(!same_ref(
            &GitRef::Tag("abcdef1".into()),
            &GitRef::Rev(commit)
        ));
        assert!(same_ref(
            &GitRef::Branch("main".into()),
            &GitRef::Branch("main".into())
        ));
        assert!(!same_ref(
            &GitRef::Branch("main".into()),
            &GitRef::Tag("main".into())
        ));
    }

    #[test]
    fn a_relative_git_url_resolves_against_the_manifest_directory() {
        let base = Path::new("/srv/app");
        assert_eq!(resolve_git_url("../repo", base), "/srv/repo");
        assert_eq!(resolve_git_url("./vendor/r", base), "/srv/app/vendor/r");
        assert_eq!(resolve_git_url("/abs/r", base), "/abs/r");
        assert_eq!(
            resolve_git_url("https://example.com/r.git", base),
            "https://example.com/r.git"
        );
    }

    #[test]
    fn git_output_lines_stay_marked_in_the_diagnostic() {
        const HOSTILE: &str = "x\nerror: FORGED\u{1b}[2K\u{202e}";
        let d = git_diagnostic(
            Span::point(FileId::default(), 0),
            "remote",
            "file:///srv/r.git",
            &GitRef::Branch("main".into()),
            &GitError::CommandFailed {
                command: "git ls-remote".into(),
                stderr: format!("fatal: no\nremote: {HOSTILE}\n"),
                exit_code: Some(128),
            },
        );
        assert_eq!(
            d.message,
            "git dependency `remote` (`file:///srv/r.git`, branch = `main`): \
             git command failed (exit 128): `git ls-remote`"
        );
        assert_eq!(
            d.notes,
            [
                "  git: fatal: no",
                "  git: remote: x",
                "  git: error: FORGED\\u{1b}[2K\\u{202e}",
            ]
        );
    }
}
