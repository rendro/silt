//! Round 87 LATENT — `find_silt_files` must skip the same directories
//! the LSP preloader skips (`target/`, `.git/`, `node_modules/`,
//! `fuzz/corpus/…`).
//!
//! Pre-fix, `src/cli/paths.rs::find_silt_files` was a 19-line recursive
//! walker that filtered only on `.silt` suffix and never looked at the
//! directory name. `src/cli/test.rs::find_test_files` was a byte-for-byte
//! clone with the same gap. Both fed `silt fmt <dir>` /
//! `silt test <dir>` / the implicit-recursive `silt fmt` flow, so a
//! workspace that vendored a sibling silt project (`vendor/silt-src/`)
//! or had any `.silt` artefact under `target/` would see those files
//! silently rewritten by `silt fmt` and executed by `silt test`. The
//! LSP indexer at `src/lsp/preload.rs::should_skip_dir` already had the
//! correct skip-list; this test locks the CLI walker against the same
//! policy by exercising the production path through `silt fmt --check`.
//!
//! The lock keys off `silt fmt --check <tmp>`'s stderr-reported "not
//! formatted" lines. Every `.silt` file we plant under the temp dir is
//! deliberately mis-formatted (trailing blank lines), so any file the
//! walker reaches produces a stderr line we can pattern-match. After
//! the fix, only `keepme.silt` and `sub/keepme2.silt` are reached;
//! before the fix, the four files under `target/`, `.git/`,
//! `node_modules/`, and `fuzz/corpus/<target>/` all leak through.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// Construct a fresh fixture tree under `std::env::temp_dir()`. The
/// returned path is leaked (no cleanup) so the subprocess can read
/// it; the OS reaps on test-process exit, matching the leak shape used
/// in `tests/round86_check_json_no_ansi_under_force_color_tests.rs`.
fn build_fixture() -> PathBuf {
    let tmp = std::env::temp_dir().join(format!(
        "silt-round87-skipdirs-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    ));
    fs::create_dir_all(&tmp).expect("create temp fixture dir");

    // Mis-formatted body: trailing blank lines that `silt fmt` will
    // flag as "not formatted". Any file the walker visits will produce
    // a stderr line we can grep for.
    let mis_formatted = "fn main() = 1\n\n\n\n";

    // Files that SHOULD be visited.
    fs::write(tmp.join("keepme.silt"), mis_formatted).expect("write keepme.silt");
    let sub = tmp.join("sub");
    fs::create_dir_all(&sub).expect("create sub");
    fs::write(sub.join("keepme2.silt"), mis_formatted).expect("write sub/keepme2.silt");

    // Files that should NOT be visited. Each lives under a directory
    // name in the shared skip-list (`silt::file_discovery::should_skip_dir`).
    for skip_parent in &["target", ".git", "node_modules"] {
        let d = tmp.join(skip_parent);
        fs::create_dir_all(&d).unwrap_or_else(|e| panic!("create {skip_parent}: {e}"));
        fs::write(d.join("skipme.silt"), mis_formatted)
            .unwrap_or_else(|e| panic!("write {skip_parent}/skipme.silt: {e}"));
    }
    // `fuzz/corpus/<target>/skipme.silt` — the skip rule is "any
    // directory under `fuzz/corpus/…`", so we plant the file two
    // levels deep to exercise that ancestor-chain case rather than
    // the simple file_name match.
    let fuzz_target = tmp.join("fuzz").join("corpus").join("fuzz_formatter");
    fs::create_dir_all(&fuzz_target).expect("create fuzz/corpus/fuzz_formatter");
    fs::write(fuzz_target.join("skipme.silt"), mis_formatted)
        .expect("write fuzz/corpus/fuzz_formatter/skipme.silt");

    tmp
}

/// Run `silt fmt --check <tmp>` and return `(exit_code, stderr)`. We
/// pass the directory as a positional so the dispatcher expands it via
/// `find_silt_files` (the production path under test). Running with no
/// positionals would also go through `find_silt_files` but would also
/// require a `silt.toml` anchor in the cwd — passing the dir
/// explicitly keeps the test independent of cwd state.
fn run_fmt_check(dir: &PathBuf) -> (i32, String) {
    let bin = env!("CARGO_BIN_EXE_silt");
    let output = Command::new(bin)
        .arg("fmt")
        .arg("--check")
        .arg(dir)
        .output()
        .expect("spawn silt fmt --check");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn find_silt_files_skips_target_git_node_modules_and_fuzz_corpus() {
    let tmp = build_fixture();
    let (code, stderr) = run_fmt_check(&tmp);

    // Sanity: exit 1 because at least one file (keepme.silt) IS
    // mis-formatted. Exit 2 would mean an infra error (parse/read
    // failure) which would mask the real signal.
    assert_eq!(
        code, 1,
        "expected exit 1 (drift detected); got code={code}\nstderr:\n{stderr}"
    );

    // The two files we DO want walked must both appear in stderr.
    let keepme = tmp.join("keepme.silt");
    let keepme2 = tmp.join("sub").join("keepme2.silt");
    assert!(
        stderr.contains(&keepme.display().to_string()),
        "expected `keepme.silt` to be visited (it's at the fixture root); \
         stderr:\n{stderr}"
    );
    assert!(
        stderr.contains(&keepme2.display().to_string()),
        "expected `sub/keepme2.silt` to be visited (one level deep, no \
         skip-list match); stderr:\n{stderr}"
    );

    // The four files we do NOT want walked must NOT appear in stderr.
    // Each assertion carries the specific skip rule it locks, so a
    // future regression points straight at the offending policy entry.
    let must_be_absent: &[(&str, PathBuf)] = &[
        (
            "target/ (Cargo build dir)",
            tmp.join("target").join("skipme.silt"),
        ),
        (".git/ (VCS metadata)", tmp.join(".git").join("skipme.silt")),
        (
            "node_modules/ (npm/yarn vendoring)",
            tmp.join("node_modules").join("skipme.silt"),
        ),
        (
            "fuzz/corpus/<target>/ (fuzz inputs, ancestor-chain match)",
            tmp.join("fuzz")
                .join("corpus")
                .join("fuzz_formatter")
                .join("skipme.silt"),
        ),
    ];
    for (label, path) in must_be_absent {
        let s = path.display().to_string();
        assert!(
            !stderr.contains(&s),
            "skip-list regression: `silt fmt --check {}` walked into \
             {label} and visited {s}\nstderr:\n{stderr}",
            tmp.display()
        );
    }
}
