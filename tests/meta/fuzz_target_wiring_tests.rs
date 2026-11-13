//! Every fuzz target is run everywhere a fuzz target runs.
//!
//! A target is a `[[bin]]` of fuzz/Cargo.toml. Each one has its source
//! file and committed seeds, is replayed by the corpus job of ci.yml,
//! fuzzed by the nightly matrix and by `fuzz/local.sh all`, and has its
//! row in fuzz/README.md. A target missing from one of them is fuzzed
//! nowhere, or its seeds are never replayed.

use std::path::PathBuf;

fn repo_file(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        .replace("\r\n", "\n")
}

fn targets() -> Vec<String> {
    let manifest = repo_file("fuzz/Cargo.toml");
    let targets: Vec<String> = manifest
        .split("[[bin]]")
        .skip(1)
        .map(|bin| {
            let name = (bin.lines())
                .find_map(|line| line.strip_prefix("name = \""))
                .and_then(|rest| rest.strip_suffix('"'))
                .expect("a [[bin]] has a name");
            let path = format!("path = \"fuzz_targets/{name}.rs\"");
            assert!(bin.contains(&path), "{name}: {path}");
            name.to_string()
        })
        .collect();
    assert!(targets.len() >= 13, "{targets:?}");
    targets
}

#[test]
fn every_fuzz_target_is_replayed_fuzzed_and_described() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let ci = repo_file(".github/workflows/ci.yml");
    let nightly = repo_file(".github/workflows/fuzz-nightly.yml");
    let local = repo_file("fuzz/local.sh");
    let readme = repo_file("fuzz/README.md");
    for target in targets() {
        assert!(
            root.join(format!("fuzz/fuzz_targets/{target}.rs"))
                .is_file(),
            "{target}: no source"
        );
        let seeds = std::fs::read_dir(root.join(format!("fuzz/corpus/{target}")))
            .map(|dir| dir.count())
            .unwrap_or(0);
        assert!(seeds > 0, "{target}: no seeds");
        assert!(
            ci.contains(&format!("run: ./fuzz/replay.sh {target}\n")),
            "{target}: not replayed by ci.yml"
        );
        assert!(
            nightly.contains(&format!("          - {target}\n")),
            "{target}: not in the nightly matrix"
        );
        let all = (local.lines())
            .find(|line| line.trim_start().starts_with("for t in "))
            .expect("local.sh's loop over every target");
        assert!(
            all.split_whitespace()
                .any(|word| word.trim_end_matches(';') == target),
            "{target}: not in `fuzz/local.sh all`"
        );
        assert!(
            readme.contains(&format!("| `{target}`")),
            "{target}: no row in fuzz/README.md"
        );
    }
}
