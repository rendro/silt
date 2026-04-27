//! Regression tests for `Lockfile::matches_manifest`'s offline
//! behaviour.
//!
//! The doc comment on `matches_manifest` promises it does not contact
//! the network: a lockfile pinning `branch = "main"` to a concrete SHA
//! stays valid across `silt run` invocations even when the upstream
//! branch has advanced. Prior to the round-60 fix the implementation
//! unconditionally shelled out to `git ls-remote` for every branch/tag
//! dep, breaking offline `silt run`. These tests lock the offline
//! contract so future refactors can't silently regress it.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use silt::git::GitRef;
use silt::lockfile::{LockedPackage, LockedSource, Lockfile};
use silt::manifest::Manifest;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn fresh_workspace(prefix: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "silt_matches_manifest_offline_{prefix}_{}_{n}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_manifest(dir: &Path, manifest_body: &str) -> PathBuf {
    let path = dir.join("silt.toml");
    fs::write(&path, manifest_body).unwrap();
    let src = dir.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("main.silt"), "fn main() {}\n").unwrap();
    path
}

/// Regression test 1: a manifest with a `{ git = "...", branch = "..." }`
/// entry that has a matching lockfile entry must NOT invoke `git` at
/// all. We verify this indirectly by using a sentinel invalid host: if
/// the code path called `git ls-remote` under the hood, the call would
/// either hang (waiting for DNS/TCP) or fail slowly (after an SSH
/// error). A fast-returning `true` is the signature of the offline path.
///
/// Pre-fix behaviour: `matches_manifest` calls `Lockfile::resolve`,
/// which calls `git::resolve_ref`, which calls `git ls-remote` on the
/// fake URL. That subprocess fails, `resolve` returns `Err`, and
/// `matches_manifest` returns `false`.
///
/// Post-fix behaviour: `matches_manifest` calls `resolve_offline`,
/// which finds the matching lock entry by `(url, ref_spec)` and reuses
/// the stored SHA. No subprocess is spawned. The function returns
/// `true`.
#[test]
fn matches_manifest_does_not_shell_out_to_git_for_valid_lock() {
    let ws = fresh_workspace("no_git_shell_out");
    // Use a fake URL that would fail hard if `git ls-remote` actually
    // ran — we don't want the test's pass/fail signal depending on
    // whether DNS for `example.invalid` resolves (it shouldn't, by RFC
    // 6761, but we don't want to rely on that either). If the code
    // path is truly offline, the URL is never used for network I/O;
    // it's only compared byte-for-byte against the lockfile entry's URL.
    let url = "ssh://git@silt-audit-invalid.example/pkg.git";
    let ref_name = "main";
    let resolved_sha = "0123456789abcdef0123456789abcdef01234567";

    let manifest_body = format!(
        "[package]\nname = \"the_app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\n\
         remote = {{ git = \"{url}\", branch = \"{ref_name}\" }}\n"
    );
    let manifest_path = write_manifest(&ws, &manifest_body);
    let manifest = Manifest::load(&manifest_path).expect("manifest loads");

    // Build a lockfile that already has both the root and the (missing)
    // remote dep "pinned". We don't bother populating the git cache —
    // `matches_manifest` only compares the *name set*, so the only
    // thing that matters is that `resolve_offline` finds the matching
    // git entry and doesn't walk into the cache dir.
    //
    // To avoid recursing into the (absent) cache directory we would
    // normally need the cache to exist. Instead we pre-create a stub
    // cache dir with a silt.toml so `resolve_offline`'s BFS succeeds
    // without any network I/O.
    let cache_dir = silt::git::cache_for(url, resolved_sha).expect("cache_for");
    fs::create_dir_all(&cache_dir).unwrap();
    fs::create_dir_all(cache_dir.join("src")).unwrap();
    fs::write(
        cache_dir.join("silt.toml"),
        "[package]\nname = \"remote\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(cache_dir.join("src").join("lib.silt"), "pub fn f() = 1\n").unwrap();

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
                checksum: "sha256:deadbeef".into(),
            },
        ],
    };

    // Time the call. A successful offline return should take at most a
    // few milliseconds (it only does path normalization + a BTreeMap
    // insert + filesystem reads of the cache dir). A spawned
    // `git ls-remote ssh://...` call on a bogus host either hangs on
    // SSH key negotiation or dies after an obvious delay — well over
    // 250ms in practice.
    let start = Instant::now();
    let ok = lock.matches_manifest(&manifest);
    let elapsed = start.elapsed();

    // Primary assertion: the offline code path returned true.
    assert!(
        ok,
        "matches_manifest should return true for a valid lock (with matching git dep); \
         pre-fix this failed because `git ls-remote` was invoked on the fake URL and \
         the resolve step bailed."
    );

    // Secondary assertion: no network round trip. If a subprocess for
    // `git ls-remote` was spawned the call would take noticeably longer
    // than purely-in-memory work. We give a generous upper bound
    // because cold filesystem reads in CI can be slow; the pre-fix
    // failure mode is multi-second SSH timeout, which this clears by
    // orders of magnitude.
    assert!(
        elapsed < Duration::from_millis(2000),
        "matches_manifest took {elapsed:?} — suspiciously long for an offline path; \
         suggests `git ls-remote` was invoked"
    );

    let _ = fs::remove_dir_all(&cache_dir);
}

