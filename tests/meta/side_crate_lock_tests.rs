//! The playground and fuzz crates are built from committed lock files,
//! as the main crate is.
//!
//! Both are crates of their own (`[workspace]` in their manifests), so the
//! main `Cargo.lock` does not pin their dependencies; their own lock files
//! were ignored, and the released playground module was built from
//! whatever crates.io had that day. Now `playground/Cargo.lock` and
//! `fuzz/Cargo.lock` are committed and hold every package they share with
//! the root lock at the root's version and checksum; the playground's
//! cargo calls pass `--locked` and `build.sh` checks its lock before
//! wasm-pack (whose own `cargo metadata` would rewrite a stale one); the
//! fuzz jobs (cargo-fuzz has no `--locked`) first check the fuzz lock; a
//! release writes its version into all three locks; the weekly audit
//! reads all three.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

const SIDE_LOCKS: [&str; 2] = ["playground/Cargo.lock", "fuzz/Cargo.lock"];

fn repo_file(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// One `[[package]]` of a lock file.
#[derive(Debug)]
struct Locked {
    name: String,
    version: String,
    /// `None` for a path package (silt, the side crate itself).
    checksum: Option<String>,
}

/// The packages of the lock file `rel`.
fn packages(rel: &str) -> Vec<Locked> {
    let text = repo_file(rel);
    let value = |block: &str, key: &str| {
        block.lines().find_map(|line| {
            line.strip_prefix(&format!("{key} = \""))
                .and_then(|rest| rest.strip_suffix('"'))
                .map(str::to_string)
        })
    };
    text.split("[[package]]")
        .skip(1)
        .map(|block| Locked {
            name: value(block, "name").unwrap_or_else(|| panic!("{rel}: a package without a name")),
            version: value(block, "version")
                .unwrap_or_else(|| panic!("{rel}: a package without a version")),
            checksum: value(block, "checksum"),
        })
        .collect()
}

/// The first `version = "..."` of Cargo.toml: the package's own.
fn silt_version() -> String {
    repo_file("Cargo.toml")
        .lines()
        .find_map(|line| {
            line.strip_prefix("version = \"")
                .and_then(|rest| rest.strip_suffix('"'))
        })
        .expect("Cargo.toml has a version")
        .to_string()
}

#[test]
fn the_side_crates_lock_files_are_committed_and_not_ignored() {
    let ignore = repo_file(".gitignore");
    for (lock, name) in SIDE_LOCKS.into_iter().zip(["silt-playground", "silt-fuzz"]) {
        assert!(
            !ignore.lines().any(|line| line.trim() == format!("/{lock}")),
            ".gitignore hides {lock}"
        );
        assert!(
            packages(lock).iter().any(|p| p.name == name),
            "{lock} is not the lock of {name}"
        );
    }
}

/// A package a side lock shares with the root lock is at one of the
/// root's versions of it, with the root's checksum: the playground ships,
/// and the fuzz targets test, what the binary is built from.
#[test]
fn a_package_shared_with_the_root_lock_is_at_the_root_s_version() {
    let mut root: BTreeMap<(String, String), Option<String>> = BTreeMap::new();
    for p in packages("Cargo.lock") {
        root.insert((p.name, p.version), p.checksum);
    }
    let root_names: BTreeSet<String> = root.keys().map(|(name, _)| name.clone()).collect();
    let mut drift = Vec::new();
    for lock in SIDE_LOCKS {
        for p in packages(lock) {
            if !root_names.contains(&p.name) {
                continue;
            }
            match root.get(&(p.name.clone(), p.version.clone())) {
                None => drift.push(format!(
                    "{lock}: {} {} (not a version of the root lock)",
                    p.name, p.version
                )),
                Some(checksum) if *checksum != p.checksum => drift.push(format!(
                    "{lock}: {} {} (another checksum)",
                    p.name, p.version
                )),
                Some(_) => {}
            }
        }
    }
    assert!(
        drift.is_empty(),
        "the side locks drift from Cargo.lock:\n  {}",
        drift.join("\n  ")
    );
}

/// silt's own version is the same in Cargo.toml and the three locks: a
/// side lock that names another one fails its crate's `--locked` build.
#[test]
fn silt_has_one_version_in_cargo_toml_and_the_three_locks() {
    let version = silt_version();
    for lock in ["Cargo.lock"].into_iter().chain(SIDE_LOCKS) {
        let silt: Vec<String> = packages(lock)
            .into_iter()
            .filter(|p| p.name == "silt")
            .map(|p| p.version)
            .collect();
        assert_eq!(silt, std::slice::from_ref(&version), "{lock}");
    }
}

/// The release writes its version into Cargo.toml and the three locks,
/// and checks each crate's lock before it commits.
#[test]
fn a_release_writes_its_version_into_the_three_locks() {
    let release = repo_file(".github/workflows/release.yml");
    assert!(
        release.contains("locks=\"Cargo.lock playground/Cargo.lock fuzz/Cargo.lock\""),
        "release.yml does not name the three locks"
    );
    assert!(
        release.contains("[ \"$(git diff --numstat | wc -l)\" = 4 ]"),
        "release.yml does not ask for exactly Cargo.toml and the three locks"
    );
    assert!(
        release.contains("for manifest in Cargo.toml playground/Cargo.toml fuzz/Cargo.toml; do"),
        "release.yml does not check each crate's lock"
    );
}

#[test]
fn the_playground_is_built_locked_and_the_fuzz_lock_is_checked() {
    let ci = repo_file(".github/workflows/ci.yml");
    for line in ci
        .lines()
        .filter(|l| l.contains("--manifest-path playground/Cargo.toml"))
    {
        let line = line.trim();
        if line.starts_with("run: cargo fmt") {
            continue;
        }
        assert!(line.contains("--locked"), "ci.yml: {line}");
    }
    // build.sh checks the lock before wasm-pack, whose own unlocked
    // `cargo metadata` would rewrite a stale one, and fails if the lock
    // changed during the build.
    let build = repo_file("playground/build.sh");
    let at = |needle: &str| {
        build
            .find(needle)
            .unwrap_or_else(|| panic!("build.sh has no `{needle}`"))
    };
    let wasm_pack = at("\nwasm-pack build ");
    assert!(
        at("\ncargo metadata --locked ") < wasm_pack,
        "build.sh checks the lock after wasm-pack"
    );
    assert!(at("before=\"$(cksum < Cargo.lock)\"") < wasm_pack);
    assert!(at("if [ \"$(cksum < Cargo.lock)\" != \"$before\" ]") > wasm_pack);
    assert!(
        build[wasm_pack..]
            .lines()
            .nth(1)
            .is_some_and(|l| l.ends_with("-- --locked")),
        "build.sh's wasm-pack line does not pass --locked"
    );

    // The check stands before every step that builds a fuzz target (the
    // lines of the workflow, comments left out).
    let check = "run: cargo metadata --locked --manifest-path fuzz/Cargo.toml";
    for workflow in ["ci.yml", "fuzz-nightly.yml"] {
        let text = repo_file(&format!(".github/workflows/{workflow}"));
        let lines: Vec<&str> = text
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .collect();
        let checked = lines
            .iter()
            .position(|l| l.contains(check))
            .unwrap_or_else(|| panic!("{workflow}: the fuzz lock is not checked"));
        for builds in [
            "./fuzz/seed.sh",
            "./fuzz/replay.sh",
            "fuzz run",
            "fuzz build",
        ] {
            if let Some(first) = lines.iter().position(|l| l.contains(builds)) {
                assert!(
                    checked < first,
                    "{workflow}: `{builds}` comes before the lock check"
                );
            }
        }
    }
}

#[test]
fn the_weekly_audit_reads_the_three_locks() {
    let audit = repo_file(".github/workflows/audit.yml");
    for line in [
        "cargo audit\n",
        "cargo audit --file playground/Cargo.lock\n",
        "cargo audit --file fuzz/Cargo.lock\n",
    ] {
        assert!(audit.contains(line), "audit.yml has no `{}`", line.trim());
    }
}
