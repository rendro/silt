//! The playground and fuzz crates are built from committed lock files,
//! as the main crate is.
//!
//! Both are crates of their own (`[workspace]` in their manifests), so the
//! main `Cargo.lock` does not pin their dependencies; their own lock files
//! were ignored, and the released playground module was built from
//! whatever crates.io had that day. Now `playground/Cargo.lock` and
//! `fuzz/Cargo.lock` are committed, the playground's cargo calls and its
//! wasm-pack build pass `--locked`, and the fuzz jobs (cargo-fuzz has no
//! `--locked`) first check that the fuzz lock is current.

use std::path::PathBuf;

fn repo_file(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn the_side_crates_lock_files_are_committed_and_not_ignored() {
    let ignore = repo_file(".gitignore");
    for krate in ["playground", "fuzz"] {
        let lock = format!("{krate}/Cargo.lock");
        assert!(
            !ignore.lines().any(|line| line.trim() == format!("/{lock}")),
            ".gitignore hides {lock}"
        );
        let text = repo_file(&lock);
        let name = if krate == "fuzz" {
            "silt-fuzz"
        } else {
            "silt-playground"
        };
        assert!(
            text.contains(&format!("name = \"{name}\"")),
            "{lock} is not the lock of {name}"
        );
    }
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
    let build = repo_file("playground/build.sh");
    let wasm_pack = build
        .lines()
        .find(|l| l.starts_with("wasm-pack build"))
        .expect("build.sh runs wasm-pack build");
    assert!(wasm_pack.ends_with("-- --locked"), "build.sh: {wasm_pack}");

    let check = "run: cargo metadata --locked --manifest-path fuzz/Cargo.toml";
    for workflow in ["ci.yml", "fuzz-nightly.yml"] {
        let text = repo_file(&format!(".github/workflows/{workflow}"));
        let checked = text.find(check);
        let built = text.find("./fuzz/seed.sh");
        assert!(
            matches!((checked, built), (Some(c), Some(b)) if c < b),
            "{workflow}: the fuzz lock is not checked before the fuzz targets are built"
        );
    }
}