/// A git dep in the manifest with no matching entry in the lockfile
/// means the user added a new git dep; `matches_manifest` must return
/// false so the auto-update path re-resolves via `Lockfile::resolve`
/// (which is allowed to hit the network).
#[test]
fn matches_manifest_returns_false_on_new_git_dep_without_lock_entry() {
    let ws = fresh_workspace("new_git_dep");
    let manifest_body = "\
[package]\n\
name = \"the_app\"\n\
version = \"0.1.0\"\n\n\
[dependencies]\n\
remote = { git = \"https://example.com/new.git\", branch = \"main\" }\n";
    let manifest_path = write_manifest(&ws, manifest_body);
    let manifest = Manifest::load(&manifest_path).expect("manifest loads");

    // Lockfile has only the root (no git entry for "remote"). Offline
    // resolution can't find a matching SHA → returns Err → matches_manifest
    // returns false.
    let lock = Lockfile {
        version: 1,
        packages: vec![LockedPackage {
            name: "the_app".into(),
            version: "0.1.0".into(),
            source: LockedSource::Local,
            checksum: String::new(),
        }],
    };

    assert!(
        !lock.matches_manifest(&manifest),
        "a manifest with a new git dep (no matching lock entry) must report \
         mismatch so the auto-update path regenerates the lockfile"
    );
}

/// Baseline: manifest with only path deps has always been offline.
/// Ensures the resolve_offline refactor didn't regress the path-dep
/// happy path (e.g. by accidentally requiring a git cache entry for
/// every dep).
#[test]
fn matches_manifest_accepts_path_deps_offline() {
    let ws = fresh_workspace("path_dep");
    let app = ws.join("app");
    let dep = ws.join("calc");

    // Dep package.
    fs::create_dir_all(dep.join("src")).unwrap();
    fs::write(
        dep.join("silt.toml"),
        "[package]\nname = \"calc\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(
        dep.join("src").join("lib.silt"),
        "pub fn add(a, b) = a + b\n",
    )
    .unwrap();

    // App package.
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(
        app.join("silt.toml"),
        "[package]\nname = \"the_app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\ncalc = { path = \"../calc\" }\n",
    )
    .unwrap();
    fs::write(app.join("src").join("main.silt"), "fn main() {}\n").unwrap();

    let manifest = Manifest::load(&app.join("silt.toml")).expect("manifest loads");
    let lock = Lockfile::resolve(&manifest).expect("initial resolve");
    assert!(
        lock.matches_manifest(&manifest),
        "freshly-resolved lockfile must match its own manifest (path deps)"
    );
}
