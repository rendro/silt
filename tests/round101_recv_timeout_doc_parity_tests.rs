//! Round-101 DOC parity locks: `channel.recv_timeout` in `docs/concurrency.md`.
//!
//! GAP: `channel.recv_timeout` was the only channel builtin with no
//! presence in `docs/concurrency.md` — no dedicated section in the
//! Channels chapter and no row in the Quick Reference table. It is
//! registered and typed at `src/typechecker/builtins/channel.rs`
//! (`(Channel(a), Duration) -> Result(a, ChannelError)`), implemented in
//! `src/builtins/concurrency.rs`, and has LSP hover docs — but a reader
//! of the concurrency guide could not discover it and was steered to the
//! clunkier `channel.timeout` + `channel.select` pattern instead.
//!
//! The prior locks (`round77_concurrency_doc_parity_tests`,
//! `round74_modules_doc_lists_all_builtins_tests`) check sections and
//! module lists, not per-function channel coverage, so nothing fired.
//!
//! Locks here:
//!
//! 1. **Registry parity**: every `channel.*` name registered in
//!    `src/typechecker/builtins/channel.rs` must be mentioned in
//!    `docs/concurrency.md`. This is the structural lock — the NEXT
//!    channel builtin cannot ship undocumented either.
//! 2. **Dedicated section**: the guide has a
//!    `### Receive with timeout: \`channel.recv_timeout(ch, dur)\``
//!    subsection documenting the `Result(a, ChannelError)` shape
//!    (`Ok` / `Err(ChannelTimeout)` / `Err(ChannelClosed)`) and the
//!    buffered-value-wins-over-expired-timer corner case, matching the
//!    doc-comment at `src/typechecker/builtins/channel.rs`.
//! 3. **Quick Reference row**: the Quick Reference table names
//!    `channel.recv_timeout` and its error variants.

const CONCURRENCY_DOC_SRC: &str = include_str!("../docs/concurrency.md");
const CHANNEL_BUILTINS_SRC: &str = include_str!("../src/typechecker/builtins/channel.rs");

/// Extract every builtin name registered as `intern("channel.<name>")`
/// in the typechecker's channel module. This is the authoritative
/// registration site: a builtin absent here does not typecheck, and a
/// builtin present here is user-callable and must be documented.
fn registered_channel_builtins() -> Vec<String> {
    let needle = "intern(\"channel.";
    let mut names = Vec::new();
    let mut rest = CHANNEL_BUILTINS_SRC;
    while let Some(pos) = rest.find(needle) {
        let after = &rest[pos + needle.len()..];
        let end = after
            .find('"')
            .expect("unterminated intern(\"channel.… string literal");
        names.push(format!("channel.{}", &after[..end]));
        rest = &after[end..];
    }
    names.sort();
    names.dedup();
    names
}

// ────────────────────────────────────────────────────────────────────
// Lock 1: registry ↔ doc parity for every channel.* builtin.
// ────────────────────────────────────────────────────────────────────

#[test]
fn every_registered_channel_builtin_is_mentioned_in_concurrency_doc() {
    let names = registered_channel_builtins();

    // Sanity: the extraction must see the known registry. If the
    // registration site moves or the intern(...) shape changes, this
    // fires loudly instead of the parity check silently passing on an
    // empty list.
    assert!(
        names.len() >= 10,
        "expected at least 10 channel.* builtins registered in \
         src/typechecker/builtins/channel.rs, found {}: {:?} — did the \
         registration site move?",
        names.len(),
        names
    );
    assert!(
        names.iter().any(|n| n == "channel.recv_timeout"),
        "channel.recv_timeout must be extracted from the registry \
         (it is registered at src/typechecker/builtins/channel.rs); \
         extracted: {:?}",
        names
    );

    let missing: Vec<&String> = names
        .iter()
        .filter(|name| !CONCURRENCY_DOC_SRC.contains(name.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "docs/concurrency.md must mention every channel.* builtin \
         registered in src/typechecker/builtins/channel.rs — a channel \
         builtin a user cannot discover from the concurrency guide is a \
         doc gap (round 101: channel.recv_timeout shipped undocumented). \
         Missing: {:?}",
        missing
    );
}

// ────────────────────────────────────────────────────────────────────
// Lock 2: dedicated recv_timeout section with the Result shape.
// ────────────────────────────────────────────────────────────────────

#[test]
fn concurrency_doc_has_recv_timeout_section() {
    assert!(
        CONCURRENCY_DOC_SRC.contains("### Receive with timeout: `channel.recv_timeout(ch, dur)`"),
        "concurrency.md must have a dedicated `### Receive with timeout: \
         `channel.recv_timeout(ch, dur)`` section in the Channels chapter, \
         mirroring the other channel.* builtins."
    );
}

#[test]
fn recv_timeout_section_documents_result_shape_and_ready_value_semantics() {
    // Restrict the assertions to the recv_timeout section (it is the
    // last subsection of the Channels chapter, ending at `## 3. Tasks`)
    // so a regression that strips the shape from the section but leaves
    // the words elsewhere is still caught.
    let start = CONCURRENCY_DOC_SRC
        .find("### Receive with timeout: `channel.recv_timeout(ch, dur)`")
        .expect("recv_timeout section must exist");
    let end = CONCURRENCY_DOC_SRC[start..]
        .find("## 3. Tasks")
        .expect("recv_timeout section must precede the Tasks chapter");
    let section = &CONCURRENCY_DOC_SRC[start..start + end];

    assert!(
        section.contains("Result(a, ChannelError)"),
        "recv_timeout section must document the `Result(a, ChannelError)` \
         return type (it is the one channel receive that does NOT return \
         ChannelResult)."
    );
    assert!(
        section.contains("ChannelTimeout"),
        "recv_timeout section must name the `ChannelTimeout` error variant."
    );
    assert!(
        section.contains("ChannelClosed"),
        "recv_timeout section must name the `ChannelClosed` error variant."
    );
    assert!(
        section.contains("wins over an expired timer"),
        "recv_timeout section must document the corner case that a value \
         already buffered (or a parked rendezvous sender) wins over an \
         expired timer — matches the doc-comment at \
         src/typechecker/builtins/channel.rs."
    );
}

// ────────────────────────────────────────────────────────────────────
// Lock 3: Quick Reference table row.
// ────────────────────────────────────────────────────────────────────

#[test]
fn quick_reference_table_has_recv_timeout_row() {
    let start = CONCURRENCY_DOC_SRC
        .find("## Quick Reference")
        .expect("concurrency.md must have a Quick Reference section");
    let quick_ref = &CONCURRENCY_DOC_SRC[start..];

    assert!(
        quick_ref.contains("`channel.recv_timeout(ch, dur)`"),
        "the Quick Reference table must have a `channel.recv_timeout(ch, dur)` \
         row — every other channel.* builtin has one."
    );
    assert!(
        quick_ref.contains("Err(ChannelTimeout)") && quick_ref.contains("Err(ChannelClosed)"),
        "the recv_timeout Quick Reference row must name the \
         `Err(ChannelTimeout)` / `Err(ChannelClosed)` shapes."
    );
}
