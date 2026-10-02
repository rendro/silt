//! How `package_graph::resolve_packages` uses `silt.lock`.
//!
//! A lockfile pinning `branch = "main"` to a concrete commit stays valid
//! across `silt run` invocations even when the upstream branch has
//! advanced: resolving the graph with a lock that pins every git
//! dependency never contacts the network. A dependency the lock does not
//! pin is resolved (`Update`) or is an error (`ReadOnly`), and a lock
//! that pins the graph is not rewritten.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use silt::diagnostic::Code;
use silt::git::GitRef;
use silt::lockfile::{LockedPackage, LockedSource, Lockfile};
use silt::package_graph::{LockChange, LockPolicy, resolve_packages};
use silt::source::SourceMap;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn fresh_workspace(prefix: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "silt_package_graph_lock_{prefix}_{}_{n}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_manifest(dir: &Path, manifest_body: &str) {
    fs::write(dir.join("silt.toml"), manifest_body).unwrap();
    let src = dir.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("main.silt"), "fn main() {}\n").unwrap();
}

/// A manifest with a `{ git = "...", branch = "..." }` entry that has a
/// matching lockfile entry must NOT invoke `git` at all. The URL names
/// a host that does not exist: had `git ls-remote` run, the call would
/// fail (slowly), and resolving would be an error.
#[test]
fn a_pinned_git_dependency_resolves_without_git() {
    let ws = fresh_workspace("no_git_shell_out");
    let url = "ssh://git@silt-audit-invalid.example/pkg.git";
    let ref_name = "main";
    let resolved_sha = "0123456789abcdef0123456789abcdef01234567";

    write_manifest(
        &ws,
        &format!(
            "[package]\nname = \"the_app\"\nversion = \"0.1.0\"\n\n\
             [dependencies]\n\
             remote = {{ git = \"{url}\", branch = \"{ref_name}\" }}\n"
        ),
    );

    // A populated cache directory for the pinned commit, so the package
    // can be read without a clone.
    let cache_dir = silt::git::cache_for(url, resolved_sha).expect("cache_for");
    fs::create_dir_all(cache_dir.join("src")).unwrap();
    fs::write(
        cache_dir.join("silt.toml"),
        "[package]\nname = \"remote\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(cache_dir.join("src").join("lib.silt"), "pub fn f() { 1 }\n").unwrap();

    let lock = Lockfile {
        version: 1,
        packages: vec![
            LockedPackage {
                name: "the_app".into(),
                version: "0.1.0".into(),
                source: LockedSource::Local,
                checksum: String::new(),
            },
            LockedPackage {
                name: "remote".into(),
                version: "0.1.0".into(),
                source: LockedSource::Git {
                    url: url.to_string(),
                    ref_spec: GitRef::Branch(ref_name.to_string()),
                    resolved_sha: resolved_sha.to_string(),
                },
                // Checksums are not compared: content drift is what
                // `silt update` is for.
                checksum: "sha256:deadbeef".into(),
            },
        ],
    };
    lock.write(&ws.join("silt.lock")).unwrap();

    let start = Instant::now();
    let result = resolve_packages(&ws, LockPolicy::Update, &mut SourceMap::new());
    let elapsed = start.elapsed();
    let graph = result.unwrap_or_else(|d| panic!("a pinned dependency must resolve: {d:?}"));
    assert_eq!(graph.packages.len(), 2);
    assert_eq!(graph.lock, LockChange::Unchanged, "the lock pins the graph");
    assert!(
        elapsed < Duration::from_millis(2000),
        "resolving took {elapsed:?} — suspiciously long for an offline path; \
         suggests `git ls-remote` was invoked"
    );

    let _ = fs::remove_dir_all(&cache_dir);
}

/// A git dependency the lock has no pin for, where the lock may not be
/// written: `LockfileStale`, without contacting the network.
#[test]
fn an_unpinned_git_dependency_is_stale_when_the_lock_is_read_only() {
    let ws = fresh_workspace("new_git_dep");
    write_manifest(
        &ws,
        "[package]\nname = \"the_app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\n\
         remote = { git = \"ssh://git@silt-audit-invalid.example/new.git\", branch = \"main\" }\n",
    );
    Lockfile {
        version: 1,
        packages: vec![LockedPackage {
            name: "the_app".into(),
            version: "0.1.0".into(),
            source: LockedSource::Local,
            checksum: String::new(),
        }],
    }
    .write(&ws.join("silt.lock"))
    .unwrap();

    let mut sources = SourceMap::new();
    let errors = resolve_packages(&ws, LockPolicy::ReadOnly, &mut sources)
        .expect_err("an unpinned dependency under a read-only lock is an error");
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].code, Code::LockfileStale, "{errors:?}");
    assert!(
        errors[0].message.contains("`remote`")
            && errors[0].message.ends_with("it has no pin for this source"),
        "{errors:?}"
    );
}

/// Path dependencies are always resolved offline: the first resolve
/// writes the lock, the second finds it pinning the graph.
#[test]
fn path_dependencies_write_the_lock_once() {
    let ws = fresh_workspace("path_dep");
    let app = ws.join("app");
    let dep = ws.join("calc");

    fs::create_dir_all(dep.join("src")).unwrap();
    fs::write(
        dep.join("silt.toml"),
        "[package]\nname = \"calc\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(
        dep.join("src").join("lib.silt"),
        "pub fn add(a, b) { a + b }\n",
    )
    .unwrap();
    fs::create_dir_all(&app).unwrap();
    write_manifest(
        &app,
        "[package]\nname = \"the_app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\ncalc = { path = \"../calc\" }\n",
    );

    let first = resolve_packages(&app, LockPolicy::Update, &mut SourceMap::new())
        .unwrap_or_else(|d| panic!("{d:?}"));
    assert_eq!(first.lock, LockChange::Created);
    let second = resolve_packages(&app, LockPolicy::Update, &mut SourceMap::new())
        .unwrap_or_else(|d| panic!("{d:?}"));
    assert_eq!(
        second.lock,
        LockChange::Unchanged,
        "a freshly written lockfile must pin its own graph"
    );
}
