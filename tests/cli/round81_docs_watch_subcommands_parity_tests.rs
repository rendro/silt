//! Regression test for DOC-G1 (round 81).
//!
//! Both `README.md` and `docs/getting-started.md` previously listed only
//! `run`, `check`, and `test` as the subcommands compatible with `--watch`,
//! while the binary actually permits `disasm` as well. This test pins the
//! docs to the subcommands the binary accepts, which its watch gate names
//! when `--watch` is given without a subcommand, so a future addition that
//! forgets to update the prose is caught immediately.

#![cfg(feature = "watch")]

use std::process::Command;

const README: &str = include_str!("../../README.md");
const GETTING_STARTED: &str = include_str!("../../docs/getting-started.md");

/// The subcommands the binary runs under `--watch`, read from the gate's
/// rejection of a bare `silt --watch`:
/// `error: --watch requires a runnable subcommand (run, check, test, disasm)`.
fn runnable_watch_subcommands() -> Vec<String> {
    let out = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("--watch")
        .output()
        .expect("failed to run silt --watch");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let line = stderr
        .lines()
        .find(|l| l.contains("requires a runnable subcommand"))
        .unwrap_or_else(|| panic!("no watch-gate error from `silt --watch`: {stderr}"));
    let open = line
        .find('(')
        .expect("gate error lists subcommands in (...)");
    let close = line.rfind(')').expect("gate error closes its list");
    let names: Vec<String> = line[open + 1..close]
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    assert!(
        !names.is_empty(),
        "gate error listed no subcommands: {line}"
    );
    names
}

/// Locate the canonical sentence prefix and return the substring from the
/// start of that sentence through the end of the line. Returns `None` if
/// the prefix is not present.
fn watch_sentence(doc: &str) -> Option<&str> {
    let prefix = "The `--watch` / `-w` flag works with";
    let start = doc.find(prefix)?;
    let tail = &doc[start..];
    let end = tail.find('\n').unwrap_or(tail.len());
    Some(&tail[..end])
}

#[test]
fn readme_watch_sentence_lists_every_runnable_subcommand() {
    let runnable = runnable_watch_subcommands();
    let sentence = watch_sentence(README).expect(
        "README.md must contain the canonical \"The `--watch` / `-w` flag works with\" sentence",
    );
    for name in &runnable {
        let token = format!("`{name}`");
        assert!(
            sentence.contains(&token),
            "README.md `--watch` sentence is missing subcommand `{name}`. \
             Sentence found: {sentence:?}. Runnable = {runnable:?}"
        );
    }
}

#[test]
fn getting_started_watch_sentence_lists_every_runnable_subcommand() {
    let runnable = runnable_watch_subcommands();
    let sentence = watch_sentence(GETTING_STARTED).expect(
        "docs/getting-started.md must contain the canonical \
         \"The `--watch` / `-w` flag works with\" sentence",
    );
    for name in &runnable {
        let token = format!("`{name}`");
        assert!(
            sentence.contains(&token),
            "docs/getting-started.md `--watch` sentence is missing subcommand `{name}`. \
             Sentence found: {sentence:?}. Runnable = {runnable:?}"
        );
    }
}

#[test]
fn runnable_watch_subcommands_baseline() {
    // The binary runs these four under `--watch` today. A 5th needs the
    // docs updated alongside it — which is exactly the contract we want.
    let runnable = runnable_watch_subcommands();
    for expected in ["run", "check", "disasm", "test"] {
        assert!(
            runnable.iter().any(|s| s == expected),
            "expected `{expected}` among the --watch subcommands, got {runnable:?}"
        );
    }
}
