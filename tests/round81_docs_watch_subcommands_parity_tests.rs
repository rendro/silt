//! Regression test for DOC-G1 (round 81).
//!
//! Both `README.md` and `docs/getting-started.md` previously listed only
//! `run`, `check`, and `test` as the subcommands compatible with `--watch`,
//! while `src/cli/watch.rs` actually permits `disasm` as well (and the help
//! banner advertises `silt disasm [--watch]`). This test pins the docs to
//! the source-of-truth `RUNNABLE` allowlist so a future addition that
//! forgets to update the prose is caught immediately.

const README: &str = include_str!("../README.md");
const GETTING_STARTED: &str = include_str!("../docs/getting-started.md");
const WATCH_RS: &str = include_str!("../src/cli/watch.rs");

/// Parse the `RUNNABLE: &[&str] = &[...]` literal from `src/cli/watch.rs`
/// and return the contained string names. We intentionally do this with
/// simple slicing rather than a regex dependency to keep the test
/// self-contained.
fn extract_runnable_from_watch_rs() -> Vec<String> {
    let needle = "RUNNABLE: &[&str] = &[";
    let start = WATCH_RS
        .find(needle)
        .expect("watch.rs should declare `RUNNABLE: &[&str] = &[...]`");
    let after = &WATCH_RS[start + needle.len()..];
    let end = after
        .find(']')
        .expect("RUNNABLE literal should be closed by `]`");
    let body = &after[..end];

    let mut names = Vec::new();
    for chunk in body.split(',') {
        let trimmed = chunk.trim();
        if trimmed.is_empty() {
            continue;
        }
        // strip surrounding double quotes
        let stripped = trimmed
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .unwrap_or_else(|| {
                panic!("expected quoted string literal in RUNNABLE, got: {trimmed:?}")
            });
        names.push(stripped.to_string());
    }
    assert!(
        !names.is_empty(),
        "RUNNABLE allowlist parsed to zero entries"
    );
    names
}

/// Locate the canonical sentence prefix and return the substring from the
/// start of that sentence through the end of the line. Returns `None` if
/// the prefix is not present.
fn watch_sentence<'a>(doc: &'a str) -> Option<&'a str> {
    let prefix = "The `--watch` / `-w` flag works with";
    let start = doc.find(prefix)?;
    // Scan forward to end-of-line — list items live on the same line in
    // both docs as a comma-separated phrase.
    let tail = &doc[start..];
    let end = tail.find('\n').unwrap_or(tail.len());
    Some(&tail[..end])
}

#[test]
fn readme_watch_sentence_lists_every_runnable_subcommand() {
    let runnable = extract_runnable_from_watch_rs();
    let sentence = watch_sentence(README).expect(
        "README.md must contain the canonical \"The `--watch` / `-w` flag works with\" sentence",
    );
    for name in &runnable {
        let token = format!("`{name}`");
        assert!(
            sentence.contains(&token),
            "README.md `--watch` sentence is missing subcommand `{name}`. \
             Sentence found: {sentence:?}. RUNNABLE = {runnable:?}"
        );
    }
}

#[test]
fn getting_started_watch_sentence_lists_every_runnable_subcommand() {
    let runnable = extract_runnable_from_watch_rs();
    let sentence = watch_sentence(GETTING_STARTED).expect(
        "docs/getting-started.md must contain the canonical \
         \"The `--watch` / `-w` flag works with\" sentence",
    );
    for name in &runnable {
        let token = format!("`{name}`");
        assert!(
            sentence.contains(&token),
            "docs/getting-started.md `--watch` sentence is missing subcommand `{name}`. \
             Sentence found: {sentence:?}. RUNNABLE = {runnable:?}"
        );
    }
}

#[test]
fn runnable_extractor_finds_expected_baseline() {
    // Sanity check: the parser itself should produce the four known
    // entries today. If a future change adds a 5th, this test will need
    // to be updated alongside the docs — which is exactly the contract
    // we want.
    let runnable = extract_runnable_from_watch_rs();
    for expected in ["run", "check", "disasm", "test"] {
        assert!(
            runnable.iter().any(|s| s == expected),
            "expected `{expected}` in RUNNABLE, got {runnable:?}"
        );
    }
}
