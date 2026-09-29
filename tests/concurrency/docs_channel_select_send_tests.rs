//! Regression tests for the `channel.select` send-arm surface.
//!
//! History: round 51 documented the pre-existing send form; round 52
//! then unified the op-list shape through the `ChannelOp(a)` tagged
//! variant — `Recv(ch)` for receive arms, `Send(ch, value)` for send
//! arms. The receive-only and `(channel, value)` tuple forms are gone;
//! every list element is a `ChannelOp`. One way to do things.
//!
//! Round 62 phase-2 inlined the formerly on-disk markdown into
//! `super::docs::*_MD` constants exposed via
//! `silt::typechecker::builtin_docs()` and surfaced through LSP
//! hover / completion / signature-help.
//!
//! These tests pin three things so neither the docs nor the code can
//! silently regress:
//!
//!   1. The `Sent` constructor's builtin doc must NOT contain the
//!      "reserved for future use" phrasing.
//!
//!   2. The `channel.select` builtin doc must document the
//!      `ChannelOp` element shape and the `Sent` result that a send
//!      arm produces.
//!
//!   3. A silt program that calls `channel.select([Send(ch, value)])`
//!      must compile and run, producing the `(ch, Sent)` arm.
//!
//! The third check executes a direct inline silt snippet — not just a
//! doc-extracted block — so that a doc-only revert still leaves a
//! runtime-level lock.

use std::time::Duration;

use silt::scheduler::test_support::InProcessRunner;
use silt::value::Value;

/// Round-23 B1: the `Sent` constructor's builtin doc must not
/// revert to the "reserved for future use" phrasing.
#[test]
fn globals_sent_section_is_not_marked_reserved() {
    let docs = silt::typechecker::builtin_docs();
    let section = docs
        .get("Sent")
        .cloned()
        .expect("Sent constructor must have a registered builtin doc");

    assert!(
        !section
            .to_ascii_lowercase()
            .contains("reserved for future use"),
        "the `Sent` builtin doc reintroduced the 'reserved for future \
         use' phrasing, but `channel.select` actively supports mixed \
         send/receive operations (see src/builtins/concurrency.rs \
         `parse_select_ops`). Section contents:\n{section}"
    );

    assert!(
        section.contains("channel.select"),
        "the `Sent` builtin doc must reference `channel.select` so \
         readers can find the use site. Section contents:\n{section}"
    );
}

/// The inlined `channel.select` doc must document the `ChannelOp`
/// element form. Reverting to the pre-round-52 signature
/// (`List(Channel(a))` with raw tuples for send) or dropping the
/// `Sent` result from the prose makes this fail.
#[test]
fn channel_select_doc_documents_send_form() {
    let docs = silt::typechecker::builtin_docs();
    let section = docs
        .get("channel.select")
        .cloned()
        .expect("channel.select must have a registered builtin doc");

    // The descriptive section must name the `Sent` result.
    assert!(
        section.contains("Sent"),
        "the `channel.select` builtin doc must document the `Sent` \
         result that a send arm produces. Section contents:\n{section}"
    );
    // And it must show both `Recv` and `Send` constructors so
    // readers see the only two legal element shapes.
    assert!(
        section.contains("Recv") && section.contains("Send"),
        "the `channel.select` builtin doc must show both `Recv(ch)` \
         and `Send(ch, value)` as the ChannelOp constructors. Section \
         contents:\n{section}"
    );
}

/// Round-23 B1: executable lock. A silt program that uses
/// `channel.select` with a send arm MUST compile and run successfully and
/// produce the send-side result.
#[test]
fn channel_select_send_arm_runs_and_returns_sent() {
    let src = r#"
import channel
fn main() {
    let out = channel.new(1)
    match channel.select([Send(out, 42)]) {
        (^out, Sent) -> 1
        _ -> 0
    }
}
"#;
    let runner = InProcessRunner::new(src).with_budget(Duration::from_secs(5));
    let outcome = runner.run_trial();
    assert!(
        outcome.ok(),
        "channel.select with a send arm should compile and run cleanly: {outcome:?}"
    );
    assert_eq!(
        outcome.result,
        Some(Value::Int(1)),
        "expected the `(^out, Sent)` arm to fire (returning 1), got {:?}",
        outcome.result
    );
}

/// Round-52 audit G2/G3: the `Available After Import` summary in the
/// inlined globals doc (`super::docs::GLOBALS_MD`) must list `Sent`,
/// `Recv`, and `Send` and describe their `channel.select` role.
#[test]
fn globals_available_after_import_table_covers_select_ops() {
    let docs = silt::typechecker::builtin_docs();
    // The globals doc body is attached to every unqualified global; we
    // pull `println`'s as the representative.
    let body = docs
        .get("println")
        .cloned()
        .expect("println builtin doc must be registered (carries the globals doc)");

    let lower = body.to_ascii_lowercase();
    assert!(
        !lower.contains("reserved for future"),
        "the inlined globals doc still contains 'Reserved for future' \
         phrasing. The `Sent` row must describe the current behavior \
         (result variant produced by a completed `channel.select` send \
         arm)."
    );

    for name in &["Sent", "Recv", "Send"] {
        let needle = format!("| `{name}` |");
        assert!(
            body.contains(&needle),
            "the inlined globals doc is missing a row for `{name}` in \
             the `Available After Import` table. These constructors \
             are globally callable after `import channel`."
        );
    }

    // The `Recv` and `Send` rows must describe their role in
    // `channel.select`.
    for name in &["Recv", "Send"] {
        let row_start = body
            .find(&format!("| `{name}` |"))
            .expect("row presence already asserted above");
        let row_end = body[row_start..]
            .find('\n')
            .map(|i| row_start + i)
            .unwrap_or(body.len());
        let row = &body[row_start..row_end];
        assert!(
            row.contains("ChannelOp"),
            "the `{name}` row must name the `ChannelOp(a)` result \
             type — that is the unified `channel.select` element shape. \
             Row was:\n{row}"
        );
        assert!(
            row.contains("channel.select") || row.contains("select"),
            "the `{name}` row must mention `channel.select` so \
             readers can find the use site. Row was:\n{row}"
        );
    }
}

/// Round-23 B1: mixed send/receive arms must also work.
#[test]
fn channel_select_mixed_send_and_receive_arms_run() {
    let src = r#"
import channel
import task
fn main() {
    let out = channel.new(0)  -- rendezvous: send would park
    let inp = channel.new(1)  -- buffered: we pre-load it so receive is ready
    channel.send(inp, 7)
    match channel.select([Send(out, 99), Recv(inp)]) {
        (^out, Sent) -> 1
        (^inp, Message(v)) -> v
        _ -> -1
    }
}
"#;
    let runner = InProcessRunner::new(src).with_budget(Duration::from_secs(5));
    let outcome = runner.run_trial();
    assert!(
        outcome.ok(),
        "mixed select (send + receive) should run cleanly: {outcome:?}"
    );
    assert_eq!(
        outcome.result,
        Some(Value::Int(7)),
        "expected the receive arm to win (inp preloaded with 7) and \
         return 7, got {:?}",
        outcome.result
    );
}
