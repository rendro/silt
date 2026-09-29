//! Round-101 DOC parity lock: every `channel.*` builtin is mentioned in
//! `docs/concurrency.md`.
//!
//! GAP: `channel.recv_timeout` was the only channel builtin with no
//! presence in `docs/concurrency.md`, so a reader of the concurrency
//! guide could not discover it. This lock compares the guide with the
//! channel module's function registry, so the NEXT channel builtin
//! cannot ship undocumented either.

const CONCURRENCY_DOC_SRC: &str = include_str!("../../docs/concurrency.md");

#[test]
fn every_registered_channel_builtin_is_mentioned_in_concurrency_doc() {
    let names: Vec<String> = silt::module::builtin_module_functions("channel")
        .into_iter()
        .map(|f| format!("channel.{f}"))
        .collect();

    // Sanity: an empty registry would make the parity check vacuous.
    assert!(
        names.iter().any(|n| n == "channel.recv_timeout"),
        "channel.recv_timeout must be in the channel registry; got {names:?}"
    );

    let missing: Vec<&String> = names
        .iter()
        .filter(|name| !CONCURRENCY_DOC_SRC.contains(name.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "docs/concurrency.md must mention every channel.* builtin — a \
         channel builtin a user cannot discover from the concurrency guide \
         is a doc gap (round 101: channel.recv_timeout shipped \
         undocumented). Missing: {missing:?}"
    );
}
