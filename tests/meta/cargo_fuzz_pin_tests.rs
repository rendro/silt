//! The fuzz jobs install one fixed cargo-fuzz.
//!
//! `cargo install cargo-fuzz` took whatever crates.io had that day, built
//! with fresh dependencies. Every workflow now installs cargo-fuzz with
//! `.github/scripts/install-cargo-fuzz.sh`: one version, its prebuilt
//! binary refused unless its SHA-256 is the one written there.

use std::path::PathBuf;

fn repo_file(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn every_workflow_installs_the_one_pinned_cargo_fuzz() {
    let mut installs = 0;
    for workflow in ["ci.yml", "fuzz-nightly.yml", "release.yml", "audit.yml"] {
        let text = repo_file(&format!(".github/workflows/{workflow}"));
        assert!(
            !text.contains("cargo install cargo-fuzz"),
            "{workflow}: cargo-fuzz built from crates.io at the version of the day"
        );
        installs += text
            .matches("run: bash .github/scripts/install-cargo-fuzz.sh")
            .count();
    }
    assert_eq!(installs, 2, "the corpus replay and the nightly fuzz jobs");

    let script = repo_file(".github/scripts/install-cargo-fuzz.sh");
    let value = |name: &str| {
        script
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{name}=")))
            .unwrap_or_else(|| panic!("the script sets no {name}"))
            .to_string()
    };
    let version = value("version");
    assert!(
        version.split('.').count() == 3 && version.split('.').all(|n| n.parse::<u32>().is_ok()),
        "an exact version, not {version:?}"
    );
    let sha256 = value("sha256");
    assert!(
        sha256.len() == 64 && sha256.bytes().all(|b| b.is_ascii_hexdigit()),
        "a SHA-256, not {sha256:?}"
    );
    assert!(script.contains("sha256sum -c -"), "the archive is checked");
}

/// The pinned binary is cargo-fuzz's musl build, whose default target is
/// its own triple: a sanitizer build for musl fails ("sanitizer is
/// incompatible with statically linked libc"). So every build or run of
/// a fuzz target names the gnu target.
#[test]
fn every_fuzz_build_names_the_gnu_target() {
    let mut calls = 0;
    for file in [
        ".github/workflows/ci.yml",
        ".github/workflows/fuzz-nightly.yml",
        "fuzz/replay.sh",
        "fuzz/local.sh",
        "fuzz/seed.sh",
        "fuzz/README.md",
    ] {
        let text = repo_file(file);
        for (at, _) in text.match_indices("cargo +nightly fuzz ") {
            let call = text[at..].lines().next().unwrap_or_default();
            assert!(
                call.contains(" --target x86_64-unknown-linux-gnu "),
                "{file}: `{call}` builds for the default target of cargo-fuzz"
            );
            calls += 1;
        }
    }
    // replay.sh's build, local.sh's run, the nightly's run, the README's two.
    assert_eq!(calls, 5);
}
